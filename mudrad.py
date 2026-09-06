#!/usr/bin/env python3
"""mudrad — the mudra daemon.

Long-running: for each instance with running=1, connect to its browser-level CDP
WebSocket, subscribe to Target.setDiscoverTargets events (targetCreated /
infoChanged / Destroyed), and sync page open/close/URL changes into the sqlite
pages table in real time (CDP is the single source of truth).

Usage: mudrad start | stop | status  (P1 supports foreground run only;
start/stop to be added later)
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

# New-window interception: the page-injected script forwards window.open /
# target=_blank here, and mudrad spawns an --app window.
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
    """Singleton lock: only one mudrad may run at a time (flock auto-releases on exit)."""
    db.DB.parent.mkdir(parents=True, exist_ok=True)
    fh = open(db.DB.parent / "mudrad.lock", "w")
    try:
        fcntl.flock(fh, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print("[mudrad] another daemon is already running")
        sys.exit(1)
        return
    globals()["_LOCK_FH"] = fh  # keep a reference so GC never releases the lock


class _InterceptHandler(BaseHTTPRequestHandler):
    """Control API (single execution point: all window/instance/page lifecycle
    transitions happen here):
    POST /open        {url, ctx?}  instance alive -> join as an --app window; dead -> new instance (with debug port)
    POST /add         {url, ctx?}  join as an --app window (instance must be alive)
    POST /close_page  {query, ctx?}   close one tab (target-destroyed event triggers the unified teardown)
    POST /close_ctx   {ctx?}          close the whole instance (kill -> _mark_down teardown)
    POST /ctx         {ctx}           switch the current context (situation leaf)
    ctx omitted = current context (state.current_context).
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

    def log_message(self, *args):  # silence
        pass

    def do_GET(self):
        """GET /config: config file (~/.config/mudra/config.kdl) -> JSON. Fetched by the extension at startup."""
        if self.path == "/config":
            from mudralib import config as config_mod
            try:
                payload = json.dumps({"ok": True, "config": config_mod.load()}).encode()
                self.send_response(200)
            except Exception as e:
                payload = json.dumps({"ok": False, "err": f"config: {e}"}).encode()
                self.send_response(500)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("Access-Control-Allow-Origin", "*")
            self.end_headers()
            self.wfile.write(payload)
        else:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()


class Mudrad:
    def __init__(self) -> None:
        self._threads: dict[int, threading.Thread] = {}

    # ---- tab->ctx reverse lookup: semantics live in ops.ctx_for_tab / ops.ctx_for_url ----
    def _ctx_for_tab(self, tab_id: str | int | None, url: str | None = None) -> str | None:
        return ops.ctx_for_tab(tab_id, url)

    def _ctx_for_url(self, url: str | None) -> str | None:
        return ops.ctx_for_url(url)

    # ---- New-window interception: inject the script into pages so new tabs get handed to mudrad to spawn --app ----
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
            cdp.call(ws, "Runtime.evaluate", {"expression": src})  # inject immediately into already-loaded pages
            ws.close()
        except Exception as e:
            print(f"[mudrad] inject {ctx}/{target_id} err: {e}")

    # ---- Control-API verbs (single execution point: window spawn / process kill / instance & page lifecycle DB writes all happen here) ----
    def _inst_for_ctx(self, conn, ctx: str) -> dict | None:
        """Instance for a context (situation leaf); reuse the existing instance row
        (profile stores the leaf name) so proxy/extensions carry over."""
        return db.instance_for_context(conn, ctx)

    def ctl_ctx(self, data: dict) -> dict:
        """Switch the current context (situation leaf)."""
        ctx = (data or {}).get("ctx") or ""
        with db.connect() as conn:
            if not db.set_context(conn, ctx):
                raise ValueError(f"not a situation leaf: {ctx!r}")
        ui._broadcast({"event": "context_changed", "ctx": ctx})
        print(f"[mudrad] ctx -> {ctx}")
        return {"ctx": ctx}

    def ctl_open(self, data: dict) -> dict:
        """Instance alive -> join as an --app window; dead -> new instance (debug port +
        reused proxy/extensions).

        ctx fallback order: first reverse-lookup the owning instance by tabId (the
        sender tab reported by the SK background), then fall back to the current context.
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
            # join an existing instance
            spawn.launch(ctx, url, None,
                         proxy=inst["proxy"],
                         extensions=inst["extensions"].split(",") if inst["extensions"] else None,
                         dev_mode=db.get_state(conn, "dev_mode") == "1")
            print(f"[mudrad] open -> --app joined: {url} (ctx {ctx})")
            return {"mode": "joined", "port": inst["port"], "ctx": ctx}
        # new instance: reuse the old row (proxy/extensions) if present, else create
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
        """Look up the remembered column width by page domain; apply it once the
        instance window gains focus (window-opening side effects live in the backend)."""
        domain = (url or "").split("//")[-1].split("/")[0]
        if not domain:
            return
        with db.connect() as conn:
            w = db.site_width(conn, domain)
        if not w:
            return
        mgr = wm.get()
        for _ in range(30):  # wait for the new window to land and take focus (new windows grab focus)
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
        """Join an existing instance as an --app window (the context instance must be
        alive; error out rather than silently spawning a new one)."""
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
        """Close one tab: only close the CDP target; marking it closed is handled by the
        backend teardown on the targetDestroyed event."""
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
        """Close the whole context instance: kill the browser process; running=0 / pages
        closed are handled by the unified _mark_down teardown."""
        with db.connect() as conn:
            ctx = (data or {}).get("ctx") or db.current_context(conn)
            inst = self._inst_for_ctx(conn, ctx)
        if not inst or not inst["running"] or not self._pid_alive(inst["pid"]):
            raise ValueError(f"ctx {ctx!r} not running")
        os.kill(inst["pid"], signal.SIGTERM)
        print(f"[mudrad] close ctx {ctx} (pid {inst['pid']})")
        return {"closed": ctx}

    def ctl_ctx_status(self, data: dict) -> dict:
        """Status-bar data source: tabId -> (ctx, page tags). Orchestration is consolidated in ops.page_info_for_tab."""
        d = data or {}
        return ops.page_info_for_tab(d.get("tabId"), d.get("url"))

    @staticmethod
    def _is_console(url: str | None) -> bool:
        """Detect the console UI (master panel) page — that role is backend data; the frontend never guesses."""
        if not url:
            return False
        from mudralib.ui import PANEL_PORT
        return url.startswith(f"http://127.0.0.1:{PANEL_PORT}/")

    def ctl_tag(self, data: dict) -> dict:
        """Add/remove a tag on a page (toggle). Semantics live in ops.tag_page."""
        d = data or {}
        return ops.tag_page(d.get("tabId"), d.get("url"), d.get("tag"))

    def ctl_tags(self, data: dict) -> dict:
        """Read the tag tree: {parent?: name} -> subtree under that parent. Semantics live in ops.tags_children."""
        return {"tags": ops.tags_children((data or {}).get("parent"))}

    def ctl_pages(self, data: dict) -> dict:
        """Open-page list (across contexts): data source for the extension's pages command. Semantics live in ops.list_open_pages."""
        return {"pages": ops.list_open_pages((data or {}).get("ctx"))}

    def ctl_focus_page(self, data: dict) -> dict:
        """Focus a page (page_id): CDP activation + bring the niri window forward. Semantics live in ops.focus_page."""
        page_id = int((data or {}).get("page_id") or 0)
        return ops.focus_page(page_id)

    def _open(self, data: dict) -> None:
        """An intercepted "new tab" -> spawn an --app window in that context (joining an existing instance)."""
        url, ctx = (data or {}).get("url"), (data or {}).get("ctx")
        if not url or not ctx:
            return
        try:
            spawn.launch(ctx, spawn.normalize_url(url), None)
            print(f"[mudrad] new-window -> --app: {url} (ctx {ctx})")
        except Exception as e:
            print(f"[mudrad] open err: {e}")

    def _sync_infos(self, inst_id: int, infos: list[dict]) -> None:
        """CDP targetInfo -> pages table (write path is db.page_upsert_by_target; parent backfill happens only on first sight)."""
        with db.connect() as conn:
            pages = [t for t in infos if t.get("type") == "page"]
            t2id: dict[str, int] = {}
            for t in pages:
                t2id[t["targetId"]] = db.page_upsert_by_target(
                    conn, inst_id, t["targetId"],
                    t.get("url", ""), t.get("title", ""),
                )
            # backfill parent-child: child's parent_id = the page that opened it (CDP openerId)
            for t in pages:
                oid = t.get("openerId")
                if not oid:
                    continue
                cid = t2id.get(t["targetId"])
                if cid is None:
                    continue
                p_id = t2id.get(oid)
                if p_id is None:  # opener not in this batch; look it up in the DB
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

    # ---- per-instance watcher ----
    def _watch(self, inst: dict) -> None:
        iid, port = inst["id"], inst["port"]
        # race guard: chromium was just spawned, so the port may not be bound yet.
        # Retry for ~10s; only mark down if it truly never comes up.
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
            # baseline: full sync once, then enable discovery; also inject the
            # new-window interception script into every page
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
        except Exception as e:  # connection lost/crashed = instance is gone
            print(f"[mudrad] instance {iid} disconnected: {e}")
        finally:
            self._mark_down(iid)
            self._threads.pop(iid, None)

    # ---- main loop ----
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