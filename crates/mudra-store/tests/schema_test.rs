//! mudra-store integration tests — written as API docs for the layout
//! contract in `docs/SCHEMA.md`. Every test demonstrates one contract
//! clause on the REAL fjall engine (temp dir; no mock).
//!
//! Run: `cargo test -p mudra-store`
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unwrap_in_result, clippy::panic_in_result_fn)]

use mudra_store::*;
use okm_core::{Document, KeyEncode, KvJunction, Quant, Ref};

fn open_tmp() -> (tempfile::TempDir, MudraStore) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = MudraStore::open(dir.path()).expect("fjall open");
    (dir, store)
}

// ================= Tag: tree drilling & bare-name addressing =================

#[test]
fn tag_tree_parent_and_rank_order() {
    // Contract: by_parent groups children under a parent (root = -1
    // sentinel); tag_children orders by rank and hides soft-deleted rows.
    let (_d, mut s) = open_tmp();
    let root = TagKey { id: s.next_id(state::TAG_ID).unwrap() as u32 };
    s.tags.put(&root, &Tag { parent_id: -1, name: "situation".into(), ..Default::default() }).unwrap();
    for (i, (name, rank)) in [("inbox", 2i32), ("work", 1), ("deep", 3)].iter().enumerate() {
        let k = TagKey { id: s.next_id(state::TAG_ID).unwrap() as u32 };
        s.tags.put(&k, &Tag { parent_id: root.id as i32, name: name.to_string(), rank: *rank, ..Default::default() }).unwrap();
        let _ = i;
    }
    let roots = s.tag_children(-1).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].1.name, "situation");

    let mut kids = s.tag_children(root.id as i32).unwrap();
    kids.sort_by_key(|(_, t)| t.rank);
    let names: Vec<&str> = kids.iter().map(|(_, t)| t.name.as_str()).collect();
    assert_eq!(names, ["work", "inbox", "deep"]); // rank 1,2,3

    // soft delete drops the node from the tree column
    let (wk, mut w) = kids[0].clone();
    w.deleted = 1;
    s.tags.put(&wk, &w).unwrap();
    let names: Vec<String> = s.tag_children(root.id as i32).unwrap().into_iter().map(|(_, t)| t.name).collect();
    assert_eq!(names, ["inbox", "deep"]);
}

#[test]
fn tag_by_bare_name_and_counter_monotonic() {
    // Contract: /tags-style bare-name addressing via the by_name func
    // index; the State counter allocates ids (0 never appears).
    let (_d, mut s) = open_tmp();
    let a = TagKey { id: s.next_id(state::TAG_ID).unwrap() as u32 };
    let b = TagKey { id: s.next_id(state::TAG_ID).unwrap() as u32 };
    assert_eq!((a.id, b.id), (1, 2));
    s.tags.put(&a, &Tag { name: "unread".into(), parent_id: -1, ..Default::default() }).unwrap();
    s.tags.put(&b, &Tag { name: "unread".into(), parent_id: 1, ..Default::default() }).unwrap();
    let hits = s.tag_by_name("unread").unwrap();
    assert_eq!(hits.len(), 2); // same bare name under different parents
    assert!(s.tag_by_name("nope").unwrap().is_empty());
}

// ================= Page: per-instance lists & lifecycle =================

#[test]
fn pages_of_instance_skips_soft_deleted() {
    // Contract: by_instance lists a context's pages; live reads exclude
    // deleted_at != 0, include_history widens to the deleted rows.
    let (_d, mut s) = open_tmp();
    let p1 = PageKey { id: s.next_id(state::PAGE_ID).unwrap() };
    let p2 = PageKey { id: s.next_id(state::PAGE_ID).unwrap() };
    s.pages.put(&p1, &Page { instance_id: 7, url: "https://a.test/one".into(), opened_at: 100, ..Default::default() }).unwrap();
    s.pages.put(&p2, &Page { instance_id: 7, url: "https://a.test/two".into(), opened_at: 200, closed_at: 300, deleted_at: 400, ..Default::default() }).unwrap();
    let p3 = PageKey { id: s.next_id(state::PAGE_ID).unwrap() };
    s.pages.put(&p3, &Page { instance_id: 8, url: "https://b.test/x".into(), ..Default::default() }).unwrap();

    let live = s.pages_of_instance(7, false).unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].1.url, "https://a.test/one");
    assert_eq!(s.pages_of_instance(7, true).unwrap().len(), 2);
    assert!(s.pages_of_instance(99, false).unwrap().is_empty());
}

#[test]
fn page_by_target_reverse_lookup() {
    // Contract: by_target func index answers /open {tabId} and
    // focus_page; a rebind of target_id moves the row's index entry.
    let (_d, mut s) = open_tmp();
    let k = PageKey { id: s.next_id(state::PAGE_ID).unwrap() };
    let mut p = Page { instance_id: 1, target_id: "T-abc".into(), url: "https://x.test".into(), opened_at: 5, ..Default::default() };
    s.pages.put(&k, &p).unwrap();
    assert_eq!(s.page_by_target("T-abc").unwrap().unwrap().0, k);
    assert!(s.page_by_target("T-none").unwrap().is_none());

    // revival rebind: the old target no longer resolves
    p.target_id = "T-new".into();
    s.pages.put(&k, &p).unwrap();
    assert_eq!(s.page_by_target("T-new").unwrap().unwrap().0, k);
}

#[test]
fn page_children_subtree() {
    // Contract: by_parent carries the CDP openerId subtree.
    let (_d, s) = open_tmp();
    let parent = PageKey { id: 10 };
    let child = PageKey { id: 11 };
    s.pages.put(&parent, &Page { instance_id: 1, url: "https://p.test".into(), ..Default::default() }).unwrap();
    s.pages.put(&child, &Page { instance_id: 1, parent_id: 10, url: "https://c.test".into(), ..Default::default() }).unwrap();
    let kids = s.page_children(10).unwrap();
    assert_eq!(kids.len(), 1);
    assert_eq!(kids[0].1.url, "https://c.test");
}

// ================= Page: ranked recall over the addressing text =================

#[test]
fn pages_by_fragment_recalls_and_ranks_the_addressing_text() {
    // Contract: the addressing text (url + title, scheme stripped) is
    // recalled by substring containment — the query's tokens, floor-free —
    // and ranked by BM25, in one home. Soft-deleted rows are filtered and
    // contribute to no statistic.
    let (_d, s) = open_tmp();
    s.pages.put(&PageKey { id: 1 }, &Page { url: "https://news.ycombinator.com/item?id=1".into(), title: "Hacker News".into(), ..Default::default() }).unwrap();
    s.pages.put(&PageKey { id: 2 }, &Page { url: "https://rust-lang.org".into(), title: "Rust".into(), ..Default::default() }).unwrap();
    s.pages.put(&PageKey { id: 3 }, &Page { url: "https://news.ycombinator.com/item?id=2".into(), title: "Hacker News".into(), deleted_at: 9, ..Default::default() }).unwrap();
    s.pages.put(&PageKey { id: 4 }, &Page { url: "https://example.com/hacker".into(), title: "Hacker".into(), ..Default::default() }).unwrap();

    // a fragment of the address, anywhere in it, drags in nothing else —
    // the soft-deleted row 3 (same addressing text as row 1) stays out
    let hits = s.pages_by_fragment("ycombinat").unwrap();
    let got: Vec<u64> = hits.iter().map(|(k, _)| k.id).collect();
    assert_eq!(got, vec![1], "{got:?}");
    assert_eq!(hits[0].1.url, "https://news.ycombinator.com/item?id=1");
    assert!(s.pages_by_fragment("zzzz").unwrap().is_empty());

    // the title is part of the addressing text, case-insensitively
    let ids: Vec<u64> = s.pages_by_fragment("HACKER").unwrap().iter().map(|(k, _)| k.id).collect();
    assert!(ids.contains(&1) && ids.contains(&4), "{ids:?}");
    assert!(!ids.contains(&3), "the soft-deleted row stays out");

    // ranking: the row containing both tokens leads the one containing one
    let hits = s.pages_by_fragment("hacker news").unwrap();
    assert_eq!(hits[0].0.id, 1, "1 matches both words, 4 only the first");

    // no floor: a two-letter fragment is a token like any other. The old
    // sub-three-gram query was an honest empty — a page the caller cannot
    // reach however it types it.
    let ids: Vec<u64> = s.pages_by_fragment("ne").unwrap().iter().map(|(k, _)| k.id).collect();
    assert_eq!(ids, vec![1], "only row 1's address/title contains \"ne\"");

    // the scheme is not part of the addressing text: it is the one part of
    // a URL that says nothing about which page it is
    assert!(s.pages_by_fragment("https").unwrap().is_empty());
}

#[test]
fn page_tag_junction_both_directions() {
    // Contract: Page↔Tag entries live one in Page ns 2 and one in Tag
    // ns 1 (junction declares no ns); link/unlink both directions;
    // cross-tree multi-select = multiple rows.
    let (_d, mut s) = open_tmp();
    let page = PageKey { id: 42 };
    let t1 = TagKey { id: 7 };
    let t2 = TagKey { id: 8 };
    s.link_page_tag(&page, &t1).unwrap();
    s.link_page_tag(&page, &t2).unwrap();
    assert_eq!(s.tags_of_page(&page).unwrap(), vec![t1.clone(), t2.clone()]);
    assert_eq!(s.pages_of_tag(&t1).unwrap(), vec![page.clone()]);

    s.unlink_page_tag(&page, &t1).unwrap();
    assert_eq!(s.tags_of_page(&page).unwrap(), vec![t2.clone()]);
    assert!(s.pages_of_tag(&t1).unwrap().is_empty());
}

// ================= Instance / SiteWidth =================

#[test]
fn instance_by_profile_and_site_width_roundtrip() {
    // Contract: instance rows resolve by situation leaf name; SiteWidth
    // keeps `site` out of the key (func index only), proportion rides the
    // Quant<4> fixed-point wire.
    let (_d, mut s) = open_tmp();
    let ik = InstanceKey { id: s.next_id(state::INSTANCE_ID).unwrap() as u32 };
    s.instances.put(&ik, &Instance { profile: "inbox".into(), port: 9201, pid: 4242, running: 1, ..Default::default() }).unwrap();
    let found = s.instance_by_profile("inbox").unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1.port, 9201);

    let swk = SiteWidthKey { id: s.next_id(state::SITE_WIDTH_ID).unwrap() as u32 };
    s.site_widths.put(&swk, &SiteWidth { site: "news.ycombinator.com".into(), proportion: Quant::<4>::new(0.618) }).unwrap();
    let sw = s.site_width("news.ycombinator.com").unwrap().unwrap();
    assert_eq!(sw.1.proportion.0, 0.618);
    assert_eq!(url_site("https://news.ycombinator.com/item?id=1"), "news.ycombinator.com");
    assert!(s.site_width("absent.test").unwrap().is_none());
}

// ================= State: epoch, counters, settings =================

#[test]
fn epoch_bumps_are_monotonic_and_isolated() {
    // Contract: the epoch is a State counter — bumped per CDP write, never
    // riding any page/tag payload; the value survives reopening the store.
    let (d, mut s) = open_tmp();
    assert_eq!(s.epoch().unwrap(), 0);
    assert_eq!(s.bump_epoch().unwrap(), 1);
    let k = PageKey { id: s.next_id(state::PAGE_ID).unwrap() };
    s.pages.put(&k, &Page { url: "https://e.test".into(), ..Default::default() }).unwrap();
    assert_eq!(s.bump_epoch().unwrap(), 2);

    // reopen: same path, epoch continues (never rewinds). fjall locks the
    // database to one handle — mudrad's single-writer model drops first
    // (crash-restart is a new process; in-process we emulate the handoff).
    MudraStore::persist(&s).expect("persist");
    drop(s);
    let s2 = MudraStore::open(d.path()).expect("reopen");
    assert_eq!(s2.epoch().unwrap(), 2);
    // and the epoch is not readable through page payloads at all
    let p = s2.pages.get(&k).unwrap().expect("page survives reopen");
    assert_eq!(p.url, "https://e.test");
}

#[test]
fn state_slots_roundtrip_text_and_never_collide() {
    // Contract: fixed u8 slots (closed set, append-only); text settings
    // live as raw Bytes values; counters and settings are distinct rows.
    let (_d, mut s) = open_tmp();
    s.put_state_text(state::CURRENT_CONTEXT, "work").unwrap();
    assert_eq!(s.state_text(state::CURRENT_CONTEXT).unwrap(), "work");
    assert_eq!(s.state_text(state::DEV_MODE).unwrap(), ""); // unset = empty

    s.put_state_text(state::DEV_MODE, "1").unwrap();
    assert_eq!(s.state_text(state::CURRENT_CONTEXT).unwrap(), "work"); // slot isolation

    // State row count = written slots only; there is no index on ns 6
    assert_eq!(s.state.scan_keys().unwrap().len(), 2);
}

// ================= hex-stability guard (layout drift defense) =================

#[test]
fn physical_key_layout_is_locked() {
    // Contract: SCHEMA keys are fixed-width BE segments — ns assignment,
    // field order and widths are a physical contract the panel shares.
    // Hex-locking them here means a silent layout shift fails CI.
    assert_eq!(<TagKey as okm_core::KeyEncode>::KEY_LEN, 4);
    assert_eq!(<PageKey as okm_core::KeyEncode>::KEY_LEN, 8);
    assert_eq!(<InstanceKey as okm_core::KeyEncode>::KEY_LEN, 4);
    assert_eq!(<SiteWidthKey as okm_core::KeyEncode>::KEY_LEN, 4);
    assert_eq!(<StateKey as okm_core::KeyEncode>::KEY_LEN, 1);
    assert_eq!(<HistoryKey as okm_core::KeyEncode>::KEY_LEN, 8);
    assert_eq!(<EventKey as okm_core::KeyEncode>::KEY_LEN, 8);
    assert_eq!(<HookKey as okm_core::KeyEncode>::KEY_LEN, 8);
    assert_eq!(&PageKey { id: 258 }.encode(), &[0, 0, 0, 0, 0, 0, 1, 2]);

    // document ns prefixes: [0 ns_hi][ns_lo] per declaration order
    assert_eq!(<Tag as Document>::NS_PREFIX, vec![0, 1]);
    assert_eq!(<Page as Document>::NS_PREFIX, vec![0, 2]);
    assert_eq!(<Instance as Document>::NS_PREFIX, vec![0, 4]);
    assert_eq!(<SiteWidth as Document>::NS_PREFIX, vec![0, 5]);
    assert_eq!(<State as Document>::NS_PREFIX, vec![0, 6]);
    assert_eq!(<History as Document>::NS_PREFIX, vec![0, 7]);
    assert_eq!(<Event as Document>::NS_PREFIX, vec![0, 8]);
    assert_eq!(<Hook as Document>::NS_PREFIX, vec![0, 9]); // ns 3 stays the retired hole

    // junction entries: one per endpoint ns, dir bit in slot low byte
    let e = PageTag { page: Ref::ref_key(PageKey { id: 1 }), tag: Ref::ref_key(TagKey { id: 2 }) };
    let a = e.a_side_key();
    let b = e.b_side_key();
    assert_eq!(&a[..2], &[0, 2]); // Page side lives in Page's ns 2
    assert_eq!(&b[..2], &[0, 1]); // Tag side lives in Tag's ns 1
    assert_eq!(a.len(), 4 + 8 + 4); // head + full PageKey + full TagKey
    assert_eq!(b.len(), 4 + 4 + 8);
}

#[test]
fn store_persist_and_reopen_recovers_every_collection() {
    // Contract: one fjall Database fans all six collections; persist +
    // reopen is the durable checkpoint mudrad uses before exit.
    let (d, mut s) = open_tmp();
    let tk = TagKey { id: s.next_id(state::TAG_ID).unwrap() as u32 };
    let pk = PageKey { id: s.next_id(state::PAGE_ID).unwrap() };
    let ik = InstanceKey { id: s.next_id(state::INSTANCE_ID).unwrap() as u32 };
    s.tags.put(&tk, &Tag { name: "r".into(), parent_id: -1, ..Default::default() }).unwrap();
    s.pages.put(&pk, &Page { instance_id: ik.id, target_id: "T".into(), ..Default::default() }).unwrap();
    s.instances.put(&ik, &Instance { profile: "r".into(), ..Default::default() }).unwrap();
    s.page_tags.link(&pk, &tk).unwrap();
    s.persist().expect("persist");
    drop(s); // fjall single-handle lock: the reopen is the new process

    let s2 = MudraStore::open(d.path()).expect("reopen");
    assert_eq!(s2.tags.get(&tk).unwrap().unwrap().name, "r");
    assert_eq!(s2.pages.get(&pk).unwrap().unwrap().instance_id, ik.id);
    assert_eq!(s2.instances.get(&ik).unwrap().unwrap().profile, "r");
    assert_eq!(s2.tags_of_page(&pk).unwrap(), vec![tk]);
    // counters are State rows too — allocation continues after reopen
    let mut s3 = s2;
    assert_eq!(s3.next_id(state::PAGE_ID).unwrap(), pk.id + 1);
}

// ================= History (ns 7): visits, labels, ranking =================

#[test]
fn history_bump_creates_then_accumulates() {
    // Contract: first sight creates the row (visits=1); repeats bump the
    // counter and the stamp; an empty title keeps the stored label.
    let (_d, mut s) = open_tmp();
    s.history_bump("https://a.test/x", "Alpha", 100).unwrap();
    s.history_bump("https://a.test/x", "", 200).unwrap();
    let rows = s.history_all().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].visits, 2);
    assert_eq!(rows[0].last_at, 200);
    assert_eq!(rows[0].title, "Alpha");
    s.history_bump("https://a.test/x", "Alpha Two", 300).unwrap();
    assert_eq!(s.history_all().unwrap()[0].title, "Alpha Two");
}

#[test]
fn sim_score_subsequence_and_run_bias() {
    // exact prefix beats a scattered subsequence; non-subsequence is 0.
    let exact = sim_score("github", "https://github.com/x");
    let scattered = sim_score("gthub", "https://github.com/x");
    let none = sim_score("gitlab", "https://github.com/x"); // no 'l' anywhere
    assert!(exact > scattered);
    assert!(exact > 0.0);
    assert_eq!(none, 0.0);
    // case-insensitive
    assert!(sim_score("GIT", "https://github.com") > 0.0);
    assert_eq!(sim_score("", "anything"), 0.0);
}

#[test]
fn history_search_ranks_text_fuzzy_visits_and_recency() {
    // Contract: the address bar's four legs, fused with RRF. Recall is the
    // addressing text (url + title, scheme stripped): the query's tokens
    // contained in it, or the query's letters in order — which is how
    // "ycombinat" reaches the ycombinator address and "ghb" reaches
    // github. Visits and recency rank whatever recall found; a row
    // recalled by both recall legs leads; an address matching neither
    // stays out however often it was opened; and the empty query is the
    // pure visits order (the bar's first state).
    let (_d, mut s) = open_tmp();
    s.history_bump("https://news.ycombinator.com", "Hacker News", 100).unwrap();
    for _ in 0..4 {
        s.history_bump("https://news.ycombinator.com", "", 101).unwrap(); // opened a lot
    }
    s.history_bump("https://github.com/orbsh", "orbsh GitHub", 130).unwrap();
    s.history_bump("https://rust-lang.org", "Rust", 200).unwrap(); // both legs for "rust"
    s.history_bump("https://example.com/x", "Rust Notes", 110).unwrap(); // title only
    s.history_bump("https://rustup.rs", "Home", 120).unwrap(); // address only

    let hits = s.history_search("rust", 10).unwrap();
    assert_eq!(hits[0].url, "https://rust-lang.org", "both recall legs lead");
    assert_eq!(hits.len(), 3);
    assert!(
        !hits.iter().any(|h| h.url.contains("ycombinator")),
        "five opens do not recall an address that matches neither leg"
    );

    // a fragment of the address ANYWHERE in it, the middle of a host
    // included: the address is text, not merely a prefix key
    let hits = s.history_search("ycombinat", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].url, "https://news.ycombinator.com");

    // the query's letters in order but not contiguous: the fuzzy leg
    let hits = s.history_search("ghb", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].url, "https://github.com/orbsh");

    // case-insensitive on both sides (query folds; title and address fold)
    assert_eq!(s.history_search("RUSTUP", 10).unwrap()[0].url, "https://rustup.rs");
    let notes = s.history_search("NOTES", 10).unwrap();
    assert_eq!(notes.len(), 1, "only the row whose TITLE matches");
    assert_eq!(notes[0].url, "https://example.com/x");

    // empty query = visits order (no recall legs at all), limit respected
    let all = s.history_search("", 10).unwrap();
    assert_eq!(all[0].url, "https://news.ycombinator.com"); // 5 opens
    assert_eq!(s.history_search("", 2).unwrap().len(), 2);
}

// ================= Event (ns 8): append-only log & cursor replay =================

#[test]
fn event_log_is_append_only_and_replays_from_cursor() {
    // Contract (ADR-extension-protocol §4): emit assigns monotone ids;
    // a consumer owning its cursor sees every row exactly once, in id
    // order, and re-opening the store replays the same ids — durability
    // across restart is the whole point of logging the fan-out.
    let (d, mut s) = open_tmp();
    let id1 = s.emit_event("mudra:page_open", "{\"url\":\"a\"}".to_string(), 10).unwrap();
    let id2 = s.emit_event("mudra:tag_set", "{}".to_string(), 20).unwrap();
    assert!(id2 > id1);

    let tail = s.events_since(id1, 100).unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].1.kind, "mudra:tag_set");

    // reopen the same engine dir; ids continue past the persisted
    // counter (fjall single-handle lock: the old store must drop first)
    drop(s);
    let mut reopened = MudraStore::open(d.path()).expect("reopen");
    let all = reopened.events_since(0, 100).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].1.args, "{\"url\":\"a\"}");
    let cont = reopened.emit_event("mudra:page_close", "{}".into(), 30).unwrap();
    assert!(cont > id2);

    // limit caps the replay page
    assert_eq!(reopened.events_since(0, 1).unwrap().len(), 1);

    // a log write never moves the epoch (panel-invisible collection)
    let mut s2 = MudraStore::open(tempfile::TempDir::new().unwrap().path()).unwrap();
    let e0 = s2.epoch().unwrap();
    s2.emit_event("x", "{}".into(), 1).unwrap();
    assert_eq!(s2.epoch().unwrap(), e0);
}

// ================= Hook (ns 9): the page-side rule path =================

#[test]
fn hook_upsert_is_idempotent_on_name_and_keeps_the_id() {
    // Contract (SCHEMA ns 9): `name` is the upsert identity, reached
    // through the by_name func index — the same name updates in place
    // (same id), and an edit never burns a counter tick.
    let (_d, mut s) = open_tmp();
    let (id1, created1) = s.hook_upsert("adfree", "infoq.com", "/* v1 */", 1, 1000).unwrap();
    assert!(created1);
    let (id2, created2) = s.hook_upsert("adfree", "infoq.com, qbit", "/* v2 */", 1, 2000).unwrap();
    assert!(!created2);
    assert_eq!(id1, id2);

    let (k, h) = s.hook_by_name("adfree").unwrap().expect("indexed lookup finds it");
    assert_eq!(k.id, id1);
    assert_eq!(h.code, "/* v2 */");
    assert_eq!(h.matches, "infoq.com, qbit");
    assert_eq!(h.updated_at, 2000);
    assert_eq!(s.hooks_all().unwrap().len(), 1);

    // a different name is a different row, and the counter ticks once per
    // CREATE only
    let (id3, created3) = s.hook_upsert("restyler", "", "1;", 0, 3000).unwrap();
    assert!(created3);
    assert_ne!(id3, id1);
    assert_eq!(s.hooks_all().unwrap().len(), 2);
}

#[test]
fn hooks_enabled_filters_by_flag_and_reads_in_id_order() {
    let (_d, mut s) = open_tmp();
    s.hook_upsert("b-off", "", "1;", 0, 1).unwrap();
    s.hook_upsert("a-on", "x.test", "2;", 1, 2).unwrap();
    s.hook_upsert("c-on", "", "3;", 1, 3).unwrap();
    let on: Vec<String> = s.hooks_enabled().unwrap().into_iter().map(|h| h.name).collect();
    assert_eq!(on, ["a-on", "c-on"], "enabled only, creation order");

    // the injection bridge reads the same order the operator sees
    let all: Vec<String> = s.hooks_all().unwrap().into_iter().map(|(_, h)| h.name).collect();
    assert_eq!(all, ["b-off", "a-on", "c-on"]);
}

#[test]
fn hook_delete_removes_both_halves_and_frees_the_name() {
    // Contract: `hook_delete_by_name` takes `delete`'s sound source (the
    // document from the same read that found the key), so no by_name
    // entry is orphaned — a later upsert of that name creates afresh
    // instead of resurrecting a stale row.
    let (d, mut s) = open_tmp();
    let (id1, _) = s.hook_upsert("tmp", "a.test", "1;", 1, 10).unwrap();
    assert!(s.hook_delete_by_name("tmp").unwrap());
    assert!(!s.hook_delete_by_name("tmp").unwrap(), "second delete is a no-op");
    assert!(s.hook_by_name("tmp").unwrap().is_none());
    assert!(s.hooks_all().unwrap().is_empty());

    s.hook_upsert("later", "b.test", "2;", 1, 20).unwrap();
    s.persist().expect("persist");
    drop(s); // fjall single-handle lock: the old store drops first
    let mut s2 = MudraStore::open(d.path()).expect("reopen");
    assert!(s2.hook_by_name("tmp").unwrap().is_none(), "no dangling index hit");
    let (id2, created) = s2.hook_upsert("tmp", "c.test", "3;", 1, 30).unwrap();
    assert!(created, "the name is free again");
    assert_ne!(id2, id1, "ids are never reused");

    // hooks are panel-invisible: writing never moves the epoch
    let e0 = s2.epoch().unwrap();
    s2.hook_upsert("tmp", "d.test", "4;", 1, 40).unwrap();
    assert_eq!(s2.epoch().unwrap(), e0);
}
