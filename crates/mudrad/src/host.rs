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
//! - `result` frames from a guest answering a delivered call are consumed
//!   nowhere yet (`Routed::Result` — no intercept plane before there is
//!   something to intercept with).
//!
//! Hang discipline (the store-lock blood lesson, mirrored): no await
//! happens while holding the store lock — the log read is a short
//! lock-only step; every pipe WRITE carries a budget, and a timed-out
//! write leaves the cursor un-advanced (the log is the buffer; a stalled
//! extension stalls only itself, by design of the per-session cursor).
//! Reads carry no budget on purpose — a live extension may be silent
//! (induction-host `Reader::next_frame`); only the channel closing ends
//! the read side.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use induction_host::{
    HostOps, InvokeCall, OpsFuture, Process, Routed, Spec, spawn, route_line,
};
use induction_protocol::CallFrame;

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

    // per-session cursor starts at the log head (no history replay, v1)
    let mut cursor = { d.store.lock().await.event_head() };

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
                        process.kill();
                        return; // supervisor respawns; cursor = new head
                    }
                    cursor = *id;
                }
            }
            line = reader.next_frame() => {
                match line {
                    Some(line) => match route_line(&VerbOps { d }, &line).await {
                        Routed::Reply(reply) => {
                            if !writer.write_line(&reply.to_line(), WRITE_TIMEOUT).await {
                                eprintln!("[host:{name}] reply write failed");
                                break;
                            }
                        }
                        // `emit` carries no reply by contract; a `result`
                        // frame has no consumer yet (v1 boundary above)
                        Routed::Accepted | Routed::Result(_) => {}
                        // protocol noise: surface it, never stall the session
                        Routed::DecodeFailed => {
                            eprintln!("[host:{name}] undecodable frame: {line}");
                        }
                    },
                    None => break, // child closed stdout
                }
            }
        }
    }
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
