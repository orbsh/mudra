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
            "SELECT i.port FROM instances i JOIN sessions s ON s.instance_id=i.id"
            " WHERE s.name=? AND i.running=1",
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
    """该会话最靠右（position 最大）且仍打开的页面 target_id。"""
    with db.connect() as conn:
        r = conn.execute(
            "SELECT i.port, p.target_id, p.url FROM sessions s"
            " JOIN instances i ON i.id=s.instance_id"
            " JOIN pages p ON p.session_id=s.id"
            " WHERE s.name=? AND i.running=1 AND p.closed_at IS NULL"
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