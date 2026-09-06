// mudra-keys shared settings + status-bar widget library.
// Used by content.js here and reusable by other mudra frontends (panel, CLI).
// Rendering is SolidJS (window.MudraSolid from solid-bundle.js); tag capsules
// come from window.MudraTags (tags.js) — same components as the panel.

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
// Solid implementation: the bar is a Solid root; render() only flips signals and the DOM updates incrementally.
// Left: ctx - numeric prefix - mode - tag capsule string; right: title + url + scroll position.
const MudraBar = {
  el: null,
  _setState: null, // {data, cfg} signals
  _dispose: null,

  async mount() {
    if (this.el && this.el.isConnected) return this;
    const { h, render, createSignal } = window.MudraSolid;
    const cfg = await MudraConfig.all();

    const [data, setData] = createSignal({});
    const [command, setCommand] = createSignal(false); // command input line has taken over
    const [cfgSig] = createSignal(cfg);
    this._setState = { data, setData, command, setCommand, cfg: cfgSig };

    const colors = (mode) => ({
      normal: { fg: cfg.statusFg, bg: cfg.statusBg },
      insert: { fg: cfg.insertFg, bg: cfg.insertBg },
      hint:   { fg: cfg.statusFg, bg: "#204080" },
    }[mode] || { fg: cfg.statusFg, bg: cfg.statusBg });

    // Capsule string: tags is an array of paths (state::unread); reuses the panel Capsule rendering logic.
    // The browser-side capsule is read-only display (click actions get menus later); render the segment structure first.
    const Capsule = (path) => {
      const segs = path.split("::");
      return h("span.capsule",
        segs.map((seg, i) => h("span", { class: "seg" + (i === segs.length - 1 ? " leaf" : "") }, seg)));
    };

    const Bar = () => {
      // A Solid component body runs only once: reading data() at top level is untracked, so the DOM would freeze at first render.
      // Dynamic content must be passed to h() as function children so Solid establishes reactive insertion.
      const c = cfgSig();
      const left = () => {
        const d = data();
        const mode = d.mode || "normal";
        return [d.ctx, d.count, mode, ...(d.tags || []).map(Capsule)].filter(Boolean);
      };
      const right = () => {
        const d = data();
        return d.message != null
          ? d.message
          : `${d.title || ""} ${d.url || ""}${d.scroll != null ? " " + d.scroll : ""}`;
      };
      const barStyle = () => {
        const d = data();
        const col = colors(d.mode || "normal");
        return {
          position: "fixed", left: "0", right: "0", bottom: "0", "z-index": "2147483647",
          height: c.statusHeight + "px", font: c.statusFont,
          color: col.fg, background: col.bg,
          display: "flex", "align-items": "center", "justify-content": "space-between",
          padding: "0 6px", "box-sizing": "border-box", "user-select": "none",
          "pointer-events": command() ? "auto" : "none",
          "white-space": "nowrap", overflow: "hidden",
        };
      };
      return h("div#mudra-bar", { style: barStyle }, [
        h("span#mudra-bar-left", {
          style: { display: "flex", gap: "6px", "align-items": "center", "min-width": "0" },
        }, left),
        h("span#mudra-bar-right", {
          style: {
            display: () => (command() ? "none" : "flex"),
            gap: "10px", "align-items": "center", overflow: "hidden", "flex-direction": "row",
          },
        }, right),
      ]);
    };

    const root = document.createElement("div");
    root.id = "mudra-bar-root";
    document.documentElement.appendChild(root);
    // The status bar must be outermost: the classic scrollbar paints above all elements (z-index cannot suppress it),
    // and the scroll position is already shown on the bar's right -> hide the page scrollbar outright.
    const st = document.createElement("style");
    st.id = "mudra-scrollbar-style";
    st.textContent = "html { scrollbar-width: none !important; } html::-webkit-scrollbar { display: none !important; }";
    document.documentElement.appendChild(st);
    this.el = root;
    this._dispose = render(Bar, root);
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
    return this;
  },

  // data: {ctx, mode, title, url, scroll, tags(path array), message, count}
  async render(data) {
    if (!this.el) return;
    // In command mode the bar is an input line; render must not overwrite it (openCommand maintains the input itself)
    if (document.getElementById("mudra-cmdinput")) return;
    this._setState.setData({ ...data });
  },

  // ---- command mode: the whole bar becomes an input line (: prompt + input filling it),
  // candidate popup above the input, full width, at most maxCandidates entries, scrollable beyond that. ----
  // onInput(query, api) filters candidates on the host side; onPick(candidate, query, api) handles selection;
  // candidate = {label, value}；Esc → onPick(null, ...)。
  async openCommand(onInput, onPick) {
    if (!this.el) await this.mount();
    const cfg = await MudraConfig.all();
    const { setCommand } = this._setState;

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

    // The input line takes over the whole bar: right slots hidden (Solid-rendered), input appended inside the bar
    setCommand(true);
    const bar = document.getElementById("mudra-bar");
    bar.style.color = cfg.insertFg;
    bar.style.background = cfg.insertBg;
    const left = document.getElementById("mudra-bar-left");
    left.textContent = ":";
    const input = document.createElement("input");
    input.id = "mudra-cmdinput";
    input.style.cssText = [
      "flex:1", "min-width:0", "background:transparent", "border:none", "outline:none",
      `color:${cfg.insertFg}`, `font:${cfg.statusFont}`, "padding:0",
    ].join(";");
    bar.appendChild(input);
    input.focus();

    let items = [];
    let sel = 0;
    const renderList = () => {
      list.textContent = "";
      items.forEach((it, i) => {
        const row = document.createElement("div");
        row.textContent = (i === sel ? "» " : "  ") + it.label;
        row.style.cssText = [
          `font:${cfg.statusFont}`, `height:${rowH}px`, "line-height:" + rowH + "px",
          "padding:0 6px", "white-space:nowrap", "box-sizing:border-box",
          "background:" + (i === sel ? "rgba(128,128,255,.35)" : "transparent"),
          "color:" + (i === sel ? cfg.insertFg : cfg.statusFg),
        ].join(";");
        list.appendChild(row);
      });
    };

    const close = () => {
      input.remove();
      document.getElementById("mudra-cmdbox")?.remove();
      setCommand(false);
      this._setState.setData((d) => ({ ...d })); // restore normal rendering (colors come from the mode signal)
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
        // Tab completion: fill the input with the selected candidate's command name (value, not the descriptive label) and refilter
        e.preventDefault();
        if (items[sel]) { input.value = ":" + items[sel].value; sel = 0; onInput(input.value, api); input.focus(); }
      }
      else if (e.key === "Enter") { e.preventDefault(); close(); onPick(items[sel] || null, input.value, api); }
    });
    return api;
  },

  unmount() {
    if (this._dispose) this._dispose();
    this._dispose = null;
    if (this.el) this.el.remove();
    this.el = null;
  },
};
