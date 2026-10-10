//! The extension host — ADR-extension-protocol §1–§5, the BGI shape.
//!
//! One resident session task per declared extension: spawn (PDEATHSIG,
//! the same discipline as chromium children), `initialize`/`hello`
//! handshake (the script's interface_schema rides the hello — §3), then
//! a loop selecting over two sources: epoch changes (pull the event log
//! from the session's own cursor, deliver as BGI call frames) and the
//! child's stdout (typed host frames → run_verb → host_reply, §5's
//! effects-in-mudra rule; everything else fails at decode).
//!
//! v1 boundaries (recorded, not bugs):
//! - cursor is in-session memory: a restart starts at the log head and
//!   does not replay history (the aura ADR-0014 subscribe rule); the
//!   durable-replay path is the `/events` verb, which is complete
//!   without the host.
//! - the extension set is read once at daemon start; changing it is a
//!   daemon restart (config hot-reload is a `/config`-face feature).
//! - `result` frames from a guest answering a delivered call are logged
//!   nowhere yet (no consumer of them before the intercept plane).
//!
//! Hang discipline (the store-lock blood lesson, mirrored): no await
//! happens while holding the store lock — the log read is a short
//! lock-only step; every pipe await carries a timeout, and a timed-out
//! write leaves the cursor un-advanced (the log is the buffer; a stalled
//! extension stalls only itself, by design of the per-session cursor).

use std::collections::HashSet;
use std::os::unix::process::CommandExt as _;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TCommand;

use crate::daemon::Daemon;

/// Handshake + write budgets. Every pipe await uses one of these —
/// nothing in the session loop can hang forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Restart backoff: constant for now (residency accounting is an aura
/// concern; mudra v1 just never hot-loops).
const RESPAWN_DELAY: Duration = Duration::from_secs(10);
/// Rows pulled per epoch change, per session.
const EVENT_BATCH: usize = 256;

/// The parsed hello: name + the subscription set (`schema.on`). Pure
/// decode so the contract is unit-testable without a pipe.
#[derive(Debug, PartialEq, Eq)]
pub struct Hello {
    pub name: String,
    pub on: HashSet<String>,
}

/// Decode a hello line; `None` on any shape violation (bad type, missing
/// fields) — decode failure is the version/contract-mismatch signal,
/// never a silent partial accept.
pub fn parse_hello(line: &str) -> Option<Hello> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v["type"] != "hello" {
        return None;
    }
    let name = v["name"].as_str()?.to_string();
    let on = v["schema"]["on"].as_array()?;
    Some(Hello {
        name,
        on: on.iter().filter_map(Value::as_str).map(str::to_string).collect(),
    })
}

/// The BGI §3 call frame for one log row. `args` was stored as a JSON
/// snapshot string; it rides as a parsed value (self-describing, the
/// consumer never calls back). A corrupt args string ships as a raw
/// string value — the frame must never be withheld by a decode miss.
pub fn call_frame(id: u64, kind: &str, args: &str, at: u64) -> String {
    let parsed: Value = serde_json::from_str(args).unwrap_or_else(|_| Value::String(args.to_string()));
    json!({ "id": id, "kind": "call", "event": kind, "args": { "event_id": id, "at": at, "snapshot": parsed } })
        .to_string()
}

/// Does this frame kind flow to the session? Subscription is string
/// equality over the declared set — free literal names, no wildcard,
/// no namespace routing (the withdrawn mechanisms stay withdrawn).
pub fn matches(on: &HashSet<String>, kind: &str) -> bool {
    on.iter().any(|k| k == kind)
}

/// The `initialize` request the host writes first (protocol 1).
pub fn initialize_frame() -> String {
    json!({ "type": "initialize", "protocol": 1 }).to_string()
}

/// Build the PDEATHSIG'd tokio command from the executable path.
/// `#[allow(unsafe_code)]`: pre_exec via from_std (tokio's builder does
/// not expose it; std's does) — the closure is async-signal-safe prctl
/// only, the same body as spawn::spawn_detached.
#[allow(unsafe_code)]
fn command(path: &str) -> TCommand {
    let mut std_cmd = std::process::Command::new(path);
    unsafe {
        std_cmd.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        })
    };
    TCommand::from(std_cmd)
}

/// One session attempt: spawn, handshake, deliver-loop. Returns when
/// the child dies, stalls past a budget, or the daemon shuts down
/// (epoch channel closed). The supervisor calls it in a retry loop.
pub async fn session(d: &std::sync::Arc<Daemon>, name: &str, path: &str) {
    let mut rx = d.epoch_tx.subscribe();
    let mut child = match command(path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[host:{name}] spawn failed: {e}");
            return;
        }
    };
    let Some(mut stdin) = child.stdin.take() else { return };
    let stdout = match child.stdout.take() {
        Some(o) => o,
        None => return,
    };

    // handshake: initialize out, hello in, budgeted both ways
    if write_line(&mut stdin, &initialize_frame()).await.is_none() {
        eprintln!("[host:{name}] initialize write failed/timeout");
        let _ = child.start_kill();
        return;
    }
    let mut lines = BufReader::new(stdout).lines();
    let hello = match tokio::time::timeout(HANDSHAKE_TIMEOUT, lines.next_line()).await {
        Ok(Ok(Some(line))) => match parse_hello(&line) {
            Some(h) => h,
            None => {
                eprintln!("[host:{name}] rejected hello: {line}");
                let _ = child.start_kill();
                return;
            }
        },
        _ => {
            eprintln!("[host:{name}] handshake timeout");
            let _ = child.start_kill();
            return;
        }
    };
    if hello.name != name {
        eprintln!("[host:{name}] hello name mismatch: {:?}", hello.name);
    }
    eprintln!("[host:{name}] ready (on: {:?})", hello.on);

    // per-session cursor starts at the log head (no history replay, v1)
    let mut cursor = { d.store.lock().await.event_head() };

    loop {
        // stdout is owned by `lines` above; host frames flow here while
        // epoch changes flow below. `select!` keeps both alive.
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
                let mut frames: Vec<(u64, String)> = Vec::new();
                for (id, e) in &rows {
                    if matches(&hello.on, &e.kind) {
                        frames.push((*id, call_frame(*id, &e.kind, &e.args, e.at)));
                    } else {
                        // not for this session: the cursor still moves
                        // past it (subscription filters delivery, not
                        // the log's meaning)
                        cursor = *id;
                    }
                }
                for (id, frame) in &frames {
                    if write_line(&mut stdin, frame).await.is_none() {
                        eprintln!("[host:{name}] write failed at event {id}; stalling session");
                        let _ = child.start_kill();
                        return; // supervisor respawns; cursor = new head
                    }
                    cursor = *id;
                }
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        match route_host_frame(d, &line).await {
                            Some(reply) => {
                                if write_line(&mut stdin, &reply).await.is_none() {
                                    eprintln!("[host:{name}] reply write failed");
                                    break;
                                }
                            }
                            // protocol noise (result / unknown kind):
                            // surface it, never stall the session
                            None => {
                                if !line.contains("\"kind\":\"result\"") && !line.contains("\"kind\": \"result\"") {
                                    eprintln!("[host:{name}] undecodable frame: {line}");
                                }
                            }
                        }
                    }
                    Ok(None) => break, // child closed stdout
                    Err(e) => {
                        eprintln!("[host:{name}] stdout read error: {e}");
                        break;
                    }
                }
            }
        }
    }
    let _ = child.start_kill();
}

/// A guest `host` frame routed through the daemon's verb surface
/// (§5: effects execute in mudra). The only accepted arm is
/// `invoke {verb, args}`; anything else is decode failure. Returns the
/// `host_reply` line to write back, or None for non-host frames.
pub async fn route_host_frame(d: &Arc<Daemon>, line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v["host"]["type"] != "invoke" {
        return None;
    }
    let verb = v["host"]["verb"].as_str()?;
    let args = v["host"]["args"].clone();
    let out = crate::daemon::run_verb(d, verb, args).await;
    Some(match out {
        Ok(value) => json!({ "host_reply": { "ok": value } }).to_string(),
        Err(e) => json!({ "host_reply": { "err": e } }).to_string(),
    })
}

/// Write one line with flush under WRITE_TIMEOUT; `None` on timeout or
/// io error — the session's pipe awaits all funnel through here.
async fn write_line(stdin: &mut tokio::process::ChildStdin, line: &str) -> Option<()> {
    let payload = format!("{line}\n");
    match tokio::time::timeout(WRITE_TIMEOUT, async {
        stdin.write_all(payload.as_bytes()).await?;
        stdin.flush().await
    })
    .await
    {
        Ok(Ok(())) => Some(()),
        _ => None,
    }
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
