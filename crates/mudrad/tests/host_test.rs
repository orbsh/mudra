//! Extension-host tests — what mudra owns of the §2–§5 contract: the event
//! row → call-frame shaping, the §5 arm surface (verb routing, booth-shape
//! refusal), and the config face (§1). The frame vocabulary, hello decode,
//! budgeted I/O and the live pipe are `induction-*`'s — covered there
//! (protocol wire tests + host session tests, including a live child).
//! Run: `cargo test -p mudrad --test host_test`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unwrap_in_result, clippy::panic_in_result_fn)]

use mudrad::host::event_row_frame;
use serde_json::{Value, json};

// ================= the event row → call frame =================

#[test]
fn event_row_becomes_a_call_frame_with_the_parsed_snapshot() {
    // Contract (§3 + the args discipline): id/event ride the frame, the
    // stored JSON snapshot arrives PARSED — a consumer never
    // double-decodes. An unparseable args string still ships (as a
    // string), never withheld by a decode miss.
    let f = event_row_frame(7, "mudra:page_open", r#"{"url":"https://a.test","ctx":"work"}"#, 1000);
    let v: Value = serde_json::from_str(&f.to_line()).unwrap();
    assert_eq!(v["id"], 7);
    assert_eq!(v["kind"], "call");
    assert_eq!(v["event"], "mudra:page_open");
    assert_eq!(v["args"]["event_id"], 7);
    assert_eq!(v["args"]["at"], 1000);
    assert_eq!(v["args"]["snapshot"]["url"], "https://a.test");
    assert_eq!(v["args"]["snapshot"]["ctx"], "work");

    let broken = event_row_frame(8, "x", "not-json{", 2000);
    let v: Value = serde_json::from_str(&broken.to_line()).unwrap();
    assert_eq!(v["args"]["snapshot"], "not-json{");
}

// ================= §5: the arm surface mudra serves =================

/// The daemon-free routing check: a `VerbOps`-shaped table over a stub
/// verb runner. The daemon-coupled path (run_verb) is the live smoke's
/// job; this locks the arm RULES.
#[tokio::test]
async fn invoke_arm_routes_verbs_and_refuses_the_booth_shape() {
    use induction_host::{HostOps, InvokeCall, OpsFuture};
    use std::sync::Arc;

    struct Ops {
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl HostOps for Ops {
        fn invoke<'a>(&'a self, call: &'a InvokeCall) -> OpsFuture<'a> {
            Box::pin(async move {
                match call {
                    InvokeCall::Verb { verb, args } => {
                        self.seen.lock().unwrap().push(verb.clone());
                        Ok(json!({ "echoed": verb, "args": args }))
                    }
                    InvokeCall::Booth { .. } => {
                        Err("mudra hosts no booths: the invoke arm's flat `verb` shape is the one this host serves".to_string())
                    }
                }
            })
        }
    }

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ops = Ops { seen: Arc::clone(&seen) };

    // flat verb shape: served
    let line = json!({ "host": { "type": "invoke", "verb": "/tags", "args": { "p": 1 } } }).to_string();
    match induction_host::route_line(&ops, &line).await {
        induction_host::Routed::Reply(reply) => {
            let v: Value = serde_json::from_str(&reply.to_line()).unwrap();
            // canonical error key per ADR-0001 (the old `err` spelling is gone)
            assert!(v["host_reply"].get("ok").is_some(), "reply: {reply:?}");
            assert_eq!(v["host_reply"]["ok"]["echoed"], "/tags");
        }
        other => panic!("expected Reply, got {other:?}"),
    }
    assert_eq!(seen.lock().unwrap().last().unwrap(), "/tags");

    // nested booth shape: an explicit error value, never silence
    let line = json!({ "host": { "type": "invoke", "args": { "type": "echo", "key": "k", "handler": "h", "args": {} } } }).to_string();
    match induction_host::route_line(&ops, &line).await {
        induction_host::Routed::Reply(reply) => {
            let v: Value = serde_json::from_str(&reply.to_line()).unwrap();
            assert!(v["host_reply"].get("error").is_some(), "reply: {reply:?}");
        }
        other => panic!("expected Reply, got {other:?}"),
    }
}

#[tokio::test]
async fn store_and_emit_arms_behave_per_contract() {
    use induction_host::{HostOps, InvokeCall, OpsFuture, Routed, route_line};
    use std::sync::Arc;

    struct Ops {
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl HostOps for Ops {
        fn invoke<'a>(&'a self, _call: &'a InvokeCall) -> OpsFuture<'a> {
            unreachable!("invoke is not exercised in this test")
        }
        fn emit(&self, event: &str, _data: &Value) -> Result<(), String> {
            self.seen.lock().unwrap().push(event.to_string());
            Ok(())
        }
    }

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ops = Ops { seen: Arc::clone(&seen) };

    // emit: accepted, NO reply frame by contract (fire-and-forget)
    let line = json!({ "host": { "type": "emit", "args": { "event": "ext:note", "data": { "x": 1 } } } }).to_string();
    assert!(matches!(route_line(&ops, &line).await, Routed::Accepted));

    // store: mudra declares no store surface (ADR-extension-protocol) —
    // the library's default declines with an explicit error value
    let line = json!({ "host": { "type": "store", "op": { "collection": "x", "op_kind": "put" } } }).to_string();
    match route_line(&ops, &line).await {
        Routed::Reply(reply) => {
            let v: Value = serde_json::from_str(&reply.to_line()).unwrap();
            assert!(v["host_reply"].get("error").is_some(), "reply: {reply:?}");
        }
        other => panic!("expected Reply, got {other:?}"),
    }
}

// ================= config: the [extensions] group =================

#[test]
fn extensions_group_parses_and_merges_per_name() {
    // Contract (§1): routing metadata only — name -> executable path;
    // the user layer adds/overrides entries without dropping the other
    // names (the keybindings merge rule, generalized).
    let dir = tempfile::tempdir().unwrap();
    let default = dir.path().join("default.kdl");
    std::fs::write(
        &default,
        r#"extensions {
    k10r-sync "/usr/local/bin/mudra-k10r"
    adfree "/opt/ads/block"
}
"#,
    )
    .unwrap();
    let user = dir.path().join("user.kdl");
    std::fs::write(
        &user,
        r#"extensions {
    adfree "/home/me/adfree"
    restyler "/home/me/restyle"
}
"#,
    )
    .unwrap();
    let cfg = mudrad::config::load(&default, &user).unwrap();
    assert_eq!(cfg["extensions"]["k10r-sync"], "/usr/local/bin/mudra-k10r");
    assert_eq!(cfg["extensions"]["adfree"], "/home/me/adfree"); // user wins
    assert_eq!(cfg["extensions"]["restyler"], "/home/me/restyle"); // user adds
    assert_eq!(cfg["extensions"].as_object().unwrap().len(), 3); // no loss
}

#[test]
fn extensions_group_absent_is_just_absent() {
    // No group = no `extensions` key at all (the empty-group skip rule);
    // the host then supervises nothing.
    let dir = tempfile::tempdir().unwrap();
    let default = dir.path().join("default.kdl");
    std::fs::write(&default, "ui {\n    thumbnails false\n}\n").unwrap();
    let cfg = mudrad::config::load(&default, &dir.path().join("missing.kdl")).unwrap();
    assert!(cfg.get("extensions").is_none());
}

// ================= §4: the intercept plane =================

#[test]
fn verdict_vocabulary_is_deny_redirect_allow() {
    use mudrad::host::{Verdict, parse_verdict};

    assert_eq!(parse_verdict(&json!({ "verdict": "allow" })), Verdict::Allow);
    assert_eq!(parse_verdict(&json!({ "verdict": "deny" })), Verdict::Deny);
    assert_eq!(
        parse_verdict(&json!({ "verdict": "redirect", "url": "https://safe.test" })),
        Verdict::Redirect("https://safe.test".into())
    );
    // every unrecognised shape is `allow`: fail-open is structural, not a
    // branch someone can forget to take
    assert_eq!(parse_verdict(&json!({})), Verdict::Allow);
    assert_eq!(parse_verdict(&json!({ "verdict": "block" })), Verdict::Allow); // a typo is not a veto
    assert_eq!(parse_verdict(&json!({ "verdict": "redirect" })), Verdict::Allow); // no url, no redirect
    assert_eq!(parse_verdict(&json!({ "verdict": "redirect", "url": "" })), Verdict::Allow);
    assert_eq!(parse_verdict(&json!("deny")), Verdict::Allow); // not an object
}

/// Stub sessions for the gate: no process, no pipe — the gate's own logic.
enum Policy {
    Answer(Value),
    /// Hold the waiter and never answer: a stalled session.
    Silent,
}

fn stub_session(reg: &mudrad::host::Registry, name: &str, declares: bool, policy: Policy) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<mudrad::host::HotCall>(4);
    let events = if declares {
        vec![mudrad::host::BEFORE_NAVIGATE.to_string()]
    } else {
        Vec::new()
    };
    reg.register(mudrad::host::Handle::new(name, events, tx));
    tokio::spawn(async move {
        while let Some(hot) = rx.recv().await {
            match &policy {
                Policy::Answer(v) => {
                    let _ = hot.reply.send(v.clone());
                }
                Policy::Silent => std::mem::forget(hot.reply),
            }
        }
    });
}

#[tokio::test]
async fn first_non_allow_verdict_wins_and_everything_else_fails_open() {
    use mudrad::host::{Registry, Verdict, before_navigate};
    use std::time::{Duration, Instant};

    // no session at all: allow
    let reg = Registry::default();
    assert_eq!(before_navigate(&reg, "https://a.test", "inbox").await.0, Verdict::Allow);

    // declared, but stalled: the budget expires and the verdict is allow
    let reg = Registry::default();
    stub_session(&reg, "silent", true, Policy::Silent);
    let t = Instant::now();
    assert_eq!(before_navigate(&reg, "https://a.test", "inbox").await.0, Verdict::Allow);
    assert!(t.elapsed() >= Duration::from_millis(200), "waited the budget: {:?}", t.elapsed());

    // a session that did not DECLARE the event is never asked
    let reg = Registry::default();
    stub_session(&reg, "other", false, Policy::Answer(json!({ "verdict": "deny" })));
    assert_eq!(before_navigate(&reg, "https://a.test", "inbox").await.0, Verdict::Allow);

    // registration order decides among declarers: allow then deny -> deny,
    // and the answer names who denied
    let reg = Registry::default();
    stub_session(&reg, "a-allow", true, Policy::Answer(json!({ "verdict": "allow" })));
    stub_session(&reg, "b-deny", true, Policy::Answer(json!({ "verdict": "deny" })));
    let (v, by) = before_navigate(&reg, "https://a.test", "inbox").await;
    assert_eq!(v, Verdict::Deny);
    assert_eq!(by.as_deref(), Some("b-deny"));

    // the first non-allow SHORT-CIRCUITS: an earlier redirect beats a later deny
    let reg = Registry::default();
    stub_session(
        &reg,
        "a-redirect",
        true,
        Policy::Answer(json!({ "verdict": "redirect", "url": "https://safe.test" })),
    );
    stub_session(&reg, "b-deny", true, Policy::Answer(json!({ "verdict": "deny" })));
    let (v, by) = before_navigate(&reg, "https://a.test", "inbox").await;
    assert_eq!(v, Verdict::Redirect("https://safe.test".into()));
    assert_eq!(by.as_deref(), Some("a-redirect"));

    // a session that is gone is not asked (the handle is unregistered)
    let reg = Registry::default();
    stub_session(&reg, "gone", true, Policy::Answer(json!({ "verdict": "deny" })));
    reg.unregister("gone");
    assert_eq!(before_navigate(&reg, "https://a.test", "inbox").await.0, Verdict::Allow);
}

#[tokio::test]
async fn hot_call_ids_stay_out_of_the_event_log_id_space() {
    use mudrad::host::{BEFORE_NAVIGATE, Registry, HotCall, Handle};
    use std::sync::Arc;

    // The gate assigns ids from its own space: a guest answering a delivered
    // EVENT can never be mis-correlated with a pending verdict.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<HotCall>(4);
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Some(hot) = rx.recv().await {
            seen2.lock().unwrap().push(hot.frame.id);
            let _ = hot.reply.send(json!({ "verdict": "allow" }));
        }
    });
    let reg = Registry::default();
    reg.register(Handle::new("stub", vec![BEFORE_NAVIGATE.to_string()], tx));

    for _ in 0..3 {
        let _ = mudrad::host::before_navigate(&reg, "https://a.test", "inbox").await;
    }
    let ids = seen.lock().unwrap().clone();
    assert_eq!(ids.len(), 3);
    assert!(ids.iter().all(|id| *id >= (1u64 << 62)), "ids: {ids:?}");
    assert!(ids.windows(2).all(|w| w[0] != w[1]), "ids must be distinct: {ids:?}");
}
