//! Daemon-assembly tests: the pieces that can be exercised without a
//! full desktop session — the flock singleton, static path routing, and
//! the session-env gate. The HTTP/WS servers themselves are exercised
//! through these same handlers where the logic is testable.
//! Run: `cargo test -p mudrad --test daemon_test`.

use mudrad::daemon::{acquire_lock, config_default_path, resolve_static_path, startup_env_gaps, WS_PORT, PANEL_PORT, CONTROL_PORT};

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

// ================= static routing (wasm dist root) =================

#[test]
fn dist_root_serves_the_bundle_flat() {
    // Contract (slice 2 switch): the served panel is the trunk build.
    // Root maps to index.html; assets resolve directly under the dist
    // root — the old /shared/* frontend remap is gone (the wasm bundle
    // is self-contained).
    let root = std::path::Path::new("/dist");
    assert_eq!(resolve_static_path(root, "/"), Some(root.join("index.html")));
    assert_eq!(resolve_static_path(root, ""), Some(root.join("index.html")));
    assert_eq!(resolve_static_path(root, "/styles.css"), Some(root.join("styles.css")));
    assert_eq!(
        resolve_static_path(root, "/mudra-panel_bg.wasm"),
        Some(root.join("mudra-panel_bg.wasm"))
    );
    // /shared/* is now just another dist path, never the frontend tree
    assert_eq!(resolve_static_path(root, "/shared/lib.js"), Some(root.join("shared/lib.js")));
}

#[test]
fn traversal_is_denied() {
    // Regression: `..` must be rejected outright — canonicalize would
    // chase symlinks out of the served tree (the store lives under the
    // same home).
    let root = std::path::Path::new("/dist");
    assert_eq!(resolve_static_path(root, "/../Cargo.toml"), None);
    assert_eq!(resolve_static_path(root, "/a/../../b"), None);
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

#[test]
fn default_config_follows_the_frontend_dir_not_the_data_dir() {
    // Regression: deriving the default layer from MUDRA_HOME looked for
    // `<data>/config.kdl`, which the deployment never ships — GET /config
    // answered 500 and the extension gate (host::supervise) refused every
    // extension. The default config ships beside `frontend/`, and the NixOS
    // install points MUDRA_FRONTEND_DIR into the config dir.
    let home = std::path::Path::new("/home/u/.local/share/mudra");
    assert_eq!(
        config_default_path(std::path::Path::new("/home/u/.config/mudra/frontend"), home),
        std::path::PathBuf::from("/home/u/.config/mudra/config.kdl")
    );
    // dev fallback (no MUDRA_FRONTEND_DIR): frontend = <home>/frontend, so
    // the derivation degrades to the Python-era <home>/config.kdl
    assert_eq!(
        config_default_path(&home.join("frontend"), home),
        home.join("config.kdl")
    );
    // a path with no parent carries no sibling: fall back instead of panicking
    assert_eq!(
        config_default_path(std::path::Path::new("/"), home),
        home.join("config.kdl")
    );
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

// ================= ns 9: hook sources (the injection bridge) =================

#[test]
fn hook_sources_guard_composes_without_dynamic_code() {
    // Contract (SCHEMA ns 9): one guarded source per hook, composed by
    // literal substitution — the code is embedded, never eval'd (strict-site
    // CSP forbids dynamic code; the injected source itself is not subject to
    // it), and every guard string is a JSON literal, i.e. an escaped JS one.
    use mudra_store::Hook;
    use mudrad::daemon::hook_source;

    let h = Hook {
        name: "adfree".into(),
        matches: "infoq.com, qbit".into(),
        code: "kill();".into(),
        enabled: 1,
        updated_at: 0,
    };
    let src = hook_source(&h);
    assert!(
        src.contains(r#"u.indexOf("infoq.com")>=0||u.indexOf("qbit")>=0"#),
        "one trimmed guard per entry: {src}"
    );
    assert!(src.contains("kill();"), "the code rides verbatim: {src}");
    assert!(
        src.contains(r#""[mudra:hook] "+"adfree""#),
        "the name rides as a JS string literal: {src}"
    );
    assert!(
        !src.contains("eval(") && !src.contains("new Function"),
        "no dynamic code: {src}"
    );

    // empty / separator-only match lists mean every page
    let all = hook_source(&Hook { matches: " , ".into(), ..h.clone() });
    assert!(all.contains("if(true)"), "{all}");
    let none = hook_source(&Hook { matches: String::new(), ..h.clone() });
    assert!(none.contains("if(true)"), "{none}");

    // a quote in a match cannot break out of the guard (JSON string literal
    // is the escaper)
    let q = hook_source(&Hook { matches: r#"a"b"#.into(), ..h.clone() });
    assert!(q.contains(r#"u.indexOf("a\"b")>=0"#), "{q}");
}
