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
};

// ---- status bar（qutebrowser 风格：底部一条，一字符高）----
// 左侧：ctx 标签 + 模式（normal/insert/hint）；右侧：title + url + 滚动位置。
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

  // data: {ctx, mode, title, url, scroll}
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
    this.slots.left.textContent = [data.ctx, mode].filter(Boolean).join(" · ");
    const pct = data.scroll != null ? ` ${data.scroll}` : "";
    this.slots.right.textContent = `${data.title || ""} ${data.url || ""}${pct}`;
  },

  unmount() {
    if (this.el) this.el.remove();
    this.el = null;
  },
};
