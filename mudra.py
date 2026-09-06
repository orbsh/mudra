"""mudra browser session management — CLI entry point (P0: new / ls)."""

from __future__ import annotations

import argparse
import sqlite3
import sys
import time
from urllib.parse import urlparse

from mudralib import ctl, db, ops, spawn, ui, wm


def cmd_ls(args: argparse.Namespace) -> int:
    with db.connect() as conn:
        if args.ctx:
            # page list of one context (situation leaf -> instance -> pages)
            inst = conn.execute(
                "SELECT id FROM instances WHERE profile=? ORDER BY id DESC LIMIT 1",
                (args.ctx,),
            ).fetchone()
            pages = conn.execute(
                "SELECT id,url,title,position,closed_at FROM pages"
                " WHERE instance_id=? ORDER BY position",
                (inst["id"],) if inst else (-1,),
            ).fetchall()
            if args.filter:
                pages = [
                    p for p in pages
                    if args.filter in f"{p['url']} {p['title'] or ''}"
                ]
            open_n = sum(1 for p in pages if not p["closed_at"])
            print(f"ctx {args.ctx!r}: {open_n} open / {len(pages)} pages")
            for p in pages:
                mark = "[closed]" if p["closed_at"] else "[open]"
                print(f"  {mark} #{p['position']} {p['url']}  {p['title'] or ''}")
        else:
            # overview of all contexts: situation leaf -> page count, current one marked *
            cur = db.current_context(conn)
            rows = conn.execute(
                "SELECT t.name AS leaf,"
                " (SELECT COUNT(*) FROM pages p JOIN instances i ON i.id=p.instance_id"
                "  WHERE i.profile=t.name AND p.closed_at IS NULL) AS n"
                " FROM tag t WHERE t.parent_id="
                " (SELECT id FROM tag WHERE parent_id=-1 AND name='situation')"
                " ORDER BY t.id"
            ).fetchall()
            for r in rows:
                mark = "*" if r["leaf"] == cur else " "
                print(f"{mark} {r['leaf']:<20} {r['n']} pages")
    return 0


def _ctl(path: str, body: dict) -> dict:
    """CLI -> mudrad control API. Window/instance/page lifecycle is executed only by the
    backend; the CLI just relays messages."""
    import json as _json
    import urllib.error
    import urllib.request
    req = urllib.request.Request(
        f"http://127.0.0.1:8899{path}",
        data=_json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return _json.loads(r.read())
    except urllib.error.HTTPError as e:
        try:
            err = _json.loads(e.read()).get("err", "")
        except Exception:
            err = e.reason
        raise SystemExit(f"mudrad: {err}")
    except urllib.error.URLError:
        raise SystemExit("mudrad not running (start: mudrad run)")


def cmd_open(args: argparse.Namespace) -> int:
    r = _ctl("/open", {"url": args.url, **({"ctx": args.ctx} if args.ctx else {})})
    if r.get("mode") == "joined":
        print(f"joined running instance of {r['ctx']!r} (port {r['port']})")
    else:
        print(f"opened {args.url!r} in ctx {r['ctx']!r}"
              f" (port {r['port']}, pid {r['pid']})")
    return 0


def _require_port(ctx: str) -> tuple[int, str] | int:
    port = ctl._port(ctx)
    if not port:
        print(f"ctx {ctx!r} not running")
        return 1
    return port, ctx


def cmd_targets(args) -> int:
    got = _require_port(args.ctx)
    if isinstance(got, int):
        return got
    port, _ = got
    for t in ctl.list_pages(port):
        print(f"{t['targetId'][:8]}  {t.get('title','')[:40]:<40} {t.get('url','')}")
    return 0


def _current_ctx(conn) -> str:
    ctx = db.current_context(conn)
    if not ctx:
        raise SystemExit("no current context (mudra ctx <situation>)")
    return ctx


def cmd_focus(args) -> int:
    with db.connect() as conn:
        ctx = args.ctx or _current_ctx(conn)
    try:
        ops.focus_ctx_query(ctx, args.query)
        return 0
    except ValueError as e:
        print(e)
        return 1


def _on_current(args, fn) -> int:
    with db.connect() as conn:
        ctx = args.ctx or _current_ctx(conn)
    cur = ctl.current_target_id(ctx)
    if not cur:
        print(f"ctx {ctx!r} not running or no open page")
        return 1
    port, tid = cur
    fn(port, tid)
    return 0


def cmd_goto(args) -> int:
    return _on_current(args, lambda p, t: ctl.goto(p, t, spawn.normalize_url(args.url)))


def cmd_back(args) -> int:
    return _on_current(args, lambda p, t: ctl.back(p, t))


def cmd_forward(args) -> int:
    return _on_current(args, lambda p, t: ctl.forward(p, t))


def cmd_reload(args) -> int:
    return _on_current(args, lambda p, t: ctl.reload(p, t))


def cmd_dev(args: argparse.Namespace) -> int:
    """Extension dev-mode switch: clear the chromium extension cache before spawn (source-loaded changes take effect immediately)."""
    with db.connect() as conn:
        if args.on is None:
            cur = db.get_state(conn, "dev_mode") == "1"
            print(f"dev mode: {'on' if cur else 'off'}")
            return 0
        db.set_state(conn, "dev_mode", "1" if args.on else "0")
    print(f"dev mode -> {'on' if args.on else 'off'}")
    return 0


def cmd_ctx(args: argparse.Namespace) -> int:
    """Switch / show the current context (situation leaf). Switching goes through mudrad /ctx (backend broadcasts to the panel)."""
    with db.connect() as conn:
        if args.ctx:
            leaf = conn.execute(
                "SELECT id FROM tag WHERE name=? AND parent_id="
                " (SELECT id FROM tag WHERE parent_id=-1 AND name='situation')",
                (args.ctx,),
            ).fetchone()
            if not leaf:
                print(f"not a situation leaf: {args.ctx!r}")
                return 1
        else:
            print(f"current ctx: {db.current_context(conn) or '(none)'}")
            return 0
    _ctl("/ctx", {"ctx": args.ctx})
    print(f"current ctx -> {args.ctx}")
    return 0


def cmd_add(args: argparse.Namespace) -> int:
    r = _ctl("/add", {"url": args.url, **({"ctx": args.ctx} if args.ctx else {})})
    print(f"added {args.url!r} to ctx {r['ctx']!r}"
          f" (new window in instance, port {r['port']})")
    return 0


def cmd_close(args: argparse.Namespace) -> int:
    if args.query:
        r = _ctl("/close_page", {"query": args.query,
                                 **({"ctx": args.ctx} if args.ctx else {})})
        print(f"closed tab {r['closed']}")
    else:
        r = _ctl("/close_ctx", {"ctx": args.ctx} if args.ctx else {})
        print(f"closed ctx {r['closed']!r}")
    return 0


def _domain(url: str) -> str:
    return urlparse(url).netloc


def cmd_col(args: argparse.Namespace) -> int:
    """Column-width memory: remember captures the focused window width -> site_widths; show lists them."""
    mgr = wm.get()
    if args.action == "remember":
        win = mgr.focused_window()
        if win is None:
            print("no focused window")
            return 1
        with db.connect() as conn:
            inst = conn.execute(
                "SELECT port FROM instances WHERE pid=? AND running=1",
                (win["pid"],),
            ).fetchone()
        if not inst:
            print("focused window is not a running mudra instance")
            return 1
        title = win.get("title") or ""
        page = next(
            (t for t in ctl.list_pages(inst["port"]) if t.get("title") == title),
            None,
        )
        if page is None:
            print(f"no CDP page matching focused window title {title!r}")
            return 1
        prop = mgr.current_col_width()
        band, frac = wm.snap_column_width(prop)
        domain = _domain(page["url"])
        if not domain:
            print(f"cannot derive domain from url {page['url']!r}")
            return 1
        with db.connect() as conn:
            conn.execute(
                "INSERT INTO site_widths(site, proportion) VALUES(?,?)"
                " ON CONFLICT(site) DO UPDATE SET proportion=excluded.proportion",
                (domain, band),
            )
            conn.commit()
        print(f"remembered {domain}: {prop:.3f} -> {frac}")
        return 0
    with db.connect() as conn:
        rows = conn.execute(
            "SELECT site, proportion FROM site_widths ORDER BY site"
        ).fetchall()
    if args.site:
        rows = [r for r in rows if args.site in r["site"]]
    if not rows:
        print("no remembered column widths")
        return 0
    for r in rows:
        _, frac = wm.snap_column_width(r["proportion"])
        print(f"  {r['site']:<28} {frac}")
    return 0


def _focused_page(conn) -> dict | None:
    """Resolve the currently focused niri window -> its mudra page (shared action target).

    Action(act copy) / tag assignment both target this page. Returns the pages row
    (including instance_id/title/url/target_id). Returns None when the focused window
    is not a mudra instance or no matching CDP page is found.
    """
    win = wm.get().focused_window()
    if not win:
        return None
    inst = conn.execute(
        "SELECT id,port,pid FROM instances WHERE pid=? AND running=1", (win["pid"],)
    ).fetchone()
    if not inst or not inst["port"]:
        return None
    title = win.get("title") or ""
    page = next(
        (t for t in ctl.list_pages(inst["port"]) if t.get("title") == title),
        None,
    )
    if page is None:
        return None
    return conn.execute(
        "SELECT * FROM pages WHERE target_id=? LIMIT 1",
        (page.get("targetId"),),
    ).fetchone()


def cmd_conf(args: argparse.Namespace) -> int:
    """Per-context proxy/extension config; pre-create an instance row with running=0
    (profile = leaf name) so `open` can reuse it."""
    with db.connect() as conn:
        leaf = conn.execute(
            "SELECT id FROM tag WHERE name=? AND parent_id="
            " (SELECT id FROM tag WHERE parent_id=-1 AND name='situation')",
            (args.ctx,),
        ).fetchone()
        if not leaf:
            print(f"not a situation leaf: {args.ctx!r}")
            return 1
        inst = conn.execute(
            "SELECT id FROM instances WHERE profile=? ORDER BY id DESC LIMIT 1",
            (args.ctx,),
        ).fetchone()
        if inst:
            iid = inst["id"]
        else:
            cur = conn.execute(
                "INSERT INTO instances(profile,port,pid,running) VALUES(?,NULL,NULL,0)",
                (args.ctx,),
            )
            iid = cur.lastrowid
        if args.proxy is not None:
            newp = None if args.proxy.lower() in ("none", "off") else args.proxy
            conn.execute("UPDATE instances SET proxy=? WHERE id=?", (newp, iid))
        if args.ext is not None:
            newe = None if args.ext.lower() in ("default", "") else args.ext
            conn.execute("UPDATE instances SET extensions=? WHERE id=?", (newe, iid))
        row = conn.execute(
            "SELECT proxy,extensions FROM instances WHERE id=?", (iid,)
        ).fetchone()
        conn.commit()
        print(f"ctx {args.ctx!r}: proxy={row['proxy'] or '(none)'}  "
              f"extensions={row['extensions'] or '(default surfingkeys)'}")
    return 0


def _menu_pages(conn, query: str) -> int:
    """Page mode (p) menu data: open pages of the current context, three TAB columns (title / url / url).
    The elephant menus script parses each line into Text/Subtext/Value in this format.
    """
    cur = db.current_context(conn)
    if not cur:
        return 0
    q = query.lower()
    rows = conn.execute(
        "SELECT p.id,p.url,p.title,p.position FROM pages p"
        " JOIN instances i ON i.id=p.instance_id"
        " WHERE p.closed_at IS NULL AND i.profile=?"
        " ORDER BY p.position",
        (cur,),
    ).fetchall()
    for p in rows:
        disp = p["title"] or p["url"]
        if q and q not in f"{disp} {p['url']}".lower():
            continue
        print("\t".join([disp, p["url"], p["url"]]))
    return 0


def _seed_tags(conn):
    """Idempotently seed the initial tag forest (situation single-select tree /
    importance·urgency rating trees / topic multi-select tree)."""
    import time as _t
    now = int(_t.time())
    created = 0
    by_name = {}

    def root(name, note=None, hidden=0):
        nonlocal created
        r = conn.execute("SELECT id FROM tag WHERE parent_id=-1 AND name=?", (name,)).fetchone()
        if r:
            return r["id"]
        cur = conn.execute(
            "INSERT INTO tag(parent_id,name,note,hidden,created,updated) VALUES(-1,?,?,?,?,?)",
            (name, note, hidden, now, now),
        )
        created += 1
        return cur.lastrowid

    def child(pid, name, note=None, isolated=0, required=0, rank=None, hidden=0, alias=None):
        nonlocal created
        r = conn.execute("SELECT id FROM tag WHERE parent_id=? AND name=?", (pid, name)).fetchone()
        if r:
            return r["id"]
        conn.execute(
            "INSERT INTO tag(parent_id,name,alias,note,isolated,required,rank,hidden,created,updated)"
            " VALUES(?,?,?,?,?,?,?,?,?,?)",
            (pid, name, alias, note, isolated, required, rank, hidden, now, now),
        )
        created += 1
        return conn.execute("SELECT last_insert_rowid()").fetchone()[0]

    sit = root("situation", "current context (single-select within the tree)")
    importance = root("importance", "importance (rating tree)")
    urgency = root("urgency", "urgency (rating tree)")
    quality = root("quality", "content quality (rating tree; important != high quality)")
    state = root("state", "processing state (single-select within the tree)")
    topic = root("topic", "topic (multi-select)")

    child(sit, "inbox", "default inbox", isolated=1, required=1, alias="pending")
    child(sit, "work", "work", isolated=1, alias="work context")
    child(sit, "personal", "personal", isolated=1, alias="life")
    child(sit, "privacy", "privacy", isolated=1, alias="isolated")
    for i, star in enumerate(["☆", "☆☆", "☆☆☆", "☆☆☆☆", "☆☆☆☆☆"], 1):
        child(importance, star, rank=i)
        child(urgency, star, rank=i)
        child(quality, star, rank=i)
    for name, alias in [("to-read", "unread"), ("reading", "reading"),
                        ("distilled", "distilled"), ("archived", "archived")]:
        child(state, name, alias=alias)
    return created


def cmd_tag(args: argparse.Namespace) -> int:
    with db.connect() as conn:
        if args.action == "init":
            n = _seed_tags(conn)
            conn.commit()
            print(f"tag forest seeded ({n} new nodes, idempotent)")
            return 0
        if args.action in ("add", "remove"):
            return _tag_set(conn, args.tag_id, on=args.action == "add", page_id=args.page_id)
        return 0


def _tag_set(conn, tag_id: str, on: bool, page_id: str | None = None) -> int:
    """Assign/remove a tag on a page (writes page_tag). When page_id is omitted, the
    currently focused page is used (works for both panel batch and single-page).

    Returns (0 success / 1 failure).
    """
    if not str(tag_id).lstrip("-").isdigit():
        print(f"invalid tag_id {tag_id!r}")
        return 1
    row = conn.execute("SELECT id,name FROM tag WHERE id=?", (tag_id,)).fetchone()
    if not row:
        print(f"no tag {tag_id!r}")
        return 1
    if page_id is not None:
        page = conn.execute("SELECT * FROM pages WHERE id=?", (page_id,)).fetchone()
    else:
        page = _focused_page(conn)
    if page is None:
        print("no page to tag (focused window is not a mudra page, or page_id is invalid)")
        return 1
    if on:
        conn.execute(
            "INSERT OR IGNORE INTO page_tag(page_id,tag_id) VALUES(?,?)",
            (page["id"], tag_id),
        )
    else:
        conn.execute(
            "DELETE FROM page_tag WHERE page_id=? AND tag_id=?",
            (page["id"], tag_id),
        )
    conn.commit()
    verb = "assigned" if on else "removed"
    print(f"{verb} tag {row['name']} -> {page['title'] or page['url']} (page#{page['id']})")
    return 0


def cmd_ui(args: argparse.Namespace) -> int:
    """Launch the management panel: mudrad serves /ui + /ws, then opens a floating centered window that loads it."""
    return ui.launch(args)


def cmd_menu(args: argparse.Namespace) -> int:
    """Launcher menu data output (three TAB columns, for the elephant/walker menus provider).
    kind: pages (p-mode menu data). Tag forest management has moved to the management panel."""
    with db.connect() as conn:
        if args.kind == "pages":
            return _menu_pages(conn, args.query)
        return 0

def cmd_sort(args) -> int:
    # kept (panel sorting is optional); MRU/time default to position order in page lists.
    with db.connect() as conn:
        db.set_state(conn, "sort", args.kind)
        conn.commit()
        print(f"sort -> {args.kind}")
    return 0


def _page_for_url(conn, ctx: str, url: str) -> dict | None:
    """Find the open page row by url in a context (the selected item of page mode)."""
    return conn.execute(
        "SELECT p.* FROM pages p JOIN instances i ON i.id=p.instance_id"
        " WHERE i.profile=? AND p.closed_at IS NULL AND p.url=?"
        " ORDER BY p.position LIMIT 1",
        (ctx, url),
    ).fetchone()


def _window_for_page(pid: int, page: dict) -> int | None:
    """The niri window id under an instance pid whose title matches the given page (from CDP list_pages)."""
    title = page.get("title") or ""
    domain = (page.get("url") or "").split("//")[-1].split("/")[0]
    for w in wm.get().windows_for_instance(pid):
        wt = w.get("title") or ""
        if title and wt == title:
            return w["id"]
        if domain and domain in wt:
            return w["id"]
    return None


def cmd_page(args) -> int:
    """Page mode actions: move-here / swap / close on the selected page (selection = url).
    move-here: move to the active workspace; swap: swap workspaces with the focused window; close: close the page.
    """
    if not getattr(args, "ctx", None):
        with db.connect() as conn:
            args.ctx = db.current_context(conn)
    got = _require_port(args.ctx)
    if isinstance(got, int):
        return got
    port, ctx = got
    with db.connect() as conn:
        page = _page_for_url(conn, ctx, args.url)
        if not page:
            print(f"no open page matching {args.url!r} in {ctx!r}")
            return 1
        row = conn.execute(
            "SELECT pid FROM instances WHERE profile=? AND running=1", (ctx,)
        ).fetchone()
    if not row:
        print(f"ctx {ctx!r} not running")
        return 1
    mgr = wm.get()
    wid = _window_for_page(row["pid"], page)
    if args.op == "close":
        _ctl("/close_page", {"ctx": ctx, "query": page["url"]})
        print(f"closed page {page['url']}")
        return 0
    if wid is None:
        print("no niri window matches that page (page not focused into its own window?)")
        return 1
    if args.op == "move-here":
        mgr.focus_window(wid)
        mgr.move_to_workspace(str(mgr.active_workspace()))
        print(f"moved {page['url']} to active workspace")
        return 0
    if args.op == "swap":
        mgr.focus_window(wid)
        wsrc = mgr.workspace_of_window(wid)
        fwin = mgr.focused_window()
        fsrc = fwin["workspace_id"] if fwin else None
        src_ws = fsrc if fsrc is not None else mgr.active_workspace()
        mgr.move_to_workspace(str(src_ws))
        if fwin is not None and fwin["id"] != wid and wsrc is not None:
            mgr.focus_window(fwin["id"])
            mgr.move_to_workspace(str(wsrc))
        print(f"swapped {page['url']} with focused window")
        return 0
    print(f"unknown op {args.op!r}")
    return 1


def cmd_move(args) -> int:
    with db.connect() as conn:
        row = conn.execute(
            "SELECT pid FROM instances WHERE profile=? AND running=1",
            (args.ctx,),
        ).fetchone()
    if not row:
        print(f"ctx {args.ctx!r} not running")
        return 1
    mgr = wm.get()
    wids = [w["id"] for w in mgr.windows_for_instance(row["pid"])]
    if not wids:
        print(f"no niri windows found for ctx {args.ctx!r}")
        return 1
    for wid in wids:
        mgr.focus_window(wid)
        mgr.move_to_workspace(args.workspace)
    print(f"moved {len(wids)} window(s) of {args.ctx!r} to workspace {args.workspace}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(prog="mudra", description="browser context manager")
    sub = ap.add_subparsers(dest="cmd", required=True)

    l = sub.add_parser("ls", help="list contexts / pages of one ctx")
    l.add_argument("ctx", nargs="?", help="list pages of a context (situation leaf)")
    l.add_argument("--filter", "-f", help="filter pages by url/title substring")
    l.set_defaults(fn=cmd_ls)

    o = sub.add_parser("open", help="open url in a context (spawn instance + first url)")
    o.add_argument("url")
    o.add_argument("--ctx", help="situation leaf (default: current context)")
    o.set_defaults(fn=cmd_open)

    t = sub.add_parser("targets", help="list live page targets (CDP)")
    t.add_argument("ctx")
    t.set_defaults(fn=cmd_targets)
    f = sub.add_parser("focus", help="find page by url/title and activate (ctx optional → current)")
    f.add_argument("query")
    f.add_argument("--ctx", help="situation leaf (default: current)")
    f.set_defaults(fn=cmd_focus)
    g = sub.add_parser("goto", help="navigate current page to url")
    g.add_argument("url")
    g.add_argument("--ctx", help="situation leaf (default: current)")
    g.set_defaults(fn=cmd_goto)
    b = sub.add_parser("back", help="history back")
    b.add_argument("--ctx")
    b.set_defaults(fn=cmd_back)
    fw = sub.add_parser("forward", help="history forward")
    fw.add_argument("--ctx")
    fw.set_defaults(fn=cmd_forward)
    rl = sub.add_parser("reload", help="reload current page")
    rl.add_argument("--ctx")
    rl.set_defaults(fn=cmd_reload)

    pg = sub.add_parser("page", help="page mode actions: move-here / swap / close on the selected page")
    pg.add_argument("op", choices=["move-here", "swap", "close"])
    pg.add_argument("url")
    pg.add_argument("--ctx", help="situation leaf (default: current)")
    pg.set_defaults(fn=cmd_page)

    m = sub.add_parser("move", help="move a context's windows to a workspace")
    m.add_argument("ctx")
    m.add_argument("workspace", help="target niri workspace")
    m.set_defaults(fn=cmd_move)

    x = sub.add_parser("ctx", help="set / show current context (situation leaf)")
    x.add_argument("ctx", nargs="?", help="context to switch to")
    dv = sub.add_parser("dev", help="extension dev mode: clear chromium extension caches on spawn")
    dv.add_argument("on", nargs="?", type=lambda s: s.lower() in ("on", "1", "true"),
                    help="on / off (omit to show current)")
    x.set_defaults(fn=cmd_ctx)
    dv.set_defaults(fn=cmd_dev)

    a = sub.add_parser("add", help="add a page to the running context instance")
    a.add_argument("url")
    a.add_argument("--ctx", help="situation leaf (default: current)")
    a.set_defaults(fn=cmd_add)

    c = sub.add_parser("close", help="close a tab (<query>) or a whole context instance")
    c.add_argument("query", nargs="?", help="url filter → close just that open tab")
    c.add_argument("--ctx", help="situation leaf (default: current)")
    c.set_defaults(fn=cmd_close)

    cf = sub.add_parser("conf", help="set per-context proxy/extensions config")
    cf.add_argument("ctx", help="situation leaf")
    cf.add_argument("--proxy", help="proxy e.g. 127.0.0.1:7890, or 'none'")
    cf.add_argument("--ext", help="comma-separated extension dirs, or 'default'")
    cf.set_defaults(fn=cmd_conf)

    col = sub.add_parser("col", help="column-width memory: remember|show")
    col.add_argument(
        "action", nargs="?", default="show", choices=["remember", "show"],
        help="remember capture focused window width; show list",
    )
    col.add_argument("site", nargs="?", help="filter by site in show")
    col.set_defaults(fn=cmd_col)

    tag = sub.add_parser("tag", help="tag forest: init seed / add|remove assignment to pages")
    tag.add_argument("action", choices=["init", "add", "remove"])
    tag.add_argument("tag_id", nargs="?", help="tag id (add/remove)")
    tag.add_argument("page_id", nargs="?", help="target page id (omit = currently focused page, for panel batch ops)")
    tag.set_defaults(fn=cmd_tag)

    ui = sub.add_parser("ui", help="launch the management panel (floating window + ws)")
    ui.set_defaults(fn=cmd_ui)

    m = sub.add_parser("menu", help="launcher menu data (TAB columns for elephant/walker)")
    m.add_argument("kind", choices=["pages"])
    m.add_argument("query", nargs="?", default="")
    m.set_defaults(fn=cmd_menu)

    so = sub.add_parser("sort", help="set sort preference (MRU/time/rating)")
    so.add_argument("kind", choices=["mru", "mtime", "rating"])
    so.set_defaults(fn=cmd_sort)

    a = ap.parse_args()
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())