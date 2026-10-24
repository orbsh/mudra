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
- **command** — `:` 触发；弹出**居中的浮动面板**，位置和大小全按比例：水平居中、上沿在视口
  高度的 1/3 处、最大高度也是 1/3（即到 2/3 处为止）、宽 `50vw`。输入行在面板顶部（不再有
  `:` 前缀），候选列表占满余下空间并**在面板内滚动**——候选条数不设上限。
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
| `y` `y` | — | 复制当前页地址到剪贴板 |
| `y` `t` | — | 复制当前页标题到剪贴板 |
| `?` | — | 键位表（在状态条上方弹出，超出高度可滚动） |
| — | q | 关闭当前页（`:q`，无默认键） |
| `:` | — | command 模式 |

`:q` 关的是**说这句话的那个 tab**：service worker 直接 `chrome.tabs.remove(sender.tab.id)`，
不做 URL 匹配（mudrad 的 `/close_page` 收的是子串，会认错页），所以控制台也能 `:q`（它只是
另一个 tab）。mudrad 的 watcher 收到 `Target.targetDestroyed` 自己把 page 行标 closed，
不需要扩展回报。

## 键序列与键位表

`y` 不单独成命令，它是**序列开头**：按下去之后状态条上方弹出一枚小菜单（`y y` 复制地址、`y t`
复制标题），等第二个键，最多等 `sequence { timeout }`（默认 1200 ms）。第二个键命中就执行并关掉
菜单；按 Esc 放弃；按别的键则当作序列没开始过、照常执行（不会吞键）——吞掉的键普通用户读作
"键没了"，这是不可接受的。等待期间没有模式切换：仍是 normal，只是下一键先由序列消费。

菜单和 `?` 的键位表是同一个组件（`mudra-menu`）：右下角、紧贴状态条上沿、宽度随内容、高度上限
`66vh`，超出就在菜单内部滚动（2px 纯滑块，与命令面板同一套规则）。配色也直接用命令面板那组
`cmdBg`/`cmdOpacity`/`cmdFg`——淡黄毛玻璃只有一份定义，改一处三处一起变。`?` 列出全部绑定
（含当前 `keybindings` 覆盖后的实际键）、两个序列、`:`、`Esc` 与数字前缀，按 key 排序。

复制走 `navigator.clipboard.writeText`；页面上下文被拒时（部分 `chrome://`、`file://`）退回隐藏
textarea + `execCommand("copy")`。成功与否都在状态条上落一行字（`copied url: …` / `copy failed`），
复制失败静默是查不出来的。

## 配置

存储在 `chrome.storage.local`（默认值见 `shared/lib.js` 的 `MudraConfig.defaults`）：

`hintChars`（字符池，顺序即分配顺序；默认 `asdfghjkl;qwertyuiopzxcv`，含 `;`，池里每个
字符都必须能敲进去）、`hintFontSize`、`statusHeight/Font/Fg/Bg`、
`insertFg/Bg`、`hintFg/Bg`、`hintOpacity`（默认 0.45——高到能压住页面花纹，低到能透出底下的字）、`statusBlur`（默认 12）、`statusOpacity`（默认 0.1）、
`cmdBg`/`cmdFg`/`cmdOpacity`/`cmdScrollbar`（命令面板的色相、文字色、alpha、候选列表滚动条宽度 px）、
`progressHeight`（默认 2）/`progressColors`（多段渐变色，空格分隔的多色列表）、
`seqTimeout`（序列等待毫秒数，默认 1200）、
`keybindings`、`scrollStepLines`（默认 3）、`pageOverlapLines`（默认 5）。

**优先级**：`config.kdl`（仓库层 + `~/.config/mudra/config.kdl` 用户层逐键合并）是源——服务
worker 每次同步都跟上文件，改文件即改外观；`:set` 写过的键会被钉住（storage 的 `setKeys`），
此后文件不再覆盖它。`:set <key> -` 把该键交还给文件（下一次 SW 同步时按文件值落地，其间用内置
默认）。

**状态栏形态**：右下角一块单行高的 chip（`#mudra-bar`），宽度随文字长（上限 `70vw`），
四角 4px 圆角，**只显示右侧那段状态**——`title · url · 滚动百分比`（消息态改成一整行提示）。
ctx、数字前缀、模式名、tag 胶囊都不再画：chip 让掉了左半边，模式只从底色色相看出来。
**颜色**：chip 底色默认半透明（`bg "rgba(0,0,0,.55)"`、insert 模式另用
`insertBg "rgba(232,232,232,.62)"`、hint 模式的深蓝 `rgba(32,64,128,.62)` 同理），配
`statusBlur`（毛玻璃/backdrop-filter 半径，`0` = 只留半透明；chip、命令面板、hint 标签
共用这一个半径）。**条本身的透明度单独一个
旋钮**：`bar.opacity`（默认 0.1）——色相仍由 `bg` 给，alpha 由它接管。
**命令模式是另一块面，颜色也是它自己的**：`:` 弹出居中浮动面板（`#mudra-cmdpanel`：上沿
`100vh / 3`、最大高度同为 `100vh / 3`（到 2/3 处为止）、宽 `50vw`、6px 圆角 + 投影），输入行
（`#mudra-cmdline`）在面板顶部，候选列表（`#mudra-cmdbox`）占满余下空间并在面板内滚动。
颜色走 `command { bg … fg … opacity … }`：`bg` 是**色相**（默认 gruvbox 淡黄 `#fbf1c7`），
alpha 由 `opacity` 单独接管（默认 0.62），`fg` 默认深灰 `#3c3836`。**色相与 alpha 分开是
为了让毛玻璃真的看得见**——alpha 接近 1 时 `backdrop-filter` 照样在跑，但 8% 的透光量等于
没有；要更虚化就调低 `opacity`。候选列表的滚动条是 `scrollbar` px 宽（默认 2）的**纯滑块**：
无轨道、无边框、无圆角，滑块色 = `fg` 的 15% alpha（比面板底色只深一点）。面板带 `statusBlur` 毛玻璃；面板开着时
chip 隐去（`visibility:hidden`），免得两处读数。`insertBg`/`insertFg` 只影响 insert 模式下的 chip。
文字色默认深灰 `#666666`；写 `"auto"` 则按背景色算 WCAG 相对亮度、取黑/白里对比度高的
那个（alpha 不参与判色——半透明下真实底色是网页内容，静态算不出来，所以按配置的色相判）；
写死别的颜色即锁死。**hint 标签（`f` 的小方块）也是毛玻璃**：底色 `hintBg`（默认 gruvbox
淡黄 `#fbf1c7`，同样是色相）、alpha 走 `hint.opacity`（默认 0.65，高于 `bar.opacity` 但
远低于 1——要能看见桌面上透过来的模糊，同时压得住底下的页面文字），模糊半径同 `statusBlur`；
文字色按 `hintFg`（`"auto"` 时由 `hintBg` 亮度取黑/白）。insert 模式的多输入框 picker 用同一套。

**加载进度条**：页面顶部一条 `progress { height … colors … }` 的横条，默认 2px 高、gruvbox 六色
从左到右的多段渐变（`#fb4934 #fe8019 #fabd2f #b8bb26 #83a598 #d3869b`，空格分隔的多色列表；
两个以上才成渐变，一个色就是纯色）。它由 content script 在 `document_start` 挂上，所以覆盖整段
加载：宽度自己爬到 72%（没有进度 API，只有 load 事件），`load` 后补到 100% 再淡出移除；20 秒
还没加载完就当作结束，免得永远卡在 72%。`height 0` 等于关掉。渐变按视口宽度铺满、被增长的
盒子裁切，所以颜色钉在视口上，不跟着进度条右沿跑。

**命令行不是 `<input>`**：`#mudra-cmdinput` 是 `contenteditable="plaintext-only"` 的 div。原因不是审美——Chrome 会在任何它认得出名字的文本控件上方弹自带的补全下拉（autofill / 输入历史），那个菜单正好盖在候选列表上，而没有任何属性组合能长期压住它；contenteditable 根本没有原生 autofill，所以第二个菜单不可能出现。代价是 caret、取值、粘贴都要自己实现（`MudraLine`：`value()` 取 `textContent`、粘贴/拖放只收纯文本、Enter 归宿主用来选候选）。

**元素查询和滚动目标都穿透 shadow root**：`chrome://settings`、`history`、`downloads`、`flags` 这些 WebUI 页把内容放进层层 shadow root（`flags` 实测 655 个），网站也越来越多这么干；`document.querySelectorAll` 只看 light DOM，会报"页面上没有链接"。所以 `f` 的候选集、`i` 的输入框集都走 `deepQueryAll`（递归进 `shadowRoot`）。滚动同理：`window.scrollBy` 在 WebUI 上滚不动任何东西，滚动目标由 `scrollTarget()` 解析——文档自己能滚就用它（绝大多数网站，零成本），否则在视口中心做命中测试、沿 flat tree（`parentElement`，再退到 shadow host）找到第一个真正在滚的容器；结果缓存 500ms，因为状态条每次滚动事件都会重算百分比。状态条的滚动百分比跟的就是同一个目标。

**`chrome://` 页面上的脚本由 SW 注入，不写进 manifest 的 matches**：`content_scripts.matches` 里出现 `"chrome://*/*"` 会让 Chrome 在**没带** `--extensions-on-chrome-urls` 的启动方式下直接拒载整个扩展（`Invalid value for 'content_scripts[0].matches[1]': Invalid scheme.`）——结果是浏览器里没有扩展 + 启动报错气泡。同一个 pattern 写在 `host_permissions` 里是合法的，所以改成：manifest 只声明 `<all_urls>`，SW 监听 `webNavigation.onCommitted`，对 `chrome://` 的主框架用 `chrome.scripting.executeScript` 注入 `shared/lib.js` + `extension/content.js`（顺序不能反，content.js 用 lib.js 定义的全局）。缺开关时只有这一个调用失败并被 catch 成一条日志，扩展照常加载——正常页面不受影响，`chrome://` 页面只是没有键位。

**`--extensions-on-chrome-urls` 必须配 `--test-type`**：这个开关在 Chromium 的 bad-flag 名单里（它确实给了扩展更大权力——能改 `chrome://settings`），所以每个窗口顶部会永久挂一条 infobar：「You are using an unsupported command-line flag: --extensions-on-chrome-urls. Stability and security will suffer.」。在 `--app=` 窗口里它直接压在页面内容上，虽然能点 × 关掉，但每次启动都回来。`--test-type` 只抑制这条启动提示，开关本身照常生效（crbug 40205764、41109334）；像素验证：同一页面同一 profile，不带 `--test-type` 时窗口顶上有条，带上之后页面内容顶到窗口上沿。

cmd 模式就地修改（写入 storage，立即生效，重启实例保留）：

```
:set scrollStepLines 5        # 空格分隔
:set pageOverlapLines=8       # = 连接等价
:set keybindings.u=pageUp     # 键位覆盖；off/- 解绑
:set                          # 裸调用：状态栏列出全部配置
```

- 数字/布尔自动判型，其余按字符串；未知 key 提示 `unknown key`。
- `keybindings.<key>=<command>` 是覆盖 `defaultKey` 的通用机制——换键、互换、
  解绑都走它（如 `:set keybindings.u=pageUp` 后 `u` 即上翻页）。
