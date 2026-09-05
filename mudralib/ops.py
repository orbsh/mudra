"""共享动作层：focus / tag / pages 语义的唯一实现。

mudrad（HTTP handler）与 mudra CLI 各自的入口都收敛到这里，消除两份重复实现：
- focus 语义 = CDP 激活 target + niri 窗口带到前台（标题/域名匹配，fallback 首窗口；
  niri 不可用不阻塞 CDP activate）。
- tag 语义 = tab/url 反查打开中的 page → page_tag toggle，广播 pages_changed。
- pages 语义 = 跨 ctx 打开页列表（扩展 :o / 面板批量共用）。

原则：动作层只编排（db.py 的函数 + CDP/wm），不写 SQL、不解析 HTTP。
"""

from __future__ import annotations

from mudralib import cdp, db, wm
from mudralib.ui import _broadcast


def focus_page(page_id: int) -> dict:
    """聚焦某页：CDP 激活 target + niri 窗口带到前台才算"切换"。"""
    with db.connect() as conn:
        row = db.page_by_id_joined(conn, page_id)
    if not row or not row["port"] or not row["target_id"]:
        raise ValueError(f"page {page_id} not found or instance down")
    ctl_activate(row["port"], row["target_id"])
    _focus_instance_window(row["pid"], title=row["title"] or "", url=row["url"] or "")
    return {"focused": page_id}


def focus_ctx_query(ctx: str, query: str) -> dict:
    """CLI focus 语义：在 ctx 实例里按标题/URL 模糊找页 → focus_page。"""
    with db.connect() as conn:
        from mudralib.db import instance_for_context  # 避免环：直接用 db
        inst = instance_for_context(conn, ctx)
        page_ids = [r["id"] for r in db.pages_open(conn, ctx)]
    if not inst or not inst["port"]:
        raise ValueError(f"ctx {ctx!r} not running")
    hits = _find_pages(inst["port"], query)
    if not hits:
        raise ValueError(f"no page matching {query!r}")
    # 命中 targetId → 映射 pages 行 id，统一走 focus_page（niri 匹配逻辑不重复写）
    with db.connect() as conn:
        page_id = db.page_id_by_target(conn, inst["id"], hits[0]["targetId"])
    if page_id is None:
        raise ValueError("page not recorded (sync pending?)")
    out = focus_page(page_id)
    print(f"focused: {hits[0].get('title') or hits[0].get('url')}")
    return out


def tag_page(tab_id: str | int | None, url: str | None, tag_name: str | None) -> dict:
    """给页面打/摘 tag（toggle）。tabId/url → ctx → 打开中的 page。"""
    if not tag_name:
        raise ValueError("need tag")
    ctx = ctx_for_tab(tab_id, url) if tab_id else None
    if not ctx or not url:
        raise ValueError("need tabId and url to resolve page")
    with db.connect() as conn:
        t = db.tag_id_by_name(conn, tag_name)
        if not t:
            raise ValueError(f"tag not found: {tag_name}")
        inst = conn.execute(
            "SELECT id FROM instances WHERE profile=? ORDER BY id DESC LIMIT 1", (ctx,)
        ).fetchone()
        if not inst:
            raise ValueError(f"no instance for ctx {ctx}")
        page = db.latest_open_page_by_url(conn, inst["id"], _url_prefix(url))
        if not page:
            raise ValueError("page not open in this ctx")
        action = db.page_tag_toggle(conn, page["id"], t)
        conn.commit()
    _broadcast({"event": "pages_changed"})
    return {"tag": tag_name, "action": action}


def list_open_pages(ctx: str | None = None) -> list[dict]:
    """打开页列表（跨 ctx）。"""
    with db.connect() as conn:
        return [dict(r) for r in db.pages_open(conn, ctx)]


def tags_children(parent: str | None = None) -> list[str]:
    """tag 树读取：父下子节点名（parent=None → 根层）。"""
    with db.connect() as conn:
        return [r["name"] for r in db.tag_children(conn, parent)]


def ctx_for_tab(tab_id: str | int | None, url: str | None = None) -> str | None:
    """反查 tab 所属实例 → ctx。

    tab_id 可能是 CDP targetId（拦截注入用）或 chrome 数字 tabId（扩展 sender）——
    前者按 /json 匹配实例，后者匹配不到时退回按 URL 落库匹配。
    """
    if not tab_id:
        return None
    for row in _running_instances():
        if _port_has_target(row["port"], str(tab_id)):
            with db.connect() as conn:
                return db.instance_ctx(conn, row["id"])
    return ctx_for_url(url)


def ctx_for_url(url: str | None) -> str | None:
    if not url:
        return None
    with db.connect() as conn:
        return db.page_ctx_for_url(conn, _url_prefix(url))


def page_info_for_tab(tab_id: str | int | None, url: str | None) -> dict:
    """状态栏数据源：tabId/url → (ctx, 页面 tags, role)。"""
    ctx = ctx_for_tab(tab_id, url)
    if not ctx:
        ctx = ctx_for_url(url)
    tags: list[str] = []
    if ctx and url:
        with db.connect() as conn:
            inst = conn.execute(
                "SELECT id FROM instances WHERE profile=? ORDER BY id DESC LIMIT 1",
                (ctx,),
            ).fetchone()
            if inst:
                page = db.latest_open_page_by_url(conn, inst["id"], _url_prefix(url))
                if page:
                    tags = db.page_tag_names(conn, page["id"])
    from mudralib.ui import PANEL_PORT
    role = "console" if (url or "").startswith(f"http://127.0.0.1:{PANEL_PORT}/") else "page"
    return {"ctx": ctx, "tags": tags, "role": role}


# ---- 内部：CDP / wm 薄包装（方便测试替换） ----

def _url_prefix(url: str) -> str:
    return (url.split("#")[0] + "%")[:200]


def _running_instances() -> list:
    with db.connect() as conn:
        return db.running_instances(conn)


def _port_has_target(port: int, target_id: str) -> bool:
    try:
        import json
        import urllib.request
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/json", timeout=2) as r:
            targets = json.loads(r.read())
        return any(t.get("id") == target_id for t in targets)
    except Exception:
        return False


def _find_pages(port: int, query: str) -> list[dict]:
    from mudralib import ctl
    return ctl.find(port, query)


def ctl_activate(port: int, target_id: str) -> None:
    from mudralib import ctl
    ctl.activate(port, target_id)


def _focus_instance_window(pid: int | None, title: str = "", url: str = "") -> None:
    """niri 窗口带到前台：标题精确或域名包含匹配，fallback 实例首窗口。"""
    if not pid:
        return
    try:
        domain = url.split("//")[-1].split("/")[0]
        mgr = wm.get()
        for w in mgr.windows_for_instance(pid):
            wt = w.get("title") or ""
            if (title and wt == title) or (domain and domain in wt):
                mgr.focus_window(w["id"])
                return
        wins = mgr.windows_for_instance(pid)
        if wins:
            mgr.focus_window(wins[0]["id"])
    except Exception:
        pass  # niri 不可用不阻塞 CDP activate
