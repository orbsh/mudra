"""chromium 实例拉起 / 端口分配."""

from __future__ import annotations

import os
import pathlib
import socket
import subprocess

from . import db

PROFILES = db.DB.parent / "profiles"


def free_port(start: int = 9200, span: int = 200) -> int:
    for port in range(start, start + span):
        with socket.socket() as s:
            try:
                s.bind(("127.0.0.1", port))
                return port
            except OSError:
                continue
    raise RuntimeError("no free port in range")


def profile_dir(name: str) -> pathlib.Path:
    return PROFILES / name


# 自带扩展：仓库内维护（extension/），chromium 直接从源码目录加载（纯 JS 零构建）
EXT_ROOT = db.DB.parent / "extensions"
REPO_EXTENSIONS = pathlib.Path(__file__).resolve().parent.parent / "extension"
DEFAULT_EXTENSIONS = [str(REPO_EXTENSIONS / "mudra-keys")]


def normalize_url(url: str) -> str:
    """无 scheme 则补 https://。bare host/域名 → https；要 http/IP 请自写 scheme。"""
    if "://" not in url:
        return "https://" + url
    return url


def launch(name, url, port, *, proxy=None, extensions=None) -> tuple[int, str]:
    """拉起一个 chromium --app 窗口；返回 (pid, profile_dir).

    port 非 None → 新实例（带 remote-debugging）；port=None → 并入已有实例（无 debug 端口）。
    proxy 非 None → --proxy-server；extensions=None 用默认(SurfingKeys)，否则按给定列表。
    """
    udir = profile_dir(name)
    udir.mkdir(parents=True, exist_ok=True)
    cmd = [
        "chromium",
        f"--app={url}",
        f"--user-data-dir={udir}",
        "--no-first-run",
        "--no-default-browser-check",
        "--enable-extensions",
    ]
    if port is not None:
        cmd.append(f"--remote-debugging-port={port}")
    if extensions is None:
        extensions = DEFAULT_EXTENSIONS
    if extensions:
        cmd.append("--load-extension=" + ",".join(extensions))
    if proxy:
        cmd.append(f"--proxy-server={proxy}")
    proc = subprocess.Popen(
        cmd,
        env={k: v for k, v in os.environ.items()},
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,  # 脱离父进程组 + 不占父管道，避免 mudra CLI 退出时被连带或挂起
    )
    return proc.pid, str(udir)