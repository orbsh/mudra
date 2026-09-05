# mudra UI 统一化与共享动作层 实施计划

日期：2026-09-05　状态：待执行（已与用户对齐方案）
上游设计：docs/ADR-self-maintained-extension.md、docs/PANEL.md §3（接口化）、docs/KEYS.md

## 目标

1. Python 侧抽共享动作层：`mudralib/ops.py`（focus/tag/pages 语义唯一实现），
   `db.py` 成为唯一 SQL 所在地，消除 mudra.py / mudrad.py 重复逻辑。
2. 前端统一 Solid 运行时：三个 ESM vendor 拼成单一全局 `solid-bundle.js`（IIFE，
   `window.MudraSolid`），panel 与扩展共用同一份（接法 A，panel 删 importmap）。
3. `frontend/shared/tags.js`：Capsule / RankAxis / chip 从 panel 抽出，panel 改引用；
   扩展状态栏 mode 后渲染当前页 tag 胶囊串（可点击：段切换同级 / ✕ 摘除 / ＋加子级）。
4. `MudraUI` shadow root：bar / cmd 弹层 / 胶囊 / 未来 AI 对话框统一挂载点。
5. hintStrings 改均匀 base-N 编码（vimium 同款），修复多链接下首字符偏斜。
6. 文档同步（repo docs + wiki keyboard-wm-browser-mode.md）。

已定交互边界：cmd 弹层过滤保持文本行 + 键盘模型（↑↓/Tab/Esc/Enter），
下级节点补全后续再做；bar 胶囊仅 normal/insert/hint 模式显示，command 模式被输入行接管。

## 任务

### T1 mudralib/ops.py — 共享动作层

- 新建 `mudralib/ops.py`：
  - `focus_page(page_id)` — 采用 mudrad 版语义（CDP activate + 标题/域名匹配 niri
    窗口，fallback 首窗口；niri 失败不阻塞）
  - `focus_ctx_query(ctx, query)` — CLI focus 语义 = ctl.find + focus_page
  - `tag_page(ctx, url, tag_name)` / `tags_children(parent)` — 自 mudrad.ctl_tag/ctl_tags 迁入
  - `list_open_pages(ctx)` — 自 ctl_pages 迁入
  - `ctx_for_tab(tab_id, url)` — 自 mudrad._ctx_for_tab + _is_console 迁入
- mudrad 的 handler 改为瘦转发（HTTP 参数 → ops 调用），mudra.py 的 cmd_focus/cmd_tag 同。
- 验证：`mudra focus <query>`（CDP+niri 双切换）、面板点页聚焦、扩展 `t` 打 tag、
  `:o` 页列表，四路行为不回归。

### T2 db.py SQL 下沉

- mudrad 内嵌业务 SQL（_sync_infos / _close_target / ctl_* 残留）迁为 db.py 函数：
  `page_upsert`、`page_set_closed_by_target`、`page_set_closed_all`、`page_next_position`、
  `page_tag_toggle`、`tag_children`、`pages_open`、`instance_running`、`page_by_instance_target`。
- 原则落地：**SQL 只出现在 db.py**。
- 验证：开/关页、tag toggle、pages 列表行为一致；`grep -rn 'SELECT\|INSERT\|UPDATE' mudrad.py mudra.py` 为空（db.py 除外）。

### T3 solid-bundle.js 拼接

- 新建 `frontend/shared/vendor/solid-bundle.js`：IIFE 包裹三文件全文，
  删 3 行 import + 2 行 export/re-export，尾部
  `window.MudraSolid = { h, render, ...{createSignal, createMemo, For, Show, ...} }`。
- 删除 `solid.js` / `solid-web.js` / `solid-h.js` 三件套。
- panel 接法 A：`index.html` 改 `<script src="/shared/vendor/solid-bundle.js">`
  （普通 script，删 importmap 与 type=module）；`app.js` 头部改
  `const { h, render, createSignal, createMemo, createEffect, For, Show } = window.MudraSolid;`。
- manifest content_scripts js 追加 `/shared/vendor/solid-bundle.js`（lib.js 之前）。
- 验证：panel 渲染正常（tag 树、胶囊、过滤）；扩展四模式全功能 CDP 冒烟。

### T4 MudraUI shadow root + MudraBar Solid 化

- `frontend/shared/lib.js`：
  - 顶层 `MudraUI`：挂 `div#mudra-ui-root`（shadow DOM open），bar / 弹层 / 胶囊 /
    未来对话框全挂此 root；`MudraBar.mount` 改为在 root 内挂载，对外 API
    （mount/render/openCommand/unmount）不变。
  - `MudraBar.render` 内部改 signal + h() 驱动（slots 变 solid 组件），
    `openCommand` 的候选 renderList 同换 h()。
- content.js 调用点零改动（API 兼容）。
- 验证：状态栏四模式配色/内容、cmd 弹层 Tab 补全、flashBar 时序（pickCmd 不覆盖）
  全回归。

### T5 shared/tags.js 组件抽取

- 新建 `frontend/shared/tags.js`（全局 `MudraTags`）：自 panel app.js 抽
  `Capsule`（含路径段切换菜单逻辑）、`RankAxis`、`chip`。
- 宿主接口注入：`MudraTags.host = { roots, byId, setTags, pageOf }`——panel 传
  WS signal 包装，扩展传 sw messaging 包装（promise → solid signal 桥）。
- panel `app.js` 改用 `MudraTags.*`（删除本地实现）。
- manifest js 追加 `/shared/tags.js`；ui.py 静态路径无新增（同 /shared）。

### T6 扩展 bar tag 胶囊 + sw 协议升级

- sw `status` 一次往返补齐 tag 节点数据：`[{id, name, path, parent_id, rank}, ...]`
  （后端 db.tag_children 树查询已有，加 `tag_path_by_ids` 查询）；新增
  `tag_remove` 转发（走 ctl_tag toggle 语义）与 `tag_children` 树查询转发。
- content.js：`pageTags` 从字符串数组升级为节点数组；bar 左槽 mode 后渲染
  `<Capsule>` 串（MudraTags 组件 + MudraSolid.h）。
- 验证：扩展内 `t` 打 tag 后胶囊即时出现、点段换同级、✕ 摘除（与 panel 双向同步经
  pages_changed 广播）。

### T7 hintStrings 均匀编码

- `content.js hintStrings` 改 base-N 数值编码（vimium 式）：
  `n 个链接 → 长度 ⌈log_|chars| n⌉，第 i 个 = i 的按位表示`；
  前缀过滤 / 唯一即激活 / Backspace 逻辑不变。
- 验证：CDP 注入 30 链接页面，序列长度均匀、首字母分布覆盖全池。

### T8 文档同步

- repo：docs/KEYS.md（胶囊交互、hint 编码）、docs/PANEL.md（solid-bundle、tags.js
  宿主接口、MudraUI）、docs/ADR-self-maintained-extension.md（加载形态更新）、
  docs/EXTENSIONS.md（WmExt 之后 ops 层一节）、PLAN.md（P7b 增补、新 P7c 记录）、
  README 实现细节。
- wiki `keyboard-wm-browser-mode.md`：§二选型 / §三架构（SurfingKeys → 自研
  mudra-keys，扩展根 frontend/，零构建 MV3 + solid 全局 bundle）、§八实现设计
  （组件：面板与扩展共用 shared/tags.js；bar 胶囊；ops 共享动作层）、§九已验证
  （mudra-keys 全功能已实测）、§十开放问题（双控制路由改述为 CDP error-page 路由）。
- infoflow-refinement.md / README.md（wiki）如有 SurfingKeys 引用一并更新。

## 风险与注记

- solid-bundle 拼接是唯一手工脆弱点：拼接后需 node --check + 双宿主冒烟。
- shadow DOM 内 fixed 定位不受影响（bar/弹层均为 position:fixed），hint 标记仍在
  页面层（不经 shadow）。
- 扩展内 solid signal 与 chrome.storage 异步读取：bar render 改用 createEffect，
  替换现有手写 render 调用点时保持 render(data) 语义为 signal 批量赋值。
- ops 收敛后 mudra.py 的 `_focused_page`/`_tag_set` 等残留直连逻辑一并迁 db/ops。
