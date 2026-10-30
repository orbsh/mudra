// mudra-keys shared settings + status-bar widget library.
// Used by content.js here; plain DOM only — the extension never loads the
// panel's Solid bundle (the wasm panel owns its own rendering).

const MudraConfig = {
  defaults: {
    hintChars: "asdfghjkl",          // letter pool; assignment follows this order
    hintFontSize: 14,                // px — the label sits on page text, so it is read at a glance in a busy corner
    statusHeight: 16,                // px, one character tall
    statusFont: "12px monospace",
    statusFg: "#666666",             // dark gray by default; "auto" = black or white, whichever contrasts with the bg
    statusBg: "rgba(0,0,0,.55)",     // normal mode: translucent, frosted by statusBlur
    insertFg: "auto",
    insertBg: "rgba(232,232,232,.62)",
    hintFg: "auto",
    hintBg: "#fbf1c7",               // gruvbox light yellow (the hue; the alpha is hintOpacity)
    hintOpacity: 0.45,               // hint label alpha — low enough that the page's own text shows through the tint
    statusBlur: 12,                  // px of frosted glass; 0 = translucency only
    statusOpacity: 0.1,              // the strip's own alpha (the tint still comes from statusBg)
    cmdBg: "#fbf1c7",                // command panel hue: same gruvbox light yellow
    cmdOpacity: 0.62,                // command panel alpha; lower = more frost, higher = more colour
    cmdFg: "#3c3836",                // command panel text: gruvbox dark gray
    cmdScrollbar: 2,                 // command list scrollbar width px (thumb only, no track)
    progressHeight: 2,               // page-load bar thickness px; 0 hides it
    progressColors: "#fb4934 #fe8019 #fabd2f #b8bb26 #83a598 #d3869b", // multi-stop gradient, left to right
    keybindings: null,               // {key: command}; null = use COMMANDS defaultKey
    seqTimeout: 1200,                // ms a key waits for its sequence partner (y → yy / yt)
    scrollStepLines: 1,              // lines per scroll key (h/j/k/l)
    pageOverlapLines: 5,             // lines kept when paging with pageDown/pageUp (overlap)
  },
  storage: chrome.storage.local,

  // A CSS colour as [r, g, b]. Alpha is dropped on purpose: a frosted bar's real
  // backdrop is whatever page is behind it, so the tint the user configured is
  // the only stable signal for choosing the text colour.
  parseColor(c) {
    const s = String(c || "").trim().toLowerCase();
    const hex = s.match(/^#([0-9a-f]{3,8})$/);
    if (hex) {
      let h = hex[1];
      if (h.length === 3 || h.length === 4) h = h.split("").map((x) => x + x).join("");
      if (h.length < 6) return null;
      return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16));
    }
    const fn = s.match(/^rgba?\(([^)]+)\)$/);
    if (fn) {
      const parts = fn[1].split(/[,\s/]+/).filter(Boolean).map(parseFloat);
      if (parts.length < 3 || parts.slice(0, 3).some(Number.isNaN)) return null;
      return parts.slice(0, 3);
    }
    return null;
  },

  // Black or white, whichever has the higher WCAG contrast against `bg`.
  readableFg(bg) {
    const rgb = this.parseColor(bg);
    if (!rgb) return "#ffffff";
    const lin = rgb.map((v) => {
      const s = v / 255;
      return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
    });
    const L = 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
    return 1.05 / (L + 0.05) >= (L + 0.05) / 0.05 ? "#ffffff" : "#000000";
  },

  // "auto" (or unset) derives the foreground from the background; anything else
  // is a literal colour the user pinned (`fg "#ffffff"` locks white).
  fgFor(bg, fg) {
    return !fg || fg === "auto" ? this.readableFg(bg) : fg;
  },

  // `color` with its alpha replaced by the bar's own opacity knob. The knob
  // exists because the frosted strip wants far less tint than the surfaces that
  // must stay readable (command line, candidate popup keep their own alpha).
  // Unknown colours pass through untouched.
  withAlpha(color, a) {
    const rgb = this.parseColor(color);
    const n = parseFloat(a);
    if (!rgb || Number.isNaN(n)) return color;
    return `rgba(${rgb.join(",")},${Math.max(0, Math.min(1, n))})`;
  },

  // Frosted-glass properties for one blur radius. One formula for every
  // surface that blurs (strip, command panel, hint labels), so they cannot
  // drift apart; 0/unset means translucent only.
  frost(px) {
    const n = parseFloat(px);
    if (!n) return [];
    const f = `blur(${n}px) saturate(1.4)`;
    return [`-webkit-backdrop-filter:${f}`, `backdrop-filter:${f}`];
  },

  async all() {
    const stored = await this.storage.get(null);
    return { ...this.defaults, ...stored };
  },

  async set(patch) {
    const keys = Object.keys(patch);
    // Pinned keys: the service worker leaves them alone when it re-reads the
    // config file, so a :set survives later file edits — and every key NOT
    // pinned keeps tracking the file.
    const stored = await this.storage.get("setKeys");
    await this.storage.set(patch);
    const pinned = (stored && stored.setKeys) || [];
    const next = [...new Set([...pinned, ...keys])];
    if (next.length !== pinned.length) await this.storage.set({ setKeys: next });
    return this.all();
  },

  // Hand a key back to the config file: drop the stored value and its pin, so
  // the next sync restores what config.kdl says (until then the built-in
  // default applies).
  async unpin(key) {
    const stored = await this.storage.get("setKeys");
    await this.storage.remove(key);
    const pinned = ((stored && stored.setKeys) || []).filter((k) => k !== key);
    await this.storage.set({ setKeys: pinned });
    return this.all();
  },

  // Import from a JSON string (replaces keybindings wholesale)
  async importJson(text) {
    const obj = JSON.parse(text);
    if (obj.keybindings !== undefined) await this.set({ keybindings: obj.keybindings });
    const pass = {};
    for (const k of Object.keys(this.defaults)) {
      if (k !== "keybindings" && obj[k] !== undefined) pass[k] = obj[k];
    }
    if (Object.keys(pass).length) await this.set(pass);
    return this.all();
  },
};

// ---- command line --------------------------------------------------------
// The line is deliberately NOT an <input>: Chrome paints its own completion
// dropdown over any text field it can name (autofill / form history), and no
// attribute combination makes that stay away for good. A plaintext-only
// contenteditable has no native autofill at all, so the second menu cannot
// appear; the caret, the value and the events are ours.
const MudraLine = {
  create({ id, color, font }) {
    const el = document.createElement("div");
    el.id = id;
    el.setAttribute("contenteditable", "plaintext-only");
    el.setAttribute("role", "textbox");
    el.setAttribute("aria-autocomplete", "none");
    el.setAttribute("spellcheck", "false");
    el.setAttribute("autocapitalize", "off");
    el.style.cssText = [
      "flex:1", "min-width:0", "background:transparent", "border:none", "outline:none",
      "white-space:pre", "overflow:hidden", `caret-color:${color}`,
      `color:${color}`, `font:${font}`, "padding:0", "line-height:1.2",
    ].join(";");

    // The value is the text, newlines normalised away: Enter belongs to the
    // host (it picks a candidate), so a line break here would be a UI bug.
    const value = () => (el.textContent || "").replace(/\n/g, "");
    const caretEnd = () => {
      const r = document.createRange();
      r.selectNodeContents(el);
      r.collapse(false);
      const s = getSelection();
      s.removeAllRanges();
      s.addRange(r);
    };
    const setValue = (v) => { el.textContent = v; caretEnd(); };
    // Paste and drop insert plain text at the caret: a dropped node would put
    // DOM inside the line, and the line's contract is "text in, text out".
    const insertText = (text) => {
      const s = getSelection();
      if (!s.rangeCount || !el.contains(s.anchorNode)) { el.textContent += text; caretEnd(); return; }
      const r = s.getRangeAt(0);
      r.deleteContents();
      const node = document.createTextNode(text.replace(/\n/g, " "));
      r.insertNode(node);
      r.setStartAfter(node);
      r.collapse(true);
      s.removeAllRanges();
      s.addRange(r);
    };

    el.addEventListener("paste", (e) => {
      e.preventDefault();
      insertText(((e.clipboardData || window.clipboardData).getData("text") || ""));
      el.dispatchEvent(new Event("input", { bubbles: true }));
    });
    el.addEventListener("drop", (e) => e.preventDefault());
    el.addEventListener("beforeinput", (e) => {
      if (e.inputType === "insertParagraph" || e.inputType === "insertLineBreak") e.preventDefault();
    });
    return { el, value, setValue, insertText, caretEnd, focus: () => { el.focus(); caretEnd(); } };
  },
};

// ---- status bar (a one-character-tall chip in the bottom-right corner) ----
// Plain DOM, imperative repaint: render(data) stores state and rewrites the
// single slot: title + url + scroll position.
const MudraBar = {
  el: null,
  bar: null,
  right: null,
  _cfg: null,
  _state: {},

  async mount() {
    if (this.el && this.el.isConnected) return this;
    const cfg = await MudraConfig.all();
    this._cfg = cfg;

    const root = document.createElement("div");
    root.id = "mudra-bar-root";
    document.documentElement.appendChild(root);
    // The status bar must be outermost: the classic scrollbar paints above all elements (z-index cannot suppress it),
    // and the scroll position is already shown on the bar's right -> hide the page scrollbar outright.
    const st = document.createElement("style");
    st.id = "mudra-scrollbar-style";
    st.textContent = "html { scrollbar-width: none !important; } html::-webkit-scrollbar { display: none !important; }";
    document.documentElement.appendChild(st);

    const bar = document.createElement("div");
    bar.id = "mudra-bar";
    const right = document.createElement("span");
    right.id = "mudra-bar-right";
    right.style.cssText = "display:flex;gap:10px;align-items:center;overflow:hidden;flex-direction:row";
    bar.appendChild(right);
    root.appendChild(bar);
    this.el = root;
    this.bar = bar;
    this.right = right;
    this._paint();
    return this;
  },

  // The bar's two colours for a mode. `fg` may be "auto": the tint then picks
  // black or white (the frosted backdrop is page content, so the tint is the
  // only stable signal).
  _colors(mode) {
    const cfg = this._cfg;
    const [fg, bg] = {
      normal: [cfg.statusFg, cfg.statusBg],
      insert: [cfg.insertFg, cfg.insertBg],
      hint:   [cfg.statusFg, "rgba(32,64,128,.62)"],
    }[mode] || [cfg.statusFg, cfg.statusBg];
    return { fg: MudraConfig.fgFor(bg, fg), bg };
  },

  // Frosted glass on the bar's own surfaces (strip + the command panel it
  // hosts). One formula lives on MudraConfig so the hint labels get the same.
  _frost() {
    return MudraConfig.frost(this._cfg.statusBlur);
  },

  // Rewrite the two slots from the stored state. Every dynamic value lives
  // here (the imperative sibling of the old Solid function children).
  _paint() {
    if (!this.bar) return;
    const d = this._state;
    const cfg = this._cfg;
    const mode = d.mode || "normal";
    const col = this._colors(mode);
    // A chip, not a strip: the box is only as wide as its text and tucks into
    // the bottom-right corner — `left` must stay unset or it spans the viewport.
    this.bar.style.cssText = [
      "position:fixed", "right:0", "bottom:0", "z-index:2147483647",
      "max-width:70vw", "border-radius:4px",
      `height:${cfg.statusHeight}px`, `font:${cfg.statusFont}`,
      `color:${col.fg}`, `background:${MudraConfig.withAlpha(col.bg, cfg.statusOpacity)}`, ...this._frost(),
      "display:flex", "align-items:center",
      "padding:0 8px", "box-sizing:border-box", "user-select:none",
      "pointer-events:none", "white-space:nowrap", "overflow:hidden",
    ].join(";");

    // The chip carries the right-hand status only (title · url · scroll).
    // ctx/tags/mode are not drawn: they were the strip's left half, and a
    // 4-corner chip has no room for a second block. Mode still reads off the
    // tint, which is why _colors() keeps feeding the background.
    this.right.textContent = d.message != null
      ? d.message
      : `${d.title || ""} ${d.url || ""}${d.scroll != null ? " " + d.scroll : ""}`;
  },

  // data: {ctx, mode, title, url, scroll, tags(path array), message, count}
  async render(data) {
    if (!this.el) return;
    // In command mode the bar is an input line; render must not overwrite it (openCommand maintains the input itself)
    if (document.getElementById("mudra-cmdinput")) return;
    this._state = { ...data };
    this._paint();
  },

  // ---- key menus (sequence hints and the ? keymap) ----
  // Bottom-right, stacked directly above the status chip, wearing the command
  // panel's clothes: same pale tint, same frost, same 2px thumb with no track.
  // A sibling of the bar, never a child — _paint rewrites the bar wholesale —
  // and it leaves the chip visible, because that is what it is anchored to.
  // `maxVh` is the only geometry knob; past it the rows scroll, which is the
  // whole reason a keymap can be shown without a row count.
  async showMenu({ rows, maxVh = 66 }) {
    if (!this.el) await this.mount();
    this.hideMenu();
    const cfg = this._cfg || (await MudraConfig.all());
    const tint = MudraConfig.withAlpha(cfg.cmdBg, cfg.cmdOpacity);
    const fg = MudraConfig.fgFor(cfg.cmdBg, cfg.cmdFg);

    const wrap = document.createElement("div");
    wrap.id = "mudra-menu";
    wrap.style.cssText = [
      "position:fixed", "right:8px", `bottom:${cfg.statusHeight + 6}px`,
      "z-index:2147483647", `max-height:${maxVh}vh`,
      `background:${tint}`, `color:${fg}`, ...this._frost(),
      "border-radius:6px", "overflow:hidden", "box-sizing:border-box",
      "box-shadow:0 6px 24px rgba(0,0,0,.18)",
      `font:${cfg.statusFont}`, "padding:3px 0",
      "min-width:150px", "max-width:46vw",
      "display:flex", "flex-direction:column",
    ].join(";");

    const box = document.createElement("div");
    box.id = "mudra-menubox";
    box.style.cssText = "flex:1 1 auto;min-height:0;overflow-y:auto;box-sizing:border-box";
    // Key column padded to the widest key: descriptions line up in one column
    // instead of ragging a pixel or two with every row.
    const keyW = rows.reduce((w, r) => Math.max(w, (r.key || "").length), 0);
    const descColor = MudraConfig.withAlpha(fg, 0.72);
    for (const r of rows) {
      const row = document.createElement("div");
      row.style.cssText = `display:flex;gap:12px;padding:0 10px;white-space:pre;line-height:${cfg.statusHeight + 2}px`;
      const k = document.createElement("span");
      k.textContent = (r.key || "") + " ".repeat(Math.max(0, keyW - (r.key || "").length));
      k.style.fontWeight = "600";
      const d = document.createElement("span");
      d.textContent = r.desc || "";
      d.style.color = descColor;
      row.append(k, d);
      box.appendChild(row);
    }
    if (!rows.length) box.textContent = "";
    wrap.appendChild(box);

    document.getElementById("mudra-menu-scrollbar-style")?.remove();
    const sb = document.createElement("style");
    sb.id = "mudra-menu-scrollbar-style";
    sb.textContent = [
      `#mudra-menubox::-webkit-scrollbar{width:${cfg.cmdScrollbar}px;height:${cfg.cmdScrollbar}px;background:transparent}`,
      "#mudra-menubox::-webkit-scrollbar-track{background:transparent;border:none}",
      `#mudra-menubox::-webkit-scrollbar-thumb{background:${MudraConfig.withAlpha(fg, 0.15)};border:none;border-radius:0}`,
      "#mudra-menubox::-webkit-scrollbar-corner{background:transparent}",
    ].join("");
    document.documentElement.appendChild(sb);
    document.documentElement.appendChild(wrap);
    this._menu = {
      wrap,
      close: () => {
        wrap.remove();
        sb.remove();
        this._menu = null;
      },
    };
    return this._menu;
  },

  hideMenu() {
    if (this._menu) this._menu.close();
  },

  // ---- command mode: a centred floating panel (input row on top, candidate
  // list below it). The panel's band is the middle third of the viewport, so
  // the list scrolls inside it instead of being capped at a row count. ----
  // onInput(query, api) filters candidates on the host side; onPick(candidate, query, api) handles selection;
  // candidate = {label, value, desc?}；Esc → onPick(null, ...)。A candidate with `desc` renders
  // as two columns (name padded to a shared width, description in dim gray) so name and
  // description never blur into one run of text.
  // Optional host hooks for non-command pickers: onBackspace(api) fires when Backspace is
  // pressed on an empty input (layer-up semantics). onPick also gets a 4th argument, reason:
  // "enter" when Enter committed the line, "cancel" when Esc dismissed it — hosts that treat
  // "no candidate" as "use the typed text" must not fire that on Esc.
  // Tab moves the selection (wrapping, Shift+Tab backwards) and never edits the line: the
  // line is what the user typed, the highlight is what Enter will take.
  async openCommand(onInput, onPick, onBackspace) {
    if (!this.el) await this.mount();
    const cfg = await MudraConfig.all();

    // The panel is placed and sized by ratios, not pixels: centred
    // horizontally, top edge on the 1/3 line, at most one third of the
    // viewport tall (so the list ends by the 2/3 line), half the viewport wide.
    const rowH = cfg.statusHeight + 2;
    // The panel has its own colours (cmdBg/cmdFg): a frosted gruvbox light
    // surface reads as a menu, where the chip's translucent dark tint reads as
    // part of the page. The hue and the alpha are separate knobs (cmdBg +
    // cmdOpacity) — at near-opaque alpha the frost is there but invisible.
    const cmdTint = MudraConfig.withAlpha(cfg.cmdBg, cfg.cmdOpacity);
    const cmdFg = MudraConfig.fgFor(cfg.cmdBg, cfg.cmdFg);
    const panel = document.createElement("div");
    panel.id = "mudra-cmdpanel";
    panel.style.cssText = [
      "position:fixed", "left:50%", "transform:translateX(-50%)",
      "top:calc(100vh / 3)", "max-height:calc(100vh / 3)",
      "width:50vw", "z-index:2147483647",
      "display:flex", "flex-direction:column",
      `background:${cmdTint}`, `color:${cmdFg}`, ...this._frost(),
      "border-radius:6px", "overflow:hidden", "box-sizing:border-box",
      "box-shadow:0 6px 24px rgba(0,0,0,.18)",
    ].join(";");

    // The input row keeps the bar's height so the type size does not change
    // between modes; the candidate list takes the rest of the panel.
    const line = document.createElement("div");
    line.id = "mudra-cmdline";
    line.style.cssText = [
      `height:${cfg.statusHeight}px`, `font:${cfg.statusFont}`,
      `color:${cmdFg}`, "flex:0 0 auto",
      "display:flex", "align-items:center", "padding:0 8px", "box-sizing:border-box",
    ].join(";");
    // The line is a contenteditable (MudraLine), not an <input>: Chrome's own
    // completion dropdown would otherwise open right above our candidate list.
    const field = MudraLine.create({ id: "mudra-cmdinput", color: cmdFg, font: cfg.statusFont });
    line.appendChild(field.el);

    const box = document.createElement("div");
    box.id = "mudra-cmdbox";
    box.style.cssText = [
      "flex:1 1 auto", "min-height:0",
      "overflow-y:auto", "box-sizing:border-box",
    ].join(";");
    const list = document.createElement("div");
    list.id = "mudra-cmdlist";
    box.appendChild(list);

    // Scrollbar on the candidate list: a thin thumb and nothing else — no
    // track, no frame, no radius. Pseudo-element rules cannot ride along in an
    // inline style, so the panel carries one scoped <style>, rewritten on every
    // open (so `:set cmdScrollbar 4` lands without a reload).
    document.getElementById("mudra-cmd-scrollbar-style")?.remove();
    const sbStyle = document.createElement("style");
    sbStyle.id = "mudra-cmd-scrollbar-style";
    sbStyle.textContent = [
      `#mudra-cmdbox::-webkit-scrollbar{width:${cfg.cmdScrollbar}px;height:${cfg.cmdScrollbar}px;background:transparent}`,
      "#mudra-cmdbox::-webkit-scrollbar-track{background:transparent;border:none}",
      `#mudra-cmdbox::-webkit-scrollbar-thumb{background:${MudraConfig.withAlpha(cmdFg, 0.15)};border:none;border-radius:0}`,
      "#mudra-cmdbox::-webkit-scrollbar-corner{background:transparent}",
    ].join("");
    document.documentElement.appendChild(sbStyle);

    // The panel is a sibling of the bar, never a node inside it: the bar is
    // repainted wholesale by _paint, and anything appended into it gets
    // shuffled. The bar is hidden while the panel is open.
    this.bar.style.visibility = "hidden";
    panel.appendChild(line);
    panel.appendChild(box);
    document.documentElement.appendChild(panel);
    field.focus();

    let items = [];
    let sel = 0;
    const renderList = () => {
      list.textContent = "";
      // two-column rows when any candidate carries a desc: the name column
      // pads to the widest name (monospace), the desc rides in dim gray.
      const descW = items.some((it) => it.desc)
        ? Math.max(...items.map((it) => (it.label || "").length))
        : 0;
      items.forEach((it, i) => {
        const row = document.createElement("div");
        const name = document.createElement("span");
        const pad = descW ? " ".repeat(2 + Math.max(0, descW - (it.label || "").length)) : " ";
        name.textContent = (i === sel ? "» " : "  ") + (it.label || "") + pad;
        row.appendChild(name);
        if (descW) {
          const desc = document.createElement("span");
          desc.textContent = it.desc || "";
          desc.style.color = "#9a9a9a";
          row.appendChild(desc);
        }
        row.style.cssText = [
          `font:${cfg.statusFont}`, `height:${rowH}px`, "line-height:" + rowH + "px",
          "padding:0 6px", "white-space:pre", "box-sizing:border-box",
          "background:" + (i === sel ? "rgba(0,0,0,.18)" : "transparent"),
          "color:" + cmdFg,
        ].join(";");
        list.appendChild(row);
      });
    };

    const close = () => {
      document.getElementById("mudra-cmdpanel")?.remove();
      document.getElementById("mudra-cmd-scrollbar-style")?.remove();
      if (this.bar) this.bar.style.visibility = "";
      this._paint(); // colors come from the stored state's mode again
    };

    const api = {
      setItems(next) { items = next; sel = Math.min(sel, Math.max(0, items.length - 1)); renderList(); },
      value: () => field.value(),
      // The one place that fills the line: value in, selection follows it,
      // host refilter, focus kept. Hosts' Tab hooks use this instead of
      // touching the input node, so no hook can leave the list and the line
      // disagreeing. Selection follows the installed value because Enter
      // takes items[sel]: a completed row must stay the Enter target (it used
      // to reset to row 0, so Enter could open a different candidate than the
      // line showed).
      setValue(next) {
        field.setValue(next);
        const hit = items.findIndex((it) => it.value === next);
        sel = hit >= 0 ? hit : 0;
        onInput(next, api);
        field.focus();
      },
      close,
    };

    field.el.addEventListener("input", () => { sel = 0; onInput(field.value(), api); });
    field.el.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") { e.preventDefault(); close(); onPick(null, field.value(), api, "cancel"); }
      else if (e.key === "ArrowDown") { e.preventDefault(); sel = Math.min(sel + 1, items.length - 1); renderList(); }
      else if (e.key === "ArrowUp") { e.preventDefault(); sel = Math.max(sel - 1, 0); renderList(); }
      else if (e.key === "Tab") {
        // Several candidates: cycle the highlight and leave the line alone
        // (filling it would refilter and hide the list under the keystroke,
        // and Enter could then take a row nobody looked at). One candidate:
        // there is nothing to cycle to, so Tab does the other half of its job
        // and completes the line with it — otherwise the key reads as dead.
        e.preventDefault();
        if (!items.length) return;
        if (items.length === 1) { api.setValue(items[0].value); return; }
        const step = e.shiftKey ? -1 : 1;
        sel = (sel + step + items.length) % items.length;
        renderList();
      }
      else if (e.key === "Backspace" && field.value() === "" && onBackspace) {
        // Layer-up hook: only fires on an empty line, so normal text editing is unaffected
        e.preventDefault(); onBackspace(api);
      }
      else if (e.key === "Enter") { e.preventDefault(); close(); onPick(items[sel] || null, field.value(), api, "enter"); }
    });
    return api;
  },

  unmount() {
    if (this.el) this.el.remove();
    this.el = null;
    this.bar = null;
    this.right = null;
    this._state = {};
  },
};

// ---- load progress bar ----------------------------------------------------
// The browser reports a load in the tab strip, which a keyboard-driven window
// rarely looks at. This is the one piece of chrome mudra adds at the top of a
// page: a 4px (`progressHeight`) bar whose fill is a multi-stop gradient
// (`progressColors`), mounted at document_start so it spans the whole load,
// then faded out. The gradient is painted at viewport width and clipped by the
// growing box, so the colours stay put instead of scrolling with the fill.
const MudraProgress = {
  el: null,
  _done: false,

  gradient(colors) {
    const cs = String(colors || "").trim().split(/\s+/).filter(Boolean);
    if (!cs.length) cs.push("#fabd2f", "#83a598");
    if (cs.length === 1) cs.push(cs[0]);
    return "linear-gradient(90deg, " +
      cs.map((c, i) => `${c} ${((i / (cs.length - 1)) * 100).toFixed(1)}%`).join(", ") + ")";
  },

  mount() {
    // A document that has already finished loading has nothing left to report
    if (this.el || document.readyState === "complete") return null;
    const d = MudraConfig.defaults;
    const el = document.createElement("div");
    el.id = "mudra-progress";
    el.style.cssText = [
      "position:fixed", "top:0", "left:0",
      `height:${d.progressHeight}px`, "width:0",
      `background-image:${this.gradient(d.progressColors)}`,
      "background-size:100vw 100%", "background-repeat:no-repeat",
      "z-index:2147483647", "pointer-events:none",
      "transition:width 2.4s cubic-bezier(.08,.72,.24,1), opacity .3s ease-out",
    ].join(";");
    this.el = el;
    (document.documentElement || document.body).appendChild(el);
    // The fill creeps forward on its own: there is no progress API, only a
    // load event, and a bar that sits at 0% until the end reports nothing.
    requestAnimationFrame(() => { if (this.el) el.style.width = "72%"; });
    MudraConfig.all().then((c) => this.restyle(c)).catch(() => {});
    if (document.readyState === "complete") this.finish();
    else window.addEventListener("load", () => this.finish(), { once: true });
    // A document that never finishes (streaming, hung socket) would leave the
    // bar parked at 72% forever; give it up after 20s.
    setTimeout(() => this.finish(), 20000);
    return el;
  },

  restyle(cfg) {
    if (!this.el || !cfg) return;
    const px = parseFloat(cfg.progressHeight);
    if (!(px > 0)) { // progressHeight 0 = no bar at all, not a zero-height div
      this.el.remove();
      this.el = null;
      return;
    }
    this.el.style.height = `${px}px`;
    this.el.style.backgroundImage = this.gradient(cfg.progressColors);
  },

  finish() {
    if (!this.el || this._done) return;
    this._done = true;
    this.el.style.width = "100%";
    setTimeout(() => { if (this.el) this.el.style.opacity = "0"; }, 150);
    setTimeout(() => {
      if (this.el) this.el.remove();
      this.el = null;
    }, 800);
  },
};
