# qw — browser session manager: implementation plan

> **Status**: planning
> **Date**: 2026-08-28
> **Design source**: `~/.hermes/wiki/keyboard-wm-browser-mode.md` (the authoritative design; keep in sync)

## 1. Goal

A minimal-UI, keyboard-driven, externally-controlled browser **usage mode** for Niri (or any tiling WM):

- Each page is a `chromium --app` window (no omnibox, no tab strip → maximal webview).
- One Chromium **instance per session/workspace**; several sessions/instances may run concurrently.
- Sessions and pages live in **sqlite**; a CLI (`qw`) + daemon (`qwd`) drive everything over **CDP**.
- **SurfingKeys** is preloaded via `--load-extension`; proxy and an **extension list** are configurable per instance.
- Links that would open a new window are **intercepted** by an injected script and opened as a new `--app` window instead of a chrome-default window.

Why (motivation): qutebrowser's modal input-method problem (`#3444`) → seek a non-modal, minimal-UI, fully external-controlled browser. See the wiki doc.

## 2. Decisions (locked)

- `--app` per page (no embedded tab strip). Multi-tab = the user opens plain Chromium themselves.
- **Path B concurrent**: one Chromium instance per session/workspace; multiple may run.
- Python (CLI + daemon). Sole non-stdlib dep: `websocket-client` (CDP). sqlite via stdlib.
- Command name `qw`; daemon `qwd`.
- SurfingKeys loaded via `--load-extension` (verified working in chromium 151).
- New-window opening routed through an injected content script → local HTTP server → new `--app`.
- **WM-control constraint**: the whole mode depends on fine-grained WM control via interface/CLI (move / focus / set-column-width / workspace routing). niri & hyprland satisfy; **cosmic-de does not yet**.

## 3. Architecture

```
  user/script ─▶ qw (CLI) ─▶ qwd (daemon) ──CDP WS──▶ chromium --app (1 per session)
                  │              │                          │
                  │ read        sqlite events (Target.*)    │ user-data-dir per session
                  ▼              ▼                          │
                sqlite ◀──────── qwd ◀────── window/target ◀┘
                 (~/.local/share/qw/qw.sqlite)
```

- **`qw`** (CLI): quick commands; reads sqlite; forwards control commands to `qwd`.
- **`qwd`** (daemon): holds a CDP WebSocket to each **running** instance; subscribes `Target.targetCreated/infoChanged/Destroyed` → real-time sqlite sync (satisfies "update the list on close"); forwards control commands; spawns/stops instances.
- **chromium**: `--app=<url>`, dedicated `--user-data-dir` per session, dynamic `--remote-debugging-port`, `--proxy-server`, `--load-extension=<list>`, uniform title prefix.

## 4. Data model

```sql
instances(id, profile TEXT, port INT, pid INT, running INT,
         proxy TEXT, extensions TEXT)            -- 1 session ↔ 1 instance
sessions(id, name UNIQUE, workspace TEXT, instance_id FK,
         created_at, last_opened_at)
pages(id, session_id FK CASCADE, target_id, url, title,
      position INT, opened_at INT, closed_at INT NULL)
site_widths(site TEXT PRIMARY KEY, proportion REAL)   -- site → column-width ratio (0..1)
state(key TEXT PRIMARY KEY, value TEXT)                -- 全局状态: current_session, walker_mode
```

- `pages.target_id` = CDP target of the window; `url` live-updated via CDP `infoChanged` → URL-filterable.
- `instances.proxy` / `.extensions` = per-process proxy and preload extension list.

## 5. Components & mechanisms

### CLI verbs (v1)
```
qw new <name>               create session (+ workspace)
qw open <name> [url...]     switch workspace, spawn instance, rebuild pages
qw add <name> <url>         add a page to current/session session
qw move <name> <workspace>  move a process's whole window set to another workspace
qw ls [name]                list sessions/pages (URL-filterable)
qw close <name>             close the instance (windows + workspace collapse)
qw rm <name>                delete session (+ its profile/workspace)
qw goto <url> | back | forward | reload
qw focus <page>
qw yank                 copy focused window URL to clipboard
qw quit                     close all instances
qw daemon start|stop|status
```

### CDP control
- Launch: `chromium --app=<url> --remote-debugging-port=<dyn> --user-data-dir=<session profile>`
  `--proxy-server=... --load-extension=<dir>,... --no-first-run` (SurfingKeys etc.)
- Open/close page: `Target.createTarget` / `Page.close` → Target events sync sqlite.
- Navigate: `Page.navigate` / `reload` / `getNavigationHistory` / `navigateToHistoryEntry`.
- **Find page by URL and activate**: `Target.getTargets` (url/title/type) → match → `Target.activateTarget`. Works on error pages.

### session = workspace (concurrent)
- Each session binds a niri workspace (`web:<name>`); `qw open` = `focus-workspace` + spawn + rebuild.
- Windows land on the workspace via niri window-rule (title prefix / app_id) + `focus-workspace`.
- Workspaces can hold mixed windows from several processes; **moving a process's windows to a workspace is the "separate" operation**.

### window↔process map & batch move
- qw records which window belongs to which process (CDP target ↔ niri window by title/app_id).
- `qw move <name> <workspace>` iterates the process's windows → `move-window-to-workspace <ws>` (verified).

### URL record & filter
- CDP `infoChanged` live-updates `pages.url`; list/filter by URL substring in `qw ls` / walker.

### column-width memory (Win+R)
- niri column width **is** an output-ratio (`preset-column-widths`, `switch-preset-column-width`/Mod+R cycles 1/3 1/2 2/3 1).
- niri exposes **no direct getter** → capture `proportion = window_width / output_width`, snap to nearest band, store with the focused URL's domain in `site_widths`.
- On open: focus the column → `set-column-width <proportion>` (fraction syntax `1/2` to confirm at impl).

### proxy & extension list
- per-instance `proxy` → `--proxy-server`; `extensions` → `--load-extension=<built dirs>`.
- **Verified**: `--app` + `--load-extension` loads SurfingKeys in an `--app` window (extension id
  `fbnpkpganphpmhekgfkanhdpombfanpj`; built unpacked at `dist/production/chrome/`). Building SurfingKeys:
  `npm install` (official registry, direct net) then webpack `build:prod`.

### new-window interception (links / window.open)
- `--app` opens `_blank`/`window.open` as a **chrome-default window** (inherent to `--app`; CDP cannot restyle it).
- Fix: injected content script (`inject.js`) overrides `window.open` and capture-phase `a[target=_blank]`
  clicks → preventDefault → Image beacon to local server `http://127.0.0.1:<port>/open?url=...` →
  server spawns a new `--app` window in the same profile.
- Caveat: error/built-in pages aren't injectable (rarely open windows). niri window-rule can catch leaks.
- **Cascade**: the interception must be (re)injected on **every** new page target (`Target.targetCreated` → `Page.addScriptToEvaluateOnNewDocument`); otherwise a window that `qwd` itself spawns carries no script, and a further `_blank` there reverts to a chrome-default (non-`--app`) window.

### Extensions（WM / launcher 对接，可插拔）

`qw` 核心是 **WM / launcher 无关**的：只管理 session/instance/page + CDP。环境相关集成做成
**扩展模块**，经统一接口加载、按需启用：

- **WM 扩展**（`move` / focus / 列宽 / workspace 路由 / 窗口映射）：核心只经 `WmExt` 接口调用，
  由具体实现提供（`niri` 现成；`hyprland` 未来可加）。niri 实现走 `niri msg` IPC。
- **Launcher 扩展**（菜单提供 + 动作处理）：核心暴露 session/page 数据，`LauncherExt` 把数据
  渲染成 launcher（walker）菜单项并处理选中动作。
- **启用清单**由配置决定（如 `extensions: [niri, walker]`）；核心不感知具体实现。

对接方式、接口定义、walker/niri 具体接法见 `docs/EXTENSIONS.md`。

## 6. Verified vs pending (facts)

**Verified**:
- `--app` = no omnibox/tab strip (niri screenshot).
- CDP lists/attaches `--app` page targets.
- niri `move-window-to-workspace` / `move-column-to-workspace` / `set-column-width` exist.
- niri has no Alt+Tab window filter (no such config; default has no Alt+Tab binding).
- `--load-extension` loads SurfingKeys into `--app` (chromium 151).

**Pending** (confirm at impl):
- CDP control of error pages (`chrome-error`).
- Deterministic per-instance window landing on its workspace.
- Current column-width read field in `niri msg -j windows`; `set-column-width` fraction grammar.
- `--load-extension` behavior with a second extension (e.g. Bitwarden) reliably.

## 7. Implementation phases

- **P0 scaffold**: `~/world/qw` repo layout; sqlite schema init; `qw new` / `qw ls`.
- **P1 core loop**: `qw open` spawns instance; `qwd` connects CDP; Target events → sqlite (open/close/url live-sync).
- **P2 CDP verbs**: `goto / back / forward / reload / focus <page>`; find-by-URL activate.
- **P3 workspace & move**: window↔process map; `qw move`; per-instance workspace routing; URL filter in `ls`.
- **P4 proxy & extensions**: `--proxy-server`; `--load-extension` list; build+ship SurfingKeys bundle.
- **P5 column-width memory**: Win+R capture → `site_widths`; auto-apply on open.
- **P6 new-window interception**: `inject.js` + local `/open` server in `qwd`.
- **P7 extension modules**: build `WmExt` (niri move / column-width / workspace routing /
  window mapping) + `LauncherExt` (walker session/page menus + hotkeys) behind the pluggable
  interface (§5 Extensions); migrate `qw move` and focus onto the `WmExt` interface.

Each phase ends with a working, verifiable slice (per 迭代闭环). Cross-cutting: verify niri/CDP APIs against reality before wiring each phase (the "fabricated API" rule).