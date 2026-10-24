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

/// The daemon's live sessions, one per declared extension name. A
/// `std::sync::Mutex`, not tokio's: the critical section is a Vec walk with
/// no await in it, and the gate must hold nothing across the verdict await.
#[derive(Default)]
pub struct Registry {
    sessions: std::sync::Mutex<Vec<Handle>>,
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

/// One session attempt: spawn, handshake, deliver-loop. Returns when
/// the child dies, or the daemon shuts down (epoch channel closed). A
/// stalled WRITE ends the session — the supervisor respawns with a fresh
/// cursor at the log head. The supervisor calls this in a retry loop.
pub async fn session(d: &std::sync::Arc<Daemon>, name: &str, path: &str) {
    let mut rx = d.epoch_tx.subscribe();
    let spec = Spec::executable(name, path);
    let mut process: Process = match spawn(&spec) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[host:{name}] spawn failed: {e}");
            return;
        }
    };

    // handshake: initialize out, hello in, budgeted both ways by the
    // library; decode failure is a refused session, never a partial accept
    let Some(hello) = process.session.handshake(HANDSHAKE_TIMEOUT).await else {
        eprintln!("[host:{name}] handshake failed (no/undecodable hello within budget)");
        process.kill();
        return;
    };
    if hello.name != name {
        eprintln!("[host:{name}] hello name mismatch: {:?}", hello.name);
    }
    eprintln!("[host:{name}] ready (on: {:?})", hello.schema.on());

    // the dispatch end of the intercept plane: one queue per session plus
    // the handle the gate reaches it by. Registering a same-name handle
    // replaces a predecessor's and closes ITS queue — the old task ends.
    let (hot_tx, mut hot_rx) = mpsc::channel::<HotCall>(HOT_QUEUE);
    d.host.register(Handle {
        name: name.to_string(),
        events: hello.schema.on(),
        tx: hot_tx,
    });

    // per-session cursor starts at the log head (no history replay, v1).
    // An unreadable head is a store fault: the session ends and the
    // supervisor respawns it (a cursor guessed from 0 would replay the
    // whole log into a fresh subscriber).
    let mut cursor = match { d.store.lock().await.event_head() } {
        Ok(head) => head,
        Err(e) => {
            eprintln!("[host:{name}] event head read failed: {e}; session ends");
            process.kill();
            return;
        }
    };
    // verdicts this host is waiting on: host-assigned id -> the waiter. The
    // table lives here because the answers arrive here.
    let mut pending: HashMap<u64, oneshot::Sender<Value>> = HashMap::new();

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
                        d.host.unregister(name);
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
                        d.host.unregister(name);
                        process.kill();
                        return; // supervisor respawns; cursor = new head
                    }
                    cursor = *id;
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
                    d.host.unregister(name);
                    process.kill();
                    return; // supervisor respawns; cursor = new head
                }
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
                    && let Some(tx) = pending.remove(&id)
                {
                    let _ = tx.send(r.result);
                    continue;
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
        }
    }
    d.host.unregister(name);
    process.kill();
}

/// The supervisor: keep every declared extension session alive.
/// `config["extensions"]` = name → executable path (boot-read, v1).
pub async fn supervise(d: std::sync::Arc<Daemon>) {
    let decls: Vec<(String, String)> = {
        let user = crate::config::user_config_path().unwrap_or_default();
        match crate::config::load(&d.config_default, &user) {
            Ok(cfg) => cfg["extensions"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|p| (k.clone(), p.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
            Err(e) => {
                eprintln!("[host] config load failed, no extensions: {e}");
                Vec::new()
            }
        }
    };
    for (name, path) in decls {
        let d2 = std::sync::Arc::clone(&d);
        tokio::spawn(async move {
            loop {
                session(&d2, &name, &path).await;
                tokio::time::sleep(RESPAWN_DELAY).await;
            }
        });
    }
}
