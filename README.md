# qw — browser session manager

Manage **chromium `--app` browser sessions** from a terminal, driven over CDP and
backed by sqlite. One Chromium instance per session (workspace-isolated); sessions
group pages; a `qwd` daemon keeps live page state in sync.

> Build status: **P0–P3 implemented** (sessions, spawn, realtime CDP sync,
> navigation verbs, niri window mapping + move). Proxy/extensions (P4) and
> column-width memory (P5) are next. See `PLAN.md`.

## Requirements

- `python3` (stdlib only — the WS client is self-written)
- `chromium` (on PATH)
- `niri` (for `move` / window mapping)

## Quick start

```bash
# 1. start the daemon (keeps sqlite pages in sync with real windows)
python3 qwd.py run

# 2. create / open a session (spawns a chromium --app instance + first page)
python3 qw.py open <name> <url>

# 3. list sessions, or a session's pages
python3 qw.py ls
python3 qw.py ls <name>
python3 qw.py ls <name> --filter news     # filter by url/title substring
```

## Environment

- To launch Chromium **windows** the shell running `qw` needs the Wayland env:
  `export WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000`
- The niri socket is auto-discovered (`NIRI_SOCKET` env or `/run/user/<uid>/niri.wayland-*.sock`).

## Commands (current)

| command | what it does |
|---|---|
| `qw.py new <name>` | create a session (no instance) |
| `qw.py open <name> <url>` | spawn an instance for the session and open the first page |
| `qw.py add <name> <url> [--bg]` | add a page to a running session (`--bg` = keep focus) |
| `qw.py close <name> [query]` | close a whole session, or one open tab (url filter) |
| `qw.py ls [name] [-f FILTER]` | list sessions, or a session's pages (URL/title filter) |
| `qw.py targets <name>` | list live page targets (CDP) |
| `qw.py focus <name> <query>` | find a page by url/title and bring it to the front |
| `qw.py goto <name> <url>` | navigate the current page of a session |
| `qw.py back / forward / reload <name>` | history back/forward, reload |
| `qw.py move <name> <workspace>` | move the session's windows to a workspace (niri) |
| `qw.py use [name]` | set (creates if missing) / show current session (`*` in `ls`) |
| `qw.py mode [session\|tab\|flip\|op]` | walker-mode state machine (default: show) |
| `qwd.py run` | daemon: connect each running instance, sync Target→sqlite |

## How sessions/instances work

- Each session owns **one Chromium instance** (its own profile dir, debug port, PID)
  under `~/.local/share/qw/profiles/<name>/`.
- State lives in sqlite at `~/.local/share/qw/qw.sqlite`
  (`instances`, `sessions`, `pages`, `site_widths`).
- The `qwd` daemon connects to each running instance's browser CDP WebSocket,
  subscribes `Target.*` events, and live-updates `pages` (open / url / title / close).
  It also marks an instance `running=0` (and closes its pages) when it exits.

## Notes / design points

- `--app` gives a minimal webview (no omnibox/tab strip); SurfingKeys can be
  preloaded via `--load-extension` (verified).
- `qw move` maps a session's windows by **PID** (niri window JSON exposes `pid`).
- Named per-session workspaces (`web:<name>`) need niri to declare named
  workspaces (deferred); `move` currently takes a workspace index/name.

## Roadmap

P4 proxy + extension list · P5 column-width memory · P6 new-window interception ·
P7 walker/niri integration as **extension modules** (see `docs/EXTENSIONS.md`).