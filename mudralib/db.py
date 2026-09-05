"""mudra 浏览器页管理 sqlite 存储层.

schema（tag-forest 模型，session 已废弃）:
instances(1 isolated tag ↔ 1 chromium 实例), pages(直接挂实例), tag, page_tag, site_widths, state.
"""

from __future__ import annotations

import pathlib
import sqlite3
import time

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


# ---- 页面写路径（业务 SQL 唯一所在地；mudrad/_sync 等只做事件→调用） ----

def instance_set_running(conn: sqlite3.Connection, inst_id: int, running: int) -> None:
    conn.execute("UPDATE instances SET running=? WHERE id=?", (running, inst_id))


def pages_close_all(conn: sqlite3.Connection, inst_id: int, ts: int) -> None:
    conn.execute(
        "UPDATE pages SET closed_at=? WHERE instance_id=? AND closed_at IS NULL",
        (ts, inst_id),
    )


def page_close_target(conn: sqlite3.Connection, inst_id: int, target_id: str, ts: int) -> None:
    conn.execute(
        "UPDATE pages SET closed_at=? WHERE instance_id=? AND target_id=?"
        " AND closed_at IS NULL",
        (ts, inst_id, target_id),
    )


def page_next_position(conn: sqlite3.Connection, inst_id: int) -> int:
    return conn.execute(
        "SELECT COALESCE(MAX(position),-1)+1 AS p FROM pages WHERE instance_id=?",
        (inst_id,),
    ).fetchone()["p"]


def page_id_by_target(conn: sqlite3.Connection, inst_id: int, target_id: str) -> int | None:
    r = conn.execute(
        "SELECT id FROM pages WHERE instance_id=? AND target_id=?",
        (inst_id, target_id),
    ).fetchone()
    return r["id"] if r else None


def page_upsert_by_target(
    conn: sqlite3.Connection, inst_id: int, target_id: str, url: str, title: str
) -> int:
    """同步 CDP targetInfo → pages 行（存在则刷新并复活，不存在则落新行），返回 page id。"""
    row = conn.execute(
        "SELECT id FROM pages WHERE instance_id=? AND target_id=?",
        (inst_id, target_id),
    ).fetchone()
    if row:
        conn.execute(
            "UPDATE pages SET url=?, title=?, closed_at=NULL WHERE id=?",
            (url, title, row["id"]),
        )
        return row["id"]
    conn.execute(
        "INSERT OR IGNORE INTO pages"
        "(instance_id,target_id,url,title,position,opened_at)"
        " VALUES(?,?,?,?,?,?)",
        (inst_id, target_id, url, title, page_next_position(conn, inst_id),
         int(time.time())),
    )
    new_id = page_id_by_target(conn, inst_id, target_id)
    if new_id is None:
        raise RuntimeError(f"page upsert failed: inst={inst_id} target={target_id}")
    return new_id


def page_set_parent_once(
    conn: sqlite3.Connection, child_id: int, parent_id: int
) -> None:
    """回填父子关系（仅首次设置，不覆盖人工值）。"""
    conn.execute(
        "UPDATE pages SET parent_id=? WHERE id=? AND parent_id IS NULL",
        (parent_id, child_id),
    )


def page_by_id_joined(conn: sqlite3.Connection, page_id: int) -> sqlite3.Row | None:
    """pages 行 + 所属实例 port/pid（focus 等动作的目标解析）。"""
    return conn.execute(
        "SELECT p.id, i.port, i.pid, p.target_id, p.url, p.title FROM pages p"
        " JOIN instances i ON i.id=p.instance_id"
        " WHERE p.id=? AND p.closed_at IS NULL", (page_id,)
    ).fetchone()


def instance_ctx(conn: sqlite3.Connection, inst_id: int) -> str | None:
    r = conn.execute(
        "SELECT profile FROM instances WHERE id=?", (inst_id,)
    ).fetchone()
    return r["profile"] if r else None


def running_instances(conn: sqlite3.Connection) -> list[sqlite3.Row]:
    return conn.execute("SELECT id, port, profile FROM instances WHERE running=1").fetchall()


def page_ctx_for_url(conn: sqlite3.Connection, url_prefix: str) -> str | None:
    """按 URL 匹配打开中的 page → 实例唯一时返回其 ctx。"""
    row = conn.execute(
        "SELECT i.profile FROM pages p JOIN instances i ON i.id=p.instance_id"
        " WHERE p.closed_at IS NULL AND p.url LIKE ?"
        " GROUP BY i.profile HAVING COUNT(DISTINCT i.id)=1",
        (url_prefix,),
    ).fetchone()
    return row["profile"] if row else None


def latest_open_page_by_url(
    conn: sqlite3.Connection, inst_id: int, url_prefix: str
) -> sqlite3.Row | None:
    return conn.execute(
        "SELECT id FROM pages WHERE instance_id=? AND url LIKE ?"
        " AND closed_at IS NULL ORDER BY id DESC LIMIT 1",
        (inst_id, url_prefix),
    ).fetchone()


def page_tag_names(conn: sqlite3.Connection, page_id: int) -> list[str]:
    return [
        r["name"] for r in conn.execute(
            "SELECT t.name FROM page_tag pt JOIN tag t ON t.id=pt.tag_id"
            " WHERE pt.page_id=?",
            (page_id,),
        )
    ]


def site_width(conn: sqlite3.Connection, domain: str) -> sqlite3.Row | None:
    """站点列宽记忆读取。"""
    return conn.execute(
        "SELECT proportion FROM site_widths WHERE site=?", (domain,)
    ).fetchone()


def page_open_by_url_substring(
    conn: sqlite3.Connection, inst_id: int, query: str
) -> sqlite3.Row | None:
    """实例内按 URL 子串找打开中的页（close page 用）。"""
    return conn.execute(
        "SELECT id,position,url,target_id FROM pages"
        " WHERE instance_id=? AND closed_at IS NULL AND url LIKE ?",
        (inst_id, f"%{query}%"),
    ).fetchone()


def instance_launch_started(
    conn: sqlite3.Connection, inst_id: int | None, ctx: str,
    port: int, pid: int, proxy: str | None, ext: str | None,
) -> None:
    """实例拉起落库：沿用旧行则更新端口/pid 并标 running，否则新建行。"""
    if inst_id:
        conn.execute(
            "UPDATE instances SET port=?,pid=?,running=1 WHERE id=?",
            (port, pid, inst_id),
        )
    else:
        conn.execute(
            "INSERT INTO instances(profile,port,pid,running,proxy,extensions)"
            " VALUES(?,?,?,1,?,?)",
            (ctx, port, pid, proxy, ext),
        )
    conn.commit()


# ---- tag 森林 ----

def tag_id_by_name(conn: sqlite3.Connection, name: str) -> int | None:
    r = conn.execute(
        "SELECT id FROM tag WHERE name=? AND deleted=0", (name,)
    ).fetchone()
    return r["id"] if r else None


def page_tag_toggle(conn: sqlite3.Connection, page_id: int, tag_id: int) -> str:
    """页↔tag toggle。返回 'added' | 'removed'。"""
    existing = conn.execute(
        "SELECT 1 FROM page_tag WHERE page_id=? AND tag_id=?", (page_id, tag_id)
    ).fetchone()
    if existing:
        conn.execute(
            "DELETE FROM page_tag WHERE page_id=? AND tag_id=?", (page_id, tag_id)
        )
        return "removed"
    conn.execute(
        "INSERT OR IGNORE INTO page_tag(page_id, tag_id) VALUES(?,?)",
        (page_id, tag_id),
    )
    return "added"


def tag_children(conn: sqlite3.Connection, parent: str | None) -> list[sqlite3.Row]:
    """父名下子 tag（parent=None → 根层）。"""
    if parent:
        return conn.execute(
            "SELECT t.name, t.rank FROM tag t WHERE t.parent_id ="
            " (SELECT id FROM tag WHERE name=? AND deleted=0) AND t.deleted=0"
            " ORDER BY t.rank IS NULL, t.rank, t.name",
            (parent,),
        ).fetchall()
    return conn.execute(
        "SELECT name, rank FROM tag WHERE parent_id=-1 AND deleted=0"
        " ORDER BY name"
    ).fetchall()


def pages_open(conn: sqlite3.Connection, ctx: str | None = None) -> list[sqlite3.Row]:
    """打开页列表（跨 ctx；ctx 给定时只看该上下文实例）。"""
    if ctx:
        return conn.execute(
            "SELECT p.id, p.title, p.url, i.profile AS ctx FROM pages p"
            " JOIN instances i ON i.id=p.instance_id"
            " WHERE p.closed_at IS NULL AND i.profile=? ORDER BY p.id",
            (ctx,),
        ).fetchall()
    return conn.execute(
        "SELECT p.id, p.title, p.url, i.profile AS ctx FROM pages p"
        " JOIN instances i ON i.id=p.instance_id"
        " WHERE p.closed_at IS NULL ORDER BY p.id",
    ).fetchall()
