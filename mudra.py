"""mudra 浏览器会话管理——命令行入口 (P0: new / ls)."""

from __future__ import annotations

import argparse
import sqlite3
import sys
import time
from urllib.parse import urlparse

from mudralib import ctl, db, spawn, ui, wm


def cmd_ls(args: argparse.Namespace) -> int:
    with db.connect() as conn:
        if args.ctx:
            # 某上下文的页列表（situation 叶 → 实例 → pages）
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
            # 全部上下文概览：situation 叶 → 页数，当前项标 *
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
    """CLI → mudrad 控制接口。窗口/实例/页面生命周期只由后端执行，CLI 只传话。"""
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
    got = _require_port(ctx)
    if isinstance(got, int):
        return got
    port, _ = got
    hits = ctl.find(port, args.query)
    if not hits:
        print(f"no page matching {args.query!r}")
        return 1
    ctl.activate(port, hits[0]["targetId"])
    # CDP 只激活 tab；把该上下文实例的 niri 窗口带到前台，才算"切换"
    try:
        with db.connect() as conn:
            row = conn.execute(
                "SELECT pid FROM instances WHERE profile=? AND running=1", (ctx,)
            ).fetchone()
        pid = row["pid"] if row else None
        if pid:
            wins = wm.get().windows_for_instance(pid)
            if wins:
                wm.get().focus_window(wins[0]["id"])
    except Exception:
        pass  # niri 不可用不阻塞 CDP activate
    print(f"focused: {hits[0].get('title') or hits[0].get('url')}")
    return 0


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
    """扩展开发模式开关：spawn 前清 chromium 扩展缓存（源码直载的改动立即可见）。"""
    with db.connect() as conn:
        if args.on is None:
            cur = db.get_state(conn, "dev_mode") == "1"
            print(f"dev mode: {'on' if cur else 'off'}")
            return 0
        db.set_state(conn, "dev_mode", "1" if args.on else "0")
    print(f"dev mode -> {'on' if args.on else 'off'}")
    return 0


def cmd_ctx(args: argparse.Namespace) -> int:
    """切换 / 显示当前上下文（situation 叶）。切换走 mudrad /ctx（后端广播面板）。"""
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
    """列宽记忆：remember 捕获聚焦窗口宽度→site_widths；show 列出。"""
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
    """解析当前聚焦 niri 窗口 → 对应 mudra 页（共享动作对象）。

    Action(act copy) / tag 指派都以此为目标页。返回 pages 行（含 instance_id/title/url/target_id）。
    focused 窗口非 mudra 实例、或找不到匹配 CDP 页时返回 None。
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
    """per-context 代理/扩展配置；预建 running=0 的实例行（profile=叶名），open 时复用。"""
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
    """Page 模式(p) 菜单数据：当前上下文的开页，TAB 三列（title / url / url）。
    elephant menus 脚本按这行格式解析成 Text/Subtext/Value。
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
    """幂等 seed 初始 tag 森林（situation 单选树 / importance·urgency 评分树 / topic 多选树）。"""
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

    sit = root("situation", "当前上下文（树内单选）")
    importance = root("importance", "重要性（评分树）")
    urgency = root("urgency", "时效性（评分树）")
    quality = root("quality", "内容质量（评分树，重要≠质量高）")
    state = root("state", "处理状态（树内单选）")
    topic = root("topic", "主题（可多选）")

    child(sit, "inbox", "默认收入箱", isolated=1, required=1, alias="待处理")
    child(sit, "work", "工作", isolated=1, alias="工作上下文")
    child(sit, "personal", "个人", isolated=1, alias="生活")
    child(sit, "privacy", "隐私", isolated=1, alias="隔离")
    for i, star in enumerate(["☆", "☆☆", "☆☆☆", "☆☆☆☆", "☆☆☆☆☆"], 1):
        child(importance, star, rank=i)
        child(urgency, star, rank=i)
        child(quality, star, rank=i)
    for name, alias in [("未读", "unread"), ("在读", "reading"),
                        ("已提炼", "distilled"), ("已归档", "archived")]:
        child(state, name, alias=alias)
    return created


def cmd_tag(args: argparse.Namespace) -> int:
    with db.connect() as conn:
        if args.action == "init":
            n = _seed_tags(conn)
            conn.commit()
            print(f"tag 森林 seed 完成（新增 {n} 节点，幂等）")
            return 0
        if args.action in ("add", "remove"):
            return _tag_set(conn, args.tag_id, on=args.action == "add", page_id=args.page_id)
        return 0


def _tag_set(conn, tag_id: str, on: bool, page_id: str | None = None) -> int:
    """指派/移除 tag 到页（写 page_tag）。page_id 省略时用当前聚焦页（面板批量/单页都可）。

    返回 (0 成功 / 1 失败)。
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
        print("no page to tag (聚焦窗不是 mudra 页，或 page_id 无效)")
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
    verb = "指派" if on else "移除"
    print(f"{verb} tag {row['name']} -> {page['title'] or page['url']} (page#{page['id']})")
    return 0


def cmd_ui(args: argparse.Namespace) -> int:
    """拉起管理面板：mudrad 服务 /ui + /ws，再开一个浮动居中窗口载入它。"""
    return ui.launch(args)


def cmd_menu(args: argparse.Namespace) -> int:
    """launcher 菜单数据出口（TAB 三列，供 elephant/walker menus provider）。
    kind: pages（p 模式菜单数据）。tag 森林管理已交管理面板。"""
    with db.connect() as conn:
        if args.kind == "pages":
            return _menu_pages(conn, args.query)
        return 0

def cmd_sort(args) -> int:
    # 保留（面板排序可选）；MRU/时间默认在 page 列表走 position。
    with db.connect() as conn:
        db.set_state(conn, "sort", args.kind)
        conn.commit()
        print(f"sort -> {args.kind}")
    return 0


def _page_for_url(conn, ctx: str, url: str) -> dict | None:
    """在某上下文里按 url 找 open 页行（page 模式的选中项）。"""
    return conn.execute(
        "SELECT p.* FROM pages p JOIN instances i ON i.id=p.instance_id"
        " WHERE i.profile=? AND p.closed_at IS NULL AND p.url=?"
        " ORDER BY p.position LIMIT 1",
        (ctx, url),
    ).fetchone()


def _window_for_page(pid: int, page: dict) -> int | None:
    """某实例 pid 下、标题匹配给定页（来自 CDP list_pages）的 niri 窗口 id。"""
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
    """Page 模式动作：对选中页执行 move-here / swap / close（选中项=url）。
    move-here: 移到当前活动工作区; swap: 与当前聚焦窗交换工作区; close: 关该页。
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
        print("no niri window matches that page (page 未聚焦成独立窗口?)")
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

    pg = sub.add_parser("page", help="page 模式动作：对选中页 move-here / swap / close")
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

    tag = sub.add_parser("tag", help="tag 森林：init seed / add|remove 指派到页")
    tag.add_argument("action", choices=["init", "add", "remove"])
    tag.add_argument("tag_id", nargs="?", help="tag id（add/remove）")
    tag.add_argument("page_id", nargs="?", help="目标页 id（省略=当前聚焦页，供面板批量）")
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