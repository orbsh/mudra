# mudra extensions — WM & launcher integration

`mudra` core is **WM/launcher-agnostic**: it manages sessions, instances, pages and CDP.
Environment-specific integrations are **pluggable extension modules** loaded behind a
small interface, so the same core works on any WM / launcher that can satisfy it
(hard constraint in the wiki: WM must offer interface/CLI fine-grained control —
niri & hyprland qualify; cosmic-de does not yet).

This document specifies the *integration approach* (P7). **P7a is live**: the `WmExt` interface
+ `NiriExt` backend (`mudralib/wm.py`) are implemented and `mudra move` / `add` run on it. P7b
(`LauncherExt` walker menus) 走 elephant `menus` provider（见下）；walker provider 不能外部插件。
交互模型已定稿：面板（`mudra ui`）承担 tag 富交互，launcher 只留 `p`（见下）。P0–P6 implement core +
the niri bits; the interface below is the target shape.

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
(`mudra move`, focus, column-width) have fully migrated onto the interface.

## Interface: Launcher extension (`LauncherExt`)

Core exposes clean data; the launcher module renders it and dispatches selections back
to core actions:

- `sessions() -> [{name, workspace, n_pages, running}]`
- `pages(session) -> [{title, url, closed}]`
- actions: `open_session(name)`, `focus_page(query)`, `open_url(url)`,
  `move_session(name, ws)`, `new_session(name, url)`

### walker implementation (elephant menus)

walker 的 provider 是 **编译进二进制的**（apps/files/windows…），不能从外部加插件。mudra 的动态
菜单走 **elephant `menus` provider**（本机 cwdhist/windowsmru 已验证的模式）：

```
用户输入 → 前缀路由(或 `;` providerlist / `-m` 直达) → `menus:<x>` provider
         → Lua `GetEntries(query)` → `io.popen("python3 scripts/mudra_menus.py list <x> <query>")`
         → TAB 三列 `text<TAB>subtext<TAB>value`
         → 选中 → Lua `Action` / walker `[providers.actions."menus:<x>"]` → 回调 `mudra` CLI
```

- **数据层**：`scripts/mudra_menus.py` 读 `mudra.sqlite`（`sessions`/`pages`/`state`），零第三方依赖。
- **动作层**：优先复用现有 `mudra` CLI（`focus`/`close`/`open`/`move`/`use`）；action 命令名与
  Lua entry 的 `Actions` map 键一致。
- **多字符前缀已核实**（`walker src/data.rs` `text.starts_with(&prefix.prefix)`，任意长 String）：
  前缀不限于单字符。`argument_delimiter`（全局 + per-provider `HashMap<String,String>`，
  `config.rs`）支持「前缀+分隔符+参数」如 `t foo`。
  - 坑①：前缀按 config 声明顺序 `find` 第一个匹配 → 配多长度前缀时**长前缀排在短前缀前**。
  - 坑②：前缀触发是**持续模式**（输入持续过滤该 provider）；「选中即返回」用 Action `after="Close"`。

#### 交互模型（p/t/a/s，键位定稿）
**关键交互演进（2026-08 定稿）**：面板取代了 launcher 的 tag/sort/action 模式。tag
多选批量、评分轴、排序这些**富操作**交给 solidjs 面板（`mudra ui`）；launcher 只保留
**Page 热路径单动作**。前缀收敛为单字符 `p`（不再需要 `t`/`a`/`s`——它们被面板吸收）：

| 前缀 | 模式 | menus provider | 动作 |
|---|---|---|---|
| `p` | Page（默认唤起 Mod+Spc） | `menus:mudrapages` | 页面列表 → focus / close / 移动本窗口 / 交换 |

> 早期草案的 `t`（tag）/`a`（action）/`s`（sort）三模式已**移交面板**：面板提供评分
> 轴 + 胶囊 tag + 批量指派（见 `docs/PANEL.md`）。launcher 不再承担富交互。

**tag 多选（面板内实现）**：面板的批量指派是**提交层动作**——勾选多个 page → 选 tag
→ 整组替换。这绕开了 walker 单选无多选的限制（面板是独立界面，非 walker provider）。
launcher 侧如需 tag 过滤，用 `p` 模式的过滤参数即可，无需多选。

需 mudra 侧补的数据能力（草案依赖）：
- **星级 / 收藏**：mudra 目前无 bookmark — 需新建（`site_stars` 或并入 state）。
- **排序偏好**：存 `state` 表（`mudra use`/`mudra mode` 已有写入机制），tabs/sessions 脚本按它排序。
- **当前聚焦 tab 识别**：`pid→instance→title 匹配 target`（同 `mudra col remember`，机制已通）。

#### Alt+Tab
浏览器窗口已出现在 `menus:windowsmru`（所有窗口 MRU 切换）。LauncherExt 的价值是**按 session
组织** + session/tab 级操作；niri 无原生切换器，浏览器窗口默认混在正常切换中（不动）。

## NixOS / config coupling

- Optional: niri config declares `web:*` named workspaces + a `mudra open`/`mudra move` binding
  (NixOS-managed, not inside the mudra repo).
- Launch binding / Mod+Space reserved for the launcher menu entry.

## Verification status

**Done**: PID window↔instance mapping; `move` via `focus-window --id` + `move-window-to-workspace`;
**P7a**: `WmExt` interface + `NiriExt` backend live (`mudralib/wm.py`), move/add migrated to it;
column-width read (`layout.tile_size[0]`/`logical.width`) + set (`<N>%`) verified; `mudra col
remember/show` + `open`/`add` auto-apply (P5).
**Pending (P7b)**: elephant menus（`mudra_menus.py` + `menus/mudra*.lua` + walker 前缀绑定 + `p`
热路径交互）；mudra 侧补星级/bookmark、排序偏好 state、当前聚焦 tab 识别；named `web:*` workspace
config；hyprland backend.

### 重构影响（tag 森林 → PLAN §9 / wiki `tag-forest.md`）
LauncherExt 从 session/page 视角向 **tag 森林** 演进：`p` 模式按 tag 维度过滤
（situation 默认 inbox；importance/urgency 为两级树 + rank 排星序）。`current_session` →
situation。**富交互（tag 多选批量、评分轴、排序）已移交面板**（`mudra ui`，见
`docs/PANEL.md`），launcher 只保留 `p` 热路径；tag 过滤走 `p` 模式过滤参数。
