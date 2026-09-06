"""chromium instance launching / port allocation."""

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


# bundled extension: maintained in-repo (extension/); chromium loads it directly from the source directory (pure JS, zero build)
EXT_ROOT = db.DB.parent / "extensions"
REPO_FRONTEND = pathlib.Path(__file__).resolve().parent.parent / "frontend"
DEFAULT_EXTENSIONS = [str(REPO_FRONTEND)]  # extension root = frontend/; manifest.json at the root, shared libs in shared/


def normalize_url(url: str) -> str:
    """Add https:// when there is no scheme. Bare hosts/domains -> https; write the
    scheme yourself for http/IP."""
    if "://" not in url:
        return "https://" + url
    return url


def clear_extension_caches(name: str) -> None:
    """Clear chromium's caches of --load-extension directory files (ScriptCache/Code Cache/HTTP Cache).

    For extensions loaded straight from a source directory, chromium does not
    re-read changed files automatically (it would run the old SW/content scripts),
    so call this before every spawn during heavy development; the only cost is a
    cold start.
    """
    d = profile_dir(name) / "Default"
    for sub in ("Service Worker/ScriptCache", "Code Cache", "Cache",
                "Extension Scripts", "Extension Rules"):
        p = d / sub
        if p.exists():
            import shutil

            shutil.rmtree(p, ignore_errors=True)


def launch(name, url, port, *, proxy=None, extensions=None, dev_mode=False) -> tuple[int, str]:
    """Launch a chromium --app window; return (pid, profile_dir).

    port not None -> new instance (with remote-debugging); port=None -> join an
    existing instance (no debug port).
    proxy not None -> --proxy-server; extensions=None uses the default (mudra-keys),
    otherwise the given list.
    dev_mode -> clear extension caches before spawn (changes to extension sources
    become visible immediately; a cold start buys instant effect).
    """
    udir = profile_dir(name)
    udir.mkdir(parents=True, exist_ok=True)
    if dev_mode:
        clear_extension_caches(name)
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
        start_new_session=True,  # leave the parent process group + keep parent pipes free, so the child isn't killed or hung when the mudra CLI exits
    )
    return proc.pid, str(udir)