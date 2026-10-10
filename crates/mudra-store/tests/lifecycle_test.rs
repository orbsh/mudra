//! Lifecycle tests — the behavioral spec ported from the Python tree
//! (`mudralib/db.py` page write paths + `mudrad.py` watchers). Each test
//! demonstrates one transition contract on the real fjall engine.
//!
//! The epoch law (SCHEMA): every state-changing transition returns the
//! bumped epoch; every no-op returns None — the invalidation push can
//! never drift from the write.

use mudra_store::*;
use okm_core::Quant;

fn open_tmp() -> (tempfile::TempDir, MudraStore) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = MudraStore::open(dir.path()).expect("fjall open");
    (dir, store)
}

fn info(target: &str, url: &str, title: &str, opener: &str) -> TargetInfo {
    TargetInfo {
        target_id: target.into(),
        url: url.into(),
        title: title.into(),
        opener_id: opener.into(),
    }
}

// ================= upsert identity order =================

#[test]
fn first_sync_inserts_with_position_and_bumps_epoch_once_per_batch() {
    // Contract: a fresh batch (a) allocates ids from the counter,
    // (b) positions pages by per-instance max+1 order, (c) bumps the epoch
    // exactly once for the whole batch, not per row.
    let (_d, mut s) = open_tmp();
    let batch = vec![
        info("T1", "https://a.test", "A", ""),
        info("T2", "https://b.test", "B", ""),
    ];
    assert_eq!(s.epoch(), 0);
    let e1 = s.sync_targets(7, &batch, 1000, true).expect("batch bumps");
    assert_eq!(e1, 1);

    let pages = s.pages_of_instance(7, false);
    assert_eq!(pages.len(), 2);
    let mut by_pos: Vec<(u32, String)> =
        pages.iter().map(|(_, p)| (p.position, p.url.clone())).collect();
    by_pos.sort();
    assert_eq!(
        by_pos,
        vec![(0, "https://a.test".into()), (1, "https://b.test".into())]
    );
    assert!(pages.iter().all(|(_, p)| p.opened_at == 1000 && p.closed_at == 0));

    // empty batch = no-op, epoch stays
    assert_eq!(s.sync_targets(7, &[], 2000, true), None);
    assert_eq!(s.epoch(), 1);
}

#[test]
fn same_target_resync_is_a_refresh_not_a_second_row() {
    // Contract: (instance, target) identity wins first — a CDP
    // infoChanged (title/url moved) refreshes the row in place; the page
    // keeps its id, position, and opened_at (db.py: UPDATE only touches
    // url/title/closed_at).
    let (_d, mut s) = open_tmp();
    s.sync_targets(7, &[info("T1", "https://a.test", "old title", "")], 1000, true);
    let (k, _) = s.pages_of_instance(7, false).remove(0);

    s.sync_targets(7, &[info("T1", "https://a.test/next", "new title", "")], 2000, true);
    let row = s.pages.get(&k).expect("same row refreshed");
    assert_eq!(row.url, "https://a.test/next");
    assert_eq!(row.title, "new title");
    assert_eq!(row.opened_at, 1000); // untouched by the refresh
    assert_eq!(s.pages_of_instance(7, false).len(), 1);
}

#[test]
fn reopen_with_new_target_revives_the_closed_row() {
    // Contract: a reopened window gets a fresh CDP target id; upsert must
    // find the most recent closed same-URL row and TAKE OVER that row
    // (rebind target_id, clear closed_at, refresh opened_at) instead of
    // inserting a duplicate — the Python E2E bug fixed before the rewrite.
    let (_d, mut s) = open_tmp();
    s.sync_targets(7, &[info("T-old", "https://a.test", "A", "")], 1000, true);
    let (k, _) = s.pages_of_instance(7, false).remove(0);
    s.close_target(7, "T-old", 1500).expect("close bumps");

    let (k2, mode) = s.upsert_target(
        7,
        &info("T-new", "https://a.test", "A again", ""),
        2000,
    true);
    assert_eq!(mode, UpsertMode::Revive);
    assert_eq!(k2, k); // same page id across the reopen
    let row = s.pages.get(&k).unwrap();
    assert_eq!(row.target_id, "T-new");
    assert_eq!(row.closed_at, 0);
    assert_eq!(row.opened_at, 2000);

    // a different URL does NOT revive — it inserts a new row
    let (_, mode) = s.upsert_target(7, &info("T-x", "https://z.test", "Z", ""), 3000, true);
    assert_eq!(mode, UpsertMode::Insert);
}

#[test]
fn reopen_cycles_never_duplicate_the_row() {
    // Contract: close -> reopen -> close -> reopen over one URL keeps a
    // single row (each sync finds the latest closed same-URL row and
    // revives it; no second row is ever inserted).
    let (_d, mut s) = open_tmp();
    s.sync_targets(7, &[info("T1", "https://a.test", "A", "")], 1000, true);
    let (k1, _) = s.pages_of_instance(7, false).remove(0);
    s.close_target(7, "T1", 1100);

    // the second sync already revives k1 (latest closed, same URL)
    s.sync_targets(7, &[info("T2", "https://a.test", "A", "")], 2000, true);
    let k1_after = s.pages.get(&k1).unwrap();
    assert_eq!(k1_after.target_id, "T2");
    assert_eq!(k1_after.closed_at, 0);

    s.close_target(7, "T2", 2500);
    s.sync_targets(7, &[info("T3", "https://a.test", "A", "")], 3000, true);
    let rows = s.pages_of_instance(7, false);
    assert_eq!(rows.len(), 1, "no duplicates across reopens");
    assert_eq!(rows[0].1.target_id, "T3");
}

// ================= parent backfill =================

#[test]
fn opener_id_backfills_once_and_never_overwrites() {
    // Contract: child's parent_id = the opener page (batch-local map
    // first, DB by target for an opener outside the batch); a first sight
    // sets it, a later sync must not move it (page_set_parent_once).
    let (_d, mut s) = open_tmp();
    let parent = info("T-p", "https://p.test", "P", "");
    let child = info("T-c", "https://c.test", "C", "T-p");
    s.sync_targets(7, &[parent, child], 1000, true);

    let (ck, _) = s.page_by_target("T-c").expect("child row");
    let (pk, _) = s.page_by_target("T-p").expect("parent row");
    assert_eq!(s.pages.get(&ck).unwrap().parent_id, pk.id);

    // a second sync with a DIFFERENT opener must not reparent
    s.sync_targets(7, &[info("T-c", "https://c.test/x", "C2", "other")], 2000, true);
    assert_eq!(s.pages.get(&ck).unwrap().parent_id, pk.id);

    // opener outside this batch resolves through the DB (by_target)
    s.sync_targets(7, &[info("T-g", "https://g.test", "G", "T-c")], 3000, true);
    let (gk, _) = s.page_by_target("T-g").unwrap();
    assert_eq!(s.pages.get(&gk).unwrap().parent_id, ck.id);

    // page_children sorts the subtree
    assert_eq!(s.page_children(pk.id).len(), 1);
}

// ================= close & teardown =================

#[test]
fn close_target_marks_closed_and_repeat_close_is_silent() {
    // Contract: the active path only closes the target; the row stays
    // (single teardown = targetDestroyed event -> close_target).
    // A second close of an already-closed target is a no-op (epoch None),
    // mirroring `WHERE closed_at IS NULL`.
    let (_d, mut s) = open_tmp();
    s.sync_targets(7, &[info("T1", "https://a.test", "A", "")], 1000, true);
    assert_eq!(s.close_target(7, "T1", 1500), Some(2)); // epoch after sync = 1
    let row = s
        .pages_of_instance(7, false)
        .into_iter()
        .find(|(_, p)| p.target_id == "T1")
        .expect("closed row still in the live (non-deleted) list")
        .1;
    assert_eq!(row.closed_at, 1500);
    assert_eq!(s.close_target(7, "T1", 1600), None);
    assert_eq!(s.close_target(7, "T-none", 1700), None);
    assert_eq!(s.epoch(), 2);
}

#[test]
fn mark_down_closes_everything_and_flags_the_instance() {
    // Contract: watcher end = running=0 + every open page of the instance
    // gets closed_at (the Python _mark_down pair); an already-down,
    // all-closed instance is a silent no-op.
    let (_d, mut s) = open_tmp();
    let ik = s.launch_started(None, "inbox", 9201, 4242, None, None);
    s.sync_targets(ik.id, &[info("T1", "https://a.test", "A", "")], 1000, true);
    s.sync_targets(ik.id, &[info("T2", "https://b.test", "B", "")], 2000, true);

    let e = s.mark_down(ik.id, 3000).expect("teardown bumps");
    let inst = s.instances.get(&ik).unwrap();
    assert_eq!(inst.running, 0);
    let closed = s.pages_of_instance(ik.id, false);
    assert!(closed.iter().all(|(_, p)| p.closed_at == 3000));

    assert_eq!(s.mark_down(ik.id, 4000), None); // idempotent silence
    assert_eq!(s.epoch(), e);
}

// ================= soft delete rule =================

#[test]
fn delete_page_requires_closed_and_deletes_once() {
    // Contract: the "only closed pages can be deleted" invariant lives in
    // the store; deleting an open page errors, deleting twice errors, an
    // unknown page errors. deleted_at rows stay readable by history lists.
    let (_d, mut s) = open_tmp();
    s.sync_targets(7, &[info("T1", "https://a.test", "A", "")], 1000, true);
    let (k, _) = s.page_by_target("T1").unwrap();

    assert!(s.delete_page(&k, 1500).is_err()); // still open
    s.close_target(7, "T1", 1600);
    let before = s.epoch();
    assert_eq!(s.delete_page(&k, 1700).unwrap(), before + 1);
    assert_eq!(s.pages.get(&k).unwrap().deleted_at, 1700);

    // live lists hide it, history lists keep it
    assert!(s.pages_of_instance(7, false).is_empty());
    assert_eq!(s.pages_of_instance(7, true).len(), 1);

    assert!(s.delete_page(&k, 1800).is_err()); // already deleted
    assert!(s.delete_page(&PageKey { id: 999 }, 1800).is_err()); // unknown
}

// ================= instance lifecycle plumbing =================

#[test]
fn launch_started_reuses_rows_and_carries_config() {
    // Contract: profile-keyed instance row; a new launch reuses the old
    // row (proxy/extensions untouched, port/pid/running refreshed) —
    // instance_for_context finds exactly the latest row per profile.
    let (_d, mut s) = open_tmp();
    let k1 = s.launch_started(None, "work", 9201, 100, Some("http://127.0.0.1:7890"), Some("/ext"));
    let (found, inst) = s.instance_for_context("work").expect("profile indexed");
    assert_eq!(found, k1);
    assert_eq!(inst.proxy, "http://127.0.0.1:7890");
    assert_eq!(inst.extensions, "/ext");

    // reuse: same id, new port/pid, config survives
    let k2 = s.launch_started(Some(&k1), "work", 9202, 200, None, None);
    assert_eq!(k2, k1);
    let (_, inst) = s.instance_for_context("work").unwrap();
    assert_eq!((inst.port, inst.pid, inst.running), (9202, 200, 1));
    assert_eq!(inst.proxy, "http://127.0.0.1:7890");

    assert!(s.instance_for_context("personal").is_none());
}

#[test]
fn site_width_row_upsert_by_site() {
    // Contract: one row per site (remembered window width); a second put
    // for the same host overwrites through the func index roundtrip.
    let (_d, mut s) = open_tmp();
    let k = SiteWidthKey { id: s.next_id(state::SITE_WIDTH_ID) as u32 };
    s.site_widths.put(&k, &SiteWidth { site: "a.test".into(), proportion: Quant::<4>::new(0.5) });
    let (k2, mut w) = s.site_width("a.test").unwrap();
    assert_eq!(k2, k);
    w.proportion = Quant::<4>::new(0.8);
    s.site_widths.put(&k2, &w);
    let (_, w) = s.site_width("a.test").unwrap();
    assert_eq!(w.proportion.0, 0.8);
    assert_eq!(s.site_widths.scan_keys().len(), 1, "overwrite, not append");
}

// ================= tag toggle & paths =================

#[test]
fn page_tag_toggle_roundtrip_bumps_epoch() {
    // Contract: toggle adds then removes ("added"/"removed" semantics);
    // every toggle invalidates the panel (returns true/false = added).
    let (_d, mut s) = open_tmp();
    s.sync_targets(7, &[info("T1", "https://a.test", "A", "")], 1000, true);
    let (pk, _) = s.page_by_target("T1").unwrap();
    let tk = TagKey { id: s.next_id(state::TAG_ID) as u32 };
    s.tags.put(&tk, &Tag { name: "unread".into(), parent_id: -1, ..Default::default() });

    let e0 = s.epoch();
    assert!(s.page_tag_toggle(&pk, &tk, 9000)); // added
    assert_eq!(s.epoch(), e0 + 1);
    assert_eq!(s.tags_of_page(&pk), vec![tk.clone()]);
    assert_eq!(s.pages_of_tag(&tk), vec![pk.clone()]);

    assert!(!s.page_tag_toggle(&pk, &tk, 9000)); // removed
    assert!(s.tags_of_page(&pk).is_empty());
}

#[test]
fn tag_path_walks_parents_to_the_root() {
    // Contract: the capsule path = root..name joined with "::"; a root
    // node returns its bare name; the walk stops at parent_id == -1.
    let (_d, mut s) = open_tmp();
    let root = TagKey { id: s.next_id(state::TAG_ID) as u32 };
    s.tags.put(&root, &Tag { name: "state".into(), parent_id: -1, ..Default::default() });
    let mid = TagKey { id: s.next_id(state::TAG_ID) as u32 };
    s.tags.put(&mid, &Tag { name: "mood".into(), parent_id: root.id as i32, ..Default::default() });
    let leaf = TagKey { id: s.next_id(state::TAG_ID) as u32 };
    s.tags.put(&leaf, &Tag { name: "deep".into(), parent_id: mid.id as i32, ..Default::default() });

    assert_eq!(s.tag_path_string(&leaf), "state::mood::deep");
    assert_eq!(s.tag_path_string(&root), "state");
}

// ================= current_context state =================

#[test]
fn context_switch_validates_against_the_situation_tree() {
    // Contract: /ctx only accepts a leaf under the situation root; the
    // switch writes the state row and bumps the epoch (panel header must
    // re-render). Port of db.set_context's tree check + ctl_ctx broadcast.
    let (_d, mut s) = open_tmp();
    let sit = TagKey { id: s.next_id(state::TAG_ID) as u32 };
    s.tags.put(&sit, &Tag { name: "situation".into(), parent_id: -1, required: 1, ..Default::default() });
    let leaf = TagKey { id: s.next_id(state::TAG_ID) as u32 };
    s.tags.put(&leaf, &Tag { name: "work".into(), parent_id: sit.id as i32, isolated: 1, ..Default::default() });

    let e = s.set_context("work").expect("valid leaf bumps");
    assert_eq!(s.state_text(state::CURRENT_CONTEXT), "work");
    assert_eq!(s.epoch(), e);

    assert!(s.set_context("nope").is_none()); // not a leaf: silent reject
    assert_eq!(s.state_text(state::CURRENT_CONTEXT), "work");
}

// ================= event-log orchestration (ADR-extension-protocol §4) =================

#[test]
fn observe_false_baseline_replay_emits_no_events() {
    // Contract: the reconnect baseline is NOT a page event — a daemon
    // restart must not flood the log with re-observations of pages that
    // did not change. The same sync with observe=true would emit two
    // page_open rows (Insert path).
    let (_d, mut s) = open_tmp();
    s.sync_targets(7, &[info("T1", "https://a.test", "A", ""), info("T2", "https://b.test", "B", "")], 1000, false);
    assert!(s.events_since(0, 10).is_empty());
}

#[test]
fn page_lifecycle_transitions_emit_the_snapshot_kinds() {
    // Contract: insert + revive + refresh-navigation emit page_open; the
    // title-only refresh stays silent (metadata, not an event); close
    // paths (watcher + verb) emit page_close; the mark_down sweep emits
    // one close per still-open page (corpses get no destroyed events).
    let (_d, mut s) = open_tmp();
    let ik = InstanceKey { id: s.next_id(state::INSTANCE_ID) as u32 };
    s.instances.put(&ik, &Instance { profile: "work".into(), port: 9301, pid: 1, running: 1, ..Default::default() });

    s.sync_targets(ik.id, &[info("T1", "https://a.test", "A", "")], 1000, true); // Insert -> open
    s.sync_targets(ik.id, &[info("T1", "https://a.test", "A2", "")], 1100, true); // title-only -> silent
    s.sync_targets(ik.id, &[info("T1", "https://a.test/x", "X", "")], 1200, true); // navigation -> open
    s.close_target(ik.id, "T1", 1300); // watcher teardown -> close
    let ev: Vec<(String, u64)> = s.events_since(0, 10).into_iter().map(|(_, e)| (e.kind, e.at)).collect();
    assert_eq!(ev, vec![
        ("mudra:page_open".to_string(), 1000),
        ("mudra:page_open".to_string(), 1200),
        ("mudra:page_close".to_string(), 1300),
    ]);

    // mark_down sweep: reopen one page, kill the instance — one close.
    s.sync_targets(ik.id, &[info("T9", "https://z.test", "Z", "")], 2000, true);
    let before = s.events_since(0, 1000).len();
    s.mark_down(ik.id, 2500);
    let new = s.events_since(0, 1000)[before..].to_vec();
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].1.kind, "mudra:page_close");
}

#[test]
fn event_args_carry_the_self_describing_snapshot() {
    // Contract (args discipline): kind/page_id/ctx/url/title ride the
    // row — a consumer never calls back into mudra. ctx resolves through
    // the instance row. tag_set carries the FULL post-change id set.
    let (_d, mut s) = open_tmp();
    let ik = InstanceKey { id: s.next_id(state::INSTANCE_ID) as u32 };
    s.instances.put(&ik, &Instance { profile: "work".into(), ..Default::default() });
    s.sync_targets(ik.id, &[info("T1", "https://a.test", "A", "")], 1000, true);
    let (_, e) = &s.events_since(0, 1)[0];
    let args: serde_json::Value = serde_json::from_str(&e.args).unwrap();
    assert_eq!(args["kind"], "mudra:page_open");
    assert_eq!(args["ctx"], "work");
    assert_eq!(args["url"], "https://a.test");
    assert_eq!(args["title"], "A");

    let pk = s.page_by_target("T1").unwrap().0;
    let t1 = TagKey { id: s.next_id(state::TAG_ID) as u32 };
    let t2 = TagKey { id: s.next_id(state::TAG_ID) as u32 };
    s.tags.put(&t1, &Tag { name: "read".into(), ..Default::default() });
    s.tags.put(&t2, &Tag { name: "star".into(), ..Default::default() });
    s.page_tag_toggle(&pk, &t1, 1500);
    s.page_tag_replace(&pk, &[t1.id, t2.id], 1600);
    let tags: Vec<serde_json::Value> = s.events_since(0, 100)
        .into_iter()
        .filter(|(_, e)| e.kind == "mudra:tag_set")
        .map(|(_, e)| serde_json::from_str(&e.args).unwrap())
        .collect();
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0]["tag_ids"], serde_json::json!([t1.id]));
    assert_eq!(tags[1]["tag_ids"], serde_json::json!([t1.id, t2.id]));
    assert_eq!(tags[1]["page_id"], serde_json::json!(pk.id));
}
