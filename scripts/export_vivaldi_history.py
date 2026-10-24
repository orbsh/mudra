#!/usr/bin/env python3
"""Export Vivaldi's browsing history as mudra's history interchange format.

The interchange format is defined by mudra (see docs/HISTORY-INTERCHANGE.md);
this script is an adapter onto it, not the other way round. One NDJSON record
per line:

    {"url": "…", "title": "…", "visits": 12, "last_visit": "2026-10-24T09:27:42.501Z"}

`last_visit` is RFC 3339 UTC with millisecond precision. Vivaldi keeps
`last_visit_time` as microseconds since 1601-01-01, converted here.

Why a copy instead of reading the live DB: Vivaldi keeps `History` open with a
WAL, and a read-only immutable open silently drops the newest visits. Copying
the three files under the locks (sqlite only needs to replay the WAL) gives a
consistent snapshot without asking the browser to stop.

Usage:
    export_vivaldi_history.py                       # newest profile, NDJSON to stdout
    export_vivaldi_history.py --out history.ndjson
    export_vivaldi_history.py --min-visits 2 --since 2026-01-01
    export_vivaldi_history.py --profile Default --all-profiles
"""
from __future__ import annotations

import argparse
import json
import shutil
import sqlite3
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

VIVALDI_ROOT = Path.home() / ".config" / "vivaldi"
# Chromium wall clock: microseconds since 1601-01-01. 11644473600 s separate
# that epoch from the Unix epoch.
CHROMIUM_EPOCH_OFFSET_MS = 11644473600000
SKIP_SCHEMES = ("chrome://", "vivaldi://", "devtools://", "chrome-extension://", "about:")


def rfc3339(chromium_us: int) -> str:
    """Chromium µs-since-1601 → RFC 3339 UTC with milliseconds."""
    ms = chromium_us // 1000 - CHROMIUM_EPOCH_OFFSET_MS
    dt = datetime.fromtimestamp(ms / 1000, tz=timezone.utc)
    return dt.strftime("%Y-%m-%dT%H:%M:%S.") + f"{ms % 1000:03d}Z"


def profile_dirs() -> list[Path]:
    """Every Vivaldi profile that has a History DB, newest first."""
    dirs = [d for d in VIVALDI_ROOT.iterdir() if (d / "History").is_file()] if VIVALDI_ROOT.is_dir() else []
    return sorted(dirs, key=lambda d: (d / "History").stat().st_mtime, reverse=True)


def snapshot(history: Path) -> Path:
    """Copy History (+ -wal/-shm) into a temp dir so SQLite sees a stable file."""
    tmp = Path(tempfile.mkdtemp(prefix="vivaldi-history-"))
    for suffix in ("", "-wal", "-shm"):
        src = history.with_name(history.name + suffix)
        if src.exists():
            shutil.copy2(src, tmp / (history.name + suffix))
    return tmp


def rows(db: Path, min_visits: int, since_ms: int, limit: int | None):
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    sql = (
        "SELECT url, title, visit_count, last_visit_time FROM urls "
        "WHERE hidden = 0 AND url <> '' AND last_visit_time > 0"
    )
    args: list[object] = []
    if min_visits > 1:
        sql += " AND visit_count >= ?"
        args.append(min_visits)
    if since_ms:
        sql += " AND last_visit_time >= ?"
        args.append((since_ms + CHROMIUM_EPOCH_OFFSET_MS) * 1000)
    sql += " ORDER BY last_visit_time DESC"
    if limit:
        sql += f" LIMIT {int(limit)}"
    for url, title, visits, last in conn.execute(sql, args):
        if url.startswith(SKIP_SCHEMES):
            continue
        yield {
            "url": url,
            "title": title or "",
            "visits": visits or 1,
            "last_visit": rfc3339(last),
        }
    conn.close()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--profile", help="profile dir name (default: the newest profile with a History DB)")
    ap.add_argument("--all-profiles", action="store_true", help="export every profile, not just the newest")
    ap.add_argument("--out", help="write here instead of stdout")
    ap.add_argument("--min-visits", type=int, default=1, help="drop URLs visited fewer times (default 1)")
    ap.add_argument("--since", help="keep visits on/after this date (YYYY-MM-DD)")
    ap.add_argument("--limit", type=int, help="stop after N records")
    args = ap.parse_args()

    since_ms = 0
    if args.since:
        since_ms = int(time.mktime(time.strptime(args.since, "%Y-%m-%d")) * 1000)

    profiles = profile_dirs()
    if args.profile:
        profiles = [p for p in profiles if p.name == args.profile]
        if not profiles:
            print(f"no History DB in profile {args.profile!r}", file=sys.stderr)
            return 2
    if not profiles:
        print(f"no Vivaldi profiles under {VIVALDI_ROOT}", file=sys.stderr)
        return 2
    if not args.all_profiles and not args.profile:
        profiles = profiles[:1]

    sink = open(args.out, "w", encoding="utf-8") if args.out else sys.stdout
    kept = 0
    try:
        for prof in profiles:
            snap = snapshot(prof / "History")
            prof_kept = 0
            try:
                for rec in rows(snap / "History", args.min_visits, since_ms, args.limit):
                    sink.write(json.dumps(rec, ensure_ascii=False) + "\n")
                    kept += 1
                    prof_kept += 1
                    if args.limit and kept >= args.limit:
                        break
                print(f"{prof.name}: {prof_kept} records", file=sys.stderr)
            finally:
                shutil.rmtree(snap, ignore_errors=True)
            if args.limit and kept >= args.limit:
                break
    finally:
        if args.out:
            sink.close()
    print(f"exported {kept} records from {len(profiles)} profile(s)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())