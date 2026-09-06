"""WM extension interface: the mudra core calls window/column-width operations only
through WmExt, without knowing the concrete WM.

The niri implementation uses the `niri msg` IPC (socket auto-discovered). hyprland will
be added later behind the same interface. The enabled set is config-driven (default niri);
see docs/EXTENSIONS.md for the interface and rationale.

Verified niri facts:
- window JSON includes pid -> windows_for_instance matches on the chromium main pid.
- focus requires `focus-window --id <id>` (positional args error out).
- move is `move-window-to-workspace <ref>`; the numeric arg is an index, not an id; named
  workspaces must be declared in the niri config.
- column width: `set-column-width <N%>` (percent) or `-N` (pixel reduction); 1/2 fractions unsupported.
- column width read: focused window `layout.tile_size[0]`; output logical width `focused-output.logical.width`.
"""

from __future__ import annotations

import abc
import glob
import json
import os
import subprocess
import time


class WmExt(abc.ABC):
    """Window enumeration/focus/move/column width -- the core-side WM abstraction."""

    @abc.abstractmethod
    def windows(self) -> list[dict]:
        """All current windows."""

    @abc.abstractmethod
    def windows_for_instance(self, pid: int) -> list[dict]:
        """Windows belonging to an instance (= chromium main pid)."""

    @abc.abstractmethod
    def window_ids(self) -> set[int]:
        """Set of all window ids (set-difference is used to spot new windows opened in background)."""

    @abc.abstractmethod
    def focused_window_id(self) -> int | None:
        """Id of the currently focused window."""

    @abc.abstractmethod
    def focused_window(self) -> dict | None:
        """Full JSON of the currently focused window."""

    @abc.abstractmethod
    def focus_window(self, wid: int) -> None:
        """Focus a window (expects an id-shaped ref)."""

    @abc.abstractmethod
    def move_to_workspace(self, ref: str) -> None:
        """Move the focused window to workspace ref (name or index)."""

    @abc.abstractmethod
    def focus_workspace(self, ref: str) -> None:
        """Switch to / create workspace ref."""

    @abc.abstractmethod
    def active_workspace(self) -> int:
        """Index of the active workspace (the move-window-to-workspace ref accepts an idx)."""

    @abc.abstractmethod
    def workspace_of_window(self, wid: int) -> int | None:
        """Index of the workspace a window is on."""

    @abc.abstractmethod
    def float_and_center(self) -> None:
        """Float the focused window and center it (used by the management panel)."""

    @abc.abstractmethod
    def wait_for_new_window(self, before: set[int], timeout: float = 8.0) -> int | None:
        """Wait for a new window (absent before the open) to appear; return its id (used for background opens)."""

    @abc.abstractmethod
    def set_column_width(self, ratio: float) -> None:
        """Set the focused column to ratio (0..1) of the output width."""

    @abc.abstractmethod
    def current_col_width(self) -> float:
        """Read the focused column's current width ratio (0..1): tile_size[0] / output logical width."""


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
    """niri implementation: via niri msg IPC."""

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
        """Index of the active workspace (for move-window-to-workspace; the ref accepts an idx)."""
        r = _niri_msg(["-j", "workspaces"])
        if r.returncode != 0:
            raise RuntimeError(f"niri workspaces: {r.stderr.strip()}")
        for w in json.loads(r.stdout):
            if w.get("is_focused"):
                return w["idx"]
        raise RuntimeError("no focused workspace")

    def workspace_of_window(self, wid: int) -> int | None:
        """Index of the workspace a window is on; None if not found."""
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


# Column-width snap bands (niri preset-column-widths gradient)
SNAP_BANDS: tuple[tuple[float, str], ...] = (
    (1 / 3, "1/3"),
    (1 / 2, "1/2"),
    (2 / 3, "2/3"),
    (1.0, "1"),
)


def snap_column_width(ratio: float) -> tuple[float, str]:
    """Snap any ratio to the nearest band; return (ratio, band string)."""
    band = min(SNAP_BANDS, key=lambda b: abs(b[0] - ratio))
    return band


_instance: WmExt | None = None


def get() -> WmExt:
    """The currently enabled WM backend (default niri; config-selectable in the future)."""
    global _instance
    if _instance is None:
        _instance = NiriExt()
    return _instance