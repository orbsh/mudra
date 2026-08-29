#!/usr/bin/env python3
"""mudrad — mudra 守护进程。

常驻：对每个 running=1 的 instance 连它的 browser-level CDP WebSocket，
订阅 Target.setDiscoverTargets 的 targetCreated / infoChanged / Destroyed，
把页面的开/关/URL 变化实时同步进 sqlite 的 pages 表（单一数据源 = CDP）。

用法：mudrad start | stop | status  （P1 先支持直接前台跑，start/stop 后续加）
"""

from __future__ import annotations

import fcntl
import json
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from mudralib import cdp, db, spawn, ui

# 新窗口拦截：页面注入脚本把 window.open / target=_blank 发送到这里，mudrad 拉起 --app 窗口。
_INTERCEPT_PORT = 8899
_INJECT_JS = r"""(() => {
  if (window.__mudraInjected) return; window.__mudraInjected = true;
  const SESSION = "__SESSION__";
  const EP = "http://127.0.0.1:__PORT__/open";
  function openUrl(u) {
    if (!/^https?:/.test(u || "")) return;
    try { navigator.sendBeacon(EP, new Blob([JSON.stringify({url:u, session:SESSION})], {type:"text/plain"})); } catch(e){}
  }
  const ow = window.open;
  window.open = function(u, n, f) {
    if (u && /^https?:/.test(u)) { openUrl(u); return null; }
    return ow.call(window, u, n, f);
  };
  document.addEventListener("click", function(e) {
    const a = e.target && e.target.closest ? e.target.closest("a") : null;
    if (a && a.href && (a.target === "_blank" || e.ctrlKey || e.metaKey || e.button === 1)) {
      e.preventDefault(); openUrl(a.href);
    }
  }, true);
})();"""


def _acquire_lock() -> None:
    """单例锁：同一时间只允许一个 mudrad 运行（flock 随进程退出自动释放）。"""
    db.DB.parent.mkdir(parents=True, exist_ok=True)
    fh = open(db.DB.parent / "mudrad.lock", "w")
    try:
        fcntl.flock(fh, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print("[mudrad] another daemon is already running")
        sys.exit(1)
        return
    globals()["_LOCK_FH"] = fh  # 持引用防 GC 释放锁


class _InterceptHandler(BaseHTTPRequestHandler):
    """页面注入脚本的 sendBeacon POST /open → mudrad._open 拉 --app 窗口。"""
    def do_POST(self):
        mudrad = self.server.mudrad
        try:
            n = int(self.headers.get("Content-Length") or 0)
            body = json.loads(self.rfile.read(n) or b"{}")
            mudrad._open(body)
        except Exception as e:
            print(f"[mudrad] intercept err: {e}")
        self.send_response(204)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()

    def log_message(self, *args):  # 静默
        pass


class Mudrad:
    def __init__(self) -> None:
        self._threads: dict[int, threading.Thread] = {}

    # ---- sqlite 同步 ----
    def _session_id(self, conn, inst_id: int) -> int | None:
        r = conn.execute(
            "SELECT id FROM sessions WHERE instance_id=?", (inst_id,)
        ).fetchone()
        return r["id"] if r else None

    def _session_name(self, conn, inst_id: int) -> str | None:
        r = conn.execute(
            "SELECT name FROM sessions WHERE instance_id=?", (inst_id,)
        ).fetchone()
        return r["name"] if r else None

    # ---- 新窗口拦截：给页面注入脚本，把新 tab 交给 mudrad 拉 --app ----
    def _inject_page(self, port: int, target_id: str, session: str) -> None:
        try:
            with urllib.request.urlopen(
                f"http://127.0.0.1:{port}/json", timeout=5
            ) as r:
                targets = json.loads(r.read())
            wsurl = next(
                t["webSocketDebuggerUrl"] for t in targets
                if t["id"] == target_id and t.get("type") == "page"
            )
            ws = cdp.WsClient(wsurl, timeout=8)
            src = _INJECT_JS.replace("__SESSION__", session).replace("__PORT__", str(_INTERCEPT_PORT))
            cdp.call(ws, "Page.addScriptToEvaluateOnNewDocument", {"source": src})
            cdp.call(ws, "Runtime.evaluate", {"expression": src})  # 已加载页立即注入
            ws.close()
        except Exception as e:
            print(f"[mudrad] inject {session}/{target_id} err: {e}")

    def _open(self, data: dict) -> None:
        """拦截到的“新开 tab” → 在该会话拉起 --app 窗口（并入已有实例）。"""
        url, session = (data or {}).get("url"), (data or {}).get("session")
        if not url or not session:
            return
        try:
            spawn.launch(session, spawn.normalize_url(url), None)
            print(f"[mudrad] new-window -> --app: {url} (session {session})")
        except Exception as e:
            print(f"[mudrad] open err: {e}")

    def _sync_infos(self, inst_id: int, infos: list[dict]) -> None:
        with db.connect() as conn:
            sid = self._session_id(conn, inst_id)
            if sid is None:
                return
            pages = [t for t in infos if t.get("type") == "page"]
            # CDP openerId = 由哪个页面打开（window.open / _blank / 新窗拦截），即父页
            t2id: dict[str, int] = {}
            for t in pages:
                tid = t["targetId"]
                row = conn.execute(
                    "SELECT id FROM pages WHERE session_id=? AND target_id=?",
                    (sid, tid),
                ).fetchone()
                if row:
                    pid = row["id"]
                    conn.execute(
                        "UPDATE pages SET url=?, title=?, closed_at=NULL WHERE id=?",
                        (t.get("url", ""), t.get("title", ""), pid),
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
                    pid = conn.execute(
                        "SELECT id FROM pages WHERE session_id=? AND target_id=?",
                        (sid, tid),
                    ).fetchone()["id"]
                t2id[tid] = pid
            # 回填父子关系：子页 parent_id = 打开它的页（仅首次设置，不覆盖人工值）
            for t in pages:
                oid = t.get("openerId")
                if not oid:
                    continue
                cid = t2id.get(t["targetId"])
                if cid is None:
                    continue
                p_id = t2id.get(oid)
                if p_id is None:  # opener 不在本批，回查库
                    r = conn.execute(
                        "SELECT id FROM pages WHERE session_id=? AND target_id=?",
                        (sid, oid),
                    ).fetchone()
                    p_id = r["id"] if r else None
                if p_id is not None:
                    conn.execute(
                        "UPDATE pages SET parent_id=? WHERE id=? AND parent_id IS NULL",
                        (p_id, cid),
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
                    print(f"[mudrad] instance {iid} never ready on :{port}; marking down")
                    ws = None
                else:
                    time.sleep(1)
        if ws is None:
            self._mark_down(iid)
            self._threads.pop(iid, None)
            return
        try:
            # 基线：先全量同步一次，再开发现；并给每个页面注入新窗口拦截脚本
            r = cdp.call(ws, "Target.getTargets")
            infos = r["result"]["targetInfos"]
            self._sync_infos(iid, infos)
            with db.connect() as conn:
                sname = self._session_name(conn, iid)
            if sname:
                for t in infos:
                    if t.get("type") == "page":
                        self._inject_page(port, t["targetId"], sname)
            cdp.call(ws, "Target.setDiscoverTargets", {"discover": True})
            while True:
                msg = json.loads(ws.recv_text())
                method = msg.get("method")
                p = msg.get("params", {})
                if method == "Target.targetCreated":
                    info = p.get("targetInfo", {})
                    if info.get("type") == "page":
                        self._sync_infos(iid, [info])
                        if sname:
                            self._inject_page(port, info["targetId"], sname)
                elif method == "Target.targetInfoChanged":
                    info = p.get("targetInfo", {})
                    if info.get("type") == "page":
                        self._sync_infos(iid, [info])
                elif method == "Target.targetDestroyed":
                    self._close_target(iid, p.get("targetId", ""))
        except Exception as e:  # 连接断开/崩 = instance 掉了
            print(f"[mudrad] instance {iid} disconnected: {e}")
        finally:
            self._mark_down(iid)
            self._threads.pop(iid, None)

    # ---- 主循环 ----
    def _start_intercept_server(self) -> None:
        srv = ThreadingHTTPServer(("127.0.0.1", _INTERCEPT_PORT), _InterceptHandler)
        srv.mudrad = self
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        print(f"[mudrad] intercept server http://127.0.0.1:{_INTERCEPT_PORT}")

    def run(self) -> None:
        print("[mudrad] started")
        self._start_intercept_server()
        ui._start_services()
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

    ap = argparse.ArgumentParser(prog="mudrad", description="mudra daemon")
    ap.add_argument("act", nargs="?", default="run", help="run (default)")
    args = ap.parse_args()
    if args.act == "run":
        _acquire_lock()
        Mudrad().run()
    else:
        print("P1: only 'run' supported")


if __name__ == "__main__":
    main()