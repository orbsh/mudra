"""qw 浏览器会话管理 sqlite 存储层.

schema 见 PLAN.md §4:
instances(1 session ↔ 1 chromium 实例), sessions, pages, site_widths.
"""

from __future__ import annotations

import pathlib
import sqlite3

DB = pathlib.Path.home() / ".local" / "share" / "qw" / "qw.sqlite"

_SCHEMA = """
CREATE TABLE IF NOT EXISTS instances(
    id         INTEGER PRIMARY KEY,
    profile    TEXT,
    port       INTEGER,
    pid        INTEGER,
    running    INTEGER NOT NULL DEFAULT 0,
    proxy      TEXT,
    extensions TEXT
);

CREATE TABLE IF NOT EXISTS sessions(
    id             INTEGER PRIMARY KEY,
    name           TEXT UNIQUE NOT NULL,
    workspace      TEXT,
    instance_id    INTEGER REFERENCES instances(id),
    created_at     INTEGER,
    last_opened_at INTEGER
);

CREATE TABLE IF NOT EXISTS pages(
    id         INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    target_id  TEXT,
    url        TEXT,
    title      TEXT,
    position   INTEGER,
    opened_at  INTEGER,
    closed_at  INTEGER
);

CREATE TABLE IF NOT EXISTS site_widths(
    site       TEXT PRIMARY KEY,
    proportion REAL
);
"""


def connect() -> sqlite3.Connection:
    DB.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(DB)
    conn.row_factory = sqlite3.Row
    conn.executescript(_SCHEMA)
    conn.commit()
    return conn