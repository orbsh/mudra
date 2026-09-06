"""Shared action layer: the single implementation of focus / tag / pages semantics.

Both mudrad (HTTP handler) and the mudra CLI route their entry points here, removing
duplicate implementations:
- focus = CDP target activation + bringing the niri window to front (title/domain
  match, fallback to first window; an unavailable niri does not block CDP activate).
- tag = reverse-lookup the open page from tab/url -> page_tag toggle, broadcast pages_changed.
- pages = open-page list across contexts (shared by extension :o and panel batch ops).

Principle: the action layer only orchestrates (db.py functions + CDP/wm); no SQL, no HTTP parsing.
"""

from __future__ import annotations

import json
import time

from mudralib import cdp, db, wm
from mudralib.ui import _broadcast


def focus_page(page_id: int) -> dict:
    """Focus a page: a switch counts only when the CDP target is activated AND the niri window comes to front."""
    with db.connect() as conn:
        row = db.page_by_id_joined(conn, page_id)
    if not row or not row["port"] or not row["target_id"]:
        raise ValueError(f"page {page_id} not found or instance down")
    ctl_activate(row["port"], row["target_id"])
    _focus_instance_window(row["pid"], title=row["title"] or "", url=row["url"] or "")
    return {"focused": page_id}


def focus_ctx_query(ctx: str, query: str) -> dict:
    """CLI focus semantics: fuzzy-find a page by title/URL in the ctx instance -> focus_page."""
    with db.connect() as conn:
        from mudralib.db import instance_for_context  # avoid a cycle: use db directly
        inst = instance_for_context(conn, ctx)
        page_ids = [r["id"] for r in db.pages_open(conn, ctx)]
    if not inst or not inst["port"]:
        raise ValueError(f"ctx {ctx!r} not running")
    hits = _find_pages(inst["port"], query)
    if not hits:
        raise ValueError(f"no page matching {query!r}")
    # Hit targetId -> map to the pages row id, then go through focus_page (no duplicate niri matching)
    with db.connect() as conn:
        page_id = db.page_id_by_target(conn, inst["id"], hits[0]["targetId"])
    if page_id is None:
        raise ValueError("page not recorded (sync pending?)")
    out = focus_page(page_id)
    print(f"focused: {hits[0].get('title') or hits[0].get('url')}")
    return out


def tag_page(tab_id: str | int | None, url: str | None, tag_name: str | None) -> dict:
    """Attach/detach a tag on a page (toggle). tabId/url -> ctx -> open page."""
    if not tag_name:
        raise ValueError("need tag")
    ctx = ctx_for_tab(tab_id, url) if tab_id else None
    if not ctx or not url:
        raise ValueError("need tabId and url to resolve page")
    with db.connect() as conn:
        t = db.tag_id_by_name(conn, tag_name)
        if not t:
            raise ValueError(f"tag not found: {tag_name}")
        inst = conn.execute(
            "SELECT id FROM instances WHERE profile=? ORDER BY id DESC LIMIT 1", (ctx,)
        ).fetchone()
        if not inst:
            raise ValueError(f"no instance for ctx {ctx}")
        page = db.latest_open_page_by_url(conn, inst["id"], _url_prefix(url))
        if not page:
            raise ValueError("page not open in this ctx")
        action = db.page_tag_toggle(conn, page["id"], t)
        conn.commit()
    _broadcast({"event": "pages_changed"})
    return {"tag": tag_name, "action": action}


def list_open_pages(ctx: str | None = None) -> list[dict]:
    """Open-page list (across contexts)."""
    with db.connect() as conn:
        return [dict(r) for r in db.pages_open(conn, ctx)]


def close_page(page_id: int) -> dict:
    """Close semantics: set closed_at + close the CDP target. The row is kept (can be reopened/deleted)."""
    with db.connect() as conn:
        row = db.page_by_id_joined(conn, page_id)
        if not row:
            raise ValueError(f"page {page_id} not found")
        conn.execute(
            "UPDATE pages SET closed_at=? WHERE id=?",
            (int(time.time()), page_id),
        )
        conn.commit()
    if row["port"] and row["target_id"]:
        from mudralib import ctl
        ctl.close_target(row["port"], row["target_id"])
    _broadcast({"event": "pages_changed"})
    return {"closed": page_id}


def open_page(page_id: int) -> dict:
    """Reopen a closed page: open the window via mudrad /open; CDP sync will reset closed_at to NULL."""
    with db.connect() as conn:
        row = conn.execute(
            "SELECT p.url, p.deleted_at, i.profile AS ctx FROM pages p"
            " JOIN instances i ON i.id=p.instance_id WHERE p.id=?",
            (page_id,),
        ).fetchone()
    if not row:
        raise ValueError(f"page {page_id} not found")
    if row["deleted_at"]:
        raise ValueError("page is deleted")
    from mudralib.ui import ctl_open
    ctl_open(row["url"] or "", row["ctx"])
    return {"opened": page_id}


def delete_page(page_id: int) -> dict:
    """Soft delete: allowed only for closed pages; open pages are rejected."""
    with db.connect() as conn:
        if not db.page_closed(conn, page_id):
            raise ValueError("cannot delete an open page; close it first")
        db.page_soft_delete(conn, page_id, int(time.time()))
        conn.commit()
    _broadcast({"event": "pages_changed"})
    return {"deleted": page_id}


def tags_children(parent: str | None = None) -> list[str]:
    """Read the tag tree: child node names under a parent (parent=None -> root level)."""
    with db.connect() as conn:
        return [r["name"] for r in db.tag_children(conn, parent)]


def ctx_for_tab(tab_id: str | int | None, url: str | None = None) -> str | None:
    """Reverse-lookup the instance a tab belongs to -> ctx.

    tab_id may be a CDP targetId (used for injection interception) or a numeric chrome
    tabId (from the extension sender) -- the former is matched against instances via
    /json; the latter falls back to a URL-based DB match when no direct hit.
    """
    if not tab_id:
        return None
    for row in _running_instances():
        if _port_has_target(row["port"], str(tab_id)):
            with db.connect() as conn:
                return db.instance_ctx(conn, row["id"])
    return ctx_for_url(url)


def ctx_for_url(url: str | None) -> str | None:
    if not url:
        return None
    with db.connect() as conn:
        return db.page_ctx_for_url(conn, _url_prefix(url))


def page_info_for_tab(tab_id: str | int | None, url: str | None) -> dict:
    """Status bar data source: tabId/url -> (ctx, page tags, role)."""
    ctx = ctx_for_tab(tab_id, url)
    if not ctx:
        ctx = ctx_for_url(url)
    tags: list[str] = []
    if ctx and url:
        with db.connect() as conn:
            inst = conn.execute(
                "SELECT id FROM instances WHERE profile=? ORDER BY id DESC LIMIT 1",
                (ctx,),
            ).fetchone()
            if inst:
                page = db.latest_open_page_by_url(conn, inst["id"], _url_prefix(url))
                if page:
                    tags = db.page_tag_paths(conn, page["id"])
    from mudralib.ui import PANEL_PORT
    role = "console" if (url or "").startswith(f"http://127.0.0.1:{PANEL_PORT}/") else "page"
    return {"ctx": ctx, "tags": tags, "role": role}


# ---- internal: thin CDP / wm wrappers (easy to swap in tests) ----

def _url_prefix(url: str) -> str:
    return (url.split("#")[0] + "%")[:200]


def _running_instances() -> list:
    with db.connect() as conn:
        return db.running_instances(conn)


def _port_has_target(port: int, target_id: str) -> bool:
    try:
        import json
        import urllib.request
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/json", timeout=2) as r:
            targets = json.loads(r.read())
        return any(t.get("id") == target_id for t in targets)
    except Exception:
        return False


def _find_pages(port: int, query: str) -> list[dict]:
    from mudralib import ctl
    return ctl.find(port, query)


def ctl_activate(port: int, target_id: str) -> None:
    from mudralib import ctl
    ctl.activate(port, target_id)


def _focus_instance_window(pid: int | None, title: str = "", url: str = "") -> None:
    """Bring the niri window to front: exact title or domain-contains match, fallback to the instance's first window."""
    if not pid:
        return
    try:
        domain = url.split("//")[-1].split("/")[0]
        mgr = wm.get()
        for w in mgr.windows_for_instance(pid):
            wt = w.get("title") or ""
            if (title and wt == title) or (domain and domain in wt):
                mgr.focus_window(w["id"])
                return
        wins = mgr.windows_for_instance(pid)
        if wins:
            mgr.focus_window(wins[0]["id"])
    except Exception:
        pass  # an unavailable niri does not block CDP activate
