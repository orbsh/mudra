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

from mudralib import cdp, ctl, db, ops, spawn, ui, wm

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
        print(f"[mudrad] POST {self.path}")
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
            elif self.path == "/tag":
                out = mudrad.ctl_tag(body)
            elif self.path == "/tags":
                out = mudrad.ctl_tags(body)
            elif self.path == "/pages":
                out = mudrad.ctl_pages(body)
            elif self.path == "/focus_page":
                out = mudrad.ctl_focus_page(body)
            elif self.path == "/ctx_status":
                out = mudrad.ctl_ctx_status(body)
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

    # ---- tab→ctx 反查：语义在 ops.ctx_for_tab / ops.ctx_for_url ----
    def _ctx_for_tab(self, tab_id: str | int | None, url: str | None = None) -> str | None:
        return ops.ctx_for_tab(tab_id, url)

    def _ctx_for_url(self, url: str | None) -> str | None:
        return ops.ctx_for_url(url)

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
        return db.instance_for_context(conn, ctx)

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
                         extensions=inst["extensions"].split(",") if inst["extensions"] else None,
                         dev_mode=db.get_state(conn, "dev_mode") == "1")
            print(f"[mudrad] open -> --app joined: {url} (ctx {ctx})")
            return {"mode": "joined", "port": inst["port"], "ctx": ctx}
        # 新实例：复用旧行（proxy/extensions），否则新建
        port = spawn.free_port(9200)
        proxy = inst["proxy"] if inst else None
        ext = (inst["extensions"] if inst else None) or None
        pid, udir = spawn.launch(ctx, url, port,
                                 proxy=proxy,
                                 extensions=ext.split(",") if ext else None,
                                 dev_mode=db.get_state(conn, "dev_mode") == "1")
        with db.connect() as conn:
            db.instance_launch_started(conn, inst["id"] if inst and inst["id"] else None,
                                       ctx, port, pid, proxy, ext)
        print(f"[mudrad] open -> new instance :{port} pid {pid} (ctx {ctx})")
        self._apply_site_width(url, pid)
        return {"mode": "new", "port": port, "pid": pid, "ctx": ctx}

    def _apply_site_width(self, url: str, pid: int) -> None:
        """按页面 domain 查记忆列宽；等该实例窗口聚焦后应用（开窗副作用统一在后端）。"""
        domain = (url or "").split("//")[-1].split("/")[0]
        if not domain:
            return
        with db.connect() as conn:
            w = db.site_width(conn, domain)
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
            cur = db.page_open_by_url_substring(conn, inst["id"], query)
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

    def ctl_ctx_status(self, data: dict) -> dict:
        """状态栏数据源：tabId → (ctx, 页面 tags)。编排收敛到 ops.page_info_for_tab。"""
        d = data or {}
        return ops.page_info_for_tab(d.get("tabId"), d.get("url"))

    @staticmethod
    def _is_console(url: str | None) -> bool:
        """console ui（总控面板）页面判定——角色属于后端数据，前端不猜。"""
        if not url:
            return False
        from mudralib.ui import PANEL_PORT
        return url.startswith(f"http://127.0.0.1:{PANEL_PORT}/")

    def ctl_tag(self, data: dict) -> dict:
        """给页面打/摘 tag（toggle）。语义在 ops.tag_page。"""
        d = data or {}
        return ops.tag_page(d.get("tabId"), d.get("url"), d.get("tag"))

    def ctl_tags(self, data: dict) -> dict:
        """tag 树读取：{parent?: 名称} → 该父下子树。语义在 ops.tags_children。"""
        return {"tags": ops.tags_children((data or {}).get("parent"))}

    def ctl_pages(self, data: dict) -> dict:
        """打开页列表（跨 ctx）：扩展 pages 命令的数据源。语义在 ops.list_open_pages。"""
        return {"pages": ops.list_open_pages((data or {}).get("ctx"))}

    def ctl_focus_page(self, data: dict) -> dict:
        """聚焦某页（page_id）：CDP 激活 + niri 前台。语义在 ops.focus_page。"""
        page_id = int((data or {}).get("page_id") or 0)
        return ops.focus_page(page_id)

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
        """CDP targetInfo → pages 表（写路径在 db.page_upsert_by_target，父子回填仅在首次）。"""
        with db.connect() as conn:
            pages = [t for t in infos if t.get("type") == "page"]
            t2id: dict[str, int] = {}
            for t in pages:
                t2id[t["targetId"]] = db.page_upsert_by_target(
                    conn, inst_id, t["targetId"],
                    t.get("url", ""), t.get("title", ""),
                )
            # 回填父子关系：子页 parent_id = 打开它的页（CDP openerId）
            for t in pages:
                oid = t.get("openerId")
                if not oid:
                    continue
                cid = t2id.get(t["targetId"])
                if cid is None:
                    continue
                p_id = t2id.get(oid)
                if p_id is None:  # opener 不在本批，回查库
                    p_id = db.page_id_by_target(conn, inst_id, oid)
                if p_id is not None:
                    db.page_set_parent_once(conn, cid, p_id)
            conn.commit()
        ui._broadcast({"event": "pages_changed", "instance_id": inst_id})

    def _close_target(self, inst_id: int, target_id: str) -> None:
        with db.connect() as conn:
            db.page_close_target(conn, inst_id, target_id, int(time.time()))
            conn.commit()
        ui._broadcast({"event": "pages_changed", "instance_id": inst_id})

    def _mark_down(self, inst_id: int) -> None:
        with db.connect() as conn:
            db.instance_set_running(conn, inst_id, 0)
            db.pages_close_all(conn, inst_id, int(time.time()))
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
                ctx = db.instance_ctx(conn, iid)
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
                insts = db.running_instances(conn)
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