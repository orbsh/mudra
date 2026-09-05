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
import os
import signal
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from mudralib import cdp, ctl, db, spawn, ui, wm

# 新窗口拦截：页面注入脚本把 window.open / target=_blank 发送到这里，mudrad 拉起 --app 窗口。
_INTERCEPT_PORT = 8899
_INJECT_JS = r"""(() => {
  if (window.__mudraInjected) return; window.__mudraInjected = true;
  const CTX = "__CTX__";
  const EP = "http://127.0.0.1:__PORT__/open";
  function openUrl(u) {
    if (!/^https?:/.test(u || "")) return;
    try { navigator.sendBeacon(EP, new Blob([JSON.stringify({url:u, ctx:CTX})], {type:"text/plain"})); } catch(e){}
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
    """控制接口（唯一执行点：窗口/实例/页面生命周期全部在此完成）：
    POST /open        {url, ctx?}  上下文实例活着 → 并入 --app 窗口；死了 → 新建实例（带 debug 端口）
    POST /add         {url, ctx?}  并入 --app 窗口（实例必须活着）
    POST /close_page  {query, ctx?}   关一个 tab（target 关闭事件触发统一收尾）
    POST /close_ctx   {ctx?}          关整个实例（kill → _mark_down 收尾）
    POST /ctx         {ctx}           切换当前上下文（situation 叶）
    ctx 缺省 = 当前上下文（state.current_context）。
    """
    def do_POST(self):
        mudrad = self.server.mudrad
        ok, err, out = True, None, {}
        try:
            n = int(self.headers.get("Content-Length") or 0)
            body = json.loads(self.rfile.read(n) or b"{}")
            if self.path == "/open":
                out = mudrad.ctl_open(body)
            elif self.path == "/add":
                out = mudrad.ctl_add(body)
            elif self.path == "/close_page":
                out = mudrad.ctl_close_page(body)
            elif self.path == "/close_ctx":
                out = mudrad.ctl_close_ctx(body)
            elif self.path == "/ctx":
                out = mudrad.ctl_ctx(body)
            else:
                ok, err = False, f"unknown endpoint {self.path}"
        except Exception as e:
            ok, err = False, str(e)
            print(f"[mudrad] ctl err: {e}")
        if ok:
            payload = json.dumps({"ok": True, **out}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("Access-Control-Allow-Origin", "*")
            self.end_headers()
            self.wfile.write(payload)
        else:
            payload = json.dumps({"ok": False, "err": err}).encode()
            self.send_response(400)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("Access-Control-Allow-Origin", "*")
            self.end_headers()
            self.wfile.write(payload)

    def log_message(self, *args):  # 静默
        pass


class Mudrad:
    def __init__(self) -> None:
        self._threads: dict[int, threading.Thread] = {}

    # ---- sqlite 同步 ----
    def _ctx_of_instance(self, conn, inst_id: int) -> str | None:
        r = conn.execute(
            "SELECT profile FROM instances WHERE id=?", (inst_id,)
        ).fetchone()
        return r["profile"] if r else None

    def _ctx_for_tab(self, tab_id: str | int | None) -> str | None:
        """按 CDP targetId（SK sender tab）反查其所属实例 → ctx（跨实例全量查）。"""
        if not tab_id:
            return None
        with db.connect() as conn:
            rows = conn.execute(
                "SELECT id, port, running FROM instances WHERE running=1"
            ).fetchall()
        for row in rows:
            try:
                with urllib.request.urlopen(
                    f"http://127.0.0.1:{row['port']}/json", timeout=2
                ) as r:
                    targets = json.loads(r.read())
                if any(t.get("id") == tab_id for t in targets):
                    with db.connect() as conn:
                        return self._ctx_of_instance(conn, row["id"])
            except Exception:
                continue
        return None

    # ---- 新窗口拦截：给页面注入脚本，把新 tab 交给 mudrad 拉 --app ----
    def _inject_page(self, port: int, target_id: str, ctx: str) -> None:
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
            src = _INJECT_JS.replace("__CTX__", ctx).replace("__PORT__", str(_INTERCEPT_PORT))
            cdp.call(ws, "Page.addScriptToEvaluateOnNewDocument", {"source": src})
            cdp.call(ws, "Runtime.evaluate", {"expression": src})  # 已加载页立即注入
            ws.close()
        except Exception as e:
            print(f"[mudrad] inject {ctx}/{target_id} err: {e}")

    # ---- 控制接口动词（唯一执行点：窗口 spawn / 进程 kill / 实例与页面生命周期写库都在这里）----
    def _inst_for_ctx(self, conn, ctx: str) -> dict | None:
        """上下文（situation 叶）对应实例；沿用旧实例行（profile 存叶名）复用 proxy/extensions。"""
        return conn.execute(
            "SELECT * FROM instances WHERE profile=? ORDER BY id DESC LIMIT 1",
            (ctx,),
        ).fetchone()

    def ctl_ctx(self, data: dict) -> dict:
        """切换当前上下文（situation 叶）。"""
        ctx = (data or {}).get("ctx") or ""
        with db.connect() as conn:
            if not db.set_context(conn, ctx):
                raise ValueError(f"not a situation leaf: {ctx!r}")
        ui._broadcast({"event": "context_changed", "ctx": ctx})
        print(f"[mudrad] ctx -> {ctx}")
        return {"ctx": ctx}

    def ctl_open(self, data: dict) -> dict:
        """上下文实例活着 → 并入 --app 窗口；死了 → 新建实例（debug 端口 + 复用 proxy/extensions）。

        ctx 缺省：先按 tabId（SK background 上报的 sender tab）反查所属实例 → ctx；
        再兜底当前上下文。
        """
        url = spawn.normalize_url((data or {}).get("url") or "")
        if not url:
            raise ValueError("need url")
        with db.connect() as conn:
            ctx = (data or {}).get("ctx")
            if not ctx:
                ctx = self._ctx_for_tab((data or {}).get("tabId")) or db.current_context(conn)
            inst = self._inst_for_ctx(conn, ctx)
        if inst and inst["running"] and self._pid_alive(inst["pid"]):
            # 并入已有实例
            spawn.launch(ctx, url, None,
                         proxy=inst["proxy"],
                         extensions=inst["extensions"].split(",") if inst["extensions"] else None)
            print(f"[mudrad] open -> --app joined: {url} (ctx {ctx})")
            return {"mode": "joined", "port": inst["port"], "ctx": ctx}
        # 新实例：复用旧行（proxy/extensions），否则新建
        port = spawn.free_port(9200)
        proxy = inst["proxy"] if inst else None
        ext = (inst["extensions"] if inst else None) or None
        pid, udir = spawn.launch(ctx, url, port,
                                 proxy=proxy,
                                 extensions=ext.split(",") if ext else None)
        with db.connect() as conn:
            if inst and inst["id"]:
                conn.execute(
                    "UPDATE instances SET port=?,pid=?,running=1 WHERE id=?",
                    (port, pid, inst["id"]),
                )
            else:
                conn.execute(
                    "INSERT INTO instances(profile,port,pid,running,proxy,extensions)"
                    " VALUES(?,?,?,1,?,?)",
                    (ctx, port, pid, proxy, ext),
                )
            conn.commit()
        print(f"[mudrad] open -> new instance :{port} pid {pid} (ctx {ctx})")
        self._apply_site_width(url, pid)
        return {"mode": "new", "port": port, "pid": pid, "ctx": ctx}

    def _apply_site_width(self, url: str, pid: int) -> None:
        """按页面 domain 查记忆列宽；等该实例窗口聚焦后应用（开窗副作用统一在后端）。"""
        domain = (url or "").split("//")[-1].split("/")[0]
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
        print(f"[mudrad] applied remembered width {w['proportion']:.3f} for {domain}")

    @staticmethod
    def _pid_alive(pid: int) -> bool:
        try:
            os.kill(pid, 0)
            return True
        except (ProcessLookupError, TypeError):
            return False
        except PermissionError:
            return os.path.exists(f"/proc/{pid}")

    def ctl_add(self, data: dict) -> dict:
        """并入已有实例的 --app 窗口（上下文实例必须活着，否则报错而非静默新开）。"""
        url = spawn.normalize_url((data or {}).get("url") or "")
        if not url:
            raise ValueError("need url")
        with db.connect() as conn:
            ctx = (data or {}).get("ctx") or db.current_context(conn)
            inst = self._inst_for_ctx(conn, ctx)
        if not inst or not inst["running"] or not self._pid_alive(inst["pid"]):
            raise ValueError(f"ctx {ctx!r} not running; use open first")
        ext = inst["extensions"] or None
        spawn.launch(ctx, url, None,
                     proxy=inst["proxy"],
                     extensions=ext.split(",") if ext else None)
        print(f"[mudrad] add -> --app joined: {url} (ctx {ctx})")
        return {"mode": "joined", "port": inst["port"], "ctx": ctx}

    def ctl_close_page(self, data: dict) -> dict:
        """关一个 tab：只关 CDP target，标 closed 由 targetDestroyed 事件统一走后端收尾。"""
        query = (data or {}).get("query") or ""
        if not query:
            raise ValueError("need query")
        with db.connect() as conn:
            ctx = (data or {}).get("ctx") or db.current_context(conn)
            inst = self._inst_for_ctx(conn, ctx)
            if not inst or not inst["running"] or not self._pid_alive(inst["pid"]):
                raise ValueError(f"ctx {ctx!r} not running")
            cur = conn.execute(
                "SELECT id,position,url,target_id FROM pages"
                " WHERE instance_id=? AND closed_at IS NULL AND url LIKE ?",
                (inst["id"], f"%{query}%"),
            ).fetchone()
            if not cur:
                raise ValueError(f"no open page in ctx {ctx!r} matching {query!r}")
        if cur["target_id"]:
            ctl.close_target(inst["port"], cur["target_id"])
        print(f"[mudrad] close page #{cur['position']} {cur['url']} (ctx {ctx})")
        return {"closed": cur["url"]}

    def ctl_close_ctx(self, data: dict) -> dict:
        """关整个上下文实例：kill 浏览器进程；running=0 / pages closed 由 _mark_down 统一收尾。"""
        with db.connect() as conn:
            ctx = (data or {}).get("ctx") or db.current_context(conn)
            inst = self._inst_for_ctx(conn, ctx)
        if not inst or not inst["running"] or not self._pid_alive(inst["pid"]):
            raise ValueError(f"ctx {ctx!r} not running")
        os.kill(inst["pid"], signal.SIGTERM)
        print(f"[mudrad] close ctx {ctx} (pid {inst['pid']})")
        return {"closed": ctx}

    def _open(self, data: dict) -> None:
        """拦截到的“新开 tab” → 在该上下文拉起 --app 窗口（并入已有实例）。"""
        url, ctx = (data or {}).get("url"), (data or {}).get("ctx")
        if not url or not ctx:
            return
        try:
            spawn.launch(ctx, spawn.normalize_url(url), None)
            print(f"[mudrad] new-window -> --app: {url} (ctx {ctx})")
        except Exception as e:
            print(f"[mudrad] open err: {e}")

    def _sync_infos(self, inst_id: int, infos: list[dict]) -> None:
        with db.connect() as conn:
            pages = [t for t in infos if t.get("type") == "page"]
            # CDP openerId = 由哪个页面打开（window.open / _blank / 新窗拦截），即父页
            t2id: dict[str, int] = {}
            for t in pages:
                tid = t["targetId"]
                row = conn.execute(
                    "SELECT id FROM pages WHERE instance_id=? AND target_id=?",
                    (inst_id, tid),
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
                        " WHERE instance_id=?",
                        (inst_id,),
                    ).fetchone()["p"]
                    conn.execute(
                        "INSERT OR IGNORE INTO pages"
                        "(instance_id,target_id,url,title,position,opened_at)"
                        " VALUES(?,?,?,?,?,?)",
                        (inst_id, tid, t.get("url", ""), t.get("title", ""), pos,
                         int(time.time())),
                    )
                pid = conn.execute(
                    "SELECT id FROM pages WHERE instance_id=? AND target_id=?",
                    (inst_id, tid),
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
                        "SELECT id FROM pages WHERE instance_id=? AND target_id=?",
                        (inst_id, oid),
                    ).fetchone()
                    p_id = r["id"] if r else None
                if p_id is not None:
                    conn.execute(
                        "UPDATE pages SET parent_id=? WHERE id=? AND parent_id IS NULL",
                        (p_id, cid),
                    )
            conn.commit()
        ui._broadcast({"event": "pages_changed", "instance_id": inst_id})

    def _close_target(self, inst_id: int, target_id: str) -> None:
        with db.connect() as conn:
            conn.execute(
                "UPDATE pages SET closed_at=? WHERE instance_id=? AND target_id=?"
                " AND closed_at IS NULL",
                (int(time.time()), inst_id, target_id),
            )
            conn.commit()
        ui._broadcast({"event": "pages_changed", "instance_id": inst_id})

    def _mark_down(self, inst_id: int) -> None:
        with db.connect() as conn:
            conn.execute("UPDATE instances SET running=0 WHERE id=?", (inst_id,))
            conn.execute(
                "UPDATE pages SET closed_at=? WHERE instance_id=? AND closed_at IS NULL",
                (int(time.time()), inst_id),
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
                ctx = self._ctx_of_instance(conn, iid)
            if ctx:
                for t in infos:
                    if t.get("type") == "page":
                        self._inject_page(port, t["targetId"], ctx)
            cdp.call(ws, "Target.setDiscoverTargets", {"discover": True})
            while True:
                msg = json.loads(ws.recv_text())
                method = msg.get("method")
                p = msg.get("params", {})
                if method == "Target.targetCreated":
                    info = p.get("targetInfo", {})
                    if info.get("type") == "page":
                        self._sync_infos(iid, [info])
                        if ctx:
                            self._inject_page(port, info["targetId"], ctx)
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