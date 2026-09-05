// mudra-keys shared settings + status-bar widget library.
// Used by content.js here and reusable by other mudra frontends (panel, CLI).

const MudraConfig = {
  defaults: {
    hintChars: "asdfghjkl",          // 字母池，顺序即分配顺序
    hintFontSize: 12,                // px
    statusHeight: 16,                // px, 一个字符高
    statusFont: "12px monospace",
    statusFg: "#ffffff",             // 普通模式白字
    statusBg: "#000000",             // 普通模式黑底
    insertFg: "#000000",
    insertBg: "#e8e8e8",
    hintFg: "#000000",
    hintBg: "#ffd76e",
    keybindings: null,               // {key: command}；null = 用 COMMANDS 的 defaultKey
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

  // 从 JSON 字符串导入（整体替换 keybindings）
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

// ---- status bar（qutebrowser 风格：底部一条，一字符高）----
// 左侧：ctx 标签 + 模式（normal/insert/hint/command）；右侧：title + url + 滚动位置。
// 做成公用库：MudraBar.mount(el?) 返回控制器，宿主页面/扩展均可复用。
const MudraBar = {
  el: null,
  slots: {},

  async mount() {
    if (this.el && this.el.isConnected) return this;
    const cfg = await MudraConfig.all();
    const bar = document.createElement("div");
    bar.id = "mudra-bar";
    bar.style.cssText = [
      "position:fixed", "left:0", "right:0", "bottom:0", "z-index:2147483647",
      `height:${cfg.statusHeight}px`,
      `font:${cfg.statusFont}`,
      `color:${cfg.statusFg}`, `background:${cfg.statusBg}`,
      "display:flex", "align-items:center", "justify-content:space-between",
      "padding:0 6px", "box-sizing:border-box", "user-select:none",
      "pointer-events:none", "white-space:nowrap", "overflow:hidden",
    ].join(";");
    bar.innerHTML = `
      <span id="mudra-bar-left" style="display:flex;gap:8px;align-items:center"></span>
      <span id="mudra-bar-right" style="display:flex;gap:10px;align-items:center;overflow:hidden"></span>`;
    document.documentElement.appendChild(bar);
    this.el = bar;
    this.slots.left = bar.querySelector("#mudra-bar-left");
    this.slots.right = bar.querySelector("#mudra-bar-right");
    this.render({});
    return this;
  },

  // data: {ctx, mode, title, url, scroll, tags, message}
  async render(data) {
    if (!this.el) return;
    const cfg = await MudraConfig.all();
    const mode = data.mode || "normal";
    const colors = {
      normal: { fg: cfg.statusFg, bg: cfg.statusBg },
      insert: { fg: cfg.insertFg, bg: cfg.insertBg },
      hint:   { fg: cfg.statusFg, bg: "#204080" },
    }[mode] || { fg: cfg.statusFg, bg: cfg.statusBg };
    this.el.style.color = colors.fg;
    this.el.style.background = colors.bg;
    const left = [data.ctx, mode, ...(data.tags || [])].filter(Boolean).join(" · ");
    const right = data.message
      ? data.message
      : `${data.title || ""} ${data.url || ""}${data.scroll != null ? " " + data.scroll : ""}`;
    this.slots.left.textContent = left;
    this.slots.right.textContent = right;
  },

  // ---- command 模式：bar 变输入栏，候选向上展开（向上浮层，随输入过滤）----
  // onPick(candidate, query) 由宿主实现；candidate = {label, value}
  async openCommand(onInput, onPick) {
    if (!this.el) await this.mount();
    const cfg = await MudraConfig.all();
    this.el.style.color = cfg.insertFg;
    this.el.style.background = cfg.insertBg;

    // 输入行复用 bar；候选浮层贴 bar 上缘
    const box = document.createElement("div");
    box.id = "mudra-cmdbox";
    box.style.cssText = [
      "position:fixed", "left:0", `bottom:${cfg.statusHeight}px`, "z-index:2147483646",
      "max-height:40vh", "overflow-y:auto", "direction:rtl", // 滚动条放右、内容反排
      "box-sizing:border-box",
    ].join(";");
    const list = document.createElement("div");
    list.style.direction = "ltr";
    list.id = "mudra-cmdlist";
    box.appendChild(list);
    document.documentElement.appendChild(box);

    const input = document.createElement("input");
    input.id = "mudra-cmdinput";
    input.style.cssText = [
      "flex:1", "width:70%", "background:transparent", "border:none", "outline:none",
      `color:${cfg.insertFg}`, `font:${cfg.statusFont}`, "padding:0",
    ].join(";");
    this.slots.left.textContent = "cmd";
    this.slots.right.textContent = "";
    this.slots.right.appendChild(input);
    input.focus();

    let items = [];
    let sel = 0;
    const renderList = () => {
      list.textContent = "";
      items.forEach((it, i) => {
        const row = document.createElement("div");
        row.textContent = (i === sel ? "» " : "  ") + it.label;
        row.style.cssText = [
          `font:${cfg.statusFont}`, `padding:1px 6px`, "white-space:nowrap",
          "background:" + (i === sel ? "rgba(128,128,255,.35)" : "transparent"),
          "color:" + (i === sel ? cfg.insertFg : cfg.statusFg),
        ].join(";");
        list.appendChild(row);
      });
    };

    const api = {
      setItems(next) { items = next; sel = Math.min(sel, Math.max(0, items.length - 1)); renderList(); },
      value: () => input.value,
      close() {
        input.remove();
        document.getElementById("mudra-cmdbox")?.remove();
      },
    };

    input.addEventListener("input", () => { sel = 0; onInput(input.value, api); });
    input.addEventListener("keydown", (e) => {
      if (e.key === "Escape") { e.preventDefault(); onPick(null, input.value, api); }
      else if (e.key === "ArrowDown") { e.preventDefault(); sel = Math.min(sel + 1, items.length - 1); renderList(); }
      else if (e.key === "ArrowUp") { e.preventDefault(); sel = Math.max(sel - 1, 0); renderList(); }
      else if (e.key === "Enter") { e.preventDefault(); onPick(items[sel] || null, input.value, api); }
      e.stopPropagation();
    });
    return api;
  },

  unmount() {
    if (this.el) this.el.remove();
    this.el = null;
  },
};
