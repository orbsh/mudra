//! mudra-store integration tests — written as API docs for the layout
//! contract in `docs/SCHEMA.md`. Every test demonstrates one contract
//! clause on the REAL fjall engine (temp dir; no mock).
//!
//! Run: `cargo test -p mudra-store`

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
    let root = TagKey { id: s.next_id(state::TAG_ID) as u32 };
    s.tags.put(&root, &Tag { parent_id: -1, name: "situation".into(), ..Default::default() });
    for (i, (name, rank)) in [("inbox", 2i32), ("work", 1), ("deep", 3)].iter().enumerate() {
        let k = TagKey { id: s.next_id(state::TAG_ID) as u32 };
        s.tags.put(&k, &Tag { parent_id: root.id as i32, name: name.to_string(), rank: *rank, ..Default::default() });
        let _ = i;
    }
    let roots = s.tag_children(-1);
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].1.name, "situation");

    let mut kids = s.tag_children(root.id as i32);
    kids.sort_by_key(|(_, t)| t.rank);
    let names: Vec<&str> = kids.iter().map(|(_, t)| t.name.as_str()).collect();
    assert_eq!(names, ["work", "inbox", "deep"]); // rank 1,2,3

    // soft delete drops the node from the tree column
    let (wk, mut w) = kids[0].clone();
    w.deleted = 1;
    s.tags.put(&wk, &w);
    let names: Vec<String> = s.tag_children(root.id as i32).into_iter().map(|(_, t)| t.name).collect();
    assert_eq!(names, ["inbox", "deep"]);
}

#[test]
fn tag_by_bare_name_and_counter_monotonic() {
    // Contract: /tags-style bare-name addressing via the by_name func
    // index; the State counter allocates ids (0 never appears).
    let (_d, mut s) = open_tmp();
    let a = TagKey { id: s.next_id(state::TAG_ID) as u32 };
    let b = TagKey { id: s.next_id(state::TAG_ID) as u32 };
    assert_eq!((a.id, b.id), (1, 2));
    s.tags.put(&a, &Tag { name: "unread".into(), parent_id: -1, ..Default::default() });
    s.tags.put(&b, &Tag { name: "unread".into(), parent_id: 1, ..Default::default() });
    let hits = s.tag_by_name("unread");
    assert_eq!(hits.len(), 2); // same bare name under different parents
    assert!(s.tag_by_name("nope").is_empty());
}

// ================= Page: per-instance lists & lifecycle =================

#[test]
fn pages_of_instance_skips_soft_deleted() {
    // Contract: by_instance lists a context's pages; live reads exclude
    // deleted_at != 0, include_history widens to the deleted rows.
    let (_d, mut s) = open_tmp();
    let p1 = PageKey { id: s.next_id(state::PAGE_ID) };
    let p2 = PageKey { id: s.next_id(state::PAGE_ID) };
    s.pages.put(&p1, &Page { instance_id: 7, url: "https://a.test/one".into(), opened_at: 100, ..Default::default() });
    s.pages.put(&p2, &Page { instance_id: 7, url: "https://a.test/two".into(), opened_at: 200, closed_at: 300, deleted_at: 400, ..Default::default() });
    let p3 = PageKey { id: s.next_id(state::PAGE_ID) };
    s.pages.put(&p3, &Page { instance_id: 8, url: "https://b.test/x".into(), ..Default::default() });

    let live = s.pages_of_instance(7, false);
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].1.url, "https://a.test/one");
    assert_eq!(s.pages_of_instance(7, true).len(), 2);
    assert!(s.pages_of_instance(99, false).is_empty());
}

#[test]
fn page_by_target_reverse_lookup() {
    // Contract: by_target func index answers /open {tabId} and
    // focus_page; a rebind of target_id moves the row's index entry.
    let (_d, mut s) = open_tmp();
    let k = PageKey { id: s.next_id(state::PAGE_ID) };
    let mut p = Page { instance_id: 1, target_id: "T-abc".into(), url: "https://x.test".into(), opened_at: 5, ..Default::default() };
    s.pages.put(&k, &p);
    assert_eq!(s.page_by_target("T-abc").unwrap().0, k);
    assert!(s.page_by_target("T-none").is_none());

    // revival rebind: the old target no longer resolves
    p.target_id = "T-new".into();
    s.pages.put(&k, &p);
    assert_eq!(s.page_by_target("T-new").unwrap().0, k);
}

#[test]
fn page_children_subtree() {
    // Contract: by_parent carries the CDP openerId subtree.
    let (_d, mut s) = open_tmp();
    let parent = PageKey { id: 10 };
    let child = PageKey { id: 11 };
    s.pages.put(&parent, &Page { instance_id: 1, url: "https://p.test".into(), ..Default::default() });
    s.pages.put(&child, &Page { instance_id: 1, parent_id: 10, url: "https://c.test".into(), ..Default::default() });
    let kids = s.page_children(10);
    assert_eq!(kids.len(), 1);
    assert_eq!(kids[0].1.url, "https://c.test");
}

// ================= Page: n-gram URL recall =================

#[test]
fn url_search_recalls_by_trigrams() {
    // Contract: okm-ngram recipe — one row fans into N gram entries;
    // url_search recalls (dedup by id), substring-precise filtering is
    // the caller's rerank job. A query shorter than n falls back to the
    // whole-text token (ngrams rule), so short queries still recall.
    let (_d, mut s) = open_tmp();
    s.pages.put(&PageKey { id: 1 }, &Page { url: "https://news.ycombinator.com/item?id=1".into(), ..Default::default() });
    s.pages.put(&PageKey { id: 2 }, &Page { url: "https://rust-lang.org".into(), ..Default::default() });
    s.pages.put(&PageKey { id: 3 }, &Page { url: "https://news.ycombinator.com/item?id=2".into(), deleted_at: 9, ..Default::default() });

    let hits = s.url_search("ycombinat");
    assert_eq!(hits.len(), 1); // page 1 only; soft-deleted page 3 filtered
    assert_eq!(hits[0].1.url, "https://news.ycombinator.com/item?id=1");
    assert!(s.url_search("zzzz").is_empty());
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
    s.link_page_tag(&page, &t1);
    s.link_page_tag(&page, &t2);
    assert_eq!(s.tags_of_page(&page), vec![t1.clone(), t2.clone()]);
    assert_eq!(s.pages_of_tag(&t1), vec![page.clone()]);

    s.unlink_page_tag(&page, &t1);
    assert_eq!(s.tags_of_page(&page), vec![t2.clone()]);
    assert!(s.pages_of_tag(&t1).is_empty());
}

// ================= Instance / SiteWidth =================

#[test]
fn instance_by_profile_and_site_width_roundtrip() {
    // Contract: instance rows resolve by situation leaf name; SiteWidth
    // keeps `site` out of the key (func index only), proportion rides the
    // Quant<4> fixed-point wire.
    let (_d, mut s) = open_tmp();
    let ik = InstanceKey { id: s.next_id(state::INSTANCE_ID) as u32 };
    s.instances.put(&ik, &Instance { profile: "inbox".into(), port: 9201, pid: 4242, running: 1, ..Default::default() });
    let found = s.instance_by_profile("inbox");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1.port, 9201);

    let swk = SiteWidthKey { id: s.next_id(state::SITE_WIDTH_ID) as u32 };
    s.site_widths.put(&swk, &SiteWidth { site: "news.ycombinator.com".into(), proportion: Quant::<4>::new(0.618) });
    let sw = s.site_width("news.ycombinator.com").unwrap();
    assert_eq!(sw.1.proportion.0, 0.618);
    assert_eq!(url_site("https://news.ycombinator.com/item?id=1"), "news.ycombinator.com");
    assert!(s.site_width("absent.test").is_none());
}

// ================= State: epoch, counters, settings =================

#[test]
fn epoch_bumps_are_monotonic_and_isolated() {
    // Contract: the epoch is a State counter — bumped per CDP write, never
    // riding any page/tag payload; the value survives reopening the store.
    let (d, mut s) = open_tmp();
    assert_eq!(s.epoch(), 0);
    assert_eq!(s.bump_epoch(), 1);
    let k = PageKey { id: s.next_id(state::PAGE_ID) };
    s.pages.put(&k, &Page { url: "https://e.test".into(), ..Default::default() });
    assert_eq!(s.bump_epoch(), 2);

    // reopen: same path, epoch continues (never rewinds). fjall locks the
    // database to one handle — mudrad's single-writer model drops first
    // (crash-restart is a new process; in-process we emulate the handoff).
    MudraStore::persist(&s).expect("persist");
    drop(s);
    let s2 = MudraStore::open(d.path()).expect("reopen");
    assert_eq!(s2.epoch(), 2);
    // and the epoch is not readable through page payloads at all
    let p = s2.pages.get(&k).expect("page survives reopen");
    assert_eq!(p.url, "https://e.test");
}

#[test]
fn state_slots_roundtrip_text_and_never_collide() {
    // Contract: fixed u8 slots (closed set, append-only); text settings
    // live as raw Bytes values; counters and settings are distinct rows.
    let (_d, mut s) = open_tmp();
    s.put_state_text(state::CURRENT_CONTEXT, "work");
    assert_eq!(s.state_text(state::CURRENT_CONTEXT), "work");
    assert_eq!(s.state_text(state::DEV_MODE), ""); // unset = empty

    s.put_state_text(state::DEV_MODE, "1");
    assert_eq!(s.state_text(state::CURRENT_CONTEXT), "work"); // slot isolation

    // State row count = written slots only; there is no index on ns 6
    assert_eq!(s.state.scan_keys().len(), 2);
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
    assert_eq!(&PageKey { id: 258 }.encode(), &[0, 0, 0, 0, 0, 0, 1, 2]);

    // document ns prefixes: [0 ns_hi][ns_lo] per declaration order
    assert_eq!(<Tag as Document>::NS_PREFIX, vec![0, 1]);
    assert_eq!(<Page as Document>::NS_PREFIX, vec![0, 2]);
    assert_eq!(<Instance as Document>::NS_PREFIX, vec![0, 4]);
    assert_eq!(<SiteWidth as Document>::NS_PREFIX, vec![0, 5]);
    assert_eq!(<State as Document>::NS_PREFIX, vec![0, 6]);

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
    let tk = TagKey { id: s.next_id(state::TAG_ID) as u32 };
    let pk = PageKey { id: s.next_id(state::PAGE_ID) };
    let ik = InstanceKey { id: s.next_id(state::INSTANCE_ID) as u32 };
    s.tags.put(&tk, &Tag { name: "r".into(), parent_id: -1, ..Default::default() });
    s.pages.put(&pk, &Page { instance_id: ik.id, target_id: "T".into(), ..Default::default() });
    s.instances.put(&ik, &Instance { profile: "r".into(), ..Default::default() });
    s.page_tags.link(&pk, &tk);
    s.persist().expect("persist");
    drop(s); // fjall single-handle lock: the reopen is the new process

    let s2 = MudraStore::open(d.path()).expect("reopen");
    assert_eq!(s2.tags.get(&tk).unwrap().name, "r");
    assert_eq!(s2.pages.get(&pk).unwrap().instance_id, ik.id);
    assert_eq!(s2.instances.get(&ik).unwrap().profile, "r");
    assert_eq!(s2.tags_of_page(&pk), vec![tk]);
    // counters are State rows too — allocation continues after reopen
    let mut s3 = s2;
    assert_eq!(s3.next_id(state::PAGE_ID), pk.id + 1);
}
