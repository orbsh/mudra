# mudra 管理面板（panel）与 tag-forest 抽象化

> 定位：本文是 **mudra 工程向**的面板设计 + tag-forest 抽象化需求文档。通用 tag
> forest 模型在 `~/.hermes/wiki/tag-forest.md`（不在此仓）。两者分工：wiki 讲"模型
> 是什么"，本文讲"mudra 如何实现它、以及如何把它抽成可复用库"。

## 一、为什么从 launcher 走向面板

launcher（walker/elephant menus）适合**热路径单动作**：呼出 → 选一 → 回车，几秒内完成
聚焦/关闭/移动。但 tag 森林的**富操作**在 launcher 里是削足适履：

- tag 多选批量指派、评分轴可视化、胶囊路径切换——这些需要"停留、浏览、多点"的界面。
- launcher 一次只交一个选择，无法表达"边看页面列表边改标签"的连续工作流。

**交互模型演进**：早期定稿是 `p/t/a/s` 四个 launcher 前缀（Page/tag/Action/sort）。
实际落地时，tag 多选（`t`）、排序（`s`）、动作（`a`）从 launcher **移交给了面板**。
launcher 只保留 `p`（Page 热路径单动作）。面板成为 tag-forest 主力交互面。

## 二、面板现状（2026-08 落地，2026-09-05 零构建化）

- **技术栈**：solidjs（hyperscript `h()`，无 JSX）+ python `mudrad` 内置 HTTP(静态) + WebSocket
  双服务（`mudralib/ui.py`）。**零构建**：无 vite/npm，源码目录即产物（ESM + 内联
  importmap，`/shared/vendor/solid*.js`）。前端位于 `frontend/ui/`，共享库
  （solid vendor、后续公共组件）在 `frontend/shared/`——panel 与 mudra-keys 扩展同根引用。
- **入口**：`mudra ui` 确保 mudrad 在跑（它持有面板服务），spawn 一个 chromium `--app`
  固定窗载入 `http://127.0.0.1:9299/`。**固定窗，非浮动**——用户直接切过去，不需要
  WM 浮动居中。
- **端口**：HTTP `:9299`（静态，`/shared/*` 由 `translate_path` 映射到 `frontend/shared/`）、
  WS `:9300`（数据通道）。前端按 `location.port + 1` 连 WS 并**自动重连**。

### WS 协议（JSON 请求/响应，`id` 匹配）

| op | 请求 | 响应 |
|---|---|---|
| `forest` | — | tag 深树（任意深度递归，root 带 `rank_axis`）+ sessions |
| `pages` | `{session}` | 该会话开页（含 `tag_ids`/`parent_id`/`opened_at`/`target_id`） |
| `set_tags` | `{page_id, tag_ids}` | 整组替换该页 tag |
| `focus` | `{page_id}` | CDP 激活目标 + 定位其 niri 窗口聚焦 |
| `close` | `{page_id}` | 删页 + 关目标 |
| `create_tag` | `{parent_id, name}` | 胶囊添加子级，返回新 id |
| `shot` | `{page_id}` | CDP `Page.captureScreenshot` → base64 data URL |

### UI 布局（按用户规格）

- **头部**：会话选择器、时间排序（新→旧/旧→新）、tag 过滤 chip 区。
- **主体**：page **树**（`parent_id` 打开关系，可折叠），每页两行：
  - 行1：标题链接（点击 → 拦截切到对应窗口；悬停 → 显示窗口截图）。
  - 行2：时间 + 三评分轴 + 普通 tag 胶囊 + 添加按钮。
- **评分轴（rank 树）**：importance=★、quality=♥、urgency=🔥，各 5 档；点击第 k 档设
  该级；**选中原色、未选中加灰**。

**评分轴的理论依据**（三评分轴背后的维度，避免当三个无关标量用）：

三个评分轴不是并列的三个"普适度"，它们归属**两个正交维度：针对性 × 概括性**。

- **针对性（情境耦合强度）**：知识与特定情境（人群 / 时间 / 地理 / 项目）的耦合深度。
  - `importance` 和 `urgency` 是**同一"相关性"在两个轴上的投影**，不是两种无关评分——
    importance = 人群轴上的相关性，urgency = 时间轴上的相关性（时间窗窄 = 当下极相关、窗口一过崩塌）。
  - 针对性强的知识对该情境的"相关人"重要、对无关人不重要——重要性天然是 person-relative 的，没有脱离观测者的绝对重要性。
- **概括性（知识本体覆盖宽度）**：内容本身覆盖范围的广度——这是**独立正交的第三轴**，与受众和时间都无关。
  - `quality` 落在这根轴上：一篇水平差但此刻极必要的资料，quality 低但 importance 高——两者可以分离。

一句话：`importance / urgency` 同源（相关性的两轴投影），`quality` 正交独立。评分时别把三个当并列的三个"重要性"。这与 [tag-forest](~/.hermes/wiki/tag-forest.md) 的 rank 语义一致。
- **胶囊（普通 tag）**：每段一级路径，点击段 → 同级切换菜单（切换后深路径取消）；
  头 ✕ 删此 tag、尾 ＋ 加子级；行尾 ＋ 指派已有 tag。

## 三、抽象化需求（tag-forest 库化）

目标：把"谁在组织内容"与"内容是什么"解耦，让 **任何 tag-forest 方案都能复用**
mudra 这套 tag 组织 + 面板交互，只换后端对接。

原始缺口（用户 2026-08 提出）："tag-forest 可以做成库，只要是 tag forest 的方案都可
以用，后端对接的接口不同。还有自定义显示区，比如 mudra 可以显示 page 所在的工作区，
点击、悬停的操作也定义成接口。"

### 3.1 三层抽象

**L1 后端接口（`ForestBackend`）**——库只认抽象输入，不认具体表：

```python
class ForestBackend:
    def nodes(self) -> Iterable[TagNode]      # id,parent_id,name,alias,rank,required,isolated,hidden
    def tag_ids(self, item_id) -> list[int]   # 某 item 当前 tag
    def assign(self, item_id, tag_ids)        # 整组替换
    def create(self, parent_id, name) -> int  # 建 tag 节点
    def items(self, filter) -> Iterable[Item] # 被贴 tag 的对象（item_id + 任意自定义字段）
```

mudra 提供 `SqliteForestBackend`（包住现有 `db.py`）。库负责复用：树递归构建、rank 轴
识别、段路径切换、create 等公共逻辑。

**L2 前端显示/操作接口**——面板只渲染"通用 tag 树"，item 的自定义字段与交互由适配
器注入：

```js
configure({
  forest: backendSDK,              // WS op 封装
  itemView: { rows: [...] },        // 自定义显示区（mudra: 标题/时间/工作区…）
  actions: { clickItem, hoverItem },// 点击/悬停 = 可注入接口
})
```

**L3 边界**——抽出的库不绑 mudra 的名字/表/CDP/niri。mudra 只是第一个适配器。

### 3.2 两种落地方案（取舍）

- **方案 A 完整库化**：把 forest 逻辑抽成独立可复用库（+ 前端通用渲染件），mudra 作
  适配器接入。改动大、周期长，但 scratch（任务）、graph-memory（节点）都能套。
- **方案 B 接口先行**：先在本仓定义接口（后端 `ForestBackend` + 前端配置对象），mudra
  内部按接口重构，验证设计正确后再独立成包。

> **定稿（2026-08）**：走 **方案 B 接口先行**——本仓先立接口、mudra 按接口重构，
> 验证后再考虑独立。独立库落在 `tag-forest.md` 的通用模型层。

### 3.3 待办清单

- [ ] `mudralib/forestlib.py`：定义 `ForestBackend` 协议 + `SqliteForestBackend` 适配器
- [ ] 前端 `configure()` 接口落地，替换硬编码 item 视图
- [ ] 自定义显示区示例：mudra 页显示所在工作区（`niri` workspace_id）
- [ ] `mudra ui` 与 `set_tags` 迁移到新接口，回归验证
- [ ] 接口验证通过后，评估独立成库（方案 B → A 的桥）

## 四、相关工作

- 通用 tag-forest 模型：`~/.hermes/wiki/tag-forest.md`（含五条递归 CTE 查询、rank 树
  设计、isolated 隔离、信息流）。
- 浏览器使用模式：`~/.hermes/wiki/keyboard-wm-browser-mode.md`。
- 扩展集成（WmExt 可插拔先例）：本仓 `docs/EXTENSIONS.md`。