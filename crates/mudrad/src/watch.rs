//! Instance watcher — the port of `mudrad.py::_watch`: the per-instance
//! session that keeps the pages table in step with CDP reality (CDP is
//! the single source of truth, mudrad.py docstring).
//!
//! Shape: `WatchSession` drives ONE step at a time — `baseline` (the
//! getTargets sync, awaited commands, short store lock per write) and
//! `on_event` (pure store writes, no await inside). The awaiting event
//! loop belongs to the daemon task: the store mutex must never be held
//! across an await (a watcher parked on events would starve the HTTP
//! verbs of the single writer — the async+lock discipline).
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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mudra_store::{MudraStore, TargetInfo, PageKey};

use crate::cdp::{self, CdpConn};

/// A window `/open` created on behalf of a page, waiting for its CDP target
/// to appear. CDP reports no `openerId` for a window mudrad spawns itself, so
/// the page tree's parent link can only come from here: `/open` knows who
/// asked, the watcher is the one that sees the target land.
pub struct PendingParent {
    pub port: u16,
    pub url: String,
    pub parent: u64,
    pub ts: u64,
}

/// How long a hand-off stays meaningful. A window slower than this simply
/// lands flat — the pre-parent-link behaviour, never a wrong parent.
const PENDING_TTL_MS: u64 = 10_000;

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
///
/// Each attempt is capped at `delay` by an explicit timeout: the budget
/// is only real if a HUNG attempt (connect completes, response framing
/// stalls) counts as a failure instead of parking the loop forever. The
/// chromium-keeps-connection-open lesson is the reason this cap exists.
pub async fn connect_ready(
    port: u16,
    attempts: u32,
    delay: Duration,
) -> Option<CdpConn> {
    for _ in 0..attempts {
        let attempt = async {
            let ws_url = cdp::browser_ws(port).await.ok()?;
            CdpConn::connect(&ws_url).await.ok()
        };
        if let Ok(Some(conn)) = tokio::time::timeout(delay, attempt).await {
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

/// One instance's watch session. Steps take `&mut MudraStore` for the
/// duration of one write burst only — the daemon's event loop awaits
/// OUTSIDE any store borrow.
pub struct WatchSession {
    pub instance_id: u32,
    pub port: u16,
    pub ctx: String,
    /// `/open`'s hand-off queue (shared with the controller); a session
    /// without it just never assigns parent links by url.
    pub pending: Option<Arc<Mutex<Vec<PendingParent>>>>,
    discovery_enabled: bool,
}

/// Effect of one processed CDP event.
pub enum Step {
    /// The event was handled; zero or more epochs were notified (the
    /// daemon already pushed the frames — bookkeeping for tests/logs).
    Handled { epochs: Vec<u64> },
    /// Not a page-lifecycle event.
    Ignored,
}

/// The store's typed refusal, in the watcher's `String` error channel
/// (the same mapping the verb layer uses — ADR-0037 keeps the sentence).
fn serr(e: mudra_store::StoreError) -> String {
    e.to_string()
}

impl WatchSession {
    pub fn new(instance_id: u32, port: u16, ctx: &str) -> Self {
        WatchSession { instance_id, port, ctx: ctx.to_string(), pending: None, discovery_enabled: false }
    }

    /// Phase 1 (awaits only, never touches the store): fetch the
    /// baseline snapshot and arm discovery. Err = the instance died
    /// between connect and first use: the caller tears down and stops.
    /// Splitting the awaits from the writes keeps the store lock off
    /// every await point (async+lock discipline).
    pub async fn start(&mut self, conn: &CdpConn) -> Result<Vec<TargetInfo>, crate::cdp::CdpError> {
        let r = conn.call("Target.getTargets", json!({})).await?;
        let infos = page_infos(&r["targetInfos"]);
        conn.call("Target.setDiscoverTargets", json!({"discover": true}))
            .await?;
        self.discovery_enabled = true;
        Ok(infos)
    }

    /// Phase 2 (writes only, no awaits): apply the baseline snapshot —
    /// inject per page (interception script), one batched sync, one bump.
    pub fn apply_baseline<F, N>(&self, store: &mut MudraStore, infos: &[TargetInfo], inject: &mut F, notify: &mut N) -> Result<Option<u64>, String>
    where
        F: FnMut(u16, &str, &str),
        N: FnMut(u64),
    {
        for t in infos {
            inject(self.port, &t.target_id, &self.ctx);
        }
        // absence (an empty batch wrote nothing) stays `None`; a refused
        // write is the `Err` the caller must act on
        let Some(epoch) = store.sync_targets(self.instance_id, infos, now_ms(), false).map_err(serr)? else {
            return Ok(None);
        };
        notify(epoch);
        Ok(Some(epoch))
    }

    /// Apply one event envelope to the store (no awaits inside). The
    /// daemon feeds it while holding the store lock, then releases.
    pub fn on_event<F, N>(&self, store: &mut MudraStore, ev: &Value, inject: &mut F, notify: &mut N) -> Result<Step, String>
    where
        F: FnMut(u16, &str, &str),
        N: FnMut(u64),
    {
        let mut epochs = Vec::new();
        match ev.get("method").and_then(Value::as_str) {
            Some("Target.targetCreated") | Some("Target.targetInfoChanged") => {
                let created = ev.get("method").and_then(Value::as_str) == Some("Target.targetCreated");
                let infos = ev
                    .get("params")
                    .and_then(|p| p.get("targetInfo"))
                    .filter(|t| t.get("type").and_then(Value::as_str) == Some("page"))
                    .map(|t| page_infos(&Value::Array(vec![t.clone()])))
                    .unwrap_or_default();
                if infos.is_empty() {
                    return Ok(Step::Ignored);
                }
                if created {
                    inject(self.port, &infos[0].target_id, &self.ctx);
                }
                if let Some(e) = store.sync_targets(self.instance_id, &infos, now_ms(), true).map_err(serr)? {
                    epochs.push(e);
                    notify(e);
                }
                // a window mudrad spawned for a page: CDP has no openerId for
                // it, so take /open's hand-off (url + port) as the parent link
                self.link_pending(store, &infos[0])?;
            }
            Some("Target.targetDestroyed") => {
                let target_id = ev
                    .get("params")
                    .and_then(|p| p.get("targetId"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(e) = store.close_target(self.instance_id, target_id, now_ms()).map_err(serr)? {
                    epochs.push(e);
                    notify(e);
                }
            }
            _ => return Ok(Step::Ignored),
        }
        Ok(Step::Handled { epochs })
    }

    /// Claim `/open`'s hand-off for a freshly created page: the window the
    /// controller asked for (same port, same url, recent) becomes a child of
    /// the page that asked for it.
    ///
    /// Only fires when CDP gave no openerId — a page opened by JS already has
    /// its parent from `sync_targets`, and guessing there would overwrite
    /// real knowledge with a url match. Never fatal: an unmatched or stale
    /// hand-off just leaves the row flat, which is what it used to be.
    fn link_pending(&self, store: &mut MudraStore, info: &TargetInfo) -> Result<(), String> {
        if !info.opener_id.is_empty() {
            return Ok(());
        }
        let Some(queue) = &self.pending else { return Ok(()) };
        let parent = {
            let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
            let now = now_ms();
            q.retain(|p| now.saturating_sub(p.ts) < PENDING_TTL_MS);
            q.iter()
                .rposition(|p| p.port == self.port && p.url == info.url)
                .map(|i| q.remove(i).parent)
        };
        let Some(parent) = parent else { return Ok(()) };
        let Some((child, _)) = store.page_by_target(&info.target_id).map_err(serr)? else {
            return Ok(());
        };
        store.set_parent_once(&child, &PageKey { id: parent }).map_err(serr)?;
        Ok(())
    }

    /// The unified teardown: instance down, all pages closed, one notify.
    /// Public so the daemon's stream-end branch (event loop exits) can
    /// call it without consuming the session.
    pub fn teardown<N: FnMut(u64)>(store: &mut MudraStore, instance_id: u32, notify: &mut N) -> Result<(), String> {
        if let Some(epoch) = store.mark_down(instance_id, now_ms()).map_err(serr)? {
            notify(epoch);
        }
        Ok(())
    }

    /// Whether the baseline finished and discovery is live (daemon's
    /// bookkeeping for reconnect decisions).
    pub fn ready(&self) -> bool {
        self.discovery_enabled
    }
}
