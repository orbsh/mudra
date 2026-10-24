#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unwrap_in_result, clippy::panic_in_result_fn)]

//! The history interchange file: what `:export` writes and `:import` folds.
//!
//! These go through the same `interchange` functions the control endpoints call,
//! so a change in the format or in the fold semantics breaks a test instead of
//! quietly splitting the palette from the file.

use mudra_store::MudraStore;

fn open_store(dir: &std::path::Path) -> MudraStore {
    MudraStore::open(dir).expect("open store")
}

#[test]
fn export_then_import_round_trips_the_history_table() {
    let src_dir = tempfile::TempDir::new().expect("tempdir");
    let mut src = open_store(src_dir.path());
    src.history_absorb("https://example.com/", "Example", 3, 1_792_831_642_501)
        .expect("absorb");
    src.history_absorb(
        "https://doc.rust-lang.org/std/",
        "std - Rust",
        1,
        1_792_800_000_000,
    )
    .expect("absorb");

    let out_dir = tempfile::TempDir::new().expect("tempdir");
    let path = out_dir.path().join("history.ndjson");
    let written = mudrad::interchange::export(&src, &path).expect("export");
    assert_eq!(written, 2);

    let text = std::fs::read_to_string(&path).expect("read export");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    // RFC 3339 UTC, millisecond precision, keys sorted (serde's map order).
    assert!(lines.iter().all(|l| l.contains("\"last_visit\":\"20")));
    assert!(lines.iter().any(|l| l.contains(".501Z\"")));
    assert!(lines.iter().any(|l| l.contains(".000Z\"")));

    let dst_dir = tempfile::TempDir::new().expect("tempdir");
    let mut dst = open_store(dst_dir.path());
    let report = mudrad::interchange::import(&mut dst, &path).expect("import");
    assert_eq!(report.rows, 2);
    assert_eq!(report.created, 2);
    assert_eq!(report.invalid, 0);

    let mut got: Vec<(String, u64, u64)> = dst
        .history_all()
        .expect("history_all")
        .into_iter()
        .map(|h| (h.url, h.visits, h.last_at))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            (
                "https://doc.rust-lang.org/std/".to_string(),
                1,
                1_792_800_000_000
            ),
            ("https://example.com/".to_string(), 3, 1_792_831_642_501),
        ]
    );
}

#[test]
fn importing_the_same_file_twice_changes_nothing() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mut store = open_store(dir.path());
    store
        .history_absorb("https://example.com/", "Example", 5, 1_792_831_642_501)
        .expect("absorb");

    let file_dir = tempfile::TempDir::new().expect("tempdir");
    let path = file_dir.path().join("h.ndjson");
    mudrad::interchange::export(&store, &path).expect("export");

    let first = mudrad::interchange::import(&mut store, &path).expect("import");
    let second = mudrad::interchange::import(&mut store, &path).expect("import");
    assert_eq!((first.created, first.updated), (0, 1));
    assert_eq!((second.created, second.updated), (0, 1));

    let rows = store.history_all().expect("history_all");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].visits, 5, "a re-import must not double the counter");
}

#[test]
fn a_stale_file_cannot_walk_a_live_row_backwards() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mut store = open_store(dir.path());
    store
        .history_absorb("https://example.com/", "Example", 9, 1_792_831_642_501)
        .expect("absorb");

    let file_dir = tempfile::TempDir::new().expect("tempdir");
    let path = file_dir.path().join("old.ndjson");
    // 2 visits, a year earlier: the import folds with max(), so nothing moves.
    std::fs::write(
        &path,
        "{\"url\":\"https://example.com/\",\"title\":\"Older\",\"visits\":2,\"last_visit\":\"2025-10-24T09:27:42.501Z\"}\n",
    )
    .expect("write fixture");
    let report = mudrad::interchange::import(&mut store, &path).expect("import");
    assert_eq!(report.updated, 1);

    let rows = store.history_all().expect("history_all");
    assert_eq!(rows[0].visits, 9);
    assert_eq!(rows[0].last_at, 1_792_831_642_501);
}

#[test]
fn broken_lines_are_counted_not_fatal() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mut store = open_store(dir.path());
    let file_dir = tempfile::TempDir::new().expect("tempdir");
    let path = file_dir.path().join("mixed.ndjson");
    std::fs::write(
        &path,
        concat!(
            "# produced by hand\n",
            "\n",
            "not json at all\n",
            "{\"url\":\"\",\"last_visit\":\"2026-10-24T09:27:42.501Z\"}\n",
            "{\"url\":\"https://no-time.example/\",\"visits\":2}\n",
            "{\"url\":\"https://local-offset.example/\",\"last_visit\":\"2026-10-24T09:27:42+08:00\"}\n",
            "{\"url\":\"https://good.example/\",\"title\":\"Good\",\"last_visit\":\"2026-10-24T09:27:42.501Z\"}\n",
        ),
    )
    .expect("write fixture");

    let report = mudrad::interchange::import(&mut store, &path).expect("import");
    assert_eq!(report.rows, 1);
    assert_eq!(report.created, 1);
    assert_eq!(
        report.invalid, 4,
        "bad json, no url, no time, non-UTC offset"
    );

    let rows = store.history_all().expect("history_all");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].url, "https://good.example/");
    assert_eq!(rows[0].visits, 1, "a missing counter defaults to one visit");
}

#[test]
fn an_empty_argument_names_the_default_file_under_the_data_dir() {
    let path = mudrad::interchange::resolve_path("  ").expect("resolve");
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some(mudrad::interchange::DEFAULT_FILE)
    );
    // `~` and a relative path both anchor at home, never at the daemon's cwd.
    let home = std::env::var("HOME").expect("HOME");
    assert_eq!(
        mudrad::interchange::resolve_path("h.ndjson").expect("resolve"),
        std::path::PathBuf::from(&home).join("h.ndjson")
    );
    assert_eq!(
        mudrad::interchange::resolve_path("~/sub/h.ndjson").expect("resolve"),
        std::path::PathBuf::from(&home).join("sub/h.ndjson")
    );
    assert_eq!(
        mudrad::interchange::resolve_path("/tmp/x.ndjson").expect("resolve"),
        std::path::PathBuf::from("/tmp/x.ndjson")
    );
}
