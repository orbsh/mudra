# mudra — browser session manager: implementation plan

> **Status**: planning
> **Date**: 2026-08-28
> **Design source**: `~/.hermes/wiki/keyboard-wm-browser-mode.md` (the authoritative design; keep in sync)

## 1. Goal

A minimal-UI, keyboard-driven, externally-controlled browser **usage mode** for Niri (or any tiling WM):

- Each page is a `chromium --app` window (no omnibox, no tab strip → maximal webview).
- One Chromium **instance per session/workspace**; several sessions/instances may run concurrently.
- Sessions and pages live in **sqlite**; a CLI (`mudra`) + daemon (`mudrad`) drive everything over **CDP**.
- **SurfingKeys** is preloaded via `--load-extension`; proxy and an **extension list** are configurable per instance.
- Links that would open a new window are **intercepted** by an injected script and opened as a new `--app` window instead of a chrome-default window.

Why (motivation): qutebrowser's modal input-method problem (`#3444`) → seek a non-modal, minimal-UI, fully external-controlled browser. See the wiki doc.

## 2. Decisions (locked)

- `--app` per page (no embedded tab strip). Multi-tab = the user opens plain Chromium themselves.
- **Path B concurrent**: one Chromium instance per session/workspace; multiple may run.
- Python (CLI + daemon). Sole non-stdlib dep: `websocket-client` (CDP). sqlite via stdlib.
- Command name `mudra`; daemon `mudrad`.
- SurfingKeys loaded via `--load-extension` (verified working in chromium 151).
- New-window opening routed through an injected content script → local HTTP server → new `--app`.
- **Global instance & global tabs**: RSS / IM 等非 session 窗口由**单独常驻全局实例**承载；它们不属于任何
  session，所有 session 内都可访问/显示。模型：`pages.session_id` 为 NULL 表示全局 tab。
- **WM-control constraint**: the whole mode depends on fine-grained WM control via interface/CLI (move / focus / set-column-width / workspace routing). niri & hyprland satisfy; **cosmic-de does not yet**.

## 3. Architecture

```
  user/script ─▶ mudra (CLI) ─▶ mudrad (daemon) ──CDP WS──▶ chromium --app (1 per session)
                  │              │                          │
                  │ read        sqlite events (Target.*)    │ user-data-dir per session
                  ▼              ▼                          │
                sqlite ◀──────── mudrad ◀────── window/target ◀┘
                 (~/.local/share/mudra/mudra.sqlite)
```

- **`mudra`** (CLI): quick commands; reads sqlite; forwards control commands to `mudrad`.
- **`mudrad`** (daemon): holds a CDP WebSocket to each **running** instance; subscribes `Target.targetCreated/infoChanged/Destroyed` → real-time sqlite sync (satisfies "update the list on close"); forwards control commands; spawns/stops instances.
- **chromium**: `--app=<url>`, dedicated `--user-data-dir` per session, dynamic `--remote-debugging-port`, `--proxy-server`, `--load-extension=<list>`, uniform title prefix.

## 4. Data model

```sql
instances(id, profile TEXT, port INT, pid INT, running INT,
         proxy TEXT, extensions TEXT)            -- 1 session ↔ 1 instance
sessions(id, name UNIQUE, workspace TEXT, instance_id FK,
         created_at, last_opened_at)
pages(id, session_id FK CASCADE NULL, target_id, url, title,
      position INT, opened_at INT, closed_at INT NULL)   -- session_id NULL = 全局 tab
site_widths(site TEXT PRIMARY KEY, proportion REAL)   -- site → column-width ratio (0..1)
state(key TEXT PRIMARY KEY, value TEXT)                -- 全局状态: current_session, walker_mode(session|tab), op_mod(1|0)
```

- `pages.target_id` = CDP target of the window; `url` live-updated via CDP `infoChanged` → URL-filterable.
- `instances.proxy` / `.extensions` = per-process proxy and preload extension list.

## 5. Components & mechanisms

### CLI verbs (v1)
```
mudra new <name>               create session (+ workspace)
mudra open <name> [url...]     switch workspace, spawn instance, rebuild pages
mudra add <name> <url>         add a page to current/session session
mudra move <name> <workspace>  move a process's whole window set to another workspace
mudra ls [name]                list sessions/pages (URL-filterable)
mudra close <name>             close the instance (windows + workspace collapse)
mudra rm <name>                delete session (+ its profile/workspace)
mudra goto <url> | back | forward | reload
mudra focus <page>
mudra yank                 copy focused window URL to clipboard
mudra quit                     close all instances
mudra daemon start|stop|status
```

### CDP control
- Launch: `chromium --app=<url> --remote-debugging-port=<dyn> --user-data-dir=<session profile>`
  `--proxy-server=... --load-extension=<dir>,... --no-first-run` (SurfingKeys etc.)
- Open/close page: `Target.createTarget` / `Page.close` → Target events sync sqlite.
- Navigate: `Page.navigate` / `reload` / `getNavigationHistory` / `navigateToHistoryEntry`.
- **Find page by URL and activate**: `Target.getTargets` (url/title/type) → match → `Target.activateTarget`. Works on error pages.

### session = workspace (concurrent)
- Each session binds a niri workspace (`web:<name>`); `mudra open` = `focus-workspace` + spawn + rebuild.
- Windows land on the workspace via niri window-rule (title prefix / app_id) + `focus-workspace`.
- Workspaces can hold mixed windows from several processes; **moving a process's windows to a workspace is the "separate" operation**.

### window↔process map & batch move
- mudra records which window belongs to which process (CDP target ↔ niri window by title/app_id).
- `mudra move <name> <workspace>` iterates the process's windows → `move-window-to-workspace <ws>` (verified).

### URL record & filter
- CDP `infoChanged` live-updates `pages.url`; list/filter by URL substring in `mudra ls` / walker.

### column-width memory (`mudra col`, P5 done)
- niri column width **is** an output-ratio (`preset-column-widths`, `switch-preset-column-width`/Mod+R cycles 1/3 1/2 2/3 1).
- **Read back** (verified): focused window `layout.tile_size[0]` ÷ output `logical.width`. e.g.
  1162.86/1755 ≈ 0.66 → band **2/3**.
- **Set** (verified): `set-column-width <N%>` — percent; the `1/2` fraction syntax **errors**.
  Emit integer percent of the snapped band. Bands {1/3, 1/2, 2/3, 1}.
- `mudra col remember` captures the focused window's domain + width into `site_widths`;
  `mudra col show` lists; `open`/`add` auto-apply by domain (wait for the instance's window to focus).

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
- **Cascade**: the interception must be (re)injected on **every** new page target (`Target.targetCreated` → `Page.addScriptToEvaluateOnNewDocument`); otherwise a window that `mudrad` itself spawns carries no script, and a further `_blank` there reverts to a chrome-default (non-`--app`) window.

### Extensions（WM / launcher 对接，可插拔）

`mudra` 核心是 **WM / launcher 无关**的：只管理 session/instance/page + CDP。环境相关集成做成
**扩展模块**，经统一接口加载、按需启用：

- **WM 扩展**（`move` / focus / 列宽 / workspace 路由 / 窗口映射）：核心只经 `WmExt` 接口调用，
  由具体实现提供（`niri` 现成；`hyprland` 未来可加）。niri 实现走 `niri msg` IPC。
- **Launcher 扩展**（菜单提供 + 动作处理）：核心暴露 session/page 数据，`LauncherExt` 把数据
  渲染成 launcher 菜单项并处理选中动作。walker 落地 = **elephant `menus` provider**（动态菜单，
  `menus/*.lua` + Python 数据脚本读 mudra.sqlite，动作回调 `mudra` CLI）。已核实：walker **支持多字符
  前缀**（`data.rs` `starts_with`）+ `argument_delimiter`（前缀+分隔符+参数）。详见 docs/EXTENSIONS.md。
- 启用清单由配置决定（如 `extensions: [niri, walker]`）；核心不感知具体实现。

**BrowserEngine backend（可选延伸）**：控制协议同样按此抽象——`BrowserEngine` 接口
（launch / list_targets / navigate / inject / close），chromium 走 CDP backend。
替代引擎（Ladybird / Servo）的控制协议是 **Firefox RDP 非 CDP** 且不成熟（核实见
wiki），故先只做 chromium=CDP，等哪个引擎协议长成熟再补新 backend，而非现在改包。

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
- `--load-extension` behavior with a second extension (e.g. Bitwarden) reliably.

## 7. Implementation phases

- **P0 scaffold**: `~/world/mudra` repo layout; sqlite schema init; `mudra new` / `mudra ls`.
- **P1 core loop**: `mudra open` spawns instance; `mudrad` connects CDP; Target events → sqlite (open/close/url live-sync).
- **P2 CDP verbs**: `goto / back / forward / reload / focus <page>`; find-by-URL activate.
- **P3 workspace & move**: window↔process map; `mudra move`; per-instance workspace routing; URL filter in `ls`.
- **P4 proxy & extensions**: `--proxy-server`; `--load-extension` list; build+ship SurfingKeys bundle.
- **P5 column-width memory**: Win+R capture → `site_widths`; auto-apply on open. **[done 2026-08]**
- **P6 new-window interception**: `inject.js` + local `/open` server in `mudrad`. [done]
- **P7 extension modules**: build `WmExt` (niri move / column-width / workspace routing /
  window mapping) + `LauncherExt` (walker session/page menus + hotkeys) behind the pluggable
  interface (§5 Extensions); migrate `mudra move` and focus onto the `WmExt` interface.
  **[P7a done 2026-08: `mudralib/wm.py` `WmExt`+`NiriExt`, move/add migrated]**
  **P7b (walker LauncherExt)**: 已核实走 **elephant menus** 而非 walker provider 插件（provider
  编译进二进制）；walker **支持多字符前缀**（`data.rs` `starts_with`, long prefix 排前）+ `argument_delimiter`。
  交互模型草案 **t/s/a/o** 单字符前缀（tab/session/action/sort，见 docs/EXTENSIONS.md，**待定稿**）；
  需 mudra 补数据能力：星级/bookmark、排序偏好 state、当前聚焦 tab 识别。

Each phase ends with a working, verifiable slice (per 迭代闭环). Cross-cutting: verify niri/CDP APIs against reality before wiring each phase (the "fabricated API" rule).

## 8. Known issues (deferred)

- **密码不记忆**：chromium `--app` 实例输入过的密码不会保存。根因：Linux 下 chromium 存密码依赖
  OS keyring（gnome-keyring / libsecret），本机未起该 daemon → 不弹「保存密码」、不落地。候选修复
  （未验证、以后再做）：① 起 `gnome-keyring`（正规 keychain）；② 或启动参数 `--password-store=basic`
  （密码存进 profile、无 keyring 也能工作，安全性弱于 keychain）。需先核实该 flag 在 chromium 151 是否仍生效。
  （2026-08 记录）

## 9. 重构方案：从 session 到标签森林（2026-08，未定稿）

> 目标重定位：从「全键盘 UX 工具」升级为「信息流精炼项目」——浏览器是信息主要入口，mudra
> 做 捕获 → 属性化 → 精炼 → 消费。通用模型在 wiki `tag-forest.md`，本节是 mudra 落地映射。
> **不推翻 P0-P7**（已实现的启动/隔离/生命周期仍需要），是架构转向。

### 核心转向
- **session 拆分**：内容组织(session 的语义) → **标签森林**；运行载体 → isolated 实例；空间展示 → 视图。
- **标签森林**：多棵树、树间不互斥、树内单选、无单一根；退化覆盖普通多标签/传统分类两端。
- **评分并入树**：importance / urgency 为两级树（叶 ☆..☆☆☆☆☆，`rank` 排星序），取代独立「评分字段」。域名/子域名规则表 = 给叶 tag 打默认分，手动覆盖。
- 术语：**tab → page**（避免与 tag 混淆）。

### 目标数据模型
```sql
tag(id, parent_id, name, isolated, required, rank, note)
page_tag(page_id, tag_id)      -- 树间多行 = 多选；树内单选为 app 层约束
```
- `isolated=true` → 内容进独立实例（profile/cookie 隔离）。目前 situation 四子节点(inbox/work/personal/privacy)隔离。
- `required`(situation, 默认 inbox)：新信息默认落 inbox 实例，处理后移到对应维度值。

### 交互 / 实例
- **inbox 分流**：inbox = 暂存入口；处理 → work/personal/privacy 实例。
- **pin 常驻（PWA）**：IM/RSS 常驻某实例、独立窗口，即渐进式 Web 应用(PWA)。
- `current_session` 语义 → situation（默认 inbox）。

### 后续
- ML（朴素贝叶斯 / 逻辑回归，非 LLM）从手动评分学「该选哪个值」，生成规则供审。
- 域名/子域名规则 → 自动打 importance/urgency 默认分。

## 10. 信息流精炼扩展路线（2026-08，方向）

> 通用管道设计（捕获→属性化→精炼→消费）见 wiki `infoflow-refinement.md`；内容组织轴
> 见 wiki `tag-forest.md`。本节是 mudra 落地路线。

**属性化 — NB 评分（importance/urgency）**：
- 朴素贝叶斯（手写，纯 stdlib），规则先行 + ML 建议规则，非黑箱替代。
- 特征：域名层级 + URL token + **正文特征**（不只标题）。5 级有序分类 → 加权期望求分。
- **标题-正文一致性分**（`|标题核心词 ∩ 正文前两段| / |标题核心词|`）检测标题党、下调 importance。
- **正文需 html→md** 提取工具（`trafilatura`/`readability-lxml`/`html2text`，实现时核实）；可同时拿页内链接做**链接挖掘**（被高价值页引用的页 → inbox/建议分）。

**捕获 — RSS/订阅监控**：订阅源进 `inbox`，幂等去重。

**精炼/消费 — LLM 总结 / 转 MD**：reader-mode 提取正文 → MD 存档；高价值页 LLM 单页/聚合摘要。
**聚合摘要 / 态势感知**：按 situation/importance 聚合 → 关键信息态势，作为更大监督系统的输入侧模块。

**优先级建议**：① 转 MD + 全文检索（轻量、无 LLM、是后续基础）→ ② NB 评分 + 标题党（属性化）→ ③ RSS → ④ LLM 总结/态势感知。
工程顺序独立于方向；`importance/urgency` 评分即 `tag-forest` 的树，先规则底座可无 ML 即时盈利。
