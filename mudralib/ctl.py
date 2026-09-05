"""CDP 控制动词：导航 / 历史 / 聚焦。

连实例的 browser-level WS 或具体 page WS，发命令。端口从 sqlite 的 instance 取
（会话名 → instance.port）。
"""

from __future__ import annotations

import urllib.request
import json

from . import cdp, db


def _port(name: str) -> int | None:
    with db.connect() as conn:
        r = conn.execute(
            "SELECT port FROM instances WHERE profile=? AND running=1",
            (name,),
        ).fetchone()
        return r["port"] if r else None


def _browser(port: int) -> cdp.WsClient:
    return cdp.WsClient(cdp.get_browser_ws(port), timeout=8)


def _page_ws(port: int, target_id: str) -> str:
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/json", timeout=5) as resp:
        for t in json.loads(resp.read()):
            if t.get("id") == target_id and t.get("type") == "page":
                return t["webSocketDebuggerUrl"]
    raise ValueError(f"no page target {target_id}")


def list_pages(port: int) -> list[dict]:
    ws = _browser(port)
    r = cdp.call(ws, "Target.getTargets")
    ws.close()
    return [t for t in r["result"]["targetInfos"] if t.get("type") == "page"]


def current_target_id(name: str) -> tuple[int, str] | None:
    """该上下文最靠右（position 最大）且仍打开的页面 target_id。"""
    with db.connect() as conn:
        r = conn.execute(
            "SELECT i.port, p.target_id, p.url FROM instances i"
            " JOIN pages p ON p.instance_id=i.id"
            " WHERE i.profile=? AND i.running=1 AND p.closed_at IS NULL"
            " ORDER BY p.position DESC LIMIT 1",
            (name,),
        ).fetchone()
        if r and r["target_id"]:
            return r["port"], r["target_id"]
    return None


def find(port: int, query: str) -> list[dict]:
    q = query.lower()
    return [
        t for t in list_pages(port)
        if q in (t.get("url", "") + " " + t.get("title", "")).lower()
    ]


def activate(port: int, target_id: str) -> None:
    ws = _browser(port)
    cdp.call(ws, "Target.activateTarget", {"targetId": target_id})
    ws.close()


def close_target(port: int, target_id: str) -> None:
    ws = _browser(port)
    cdp.call(ws, "Target.closeTarget", {"targetId": target_id})
    ws.close()


def goto(port: int, target_id: str, url: str) -> None:
    ws = cdp.WsClient(_page_ws(port, target_id), timeout=8)
    cdp.call(ws, "Page.navigate", {"url": url})
    ws.close()


def back(port: int, target_id: str) -> None:
    _history_step(port, target_id, -1)


def forward(port: int, target_id: str) -> None:
    _history_step(port, target_id, 1)


def _history_step(port: int, target_id: str, delta: int) -> None:
    ws = cdp.WsClient(_page_ws(port, target_id), timeout=8)
    idx = cdp.call(ws, "Page.getNavigationHistory")["result"]["currentIndex"]
    cdp.call(ws, "Page.navigateToHistoryEntry", {"entryId": idx + delta})
    ws.close()


def reload(port: int, target_id: str) -> None:
    ws = cdp.WsClient(_page_ws(port, target_id), timeout=8)
    cdp.call(ws, "Page.reload", {"ignoreCache": False})
    ws.close()


def screenshot(port: int, target_id: str) -> str | None:
    """抓目标页内容截图，返回 base64 PNG（'data:image/png;base64,...'）；失败返回 None。"""
    try:
        ws = cdp.WsClient(_page_ws(port, target_id), timeout=10)
        r = cdp.call(ws, "Page.captureScreenshot", {"format": "png"})
        ws.close()
        data = r["result"].get("data")
        return f"data:image/png;base64,{data}" if data else None
    except Exception as e:
        print(f"[ctl] screenshot err: {e}")
        return None