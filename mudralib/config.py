"""mudra 配置：KDL（kdl-py 1.2.0，tabatkins/kdlpy v2 语法）→ 与扩展
MudraConfig.defaults 同键名的扁平 dict。

两层来源：仓库根目录 config.kdl 是随代码分发的默认配置；
~/.config/mudra/config.kdl 存在时按键覆盖合并（用户层 > 仓库层）。

结构约定：顶层节点是配置组（bar/hint/keys/scroll/command/server），组内
每个子节点是一项配置——单参数为值，keys 组的子节点名是按键、参数是命令。
keys 的合并是按键级（用户层改一个键不动其他键）。

kdl-py 1.2.0（tabatkins/kdlpy，KDL v2 语法）实测怪癖（收敛在本模块）：
- 数字解析为 float（`height 16` → 16.0）→ 对整型配置键强转 int；
- 裸标识符不能做参数值（`j scrollDown` 报错）→ 字符串值必须加引号；
- `#true` 不支持（# 是 tag 语法），裸 `true`/`false` → bool。

优先级（扩展侧）：chrome.storage.local（:set 运行时改键）> 用户层 kdl >
仓库默认 kdl > 内置 defaults。
server 组当前是占位（mudrad 端口仍由 mudrad.py 参数决定），解析但不消费。
"""

from __future__ import annotations

import pathlib

import kdl

# 仓库默认配置（随代码分发）；用户层放 ~/.config/mudra/config.kdl，按键覆盖
DEFAULT_PATH = pathlib.Path(__file__).resolve().parent.parent / "config.kdl"
USER_PATH = pathlib.Path.home() / ".config" / "mudra" / "config.kdl"

# 组名 → 扁平键名映射（与 frontend/shared/lib.js MudraConfig.defaults 对齐）
_BAR_KEYS = {
    "font": "statusFont",
    "height": "statusHeight",
    "fg": "statusFg",
    "bg": "statusBg",
    "insertFg": "insertFg",
    "insertBg": "insertBg",
    "hintFg": "hintFg",
    "hintBg": "hintBg",
}
_HINT_KEYS = {"chars": "hintChars", "fontSize": "hintFontSize"}
_SCROLL_KEYS = {"stepLines": "scrollStepLines", "overlapLines": "pageOverlapLines"}
_COMMAND_KEYS = {"maxCandidates": "maxCandidates"}
_UI_KEYS = {"thumbnails": "thumbnails"}

# 整型配置键（kdl-py 把数字解析成 float，这些键强转回 int）
_INT_KEYS = {"statusHeight", "hintFontSize", "scrollStepLines",
             "pageOverlapLines", "maxCandidates"}

# 占位组：解析保留结构，暂不消费
_PLACEHOLDER_GROUPS = ("server",)


def _coerce(key: str, value):
    """数值怪癖收敛：整型键 float → int。"""
    if key in _INT_KEYS and isinstance(value, float) and value.is_integer():
        return int(value)
    return value


def parse(text: str) -> dict:
    """KDL 文本 → 扁平配置 dict。未知组/键忽略（前向兼容：新字段旧后端不炸）。"""
    out: dict = {}
    doc = kdl.parse(text)
    for group in doc.nodes:
        g = group.name
        table = {"bar": _BAR_KEYS, "hint": _HINT_KEYS, "scroll": _SCROLL_KEYS,
                 "command": _COMMAND_KEYS, "ui": _UI_KEYS}.get(g)
        if table:
            for child in group.nodes:
                key = table.get(child.name)
                if key and child.args:
                    out[key] = _coerce(key, child.args[0])
        elif g == "keys":
            kb = out.setdefault("keybindings", {})
            for child in group.nodes:
                if child.args:
                    kb[child.name] = _coerce(child.name, child.args[0])
        elif g in _PLACEHOLDER_GROUPS:
            continue  # 占位：结构合法即可，暂不消费
    return out


def load() -> dict:
    """仓库默认 config.kdl + 用户层 ~/.config/mudra/config.kdl 按键覆盖合并。

    两层都缺失 → {}（全用扩展内置 defaults）；用户层解析错误向上抛（带文件位置）。
    """
    out: dict = {}
    for path, required in ((DEFAULT_PATH, True), (USER_PATH, False)):
        if not path.is_file():
            if required:
                raise FileNotFoundError(f"default config missing: {path}")
            continue
        parsed = parse(path.read_text())
        kb = parsed.pop("keybindings", None)
        out.update(parsed)
        if kb:  # keys 按键级合并：用户层改一个键不动其他键
            out.setdefault("keybindings", {}).update(kb)
    return out
