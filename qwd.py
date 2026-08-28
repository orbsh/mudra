#!/usr/bin/env python3
"""qwd — qw 守护进程。

常驻：对每个 running=1 的 instance 连它的 browser-level CDP WebSocket，
订阅 Target.setDiscoverTargets 的 targetCreated / infoChanged / Destroyed，
把页面的开/关/URL 变化实时同步进 sqlite 的 pages 表（单一数据源 = CDP）。

用法：qwd start | stop | status  （P1 先支持直接前台跑，start/stop 后续加）
"""

from __future__ import annotations

import fcntl
import json
import sys
import threading
import time

from qwlib import cdp, db


def _acquire_lock() -> None:
    """单例锁：同一时间只允许一个 qwd 运行（flock 随进程退出自动释放）。"""
    db.DB.parent.mkdir(parents=True, exist_ok=True)
    fh = open(db.DB.parent / "qwd.lock", "w")
    try:
        fcntl.flock(fh, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print("[qwd] another daemon is already running")
        sys.exit(1)
        return
    globals()["_LOCK_FH"] = fh  # 持引用防 GC 释放锁


class Qwd:
    def __init__(self) -> None:
        self._threads: dict[int, threading.Thread] = {}

    # ---- sqlite 同步 ----
    def _session_id(self, conn, inst_id: int) -> int | None:
        r = conn.execute(
            "SELECT id FROM sessions WHERE instance_id=?", (inst_id,)
        ).fetchone()
        return r["id"] if r else None

    def _sync_infos(self, inst_id: int, infos: list[dict]) -> None:
        with db.connect() as conn:
            sid = self._session_id(conn, inst_id)
            if sid is None:
                return
            for t in infos:
                if t.get("type") != "page":
                    continue
                tid = t["targetId"]
                row = conn.execute(
                    "SELECT id FROM pages WHERE session_id=? AND target_id=?",
                    (sid, tid),
                ).fetchone()
                if row:
                    conn.execute(
                        "UPDATE pages SET url=?, title=?, closed_at=NULL WHERE id=?",
                        (t.get("url", ""), t.get("title", ""), row["id"]),
                    )
                else:
                    pos = conn.execute(
                        "SELECT COALESCE(MAX(position),-1)+1 AS p FROM pages"
                        " WHERE session_id=?",
                        (sid,),
                    ).fetchone()["p"]
                    conn.execute(
                        "INSERT OR IGNORE INTO pages"
                        "(session_id,target_id,url,title,position,opened_at)"
                        " VALUES(?,?,?,?,?,?)",
                        (sid, tid, t.get("url", ""), t.get("title", ""), pos,
                         int(time.time())),
                    )
            conn.commit()

    def _close_target(self, inst_id: int, target_id: str) -> None:
        with db.connect() as conn:
            sid = self._session_id(conn, inst_id)
            if sid is None:
                return
            conn.execute(
                "UPDATE pages SET closed_at=? WHERE session_id=? AND target_id=?"
                " AND closed_at IS NULL",
                (int(time.time()), sid, target_id),
            )
            conn.commit()

    def _mark_down(self, inst_id: int) -> None:
        with db.connect() as conn:
            conn.execute("UPDATE instances SET running=0 WHERE id=?", (inst_id,))
            sid = self._session_id(conn, inst_id)
            if sid is not None:
                conn.execute(
                    "UPDATE pages SET closed_at=? WHERE session_id=? AND closed_at IS NULL",
                    (int(time.time()), sid),
                )
            conn.commit()

    # ---- 单 instance 监听 ----
    def _watch(self, inst: dict) -> None:
        iid, port = inst["id"], inst["port"]
        # 竞态防护：chromium 刚 spawn，端口可能还没绑定。重试十多秒，真起不来才标 down。
        ws = None
        for attempt in range(12):
            try:
                ws = cdp.WsClient(cdp.get_browser_ws(port, timeout=2), timeout=60)
                break
            except Exception:
                if attempt == 11:
                    print(f"[qwd] instance {iid} never ready on :{port}; marking down")
                    ws = None
                else:
                    time.sleep(1)
        if ws is None:
            self._mark_down(iid)
            self._threads.pop(iid, None)
            return
        try:
            # 基线：先全量同步一次，再开发现
            r = cdp.call(ws, "Target.getTargets")
            self._sync_infos(iid, r["result"]["targetInfos"])
            cdp.call(ws, "Target.setDiscoverTargets", {"discover": True})
            while True:
                msg = json.loads(ws.recv_text())
                method = msg.get("method")
                p = msg.get("params", {})
                if method == "Target.targetCreated":
                    info = p.get("targetInfo", {})
                    if info.get("type") == "page":
                        self._sync_infos(iid, [info])
                elif method == "Target.targetInfoChanged":
                    info = p.get("targetInfo", {})
                    if info.get("type") == "page":
                        self._sync_infos(iid, [info])
                elif method == "Target.targetDestroyed":
                    self._close_target(iid, p.get("targetId", ""))
        except Exception as e:  # 连接断开/崩 = instance 掉了
            print(f"[qwd] instance {iid} disconnected: {e}")
        finally:
            self._mark_down(iid)
            self._threads.pop(iid, None)

    # ---- 主循环 ----
    def run(self) -> None:
        print("[qwd] started")
        while True:
            with db.connect() as conn:
                insts = conn.execute(
                    "SELECT * FROM instances WHERE running=1"
                ).fetchall()
            for inst in insts:
                iid = inst["id"]
                if iid in self._threads:
                    continue
                t = threading.Thread(target=self._watch, args=(inst,), daemon=True)
                self._threads[iid] = t
                t.start()
            stale = [iid for iid, t in self._threads.items() if not t.is_alive()]
            for iid in stale:
                self._threads.pop(iid, None)
            time.sleep(2)


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(prog="qwd", description="qw daemon")
    ap.add_argument("act", nargs="?", default="run", help="run (default)")
    args = ap.parse_args()
    if args.act == "run":
        _acquire_lock()
        Qwd().run()
    else:
        print("P1: only 'run' supported")


if __name__ == "__main__":
    main()