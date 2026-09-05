"""mudra 管理面板 — 独立 HTTP/WS 服务 + 拉起浮动窗口。

launcher 的 `` p `` 只管「热路径单动作」；tag 森林的一切（多选、批量指派、
评分轴可视化）交给这个富 UI 面板。面板与 launcher 共用同一 sqlite 与
`_focused_page()` 语义层，无状态分叉。

- `launch()`：`mudra ui` — 起 HTTP(静态 dist) + WS，spawn chromium --app
  载入面板 URL，niri 转浮动并居中。面板窗口与 launcher 类型无关。
- WS 协议（JSON，请求/响应）：op in
    forest        → tag 森林（根 + 完整路径 children）
    pages [ctx] → 某上下文开页（含其 tag 集与 special 轴值）
    set_tags {page_id, tag_ids} → 整组替换该页 tag（面板批量指派用）
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
except ImportError:  # 面板依赖 websockets；缺失则 ui 命令报错
    websockets = None

from . import ctl, db, spawn, wm

PANEL_PORT = int(os.environ.get("MUDRA_PANEL_PORT", "9299"))
DIST = pathlib.Path(__file__).resolve().parent.parent / "ui" / "dist"


def ctl_open(url: str, ctx: str | None = None) -> None:
    """POST mudrad 控制接口 /open —— 开窗口统一由 mudrad 执行（ctx 缺省=当前上下文）。"""
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
    """POST mudrad /ctx —— 切换当前上下文。"""
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


# ---------------------------------------------------------------- 数据查询
# rank 轴 → emoji 图元（根名定轴）
ROOT_AXIS = {"importance": "★", "quality": "♥", "urgency": "🔥"}


def _forest(conn) -> list[dict]:
    """tag 森林：任意深度递归树。每个节点带完整 path、rank 轴、children。"""
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
    """某上下文（situation 叶 → 实例）的打开页。"""
    rows = conn.execute(
        "SELECT p.id,p.url,p.title,p.position,p.target_id,p.parent_id,p.opened_at"
        " FROM pages p JOIN instances i ON i.id=p.instance_id"
        " WHERE p.closed_at IS NULL AND i.profile=?"
        " ORDER BY p.position", (ctx,)
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
        })
    return result


def _contexts(conn) -> list[str]:
    """situation 树的叶名列表（面板顶部上下文切换）。"""
    rows = conn.execute(
        "SELECT t.name FROM tag t WHERE t.parent_id="
        " (SELECT id FROM tag WHERE parent_id=-1 AND name='situation') ORDER BY t.id"
    ).fetchall()
    return [r["name"] for r in rows]


# ---------------------------------------------------------------- WS 循环
def _reply(msg, **kw):
    """构造带请求 id 的响应（前端按 id 匹配 pending promise）。"""
    resp = {"ok": False, "err": "unknown", **kw}
    if msg.get("id"):
        resp["id"] = msg.get("id")
    return json.dumps(resp)


def _handle(msg: dict) -> str:
    """同步处理单个 WS 请求：DB 连接 + 操作 + 关闭，全程在同一线程。
    返回 _reply 生成的 JSON 字符串；所有阻塞 IO（CDP/niri）也在此线程，不冻结 asyncio loop。"""
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
                # 开新 page 不在面板进程做——统一走 mudrad 控制接口：
                # mudrad 是唯一开窗口者，开完经 CDP 同步写库并广播。
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
                _close(conn, msg.get("page_id"))
                return _reply(msg, ok=True)
            elif op == "create_tag":
                nid = _create_tag(conn, msg.get("parent_id"), msg.get("name"))
                return _reply(msg, ok=True, id=nid)
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


# ---------------------------------------------------------------- 广播
# 面板客户端注册表：page 集变化（mudrad CDP 同步 / open / close）时推送，
# 前端不做轮询。写侧任意线程，通过 loop.call_soon_threadsafe 投递。
_CLIENTS: set = set()
_LOOP = None  # asyncio loop 持有（_start_services 里赋值）


def _broadcast(event: dict) -> None:
    """向所有面板 WS 客户端推事件（线程安全）。"""
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
                # 客户端断开/连接失效：清理退出，不让 handler 卡在 CLOSE-WAIT
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
    """聚焦某页：CDP 激活目标 + 定位其 niri 窗口（按 title/域名）并聚焦。"""
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
    # 退化：聚焦该实例任一窗口
    for w in mgr.windows_for_instance(rid["pid"]):
        mgr.focus_window(w["id"])
        break


def _close(conn, page_id) -> None:
    row = conn.execute(
        "SELECT i.port,p.target_id FROM pages p"
        " JOIN instances i ON i.id=p.instance_id"
        " WHERE p.id=?", (int(page_id),)
    ).fetchone()
    conn.execute("DELETE FROM pages WHERE id=?", (int(page_id),))
    conn.commit()
    if row and row["port"] and row["target_id"]:
        ctl.close_target(row["port"], row["target_id"])


def _create_tag(conn, parent_id, name) -> int:
    """在 parent_id 下新建一个 tag 节点（胶囊 + 添加子级）。返回新节点 id。"""
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
    """抓页窗口截图（CDP Page.captureScreenshot）→ base64 data URL。"""
    row = conn.execute(
        "SELECT i.port,p.target_id FROM pages p"
        " JOIN instances i ON i.id=p.instance_id"
        " WHERE p.id=? AND p.closed_at IS NULL", (int(page_id),)
    ).fetchone()
    if not row or not row["port"] or not row["target_id"]:
        return None
    return ctl.screenshot(row["port"], row["target_id"])


# ---------------------------------------------------------------- 服务
def _serve_static() -> None:
    from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

    class H(SimpleHTTPRequestHandler):
        def __init__(self, *a, **kw):
            super().__init__(*a, directory=str(DIST), **kw)

        def log_message(self, *a):
            pass

        def end_headers(self):
            self.send_header("Access-Control-Allow-Origin", "*")
            self.send_header("Cache-Control", "no-cache")  # dist 每次构建都换，禁缓存免硬刷新
            super().end_headers()

        def do_GET(self):
            if self.path == "/" or self.path == "":
                self.path = "/index.html"
            return super().do_GET()

    srv = ThreadingHTTPServer(("127.0.0.1", PANEL_PORT), H)
    srv.serve_forever()


def _start_services() -> None:
    """在后台线程里启动 HTTP(静态) + WS。幂等：端口被占视为已启动。"""
    if websockets is None:
        return
    t = threading.Thread(target=_serve_static, daemon=True)
    t.start()
    # websockets v16: serve 是 async context manager，跑在独立线程 event loop
    global _LOOP

    def run():
        import asyncio

        async def main():
            global _LOOP
            _LOOP = asyncio.get_running_loop()
            async with websockets.serve(
                _ws_handler, "127.0.0.1", PANEL_PORT + 1,
                ping_interval=15,   # 每个连接周期性 ping，探测死连接
                ping_timeout=10,    # ping 无响应 → 判定死连接 → 服务端回收（治 CLOSE-WAIT 堆积）
                max_queue=64,       # 单连接读队列上限，防慢客户端无限堆积
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
    """等 WS 端口可做握手（不产生 handshake 噪音，区别于 _wait_port 的裸 TCP）。"""
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
    """确保 mudrad 守护进程在跑（它持有面板 /ui + /ws 服务）。没跑就拉起来。"""
    try:
        subprocess.run(["pgrep", "-f", "mudrad.py"], capture_output=True, check=True)
        return  # 已有 mudrad
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
    """已打开的面板窗口 id：niri 窗口 pid 的 cmdline 含 panel-profile（按进程身份识别，不依赖 title）。"""
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
    """`mudra ui`：确保 mudrad（持面板服务）在跑；已有面板窗口则直接聚焦，否则 spawn。"""
    if websockets is None:
        print("panel requires 'websockets' python package")
        return 1
    if not DIST.exists():
        print(f"panel frontend not built: {DIST} (cd ui && npm run build)")
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
    # 控制台也跑 mudra-keys 扩展（角色 console：open 过滤现有 page 等）
    ext_args = [f"--load-extension={e}" for e in spawn.DEFAULT_EXTENSIONS]
    proc = subprocess.Popen(
        ["chromium", f"--app={url}", f"--user-data-dir={udir}",
         "--no-first-run", "--no-default-browser-check", *ext_args],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    # 固定窗（平铺），不自转浮动：用户直接切 workspace / 正常窗口切换过去即可。
    time.sleep(1.5)
    print(f"mudra panel: {url} (pid {proc.pid})")
    return 0