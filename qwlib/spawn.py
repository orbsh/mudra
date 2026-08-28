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


def normalize_url(url: str) -> str:
    """无 scheme 则补 https://。bare host/域名 → https；要 http/IP 请自写 scheme。"""
    if "://" not in url:
        return "https://" + url
    return url


def launch(name: str, url: str, port: int) -> tuple[int, str]:
    """拉一个 chromium --app 实例；返回 (pid, profile_dir)."""
    udir = profile_dir(name)
    udir.mkdir(parents=True, exist_ok=True)
    cmd = [
        "chromium",
        f"--app={url}",
        f"--user-data-dir={udir}",
        f"--remote-debugging-port={port}",
        "--no-first-run",
        "--no-default-browser-check",
        "--enable-extensions",
    ]
    proc = subprocess.Popen(
        cmd,
        env={k: v for k, v in os.environ.items()},
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,  # 脱离父进程组 + 不占父管道，避免 qw CLI 退出时被连带或挂起
    )
    return proc.pid, str(udir)