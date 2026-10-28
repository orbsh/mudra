# mudra — 全键盘的浏览器模式：信息流精炼

> **语言**: 中文（`README.md` 的译本）· [English](README.md)
>
> 对多数人来说，浏览器是**信息的主要流入途径**。`mudra` 不止是用键盘驱动浏览器——它把浏览器入口变成一条
> **信息流管道**：**捕获 → 属性化 → 精炼 → 消费**。
>
> **设计理由不在本文**：`https://github.com/orbsh/wiki/blob/main/keyboard-wm-browser-mode.md`（中文主文档，英文版 `https://github.com/orbsh/wiki/blob/main/keyboard-wm-browser-mode-en.md`）讲为什么——定位、硬约束、选型、层边界、被否决的路。本文是实现面：命令、存储布局、CDP 映射、协议形状、状态台账。通用 tag 森林模型见 `https://github.com/orbsh/wiki/blob/main/tag-forest.md`。

通过 **CDP** 从外部驱动 **`chromium --app`** 窗口，底层是内嵌的 **okm/fjall KV 存储**，跑在平铺 WM（`niri`）下，全程键盘。页面按 **tag 森林**组织（多轴，取代 session），并带重要 / 紧急评分——浏览器因此先替你分拣，而不只是显示网页。

**状态**：全栈 Rust——`mudra`（CLI）/ `mudrad`（daemon）+ okm/fjall 存储 + leptos wasm 控制台；Python 树已删除（2026-09-29）。核心（拉起 / 实时 CDP 同步 / 导航 / niri 窗口映射与移动 / 代理与扩展 / 列宽记忆）可用。**tag 森林**与 **wasm 控制台**（`mudra ui`）是管理面，所有生命周期操作都走 `mudrad` 控制 API。**扩展面**已落地：生命周期落进只追加的事件日志，常驻子进程扩展经 stdio 按 Induction 契约被托管，声明了 `mudra:before_navigate` 的 guest 在导航前拿到判决（allow / deny / redirect），预算内失败放行；页面侧**钩子**（ns 9）承载"JS 作为数据"。已验证 / 待验证台账见文末 *Status & known issues*。

**前提**：需要一个既有精细 IPC、又有无上限 workspace 的 WM——只有 niri 两条都满足；论证在 wiki 文档里，不在这里。

---

## 快速开始

```bash
mudrad run                        # daemon：控制 API + WS 推送 + Target→store 同步
mudra open https://example.com    # 在当前上下文打开一页（实例不在则拉起）
mudra add https://example.com     # 同一实例里再加一页
mudra ctx work                    # 切换上下文（situation 叶）
mudra ls                          # 各上下文页数
mudra ui                          # web 控制台
```

每个动词都是 `http://127.0.0.1:8899` 的薄 HTTP 客户端；daemon 是唯一拉起 / 关停窗口并写存储的进程。

## 命令

| 命令 | 作用 |
|---|---|
| `mudra open <url> [--ctx LEAF]` | 在某个上下文打开页面（实例不在则拉起） |
| `mudra add <url> [--ctx LEAF]` | 往该上下文运行中的实例再加一页 |
| `mudra close [query] [--ctx LEAF]` | 关掉整个上下文实例，或按 url 过滤关单页 |
| `mudra ctx [LEAF]` | 显示 / 切换当前上下文（situation 叶） |
| `mudra ls [LEAF] [-f FILTER]` | 各上下文页数 / 某上下文的页列表（url·title 过滤） |
| `mudra targets <ctx>` | 列活动 page target（CDP） |
| `mudra focus <query> [--ctx LEAF]` | 对指称文本排名召回，取该上下文第一个活页命中 |
| `mudra goto <url>` / `back` / `forward` / `reload` `[--ctx LEAF]` | 导航 |
| `mudra page move-here\|swap\|close <url> [--ctx]` | 对选中页做页级动作 |
| `mudra move <ctx> <workspace>` | 把该上下文的窗口移到某 workspace（niri） |
| `mudra conf <ctx> [--proxy <p>] [--ext <csv>]` | 按上下文设置代理 / 扩展清单（下次 open/add 生效） |
| `mudra col [remember\|show] [site]` | 列宽记忆（niri） |
| `mudra tag init\|add\|remove [tag_id] [page_id]` | tag 森林种子 / 页指派（省略 `page_id` = 聚焦页） |
| `mudra hook set\|ls\|rm <name> [--code\|--file] [--matches] [--enable\|--disable]` | 页面侧钩子（ns 9） |
| `mudra sort mru\|mtime\|rating` | 控制台排序偏好（state 槽） |
| `mudra dev [on\|off]` | 扩展开发模式（拉起前清 chromium 扩展缓存） |
| `mudra ui` | 打开 web 控制台（聚焦既有窗口或新开） |

`mudrad` 不是子命令：`mudrad run` 作为 systemd user 服务常驻。

## 数据模型 — fjall 上的 okm collection

单一存储在 `~/.local/share/mudra/store`（developMode 下 `~/.local/share/mudra` 是仓库 symlink，wrapper 解析 `target/{release,debug}`）。完整布局契约与验收标准见 **`docs/SCHEMA.md`**（中文版 `docs/SCHEMA.zh-CN.md`）。

| ns | collection | key → payload |
|---|---|---|
| 1 | Tag | `id: u32` → `parent_id`（i32，`-1` = 根哨兵）、`name`、`alias`、标志 `isolated`/`required`/`hidden`/`deleted`、`rank`、`note`；访问法 `by_parent`、`by_name`（函数索引） |
| 2 | Page | `id: u64` → `instance_id`、`target_id`（CDP 字符串）、`url`、`title`、`position`、`opened_at`、`closed_at`、`deleted_at`、`parent_id`（开它的页）；访问法 `by_instance`、`by_parent`、`by_target`；指称文本**不设索引** |
| — | page_tag | junction，自己不占 ns：一条边落在 Page ns 2，另一条落在 Tag ns 1 |
| 3 | *退役* | 永久空洞——槽位不复用 |
| 4 | Instance | `id: u32` → `profile`（situation 叶）、`port`、`pid`、`running`、`proxy`、`extensions`；访问法 `by_profile` |
| 5 | SiteWidth | 站点 → 列宽比例（0–1） |
| 6 | State | 定长槽位：`current_context`、`sort`、`dev_mode`，以及 `tag_id` / `page_id` / `instance_id` 三个计数器 |
| 7 | History | 地址访问记录，`by_url` 函数索引；由 `/open`、`/add` 经 `history_bump` 写入；**不 bump epoch** |
| 8 | Event | 只追加日志（`page_open` / `page_close` / `tag_set`）；`POST /events {cursor,limit}` 或 `{tail:N}`；响应一律带回 `head`；checkpoint 归消费方；不 bump epoch |
| 9 | Hook | `name`、`matches`（逗号分隔 URL 子串，空 = 所有页）、`code`、`enabled`、`updated_at`；按 name 幂等 upsert（缺省字段保留原值）；不 bump epoch |

- **迁移策略（原型阶段）**：schema 变更 = 删库重建——停 daemon、`rm -rf ~/.local/share/mudra/store`、起、`POST /tag_seed`。不写迁移代码。
- **槽位纪律**：槽位即声明序（okm ADR-0005）。删掉的索引留下无人读取的死字节——旧 store 照常可用、无需重建——该位置以后不得直接声明新索引，除非把旧索引保留为 `deprecated`。
- **单写者**：fjall 的文件锁让第二个写者根本无法打开存储，所以竞态不需要 DB 层唯一约束。
- **tag 种子**（`POST /tag_seed`，幂等）：`situation`（inbox/work/personal/privacy——四叶都 isolated，inbox 必选）、`importance`/`urgency`/`quality`（各 ☆..☆☆☆☆☆ 五级，`rank` 排星序）、`state`（to-read/reading/distilled/archived）、`topic`。

## 控制 API 与 daemon

- `mudrad` 为每个运行中的实例持一条 CDP WebSocket，订阅 Target 事件、写存储、转发控制命令、拉起与停掉实例。单例（flock）。
- systemd user 服务挂在 `graphical-session.target`，启动脚本带 Wayland env 闸门。
- 拉起的实例带 `PDEATHSIG`：daemon 死掉不会留下孤儿浏览器。
- 控制台的数据来自同一 API 的 shaped reads（`/forest`、`/ctx_pages`、`/events`、`/hook_list`）；WebSocket 只承载 epoch 失效帧，从不承载数据。
- **epoch 纪律**：写操作报告"是否有实际变化"，只有变了才 bump epoch。控制台从不轮询——听到 epoch 数字后重拉。
- 拉起参数：`chromium --app=<url> --remote-debugging-port=<动态端口> --user-data-dir=<profile> --no-first-run`，外加实例行里的 `--proxy-server=<url>`（`--proxy-bypass-list` 可选）与 `--load-extension=<dir1>,<dir2>`。默认扩展是仓内 `frontend/`。

## 页面、窗口与 CDP

- **Target**：`Target.createTarget` 开页、`Page.close` 关页、`Target.activateTarget` 聚焦；`Target.getTargets` 列所有 target（url/title/type），因此按页精确控制对错误页同样成立。
- **导航**：`Page.navigate`、`Page.reload`、`Page.getNavigationHistory`、`Page.navigateToHistoryEntry`——统一走 `/nav` 一个动词。
- **实时同步**：`targetCreated` / `infoChanged` → upsert page（url/title 实时）；`targetDestroyed` → 置 `closed_at`。不轮询。
- **新窗口拦截（级联）**：`--app` 里 `_blank` / `window.open` 仍会开出 chrome 默认窗口，所以 `mudrad` 注入脚本（改写 `window.open`、捕获 `a[target=_blank]` → Image beacon → 本地 `/open` → 新的 `--app`）。**每个新 page target 出现时都要注入**（`Target.targetCreated` → `Page.addScriptToEvaluateOnNewDocument`）；否则 mudrad 自己开出的窗口没带脚本，它里面再点 `_blank` 又退回 chrome 默认。
- **窗口 ↔ 实例映射**：拉起时即记录（地址、pid、窗口 id）——CDP target 归属对齐到 niri 窗口 id，从不看窗口标题。
- **关闭语义**：mudra 主动关 → 从归档删除（不留痕）；WM 关窗或崩溃 → 保留该行、只标 `closed_at`。三个关闭出口（`close_target` / `close_page` / `mark_down`）都发 `page_close`——实例尸体不会产生 `targetDestroyed`，teardown 必须补发。
- **事件日志规则**：daemon 启动时的 baseline 重放不算页面事件（重启不是打开）；Refresh 分支只在 url 实际变化时发 `page_open`——只有 title 变化是元数据，不是事件。

## 实例、隔离与 workspace

- 每个 isolated tag 维度一个实例（独立 profile / cookie / cache / 进程）。默认不跑；`mudra open` 才拉起。可并发多个。
- 每个 isolated tag 绑定一个 niri workspace（`web:<tag>`）。`mudra open` = 聚焦该 workspace + 拉起实例 + 重建它的页。
- 窗口按映射进 workspace，不按标题：打开时 `niri msg action focus-workspace <name>`，加上记录的窗口 id。
- 不同实例的窗口可以摆在同一 workspace；`mudra move <ctx> <workspace>` 遍历该实例的窗口并发 `niri msg action move-window-to-workspace`。移动即"分开"的手段。
- **资源模型**：process-per-page，所以一个实例里 N 个 `--app` 窗口的成本等于 N 个标签；独立实例则各付一个 browser 进程 + cache。实例数 = 隔离维度数，绝不是页面数。

## 列宽记忆（Win+R）

- niri 的列宽**就是**输出宽度比例；`preset-column-widths` 的档位是 1/3、1/2、2/3、1（`switch-preset-column-width`，Mod+R）。
- 读取（2026-08 核实）：`niri msg -j windows` 顶层没有宽度字段；聚焦窗口的嵌套 `layout.tile_size[0]` 就是列宽——除以 `focused-output.logical.width`。
- 设置（已核实）：`set-column-width <N%>` 收百分比（或 `-N` 像素）；分数形式 `1/2` 报错。恢复时把存下的比例 snap 到最近档位，按整数百分比发射。
- 捕获：Win+R 读聚焦窗的列宽比例 + 聚焦页的域名，snap 后按站点写入；打开该域名时恢复。

## 键盘：一条地址行、两个打开动词、一个关闭

- **地址栏就是命令行**（2026-10-22，三轮裁决）。`o` / `O` 只是把 `open ` / `new ` 打进同一行。出现空格后候选换成*活页（仅 console 角色）+ 打开历史*；Tab 在多候选间移选中，唯一候选补全到行里，Enter 取高亮那条（无匹配则用敲进去的文本）。**没有第二层 picker**——两种情形都只有一次回车，`:open ` 之后也没有死键（空行 Backspace 退出命令模式）。
- **两个动词**：`open` 就地导航（`location.href`；hint 的 `f` 与 picker 的 Enter 用它），`new` 另开一个 `--app` 页窗口（hint 的 `F`，即 mudrad 的 `/open`）。控制台页永不就地导航——一律 `new`。
- **地址归一化只有一处**（`spawn::normalize_url`）：缺 scheme 补 `https://`；单标签裸词补 `.com`（`bing` → `bing.com`），而 `localhost`、带端口的 host、带 userinfo 或带点的 host 原样保留。两个动词都走它，所以地址行里的裸词不会是死域名 `https://bing`。
- **`:q`** 关掉"说这句话的那个页"（2026-10-22，无默认键）：service worker 直接 `chrome.tabs.remove(sender.tab.id)`，**不走** mudrad 的 `/close_page`（它按 url/title 子串匹配，会认错页）。控制台也能 `:q`——它只是另一个标签。daemon 侧不需要配合：watcher 收到 `Target.targetDestroyed` 就把该行标 `closed_at`。
- 命令行那一行、它上方的候选浮层与状态栏共用一套外观（`bg` 色相 + `bar.opacity` + `statusBlur` + `fg`）。键位与配置键见 `docs/KEYS.md`。

## 历史召回

指称文本 = 剥掉 scheme 的小写地址 + 小写标题。召回与打分（2026-10-14）：

- **召回**：查询词元的**子串包含**（**无长度地板**，两个字符也算词元）∪ **子序列**（`ghb` → github；`ycombinat` → news.ycombinator.com，由地址中段命中）。
- **打分**：对指称文本算 BM25（URL 噪声由 idf 承担，不靠把 URL 挡在召回之外）+ 访问次数 + 时间 → 四条等权 RRF 腿。做召回的那次扫描同时是语料普查，所以 BM25 统计量是精确值。
- 两条召回腿都不命中的地址，无论访问多少次都不出现在非空查询里；空查询保留旧规则（全表、按访问次数）。
- `by_addr` / `by_title` 已退役：子序列没有索引可答，召回就是一次全读，索引失去了剪枝对象。历史一行一个地址（不是一次访问一行）；本机全表约 108 KB，空查询本来就全读。

## 扩展面

在 `config.kdl` 的 `[extensions]` 里声明为 **name → 可执行路径**（只是路由元数据）。订阅声明在 guest 的 `interface_schema` 里、由 `hello` 帧携带——从不进配置，也从不是协商。线契约：Induction（`~/world/induction` 的 crate，像 okm 一样以 git 源引入）；mudra **不引任何 aura crate**——契约即依赖。CDP socket 用 `tungstenite`；CDP 本身是手写 JSON，按信封 id 路由 waiter、超时自摘（不套 JSON-RPC）。

- **投递 = 声明的 `Tier`**：`Hot` + deadline = 拦截语义（导航前判决；超时是 failure value，宿主侧映射为放行）；`Cold` = 观察语义（dispatch 返回 Pending，不泊产生方；处理器可后续作答，也可以永远不答）。
- **已落地的形状（拦截面，2026-10-13）**：调用帧 `mudra:before_navigate`，args `{url, ctx}`；应答 `{"verdict":"allow"|"deny"|"redirect","url"?}`。调用 id 由宿主分配，取自与事件日志行 id 不相交的空间。等待预算是宿主常量 200 ms（`Tier`/deadline 在 hello schema 里是预留位；库与 guest 尚未携带）。多扩展按注册序（名字序）执行，取**第一个非 allow**：`deny` 以 `{"blocked":true,"by":<扩展名>,"url":…}` 回复调用方且不执行动词；`redirect` 改写请求 url 后继续。闸门挂在两个导航**入口**——`/open` 与 `/nav` 的 `goto`——刻意**不挂** `run_verb`（guest 自己的 `host:invoke` 不能被它正在执行的那个判决拦住）。所有失败面（无声明会话、预算到期、通道断、队列满、应答不可解析）一律放行。
- **钩子（无进程的那一半，ns 9）**：页面侧 JS 作为数据——`name` / `matches` / `code` / `enabled` / `updated_at`，按 name 幂等 upsert，不 bump epoch。治理：`/hook_set` `/hook_list` `/hook_delete`、`mudra hook`、控制台的 `hooks` 视图。执行落点是既有的 `INJECT_JS` 注入桥：**一钩子一份**带字面子串守卫的源码（语法错只杀自己），`code` 逐字内联（不用 `eval`，严格站点 CSP）。写钩子会对运行中实例的活页做一次 best-effort 重注入；disable/delete 只影响未来文档——注入过的代码收不回，reload 重新武装。
- **未实现**：`store` 臂（扩展状态归扩展自己；宿主拥有的事件日志是投递设施，不是存储）与 `iterate` 各臂（空载；触及即返回显式错误值，等消费方报名）。
- 设计：`docs/ADR-extension-protocol.md`（双语）。被取代的草案：`docs/ADR-aura-probe-federation.md`。

## Web 控制台

`mudra ui`——leptos wasm（trunk 构建）在一个 `chromium --app` 窗口里。头部按钮切换三个面：**pages**（tag 森林树、ctx 选择器、过滤 chips）、**events**（只读，倒序，kind chips，按 `/events {tail}` 重拉）、**hooks**（与 CLI 同一组动词的 CRUD：开关 / 带二次确认的删除 / 新建 / 编辑，写完自持重拉——钩子写不 bump epoch）。不开第二通道。架构见 `docs/PANEL.md`。

## 构建与环境

- `cargo build --release` 产出 `mudra`（CLI）与 `mudrad`（daemon）。控制台 = `crates/mudra-panel/dist` 里的 trunk 产物（`wasm-bindgen-cli` 与 workspace 版本钉死对齐）。
- 拉起窗口需要 Wayland env：`export WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000`。niri socket 自动发现（`NIRI_SOCKET` 或 `/run/user/<uid>/niri.wayland-*.sock`）。
- git 源依赖：okm（schema 派生 + fjall 引擎）、induction（扩展帧与宿主半）。另有 `tokio`、`tungstenite`、`kdl`（config 解析）；控制台侧 `leptos` + `web-sys`。
- 剪贴板走 Wayland `wl-copy`（CDP 取聚焦窗口 URL → 管道）。

## 状态与已知问题

**已验证**：`--app` 无 omnibox / 标签条，在 niri 下就是一张纯页面 · CDP 能列出并 attach `--app` 窗口（它们就是普通 page target）· niri 的 `move-window-to-workspace` / `move-column-to-workspace` / `set-column-width` 存在，列宽可按 `layout.tile_size[0]` ÷ `logical.width` 读出 · `--app` + `--load-extension` 能载入 unpacked 扩展（2026-08-28 用 SurfingKeys 验证；2026-09-05 起是 mudra-keys 自身）· 自研 mudra-keys 在 `--app` 里全功能运行（四模式、hint、滚动、`:set`、状态栏——2026-09-05）· tag 森林 2026-09-05 落地，History 与补全 2026-10-01 · Rust 全栈 2026-09-28–29 落地（`cargo test --workspace` 138 测 + 活体冒烟）· 扩展面 2026-10-01/10-10（事件日志 + `/events` 回放、stdio 宿主、`host:invoke`）· 拦截判决与 Hook collection + 控制台 `events`/`hooks` 视图 2026-10-13。

**待验证（⏳）**：CDP 对 `chrome-error` 页的 reload / navigate（按机制应当可控）· "一个实例多窗口 ≈ 多标签"的资源等价性（未实测）· 精炼管线（html→md + 全文检索、朴素贝叶斯评分、RSS 捕获、LLM 汇总）。

**已知问题（延后）**：`--app` 里密码不保存（无 Linux keyring）——候选方案 `gnome-keyring` 或 `--password-store=basic`。

**旧实例残留陷阱**：上一版 schema 留下的 chromium 仍持有该 profile 的 `SingletonLock`；新拉起会把参数交给旧进程然后退出——debug 端口永不监听，daemon 把实例标成 down。若换 profile / schema 后实例"永远起不来"，先杀掉旧的 `profiles/*` chromium 进程。

**未决**：错误页的控制路由（那里注入不了扩展——只能 CDP；具体需要哪些操作未定义）· 下载 / 打印（chrome UI 缺口）· tag 森林的 app 层单选约束 · ML 打分（从手动评分学规则供审，不是黑箱）· 钩子的 disable/delete 是否需要一个"重载匹配页"的动词。

## 路线

见 `PLAN.md`——P4/P5 完成（代理与扩展、列宽记忆），P6 完成（新窗口拦截），P7a 完成（`WmExt` 接口 + niri 后端）；P7b（launcher 菜单）2026-09 废弃——控制台就是全部管理面。tag 森林架构与 Rust 重写于 2026-09 落地；扩展面 2026-10-13 落地。下一步：精炼阶段（P8–P12）。

## 设计文档在哪

| 文档 | 角色 |
|---|---|
| `https://github.com/orbsh/wiki/blob/main/keyboard-wm-browser-mode.md`（+ `-en.md`） | **为什么**——定位、硬约束、选型、层边界、被否决的路、开放问题 |
| `https://github.com/orbsh/wiki/blob/main/tag-forest.md` | 通用多轴内容模型 |
| `docs/SCHEMA.md`（+ `.zh-CN.md`） | 存储布局契约（store 改动的验收门） |
| `docs/KEYS.md` | 键位与扩展配置 |
| `docs/PANEL.md` | 控制台架构与 tag 森林抽象路线 |
| `docs/EXTENSIONS.md` | 扩展编写 |
| `docs/ADR-*.md` | 决策记录——`rust-fullstack`、`self-maintained-extension`、`no-favorites`（双语）、`extension-protocol`（双语）、`aura-probe-federation`（已被取代，双语） |
| `docs/WS-SYNC.md`、`docs/BASELINE.md` | 同步设计与 baseline 记录 |