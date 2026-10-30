#!/usr/bin/env python3
"""bw-fill — an Induction guest that answers mudra's `mudra:fill_password` event
from a Bitwarden vault through `rbw`.

The wire is hand-written (JSON lines on stdin/stdout; `induction-protocol` is the
spec, not a library this script needs):

    <- {"type": "initialize", "protocol": 1}
    -> {"type": "hello", "name": "bwfill", "schema": {"on": ["mudra:fill_password"]}}
    <- {"id": N, "kind": "call", "event": "mudra:fill_password",
        "args": {"event_id": N, "at": <ms>, "snapshot": {"url": …}}}
    -> {"id": N, "result": {"ok": true, "user": …, "password": …}}

The row id is the call id, so the answer lands on whoever asked.

It runs on demand: mudra starts it when a fill is requested and reaps it after
its idle window. Nothing here keeps state across answers, so that is free — every
request re-reads the vault, which is also what makes it correct after an unlock in
another terminal.

Matching is by URI host, never by name: `rbw get github` answers "multiple
entries found" for this vault (five of them), and a wrong password pasted into a
login form is worse than an error message. Ambiguity is narrowed by the username
the page already carries, and left as an error when it is still ambiguous.
"""
import json
import subprocess
import sys
from urllib.parse import urlsplit

GUEST = "bwfill"
EVENT = "mudra:fill_password"
RBW_TIMEOUT = 10  # seconds; the host's own ask budget is 15

# What a handler declares. Empty object per event: the reserved place for
# tier/deadline, unpopulated today (induction ADR-0001).
SCHEMA = {"on": [EVENT]}


def log(msg):
    print(f"[{GUEST}] {msg}", file=sys.stderr, flush=True)


def send(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def rbw(*args):
    """Run rbw with no tty: a prompt here has nobody to answer it, so a locked
    vault must fail loudly instead of blocking the guest."""
    return subprocess.run(
        ["rbw", *args],
        capture_output=True,
        text=True,
        timeout=RBW_TIMEOUT,
        stdin=subprocess.DEVNULL,
    )


def host_of(url):
    try:
        host = (urlsplit(url).hostname or "").lower()
    except ValueError:
        return ""
    return host[4:] if host.startswith("www.") else host


def uri_hosts(entry):
    out = []
    for uri in entry.get("uris") or []:
        parsed = uri if "://" in uri else f"https://{uri}"
        try:
            h = (urlsplit(parsed).hostname or "").lower()
        except ValueError:
            continue
        if h:
            out.append(h[4:] if h.startswith("www.") else h)
    return out


def matches(entries, host):
    """Entries whose URI host covers the page's host. One direction only: an
    entry for `example.com` covers `mail.example.com`, never the reverse."""
    if not host:
        return []
    found = []
    for e in entries:
        for h in uri_hosts(e):
            if h == host or host.endswith("." + h):
                found.append(e)
                break
    return found


def narrow(candidates, user_hint):
    if not user_hint or len(candidates) <= 1:
        return candidates
    hint = user_hint.strip().lower()
    exact = [e for e in candidates if (e.get("user") or "").lower() == hint]
    if exact:
        return exact
    prefix = [e for e in candidates if (e.get("user") or "").lower().startswith(hint)]
    return prefix or candidates


def fill(snapshot):
    url = snapshot.get("url") or ""
    host = host_of(url)
    if not host:
        return {"ok": False, "err": f"no host in {url!r}"}

    listed = rbw("list", "--raw")
    if listed.returncode != 0:
        err = (listed.stderr or "").strip() or f"rbw list exit {listed.returncode}"
        # rbw says this when the vault is locked, and there is no one to prompt
        return {"ok": False, "err": f"vault: {err}"}
    try:
        entries = json.loads(listed.stdout or "[]")
    except json.JSONDecodeError as e:
        return {"ok": False, "err": f"rbw list is not JSON: {e}"}

    candidates = narrow(matches(entries, host), snapshot.get("user_hint") or "")
    if not candidates:
        return {"ok": False, "err": f"no bitwarden entry for {host}"}
    if len(candidates) > 1:
        users = ", ".join((e.get("user") or e.get("name") or "?") for e in candidates[:6])
        return {
            "ok": False,
            "err": f"{len(candidates)} entries for {host}: {users}",
            "matched": len(candidates),
        }

    entry = candidates[0]
    got = rbw("get", "--raw", entry["id"])
    if got.returncode != 0:
        err = (got.stderr or "").strip() or f"rbw get exit {got.returncode}"
        return {"ok": False, "err": f"vault: {err}"}
    try:
        data = json.loads(got.stdout).get("data") or {}
    except json.JSONDecodeError as e:
        return {"ok": False, "err": f"rbw get is not JSON: {e}"}
    password = data.get("password") or ""
    if not password:
        return {"ok": False, "err": f"entry {entry.get('name')!r} has no password"}
    return {
        "ok": True,
        "user": data.get("username") or entry.get("user") or "",
        "password": password,
        "name": entry.get("name") or "",
        "matched": len(candidates),
    }


def handle(event, args):
    if event == "interface_schema":
        return {"receives": {EVENT: {}}, "wildcard_receives": []}
    if event != EVENT:
        return {"ok": False, "err": f"{GUEST} does not handle {event}"}
    # The host wraps an event row as {event_id, at, snapshot}: the snapshot is the
    # caller's args, which is where the url lives.
    snapshot = (args or {}).get("snapshot")
    if not isinstance(snapshot, dict):
        snapshot = args if isinstance(args, dict) else {}
    try:
        return fill(snapshot)
    except subprocess.TimeoutExpired:
        return {"ok": False, "err": f"rbw timed out after {RBW_TIMEOUT}s"}
    except Exception as e:  # never die on one bad request: the host would wait 15s
        return {"ok": False, "err": f"{type(e).__name__}: {e}"}


def main():
    send({"type": "hello", "name": GUEST, "schema": SCHEMA})
    log("hello sent")
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            frame = json.loads(line)
        except json.JSONDecodeError:
            log(f"undecodable frame: {line[:120]}")
            continue
        if frame.get("type") == "initialize":
            continue  # the hello above is the answer, already sent
        if frame.get("kind") != "call":
            continue
        send({"id": frame.get("id"), "result": handle(frame.get("event"), frame.get("args"))})


if __name__ == "__main__":
    main()