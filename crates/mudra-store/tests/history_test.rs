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
