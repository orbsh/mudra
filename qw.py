"""qw 浏览器会话管理——命令行入口 (P0: new / ls)."""

from __future__ import annotations

import argparse
import sqlite3
import sys
import time

from qwlib import ctl, db, spawn


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
            print(f"session {args.name!r} (ws={s['workspace'] or '-'}): {len(pages)} pages")
            for p in pages:
                mark = "[closed]" if p["closed_at"] else "[open]"
                print(f"  {mark} #{p['position']} {p['url']}  {p['title'] or ''}")
        else:
            rows = conn.execute(
                "SELECT id,name,workspace,created_at FROM sessions ORDER BY id"
            ).fetchall()
            for s in rows:
                print(f"{s['id']:>2}  {s['name']:<20} ws={s['workspace'] or ''}")
    return 0


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
        print(f"session {args.name!r} already running (port {inst['port']})")
        return 0
    port = spawn.free_port(9200)
    pid, udir = spawn.launch(args.name, args.url, port)
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
    return _on_current(args, lambda p, t: ctl.goto(p, t, args.url))


def cmd_back(args) -> int:
    return _on_current(args, lambda p, t: ctl.back(p, t))


def cmd_forward(args) -> int:
    return _on_current(args, lambda p, t: ctl.forward(p, t))


def cmd_reload(args) -> int:
    return _on_current(args, lambda p, t: ctl.reload(p, t))


def main() -> int:
    ap = argparse.ArgumentParser(prog="qw", description="browser session manager")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("new", help="create a session")
    p.add_argument("name")
    p.add_argument("--workspace", "-w", help="niri workspace (default web:<name>)")
    p.set_defaults(fn=cmd_new)

    l = sub.add_parser("ls", help="list sessions / pages")
    l.add_argument("name", nargs="?", help="list pages of a session")
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

    a = ap.parse_args()
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())