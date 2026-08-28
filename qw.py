"""qw 浏览器会话管理——命令行入口 (P0: new / ls)."""

from __future__ import annotations

import argparse
import sqlite3
import sys
import time

from qwlib import db, spawn


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

    a = ap.parse_args()
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())