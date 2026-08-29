"""WM 扩展接口：mudra 核心只经 WmExt 调用窗口/列宽操作，不感知具体 WM。

niri 实现走 `niri msg` IPC（socket 自动发现）。hyprland 未来按同一接口补实现。
启用清单由配置决定（默认 niri），接口与选型见 docs/EXTENSIONS.md。

已验证的 niri 事实：
- window JSON 含 pid -> windows_for_instance 按 chromium main pid 匹配。
- focus 必须 `focus-window --id <id>`（位置参数报错）。
- move `move-window-to-workspace <ref>`，数字 arg 是下标非 id；命名 ws 需 niri 声明。
- 列宽：`set-column-width <N%>`(百分比)或 `-N`(像素减)；不支持 1/2 分数。
- 列宽读取：聚焦窗 `layout.tile_size[0]`；output 逻辑宽 `focused-output.logical.width`。
"""

from __future__ import annotations

import abc
import glob
import json
import os
import subprocess
import time


class WmExt(abc.ABC):
    """窗口枚举/聚焦/移动/列宽——核心侧的 WM 抽象。"""

    @abc.abstractmethod
    def windows(self) -> list[dict]:
        """当前所有窗口。"""

    @abc.abstractmethod
    def windows_for_instance(self, pid: int) -> list[dict]:
        """属于某实例(=chromium main pid)的窗口。"""

    @abc.abstractmethod
    def window_ids(self) -> set[int]:
        """所有窗口 id 的集合(后台打开用集合差辨认新窗)。"""

    @abc.abstractmethod
    def focused_window_id(self) -> int | None:
        """当前聚焦窗口 id。"""

    @abc.abstractmethod
    def focused_window(self) -> dict | None:
        """当前聚焦窗口的完整 JSON。"""

    @abc.abstractmethod
    def focus_window(self, wid: int) -> None:
        """聚焦一个窗口(需 id 形态)。"""

    @abc.abstractmethod
    def move_to_workspace(self, ref: str) -> None:
        """把聚焦窗口移到工作区 ref(名字或下标)。"""

    @abc.abstractmethod
    def focus_workspace(self, ref: str) -> None:
        """切到/创建工作区 ref。"""

    @abc.abstractmethod
    def active_workspace(self) -> int:
        """当前活动工作区 idx（move-window-to-workspace 的 ref 接受 idx）。"""

    @abc.abstractmethod
    def workspace_of_window(self, wid: int) -> int | None:
        """窗口所在工作区 idx。"""

    @abc.abstractmethod
    def float_and_center(self) -> None:
        """把聚焦窗口转浮动并居中（管理面板用）。"""

    @abc.abstractmethod
    def wait_for_new_window(self, before: set[int], timeout: float = 8.0) -> int | None:
        """等一个(开窗前不存在)的新窗口出现，返回其 id(后台打开用)。"""

    @abc.abstractmethod
    def set_column_width(self, ratio: float) -> None:
        """把聚焦列设成输出宽度的 ratio(0..1)。"""

    @abc.abstractmethod
    def current_col_width(self) -> float:
        """读聚焦列当前宽度比例(0..1): tile_size[0] / output 逻辑宽。"""


def _niri_socket() -> str | None:
    env = os.environ.get("NIRI_SOCKET")
    if env:
        return env
    cands = sorted(glob.glob(f"/run/user/{os.getuid()}/niri.wayland-*.sock"))
    return cands[-1] if cands else None


def _niri_msg(args: list[str]) -> subprocess.CompletedProcess:
    sock = _niri_socket()
    if not sock:
        raise RuntimeError("cannot find niri socket")
    env = {**os.environ, "NIRI_SOCKET": sock}
    return subprocess.run(
        ["niri", "msg"] + args, capture_output=True, text=True, env=env
    )


class NiriExt(WmExt):
    """niri 实现：走 niri msg IPC。"""

    def windows(self) -> list[dict]:
        r = _niri_msg(["-j", "windows"])
        if r.returncode != 0:
            raise RuntimeError(f"niri windows: {r.stderr.strip()}")
        return json.loads(r.stdout)

    def windows_for_instance(self, pid: int) -> list[dict]:
        return [w for w in self.windows() if w.get("pid") == pid]

    def window_ids(self) -> set[int]:
        return {w["id"] for w in self.windows()}

    def focused_window_id(self) -> int | None:
        for w in self.windows():
            if w.get("is_focused"):
                return w["id"]
        return None

    def focused_window(self) -> dict | None:
        for w in self.windows():
            if w.get("is_focused"):
                return w
        return None

    def focus_window(self, wid: int) -> None:
        _niri_msg(["action", "focus-window", "--id", str(wid)])

    def move_to_workspace(self, ref: str) -> None:
        _niri_msg(["action", "move-window-to-workspace", ref])

    def focus_workspace(self, ref: str) -> None:
        _niri_msg(["action", "focus-workspace", ref])

    def float_and_center(self) -> None:
        _niri_msg(["action", "move-window-to-floating"])
        _niri_msg(["action", "center-window"])

    def active_workspace(self) -> int:
        """当前活动工作区的 idx（供 move-window-to-workspace 用，ref 接受 idx）。"""
        r = _niri_msg(["-j", "workspaces"])
        if r.returncode != 0:
            raise RuntimeError(f"niri workspaces: {r.stderr.strip()}")
        for w in json.loads(r.stdout):
            if w.get("is_focused"):
                return w["idx"]
        raise RuntimeError("no focused workspace")

    def workspace_of_window(self, wid: int) -> int | None:
        """窗口所在工作区的 idx；查不到返回 None。"""
        for w in self.windows():
            if w["id"] == wid:
                return w.get("workspace_id")
        return None

    def wait_for_new_window(self, before: set[int], timeout: float = 8.0) -> int | None:
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            new = self.window_ids() - before
            if new:
                return sorted(new)[0]
            time.sleep(0.3)
        return None

    def set_column_width(self, ratio: float) -> None:
        pct = max(1, min(99, round(ratio * 100)))
        _niri_msg(["action", "set-column-width", f"{pct}%"])

    def current_col_width(self) -> float:
        win = self.focused_window()
        if win is None:
            raise RuntimeError("no focused window")
        tile_w = win["layout"]["tile_size"][0]
        r = _niri_msg(["-j", "focused-output"])
        if r.returncode != 0:
            raise RuntimeError(f"niri focused-output: {r.stderr.strip()}")
        out_w = json.loads(r.stdout)["logical"]["width"]
        return tile_w / out_w


# 列宽档位（niri preset-column-widths 梯度）
SNAP_BANDS: tuple[tuple[float, str], ...] = (
    (1 / 3, "1/3"),
    (1 / 2, "1/2"),
    (2 / 3, "2/3"),
    (1.0, "1"),
)


def snap_column_width(ratio: float) -> tuple[float, str]:
    """把任意比例吸到最近档位，返回 (比例, 档位字符串)。"""
    band = min(SNAP_BANDS, key=lambda b: abs(b[0] - ratio))
    return band


_instance: WmExt | None = None


def get() -> WmExt:
    """当前启用的 WM backend(默认 niri；未来按配置选)。"""
    global _instance
    if _instance is None:
        _instance = NiriExt()
    return _instance