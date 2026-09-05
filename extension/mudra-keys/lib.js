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
    scrollStepLines: 3,              // j/k 每次滚动行数
    pageOverlapLines: 5,             // w/s 翻页时保留的行数（重叠）
    maxCandidates: 10,               // 命令模式弹出菜单最多显示条目数
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
      <span id="mudra-bar-left" style="display:flex;gap:8px;align-items:center;min-width:0"></span>
      <span id="mudra-bar-right" style="display:flex;gap:10px;align-items:center;overflow:hidden"></span>`;
    document.documentElement.appendChild(bar);
    // 状态栏要在最外层：经典滚动条画在所有元素之上（z-index 压不住），
    // 而滚动位置已在 bar 右侧显示 → 直接隐藏页面滚动条（滚轮/键盘滚动不受影响）。
    const st = document.createElement("style");
    st.id = "mudra-scrollbar-style";
    st.textContent = "html { scrollbar-width: none !important; } html::-webkit-scrollbar { display: none !important; }";
    document.documentElement.appendChild(st);
    this.el = bar;
    this.slots.left = bar.querySelector("#mudra-bar-left");
    this.slots.right = bar.querySelector("#mudra-bar-right");
    this.render({});
    return this;
  },

  // data: {ctx, mode, title, url, scroll, tags, message}
  async render(data) {
    if (!this.el) return;
    // command 模式时 bar 是输入行，render 不覆盖（输入行由 openCommand 自己维护）
    if (document.getElementById("mudra-cmdinput")) return;
    const cfg = await MudraConfig.all();
    const mode = data.mode || "normal";
    const colors = {
      normal: { fg: cfg.statusFg, bg: cfg.statusBg },
      insert: { fg: cfg.insertFg, bg: cfg.insertBg },
      hint:   { fg: cfg.statusFg, bg: "#204080" },
    }[mode] || { fg: cfg.statusFg, bg: cfg.statusBg };
    this.el.style.color = colors.fg;
    this.el.style.background = colors.bg;
    const left = [data.ctx, data.count, mode, ...(data.tags || [])].filter(Boolean).join(" · ");
    const right = data.message
      ? data.message
      : `${data.title || ""} ${data.url || ""}${data.scroll != null ? " " + data.scroll : ""}`;
    this.slots.left.textContent = left;
    this.slots.right.textContent = right;
  },

  // ---- command 模式：bar 整条变输入行（: 提示符 + 输入框占满），
  // 候选浮层在输入行上方，宽度 100%，最多 maxCandidates 条，超出滚动。----
  // onInput(query, api) 由宿主过滤候选；onPick(candidate, query, api) 处理选中；
  // candidate = {label, value}；Esc → onPick(null, ...)。
  async openCommand(onInput, onPick) {
    if (!this.el) await this.mount();
    const cfg = await MudraConfig.all();

    // 候选浮层：贴在 bar 上缘，宽度 100%（left0/right0），高度最多 maxCandidates 行
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

    // 输入行接管整条 bar：左侧 ": " 提示符，输入框占满剩余宽度
    const bar = this.el;
    bar.style.color = cfg.insertFg;
    bar.style.background = cfg.insertBg;
    bar.style.pointerEvents = "auto";
    this.slots.left.textContent = ":";
    // 输入行接管整条 bar：right slot 隐藏（flex 布局下会残留占位），输入框占满其余宽度
    this.slots.right.style.display = "none";
    const input = document.createElement("input");
    input.id = "mudra-cmdinput";
    input.style.cssText = [
      "flex:1", "min-width:0", "background:transparent", "border:none", "outline:none",
      `color:${cfg.insertFg}`, `font:${cfg.statusFont}`, "padding:0",
    ].join(";");
    this.slots.left.parentElement.appendChild(input);
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
      this.slots.right.style.display = "";
      bar.style.pointerEvents = "none";
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
        // Tab 补全：把当前选中候选的命令名（value，非含描述的 label）填入输入框，重新过滤
        e.preventDefault();
        if (items[sel]) { input.value = ":" + items[sel].value; sel = 0; onInput(input.value, api); input.focus(); }
      }
      else if (e.key === "Enter") { e.preventDefault(); close(); onPick(items[sel] || null, input.value, api); }
    });
    return api;
  },

  unmount() {
    if (this.el) this.el.remove();
    this.el = null;
  },
};
