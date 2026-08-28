# qw extensions — WM & launcher integration

`qw` core is **WM/launcher-agnostic**: it manages sessions, instances, pages and CDP.
Environment-specific integrations are **pluggable extension modules** loaded behind a
small interface, so the same core works on any WM / launcher that can satisfy it
(hard constraint in the wiki: WM must offer interface/CLI fine-grained control —
niri & hyprland qualify; cosmic-de does not yet).

This document specifies the *integration approach* (P7). P0–P5 implement core + the
niri bits that `move` already needs; the interface below is the target shape.

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
- **Column width**: `set-column-width <fraction>` is pending grammar confirmation;
  reading it back is `proportion = window_width / output_width` snapped to a band
  (no direct field — see PLAN §5).
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

> ⚠️ walker's exact provider/plugin protocol is **to be verified against its real API**
> before wiring (the "fabricated API" rule). The interface above is the contract qw
> exposes; the provider code adapts to walker specifics.

## NixOS / config coupling

- Optional: niri config declares `web:*` named workspaces + a `qw open`/`qw move` binding
  (NixOS-managed, not inside the qw repo).
- Launch binding / Mod+Space reserved for the launcher menu entry.

## Verification status

**Done**: PID window↔instance mapping; `move` via `focus-window --id` + `move-window-to-workspace`.
**Pending (P7)**: `set-column-width` grammar; column-width read field; exact walker provider API;
named `web:*` workspace config.