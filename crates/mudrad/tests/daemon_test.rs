//! Daemon-assembly tests: the pieces that can be exercised without a
//! full desktop session — the flock singleton, static path routing, and
//! the session-env gate. The HTTP/WS servers themselves are exercised
//! through these same handlers where the logic is testable.
//! Run: `cargo test -p mudrad --test daemon_test`.

use mudrad::daemon::{acquire_lock, startup_env_gaps, WS_PORT, PANEL_PORT, CONTROL_PORT};

// ================= flock singleton =================

#[test]
fn second_daemon_is_refused_while_the_lock_holder_lives() {
    // Contract: the lock handle keeps the flock; dropping it releases.
    // Two live mudrad processes must never share a store (double-sync,
    // instance-row drift — the Python lesson), so the second acquire
    // fails while the first handle is alive.
    let dir = tempfile::TempDir::new().unwrap();
    let a = acquire_lock(dir.path()).expect("first lock");
    let b = acquire_lock(dir.path());
    assert!(b.is_err(), "second acquire must be refused");
    assert!(b.unwrap_err().contains("already running"));
    drop(a); // release
    // and re-acquire works after the holder dies (crash recovery: the
    // kernel drops flocks with the fd)
    let c = acquire_lock(dir.path()).expect("re-acquire after drop");
    drop(c);
}

// ================= static routing constants =================

#[test]
fn port_layout_matches_the_python_tree() {
    // Contract: extensions and panels hardcode these; a silent shift
    // breaks the frontend (the beacons and ws urls are literals in JS).
    assert_eq!(CONTROL_PORT, 8899);
    assert_eq!(PANEL_PORT, 9299);
    assert_eq!(WS_PORT, 9300);
}

#[test]
fn inject_script_carries_the_wired_placeholders_and_convergence_rules() {
    // Contract (the mouse-click convergence): the beacon posts to the
    // control port, all left clicks route through /open, same-page
    // anchors jump in place. These exact strings are load-bearing for
    // the extension and every spawned page.
    let js = mudrad::daemon::INJECT_JS;
    assert!(js.contains("http://127.0.0.1:8899/open"), "beacon target");
    assert!(js.contains("__CTX__"), "ctx placeholder for replacement");
    assert!(js.contains("a.pathname === location.pathname"), "anchor in-place rule");
    assert!(js.contains("e.preventDefault(); openUrl(a.href);"), "all-clicks convergence");
    assert!(js.contains("window.__mudraInjected"), "double-injection guard");
}

// ================= GET /config wiring =================

#[test]
fn config_endpoint_returns_loaded_layers_on_200() {
    // Contract: the extension's syncConfig parses {ok, config}; the
    // wiring must expose the merged layers, not the old empty placeholder
    // (an empty config silently degrades to built-in defaults — a broken
    // wiring would be invisible).
    let dir = tempfile::TempDir::new().unwrap();
    let default = dir.path().join("default.kdl");
    std::fs::write(&default, "bar {\n    height 16\n}\n").unwrap();
    let user = dir.path().join("user.kdl"); // does not exist: optional layer
    let (status, body): (u16, String) = mudrad::daemon::config_response(&default, &user);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["config"]["statusHeight"], serde_json::json!(16));
}

#[test]
fn config_endpoint_surfaces_broken_file_as_500() {
    // A parse error must be loud (Python's `config: {e}` shape) — never
    // a 200 with an empty config the extension would silently ignore.
    let dir = tempfile::TempDir::new().unwrap();
    let default = dir.path().join("default.kdl");
    std::fs::write(&default, "bar { font \"unterminated }\n").unwrap(); // string never closes
    let (status, body) = mudrad::daemon::config_response(&default, &dir.path().join("no-user.kdl"));
    assert_eq!(status, 500);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["ok"], serde_json::json!(false));
    let err = v["err"].as_str().unwrap();
    assert!(err.starts_with("config: "), "{err}");
}

// ================= session env gate =================

#[test]
fn env_gate_uses_the_full_session_var_set() {
    // Contract: the gate consults every WAYLAND-era var (the real box
    // outside a graphical session may report gaps); the four names are
    // the fixed set the Chromium lessons established — a fifth missing
    // name means someone added an env requirement without a lesson.
    assert_eq!(mudrad::spawn::SESSION_ENV, [
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
    ]);
    let gaps = startup_env_gaps();
    assert!(gaps.len() <= 4, "subset of the set: {gaps:?}");
}
