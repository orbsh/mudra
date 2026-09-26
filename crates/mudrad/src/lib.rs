//! mudrad — the single control point for mudra browser mode (Rust, R1).
//!
//! Module map (PLAN §11 deliverables; each lands with its blood-lesson
//! tests ported from the Python tree, which stays as the behavioral
//! spec until R2):
//!
//! - `store`    — MudraStore open + state transitions (writes bump epoch)
//! - `spawn`    — chromium launch: PDEATHSIG, SingletonLock recovery,
//!   WAYLAND env injection, 5 extension cache points, dev_mode
//! - `cdp`      — CDP over WS: target lifecycle -> page upsert/close,
//!   zombie /proc liveness probe (never os.kill(pid,0))
//! - `control`  — HTTP verbs /open /add /close_page /close_ctx /ctx
//! - `panel`    — static file server + WS frame endpoint
//!   (NestStorage::apply + epoch invalidation hint frames)

pub fn _r1_skeleton() {
    // The crate family starts here; SCHEMA.md is implemented in mudra-store.
    let _ = mudra_store::url_site("https://example.com/a/b");
}
