# mudra-keys 按键与配置

> 扩展根 `frontend/`：`extension/content.js`（模式机）、`shared/lib.js`（MudraConfig +
> 状态栏 widget，与 panel 共用）。零构建，`--load-extension=frontend/` 直接加载源码。

## 模式

四模式状态机，Esc 任何模式回 normal：

- **normal** — 全部键位生效；数字前缀累积（如 `30g`），状态栏 `count` 显示。
- **hint** — `f` 标记可见链接，输入 hint 前缀，**唯一候选即激活**（无需敲全）；
  `F` 同 hint 但经 mudrad `/open?new_window=1` 开新窗。
- **insert** — `i` 聚焦输入框；页内多个输入框时先进 hint 选择；Esc 失焦回 normal。
- **command** — `:` 触发；输入框在状态栏底部占满全宽，候选浮层在上方（最多
  `maxCandidates` 条），Tab 补全命令名，↑↓ 选择，Enter 执行，Esc 退出。

## 默认键位（均可被 keybindings 覆盖）

| key | command | 行为 |
|---|---|---|
| `f` / `F` | hint / hintNew | 链接 hints（本窗 / 新窗） |
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
`insertFg/Bg`、`hintFg/Bg`、`keybindings`、`scrollStepLines`（默认 3）、
`pageOverlapLines`（默认 5）、`maxCandidates`（默认 10）。

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
