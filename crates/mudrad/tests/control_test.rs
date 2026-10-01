//! Control verb tests — behavior ported from the Python mudrad handler
//! and ops.py, with the machine side behind the `Runtime` seam (a `Fake`
//! that records calls and answers target probes). State assertions
//! read the real fjall store; effect assertions read the recorded call
//! list. Run: `cargo test -p mudrad --test control_test`.

use mudrad::control::Controller;
use mudrad::runtime::Runtime;
use mudra_store::{
    Instance, InstanceKey, MudraStore, PageKey, Tag, TagKey, state,
};
use serde_json::{json, Value};
use std::collections::HashSet;

/// Recorded effect, in call order.
#[derive(Debug, PartialEq, Clone)]
enum Call {
    LaunchNew { ctx: String, url: String, proxy: Option<String> },
    LaunchJoin { ctx: String, url: String },
    CloseTarget { port: u16, target: String },
    Activate { port: u16, target: String },
    Kill(u32),
    FocusWindow { pid: u32 },
    ApplyWidth { pid: u32, prop: f64 },
    Notify(u64),
    Shot { port: u16, target: String },
    Page { port: u16, target: String, method: String },
    Niri(Vec<String>),
    Panel,
}

struct Fake {
    calls: Vec<Call>,
    /// (port, targetId) pairs the fake browser currently answers on /json.
    live_targets: HashSet<(u16, String)>,
    /// pid the fake launch_new returns (caller chooses before the verb).
    next_pid: u32,
    /// what the fake screenshot answers (None = "no live page" per the
    /// seam contract; Some(data url) = a capture).
    shot_answer: Option<String>,
    /// page_command answers: (method -> result JSON); missing = Ok(None)
    /// (target not live), matching the seam's "no page" contract.
    page_answers: std::collections::HashMap<String, serde_json::Value>,
    /// the niri snapshot the fake serves (default: empty windows).
    snapshot: mudrad::runtime::NiriSnapshot,
    /// /json rows list_targets answers.
    targets: Vec<serde_json::Value>,
    dev: bool,
    panel_pids: u32,
}

impl Fake {
    fn new() -> Self {
        Fake {
            calls: Vec::new(),
            live_targets: HashSet::new(),
            next_pid: std::process::id(),
            shot_answer: None,
            page_answers: std::collections::HashMap::new(),
            snapshot: mudrad::runtime::NiriSnapshot::default(),
            targets: Vec::new(),
            dev: false,
            panel_pids: 0,
        }
    }
    fn take_calls(&mut self) -> Vec<Call> {
        std::mem::take(&mut self.calls)
    }
}

impl Runtime for Fake {
    fn launch_new(&mut self, ctx: &str, url: &str, _port: u16, proxy: Option<&str>, _ext: Option<&str>) -> Result<u32, String> {
        self.calls.push(Call::LaunchNew {
            ctx: ctx.into(),
            url: url.into(),
            proxy: proxy.map(str::to_string),
        });
        Ok(self.next_pid)
    }
    fn launch_join(&mut self, ctx: &str, url: &str, _proxy: Option<&str>, _ext: Option<&str>) -> Result<(), String> {
        self.calls.push(Call::LaunchJoin { ctx: ctx.into(), url: url.into() });
        Ok(())
    }
    fn close_target(&mut self, port: u16, target_id: &str) -> Result<(), String> {
        self.calls.push(Call::CloseTarget { port, target: target_id.into() });
        Ok(())
    }
    fn activate_target(&mut self, port: u16, target_id: &str) -> Result<(), String> {
        self.calls.push(Call::Activate { port, target: target_id.into() });
        Ok(())
    }
    fn target_exists(&mut self, port: u16, target_id: &str) -> bool {
        self.live_targets.contains(&(port, target_id.to_string()))
    }
    fn kill(&mut self, pid: u32) -> Result<(), String> {
        self.calls.push(Call::Kill(pid));
        Ok(())
    }
    fn focus_window(&mut self, pid: u32, _title: &str, _url: &str) {
        self.calls.push(Call::FocusWindow { pid });
    }
    fn apply_site_width(&mut self, pid: u32, proportion: f64) {
        self.calls.push(Call::ApplyWidth { pid, prop: proportion });
    }
    fn notify(&mut self, epoch: u64) {
        self.calls.push(Call::Notify(epoch));
    }
    fn screenshot(&mut self, port: u16, target_id: &str) -> Result<Option<String>, String> {
        self.calls.push(Call::Shot { port, target: target_id.into() });
        Ok(self.shot_answer.clone())
    }
    fn page_command(&mut self, port: u16, target_id: &str, method: &str,
                    _params: serde_json::Value) -> Result<Option<serde_json::Value>, String> {
        self.calls.push(Call::Page { port, target: target_id.into(), method: method.into() });
        Ok(self.page_answers.get(method).cloned())
    }
    fn list_targets(&mut self, port: u16) -> Result<Vec<serde_json::Value>, String> {
        let _ = port;
        Ok(self.targets.clone())
    }
    fn niri_snapshot(&mut self) -> Result<mudrad::runtime::NiriSnapshot, String> {
        Ok(self.snapshot.clone())
    }
    fn niri_action(&mut self, args: &[&str]) -> Result<(), String> {
        self.calls.push(Call::Niri(args.iter().map(ToString::to_string).collect()));
        Ok(())
    }
    fn set_dev_mode(&mut self, on: bool) {
        self.dev = on;
    }
    fn launch_panel(&mut self) -> Result<u32, String> {
        self.calls.push(Call::Panel);
        self.panel_pids += 1;
        Ok(900_000 + self.panel_pids)
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    store: MudraStore,
    rt: Fake,
}

impl Harness {
    fn new() -> Harness {
        let dir = tempfile::TempDir::new().unwrap();
        let store = MudraStore::open(dir.path()).unwrap();
        Harness { _dir: dir, store, rt: Fake::new() }
    }

    /// Run one verb. The split borrow (store / rt) is why Controller is
    /// built per call.
    fn verb(&mut self, path: &str, body: Value) -> Result<Value, String> {
        let Harness { store, rt, .. } = self;
        let mut c = Controller { store, rt };
        c.handle(path, &body)
    }

    fn calls(&mut self) -> Vec<Call> {
        self.rt.take_calls()
    }
}

/// Seed: situation root + `work` leaf; a running instance for `work`
/// with one open page (target T1 at https://a.test).
fn seeded(h: &mut Harness) -> InstanceKey {
    let sit = TagKey { id: h.store.next_id(state::TAG_ID) as u32 };
    h.store.tags.put(&sit, &Tag { name: "situation".into(), parent_id: -1, ..Default::default() });
    let work = TagKey { id: h.store.next_id(state::TAG_ID) as u32 };
    h.store.tags.put(&work, &Tag { name: "work".into(), parent_id: sit.id as i32, isolated: 1, ..Default::default() });

    let k = InstanceKey { id: h.store.next_id(state::INSTANCE_ID) as u32 };
    h.store.instances.put(
        &k,
        &Instance { profile: "work".into(), port: 9301, pid: std::process::id(), running: 1, proxy: "http://p:1".into(), extensions: "/e1".into() },
    );
    h.store.sync_targets(
        k.id,
        &[mudra_store::TargetInfo {
            target_id: "T1".into(),
            url: "https://a.test/page".into(),
            title: "Alpha".into(),
            opener_id: String::new(),
        }],
        1000,
    );
    k
}

// ================= open =================

#[test]
fn open_without_url_is_an_error() {
    let mut h = Harness::new();
    assert_eq!(h.verb("/open", json!({})), Err("need url".into()));
    assert!(h.calls().is_empty());
}

#[test]
fn open_joins_a_live_instance_without_touching_the_store() {
    // Contract: alive -> --app join (no debug port, proxy/ext from the row),
    // no new instance row, pages untouched until the watcher reports.
    let mut h = Harness::new();
    let k = seeded(&mut h);
    h.store.put_state_text(state::CURRENT_CONTEXT, "work");
    let epoch_before = h.store.epoch();

    let r = h.verb("/open", json!({"url": "https://b.test"})).unwrap();
    assert_eq!(r["mode"], "joined");
    assert_eq!(r["port"], 9301);
    assert_eq!(h.calls(), vec![Call::LaunchJoin { ctx: "work".into(), url: "https://b.test".into() }]);
    assert_eq!(h.store.epoch(), epoch_before, "the join itself writes no state");
    // the page is NOT recorded here — CDP/watcher owns the row lifecycle
    assert_eq!(h.store.pages_of_instance(k.id, false).len(), 1);
}

#[test]
fn open_creates_an_instance_when_none_exists() {
    // Contract: no row -> new debug-port instance; row recorded running,
    // port in 9200.. range; url bare -> https normalized.
    let mut h = Harness::new();
    let sit = TagKey { id: 1 };
    h.store.tags.put(&sit, &Tag { name: "situation".into(), parent_id: -1, ..Default::default() });
    let r = h.verb("/open", json!({"url": "example.com", "ctx": "inbox"})).unwrap();
    assert_eq!(r["mode"], "new");
    assert_eq!(r["ctx"], "inbox");
    let port = r["port"].as_u64().unwrap() as u16;
    assert!((9200..9400).contains(&port));
    let calls = h.calls();
    assert!(matches!(&calls[0], Call::LaunchNew { ctx, url, proxy: None } if ctx == "inbox" && url == "https://example.com"), "{calls:?}");
    let (_, inst) = h.store.instance_for_context("inbox").expect("row written");
    assert_eq!(inst.running, 1);
    assert_eq!(inst.port as u16, port);
}

#[test]
fn open_replaces_a_dead_instance_row_and_carries_config() {
    // Contract (the zombie-join blood lesson): DB running=1 alone never
    // means alive — pid liveness decides. A dead row is replaced by a new
    // debug-port instance reusing the row (same id, proxy/extensions).
    let mut h = Harness::new();
    let k = seeded(&mut h);
    // kill the liveness: pid -> nonexistent, running flag left at 1
    let mut inst = h.store.instances.get(&k).unwrap();
    inst.pid = u32::MAX;
    h.store.instances.put(&k, &inst);
    h.store.put_state_text(state::CURRENT_CONTEXT, "work");

    let r = h.verb("/open", json!({"url": "https://c.test"})).unwrap();
    assert_eq!(r["mode"], "new", "a dead pid must never be joined");
    let calls = h.calls();
    assert!(matches!(&calls[0], Call::LaunchNew { proxy: Some(p), .. } if p == "http://p:1"), "proxy carried: {calls:?}");
    let (k2, inst2) = h.store.instance_for_context("work").unwrap();
    assert_eq!(k2, k, "old row reused (id stable)");
    assert_ne!(inst2.pid, u32::MAX);
    assert_eq!(inst2.extensions, "/e1", "extensions carried");
}

#[test]
fn open_applies_remembered_site_width() {
    // Contract (spawn side effect): a known site's proportion goes to the
    // window manager exactly on the NEW-instance path.
    let mut h = Harness::new();
    let swk = mudra_store::SiteWidthKey { id: 1 };
    h.store.site_widths.put(
        &swk,
        &mudra_store::SiteWidth { site: "news.ycombinator.com".into(), proportion: okm_core::Quant::<4>::new(0.618) },
    );
    let r = h.verb("/open", json!({"url": "https://news.ycombinator.com/item?id=1", "ctx": "inbox"})).unwrap();
    let pid = r["pid"].as_u64().unwrap() as u32;
    let calls = h.calls();
    assert!(calls.contains(&Call::ApplyWidth { pid, prop: 0.618 }), "{calls:?}");
}

// ================= add =================

#[test]
fn add_never_spawns_silently() {
    // Contract: /add requires a live instance; otherwise a named error
    // pointing at /open (Python: "use open first").
    let mut h = Harness::new();
    let e = h.verb("/add", json!({"url": "https://x.test", "ctx": "work"})).unwrap_err();
    assert!(e.contains("not running; use open first"), "{e}");
    assert!(h.calls().is_empty());
}

#[test]
fn add_joins_the_live_instance() {
    let mut h = Harness::new();
    seeded(&mut h);
    let r = h.verb("/add", json!({"url": "https://x.test", "ctx": "work"})).unwrap();
    assert_eq!(r["mode"], "joined");
    assert_eq!(h.calls(), vec![Call::LaunchJoin { ctx: "work".into(), url: "https://x.test".into() }]);
}

// ================= close_page / close_ctx =================

#[test]
fn close_page_targets_the_matching_open_page() {
    // Contract: query matches url OR title (substring), close request
    // goes to the debug port; the ROW teardown happens on the watcher's
    // destroyed event, not here (single close path).
    let mut h = Harness::new();
    let k = seeded(&mut h);
    let r = h.verb("/close_page", json!({"query": "a.test", "ctx": "work"})).unwrap();
    assert_eq!(r["closed"], "https://a.test/page");
    assert_eq!(h.calls(), vec![Call::CloseTarget { port: 9301, target: "T1".into() }]);
    assert_eq!(h.store.pages_of_instance(k.id, false)[0].1.closed_at, 0, "row untouched by the verb");
    assert!(h.verb("/close_page", json!({"query": "nomatch", "ctx": "work"})).is_err());
}

#[test]
fn close_ctx_kills_the_process() {
    // Contract: kill by pid; teardown follows via the watcher.
    let mut h = Harness::new();
    seeded(&mut h);
    let pid = std::process::id();
    h.verb("/close_ctx", json!({"ctx": "work"})).unwrap();
    assert_eq!(h.calls(), vec![Call::Kill(pid)]);
}

// ================= ctx =================

#[test]
fn ctx_switch_validates_and_notifies() {
    // Contract: situation leaf only; success writes state + one notify.
    let mut h = Harness::new();
    seeded(&mut h);
    let r = h.verb("/ctx", json!({"ctx": "work"})).unwrap();
    assert_eq!(r["ctx"], "work");
    assert_eq!(h.store.state_text(state::CURRENT_CONTEXT), "work");
    let calls = h.calls();
    assert!(matches!(&calls[..], [Call::Notify(_)]));
    assert_eq!(h.verb("/ctx", json!({"ctx": "nope"})), Err("not a situation leaf: \"nope\"".into()));
}

// ================= tag / tags =================

#[test]
fn tag_toggles_through_tab_resolution() {
    // Contract: tabId resolves ctx via the live-target probe; toggle
    // flips added/removed and notifies the new epoch.
    let mut h = Harness::new();
    let k = seeded(&mut h);
    let t = TagKey { id: h.store.next_id(state::TAG_ID) as u32 };
    h.store.tags.put(&t, &Tag { name: "unread".into(), parent_id: -1, ..Default::default() });
    h.rt.live_targets.insert((9301, "123".into())); // extension tabId

    let r = h.verb("/tag", json!({"tabId": "123", "url": "https://a.test/page", "tag": "unread"})).unwrap();
    assert_eq!(r["action"], "added");
    let pk = h.store.page_by_target("T1").unwrap().0;
    assert_eq!(h.store.tags_of_page(&pk), vec![t.clone()]);
    assert!(h.calls().iter().any(|c| matches!(c, Call::Notify(_))));

    let r = h.verb("/tag", json!({"tabId": "123", "url": "https://a.test/page", "tag": "unread"})).unwrap();
    assert_eq!(r["action"], "removed");
    assert!(h.store.tags_of_page(&pk).is_empty());
    let _ = k;

    assert!(h.verb("/tag", json!({"tabId": "123", "url": "https://a.test/page", "tag": "ghost"})).is_err(), "unknown tag");
}

#[test]
fn tags_drills_root_and_named_parent() {
    let mut h = Harness::new();
    seeded(&mut h); // situation -> work
    let inbox = TagKey { id: h.store.next_id(state::TAG_ID) as u32 };
    h.store.tags.put(&inbox, &Tag { name: "inbox".into(), parent_id: 1, ..Default::default() });
    let root = h.verb("/tags", json!({})).unwrap();
    assert_eq!(root["tags"], json!(["situation"]));
    let under = h.verb("/tags", json!({"parent": "situation"})).unwrap();
    assert_eq!(under["tags"], json!(["work", "inbox"]));
}

// ================= pages / focus / ctx_status =================

#[test]
fn pages_lists_open_rows_across_contexts() {
    let mut h = Harness::new();
    let k = seeded(&mut h);
    let _ = PageKey { id: 0 };
    let r = h.verb("/pages", json!({})).unwrap();
    let list = r["pages"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["ctx"], "work");
    assert_eq!(list[0]["url"], "https://a.test/page");
    let filtered = h.verb("/pages", json!({"ctx": "inbox"})).unwrap();
    assert_eq!(filtered["pages"].as_array().unwrap().len(), 0);
    let _ = k;
}

#[test]
fn focus_page_acts_on_both_planes() {
    // Contract: CDP activate by (port, targetId) + niri bring-forward by
    // pid (best effort: Fake focus never fails, Real logs and moves on).
    let mut h = Harness::new();
    let k = seeded(&mut h);
    let pk = h.store.page_by_target("T1").unwrap().0;
    h.verb("/focus_page", json!({"page_id": pk.id})).unwrap();
    let pid = std::process::id();
    assert_eq!(h.calls(), vec![Call::Activate { port: 9301, target: "T1".into() }, Call::FocusWindow { pid }]);
    // closed page -> not found
    h.store.close_target(k.id, "T1", 2000);
    assert!(h.verb("/focus_page", json!({"page_id": pk.id})).is_err());
}

#[test]
fn ctx_status_answers_console_role_without_lookup() {
    let mut h = Harness::new();
    let r = h.verb("/ctx_status", json!({"url": "http://127.0.0.1:9299/"})).unwrap();
    assert_eq!(r["role"], "console");
    assert!(r["ctx"].is_null());
    assert_eq!(r["tags"].as_array().unwrap().len(), 0);
    assert_eq!(r["capsules"], "");
    assert!(h.calls().is_empty(), "console short-circuits the probes");
}

#[test]
fn ctx_status_resolves_page_role_with_tag_paths() {
    // Contract (path capsules): tags arrive as full ::-joined paths.
    let mut h = Harness::new();
    let k = seeded(&mut h);
    let _ = k;
    h.rt.live_targets.insert((9301, "77".into()));
    let state_root = TagKey { id: h.store.next_id(state::TAG_ID) as u32 };
    h.store.tags.put(&state_root, &Tag { name: "state".into(), parent_id: -1, ..Default::default() });
    let unread = TagKey { id: h.store.next_id(state::TAG_ID) as u32 };
    h.store.tags.put(&unread, &Tag { name: "unread".into(), parent_id: state_root.id as i32, ..Default::default() });
    let pk = h.store.page_by_target("T1").unwrap().0;
    h.store.link_page_tag(&pk, &unread);

    let r = h.verb("/ctx_status", json!({"tabId": "77", "url": "https://a.test/page"})).unwrap();
    assert_eq!(r["role"], "page");
    assert_eq!(r["ctx"], "work");
    assert_eq!(r["tags"], json!(["state::unread"]));
    // SSR capsule row from the tag-forest crate (read-only shape: the
    // last segment carries .leaf; the bar drops this string in verbatim)
    assert_eq!(
        r["capsules"],
        "<span class=\"capsule\"><span class=\"seg\">state</span><span class=\"seg leaf\">unread</span></span>"
    );
}

#[test]
fn unknown_endpoint_errors() {
    let mut h = Harness::new();
    assert!(h.verb("/bogus", json!({})).is_err());
}

// ================= R2 slice 2: panel control-plane verbs =================

#[test]
fn forest_shapes_roots_rank_axis_and_paths() {
    // Contract (panel's load()): roots carry root/rank_axis, children
    // carry `::`-joined paths; rank 0 marshals as null (the panel splits
    // plain vs rank nodes on `rank === null`); contexts = situation
    // leaves in id order; current resolves through the same default.
    let mut h = Harness::new();
    let _k = seeded(&mut h); // situation -> work
    let imp = TagKey { id: h.store.next_id(state::TAG_ID) as u32 };
    h.store.tags.put(&imp, &Tag { name: "importance".into(), parent_id: -1, ..Default::default() });
    let hi = TagKey { id: h.store.next_id(state::TAG_ID) as u32 };
    h.store.tags.put(&hi, &Tag { name: "high".into(), parent_id: imp.id as i32, rank: 3, ..Default::default() });
    h.store.put_state_text(state::CURRENT_CONTEXT, "work");

    let r = h.verb("/forest", json!({})).unwrap();
    assert_eq!(r["current"], "work");
    assert_eq!(r["contexts"], json!(["work"]));
    let forest = r["forest"].as_array().unwrap();
    assert_eq!(forest.len(), 2, "two roots, id order");
    assert_eq!(forest[0]["name"], "situation");
    assert_eq!(forest[0]["rank_axis"], Value::Null);
    assert_eq!(forest[0]["children"][0]["name"], "work");
    assert_eq!(forest[0]["children"][0]["path"], "situation::work");
    assert_eq!(forest[1]["rank_axis"], "★");
    assert_eq!(forest[1]["children"][0]["rank"], 3);
    assert_eq!(forest[1]["children"][0]["path"], "importance::high");
    assert!(h.calls().is_empty(), "reads do not push");
}

#[test]
fn forest_drops_deleted_and_their_subtrees_are_orphan_free() {
    let mut h = Harness::new();
    seeded(&mut h);
    let gone = TagKey { id: h.store.next_id(state::TAG_ID) as u32 };
    h.store.tags.put(&gone, &Tag { name: "gone".into(), parent_id: 1, deleted: 1, ..Default::default() });
    let r = h.verb("/forest", json!({})).unwrap();
    let kids = r["forest"][0]["children"].as_array().unwrap();
    assert!(kids.iter().all(|c| c["name"] != "gone"));
}

#[test]
fn ctx_pages_includes_closed_rows_with_tags() {
    // Contract (panel strikes closed rows): open + closed-undeleted all
    // come back, position-ordered, tag_ids and closed flag per row.
    let mut h = Harness::new();
    let k = seeded(&mut h);
    let unread = TagKey { id: h.store.next_id(state::TAG_ID) as u32 };
    h.store.tags.put(&unread, &Tag { name: "unread".into(), parent_id: -1, ..Default::default() });
    let pk = h.store.page_by_target("T1").unwrap().0;
    h.store.link_page_tag(&pk, &unread);
    // a second page, then close it
    h.store.sync_targets(
        k.id,
        &[mudra_store::TargetInfo {
            target_id: "T2".into(),
            url: "https://b.test".into(),
            title: "".into(),
            opener_id: "T1".into(),
        }],
        2000,
    );
    let pk2 = h.store.page_by_target("T2").unwrap().0;
    h.store.close_target(k.id, "T2", 3000);

    let r = h.verb("/ctx_pages", json!({"ctx": "work"})).unwrap();
    let pages = r["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 2);
    let p1 = pages.iter().find(|p| p["id"] == pk.id).unwrap();
    assert_eq!(p1["closed"], false);
    assert_eq!(p1["tag_ids"], json!([unread.id]));
    assert_eq!(p1["title"], "Alpha");
    let p2 = pages.iter().find(|p| p["id"] == pk2.id).unwrap();
    assert_eq!(p2["closed"], true);
    assert_eq!(p2["title"], "https://b.test", "empty title falls back to url");
    assert_eq!(p2["parent_id"], pk.id, "openerId backfill rode along");
}

#[test]
fn set_tags_replaces_whole_set_and_filters_deleted() {
    let mut h = Harness::new();
    seeded(&mut h);
    let a = TagKey { id: h.store.next_id(state::TAG_ID) as u32 };
    h.store.tags.put(&a, &Tag { name: "a".into(), parent_id: -1, ..Default::default() });
    let dead = TagKey { id: h.store.next_id(state::TAG_ID) as u32 };
    h.store.tags.put(&dead, &Tag { name: "dead".into(), parent_id: -1, deleted: 1, ..Default::default() });
    let pk = h.store.page_by_target("T1").unwrap().0;
    h.store.link_page_tag(&pk, &dead); // pre-existing link to a deleted tag

    let epoch_before = h.store.epoch();
    h.verb("/set_tags", json!({"page_id": pk.id, "tag_ids": [a.id, dead.id, 999]})).unwrap();
    assert_eq!(h.store.tags_of_page(&pk), vec![a]);
    assert!(h.store.epoch() > epoch_before, "a replace always invalidates");
    let calls = h.calls();
    assert!(calls.iter().any(|c| matches!(c, Call::Notify(_))));
    // unknown page -> error, no write
    assert!(h.verb("/set_tags", json!({"page_id": 4242, "tag_ids": []})).is_err());
}

#[test]
fn create_tag_is_idempotent_under_parent_name() {
    let mut h = Harness::new();
    seeded(&mut h);
    let epoch_before = h.store.epoch();
    let r1 = h.verb("/create_tag", json!({"parent_id": 1, "name": "deep"})).unwrap();
    let r2 = h.verb("/create_tag", json!({"parent_id": 1, "name": "deep"})).unwrap();
    assert_eq!(r1, r2, "same id both times");
    assert_eq!(h.store.tag_by_name("deep").len(), 1);
    // created once bumps; the idempotent second call must not notify again
    assert!(h.store.epoch() > epoch_before);
    assert_eq!(h.calls().iter().filter(|c| matches!(c, Call::Notify(_))).count(), 1);
    assert!(h.verb("/create_tag", json!({"parent_id": 1, "name": "  "})).is_err());
}

#[test]
fn close_by_id_marks_row_and_closes_target() {
    // Contract (ops.close_page port): row closed_at set, THEN target
    // closed on the machine; double close is a silent no-op (one write).
    let mut h = Harness::new();
    let _k = seeded(&mut h);
    let pk = h.store.page_by_target("T1").unwrap().0;
    h.verb("/close", json!({"page_id": pk.id})).unwrap();
    let page = h.store.pages.get(&pk).unwrap();
    assert_ne!(page.closed_at, 0);
    let got = h.calls();
    assert!(
        matches!(&got[..], [Call::CloseTarget { port: 9301, target }, Call::Notify(_)] if target == "T1"),
        "{got:?}"
    );
    // second close: no re-mark, no effect, epoch already moved once
    let epoch = h.store.epoch();
    h.verb("/close", json!({"page_id": pk.id})).unwrap();
    assert_eq!(h.store.epoch(), epoch, "no-op close stays silent");
    assert!(h.calls().is_empty());
    assert!(h.verb("/close", json!({"page_id": 4242})).is_err());
}

#[test]
fn reopen_routes_through_open_and_rejects_deleted() {
    let mut h = Harness::new();
    seeded(&mut h);
    // close first, then reopen: the URL re-enters via the normal open
    // path (join on the live instance; revive happens later via watcher)
    let pk = h.store.page_by_target("T1").unwrap().0;
    h.store.close_target(1, "T1", 2000);
    h.verb("/reopen", json!({"page_id": pk.id})).unwrap();
    assert_eq!(h.calls(), vec![Call::LaunchJoin { ctx: "work".into(), url: "https://a.test/page".into() }]);
    // deleted row -> refused
    h.store.delete_page(&pk, 3000).unwrap();
    assert_eq!(h.verb("/reopen", json!({"page_id": pk.id})), Err("page is deleted".into()));
}

#[test]
fn delete_requires_closed_first() {
    let mut h = Harness::new();
    seeded(&mut h);
    let pk = h.store.page_by_target("T1").unwrap().0;
    // open page -> the lifecycle invariant refuses (ops.py parity)
    assert!(h.verb("/delete", json!({"page_id": pk.id})).is_err());
    h.store.close_target(1, "T1", 2000);
    h.verb("/delete", json!({"page_id": pk.id})).unwrap();
    assert_ne!(h.store.pages.get(&pk).unwrap().deleted_at, 0);
    let calls = h.calls();
    assert!(calls.iter().any(|c| matches!(c, Call::Notify(_))));
}

#[test]
fn shot_answers_live_pages_only() {
    // Contract (ui.py _shot port): closed/absent page answers data null
    // with NO runtime call; a live page forwards (port, targetId).
    let mut h = Harness::new();
    seeded(&mut h);
    let pk = h.store.page_by_target("T1").unwrap().0;

    h.rt.shot_answer = Some("data:image/png;base64,QUJD".into());
    let r = h.verb("/shot", json!({"page_id": pk.id})).unwrap();
    assert_eq!(r["data"], "data:image/png;base64,QUJD");
    assert_eq!(h.calls(), vec![Call::Shot { port: 9301, target: "T1".into() }]);

    h.store.close_target(1, "T1", 2000);
    let r = h.verb("/shot", json!({"page_id": pk.id})).unwrap();
    assert_eq!(r["data"], Value::Null);
    assert!(h.calls().is_empty(), "no round trip for a closed page");
}

// ================= A2: CLI fact-source verbs =================

#[test]
fn contexts_lists_leaves_with_open_counts_and_current_mark() {
    let mut h = Harness::new();
    seeded(&mut h); // situation -> work leaf with one open page
    h.store.put_state_text(state::CURRENT_CONTEXT, "work");
    let r = h.verb("/contexts", json!({})).unwrap();
    let list = r["contexts"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["leaf"], "work");
    assert_eq!(list[0]["pages"], 1);
    assert_eq!(list[0]["current"], true);
}

#[test]
fn targets_passthroughs_cdp_rows_and_refuses_dead_ctx() {
    let mut h = Harness::new();
    seeded(&mut h);
    h.rt.targets = vec![json!({"id": "T1", "type": "page", "title": "Alpha", "url": "https://a.test/page"})];
    let r = h.verb("/targets", json!({"ctx": "work"})).unwrap();
    assert_eq!(r["targets"][0]["targetId"], "T1");
    let e = h.verb("/targets", json!({"ctx": "ghost"})).unwrap_err();
    assert!(e.contains("not running"), "{e}");
}

#[test]
fn focus_finds_by_query_and_acts_on_both_planes() {
    // ops.focus_ctx_query port contract: fuzzy CDP match -> stored row ->
    // the exact /focus_page path (activate + niri window).
    let mut h = Harness::new();
    seeded(&mut h);
    h.store.put_state_text(state::CURRENT_CONTEXT, "work");
    h.rt.targets = vec![json!({"id": "T1", "title": "Alpha", "url": "https://a.test/page"})];
    let r = h.verb("/focus", json!({"query": "alp"})).unwrap();
    assert_eq!(r["focused"], 1);
    let calls = h.calls();
    assert!(matches!(&calls[0], Call::Activate { port: 9301, target } if target == "T1"), "{calls:?}");
    assert!(matches!(&calls[1], Call::FocusWindow { .. }));

    h.rt.take_calls();
    let e = h.verb("/focus", json!({"query": "zzz"})).unwrap_err();
    assert!(e.contains("no page matching"), "{e}");
    assert!(h.calls().is_empty(), "a miss must not act");
}

#[test]
fn nav_goto_reloads_the_rightmost_open_page() {
    let mut h = Harness::new();
    let k = seeded(&mut h);
    h.store.put_state_text(state::CURRENT_CONTEXT, "work");
    h.rt.page_answers.insert("Page.navigate".into(), json!({"result": {}}));
    let r = h.verb("/nav", json!({"cmd": "goto", "url": "b.test/next"})).unwrap();
    assert_eq!(r["ok"], true);
    let calls = h.calls();
    assert!(matches!(&calls[0], Call::Page { port, target, method }
        if *port == 9301 && target == "T1" && method == "Page.navigate"), "{calls:?}");
    let _ = k;
    let e = h.verb("/nav", json!({"cmd": "goto"})).unwrap_err();
    assert_eq!(e, "goto needs url");
}

#[test]
fn nav_history_step_jumps_to_the_adjacent_entry() {
    // the ctl._history_step two-step port: read the history, then jump to
    // currentIndex + delta; out-of-range answers an error (no guessing).
    let mut h = Harness::new();
    seeded(&mut h);
    h.store.put_state_text(state::CURRENT_CONTEXT, "work");
    h.rt.page_answers.insert(
        "Page.getNavigationHistory".into(),
        // the seam hands back the envelope's `result` — the fake answers
        // at that level too (unwrapped), same as Real does
        json!({"currentIndex": 1, "entries": [{"id": 10}, {"id": 11}, {"id": 12}]}),
    );
    h.rt.page_answers.insert("Page.navigateToHistoryEntry".into(), json!({"result": {}}));
    let r = h.verb("/nav", json!({"cmd": "back"})).unwrap();
    assert_eq!(r["ok"], true);
    let calls = h.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(matches!(&calls[1], Call::Page { method, .. } if method == "Page.navigateToHistoryEntry"));

    // index 0 -> back lands on -1: out of range
    h.rt.page_answers.insert(
        "Page.getNavigationHistory".into(),
        json!({"currentIndex": 0, "entries": [{"id": 10}]}),
    );
    let e = h.verb("/nav", json!({"cmd": "back"})).unwrap_err();
    assert_eq!(e, "history step out of range");
}

#[test]
fn col_remember_snaps_and_show_lists_bands() {
    // cmd_col port: focused window -> instance row (pid) -> CDP page
    // (title) -> tile/output ratio -> band -> SiteWidth row.
    let mut h = Harness::new();
    let _k = seeded(&mut h);
    let pid = std::process::id();
    h.rt.snapshot = mudrad::runtime::NiriSnapshot {
        windows: vec![json!({"id": 7, "pid": pid, "title": "Alpha", "is_focused": true,
            "workspace_id": 1, "layout": {"tile_size": [500, 800]}})],
        workspaces: vec![json!({"idx": 1, "id": 1, "is_focused": true})],
        output_width: 1000.0,
    };
    h.rt.targets = vec![json!({"id": "T1", "title": "Alpha", "url": "https://a.test/page"})];
    let r = h.verb("/col", json!({"action": "remember"})).unwrap();
    assert_eq!(r["site"], "a.test");
    assert_eq!(r["proportion"], 0.5);
    assert_eq!(r["band"], "1/2");
    let (_, row) = h.store.site_width("a.test").expect("row written");
    assert_eq!(row.proportion.0, 0.5);

    let show = h.verb("/col", json!({"action": "show"})).unwrap();
    assert_eq!(show["widths"][0]["site"], "a.test");
    let filtered = h.verb("/col", json!({"action": "show", "site": "nomatch"})).unwrap();
    assert_eq!(filtered["widths"].as_array().unwrap().len(), 0);
}

#[test]
fn conf_checks_leaf_and_persists_omitted_fields_untouched() {
    let mut h = Harness::new();
    seeded(&mut h); // work leaf, instance proxy=http://p:1 extensions=/e1
    let e = h.verb("/conf", json!({"ctx": "nope", "proxy": "x"})).unwrap_err();
    assert!(e.contains("not a situation leaf"), "{e}");

    let r = h.verb("/conf", json!({"ctx": "work", "proxy": "http://q:2"})).unwrap();
    assert_eq!(r["proxy"], "http://q:2");
    assert_eq!(r["extensions"], "/e1", "omitted field stays");

    // an unseen-but-legal leaf pre-creates a stopped row (running=0)
    let sit = h
        .store
        .tag_by_name("situation")
        .into_iter()
        .find(|(_, t)| t.parent_id == -1)
        .unwrap()
        .0;
    let personal = TagKey { id: h.store.next_id(state::TAG_ID) as u32 };
    h.store.tags.put(&personal, &Tag { name: "personal".into(), parent_id: sit.id as i32, ..Default::default() });
    // 'none'/'default' -> "" is the thin CLI's spelling job (Python
    // cmd_conf resolved it before the write); the backend stays literal.
    let r = h.verb("/conf", json!({"ctx": "personal", "proxy": ""})).unwrap();
    assert_eq!(r["proxy"], Value::Null, "empty string clears the proxy");
    let (_, inst) = h.store.instance_for_context("personal").expect("stopped row");
    assert_eq!(inst.running, 0);
}

#[test]
fn dev_switch_writes_the_slot_and_the_live_seam_flag() {
    let mut h = Harness::new();
    let r = h.verb("/dev", json!({})).unwrap();
    assert_eq!(r["dev"], false);
    let r = h.verb("/dev", json!({"on": "1"})).unwrap();
    assert_eq!(r["dev"], true);
    assert_eq!(h.store.state_text(state::DEV_MODE), "1");
    assert!(h.rt.dev, "the live Real projection follows without a restart");
    let r = h.verb("/dev", json!({})).unwrap();
    assert_eq!(r["dev"], true);
}

#[test]
fn tag_seed_is_idempotent_and_bumps_only_on_change() {
    let mut h = Harness::new();
    seeded(&mut h); // situation+work already exist: seed fills the rest
    // 5 remaining roots + 3 sit leaves + 15 stars + 4 state kids = 27
    let r = h.verb("/tag_seed", json!({})).unwrap();
    assert_eq!(r["seeded"], 27);
    assert!(h.store.epoch() > 0);
    let e_before = h.store.epoch();
    h.rt.take_calls();
    let r = h.verb("/tag_seed", json!({})).unwrap();
    assert_eq!(r["seeded"], 0, "a re-run creates nothing");
    assert_eq!(h.store.epoch(), e_before, "no-op stays silent");
    assert!(h.calls().iter().all(|c| !matches!(c, Call::Notify(_))), "no notify for no writes");
}

#[test]
fn tag_set_assigns_removes_and_stays_silent_on_noops() {
    let mut h = Harness::new();
    let _k = seeded(&mut h);
    let pk = PageKey { id: 1 };
    let work = h.store.tag_by_name("work").into_iter().next().unwrap().0;
    let r = h.verb("/tag_set", json!({"page_id": 1, "tag_id": work.id, "on": true})).unwrap();
    assert_eq!(r["changed"], true);
    assert!(h.store.tags_of_page(&pk).contains(&work));

    h.rt.take_calls();
    let r = h.verb("/tag_set", json!({"page_id": 1, "tag_id": work.id, "on": true})).unwrap();
    assert_eq!(r["changed"], false);
    assert!(h.calls().iter().all(|c| !matches!(c, Call::Notify(_))), "idempotent add stays silent");

    let r = h.verb("/tag_set", json!({"page_id": 1, "tag_id": work.id, "on": false})).unwrap();
    assert_eq!(r["changed"], true);
    assert!(h.store.tags_of_page(&pk).is_empty());

    let e = h.verb("/tag_set", json!({"page_id": 999, "tag_id": work.id})).unwrap_err();
    assert!(e.contains("page 999 not found"), "{e}");
}

#[test]
fn sort_writes_the_closed_choice() {
    let mut h = Harness::new();
    let r = h.verb("/sort", json!({"kind": "mru"})).unwrap();
    assert_eq!(r["sort"], "mru");
    assert_eq!(h.store.state_text(state::SORT), "mru");
    let e = h.verb("/sort", json!({"kind": "bogus"})).unwrap_err();
    assert!(e.contains("unknown sort kind"), "{e}");
}

#[test]
fn panel_focus_without_a_window_spawns_one() {
    // ui.launch parity: focus-or-spawn is one verb; the status probe and
    // the spawn side share the fake snapshot (no real panel-profile
    // process exists in tests, so cmdline probes answer false).
    let mut h = Harness::new();
    let r = h.verb("/panel", json!({"action": "status"})).unwrap();
    assert_eq!(r["running"], false);

    h.rt.take_calls();
    let r = h.verb("/panel", json!({"action": "focus"})).unwrap();
    assert_eq!(r["spawned"], 900001, "no window up -> spawn fallback");
    assert_eq!(h.store.state_text(state::PANEL_PID), "900001");
    assert!(matches!(h.calls()[0], Call::Panel));
}

// ================= history =================

#[test]
fn open_records_history_visits_without_epoch_bump() {
    // The join branch writes a History row (visits accumulate) but never
    // bumps the epoch: History is not panel-visible; the epoch discipline
    // is for collections the panel re-scans.
    let mut h = Harness::new();
    seeded(&mut h);
    h.store.put_state_text(state::CURRENT_CONTEXT, "work");
    let epoch_before = h.store.epoch();

    h.verb("/open", json!({"url": "https://b.test"})).unwrap();
    h.verb("/open", json!({"url": "https://b.test"})).unwrap();
    let rows = h.store.history_all();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].visits, 2);
    assert_eq!(h.store.epoch(), epoch_before, "history writes do not bump the epoch");
}

#[test]
fn open_history_carry_from_reopen_of_watched_page() {
    // The join branch refreshes the title from live page rows (watcher
    // synced https://a.test/page with title "Alpha" — re-opening it
    // updates the history label).
    let mut h = Harness::new();
    seeded(&mut h);
    h.store.put_state_text(state::CURRENT_CONTEXT, "work");
    h.verb("/open", json!({"url": "https://a.test/page"})).unwrap();
    let rows = h.store.history_all();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Alpha");
}

#[test]
fn history_verb_ranks_by_fused_score() {
    let mut h = Harness::new();
    let epoch_before = h.store.epoch();
    // seed history directly on the store (visits semantics live in /open,
    // the verb under test is the read/rank surface).
    h.store.history_bump("https://news.ycombinator.com", "Hacker News", 1);
    h.store.history_bump("https://github.com/orbsh", "orbsh GitHub", 1);
    for _ in 0..3 {
        h.store.history_bump("https://github.com/orbsh", "", 2);
    }

    let r = h.verb("/history", json!({"query": "git"})).unwrap();
    let c = r["candidates"].as_array().unwrap();
    assert_eq!(c.len(), 1, "ycombinator is not a subsequence of 'git'");
    assert_eq!(c[0]["url"], "https://github.com/orbsh");
    assert_eq!(c[0]["visits"], 4);
    // read-only verb: no epoch write
    assert_eq!(h.store.epoch(), epoch_before);
    // limit clamps
    let r = h.verb("/history", json!({"query": "", "limit": 1})).unwrap();
    assert_eq!(r["candidates"].as_array().unwrap().len(), 1);
}
