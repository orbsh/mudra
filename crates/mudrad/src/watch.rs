//! Instance watcher — the port of `mudrad.py::_watch`: the per-instance
//! task that keeps the pages table in step with CDP reality (CDP is the
//! single source of truth, mudrad.py docstring).
//!
//! Blood-lesson inventory (each maps to a test in `tests/watcher_test.rs`):
//!
//! - never-ready: a just-spawned chromium may not have bound its debug
//!   port yet; retry `attempts` x `delay`, then give up and `_mark_down`
//!   (the Python loop's `never ready; marking down` path — the watch task
//!   ENDS, the main loop may retry the instance later).
//!   Precondition lesson from the join-path bug: liveness was /proc
//!   zombie-safe before reaching here (spawn::pid_alive); the watcher
//!   trusts the port, not the DB flag.
//! - disconnect = instance gone: the event stream ending is the teardown
//!   trigger (Python: recv exception -> finally: _mark_down). Everything
//!   else is handled by store lifecycle ops.
//! - page-type filter: CDP emits non-page targets too (workers, iframes);
//!   `_sync_infos` filtered `type == "page"` before touching the table.
//! - epoch notification: store ops return the new epoch; the watcher
//!   forwards each bump to `notify` — this is the hook where the daemon
//!   sends the panel's invalidation frame (SCHEMA: push only as a hint,
//!   data stays a pure KV read).

use serde_json::{json, Value};
use std::time::Duration;

use mudra_store::{MudraStore, TargetInfo};

use crate::cdp::{self, CdpConn};

/// Wall-clock millis — the daemon owns the clock; lifecycle ops take it
/// as a parameter so the store stays pure.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Connect with retries — the race guard for a freshly spawned instance
/// (port not yet bound). `None` after `attempts` failures: the caller
/// must `_mark_down` and stop (never spin forever on a corpse).
pub async fn connect_ready(
    port: u16,
    attempts: u32,
    delay: Duration,
) -> Option<CdpConn> {
    for _ in 0..attempts {
        if let Ok(ws_url) = cdp::browser_ws(port).await
            && let Ok(conn) = CdpConn::connect(&ws_url).await
        {
            return Some(conn);
        }
        tokio::time::sleep(delay).await;
    }
    None
}

/// Filter a `targetInfos` array (event or getTargets shape) down to the
/// page snapshots the store lifecycle consumes.
pub fn page_infos(arr: &Value) -> Vec<TargetInfo> {
    arr.as_array()
        .map(|rows| {
            rows.iter()
                .filter(|t| t.get("type").and_then(Value::as_str) == Some("page"))
                .map(cdp::target_info_of)
                .collect()
        })
        .unwrap_or_default()
}

/// Run the watcher loop to completion. `inject` is called per page target
/// (baseline + created) with (port, target_id, ctx) — the daemon wires the
/// new-window interception script; tests record. `notify` receives every
/// bumped epoch (panel invalidation frames). Returns when the event
/// stream ends or a command fails; either way the instance is marked down
/// and pages closed before returning.
pub async fn run_watcher<Inject, Notify>(
    store: &mut MudraStore,
    instance_id: u32,
    port: u16,
    ctx: &str,
    conn: &CdpConn,
    inject: &mut Inject,
    notify: &mut Notify,
) where
    Inject: FnMut(u16, &str, &str),
    Notify: FnMut(u64),
{
    // baseline: full sync once, then enable discovery (Python order kept —
    // subscribing first would double-sync targets the baseline already has)
    match conn.call("Target.getTargets", json!({})).await {
        Ok(r) => {
            let infos = page_infos(&r["targetInfos"]);
            for t in &infos {
                inject(port, &t.target_id, ctx);
            }
            if let Some(epoch) = store.sync_targets(instance_id, &infos, now_ms()) {
                notify(epoch);
            }
        }
        Err(_) => {
            teardown(store, instance_id, notify);
            return;
        }
    }
    if let Err(e) = conn
        .call("Target.setDiscoverTargets", json!({"discover": true}))
        .await
    {
        eprintln!("[watcher] {ctx}: discovery failed: {e}");
        teardown(store, instance_id, notify);
        return;
    }

    // event loop until the socket dies
    while let Some(ev) = conn.next_event().await {
        match ev.get("method").and_then(Value::as_str) {
            Some("Target.targetCreated") | Some("Target.targetInfoChanged") => {
                let infos = ev
                    .get("params")
                    .and_then(|p| p.get("targetInfo"))
                    .map(|t| {
                        if t.get("type").and_then(Value::as_str) == Some("page") {
                            page_infos(&Value::Array(vec![t.clone()]))
                        } else {
                            Vec::new()
                        }
                    })
                    .unwrap_or_default();
                if infos.is_empty() {
                    continue;
                }
                if ev.get("method").and_then(Value::as_str) == Some("Target.targetCreated") {
                    inject(port, &infos[0].target_id, ctx);
                }
                if let Some(epoch) = store.sync_targets(instance_id, &infos, now_ms()) {
                    notify(epoch);
                }
            }
            Some("Target.targetDestroyed") => {
                let target_id = ev
                    .get("params")
                    .and_then(|p| p.get("targetId"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(epoch) = store.close_target(instance_id, target_id, now_ms()) {
                    notify(epoch);
                }
            }
            _ => {} // other events are not page lifecycle
        }
    }
    teardown(store, instance_id, notify);
}

/// The unified teardown: instance down, all pages closed, one notify.
fn teardown<Notify: FnMut(u64)>(store: &mut MudraStore, instance_id: u32, notify: &mut Notify) {
    if let Some(epoch) = store.mark_down(instance_id, now_ms()) {
        notify(epoch);
    }
}
