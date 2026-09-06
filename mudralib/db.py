"""mudra browser-page management sqlite storage layer.

schema (tag-forest model; the session concept is deprecated):
instances (1 isolated tag <-> 1 chromium instance), pages (attached directly to an instance), tag, page_tag, site_widths, state.
"""

from __future__ import annotations

import pathlib
import sqlite3
import time

DB = pathlib.Path.home() / ".local" / "share" / "mudra" / "mudra.sqlite"

_SCHEMA = """
CREATE TABLE IF NOT EXISTS instances(
    id         INTEGER PRIMARY KEY,
    profile    TEXT,          -- situation leaf name (inbox/work/personal/privacy)
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
    deleted_at INTEGER,                                        -- soft delete (only closed pages can be deleted)
    parent_id  INTEGER REFERENCES pages(id)   -- child page: which target opened it (CDP openerId)
);

CREATE TABLE IF NOT EXISTS site_widths(
    site       TEXT PRIMARY KEY,
    proportion REAL
);
CREATE TABLE IF NOT EXISTS tag(
    id         INTEGER PRIMARY KEY,
    parent_id  INTEGER,                      -- -1 = root sentinel (no real parent), see tag-forest
    name       TEXT NOT NULL,
    alias      TEXT,
    isolated   INTEGER NOT NULL DEFAULT 0,   -- when matched -> isolated instance/workspace
    required   INTEGER NOT NULL DEFAULT 0,   -- mandatory within the tree (e.g. situation)
    rank       INTEGER,                      -- ordering within the same parent (rating-tree leaves ☆..☆☆☆☆☆)
    hidden     INTEGER NOT NULL DEFAULT 0,
    note       TEXT,
    deleted    INTEGER NOT NULL DEFAULT 0,   -- soft delete
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
    # no ALTER migrations in the prototype stage: on schema change, delete mudra.sqlite and rebuild (see PLAN §4 migration strategy)
    # dedupe pages(instance_id,target_id) and add a unique constraint (guards against duplicate inserts from multi-daemon/concurrency races)
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
    """The current situation leaf name (the single-select value within the tree)."""
    return get_state(conn, "current_context") or DEFAULT_CONTEXT


def set_context(conn: sqlite3.Connection, name: str) -> bool:
    """Switch context; must be a leaf of the situation tree."""
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
    """The instance row for a context (situation leaf)."""
    ctx = ctx or current_context(conn)
    return conn.execute(
        "SELECT * FROM instances WHERE profile=? ORDER BY id DESC LIMIT 1", (ctx,)
    ).fetchone()


# ---- page write paths (the single home for business SQL; mudrad/_sync etc. only map events -> calls) ----

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
    """Sync CDP targetInfo -> pages row (refresh & revive if it exists, insert a new row otherwise); returns the page id."""
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
    # reopening a window = new targetId. First look for a closed page of the same
    # instance with the same URL (most recent first); on a hit, take over that row
    # (rebind target_id and revive it) instead of inserting a new row.
    closed = conn.execute(
        "SELECT id FROM pages WHERE instance_id=? AND target_id IS NOT NULL"
        " AND closed_at IS NOT NULL AND deleted_at IS NULL AND url=?"
        " ORDER BY closed_at DESC LIMIT 1",
        (inst_id, url),
    ).fetchone()
    if closed:
        conn.execute(
            "UPDATE pages SET target_id=?, title=?, closed_at=NULL, opened_at=?"
            " WHERE id=?",
            (target_id, title, int(time.time()), closed["id"]),
        )
        return closed["id"]
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
    """Backfill parent-child (set only the first time; never overwrites manual values)."""
    conn.execute(
        "UPDATE pages SET parent_id=? WHERE id=? AND parent_id IS NULL",
        (parent_id, child_id),
    )


def page_by_id_joined(conn: sqlite3.Connection, page_id: int) -> sqlite3.Row | None:
    """pages row + owning instance port/pid (target resolution for focus and similar actions)."""
    return conn.execute(
        "SELECT p.id, i.port, i.pid, p.target_id, p.url, p.title FROM pages p"
        " JOIN instances i ON i.id=p.instance_id"
        " WHERE p.id=? AND p.closed_at IS NULL AND p.deleted_at IS NULL", (page_id,)
    ).fetchone()


def instance_ctx(conn: sqlite3.Connection, inst_id: int) -> str | None:
    r = conn.execute(
        "SELECT profile FROM instances WHERE id=?", (inst_id,)
    ).fetchone()
    return r["profile"] if r else None


def running_instances(conn: sqlite3.Connection) -> list[sqlite3.Row]:
    return conn.execute("SELECT id, port, profile FROM instances WHERE running=1").fetchall()


def page_ctx_for_url(conn: sqlite3.Connection, url_prefix: str) -> str | None:
    """Match an open page by URL -> return its ctx when the instance is unambiguous."""
    row = conn.execute(
        "SELECT i.profile FROM pages p JOIN instances i ON i.id=p.instance_id"
        " WHERE p.closed_at IS NULL AND p.deleted_at IS NULL AND p.url LIKE ?"
        " GROUP BY i.profile HAVING COUNT(DISTINCT i.id)=1",
        (url_prefix,),
    ).fetchone()
    return row["profile"] if row else None


def latest_open_page_by_url(
    conn: sqlite3.Connection, inst_id: int, url_prefix: str
) -> sqlite3.Row | None:
    return conn.execute(
        "SELECT id FROM pages WHERE instance_id=? AND url LIKE ?"
        " AND closed_at IS NULL AND deleted_at IS NULL"
        " ORDER BY id DESC LIMIT 1",
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


def page_tag_paths(conn: sqlite3.Connection, page_id: int) -> list[str]:
    """Full paths of a page's tags (in "state::to-read" form), for capsule rendering.

    When a tag has no parent (a root-level orphan tag), the path is just its name.
    """
    rows = conn.execute(
        "SELECT t.name, t.parent_id FROM page_tag pt JOIN tag t ON t.id=pt.tag_id"
        " WHERE pt.page_id=?",
        (page_id,),
    ).fetchall()
    out: list[str] = []
    for r in rows:
        parts = [r["name"]]
        pid = r["parent_id"]
        while pid and pid != -1:
            p = conn.execute(
                "SELECT name, parent_id FROM tag WHERE id=?", (pid,)
            ).fetchone()
            if not p:
                break
            parts.append(p["name"])
            pid = p["parent_id"]
        out.append("::".join(reversed(parts)))
    return out


def page_soft_delete(conn: sqlite3.Connection, page_id: int, ts: int) -> None:
    """Soft-delete a page (allowed only for closed pages; the caller validates)."""
    conn.execute("UPDATE pages SET deleted_at=? WHERE id=?", (ts, page_id))


def page_closed(conn: sqlite3.Connection, page_id: int) -> bool:
    return conn.execute(
        "SELECT 1 FROM pages WHERE id=? AND closed_at IS NOT NULL", (page_id,)
    ).fetchone() is not None


def site_width(conn: sqlite3.Connection, domain: str) -> sqlite3.Row | None:
    return conn.execute(
        "SELECT proportion FROM site_widths WHERE site=?", (domain,)
    ).fetchone()


def page_open_by_url_substring(
    conn: sqlite3.Connection, inst_id: int, query: str
) -> sqlite3.Row | None:
    """Find an open page by URL substring within an instance (used by close page)."""
    return conn.execute(
        "SELECT id,position,url,target_id FROM pages"
        " WHERE instance_id=? AND closed_at IS NULL AND deleted_at IS NULL"
        " AND url LIKE ?",
        (inst_id, f"%{query}%"),
    ).fetchone()


def instance_launch_started(
    conn: sqlite3.Connection, inst_id: int | None, ctx: str,
    port: int, pid: int, proxy: str | None, ext: str | None,
) -> None:
    """Record an instance launch: if reusing an old row, update port/pid and mark running; otherwise create a new row."""
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


# ---- tag forest ----

def tag_id_by_name(conn: sqlite3.Connection, name: str) -> int | None:
    r = conn.execute(
        "SELECT id FROM tag WHERE name=? AND deleted=0", (name,)
    ).fetchone()
    return r["id"] if r else None


def page_tag_toggle(conn: sqlite3.Connection, page_id: int, tag_id: int) -> str:
    """Toggle a tag on a page. Returns 'added' | 'removed'."""
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
    """Child tags under a parent (parent=None -> root layer)."""
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
    """Open-page list (across contexts; when ctx is given, only that context's instance is considered)."""
    if ctx:
        return conn.execute(
            "SELECT p.id, p.title, p.url, i.profile AS ctx FROM pages p"
            " JOIN instances i ON i.id=p.instance_id"
            " WHERE p.closed_at IS NULL AND p.deleted_at IS NULL"
            " AND i.profile=? ORDER BY p.id",
            (ctx,),
        ).fetchall()
    return conn.execute(
        "SELECT p.id, p.title, p.url, i.profile AS ctx FROM pages p"
        " JOIN instances i ON i.id=p.instance_id"
        " WHERE p.closed_at IS NULL AND p.deleted_at IS NULL ORDER BY p.id",
    ).fetchall()
