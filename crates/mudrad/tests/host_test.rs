//! Extension-host tests — what mudra owns of the §2–§5 contract: the event
//! row → call-frame shaping, the §5 arm surface (verb routing, booth-shape
//! refusal), and the config face (§1). The frame vocabulary, hello decode,
//! budgeted I/O and the live pipe are `induction-*`'s — covered there
//! (protocol wire tests + host session tests, including a live child).
//! Run: `cargo test -p mudrad --test host_test`.

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
