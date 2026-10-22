//! Live extension-host e2e — the pipe loop against a REAL child process and
//! the real store/verb surface, the same planes the v1 smoke (2026-10-10)
//! exercised by hand, now a resident test: spawn + `initialize`/`hello`
//! handshake with the script-carried schema (§3), event-log fan-out on epoch
//! changes with the head-starting cursor (§4), and `host:invoke` routed to
//! `run_verb` with the canonical `ok` reply key (§5).
//! Run: `cargo test -p mudrad --test host_live_test`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{Mutex, watch};

use mudra_store::MudraStore;

/// The fixture guest: a bin of THIS package, so cargo links it for these
/// integration tests and exports its path — no manual build step, and the
/// test can never run against a stale fixture.
fn fixture_guest() -> &'static str {
    env!("CARGO_BIN_EXE_induction_guest_fixture")
}

/// A daemon assembled for tests: real fjall store (tempdir), real verb
/// surface, no chromium/browser runtime touches (the Real runtime is
/// constructed but never called on this path).
async fn test_daemon(home: &std::path::Path) -> Arc<mudrad::daemon::Daemon> {
    let store = MudraStore::open(&home.join("store")).unwrap();
    let real = mudrad::runtime::Real {
        profiles_dir: home.join("profiles"),
        default_extensions: Vec::new(),
        niri_socket: None,
        dev_mode: false,
    };
    // bare receiver, same shape as daemon::main (the nest plane is not
    // exercised here; it is assembled so the Daemon is the real shape)
    let (nest_host, nest_handle) = okm_core::NestStorage::new(store.engine(), &[]);
    let nest = nest_host.serve();
    // the handle keeps the pump thread alive; `serve` consumed the host.
    // Leak the handle so the pump outlives this helper (the test process
    // is short-lived either way)
    std::mem::forget(nest_handle);

    let (epoch_tx, _) = watch::channel(0u64);
    Arc::new(mudrad::daemon::Daemon {
        store: Arc::new(Mutex::new(store)),
        rt: Arc::new(Mutex::new(real)),
        epoch_tx,
        watched: Arc::new(Mutex::new(std::collections::HashMap::new())),
        host: Arc::new(mudrad::host::Registry::default()),
        frontend_dir: home.join("frontend"),
        panel_root: home.join("panel-dist"),
        config_default: home.join("config.kdl"),
        nest,
        pending_parent: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    })
}

async fn wait_for_file(path: &str, seconds: u64) -> Option<String> {
    for _ in 0..(seconds * 10) {
        if let Ok(s) = std::fs::read_to_string(path)
            && !s.is_empty()
        {
            return Some(s);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

#[tokio::test]
async fn a_real_extension_handshakes_receives_fanout_and_invokes_a_verb() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let d = test_daemon(home).await;

    // stale fixture outputs from a previous run must not satisfy the poll
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/induction-live");
    for p in [
        format!("{dir}/seen.json"),
        format!("{dir}/out.json"),
        format!("{dir}/nav.json"),
    ] {
        let _ = std::fs::remove_file(&p);
    }

    // a DECLARED event emitted BEFORE the session exists: the cursor rule
    // (v1 boundary — no history replay) says it never reaches this session
    {
        let mut store = d.store.lock().await;
        store.emit_event(
            "mudra:page_open",
            json!({ "url": "https://before.test" }).to_string(),
            1,
        );
    }

    // the session under test: spawn → handshake → deliver loop
    let d2 = Arc::clone(&d);
    let path = fixture_guest().to_string();
    let session_task =
        tokio::spawn(async move { mudrad::host::session(&d2, "probe", &path).await });
    // let the handshake complete and the cursor snapshot at the head
    tokio::time::sleep(Duration::from_millis(300)).await;

    // §4 intercept plane, against the REAL child: the fixture answers `deny`
    // for urls carrying "blocked" and `allow` otherwise, echoing the id the
    // host assigned (the guest never picks the id).
    let (verdict, by) =
        mudrad::host::before_navigate(&d.host, "https://blocked.test/x", "work").await;
    assert_eq!(verdict, mudrad::host::Verdict::Deny);
    assert_eq!(by.as_deref(), Some("probe"));

    let nav = wait_for_file(&format!("{dir}/nav.json"), 5)
        .await
        .expect("the guest must receive the verdict call");
    let f: Value = serde_json::from_str(&nav).unwrap();
    assert_eq!(f["event"], "mudra:before_navigate");
    assert_eq!(f["args"]["url"], "https://blocked.test/x");
    assert_eq!(f["args"]["ctx"], "work");
    assert!(
        f["id"].as_u64().unwrap() >= (1u64 << 62),
        "host-assigned id space (disjoint from event-log ids): {}",
        f["id"]
    );

    // the same session answering the other way: allow, and nothing blocks
    let (verdict, by) =
        mudrad::host::before_navigate(&d.host, "https://fine.test/", "inbox").await;
    assert_eq!(verdict, mudrad::host::Verdict::Allow);
    assert_eq!(by, None);

    // the request-level policy the 8899 shell applies: `/open` and `/nav
    // goto` are gated; a deny answers WITHOUT executing the verb and a
    // redirect rewrites the body (the gate never touches the store).
    let (req, blocked) =
        mudrad::host::intercept(&d, "/open", json!({ "url": "https://blocked.test/x" })).await;
    assert_eq!(req["url"], "https://blocked.test/x");
    let blocked = blocked.expect("a deny must produce the short-circuit reply");
    assert_eq!(blocked["blocked"], true);
    assert_eq!(blocked["by"], "probe");

    let (req, blocked) = mudrad::host::intercept(
        &d,
        "/open",
        json!({ "url": "https://redirect-me.test/x" }),
    )
    .await;
    assert!(blocked.is_none(), "a redirect executes, it does not short-circuit");
    assert_eq!(req["url"], "https://safe.test/");

    let (_, blocked) = mudrad::host::intercept(
        &d,
        "/nav",
        json!({ "cmd": "goto", "url": "https://blocked.test/y" }),
    )
    .await;
    assert!(blocked.is_some(), "`goto` is a navigation entry too");

    // the other nav commands carry no url: never gated
    let (_, blocked) = mudrad::host::intercept(&d, "/nav", json!({ "cmd": "reload" })).await;
    assert!(blocked.is_none());

    // and a verb that is not a navigation entry is untouched
    let (_, blocked) = mudrad::host::intercept(&d, "/forest", json!({})).await;
    assert!(blocked.is_none());

    // the live event, then the epoch change that triggers the pull
    {
        let mut store = d.store.lock().await;
        store.emit_event(
            "mudra:page_open",
            json!({ "url": "https://live.test" }).to_string(),
            2,
        );
    }
    d.epoch_tx.send(1).expect("epoch receiver alive");

    // the guest saw EXACTLY the live event: fan-out works, and the
    // pre-head event did not replay into the session
    let seen = wait_for_file(&format!("{dir}/seen.json"), 10)
        .await
        .expect("the guest must receive the delivered call");
    assert!(seen.contains("live.test"), "seen: {seen}");
    assert!(!seen.contains("before.test"), "no history replay; seen: {seen}");

    // §5 round trip: the host_reply came back through run_verb with the
    // canonical `ok` key and the real event log behind it
    let reply = wait_for_file(&format!("{dir}/out.json"), 10)
        .await
        .expect("the guest must receive its host_reply");
    let reply: Value = serde_json::from_str(&reply).unwrap();
    let events = reply["host_reply"]["ok"]["events"]
        .as_array()
        .expect("ok arm with the /events payload");
    assert!(
        events
            .iter()
            .any(|e| e["kind"] == "mudra:page_open" && e["args"]["url"] == "https://live.test"),
        "events: {events:?}"
    );

    // the session ends on its own: the child exited after answering
    let _ = tokio::time::timeout(Duration::from_secs(5), session_task).await;
}
