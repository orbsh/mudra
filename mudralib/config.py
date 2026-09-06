"""mudra config: KDL (kdl-py 1.2.0, tabatkins/kdlpy v2 syntax) -> a flat dict
whose key names mirror the extension's MudraConfig.defaults.

Two layers of sources: the repo-root config.kdl ships with the code as the
default config; when ~/.config/mudra/config.kdl exists it overrides keys
(user layer > repo layer).

Structure convention: top-level nodes are config groups (bar/hint/keys/scroll/
command/server); each child node within a group is one setting — a single
argument is the value, and in the keys group the child node name is the key
and the argument is the command. The keys group merges per-key (changing one
key in the user layer leaves other keys untouched).

Observed quirks of kdl-py 1.2.0 (tabatkins/kdlpy, KDL v2 syntax), all
consolidated in this module:
- numbers parse as float (`height 16` -> 16.0) -> int-coerce integer config keys;
- bare identifiers cannot serve as argument values (`j scrollDown` errors)
  -> string values must be quoted;
- `#true` is unsupported (# is tag syntax); bare `true`/`false` -> bool.

Priority (extension side): chrome.storage.local (:set runtime key changes) >
user-layer kdl > repo default kdl > built-in defaults.
The server group is currently a placeholder (the mudrad port is still decided
by mudrad.py arguments) — parsed but not consumed.
"""

from __future__ import annotations

import pathlib

import kdl

# repo default config (ships with the code); the user layer goes to
# ~/.config/mudra/config.kdl and overrides per key
DEFAULT_PATH = pathlib.Path(__file__).resolve().parent.parent / "config.kdl"
USER_PATH = pathlib.Path.home() / ".config" / "mudra" / "config.kdl"

# group name -> flat key name mapping (aligned with frontend/shared/lib.js MudraConfig.defaults)
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

# integer config keys (kdl-py parses numbers as float; these keys are coerced back to int)
_INT_KEYS = {"statusHeight", "hintFontSize", "scrollStepLines",
             "pageOverlapLines", "maxCandidates"}

# placeholder groups: parsed to keep the structure valid, not consumed yet
_PLACEHOLDER_GROUPS = ("server",)


def _coerce(key: str, value):
    """Consolidate the numeric quirk: float -> int for integer keys."""
    if key in _INT_KEYS and isinstance(value, float) and value.is_integer():
        return int(value)
    return value


def parse(text: str) -> dict:
    """KDL text -> flat config dict. Unknown groups/keys are ignored (forward
    compatibility: new fields don't break old backends)."""
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
            continue  # placeholder: structure only needs to be valid, not consumed yet
    return out


def load() -> dict:
    """Merge the repo default config.kdl with the user layer ~/.config/mudra/config.kdl,
    overriding per key.

    If both layers are missing -> {} (the extension's built-in defaults are used
    throughout); user-layer parse errors propagate up (with file location).
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
        if kb:  # per-key merge for keys: changing one key in the user layer leaves other keys untouched
            out.setdefault("keybindings", {}).update(kb)
    return out
