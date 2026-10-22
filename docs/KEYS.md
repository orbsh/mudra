# mudra-keys 按键与配置

> 扩展根 `frontend/`：`extension/content.js`（模式机）、`shared/lib.js`（MudraConfig +
> 状态栏 widget，与 panel 共用）。零构建，`--load-extension=frontend/` 直接加载源码。

## 模式

四模式状态机，Esc 任何模式回 normal：

- **normal** — 全部键位生效；数字前缀累积（如 `30g`），状态栏 `count` 显示。
- **hint** — `f` 标记可见链接，输入 hint 前缀，**唯一候选即激活**（无需敲全），选中
  走 `open` 动词（本页就地导航）；`F` 同 hint，选中走 `new` 动词（另开一个 page 窗口）。
- **insert** — `i` 聚焦输入框；页内多个输入框时先进 hint 选择；Esc 失焦回 normal。
- **command** — `:` 触发；输入框在状态栏底部占满全宽，候选浮层在上方（最多
  `maxCandidates` 条），Tab 补全命令名，↑↓ 选择，Enter 执行，Esc 退出。
  **Tab 在参数型命令上不吞输入**：`:open` 上按 Tab 不是把行填成命令名，而是把行
  交给地址补全（活页 + 打开历史，prompt 变 `:open `），已键入的参数作为初筛查询；
  在该 picker 里 Tab 填选中的 URL（**候选列表保持不动**，选中跟着填进去的那一行走，
  再敲字符才重新按新输入过滤）、Enter 用 `open` 动词打开，Esc **取消**（不会把已键
  入的半截文本当 URL 打开）。`:set` 这类无 picker 的参数命令按 Tab 保留已键入的参数，
  只规范化命令名与分隔空格。

## 打开网页的两个动词

| 动词 | 语义 | 触发 |
|---|---|---|
| `open` | 在**本页**就地导航（`location.href`），并记一次历史 | `f`（hints）、`:open` picker 的 Enter |
| `new` | 另开一个 `--app` page 窗口（mudrad `/open`，归当前 ctx） | `F`（hints） |

两者的地址都经后端同一处 `spawn::normalize_url` 解析：缺 scheme 补 `https://`，
**单标签裸词补 `.com`**（`bing` → `bing.com`；`localhost`、带端口、带 userinfo、
带点的 host 一律原样）。`open` 的解析与历史记录由 `/visit` 承担（就地导航只刷新
既有 page 行，watcher 看不到新访问），console 页（面板本身）永不就地导航，一律走 `new`。

## 默认键位（均可被 keybindings 覆盖）

| key | command | 行为 |
|---|---|---|
| `f` / `F` | hint / hintNew | 链接 hints：`f`=open（本页），`F`=new（另开窗口） |
| `a` / `d` | back / forward | 历史后退 / 前进 |
| `i` | insert | insert 模式 |
| `j` / `k` | scrollDown / scrollUp | 纵向滚 `scrollStepLines` 行 |
| `h` / `l` | scrollLeft / scrollRight | 横向滚动 |
| `w` / `s` | pageDown / pageUp | 翻页（保留 `pageOverlapLines` 行重叠） |
| `g` / `G` | scrollTop / scrollBottom | 顶部 / 底部；`<N>g` 跳页面前 N% |
| `r` | refresh | 重载 |
| `t` | tag | 打 tag（弹 tag 输入） |
| `o` | open | 开 URL / 过滤页（console 路由） |
| `P` | pages | 切页（mudrad） |
| `:` | — | command 模式 |

## 配置

存储在 `chrome.storage.local`（默认值见 `shared/lib.js` 的 `MudraConfig.defaults`）：

`hintChars`（字母池，顺序即分配顺序）、`hintFontSize`、`statusHeight/Font/Fg/Bg`、
`insertFg/Bg`、`hintFg/Bg`、`statusBlur`（默认 12）、`keybindings`、`scrollStepLines`（默认 3）、
`pageOverlapLines`（默认 5）、`maxCandidates`（默认 10）。

**状态栏外观**：底色默认半透明（`bg "rgba(0,0,0,.55)"`、`insertBg "rgba(232,232,232,.62)"`、
hint 模式的深蓝 `rgba(32,64,128,.62)` 同理），配 `statusBlur`（毛玻璃/backdrop-filter 半径，
`0` = 只留半透明）。命令输入行与候选浮层是同一处外观，一起毛玻璃。
文字色默认 `"auto"`：按背景色算 WCAG 相对亮度，取黑/白里对比度高的那个（alpha 不参与判色——
半透明下真实底色是网页内容，静态算不出来，所以按配置的色相判）；写死颜色（`fg "#ffffff"`）
即锁死。hint 标签（`f` 的小方块）保持不透明：它要压在页面文字上。

cmd 模式就地修改（写入 storage，立即生效，重启实例保留）：

```
:set scrollStepLines 5        # 空格分隔
:set maxCandidates=20         # = 连接等价
:set keybindings.u=pageUp     # 键位覆盖；off/- 解绑
:set                          # 裸调用：状态栏列出全部配置
```

- 数字/布尔自动判型，其余按字符串；未知 key 提示 `unknown key`。
- `keybindings.<key>=<command>` 是覆盖 `defaultKey` 的通用机制——换键、互换、
  解绑都走它（如 `:set keybindings.u=pageUp` 后 `u` 即上翻页）。
