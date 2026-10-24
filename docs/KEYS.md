# mudra-keys 按键与配置

> 扩展根 `frontend/`：`extension/content.js`（模式机）、`shared/lib.js`（MudraConfig +
> 状态栏 widget，与 panel 共用）。零构建，`--load-extension=frontend/` 直接加载源码。

## 模式

四模式状态机，Esc 任何模式回 normal：

- **normal** — 全部键位生效；数字前缀累积（如 `30g`）。前缀与模式名都不再画在条上（见
  「状态栏外观」），累积过程只能从命令效果看出来。
- **hint** — `f` 标记可见链接，输入 hint 前缀，**唯一候选即激活**（无需敲全），选中
  走 `open` 动词（本页就地导航）；`F` 同 hint，选中走 `new` 动词（另开一个 page 窗口）。
  两个键底色相同，条上不再写 `hintnew`——按下之后的区别只剩你选中元素时发生的事。
- **insert** — `i` 聚焦输入框；页内多个输入框时先进 hint 选择；Esc 失焦回 normal。
- **command** — `:` 触发；弹出**居中的浮动面板**（上沿在视口高度的 1/3 处，宽
  `min(720px, 80vw)`）：输入行在面板顶部，候选列表向下展开（最多 `maxCandidates` 条）。
  **Tab 在多个候选之间移动选中项**（循环，Shift+Tab 反向），
  行不动：行是用户敲的东西，高亮是 Enter 会取的东西——多候选时填值会触发重过滤，列表
  在按键底下消失，Enter 可能取走一行没看过的候选。**唯一候选时 Tab 反而补全到行里**
  （没得选的时候，补全是这个键唯一的用处，否则 `:o` + Tab 看起来是死的）。Enter 执行，
  Esc 退出。
  **`open` / `new` 的行就是地址栏**：行里出现空格后，候选换成"活页（仅 console）+ 打开
  历史"，Tab 走、Enter 取高亮的那条 URL；没有匹配的候选时 Enter 用敲进去的文本。空行
  Backspace 退出命令模式（不再有第二层 picker）。

## 打开网页的两个动词

| 动词 | 语义 | 触发 |
|---|---|---|
| `open` | 在**本页**就地导航（`location.href`），并记一次历史 | `f`（hints）、`o`（≡ `:open `）、`:open <url>` 回车 |
| `new` | 另开一个 `--app` page 窗口（mudrad `/open`，归当前 ctx） | `F`（hints）、`O`（≡ `:new `）、`:new <url>` 回车 |

`o` / `O` 与 `:open` / `:new` 是同一件事：按 `o` 就是把 `open ` 打进命令行（`O` 打
`new `），候选（活页 + 打开历史）列在同一行上方；Tab 走选中、Enter 取高亮的那条 URL，
没有匹配的候选时 Enter 用敲进去的文本——所以带不带参数都只有一次回车。`new` 不列活页：
新窗口没有可跳转的目标。

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
| `o` | open | 命令行打 `open `（≡ `:open`），Enter 在本页打开 |
| `O` | new | 命令行打 `new `（≡ `:new`），Enter 另开窗口 |
| `P` | pages | 切页（mudrad） |
| — | q | 关闭当前页（`:q`，无默认键） |
| `:` | — | command 模式 |

`:q` 关的是**说这句话的那个 tab**：service worker 直接 `chrome.tabs.remove(sender.tab.id)`，
不做 URL 匹配（mudrad 的 `/close_page` 收的是子串，会认错页），所以控制台也能 `:q`（它只是
另一个 tab）。mudrad 的 watcher 收到 `Target.targetDestroyed` 自己把 page 行标 closed，
不需要扩展回报。

## 配置

存储在 `chrome.storage.local`（默认值见 `shared/lib.js` 的 `MudraConfig.defaults`）：

`hintChars`（字母池，顺序即分配顺序）、`hintFontSize`、`statusHeight/Font/Fg/Bg`、
`insertFg/Bg`、`hintFg/Bg`、`statusBlur`（默认 12）、`statusOpacity`（默认 0.15）、`keybindings`、`scrollStepLines`（默认 3）、
`pageOverlapLines`（默认 5）、`maxCandidates`（默认 10）。

**优先级**：`config.kdl`（仓库层 + `~/.config/mudra/config.kdl` 用户层逐键合并）是源——服务
worker 每次同步都跟上文件，改文件即改外观；`:set` 写过的键会被钉住（storage 的 `setKeys`），
此后文件不再覆盖它。`:set <key> -` 把该键交还给文件（下一次 SW 同步时按文件值落地，其间用内置
默认）。

**状态栏形态**：右下角一块单行高的 chip（`#mudra-bar`），宽度随文字长（上限 `70vw`），
四角 4px 圆角，**只显示右侧那段状态**——`title · url · 滚动百分比`（消息态改成一整行提示）。
ctx、数字前缀、模式名、tag 胶囊都不再画：chip 让掉了左半边，模式只从底色色相看出来。
**颜色**：底色默认半透明（`bg "rgba(0,0,0,.55)"`、insert 模式另用
`insertBg "rgba(232,232,232,.62)"`、hint 模式的深蓝 `rgba(32,64,128,.62)` 同理），配
`statusBlur`（毛玻璃/backdrop-filter 半径，`0` = 只留半透明）。**条本身的透明度单独一个
旋钮**：`bar.opacity`（默认 0.1）——色相仍由 `bg` 给，alpha 由它接管。
**命令模式是另一块面**：`:` 弹出居中浮动面板（`#mudra-cmdpanel`：上沿在 `100vh / 3`、宽
`min(720px, 80vw)`、6px 圆角 + 投影），输入行（`#mudra-cmdline`）在面板顶部，候选列表
（`#mudra-cmdbox`）向下展开。面板与 chip 共用 `bg` 的色相 + `bar.opacity` + `statusBlur` +
`fg` 的文字色；面板开着时 chip 隐去（`visibility:hidden`），免得两处读数。
`insertBg`/`insertFg` 只影响 insert 模式下的 chip。
文字色默认深灰 `#666666`；写 `"auto"` 则按背景色算 WCAG 相对亮度、取黑/白里对比度高的
那个（alpha 不参与判色——半透明下真实底色是网页内容，静态算不出来，所以按配置的色相判）；
写死别的颜色即锁死。hint 标签（`f` 的小方块）保持不透明：它要压在页面文字上。

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
