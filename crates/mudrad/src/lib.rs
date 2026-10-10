//! mudrad — the single control point for mudra browser mode (Rust, R1).
//!
//! Module map (PLAN §11 deliverables; each lands with its blood-lesson
//! tests ported from the Python tree, which stays as the behavioral
//! spec until R2):
//!
//! - `store`    — MudraStore open + state transitions (writes bump epoch)
//!   (the mudra-store crate; SCHEMA.md is the layout contract)
//! - `spawn`    — chromium launch: PDEATHSIG, SingletonLock recovery,
//!   WAYLAND env injection, 5 extension cache points, dev_mode
//! - `cdp`      — CDP over WS: single-reader routing, /json* framed GETs
//!   (Content-Length, never EOF-wait — the chromium keep-alive lesson)
//! - `watch`    — per-instance WatchSession: stepped API so the store
//!   lock never spans an await (baseline/event/teardown)
//! - `control`  — HTTP verbs /open /add /close_page /close_ctx /ctx
//!   (/tag /tags /pages /focus_page /ctx_status) over the Runtime seam
//! - `config`   — KDL loader: two-layer merge, per-key keybindings,
//!   typed int reads (port of mudralib/config.py)
//! - `daemon`   — assembly: flock singleton, env gate, control/static/WS
//!   servers, watcher scheduler, capsule SSR passthrough
//! - `host`     — extension host: BGI sessions over stdio (ADR-
//!   extension-protocol): spawn+PDEATHSIG, initialize/hello, event-log
//!   fan-out on epoch changes, host:invoke frames routed to run_verb

pub mod cdp;
pub mod config;
pub mod control;
pub mod daemon;
pub mod host;
pub mod runtime;
pub mod spawn;
pub mod watch;

pub fn _r1_skeleton() {
    // The crate family starts here; SCHEMA.md is implemented in mudra-store.
    let _ = mudra_store::url_site("https://example.com/a/b");
}
