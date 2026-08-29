"""mudra 浏览器会话管理——命令行入口 (P0: new / ls)."""

from __future__ import annotations

import argparse
import os
import signal
import sqlite3
import sys
import time
from urllib.parse import urlparse

from mudralib import ctl, db, spawn, ui, wm


def cmd_new(args: argparse.Namespace) -> int:
    ws = args.workspace or f"web:{args.name}"
    now = int(time.time())
    try:
        with db.connect() as conn:
            cur = conn.execute(
                "INSERT INTO sessions(name, workspace, created_at) VALUES(?,?,?)",
                (args.name, ws, now),
            )
            sid = cur.lastrowid
    except sqlite3.IntegrityError:
        print(f"session {args.name!r} already exists")
        return 1
    print(f"session {args.name!r} created (id={sid}, workspace={ws})")
    return 0


def cmd_ls(args: argparse.Namespace) -> int:
    with db.connect() as conn:
        if args.name:
            s = conn.execute(
                "SELECT id,name,workspace FROM sessions WHERE name=?", (args.name,)
            ).fetchone()
            if not s:
                print(f"no session {args.name!r}")
                return 1
            pages = conn.execute(
                "SELECT id,url,title,position,closed_at FROM pages"
                " WHERE session_id=? ORDER BY position",
                (s["id"],),
            ).fetchall()
            if args.filter:
                pages = [
                    p for p in pages
                    if args.filter in f"{p['url']} {p['title'] or ''}"
                ]
            print(f"session {args.name!r} (ws={s['workspace'] or '-'}): {len(pages)} pages")
            for p in pages:
                mark = "[closed]" if p["closed_at"] else "[open]"
                print(f"  {mark} #{p['position']} {p['url']}  {p['title'] or ''}")
        else:
            cur_session = db.get_state(conn, "current_session")
            rows = conn.execute(
                "SELECT id,name,workspace,created_at FROM sessions ORDER BY id"
            ).fetchall()
            for s in rows:
                mark = "*" if s["name"] == cur_session else " "
                print(f"{mark}{s['id']:>2}  {s['name']:<20} ws={s['workspace'] or ''}")
    return 0


def _pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)  # 信号 0 = 只探存活
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return os.path.exists(f"/proc/{pid}")


def cmd_open(args: argparse.Namespace) -> int:
    now = int(time.time())
    with db.connect() as conn:
        s = conn.execute(
            "SELECT id,instance_id FROM sessions WHERE name=?", (args.name,)
        ).fetchone()
        if s:
            sid = s["id"]
            inst = (
                conn.execute(
                    "SELECT * FROM instances WHERE id=? AND running=1", (s["instance_id"],)
                ).fetchone()
                if s["instance_id"]
                else None
            )
        else:
            cur = conn.execute(
                "INSERT INTO sessions(name,workspace,created_at) VALUES(?,?,?)",
                (args.name, f"web:{args.name}", now),
            )
            sid, inst, s = cur.lastrowid, None, {"id": cur.lastrowid, "instance_id": None}
    if inst:
        if _pid_alive(inst["pid"]):
            print(f"session {args.name!r} already running (port {inst['port']})")
            return 0
        with db.connect() as conn:
            conn.execute("UPDATE instances SET running=0 WHERE id=?", (inst["id"],))
            conn.execute(
                "UPDATE pages SET closed_at=? WHERE session_id=? AND closed_at IS NULL",
                (int(time.time()), sid),
            )
            conn.commit()
    # 复用 conf 预建的实例配置行(running=0),取 proxy/extensions
    with db.connect() as conn:
        irow = (
            conn.execute(
                "SELECT * FROM instances WHERE id=?", (s["instance_id"],)
            ).fetchone()
            if s and s["instance_id"]
            else None
        )
    port = spawn.free_port(9200)
    url = spawn.normalize_url(args.url)
    proxy = irow["proxy"] if irow else None
    ext = irow["extensions"] if irow and irow["extensions"] else None
    pid, udir = spawn.launch(
        args.name, url, port,
        proxy=proxy,
        extensions=ext.split(",") if ext else None,
    )
    if args.url:
        _apply_site_width(url, pid)
    with db.connect() as conn:
        if irow:
            conn.execute(
                "UPDATE instances SET profile=?,port=?,pid=?,running=1 WHERE id=?",
                (udir, port, pid, irow["id"]),
            )
            iid = irow["id"]
        else:
            cur = conn.execute(
                "INSERT INTO instances(profile,port,pid,running,proxy,extensions)"
                " VALUES(?,?,?,1,?,?)",
                (udir, port, pid, proxy, ext),
            )
            iid = cur.lastrowid
        conn.execute("UPDATE sessions SET instance_id=? WHERE id=?", (iid, sid))
    print(f"opened {args.url!r} in session {args.name!r} (port {port}, pid {pid})"
          + (f" proxy={proxy}" if proxy else ""))
    print("pages 由 mudrad daemon 实时同步")
    return 0


def _require_port(args) -> tuple[int, str] | int:
    port = ctl._port(args.name)
    if not port:
        print(f"session {args.name!r} not running")
        return 1
    return port, args.name


def cmd_targets(args) -> int:
    got = _require_port(args)
    if isinstance(got, int):
        return got
    port, _ = got
    for t in ctl.list_pages(port):
        print(f"{t['targetId'][:8]}  {t.get('title','')[:40]:<40} {t.get('url','')}")
    return 0


def cmd_focus(args) -> int:
    if not getattr(args, "name", None):
        with db.connect() as conn:
            args.name = db.get_state(conn, "current_session")
    got = _require_port(args)
    if isinstance(got, int):
        return got
    port, name = got
    hits = ctl.find(port, args.query)
    if not hits:
        print(f"no page matching {args.query!r}")
        return 1
    ctl.activate(port, hits[0]["targetId"])
    # CDP 只激活 tab；把该 session 实例的 niri 窗口带到前台，才算"切换"
    try:
        with db.connect() as conn:
            row = conn.execute(
                "SELECT i.pid FROM instances i JOIN sessions s ON s.instance_id=i.id"
                " WHERE s.name=? AND i.running=1", (name,)
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
    cur = ctl.current_target_id(args.name)
    if not cur:
        print(f"session {args.name!r} not running or no open page")
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


def cmd_use(args: argparse.Namespace) -> int:
    with db.connect() as conn:
        if args.name:
            s = conn.execute(
                "SELECT id FROM sessions WHERE name=?", (args.name,)
            ).fetchone()
            if not s:
                conn.execute(
                    "INSERT INTO sessions(name,workspace,created_at) VALUES(?,?,?)",
                    (args.name, f"web:{args.name}", int(time.time())),
                )
                print(f"created session {args.name!r}")
            db.set_state(conn, "current_session", args.name)
            conn.commit()
            print(f"current session -> {args.name}")
        else:
            cur = db.get_state(conn, "current_session")
            print(f"current session: {cur or '(none)'}")
    return 0


def cmd_mode(args: argparse.Namespace) -> int:
    """walker 模式状态机：walker_mode(session|tab) + op_mod(1|0)。"""
    with db.connect() as conn:
        shown = args.cmd in (None, "show")
        if shown:
            wm = db.get_state(conn, "walker_mode") or "session"
            om = db.get_state(conn, "op_mod") or "0"
            print(f"walker_mode={wm}  op_mod={om}")
        elif args.cmd in ("session", "tab"):
            db.set_state(conn, "walker_mode", args.cmd)
            conn.commit()
            print(f"walker_mode -> {args.cmd}")
        elif args.cmd == "flip":
            wm = db.get_state(conn, "walker_mode") or "session"
            nxt = "tab" if wm == "session" else "session"
            db.set_state(conn, "walker_mode", nxt)
            conn.commit()
            print(f"@: walker_mode {wm} -> {nxt}")
        elif args.cmd == "op":
            om = db.get_state(conn, "op_mod") or "0"
            nxt = "0" if om == "1" else "1"
            db.set_state(conn, "op_mod", nxt)
            conn.commit()
            print(f"#: op_mod {om} -> {nxt}")
        else:
            print("usage: mode [session|tab|flip|op]")
            return 1
    return 0


def cmd_add(args: argparse.Namespace) -> int:
    with db.connect() as conn:
        s = conn.execute(
            "SELECT id,instance_id FROM sessions WHERE name=?", (args.name,)
        ).fetchone()
        inst = None
        if s and s["instance_id"]:
            inst = conn.execute(
                "SELECT * FROM instances WHERE id=? AND running=1", (s["instance_id"],)
            ).fetchone()
    if not s:
        print(f"no session {args.name!r}")
        return 1
    if not inst or not _pid_alive(inst["pid"]):
        print(f"session {args.name!r} not running; use `mudra open` first")
        return 1
    url = spawn.normalize_url(args.url)
    mgr = wm.get()
    prev = mgr.focused_window_id() if args.bg else None
    before = mgr.window_ids() if args.bg else None
    ext = inst["extensions"] if inst["extensions"] else None
    spawn.launch(
        args.name, url, None,  # 无 debug 端口 → 并入已有实例的新 --app 窗口
        proxy=inst["proxy"],
        extensions=ext.split(",") if ext else None,
    )
    if args.bg:
        assert before is not None
        nwid = mgr.wait_for_new_window(before)
        if nwid is not None and prev is not None:
            import time
            time.sleep(0.5)  # 等新窗抢焦落定，再还给旧窗
            mgr.focus_window(prev)
        print(f"added {url!r} to session {args.name!r} in BACKGROUND (focus kept on {prev})")
    else:
        _apply_site_width(url, inst["pid"])
        print(f"added {url!r} to session {args.name!r} (new window in instance)")
    return 0


def cmd_close(args: argparse.Namespace) -> int:
    with db.connect() as conn:
        s = conn.execute(
            "SELECT id,instance_id FROM sessions WHERE name=?", (args.name,)
        ).fetchone()
        if not s:
            print(f"no session {args.name!r}")
            return 1
        inst = (
            conn.execute(
                "SELECT * FROM instances WHERE id=? AND running=1", (s["instance_id"],)
            ).fetchone()
            if s["instance_id"]
            else None
        )
        if args.query:
            # 按 tab 主动关闭：删除该页并关掉对应 target
            if not inst or not _pid_alive(inst["pid"]):
                print(f"session {args.name!r} not running")
                return 1
            cur = conn.execute(
                "SELECT id,position,url,target_id FROM pages"
                " WHERE session_id=? AND closed_at IS NULL AND url LIKE ?",
                (s["id"], f"%{args.query}%"),
            ).fetchone()
            if not cur:
                print(f"no open page in {args.name!r} matching {args.query!r}")
                return 1
            conn.execute("DELETE FROM pages WHERE id=?", (cur["id"],))  # 主动关 → 删除
            conn.commit()
            if cur["target_id"]:
                ctl.close_target(inst["port"], cur["target_id"])
            print(f"closed tab #{cur['position']} {cur['url']}")
        else:
            # 关整个 session
            if inst and _pid_alive(inst["pid"]):
                os.kill(inst["pid"], signal.SIGTERM)  # 主动关浏览器
            conn.execute("DELETE FROM pages WHERE session_id=?", (s["id"],))
            if inst:
                conn.execute(
                    "UPDATE instances SET running=0 WHERE id=?", (inst["id"],)
                )
            conn.commit()
            print(f"closed session {args.name!r}")
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

    Action(act copy) / tag 指派都以此为目标页。返回 pages 行（含 session_id/title/url/target_id）。
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


def _apply_site_width(url: str, pid: int) -> None:
    """按页面 domain 查记忆列宽；等该实例窗口聚焦后应用。"""
    domain = _domain(url)
    if not domain:
        return
    with db.connect() as conn:
        w = conn.execute(
            "SELECT proportion FROM site_widths WHERE site=?", (domain,)
        ).fetchone()
    if not w:
        return
    mgr = wm.get()
    for _ in range(30):  # 等新窗落地并聚焦（新窗抢焦）
        win = mgr.focused_window()
        if win is not None and win.get("pid") == pid:
            break
        time.sleep(0.1)
    time.sleep(0.2)
    mgr.set_column_width(w["proportion"])
    print(f"  applied remembered width {w['proportion']:.3f} for {domain}")


def cmd_conf(args: argparse.Namespace) -> int:
    """per-session 代理/扩展配置；预建 running=0 的实例行，open 时复用。"""
    with db.connect() as conn:
        s = conn.execute(
            "SELECT id,instance_id FROM sessions WHERE name=?", (args.name,)
        ).fetchone()
        if s:
            sid, iid = s["id"], s["instance_id"]
        else:
            cur = conn.execute(
                "INSERT INTO sessions(name,workspace,created_at) VALUES(?,?,?)",
                (args.name, f"web:{args.name}", int(time.time())),
            )
            sid, iid = cur.lastrowid, None
        if not iid:
            cur = conn.execute(
                "INSERT INTO instances(profile,port,pid,running) VALUES(NULL,NULL,NULL,0)"
            )
            iid = cur.lastrowid
            conn.execute("UPDATE sessions SET instance_id=? WHERE id=?", (iid, sid))
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
        print(f"session {args.name!r}: proxy={row['proxy'] or '(none)'}  "
              f"extensions={row['extensions'] or '(default surfingkeys)'}")
    return 0


def _menu_pages(conn, query: str) -> int:
    """Page 模式(p) 菜单数据：当前 session 的开页，TAB 三列（title / url / url）。
    elephant menus 脚本按这行格式解析成 Text/Subtext/Value。
    """
    cur = db.get_state(conn, "current_session")
    if not cur:
        return 0
    q = query.lower()
    rows = conn.execute(
        "SELECT id,url,title,position FROM pages"
        " WHERE closed_at IS NULL"
        " AND session_id = (SELECT id FROM sessions WHERE name=?)"
        " ORDER BY position",
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


def _page_for_url(conn, name: str, url: str) -> dict | None:
    """在某 session 里按 url 找 open 页行（page 模式的选中项）。"""
    return conn.execute(
        "SELECT p.* FROM pages p JOIN sessions s ON s.id=p.session_id"
        " WHERE s.name=? AND p.closed_at IS NULL AND p.url=?"
        " ORDER BY p.position LIMIT 1",
        (name, url),
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
    if not getattr(args, "name", None):
        with db.connect() as conn:
            args.name = db.get_state(conn, "current_session")
    got = _require_port(args)
    if isinstance(got, int):
        return got
    port, name = got
    with db.connect() as conn:
        page = _page_for_url(conn, name, args.url)
        if not page:
            print(f"no open page matching {args.url!r} in {name!r}")
            return 1
        row = conn.execute(
            "SELECT i.pid FROM instances i JOIN sessions s ON s.instance_id=i.id"
            " WHERE s.name=? AND i.running=1", (name,)
        ).fetchone()
    if not row:
        print(f"session {name!r} not running")
        return 1
    mgr = wm.get()
    wid = _window_for_page(row["pid"], page)
    if args.op == "close":
        conn = db.connect()
        conn.execute("DELETE FROM pages WHERE id=?", (page["id"],))
        conn.commit()
        if page["target_id"]:
            ctl.close_target(port, page["target_id"])
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
            "SELECT i.pid FROM instances i JOIN sessions s ON s.instance_id=i.id"
            " WHERE s.name=? AND i.running=1",
            (args.name,),
        ).fetchone()
    if not row:
        print(f"session {args.name!r} not running")
        return 1
    mgr = wm.get()
    wids = [w["id"] for w in mgr.windows_for_instance(row["pid"])]
    if not wids:
        print(f"no niri windows found for session {args.name!r}")
        return 1
    for wid in wids:
        mgr.focus_window(wid)
        mgr.move_to_workspace(args.workspace)
    print(f"moved {len(wids)} window(s) of {args.name!r} to workspace {args.workspace}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(prog="mudra", description="browser session manager")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("new", help="create a session")
    p.add_argument("name")
    p.add_argument("--workspace", "-w", help="niri workspace (default web:<name>)")
    p.set_defaults(fn=cmd_new)

    l = sub.add_parser("ls", help="list sessions / pages")
    l.add_argument("name", nargs="?", help="list pages of a session")
    l.add_argument("--filter", "-f", help="filter pages by url/title substring")
    l.set_defaults(fn=cmd_ls)

    o = sub.add_parser("open", help="open a session (spawn instance + first url)")
    o.add_argument("name")
    o.add_argument("url")
    o.set_defaults(fn=cmd_open)

    t = sub.add_parser("targets", help="list live page targets (CDP)")
    t.add_argument("name")
    t.set_defaults(fn=cmd_targets)
    f = sub.add_parser("focus", help="find page by url/title and activate (name optional → current session)")
    f.add_argument("name", nargs="?", help="session name (default: current)")
    f.add_argument("query")
    f.set_defaults(fn=cmd_focus)
    g = sub.add_parser("goto", help="navigate current page to url")
    g.add_argument("name")
    g.add_argument("url")
    g.set_defaults(fn=cmd_goto)
    b = sub.add_parser("back", help="history back")
    b.add_argument("name")
    b.set_defaults(fn=cmd_back)
    fw = sub.add_parser("forward", help="history forward")
    fw.add_argument("name")
    fw.set_defaults(fn=cmd_forward)
    rl = sub.add_parser("reload", help="reload current page")
    rl.add_argument("name")
    rl.set_defaults(fn=cmd_reload)

    pg = sub.add_parser("page", help="page 模式动作：对选中页 move-here / swap / close")
    pg.add_argument("op", choices=["move-here", "swap", "close"])
    pg.add_argument("url")
    pg.add_argument("name", nargs="?", help="session name (default: current)")
    pg.set_defaults(fn=cmd_page)

    m = sub.add_parser("move", help="move a session's windows to a workspace")
    m.add_argument("name")
    m.add_argument("workspace", help="target niri workspace")
    m.set_defaults(fn=cmd_move)

    u = sub.add_parser("use", help="set / show current session (k8s-ns like)")
    u.add_argument("name", nargs="?", help="session to switch to (creates if missing)")
    u.set_defaults(fn=cmd_use)

    mo = sub.add_parser("mode", help="walker mode state: session|tab|flip|op")
    mo.add_argument("cmd", nargs="?", help="session/tab/flip/op (default: show)")
    mo.set_defaults(fn=cmd_mode)

    a = sub.add_parser("add", help="add a page to a running session")
    a.add_argument("name")
    a.add_argument("url")
    a.add_argument("--bg", action="store_true", help="open in background (keep current focus)")
    a.set_defaults(fn=cmd_add)

    c = sub.add_parser("close", help="close a tab (<query>) or a whole session")
    c.add_argument("name")
    c.add_argument("query", nargs="?", help="url filter → close just that open tab")
    c.set_defaults(fn=cmd_close)

    cf = sub.add_parser("conf", help="set per-session proxy/extensions config")
    cf.add_argument("name")
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