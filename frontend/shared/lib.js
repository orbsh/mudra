// mudra-keys shared settings + status-bar widget library.
// Used by content.js here; plain DOM only — the extension never loads the
// panel's Solid bundle (the wasm panel owns its own rendering; the bar's
// tag capsules come from mudrad's SSR HTML string — tag-forest crate is
// the single component source).

const MudraConfig = {
  defaults: {
    hintChars: "asdfghjkl",          // letter pool; assignment follows this order
    hintFontSize: 12,                // px
    statusHeight: 16,                // px, one character tall
    statusFont: "12px monospace",
    statusFg: "#ffffff",             // normal mode white text
    statusBg: "#000000",             // normal mode black background
    insertFg: "#000000",
    insertBg: "#e8e8e8",
    hintFg: "#000000",
    hintBg: "#ffd76e",
    keybindings: null,               // {key: command}; null = use COMMANDS defaultKey
    scrollStepLines: 3,              // lines per j/k scroll
    pageOverlapLines: 5,             // lines kept when paging with w/s (overlap)
    maxCandidates: 10,               // max entries shown in the command-mode popup menu
  },
  storage: chrome.storage.local,

  async all() {
    const stored = await this.storage.get(null);
    return { ...this.defaults, ...stored };
  },

  async set(patch) {
    await this.storage.set(patch);
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

// ---- status bar (qutebrowser style: a single strip at the bottom, one character tall) ----
// Plain DOM, imperative repaint: render(data) stores state and rewrites the
// two slots. Left: ctx - numeric prefix - mode - tag capsule string; right: title + url + scroll position.
const MudraBar = {
  el: null,
  bar: null,
  left: null,
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
    // Capsule/mode segment styles are shared with the panel (styles.css loads only in the panel), so inject them inline here
    const css = document.createElement("style");
    css.id = "mudra-tags-style";
    css.textContent = [
      "#mudra-bar-root .capsule{display:inline-flex;align-items:stretch;border:1px solid #555;border-radius:9px;overflow:hidden}",
      "#mudra-bar-root .seg{padding:0 5px;border-right:1px solid #333;white-space:nowrap}",
      "#mudra-bar-root .seg:last-child{border-right:none}",
      "#mudra-bar-root .seg.leaf{background:rgba(122,162,247,.25)}",
    ].join("");
    document.documentElement.appendChild(css);

    const bar = document.createElement("div");
    bar.id = "mudra-bar";
    const left = document.createElement("span");
    left.id = "mudra-bar-left";
    left.style.cssText = "display:flex;gap:6px;align-items:center;min-width:0";
    const right = document.createElement("span");
    right.id = "mudra-bar-right";
    right.style.cssText = "display:flex;gap:10px;align-items:center;overflow:hidden;flex-direction:row";
    bar.appendChild(left);
    bar.appendChild(right);
    root.appendChild(bar);
    this.el = root;
    this.bar = bar;
    this.left = left;
    this.right = right;
    this._paint();
    return this;
  },

  _colors(mode) {
    const cfg = this._cfg;
    return {
      normal: { fg: cfg.statusFg, bg: cfg.statusBg },
      insert: { fg: cfg.insertFg, bg: cfg.insertBg },
      hint:   { fg: cfg.statusFg, bg: "#204080" },
    }[mode] || { fg: cfg.statusFg, bg: cfg.statusBg };
  },

  // Rewrite the two slots from the stored state. Every dynamic value lives
  // here (the imperative sibling of the old Solid function children).
  _paint() {
    if (!this.bar) return;
    const d = this._state;
    const cfg = this._cfg;
    const mode = d.mode || "normal";
    const col = this._colors(mode);
    this.bar.style.cssText = [
      "position:fixed", "left:0", "right:0", "bottom:0", "z-index:2147483647",
      `height:${cfg.statusHeight}px`, `font:${cfg.statusFont}`,
      `color:${col.fg}`, `background:${col.bg}`,
      "display:flex", "align-items:center", "justify-content:space-between",
      "padding:0 6px", "box-sizing:border-box", "user-select:none",
      "pointer-events:none", "white-space:nowrap", "overflow:hidden",
    ].join(";");

    // Capsule row: mudrad renders the capsule HTML (tag-forest crate — one
    // component source; content scripts can't compile wasm under the page's
    // CSP). The local path-segment renderer is the fallback for a mudrad
    // that predates the `capsules` field. The sentinel must stay
    // null-vs-value: "" is a legal server answer ("no tags"), only
    // null/undefined means "old backend".
    const seg = (text, cls) => {
      const s = document.createElement("span");
      if (cls) s.className = cls;
      s.textContent = text;
      return s;
    };
    this.left.textContent = "";
    for (const part of [d.ctx, d.count, mode].filter(Boolean)) {
      this.left.appendChild(seg(part));
    }
    if (d.capsules != null) {
      if (d.capsules) {
        const holder = document.createElement("span");
        holder.innerHTML = d.capsules;
        this.left.appendChild(holder);
      }
    } else {
      for (const path of d.tags || []) {
        const segs = path.split("::");
        const cap = seg("", "capsule");
        segs.forEach((name, i) => {
          cap.appendChild(seg(name, "seg" + (i === segs.length - 1 ? " leaf" : "")));
        });
        this.left.appendChild(cap);
      }
    }

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

  // ---- command mode: the whole bar becomes an input line (: prompt + input filling it),
  // candidate popup above the input, full width, at most maxCandidates entries, scrollable beyond that. ----
  // onInput(query, api) filters candidates on the host side; onPick(candidate, query, api) handles selection;
  // candidate = {label, value, desc?}；Esc → onPick(null, ...)。A candidate with `desc` renders
  // as two columns (name padded to a shared width, description in dim gray) so name and
  // description never blur into one run of text.
  // Optional host hooks for non-command pickers: onTab(candidate, api) replaces the default
  // fill-and-refilter (drill-down semantics), onBackspace(api) fires when Backspace is pressed
  // on an empty input (layer-up semantics). opts.prompt replaces the leading ":" marker
  // (a host entered by a key binding shows what that key would have typed, e.g. ":open ").
  async openCommand(onInput, onPick, onTab, onBackspace, opts) {
    if (!this.el) await this.mount();
    const cfg = await MudraConfig.all();

    // Candidate popup: flush with the bar's top edge, 100% width (left0/right0), at most maxCandidates rows tall
    const rowH = cfg.statusHeight + 2;
    const box = document.createElement("div");
    box.id = "mudra-cmdbox";
    box.style.cssText = [
      "position:fixed", "left:0", "right:0", `bottom:${cfg.statusHeight}px`,
      "z-index:2147483646", `max-height:${cfg.maxCandidates * rowH}px`,
      "overflow-y:auto", "box-sizing:border-box", "background:" + cfg.statusBg,
    ].join(";");
    const list = document.createElement("div");
    list.id = "mudra-cmdlist";
    box.appendChild(list);
    document.documentElement.appendChild(box);

    // The input line REPLACES the bar (not appended into it): any host
    // node inserted into a frame-owned tree gets shuffled by re-renders —
    // the bar (and later Solid) taught this the hard way. A standalone
    // fixed line at the same position avoids all ownership conflicts; the
    // bar is hidden while open.
    this.bar.style.visibility = "hidden";
    const line = document.createElement("div");
    line.id = "mudra-cmdline";
    line.style.cssText = [
      "position:fixed", "left:0", "right:0", "bottom:0", "z-index:2147483647",
      `height:${cfg.statusHeight}px`, `font:${cfg.statusFont}`,
      `color:${cfg.insertFg}`, `background:${cfg.insertBg}`,
      "display:flex", "align-items:center", "padding:0 6px", "box-sizing:border-box",
    ].join(";");
    const promptEl = document.createElement("span");
    promptEl.textContent = (opts && opts.prompt) || ":";
    promptEl.style.whiteSpace = "pre"; // keep the trailing space of ":open "
    const input = document.createElement("input");
    input.id = "mudra-cmdinput";
    input.style.cssText = [
      "flex:1", "min-width:0", "background:transparent", "border:none", "outline:none",
      `color:${cfg.insertFg}`, `font:${cfg.statusFont}`, "padding:0",
    ].join(";");
    line.appendChild(promptEl);
    line.appendChild(input);
    document.documentElement.appendChild(line);
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
          "background:" + (i === sel ? "rgba(128,128,255,.35)" : "transparent"),
          "color:" + (i === sel ? cfg.insertFg : cfg.statusFg),
        ].join(";");
        list.appendChild(row);
      });
    };

    const close = () => {
      document.getElementById("mudra-cmdline")?.remove();
      document.getElementById("mudra-cmdbox")?.remove();
      if (this.bar) this.bar.style.visibility = "";
      this._paint(); // colors come from the stored state's mode again
    };

    const api = {
      setItems(next) { items = next; sel = Math.min(sel, Math.max(0, items.length - 1)); renderList(); },
      value: () => input.value,
      close,
    };

    input.addEventListener("input", () => { sel = 0; onInput(input.value, api); });
    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Escape") { e.preventDefault(); close(); onPick(null, input.value, api); }
      else if (e.key === "ArrowDown") { e.preventDefault(); sel = Math.min(sel + 1, items.length - 1); renderList(); }
      else if (e.key === "ArrowUp") { e.preventDefault(); sel = Math.max(sel - 1, 0); renderList(); }
      else if (e.key === "Tab") {
        e.preventDefault();
        if (!items[sel]) return;
        // Drill-down hosts take over Tab entirely; default is fill the selected value and refilter
        if (onTab) onTab(items[sel], api);
        else { input.value = items[sel].value; sel = 0; onInput(input.value, api); input.focus(); }
      }
      else if (e.key === "Backspace" && input.value === "" && onBackspace) {
        // Layer-up hook: only fires on an empty input, so normal text editing is unaffected
        e.preventDefault(); onBackspace(api);
      }
      else if (e.key === "Enter") { e.preventDefault(); close(); onPick(items[sel] || null, input.value, api); }
    });
    return api;
  },

  unmount() {
    if (this.el) this.el.remove();
    this.el = null;
    this.bar = null;
    this.left = null;
    this.right = null;
    this._state = {};
  },
};
