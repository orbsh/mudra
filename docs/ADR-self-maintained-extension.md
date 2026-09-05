# ADR: 自维护 mudra-keys 扩展（放弃复用 SurfingKeys / Vimium 等已有扩展）

日期：2026-09-05　状态：已定案并实施（`frontend/`，扩展根 = `frontend/`，共享库在 `frontend/shared/`）

## 问题

mudra 需要"每个页面都可键盘驱动"：链接 hints、前进后退、状态栏、命令模式、
tag 打标。这是浏览器内行为，只能由浏览器扩展承担。选择是：复用已有扩展
（SurfingKeys、Vimium 等），还是自写。

## 决定：自写 mudra-keys

### 为什么不用已有扩展

1. **功能错配**。第三方扩展 95% 的功能（多搜索引擎前缀、OmniBar、查找、
   代理切换、markdown 导出…）我们不用；而 mudra 需要的核心能力——所有
   "开页/打标/跳页"动作必须经过 mudrad 控制接口（后端是唯一控制点）——
   恰好是第三方扩展**不可能自发提供**的。
2. **开页路径必须改**。默认扩展用 `chrome.tabs.create` / `window.open`
   开新页，绕过 mudrad，产生野生 tab/窗口，破坏"一切生命周期走后端"的
   不变量。复用意味着每次上游更新都要重放补丁——维护负担随上游版本线性增长。
3. **MV3 迁移不受我们控制**。SurfingKeys mv3 分支需 webpack 构建（我们已
   打过 transpileOnly 补丁才编过），构建链是外部的；Vimium 生态同样有各自
   的构建/打包假设。而 mudra 的扩展形态约束是：**MV3、零构建、仓库内源码
   目录直接 `--load-extension`**。
4. **自写成本低于接入成本**。核心形态（hints + 模式 + 状态栏 + SW 桥）约
   300–500 行纯 JS，无依赖；对比"克隆上游仓库 + 修 TS 编译 + 维护开页
   补丁 + 跟踪上游破坏性变更"，自写是更小的一笔账。

### 形态约束

- **MV3 + 零构建**：manifest 直接指向源码 JS；`--load-extension=<repo>/frontend`
  （扩展根 = `frontend/`：`manifest.json` 在根，`extension/` 放 content/SW，
  `shared/` 放与 panel 共用的库——manifest 以 `/shared/lib.js` 绝对路径引用跨子目录共享代码）。
- **模式机与按键**（2026-09-05）：四模式 FSM（normal / hint / insert / command），
  qutebrowser 式状态栏（左 ctx·count·mode·tags，右 title/url/scroll%）。键位与全部
  行为参数（hintChars、scrollStepLines、pageOverlapLines、maxCandidates、keybindings
  覆盖表）存 `chrome.storage.local`，cmd 模式 `:set` 就地修改（`:set scrollStepLines 5`、
  `:set keybindings.u=pageUp`、裸 `:set` 列出全部）。
- **SW 只做桥**：service worker 不含业务逻辑，content script 消息 →
  mudrad HTTP（`127.0.0.1:8899`）。开页、打标、页列表、跳页全部由 mudrad
  执行（tabId 路由回所属 ctx）。状态、广播、生命周期的所有权都在后端。
- **开发模式**：chromium 对源码目录扩展有深层缓存（SW ScriptCache /
  Code Cache / HTTP Cache），改文件不会自动生效。`mudra dev on` 打开后，
  每次 spawn 前清缓存，冷启动换"改动立即可见"。密集开发期常开。

## 后果

- 扩展是 mudrad 的一等客户端而非"装进浏览器的外来者"：`/open` `/tag`
  `/pages` `/focus_page` `/ctx_status` 均为扩展场景设计。
- 轻量操作（跳页、打标、开链接）在任意页面就地完成，不必打开 console ui；
  console ui 仍是总控（跨 ctx 树视图 + 富操作），两者数据同源（同一 DB + 同一后端）。
- 代价：浏览器键盘体验的通用功能（如全页查找）需要时自己补——接受，
  因为每一行都在自己仓库里，无上游维护负担。
