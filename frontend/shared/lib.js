// mudra-keys shared settings + status-bar widget library.
// Used by content.js here; plain DOM only — the extension never loads the
// panel's Solid bundle (the wasm panel owns its own rendering).

const MudraConfig = {
  defaults: {
    hintChars: "asdfghjkl",          // letter pool; assignment follows this order
    hintFontSize: 12,                // px
    statusHeight: 16,                // px, one character tall
    statusFont: "12px monospace",
    statusFg: "#666666",             // dark gray by default; "auto" = black or white, whichever contrasts with the bg
    statusBg: "rgba(0,0,0,.55)",     // normal mode: translucent, frosted by statusBlur
    insertFg: "auto",
    insertBg: "rgba(232,232,232,.62)",
    hintFg: "auto",
    hintBg: "#ffd76e",               // hint labels stay opaque: they sit on top of page text
    statusBlur: 12,                  // px of frosted glass; 0 = translucency only
    statusOpacity: 0.1,              // the strip's own alpha (the tint still comes from statusBg)
    keybindings: null,               // {key: command}; null = use COMMANDS defaultKey
    scrollStepLines: 3,              // lines per j/k scroll
    pageOverlapLines: 5,             // lines kept when paging with w/s (overlap)
    maxCandidates: 10,               // max entries shown in the command-mode popup menu
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

  // Frosted glass: the blur lives on every surface of this bar (the strip, the
  // command line that replaces it, the candidate popup above it) so command
  // mode does not snap back to an opaque strip.
  _frost() {
    const px = this._cfg.statusBlur;
    if (!px) return [];
    const f = `blur(${px}px) saturate(1.4)`;
    return [`-webkit-backdrop-filter:${f}`, `backdrop-filter:${f}`];
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

  // ---- command mode: a centred floating panel (input row on top, candidate
  // list below it), at most maxCandidates entries, scrollable beyond that. ----
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
  // opts.prompt replaces the leading ":" marker (a host entered by a key binding shows what
  // that key would have typed, e.g. ":open ").
  async openCommand(onInput, onPick, onBackspace, opts) {
    if (!this.el) await this.mount();
    const cfg = await MudraConfig.all();

    // Command mode is a floating panel, not the strip moved somewhere else:
    // centred horizontally, its top edge on the 1/3 line of the viewport,
    // input row first and candidates hanging below it.
    const rowH = cfg.statusHeight + 2;
    const barFg = MudraConfig.fgFor(cfg.statusBg, cfg.statusFg);
    const stripBg = MudraConfig.withAlpha(cfg.statusBg, cfg.statusOpacity);
    const panel = document.createElement("div");
    panel.id = "mudra-cmdpanel";
    panel.style.cssText = [
      "position:fixed", "left:50%", "transform:translateX(-50%)", "top:calc(100vh / 3)",
      "width:min(720px,80vw)", "z-index:2147483647",
      `background:${stripBg}`, `color:${barFg}`, ...this._frost(),
      "border-radius:6px", "overflow:hidden", "box-sizing:border-box",
      "box-shadow:0 6px 24px rgba(0,0,0,.18)",
    ].join(";");

    // The input row keeps the bar's height so the type size does not change
    // between modes; the candidate list grows downward from it.
    const line = document.createElement("div");
    line.id = "mudra-cmdline";
    line.style.cssText = [
      `height:${cfg.statusHeight}px`, `font:${cfg.statusFont}`,
      `color:${barFg}`,
      "display:flex", "align-items:center", "padding:0 8px", "box-sizing:border-box",
    ].join(";");
    const promptEl = document.createElement("span");
    promptEl.textContent = (opts && opts.prompt) || ":";
    promptEl.style.whiteSpace = "pre"; // keep the trailing space of ":open "
    const input = document.createElement("input");
    input.id = "mudra-cmdinput";
    input.style.cssText = [
      "flex:1", "min-width:0", "background:transparent", "border:none", "outline:none",
      `color:${barFg}`, `font:${cfg.statusFont}`, "padding:0",
    ].join(";");
    line.appendChild(promptEl);
    line.appendChild(input);

    const box = document.createElement("div");
    box.id = "mudra-cmdbox";
    box.style.cssText = [
      `max-height:${cfg.maxCandidates * rowH}px`,
      "overflow-y:auto", "box-sizing:border-box",
    ].join(";");
    const list = document.createElement("div");
    list.id = "mudra-cmdlist";
    box.appendChild(list);

    // The panel is a sibling of the bar, never a node inside it: the bar is
    // repainted wholesale by _paint, and anything appended into it gets
    // shuffled. The bar is hidden while the panel is open.
    this.bar.style.visibility = "hidden";
    panel.appendChild(line);
    panel.appendChild(box);
    document.documentElement.appendChild(panel);
    input.focus();

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
          "color:" + barFg,
        ].join(";");
        list.appendChild(row);
      });
    };

    const close = () => {
      document.getElementById("mudra-cmdpanel")?.remove();
      if (this.bar) this.bar.style.visibility = "";
      this._paint(); // colors come from the stored state's mode again
    };

    const api = {
      setItems(next) { items = next; sel = Math.min(sel, Math.max(0, items.length - 1)); renderList(); },
      value: () => input.value,
      // The one place that fills the line: value in, selection follows it,
      // host refilter, focus kept. Hosts' Tab hooks use this instead of
      // touching the input node, so no hook can leave the list and the line
      // disagreeing. Selection follows the installed value because Enter
      // takes items[sel]: a completed row must stay the Enter target (it used
      // to reset to row 0, so Enter could open a different candidate than the
      // line showed).
      setValue(next) {
        input.value = next;
        const hit = items.findIndex((it) => it.value === next);
        sel = hit >= 0 ? hit : 0;
        onInput(next, api);
        input.focus();
      },
      close,
    };

    input.addEventListener("input", () => { sel = 0; onInput(input.value, api); });
    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") { e.preventDefault(); close(); onPick(null, input.value, api, "cancel"); }
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
      else if (e.key === "Backspace" && input.value === "" && onBackspace) {
        // Layer-up hook: only fires on an empty input, so normal text editing is unaffected
        e.preventDefault(); onBackspace(api);
      }
      else if (e.key === "Enter") { e.preventDefault(); close(); onPick(items[sel] || null, input.value, api, "enter"); }
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
