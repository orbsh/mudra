//! Extension-host tests — the pure decode/shape half of the BGI session
//! contract (ADR-extension-protocol §2–§4), plus the config face (§1).
//! The pipe loop itself needs a live child (the next e2e step; the unit
//! surface is what the loop trusts).
//! Run: `cargo test -p mudrad --test host_test`.

use mudrad::host::{call_frame, initialize_frame, matches, parse_hello};
use std::collections::HashSet;

// ================= hello decode (the schema-carried subscription) =================

#[test]
fn hello_carries_name_and_the_on_set() {
    // Contract: a valid hello parses to its exact subscription set; the
    // script's interface_schema arrives as session data (§3).
    let h = parse_hello(
        r#"{"type":"hello","name":"k10r-sync","schema":{"on":["mudra:page_open","mudra:tag_set"]}}"#,
    )
    .expect("valid hello");
    assert_eq!(h.name, "k10r-sync");
    assert_eq!(h.on.len(), 2);
    assert!(h.on.contains("mudra:page_open"));
}

#[test]
fn bad_hello_shape_rejects_at_decode() {
    // Decode failure IS the version/contract-mismatch signal — never a
    // partial accept: wrong type, missing schema, missing name, junk.
    for line in [
        r#"{"type":"call","name":"x","schema":{"on":[]}}"#,
        r#"{"type":"hello","name":"x"}"#, // no schema
        r#"{"type":"hello","schema":{"on":[]}}"#, // no name
        r#"{"type":"hello","name":7,"schema":{"on":[]}}"#, // wrong type
        "not json",
    ] {
        assert!(parse_hello(line).is_none(), "must reject: {line}");
    }
    // an EMPTY on set is a valid hello (listens to nothing; the shape is
    // what decode checks)
    assert!(parse_hello(r#"{"type":"hello","name":"x","schema":{"on":[]}}"#).is_some());
}

// ================= subscription matching =================

#[test]
fn subscription_is_string_equality_no_routing() {
    // Free literal names; no wildcard, no prefix semantics (the
    // withdrawn mechanisms stay withdrawn).
    let on: HashSet<String> = ["mudra:page_open".to_string()].into();
    assert!(matches(&on, "mudra:page_open"));
    assert!(!matches(&on, "mudra:page_close"));
    assert!(!matches(&on, "mudra:page")); // no prefix match
    assert!(!matches(&on, "*")); // no wildcard
}

// ================= the BGI call frame =================

#[test]
fn call_frame_wraps_the_snapshot_as_a_value() {
    // Contract (BGI §3 + the args discipline): id/event ride the frame,
    // the stored JSON snapshot arrives PARSED — a consumer never
    // double-decodes. An unparseable args string still ships (as a
    // string), never withheld by a decode miss.
    let f = call_frame(7, "mudra:page_open", r#"{"url":"https://a.test","ctx":"work"}"#, 1000);
    let v: serde_json::Value = serde_json::from_str(&f).unwrap();
    assert_eq!(v["id"], 7);
    assert_eq!(v["kind"], "call");
    assert_eq!(v["event"], "mudra:page_open");
    assert_eq!(v["args"]["event_id"], 7);
    assert_eq!(v["args"]["at"], 1000);
    assert_eq!(v["args"]["snapshot"]["url"], "https://a.test");
    assert_eq!(v["args"]["snapshot"]["ctx"], "work");

    let broken = call_frame(8, "x", "not-json{", 2000);
    let v: serde_json::Value = serde_json::from_str(&broken).unwrap();
    assert_eq!(v["args"]["snapshot"], "not-json{");
}

#[test]
fn initialize_is_protocol_one() {
    let v: serde_json::Value = serde_json::from_str(&initialize_frame()).unwrap();
    assert_eq!(v["type"], "initialize");
    assert_eq!(v["protocol"], 1);
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
