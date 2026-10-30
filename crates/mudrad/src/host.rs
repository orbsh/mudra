//! The extension host — ADR-extension-protocol §1–§5 over the Induction
//! protocol crates: the frame vocabulary, the session I/O (budgeted writes,
//! unbudgeted reads) and the PDEATHSIG spawn are `induction-host`'s job;
//! mudra supplies only what a host must own — the event model (§4: the epoch
//! watch triggers a log-window pull from the session's own cursor) and the
//! effect surface (§5: the invoke arm routed to `run_verb`; effects execute
//! in mudra, never on the pipe).
//!
//! One resident session task per declared extension: spawn + `initialize`/
//! `hello` handshake (the script's interface_schema rides the hello — §3),
//! then a loop selecting over two sources: epoch changes (pull the event log
//! from the session's cursor, deliver as `call` frames) and the child's
//! stdout (host frames → `route_line` → `host_reply`; everything else fails
//! at decode).
//!
//! v1 boundaries (recorded, not bugs):
//! - cursor is in-session memory: a restart starts at the log head and
//!   does not replay history (the aura ADR-0014 subscribe rule); the
//!   durable-replay path is the `/events` verb, which is complete
//!   without the host.
//! - the extension set is read once at daemon start; changing it is a
//!   daemon restart (config hot-reload is a `/config`-face feature).
//! - event-log delivery is the Cold face; the intercept (`Hot`) face waits
//!   on verdicts, and its budget is a host constant — the per-handler
//!   `Tier`/deadline declaration is reserved in the hello schema
//!   (ADR-0001) but neither the guests nor the library carry it yet.
//!
//! §4's intercept plane: a host-initiated `call` frame goes out under a
//! host-assigned id, the guest's `result` frame comes back, and the answer
//! is consumed here — the first non-`allow` verdict wins, and every failure
//! mode (no declaring session, expired budget, dropped channel, stalled
//! queue, unparsable answer) fails OPEN, because blocking a navigation on a
//! late verdict is the worse default. The gate never takes the store lock
//! (it runs before the verb does) and is deliberately NOT on the verb path
//! guests reach through `host:invoke`: a guest's own effect must not be
//! gated by the verdict it is carrying out.
//!
//! Hang discipline (the store-lock blood lesson, mirrored): no await
//! happens while holding the store lock — the log read is a short
//! lock-only step; every pipe WRITE carries a budget, and a timed-out
//! write leaves the cursor un-advanced (the log is the buffer; a stalled
//! extension stalls only itself, by design of the per-session cursor).
//! Reads carry no budget on purpose — a live extension may be silent
//! (induction-host `Reader::next_frame`); only the channel closing ends
//! the read side.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use induction_host::{
    HostOps, InvokeCall, OpsFuture, Process, Routed, Spec, spawn, route_line,
};
use induction_protocol::{CallFrame, GuestFrame, parse_guest_line};

use crate::daemon::Daemon;

/// Handshake + write budgets. Every pipe await that can hang uses one of
/// these — nothing in the session loop can hang forever except by design
/// (the unbudgeted read above).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Restart backoff: constant for now (residency accounting is an aura
/// concern; mudra v1 just never hot-loops) — induction-host's
/// `DEFAULT_BACKOFF` is the same number.
const RESPAWN_DELAY: Duration = Duration::from_secs(10);
/// Rows pulled per epoch change, per session.
const EVENT_BATCH: usize = 256;
/// How long a session that is NOT declared `resident` stays after its last
/// delivery before it reaps itself. Chosen against the cost it pays on the next
/// one: a python guest cold start measured 16ms (median of 20) — nothing next to
/// the work a guest like `bw-fill` does (one `rbw list` is 138ms).
const DEFAULT_IDLE_TTL: Duration = Duration::from_secs(60);

// ================= §4b the ask face: an event that answers =================

/// A user-fired event whose consumer answers it, so the trigger can carry a
/// value back (the fill-password verb). Names are free strings (§3); this one is
/// mudra's, like the lifecycle kinds.
pub const FILL_PASSWORD: &str = "mudra:fill_password";
/// The ask budget. Generous on purpose: the answer may run a password manager
/// (unlock, a local DB query), which is seconds, not milliseconds — and unlike a
/// navigation verdict there is nothing to lose by waiting, since a wrong or
/// absent password would fill the wrong fields.
const ASK_BUDGET: Duration = Duration::from_secs(15);

// ================= §4 intercept plane: the Hot face =================

/// Host-initiated calls carry ids in their own space, far above the
/// event-log row ids (the store's monotonic counter): a guest answering a
/// delivered event can never be mis-correlated with a pending verdict.
const HOT_ID_BASE: u64 = 1 << 62;
/// The pre-navigation budget. mudra's policy is fail-open, so this is a UX
/// budget, not a correctness one: a late verdict loses, a missing one loses.
const HOT_DEADLINE: Duration = Duration::from_millis(200);
/// Backlog of host-initiated calls per session. A full queue means the
/// session is stalled; the gate fails open rather than park on it.
const HOT_QUEUE: usize = 4;
/// §4's intercept event name — a free literal string (§3: names are free
/// strings, matched by equality on both sides).
pub const BEFORE_NAVIGATE: &str = "mudra:before_navigate";

/// The verdict vocabulary of `mudra:before_navigate`: `allow` (also what an
/// absent answer means), `deny`, `redirect` + the url to use instead.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Allow,
    Deny,
    Redirect(String),
}

/// Decode one `result` payload into a verdict. Anything unrecognised — no
/// `verdict` field, a typo'd spelling, a `redirect` without a url — is
/// `Allow`: the failure value and the fail-open policy are the same outcome
/// by construction.
pub fn parse_verdict(result: &Value) -> Verdict {
    match result.get("verdict").and_then(Value::as_str) {
        Some("deny") => Verdict::Deny,
        Some("redirect") => match result.get("url").and_then(Value::as_str) {
            Some(u) if !u.is_empty() => Verdict::Redirect(u.to_string()),
            _ => Verdict::Allow,
        },
        _ => Verdict::Allow,
    }
}

/// One host-initiated call queued to a live session: the frame to write plus
/// the oneshot its answer lands on.
pub struct HotCall {
    pub frame: CallFrame,
    pub reply: oneshot::Sender<Value>,
}

/// A live session's control handle — what the intercept plane dispatches
/// through. `events` is the declared set from the hello (the same list that
/// filters delivery).
#[derive(Clone)]
pub struct Handle {
    pub name: String,
    pub events: Vec<String>,
    tx: mpsc::Sender<HotCall>,
}

impl Handle {
    /// Build a handle for a session (used by `session` and by tests).
    pub fn new(name: impl Into<String>, events: Vec<String>, tx: mpsc::Sender<HotCall>) -> Self {
        Self { name: name.into(), events, tx }
    }

    fn declares(&self, event: &str) -> bool {
        self.events.iter().any(|e| e == event)
    }
}

/// One `extensions` entry: what to run, and how long to keep it alive.
///
/// Residency is a BUDGET question, not a preference: a guest that has to be
/// consulted inside the 200ms verdict budget cannot be cold-started there, so it
/// is declared `resident` (started at boot, never reaped). Everything else is
/// started when something must be delivered to it and reaps itself after
/// `idle_ttl` — the induction shape (aura's `lifecycle.idle_ttl` + evictor), not
/// a set of daemons parked since login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decl {
    pub name: String,
    pub cmd: String,
    /// Started at boot and never reaped.
    pub resident: bool,
    /// Idle window; ignored when `resident`. `None` = the default window.
    pub idle_ttl: Option<Duration>,
    /// The directory of the config file that declared this — a relative `cmd` is
    /// resolved against it, so a tracked config can name its guest relatively.
    pub base: Option<std::path::PathBuf>,
}

impl Decl {
    /// A declaration with defaults — what a bare `name "path"` means.
    pub fn new(name: impl Into<String>, cmd: impl Into<String>) -> Self {
        Self { name: name.into(), cmd: cmd.into(), resident: false, idle_ttl: None, base: None }
    }

    /// Decode one `config["extensions"]` entry: a bare string is the command, a
    /// table adds `resident` / `idle_ttl` (seconds).
    pub fn from_value(name: &str, v: &Value) -> Option<Self> {
        match v {
            Value::String(cmd) => Some(Self::new(name, cmd.clone())),
            Value::Object(m) => Some(Self {
                name: name.to_string(),
                cmd: m.get("cmd").and_then(Value::as_str)?.to_string(),
                resident: m.get("resident").and_then(Value::as_bool).unwrap_or(false),
                idle_ttl: m.get("idle_ttl").and_then(Value::as_u64).map(Duration::from_secs),
                base: None,
            }),
            _ => None,
        }
    }

    /// The idle window this session reaps at, or `None` for "never". A resident
    /// declaration has no window; `0` is the same statement written explicitly.
    fn ttl(&self) -> Option<Duration> {
        if self.resident {
            return None;
        }
        match self.idle_ttl {
            Some(d) if d.is_zero() => None,
            Some(d) => Some(d),
            None => Some(DEFAULT_IDLE_TTL),
        }
    }

    /// The executable to run. Three spellings, resolved here rather than by the
    /// kernel: `~/…` against HOME, an absolute path as written, and a relative
    /// path against the directory of the config file that declared it (so the
    /// repo's own config names `scripts/guests/bw_fill.py` and works wherever the
    /// repo is checked out, for whoever checks it out).
    fn exec(&self) -> std::path::PathBuf {
        if let Some(rest) = self.cmd.strip_prefix("~/") {
            return match std::env::var_os("HOME") {
                Some(home) => std::path::PathBuf::from(home).join(rest),
                None => std::path::PathBuf::from(&self.cmd),
            };
        }
        let path = std::path::Path::new(&self.cmd);
        if path.is_absolute() {
            return path.to_path_buf();
        }
        match &self.base {
            Some(base) => base.join(path),
            None => path.to_path_buf(),
        }
    }
}

/// The daemon's live sessions, one per declared extension name. A
/// `std::sync::Mutex`, not tokio's: the critical section is a Vec walk with
/// no await in it, and the gate must hold nothing across the verdict await.
#[derive(Default)]
pub struct Registry {
    sessions: std::sync::Mutex<Vec<Handle>>,
    /// The config declarations, read once at boot. Kept here because the demand
    /// path needs them long after `supervise` returned.
    decls: std::sync::Mutex<Vec<Decl>>,
    /// Event ids a caller is waiting on: an answer frame whose id is neither a
    /// pending Hot call nor here is dropped (the guest answered something nobody
    /// asked for).
    answers: std::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    /// Answers that arrived BEFORE the waiter registered. The window is real:
    /// the row id is assigned by the store write, so the waiter can only
    /// register after it, and a fast guest can answer in between.
    late: std::sync::Mutex<HashMap<u64, Value>>,
    /// Names whose spawn is in flight — one spawn per name, no pile-up.
    spawning: std::sync::Mutex<std::collections::HashSet<String>>,
    next_id: std::sync::atomic::AtomicU64,
}

impl Registry {
    /// Register one live session, replacing a same-name predecessor (one
    /// session per name: the supervisor respawns, it does not duplicate).
    pub fn register(&self, h: Handle) {
        let mut v = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        v.retain(|s| s.name != h.name);
        v.push(h);
    }

    /// A session ended: drop its handle so the gate stops dispatching to it.
    pub fn unregister(&self, name: &str) {
        let mut v = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        v.retain(|s| s.name != name);
    }

    fn declaring(&self, event: &str) -> Vec<Handle> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|s| s.declares(event))
            .cloned()
            .collect()
    }

    /// Record the config declarations (boot, or a test). They outlive their
    /// sessions: the demand path re-reads them for every cold start.
    pub fn set_decls(&self, decls: Vec<Decl>) {
        *self.decls.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = decls;
    }

    fn decl(&self, name: &str) -> Option<Decl> {
        self.decls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|d| d.name == name)
            .cloned()
    }

    fn is_spawning(&self, name: &str) -> bool {
        self.spawning
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(name)
    }

    /// Every declaration that is not `resident` — the ones the demand path may
    /// cold-start.
    fn non_resident(&self) -> Vec<Decl> {
        self.decls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|d| !d.resident)
            .cloned()
            .collect()
    }

    pub fn is_live(&self, name: &str) -> bool {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|s| s.name == name)
    }

    /// Claim the right to spawn `name`. `false` = someone else is already
    /// spawning it (the caller waits for the handshake instead of a second
    /// process).
    fn claim_spawn(&self, name: &str) -> bool {
        self.spawning
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(name.to_string())
    }

    fn release_spawn(&self, name: &str) {
        self.spawning
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(name);
    }

    /// Where the answer to event row `id` lands. A value that arrived before this
    /// call is handed straight back — the waiter can only register after the store
    /// assigned the id, so "already answered" is a live case, not a defensive one.
    fn answer_for(&self, id: u64) -> Answer {
        if let Some(v) = self
            .late
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
        {
            return Answer::Ready(v);
        }
        let (tx, rx) = oneshot::channel();
        self.answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, tx);
        Answer::Pending(rx)
    }

    /// Route one guest answer to its waiter. `false` = nobody asked for this id,
    /// and it is stashed in case the waiter is a moment late.
    fn deliver(&self, id: u64, value: Value) -> bool {
        if let Some(tx) = self
            .answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
        {
            let _ = tx.send(value);
            return true;
        }
        // only event-row ids can be answered late (Hot ids are the host's own and
        // always have a waiter); the cap keeps a chatty guest from growing this
        let mut late = self.late.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if id < HOT_ID_BASE {
            if late.len() >= 64 {
                late.clear();
            }
            late.insert(id, value);
        }
        false
    }
}

/// What `answer_for` produced: a value already in hand, or a receiver to await.
pub enum Answer {
    Ready(Value),
    Pending(oneshot::Receiver<Value>),
}

/// §4's intercept plane: ask every session that declared the event, in
/// registration order (the config group arrives as a JSON object, so that
/// order is by name), and take the first non-`allow` verdict. No declaring
/// session, an expired budget, a dropped channel, a stalled queue or an
/// unparsable answer all mean `Allow` — fail-open, navigation UX wins.
pub async fn before_navigate(reg: &Registry, url: &str, ctx: &str) -> (Verdict, Option<String>) {
    for h in reg.declaring(BEFORE_NAVIGATE) {
        let id = HOT_ID_BASE + reg.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let frame = CallFrame::call(id, BEFORE_NAVIGATE, json!({ "url": url, "ctx": ctx }));
        let (tx, rx) = oneshot::channel();
        if h.tx.try_send(HotCall { frame, reply: tx }).is_err() {
            eprintln!(
                "[host:{}] {BEFORE_NAVIGATE}: queue full or session gone -> fail open",
                h.name
            );
            continue;
        }
        match tokio::time::timeout(HOT_DEADLINE, rx).await {
            Ok(Ok(result)) => match parse_verdict(&result) {
                Verdict::Allow => {}
                other => return (other, Some(h.name)),
            },
            Ok(Err(_)) => eprintln!("[host:{}] verdict channel dropped -> fail open", h.name),
            Err(_) => eprintln!("[host:{}] verdict budget expired -> fail open", h.name),
        }
    }
    (Verdict::Allow, None)
}

/// Apply the intercept plane to one request off 8899, returning the
/// (possibly rewritten) body plus, on a `deny`, the reply to send instead of
/// executing the verb. Only the two navigation ENTRIES are gated: `/open`
/// (the page-click beacon, the console, the CLI) and `/nav` with `cmd:
/// goto`. back/forward/reload carry no new url to rule on.
///
/// The `ctx` handed to a verdict is the one the request named; an empty
/// string means "whatever the daemon's current context is" — resolving the
/// default here would duplicate a Controller policy.
pub async fn intercept(d: &Arc<Daemon>, path: &str, req: Value) -> (Value, Option<Value>) {
    let gated = path == "/open"
        || (path == "/nav" && req.get("cmd").and_then(Value::as_str) == Some("goto"));
    let url = req
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !gated || url.is_empty() {
        return (req, None); // the verb's own validation owns a missing url
    }
    let ctx = req
        .get("ctx")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match before_navigate(&d.host, &url, &ctx).await {
        (Verdict::Deny, by) => {
            let by = by.unwrap_or_default();
            eprintln!("[host] {BEFORE_NAVIGATE}: {url} denied by {by}");
            (req, Some(json!({ "blocked": true, "by": by, "url": url })))
        }
        (Verdict::Redirect(to), by) => {
            eprintln!(
                "[host] {BEFORE_NAVIGATE}: {url} -> {to} (by {})",
                by.unwrap_or_default()
            );
            let mut req = req;
            req["url"] = json!(to);
            (req, None)
        }
        (Verdict::Allow, _) => (req, None),
    }
}

/// One log row as a `call` frame. `args` was stored as a JSON snapshot
/// string; it rides as a parsed value (self-describing, the consumer
/// never calls back). A corrupt args string ships as a raw string value —
/// the frame must never be withheld by a decode miss.
pub fn event_row_frame(id: u64, kind: &str, args: &str, at: u64) -> CallFrame {
    let parsed: Value =
        serde_json::from_str(args).unwrap_or_else(|_| Value::String(args.to_string()));
    CallFrame::call(id, kind, json!({ "event_id": id, "at": at, "snapshot": parsed }))
}

/// §5: effects execute in mudra. The invoke arm routes through the daemon's
/// verb surface; the nested booth shape has no target here (no booths), so it
/// is refused with an error value — every other arm is declined by the
/// library's defaults (an explicit error back to the guest, never silence).
struct VerbOps<'a> {
    d: &'a Arc<Daemon>,
}

impl HostOps for VerbOps<'_> {
    fn invoke<'a>(&'a self, call: &'a InvokeCall) -> OpsFuture<'a> {
        let d = self.d;
        Box::pin(async move {
            match call {
                InvokeCall::Verb { verb, args } => crate::daemon::run_verb(d, verb, args.clone()).await,
                InvokeCall::Booth { .. } => {
                    Err("mudra hosts no booths: the invoke arm's flat `verb` shape is the one this host serves".to_string())
                }
            }
        })
    }
}

/// One session: spawn, handshake, deliver-loop. Returns when the child dies, the
/// idle window expires, or the daemon shuts down (epoch channel closed). A
/// stalled WRITE ends the session.
///
/// The session is the residency: whoever starts it (`supervise` for a `resident`
/// declaration, the demand path for everything else) gets the same task, and the
/// idle window is the only thing that decides when it goes away — a session that
/// keeps being delivered to does not expire, and one that is not expires without
/// anyone watching.
pub async fn session(d: &std::sync::Arc<Daemon>, decl: Decl) {
    let name = decl.name.clone();
    let mut rx = d.epoch_tx.subscribe();
    let spec = Spec::executable(&name, decl.exec().to_string_lossy().to_string());
    let mut process: Process = match spawn(&spec) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[host:{name}] spawn failed: {e}");
            d.host.release_spawn(&name);
            return;
        }
    };

    // handshake: initialize out, hello in, budgeted both ways by the
    // library; decode failure is a refused session, never a partial accept
    let Some(hello) = process.session.handshake(HANDSHAKE_TIMEOUT).await else {
        eprintln!("[host:{name}] handshake failed (no/undecodable hello within budget)");
        process.kill();
        d.host.release_spawn(&name);
        return;
    };
    if hello.name != name {
        eprintln!("[host:{name}] hello name mismatch: {:?}", hello.name);
    }
    // The verdict budget decides residency, not the config author: a guest the
    // intercept plane has to consult cannot be cold-started inside 200ms, so a
    // session that declares the Hot event never expires — it would fail open on
    // every request after its first idle window, silently.
    let declares_hot = hello.schema.receives(BEFORE_NAVIGATE);
    let ttl = if declares_hot { None } else { decl.ttl() };
    eprintln!(
        "[host:{name}] ready (on: {:?}, retention: {})",
        hello.schema.on(),
        match ttl {
            Some(t) => format!("{}s idle", t.as_secs()),
            None if declares_hot => "resident (Hot face)".to_string(),
            None => "resident".to_string(),
        }
    );

    // the dispatch end of the intercept plane: one queue per session plus
    // the handle the gate reaches it by. Registering a same-name handle
    // replaces a predecessor's and closes ITS queue — the old task ends.
    let (hot_tx, mut hot_rx) = mpsc::channel::<HotCall>(HOT_QUEUE);
    d.host.register(Handle {
        name: name.clone(),
        events: hello.schema.on(),
        tx: hot_tx,
    });
    // the handshake is done: a second spawn for this name would be a duplicate
    d.host.release_spawn(&name);

    // per-session cursor starts at the log head (no history replay, v1). The
    // order matters for the demand path: the caller spawns us BEFORE writing the
    // row it wants answered, so the row is future, not history.
    let mut cursor = match { d.store.lock().await.event_head() } {
        Ok(head) => head,
        Err(e) => {
            eprintln!("[host:{name}] event head read failed: {e}; session ends");
            d.host.unregister(&name);
            process.kill();
            return;
        }
    };
    // verdicts this host is waiting on: host-assigned id -> the waiter. The
    // table lives here because the answers arrive here.
    let mut pending: HashMap<u64, oneshot::Sender<Value>> = HashMap::new();
    // last delivery of any kind; the idle window is measured from here
    let mut last = tokio::time::Instant::now();

    // Disjoint read/write borrows — the shape the library's Reader/Writer
    // split exists for: both halves live across one select!.
    let induction_host::Session { reader, writer } = &mut process.session;

    loop {
        tokio::select! {
            changed = rx.changed() => {
                if changed.is_err() {
                    break; // daemon shutting down
                }
                // short store lock: pull the window, then release before
                // any pipe await (the run_verb discipline, same shape)
                let rows = {
                    let store = d.store.lock().await;
                    store.events_since(cursor, EVENT_BATCH)
                };
                // a failed read is not an empty window: the session ends
                // rather than advancing a cursor over rows it never saw
                let rows = match rows {
                    Ok(rows) => rows,
                    Err(e) => {
                        eprintln!("[host:{name}] event read failed: {e}; stalling session");
                        d.host.unregister(&name);
                        process.kill();
                        return; // supervisor respawns; cursor = new head
                    }
                };
                for (id, e) in &rows {
                    if !hello.schema.receives(&e.kind) {
                        // not for this session: the cursor still moves
                        // past it (subscription filters delivery, not
                        // the log's meaning)
                        cursor = *id;
                        continue;
                    }
                    let frame = event_row_frame(*id, &e.kind, &e.args, e.at);
                    if !writer.write_line(&frame.to_line(), WRITE_TIMEOUT).await {
                        eprintln!("[host:{name}] write failed at event {id}; stalling session");
                        d.host.unregister(&name);
                        process.kill();
                        return; // supervisor respawns; cursor = new head
                    }
                    cursor = *id;
                    last = tokio::time::Instant::now();
                }
            }
            hot = hot_rx.recv() => {
                // the registry dropped its sender (shutdown) or this handle
                // was replaced: end the session
                let Some(hot) = hot else { break };
                let id = hot.frame.id;
                // register the waiter BEFORE the write: an answer can only
                // arrive after the frame lands, so a fast guest cannot beat
                // its own registration
                pending.insert(id, hot.reply);
                if !writer.write_line(&hot.frame.to_line(), WRITE_TIMEOUT).await {
                    // the frame did not land: drop the waiter (the gate sees
                    // the channel close and fails open), end the session
                    pending.remove(&id);
                    eprintln!("[host:{name}] verdict write failed; stalling session");
                    d.host.unregister(&name);
                    process.kill();
                    return; // supervisor respawns; cursor = new head
                }
                last = tokio::time::Instant::now();
            }
            line = reader.next_frame() => {
                let Some(line) = line else { break }; // child closed stdout
                // An answer to a call THIS host sent: correlate on the id and
                // hand it to the waiter. Correlation is host state, and
                // `route_line` reports a result frame as `Routed::Result(value)`
                // — the id is dropped there — so the host decodes the frame
                // first, through the library's own decoder (never a second wire
                // parser).
                if let Some(GuestFrame::Result(r)) = parse_guest_line(&line)
                    && let Some(id) = r.id.as_u64()
                {
                    // two id spaces, one decode: a Hot verdict this host is
                    // waiting on, or the answer to an event row (`ask`)
                    if let Some(tx) = pending.remove(&id) {
                        let _ = tx.send(r.result);
                        continue;
                    }
                    if d.host.deliver(id, r.result) {
                        continue;
                    }
                }
                match route_line(&VerbOps { d }, &line).await {
                    Routed::Reply(reply) => {
                        if !writer.write_line(&reply.to_line(), WRITE_TIMEOUT).await {
                            eprintln!("[host:{name}] reply write failed");
                            break;
                        }
                    }
                    // `emit` carries no reply by contract; a `result` frame
                    // with no waiter is a Cold handler answering anyway —
                    // nothing to do with it, and nothing to stall for
                    Routed::Accepted | Routed::Result(_) => {}
                    // protocol noise: surface it, never stall the session
                    Routed::DecodeFailed => {
                        eprintln!("[host:{name}] undecodable frame: {line}");
                    }
                }
            }
            // the idle window: nothing delivered for `ttl` ends the session, and
            // the next delivery cold-starts it. A 1s tick is enough for a window
            // measured in tens of seconds, and a `None` window ticks to nothing.
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if let Some(ttl) = ttl
                    && last.elapsed() >= ttl
                {
                    eprintln!("[host:{name}] idled out after {}s", ttl.as_secs());
                    break;
                }
            }
        }
    }
    d.host.unregister(&name);
    process.kill();
}

/// Boot wiring: read the declarations and keep only the `resident` ones warm (a
/// constant-backoff loop each). Everything else is started when something has to
/// be delivered to it and reaps itself after its idle window — so a daemon that
/// nobody uses hosts no guest processes.
pub async fn supervise(d: std::sync::Arc<Daemon>) {
    let decls = load_decls(&d);
    let resident: Vec<Decl> = decls.iter().filter(|decl| decl.resident).cloned().collect();
    eprintln!(
        "[host] {} extension(s) declared, resident: {:?}",
        decls.len(),
        resident.iter().map(|d| d.name.as_str()).collect::<Vec<_>>()
    );
    d.host.set_decls(decls);
    for decl in resident {
        let d2 = std::sync::Arc::clone(&d);
        tokio::spawn(async move {
            loop {
                session(&d2, decl.clone()).await;
                tokio::time::sleep(RESPAWN_DELAY).await;
            }
        });
    }
}

/// The declarations, read once at boot. The two layers are parsed SEPARATELY
/// here — not through `config::load`, which merges into one object and loses
/// which file an entry came from, and a relative `cmd` has to be resolved
/// against exactly that file's directory. The user layer wins on a name clash,
/// same as the merge.
fn load_decls(d: &Arc<Daemon>) -> Vec<Decl> {
    let user = crate::config::user_config_path().unwrap_or_default();
    let mut decls: Vec<Decl> = Vec::new();
    for path in [&d.config_default, &user] {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let cfg = match crate::config::parse(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("[host] {}: {e}", path.display());
                continue;
            }
        };
        let Some(entries) = cfg["extensions"].as_object() else { continue };
        for (name, value) in entries {
            let Some(mut decl) = Decl::from_value(name, value) else {
                eprintln!("[host] {name}: needs a `cmd` (string, or `cmd` in a block)");
                continue;
            };
            decl.base = path.parent().map(std::path::Path::to_path_buf);
            decls.retain(|d| d.name != decl.name);
            decls.push(decl);
        }
    }
    decls
}

/// Make sure `name` has a live session, starting one if needed. `false` = no
/// declaration, or the spawn/handshake failed.
///
/// Returns once the handle is registered — which is also the cursor snapshot —
/// so a caller can write the row it wants answered right after this returns and
/// know the row is future, not history.
pub async fn ensure(d: &Arc<Daemon>, name: &str) -> bool {
    if d.host.is_live(name) {
        return true;
    }
    let Some(decl) = d.host.decl(name) else { return false };
    if d.host.claim_spawn(name) {
        let d2 = std::sync::Arc::clone(d);
        let spawned = decl.clone();
        tokio::spawn(async move { session(&d2, spawned).await });
    }
    // bounded by the handshake budget: either the handle appears, or the spawn
    // gave up and said so
    for _ in 0..(HANDSHAKE_TIMEOUT.as_millis() / 20) as usize + 5 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if d.host.is_live(name) {
            return true;
        }
        if !d.host.is_spawning(name) {
            break;
        }
    }
    d.host.is_live(name)
}

/// Fire one user event and wait for the first answer that declares it.
///
/// The row id IS the call id: a guest answers with a `result` frame carrying it,
/// so an event can return a value without a second protocol. Order is
/// load-bearing — a session snapshots its cursor at the handshake, so the guest
/// must be live BEFORE the row is written, or it would read the row as history.
pub async fn ask(
    d: &Arc<Daemon>,
    kind: &str,
    args: Value,
    budget: Duration,
) -> Result<(u64, Value), String> {
    let mut askers = d.host.declaring(kind);
    if askers.is_empty() {
        // The schema arrives with the hello, so with nothing running the only way
        // to find who declares this kind is to start the candidates. One that does
        // not declare it just reaps itself after its idle window.
        for decl in d.host.non_resident() {
            ensure(d, &decl.name).await;
        }
        askers = d.host.declaring(kind);
    }
    if askers.is_empty() {
        return Err(format!("no extension declares {kind}"));
    }
    let id = {
        let mut store = d.store.lock().await;
        let id = store
            .emit_event(kind, args.to_string(), crate::watch::now_ms())
            .map_err(|e| e.to_string())?;
        // the fan-out wakes on the epoch; a log-only write moves nothing, so a
        // standalone event bumps it (the panel re-scans once, harmlessly)
        let epoch = store.bump_epoch().map_err(|e| e.to_string())?;
        drop(store);
        d.publish_epoch(epoch);
        id
    };
    match d.host.answer_for(id) {
        Answer::Ready(v) => Ok((id, v)),
        Answer::Pending(rx) => match tokio::time::timeout(budget, rx).await {
            Ok(Ok(v)) => Ok((id, v)),
            Ok(Err(_)) => Err(format!("{kind}: the answer channel dropped")),
            Err(_) => Err(format!("{kind}: no answer within {}s", budget.as_secs())),
        },
    }
}

/// `/fill_password {url, ctx?, user_hint?}` — the address bar's ask. The guest's
/// answer rides back verbatim under `answer` (`{ok, user, password, …}` for
/// bw-fill), beside the event id it landed on so the console log and the reply
/// line up.
pub async fn fill_password(d: &Arc<Daemon>, req: &Value) -> Result<Value, String> {
    let url = req.get("url").and_then(Value::as_str).unwrap_or_default();
    if url.is_empty() {
        return Err("need url".into());
    }
    let args = json!({
        "kind": FILL_PASSWORD,
        "url": url,
        "ctx": req.get("ctx").cloned().unwrap_or(Value::Null),
        "user_hint": req.get("user_hint").cloned().unwrap_or(Value::Null),
    });
    let (event_id, answer) = ask(d, FILL_PASSWORD, args, ASK_BUDGET).await?;
    Ok(json!({ "event_id": event_id, "answer": answer }))
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_IDLE_TTL, Decl};
    use std::time::Duration;

    /// The residency arithmetic, in one place: a declared `resident` never
    /// expires, an explicit `0` says the same thing, a number is the window, and
    /// no number means the default. The reap loop only reads this.
    #[test]
    fn idle_window_is_the_declaration() {
        let mut d = Decl::new("x", "/bin/true");
        assert_eq!(d.ttl(), Some(DEFAULT_IDLE_TTL));
        d.idle_ttl = Some(Duration::from_secs(5));
        assert_eq!(d.ttl(), Some(Duration::from_secs(5)));
        d.idle_ttl = Some(Duration::ZERO);
        assert_eq!(d.ttl(), None);
        d.idle_ttl = None;
        d.resident = true;
        assert_eq!(d.ttl(), None);
    }

    /// The three spellings: `~/` against HOME, absolute as written, and relative
    /// against the declaring config file's directory.
    #[test]
    fn cmd_spellings_resolve() {
        let home = std::env::var("HOME").unwrap_or_default();
        let tilde = Decl::new("x", "~/world/mudra/scripts/guests/bw_fill.py");
        assert_eq!(
            tilde.exec().to_string_lossy().to_string(),
            format!("{home}/world/mudra/scripts/guests/bw_fill.py")
        );
        let abs = Decl::new("x", "/usr/bin/true");
        assert_eq!(abs.exec().to_string_lossy().to_string(), "/usr/bin/true");
        let mut rel = Decl::new("x", "scripts/guests/bw_fill.py");
        rel.base = Some(std::path::PathBuf::from("/repo/world/mudra"));
        assert_eq!(
            rel.exec().to_string_lossy().to_string(),
            "/repo/world/mudra/scripts/guests/bw_fill.py"
        );
    }
}
