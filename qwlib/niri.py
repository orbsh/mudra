"""niri IPC：窗口列举 / 按 pid 映射 / 聚焦 / 移到工作区.

窗口↔进程映射：niri 窗口 JSON 的 `pid` 字段 == chromium 实例 main pid（已验证）。
socket 自动发现（NIRI_SOCKET 优先，否则 /run/user/<uid>/niri.wayland-*.sock）。
"""

from __future__ import annotations

import glob
import json
import os
import subprocess


def _socket() -> str | None:
    env = os.environ.get("NIRI_SOCKET")
    if env:
        return env
    cands = sorted(glob.glob(f"/run/user/{os.getuid()}/niri.wayland-*.sock"))
    return cands[-1] if cands else None


def _msg(args: list[str]) -> subprocess.CompletedProcess:
    sock = _socket()
    if not sock:
        raise RuntimeError("cannot find niri socket")
    env = {**os.environ, "NIRI_SOCKET": sock}
    return subprocess.run(["niri", "msg"] + args, capture_output=True, text=True, env=env)


def windows() -> list[dict]:
    r = _msg(["-j", "windows"])
    if r.returncode != 0:
        raise RuntimeError(f"niri windows: {r.stderr.strip()}")
    return json.loads(r.stdout)


def windows_for_pid(pid: int) -> list[dict]:
    return [w for w in windows() if w.get("pid") == pid]


def focus_window(wid: int) -> None:
    _msg(["action", "focus-window", "--id", str(wid)])


def move_focused_to_workspace(workspace: str) -> None:
    _msg(["action", "move-window-to-workspace", workspace])


def window_ids() -> set[int]:
    return {w["id"] for w in windows()}


def focused_window_id() -> int | None:
    for w in windows():
        if w.get("is_focused"):
            return w["id"]
    return None


def wait_for_new_window(before: set[int], timeout: float = 8.0) -> int | None:
    """等一个“开窗前不存在”的新窗口出现（后台打开用）。返回新窗口 id 或 None。"""
    import time
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        new = window_ids() - before
        if new:
            return sorted(new)[0]
        time.sleep(0.3)
    return None