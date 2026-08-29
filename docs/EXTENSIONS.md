# qw extensions — WM & launcher integration

`qw` core is **WM/launcher-agnostic**: it manages sessions, instances, pages and CDP.
Environment-specific integrations are **pluggable extension modules** loaded behind a
small interface, so the same core works on any WM / launcher that can satisfy it
(hard constraint in the wiki: WM must offer interface/CLI fine-grained control —
niri & hyprland qualify; cosmic-de does not yet).

This document specifies the *integration approach* (P7). **P7a is live**: the `WmExt` interface
+ `NiriExt` backend (`qwlib/wm.py`) are implemented and `qw move` / `add` run on it. P7b
(`LauncherExt` walker menus) is pending the real walker provider API. P0–P6 implement core + the
niri bits; the interface below is the target shape.

## Interface: WM extension (`WmExt`)

Core calls these operations on an enabled WM module (it never touches `niri` directly):

| op | purpose |
|---|---|
| `windows_for_instance(pid) -> [window]` | which niri windows belong to an instance |
| `focus_window(id)` | focus a window by id |
| `move_to_workspace(ref)` | move focused (or given) window to a workspace |
| `focus_workspace(ref)` | switch to / create a workspace |
| `set_column_width(ratio)` | set the focused column to a fraction |
| `current_col_width() -> ratio` | read back the column width (Win+R memory) |

### niri implementation

- **Transport**: `niri msg` IPC. Socket auto-discovered: `NIRI_SOCKET` env, else
  `/run/user/<uid>/niri.wayland-*.sock`.
- **Window JSON** (`niri msg -j windows`) exposes `id`, `pid`, `title`, `app_id`,
  `workspace_id`, `is_floating`, `is_focused` — this drives `windows_for_instance`
  (match `pid == instance.pid`, verified).
- **Focus** requires the flag form: `niri msg action focus-window --id <ID>`.
- **Move**: `niri msg action move-window-to-workspace <ref>` where `<ref>` is a
  workspace **index or name** (numeric args are indexes, not ids — verified).
- **Column width**: `set-column-width <N%>` (percent; the `1/2` fraction grammar **errors**).
  Read back = focused window `layout.tile_size[0]` ÷ focused-output `logical.width`, snapped to a
  band {1/3, 1/2, 2/3, 1} (both verified). See PLAN §5.
- **Per-session named workspaces** (`web:<name>`) need niri to declare named
  workspaces (config change, deferred); until then `move` takes an index/name ref.

### hyprland (future)
Same `WmExt` interface, implemented over hyprctl IPC. Include once core consumers
(`qw move`, focus, column-width) have fully migrated onto the interface.

## Interface: Launcher extension (`LauncherExt`)

Core exposes clean data; the launcher module renders it and dispatches selections back
to core actions:

- `sessions() -> [{name, workspace, n_pages, running}]`
- `pages(session) -> [{title, url, closed}]`
- actions: `open_session(name)`, `focus_page(query)`, `open_url(url)`,
  `move_session(name, ws)`, `new_session(name, url)`

### walker implementation

- **Menu source**: a launcher plugin/provider that lists sessions + their live pages
  (from `pages` / `Target.*` data) as entries; typing filters by url/title (reuse the
  `ls --filter` logic).
- **Selection**: dispatch to core actions (spawn/activate via CDP, move via `WmExt`).
- **Alt+Tab replacement**: niri has no native window-switcher filter (verified), so the
  browser's Alt+Tab is bound to a walker menu whose entries **exclude** browser windows
  (prefix filter). This is the "browser excluded from normal switching" behaviour.

#### walker 模式与切换（`@` / `#`）

不用多个前缀；两个键 + 全局状态（`walker_mode`、`op_mod`）即可切换：

- **`@`**：在 **session ↔ tab** 之间翻转（`state.walker_mode` 已记录当前模式，直接翻到另一种，无需多个前缀）。
- **`#`**：切换**操作模式** `state.op_mod = 1/0` —— 仅当**当前激活窗口是 tab** 时有效：
  - 激活窗口不是 tab → `#` **无效**。
  - 是 tab → `#` 进入操作模式（对当前 tab 展示操作：关闭、导航等）；再按一次回到原 session/tab 模式。
  - 当前是 session、按 `#` → 进入操作模式；按 `@` → 直接进入 tab 模式。

各模式行为：
- **session**：切换 / 创建 / 删除 session；输入**不存在的名字 → 直接创建并切换**（k8s `ns` 语义）；
  `state.current_session` 持久化，后续 tab 操作都基于它（namespace 语义）。
- **tab**：搜索当前 session 的**打开窗口**；`Enter` 切过去（`activateTarget` + niri focus window）。
- **op_mod=true**：对当前激活 tab 提供操作（关闭等，可再扩）。
- **全局 tab**（RSS / IM 等非 session 内）：在**所有 session 里都可显示/访问**，属一个单独的常驻
  全局实例（`session_id` 为 NULL），不随 `current_session` 改变。
- **关闭语义**：`qw` 主动关闭 → 从 session **删除**该页；通过 niri 关闭 / 意外（如崩溃）关闭 →
  **保留**（仅标 `closed_at`，不删）——主动关不留痕，外部/意外关不丢。

> ⚠️ walker's exact provider/plugin protocol is **to be verified against its real API**
> before wiring (the "fabricated API" rule). The interface above is the contract qw
> exposes; the provider code adapts to walker specifics.

## NixOS / config coupling

- Optional: niri config declares `web:*` named workspaces + a `qw open`/`qw move` binding
  (NixOS-managed, not inside the qw repo).
- Launch binding / Mod+Space reserved for the launcher menu entry.

## Verification status

**Done**: PID window↔instance mapping; `move` via `focus-window --id` + `move-window-to-workspace`;
**P7a**: `WmExt` interface + `NiriExt` backend live (`qwlib/wm.py`), move/add migrated to it;
column-width read (`layout.tile_size[0]`/`logical.width`) + set (`<N>%`) verified; `qw col
remember/show` + `open`/`add` auto-apply (P5).
**Pending (P7b)**: `LauncherExt` walker provider API to verify; named `web:*` workspace config;
hyprland backend.