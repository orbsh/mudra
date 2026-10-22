# mudra-keys 按键与配置

> 扩展根 `frontend/`：`extension/content.js`（模式机）、`shared/lib.js`（MudraConfig +
> 状态栏 widget，与 panel 共用）。零构建，`--load-extension=frontend/` 直接加载源码。

## 模式

四模式状态机，Esc 任何模式回 normal：

- **normal** — 全部键位生效；数字前缀累积（如 `30g`），状态栏 `count` 显示。
- **hint** — `f` 标记可见链接，输入 hint 前缀，**唯一候选即激活**（无需敲全），选中
  走 `open` 动词（本页就地导航）；`F` 同 hint，选中走 `new` 动词（另开一个 page 窗口），
  状态栏标 `hintnew`（否则两个键在按下之前完全无法区分）。
- **insert** — `i` 聚焦输入框；页内多个输入框时先进 hint 选择；Esc 失焦回 normal。
- **command** — `:` 触发；输入框在状态栏底部占满全宽，候选浮层在上方（最多
  `maxCandidates` 条）。**Tab 只移动选中项**（循环，Shift+Tab 反向），不往行里填字：
  行是用户敲的东西，高亮是 Enter 会取的东西——早先 Tab 填值会触发重过滤，列表在按键
  底下消失，Enter 可能取走一行用户没看过的候选。Enter 执行，Esc 退出。
  **Backspace 是逐层退**：输入为空时再按一次退回上一层（地址 picker → prompt 变成
  `:open ` 的命令行 → 命令行空再按即退出 command 模式），所以永久卡在 `:open ` 那个
  空格后面的死键没有了。

## 打开网页的两个动词

| 动词 | 语义 | 触发 |
|---|---|---|
| `open` | 在**本页**就地导航（`location.href`），并记一次历史 | `f`（hints）、`o`、`:open` picker 的 Enter |
| `new` | 另开一个 `--app` page 窗口（mudrad `/open`，归当前 ctx） | `F`（hints）、`O`、`:new` picker 的 Enter |

`o` / `O` 与 `:open` / `:new` 是同一件事：都落到同一个地址 picker，只是 prompt 与
Enter 用的动词不同（`o` 的 prompt 就是 `:open `，`O` 是 `:new `，候选同样来自打开历史；
`new` 不列活页——新窗口没有可跳转的目标）。**带参数则不再绕 picker**：`:open bing`、
`:new bing` 回车即执行（参数在手，动词已经无歧义）；不带参数才打开 picker 挑历史。

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
| `o` | open | 地址 picker，Enter 在本页打开（`:open`） |
| `O` | new | 地址 picker，Enter 另开窗口（`:new`） |
| `P` | pages | 切页（mudrad） |
| `:` | — | command 模式 |

## 配置

存储在 `chrome.storage.local`（默认值见 `shared/lib.js` 的 `MudraConfig.defaults`）：

`hintChars`（字母池，顺序即分配顺序）、`hintFontSize`、`statusHeight/Font/Fg/Bg`、
`insertFg/Bg`、`hintFg/Bg`、`statusBlur`（默认 12）、`statusOpacity`（默认 0.15）、`keybindings`、`scrollStepLines`（默认 3）、
`pageOverlapLines`（默认 5）、`maxCandidates`（默认 10）。

**优先级**：`config.kdl`（仓库层 + `~/.config/mudra/config.kdl` 用户层逐键合并）是源——服务
worker 每次同步都跟上文件，改文件即改外观；`:set` 写过的键会被钉住（storage 的 `setKeys`），
此后文件不再覆盖它。`:set <key> -` 把该键交还给文件（下一次 SW 同步时按文件值落地，其间用内置
默认）。

**状态栏外观**：底色默认半透明（`bg "rgba(0,0,0,.55)"`、`insertBg "rgba(232,232,232,.62)"`、
hint 模式的深蓝 `rgba(32,64,128,.62)` 同理），配 `statusBlur`（毛玻璃/backdrop-filter 半径，
`0` = 只留半透明）。**条本身的透明度单独一个旋钮**：`bar.opacity`（默认 0.1）——色相仍由
`bg` 给，alpha 由它接管；命令输入行与候选浮层不用这个旋钮（它们是临时面板，要保证可读性，
各用自己 `insertBg`/`bg` 的 alpha，毛玻璃照旧）。
文字色默认灰 `#999999`；写 `"auto"` 则按背景色算 WCAG 相对亮度、取黑/白里对比度高的那个
（alpha 不参与判色——半透明下真实底色是网页内容，静态算不出来，所以按配置的色相判）；写死
别的颜色即锁死。候选浮层的字**不跟随**这个设置，永远按 `bg` 的亮度自动取黑/白（那里要读
URL，不能是淡灰）。hint 标签（`f` 的小方块）保持不透明：它要压在页面文字上。

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
