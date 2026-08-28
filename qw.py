"""qw 浏览器会话管理——命令行入口 (P0: new / ls)."""

from __future__ import annotations

import argparse
import os
import signal
import sqlite3
import sys
import time

from qwlib import ctl, db, niri, spawn


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
            sid, inst = cur.lastrowid, None
    if inst:
        if _pid_alive(inst["pid"]):
            print(f"session {args.name!r} already running (port {inst['port']})")
            return 0
        # 实例 chromium 已死但 DB 还标 running(如 daemon 缺席时) → 复位后重新拉起
        with db.connect() as conn:
            conn.execute("UPDATE instances SET running=0 WHERE id=?", (s["instance_id"],))
            conn.execute(
                "UPDATE pages SET closed_at=? WHERE session_id=? AND closed_at IS NULL",
                (int(time.time()), sid),
            )
            conn.commit()
    port = spawn.free_port(9200)
    url = spawn.normalize_url(args.url)
    pid, udir = spawn.launch(args.name, url, port)
    with db.connect() as conn:
        cur = conn.execute(
            "INSERT INTO instances(profile,port,pid,running) VALUES(?,?,?,1)",
            (udir, port, pid),
        )
        iid = cur.lastrowid
        conn.execute("UPDATE sessions SET instance_id=? WHERE id=?", (iid, sid))
    print(f"opened {args.url!r} in session {args.name!r} (port {port}, pid {pid})")
    print("pages 由 qwd daemon 实时同步")
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
    got = _require_port(args)
    if isinstance(got, int):
        return got
    port, _ = got
    hits = ctl.find(port, args.query)
    if not hits:
        print(f"no page matching {args.query!r}")
        return 1
    ctl.activate(port, hits[0]["targetId"])
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
        print(f"session {args.name!r} not running; use `qw open` first")
        return 1
    url = spawn.normalize_url(args.url)
    spawn.launch(args.name, url, None)  # 无 debug 端口 → 并入已有实例的新 --app 窗口
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
    wids = [w["id"] for w in niri.windows_for_pid(row["pid"])]
    if not wids:
        print(f"no niri windows found for session {args.name!r}")
        return 1
    for wid in wids:
        niri.focus_window(wid)
        niri.move_focused_to_workspace(args.workspace)
    print(f"moved {len(wids)} window(s) of {args.name!r} to workspace {args.workspace}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(prog="qw", description="browser session manager")
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
    f = sub.add_parser("focus", help="find page by url/title and activate")
    f.add_argument("name")
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
    a.set_defaults(fn=cmd_add)

    c = sub.add_parser("close", help="close a tab (<query>) or a whole session")
    c.add_argument("name")
    c.add_argument("query", nargs="?", help="url filter → close just that open tab")
    c.set_defaults(fn=cmd_close)

    a = ap.parse_args()
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())