"""mudra 浏览器会话管理 sqlite 存储层.

schema 见 PLAN.md §4:
instances(1 session ↔ 1 chromium 实例), sessions, pages, site_widths.
"""

from __future__ import annotations

import pathlib
import sqlite3

DB = pathlib.Path.home() / ".local" / "share" / "mudra" / "mudra.sqlite"

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
    closed_at  INTEGER,
    parent_id  INTEGER REFERENCES pages(id)   -- 子页：由谁(target)打开（CDP openerId）
);

CREATE TABLE IF NOT EXISTS site_widths(
    site       TEXT PRIMARY KEY,
    proportion REAL
);
CREATE TABLE IF NOT EXISTS tag(
    id         INTEGER PRIMARY KEY,
    parent_id  INTEGER,                      -- -1 = 根哨兵（无真实父），见 tag-forest
    name       TEXT NOT NULL,
    alias      TEXT,
    isolated   INTEGER NOT NULL DEFAULT 0,   -- 命中 → 独立实例/工作区
    required   INTEGER NOT NULL DEFAULT 0,   -- 树内必选（如 situation）
    rank       INTEGER,                      -- 同父内排序（评分树叶 ☆..☆☆☆☆☆）
    hidden     INTEGER NOT NULL DEFAULT 0,
    note       TEXT,
    deleted    INTEGER NOT NULL DEFAULT 0,   -- 软删
    created    INTEGER,
    updated    INTEGER,
    UNIQUE(parent_id, name)
);
CREATE TABLE IF NOT EXISTS page_tag(
    page_id  INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    tag_id   INTEGER NOT NULL REFERENCES tag(id),
    PRIMARY KEY(page_id, tag_id)
);

CREATE TABLE IF NOT EXISTS state(
    key   TEXT PRIMARY KEY,
    value TEXT
);
"""


def connect() -> sqlite3.Connection:
    DB.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(DB)
    conn.row_factory = sqlite3.Row
    conn.executescript(_SCHEMA)
    # 原型阶段不做 ALTER 迁移：schema 变更删 mudra.sqlite 重建（见 PLAN §4 迁移策略）
    # 迁移：去重 pages(session_id,target_id) 并加唯一约束（防御多 daemon/并发竞态重复插入）
    conn.execute(
        "DELETE FROM pages WHERE id NOT IN"
        " (SELECT MIN(id) FROM pages GROUP BY session_id, target_id)"
    )
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_pages_session_target"
        " ON pages(session_id, target_id)"
    )
    conn.commit()
    return conn


def get_state(conn: sqlite3.Connection, key: str) -> str | None:
    r = conn.execute("SELECT value FROM state WHERE key=?", (key,)).fetchone()
    return r["value"] if r else None


def set_state(conn: sqlite3.Connection, key: str, value: str) -> None:
    conn.execute(
        "INSERT INTO state(key,value) VALUES(?,?)"
        " ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        (key, value),
    )