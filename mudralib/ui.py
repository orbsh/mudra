"""mudra management panel -- standalone HTTP/WS service + floating window launcher.

The launcher's `` p `` handles only hot-path single actions; everything around the tag
forest (multi-select, batch assignment, rank-axis visualization) lives in this rich UI
panel. The panel and the launcher share the same sqlite and the `_focused_page()`
semantics layer -- no state fork.

- `launch()`: `mudra ui` -- starts HTTP (static dist) + WS, spawns chromium --app with
  the panel URL, floats and centers the window via niri. The panel window is launcher-agnostic.
- WS protocol (JSON, request/response): op is one of
    forest        -> tag forest (roots + children with full paths)
    pages [ctx]   -> a context's open pages (with tag sets and special-axis values)
    set_tags {page_id, tag_ids} -> replace the page's whole tag set (panel batch assignment)
    focus/close {page_id}
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import pathlib
import subprocess
import threading
import time

try:
    import websockets
    import websockets.server
except ImportError:  # the panel depends on websockets; the ui command errors out without it
    websockets = None

from . import ctl, db, spawn, wm

PANEL_PORT = int(os.environ.get("MUDRA_PANEL_PORT", "9299"))
# Zero-build: the panel static root is the frontend/ui/ source directory itself (source is the artifact, no vite)
FRONTEND = pathlib.Path(__file__).resolve().parent.parent / "frontend"
DIST = FRONTEND / "ui"   # static root = ui/; /shared/ is mounted from FRONTEND/shared (shared by extension and panel)


def ctl_open(url: str, ctx: str | None = None) -> None:
    """POST to the mudrad control endpoint /open -- window opens are executed exclusively by mudrad (ctx defaults to the current context)."""
    import urllib.request
    body = {"url": url}
    if ctx:
        body["ctx"] = ctx
    req = urllib.request.Request(
        "http://127.0.0.1:8899/open",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=10) as r:
        resp = json.loads(r.read())
    if not resp.get("ok"):
        raise RuntimeError(resp.get("err", "open failed"))


def ctl_ctx(ctx: str) -> None:
    """POST to mudrad /ctx -- switch the current context."""
    import urllib.request
    req = urllib.request.Request(
        "http://127.0.0.1:8899/ctx",
        data=json.dumps({"ctx": ctx}).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=10) as r:
        resp = json.loads(r.read())
    if not resp.get("ok"):
        raise RuntimeError(resp.get("err", "ctx switch failed"))


# ---------------------------------------------------------------- data queries
# rank axis -> emoji glyph (axis is determined by the root name)
ROOT_AXIS = {"importance": "★", "quality": "♥", "urgency": "🔥"}


def _forest(conn) -> list[dict]:
    """Tag forest: recursive tree of arbitrary depth. Each node carries a full path, rank axis, and children."""
    rows = conn.execute(
        "SELECT id,parent_id,name,alias,isolated,required,rank FROM tag"
        " WHERE deleted=0 ORDER BY id"
    ).fetchall()
    roots, children_of = {}, {}
    for r in rows:
        if r["parent_id"] == -1:
            roots[r["id"]] = {
                "id": r["id"], "name": r["name"], "alias": r["alias"],
                "root": True, "rank_axis": ROOT_AXIS.get(r["name"]),
                "children": [],
            }
        else:
            children_of.setdefault(r["parent_id"], []).append(r)

    def build(parent_id: int, prefix: str) -> list[dict]:
        out = []
        for r in children_of.get(parent_id, []):
            path = f"{prefix}::{r['name']}"
            node = {
                "id": r["id"], "name": r["name"], "alias": r["alias"],
                "path": path, "rank": r["rank"],
                "isolated": bool(r["isolated"]), "required": bool(r["required"]),
                "children": build(r["id"], path),
            }
            out.append(node)
        return out

    for rid, root in roots.items():
        root["children"] = build(rid, root["name"])
    return list(roots.values())


def _pages(conn, ctx: str) -> list[dict]:
    """Pages of a context (situation leaf -> instance): all undeleted pages shown (open + closed)."""
    rows = conn.execute(
        "SELECT p.id,p.url,p.title,p.position,p.target_id,p.parent_id,p.opened_at,p.closed_at"
        " FROM pages p JOIN instances i ON i.id=p.instance_id"
        " WHERE p.deleted_at IS NULL AND i.profile=?"
        " ORDER BY p.position", (ctx,),
    ).fetchall()
    result = []
    for p in rows:
        tags = [r["tag_id"] for r in conn.execute(
            "SELECT tag_id FROM page_tag WHERE page_id=?", (p["id"],)
        )]
        result.append({
            "id": p["id"], "url": p["url"], "title": p["title"] or p["url"],
            "position": p["position"], "tag_ids": tags,
            "target_id": p["target_id"], "parent_id": p["parent_id"],
            "opened_at": p["opened_at"],
            "closed": p["closed_at"] is not None,
        })
    return result


def _contexts(conn) -> list[str]:
    """Leaf-name list of the situation tree (top-of-panel context switcher)."""
    rows = conn.execute(
        "SELECT t.name FROM tag t WHERE t.parent_id="
        " (SELECT id FROM tag WHERE parent_id=-1 AND name='situation') ORDER BY t.id"
    ).fetchall()
    return [r["name"] for r in rows]


# ---------------------------------------------------------------- WS loop
def _reply(msg, **kw):
    """Build a response carrying the request id (the frontend matches pending promises by id)."""
    resp = {"ok": False, "err": "unknown"} if not kw.get("ok") else {}
    resp.update(kw)
    if msg.get("id"):
        resp["id"] = msg.get("id")
    return json.dumps(resp)


def _handle(msg: dict) -> str:
    """Handle a single WS request synchronously: DB connect + work + close, all in one thread.
    Returns the JSON string built by _reply; all blocking IO (CDP/niri) also runs in this
    thread so the asyncio loop never freezes."""
    op = msg.get("op")
    try:
        conn = db.connect()
        try:
            if op == "forest":
                return _reply(msg, ok=True, forest=_forest(conn),
                              contexts=_contexts(conn),
                              current=db.current_context(conn))
            elif op == "pages":
                return _reply(msg, ok=True,
                              pages=_pages(conn, msg.get("ctx") or db.current_context(conn)))
            elif op == "open":
                # New pages are NOT opened in the panel process -- always via the mudrad control API:
                # mudrad is the sole window opener; after opening it syncs the DB via CDP and broadcasts.
                ctl_open(msg.get("url", ""), msg.get("ctx"))
                return _reply(msg, ok=True)
            elif op == "set_ctx":
                ctl_ctx(msg.get("ctx", ""))
                return _reply(msg, ok=True)
            elif op == "set_tags":
                _set_tags(conn, msg.get("page_id"), msg.get("tag_ids", []))
                conn.commit()
                return _reply(msg, ok=True)
            elif op == "focus":
                _focus(conn, msg.get("page_id"))
                return _reply(msg, ok=True)
            elif op == "close":
                import mudralib.ops as ops
                return _reply(msg, ok=True, **ops.close_page(int(msg["page_id"])))
            elif op == "reopen":
                import mudralib.ops as ops
                return _reply(msg, ok=True, **ops.open_page(int(msg["page_id"])))
            elif op == "delete":
                import mudralib.ops as ops
                return _reply(msg, ok=True, **ops.delete_page(int(msg["page_id"])))
            elif op == "create_tag":
                nid = _create_tag(conn, msg.get("parent_id"), msg.get("name"))
                return _reply(msg, ok=True, id=nid)
            elif op == "config":
                from mudralib.config import load
                return _reply(msg, ok=True, config=load())
            elif op == "shot":
                data = _shot(conn, msg.get("page_id"))
                return _reply(msg, ok=True, data=data)
            return _reply(msg, err=f"unknown op {op}")
        except Exception as e:
            return _reply(msg, err=str(e))
        finally:
            conn.close()
    except Exception as e:
        return _reply(msg, err=f"db connect: {e}")


# ---------------------------------------------------------------- broadcast
# Panel client registry: pushes when the page set changes (mudrad CDP sync / open / close);
# the frontend does no polling. Writers may be any thread, delivered via loop.call_soon_threadsafe.
_CLIENTS: set = set()
_LOOP = None  # holds the asyncio loop (assigned in _start_services)


def _broadcast(event: dict) -> None:
    """Push an event to all panel WS clients (thread-safe)."""
    if _LOOP is None or not _CLIENTS:
        return
    payload = json.dumps(event)

    async def _push():
        dead = []
        for ws in list(_CLIENTS):
            try:
                await ws.send(payload)
            except Exception:
                dead.append(ws)
        for ws in dead:
            _CLIENTS.discard(ws)

    _LOOP.call_soon_threadsafe(
        lambda: asyncio.ensure_future(_push()))


async def _ws_handler(ws) -> None:
    _CLIENTS.add(ws)
    try:
        async for raw in ws:
            try:
                msg = json.loads(raw)
            except json.JSONDecodeError:
                continue
            try:
                async with asyncio.timeout(15):
                    reply = await asyncio.to_thread(_handle, msg)
                await ws.send(reply)
            except asyncio.TimeoutError:
                try:
                    await ws.send(_reply(msg, err="handler timeout"))
                except Exception:
                    pass
            except (asyncio.CancelledError, ConnectionError, RuntimeError, OSError):
                # Client disconnected / connection dead: clean up and exit so the handler does not sit in CLOSE-WAIT
                break
    finally:
        _CLIENTS.discard(ws)


def _set_tags(conn, page_id, tag_ids) -> None:
    page_id = int(page_id)
    ok = [int(x) for x in tag_ids if str(x).lstrip("-").isdigit()]
    valid = set()
    if ok:
        q = ",".join("?" * len(ok))
        valid = {r["id"] for r in conn.execute(
            f"SELECT id FROM tag WHERE deleted=0 AND id IN ({q})", tuple(ok)
        )}
    conn.execute("DELETE FROM page_tag WHERE page_id=?", (page_id,))
    for t in valid:
        conn.execute(
            "INSERT OR IGNORE INTO page_tag(page_id,tag_id) VALUES(?,?)",
            (page_id, t),
        )


def _focus(conn, page_id) -> None:
    """Focus a page: activate the CDP target + locate its niri window (by title/domain) and focus it."""
    row = conn.execute(
        "SELECT i.port,p.target_id,p.url,p.title FROM pages p"
        " JOIN instances i ON i.id=p.instance_id"
        " WHERE p.id=? AND p.closed_at IS NULL", (int(page_id),)
    ).fetchone()
    if not row or not row["port"] or not row["target_id"]:
        return
    port, tid = row["port"], row["target_id"]
    ctl.activate(port, tid)
    rid = conn.execute(
        "SELECT pid FROM instances WHERE port=?", (port,)
    ).fetchone()
    if not rid:
        return
    title = row["title"] or ""
    domain = (row["url"] or "").split("//")[-1].split("/")[0]
    mgr = wm.get()
    for w in mgr.windows_for_instance(rid["pid"]):
        wt = w.get("title") or ""
        if (title and wt == title) or (domain and domain in wt):
            mgr.focus_window(w["id"])
            return
    # Fallback: focus any window of the instance
    for w in mgr.windows_for_instance(rid["pid"]):
        mgr.focus_window(w["id"])
        break



def _create_tag(conn, parent_id, name) -> int:
    """Create a new tag node under parent_id (capsule + add-child). Returns the new node id."""
    name = (name or "").strip()
    if not name:
        raise ValueError("tag name required")
    pid = int(parent_id)
    exists = conn.execute(
        "SELECT id FROM tag WHERE parent_id=? AND name=? AND deleted=0",
        (pid, name),
    ).fetchone()
    if exists:
        return exists["id"]
    now = int(time.time())
    cur = conn.execute(
        "INSERT INTO tag(parent_id,name,note,hidden,created,updated)"
        " VALUES(?,?,NULL,0,?,?)", (pid, name, now, now),
    )
    conn.commit()
    return cur.lastrowid


def _shot(conn, page_id) -> str | None:
    """Capture a page window screenshot (CDP Page.captureScreenshot) -> base64 data URL."""
    row = conn.execute(
        "SELECT i.port,p.target_id FROM pages p"
        " JOIN instances i ON i.id=p.instance_id"
        " WHERE p.id=? AND p.closed_at IS NULL", (int(page_id),)
    ).fetchone()
    if not row or not row["port"] or not row["target_id"]:
        return None
    return ctl.screenshot(row["port"], row["target_id"])


# ---------------------------------------------------------------- services
def _serve_static() -> None:
    from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

    class H(SimpleHTTPRequestHandler):
        def __init__(self, *a, **kw):
            super().__init__(*a, directory=str(DIST), **kw)

        def translate_path(self, path):
            # /shared/* -> frontend/shared/ (cross-static-root shared library references)
            p = super().translate_path(path)
            if path.startswith("/shared/"):
                rel = pathlib.PurePosixPath(path).relative_to("/shared")
                p = str(FRONTEND / "shared" / rel)
            return p

        def log_message(self, *a):
            pass

        def end_headers(self):
            self.send_header("Access-Control-Allow-Origin", "*")
            self.send_header("Cache-Control", "no-cache")  # dist changes every build; disable cache to avoid hard refreshes
            super().end_headers()

        def do_GET(self):
            if self.path == "/" or self.path == "":
                self.path = "/index.html"
            return super().do_GET()

    srv = ThreadingHTTPServer(("127.0.0.1", PANEL_PORT), H)
    srv.serve_forever()


def _start_services() -> None:
    """Start HTTP (static) + WS in background threads. Idempotent: a busy port counts as already started."""
    if websockets is None:
        return
    t = threading.Thread(target=_serve_static, daemon=True)
    t.start()
    # websockets v16: serve is an async context manager running on a separate thread event loop
    global _LOOP

    def run():
        import asyncio

        async def main():
            global _LOOP
            _LOOP = asyncio.get_running_loop()
            async with websockets.serve(
                _ws_handler, "127.0.0.1", PANEL_PORT + 1,
                ping_interval=15,   # periodic ping per connection to detect dead ones
                ping_timeout=10,    # no ping response -> considered dead -> server reaps it (fixes CLOSE-WAIT pileup)
                max_queue=64,       # per-connection read queue cap; keeps slow clients from piling up unbounded
            ):
                await asyncio.Future()  # run forever

        asyncio.run(main())

    threading.Thread(target=run, daemon=True).start()


def _wait_port(port: int, timeout: float = 8.0) -> bool:
    import socket
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1):
                return True
        except OSError:
            time.sleep(0.2)
    return False


def _wait_ws(timeout: float = 8.0) -> bool:
    """Wait until the WS port can complete a handshake (no handshake noise, unlike _wait_port's bare TCP)."""
    import asyncio

    async def probe() -> bool:
        try:
            async with websockets.connect(
                f"ws://127.0.0.1:{PANEL_PORT + 1}/", open_timeout=1
            ):
                return True
        except Exception:
            return False

    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if asyncio.run(probe()):
            return True
        time.sleep(0.25)
    return False


def _ensure_mudrad() -> None:
    """Make sure the mudrad daemon is running (it hosts the panel /ui + /ws services); start it if not."""
    try:
        subprocess.run(["pgrep", "-f", "mudrad.py"], capture_output=True, check=True)
        return  # mudrad already running
    except subprocess.CalledProcessError:
        pass
    root = pathlib.Path(__file__).resolve().parent.parent
    subprocess.Popen(
        ["python3", str(root / "mudrad.py")],
        cwd=str(root),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    print("[mudra ui] started mudrad (holds panel services)")


def _panel_window_ids() -> list[int]:
    """Ids of open panel windows: the niri window pid's cmdline contains panel-profile (identified by process identity, not title)."""
    ids = []
    for w in wm.get().windows():
        pid = w.get("pid")
        try:
            cmdline = (pathlib.Path("/proc") / str(pid) / "cmdline").read_bytes()
        except OSError:
            continue
        if b"panel-profile" in cmdline:
            ids.append(w["id"])
    return ids


def launch(args: argparse.Namespace) -> int:
    """`mudra ui`: ensure mudrad (which hosts the panel services) is running; focus the existing panel window if any, else spawn."""
    if websockets is None:
        print("panel requires 'websockets' python package")
        return 1
    if not DIST.exists():
        print(f"panel frontend missing: {DIST}")
        return 1
    existing = _panel_window_ids()
    if existing:
        wm.get().focus_window(existing[0])
        print(f"mudra panel: focused existing window #{existing[0]}")
        return 0
    _ensure_mudrad()
    if not _wait_port(PANEL_PORT) or not _wait_ws():
        print(f"panel services not up on :{PANEL_PORT}(+/ws)")
        return 1
    url = f"http://127.0.0.1:{PANEL_PORT}/"
    udir = pathlib.Path.home() / ".local" / "share" / "mudra" / "panel-profile"
    udir.mkdir(parents=True, exist_ok=True)
    # The console also runs the mudra-keys extension (role console: open filters existing pages, etc.)
    ext_args = [f"--load-extension={e}" for e in spawn.DEFAULT_EXTENSIONS]
    proc = subprocess.Popen(
        ["chromium", f"--app={url}", f"--user-data-dir={udir}",
         "--no-first-run", "--no-default-browser-check", *ext_args],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    # Keep the window tiled (do not float it): the user can just switch workspaces or use normal window switching.
    time.sleep(1.5)
    print(f"mudra panel: {url} (pid {proc.pid})")
    return 0