#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_in_result,
    clippy::panic_in_result_fn
)]
//! History writes: which row a url lands on.
//!
//! `by_url` is a prefix index, so a query for `https://x/` also matches every
//! longer url that starts with it. Both writers (the open verb's bump and the
//! interchange fold) must land on the exact row — taking the first hit merged
//! unrelated urls, which at import scale silently dropped thousands of rows.

use mudra_store::MudraStore;

fn store(dir: &std::path::Path) -> MudraStore {
    MudraStore::open(dir).expect("open store")
}

#[test]
fn bump_does_not_fold_a_url_into_a_longer_one_with_the_same_prefix() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut s = store(dir.path());
    s.history_bump("https://example.com/a/deep", "Deep", 1_000)
        .unwrap();
    s.history_bump("https://example.com/", "Root", 2_000)
        .unwrap();

    let rows = s.history_all().unwrap();
    assert_eq!(rows.len(), 2, "the short url must get its own row");
    let root = rows
        .iter()
        .find(|h| h.url == "https://example.com/")
        .unwrap();
    assert_eq!(root.visits, 1);
    assert_eq!(root.last_at, 2_000);
    let deep = rows
        .iter()
        .find(|h| h.url == "https://example.com/a/deep")
        .unwrap();
    assert_eq!(deep.visits, 1);
    assert_eq!(deep.last_at, 1_000);
}

#[test]
fn a_second_bump_of_the_same_url_still_increments_one_row() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut s = store(dir.path());
    s.history_bump("https://example.com/", "Root", 1_000)
        .unwrap();
    s.history_bump("https://example.com/", "Root again", 2_000)
        .unwrap();
    s.history_bump("https://example.com/longer", "Longer", 3_000)
        .unwrap();

    let rows = s.history_all().unwrap();
    assert_eq!(rows.len(), 2);
    let root = rows
        .iter()
        .find(|h| h.url == "https://example.com/")
        .unwrap();
    assert_eq!(root.visits, 2);
    assert_eq!(root.title, "Root again");
}

#[test]
fn absorb_keeps_prefix_sharing_urls_apart() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut s = store(dir.path());
    s.history_absorb("https://example.com/", "Root", 3, 1_000)
        .unwrap();
    s.history_absorb("https://example.com/a", "A", 7, 2_000)
        .unwrap();
    s.history_absorb("https://example.com/a/b", "AB", 11, 3_000)
        .unwrap();

    let mut rows: Vec<(String, u64, u64)> = s
        .history_all()
        .unwrap()
        .into_iter()
        .map(|h| (h.url, h.visits, h.last_at))
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        vec![
            ("https://example.com/".to_string(), 3, 1_000),
            ("https://example.com/a".to_string(), 7, 2_000),
            ("https://example.com/a/b".to_string(), 11, 3_000),
        ]
    );
}

// ================= the resident corpus =================

/// Contract: the ranked read serves the corpus, so a write that lands after
/// the first read must show up in the very next answer — the corpus is kept in
/// step by the write path, not reloaded by the read.
#[test]
fn the_corpus_sees_a_write_that_lands_after_it_was_built() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut s = store(dir.path());
    s.history_bump("https://a.test/one", "One", 100).unwrap();
    // first read builds the corpus
    let hits = s.history_search("a.test", 10).unwrap();
    assert_eq!(hits.len(), 1);

    // a later write must be visible without a rebuild
    s.history_bump("https://b.test/two", "Two", 200).unwrap();
    let hits = s.history_search("b.test", 10).unwrap();
    assert_eq!(hits.len(), 1, "the new row is in the corpus");
    assert_eq!(hits[0].url, "https://b.test/two");

    // and a bump of an existing row updates the resident copy (title + visits)
    s.history_bump("https://a.test/one", "One New", 300).unwrap();
    let hits = s.history_search("one new", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, "One New");
    assert_eq!(hits[0].visits, 2);
}

/// Contract: the corpus and the table answer the same row set. The corpus is a
/// derived view, so the two drifting apart is a bug with no other symptom.
#[test]
fn corpus_and_table_agree_on_every_row() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut s = store(dir.path());
    for (url, title, visits, ts) in [
        ("https://a.test/one", "One", 1, 100),
        ("https://a.test/two", "Two", 2, 200),
        ("https://b.test/", "Bee", 3, 300),
    ] {
        s.history_absorb(url, title, visits, ts).unwrap();
    }
    let mut table: Vec<(String, u64, u64)> = s
        .history_all()
        .unwrap()
        .into_iter()
        .map(|h| (h.url, h.visits, h.last_at))
        .collect();
    let mut corpus: Vec<(String, u64, u64)> = s
        .history_search("", 100)
        .unwrap()
        .into_iter()
        .map(|h| (h.url, h.visits, h.last_at))
        .collect();
    table.sort();
    corpus.sort();
    assert_eq!(corpus, table);
}

/// Contract: the empty query answers recency, and a frequent-but-stale row
/// keeps its lower place — recency is not visits with a tiebreak.
#[test]
fn the_empty_query_is_recency_not_visits() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut s = store(dir.path());
    for _ in 0..9 {
        s.history_bump("https://often.test/", "Often", 100).unwrap();
    }
    s.history_bump("https://latest.test/", "Latest", 900).unwrap();
    let hits = s.history_search("", 10).unwrap();
    assert_eq!(hits[0].url, "https://latest.test/");
    assert_eq!(hits[1].url, "https://often.test/");
    // and it is not a search: a row no query would recall still leads here
    assert_eq!(s.history_search("", 1).unwrap().len(), 1);
}
