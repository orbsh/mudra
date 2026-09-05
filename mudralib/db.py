"""mudra 浏览器页管理 sqlite 存储层.

schema（tag-forest 模型，session 已废弃）:
instances(1 isolated tag ↔ 1 chromium 实例), pages(直接挂实例), tag, page_tag, site_widths, state.
"""

from __future__ import annotations

import pathlib
import sqlite3

DB = pathlib.Path.home() / ".local" / "share" / "mudra" / "mudra.sqlite"

_SCHEMA = """
CREATE TABLE IF NOT EXISTS instances(
    id         INTEGER PRIMARY KEY,
    profile    TEXT,          -- situation 叶名（inbox/work/personal/privacy）
    port       INTEGER,
    pid        INTEGER,
    running    INTEGER NOT NULL DEFAULT 0,
    proxy      TEXT,
    extensions TEXT
);

CREATE TABLE IF NOT EXISTS pages(
    id         INTEGER PRIMARY KEY,
    instance_id INTEGER NOT NULL REFERENCES instances(id) ON DELETE CASCADE,
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

DEFAULT_CONTEXT = "inbox"


def connect() -> sqlite3.Connection:
    DB.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(DB)
    conn.row_factory = sqlite3.Row
    conn.executescript(_SCHEMA)
    # 原型阶段不做 ALTER 迁移：schema 变更删 mudra.sqlite 重建（见 PLAN §4 迁移策略）
    # 去重 pages(instance_id,target_id) 并加唯一约束（防御多 daemon/并发竞态重复插入）
    conn.execute(
        "DELETE FROM pages WHERE id NOT IN"
        " (SELECT MIN(id) FROM pages GROUP BY instance_id, target_id)"
    )
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_pages_instance_target"
        " ON pages(instance_id, target_id)"
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
    conn.commit()


def current_context(conn: sqlite3.Connection) -> str:
    """当前 situation 叶名（树内单选的值）。"""
    return get_state(conn, "current_context") or DEFAULT_CONTEXT


def set_context(conn: sqlite3.Connection, name: str) -> bool:
    """切上下文；必须是 situation 树的叶。"""
    r = conn.execute(
        "SELECT t.id FROM tag t WHERE t.name=? AND t.parent_id="
        " (SELECT id FROM tag WHERE parent_id=-1 AND name='situation')",
        (name,),
    ).fetchone()
    if not r:
        return False
    set_state(conn, "current_context", name)
    return True


def instance_for_context(conn: sqlite3.Connection, ctx: str | None = None) -> dict | None:
    """上下文（situation 叶）对应的实例行。"""
    ctx = ctx or current_context(conn)
    return conn.execute(
        "SELECT * FROM instances WHERE profile=? ORDER BY id DESC LIMIT 1", (ctx,)
    ).fetchone()
