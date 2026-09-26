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
}

struct Fake {
    calls: Vec<Call>,
    /// (port, targetId) pairs the fake browser currently answers on /json.
    live_targets: HashSet<(u16, String)>,
    /// pid the fake launch_new returns (caller chooses before the verb).
    next_pid: u32,
}

impl Fake {
    fn new() -> Self {
        Fake { calls: Vec::new(), live_targets: HashSet::new(), next_pid: std::process::id() }
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
}

#[test]
fn unknown_endpoint_errors() {
    let mut h = Harness::new();
    assert!(h.verb("/bogus", json!({})).is_err());
}
