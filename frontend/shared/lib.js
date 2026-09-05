// mudra-keys shared settings + status-bar widget library.
// Used by content.js here and reusable by other mudra frontends (panel, CLI).
// Rendering is SolidJS (window.MudraSolid from solid-bundle.js); tag capsules
// come from window.MudraTags (tags.js) — same components as the panel.

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
// Solid 实现：bar 是一个 Solid root，render() 只改 signal，DOM 增量更新。
// 左侧：ctx · 数字前缀 · 模式 · tag 胶囊串；右侧：title + url + 滚动位置。
const MudraBar = {
  el: null,
  _setState: null, // {data, cfg} signals
  _dispose: null,

  async mount() {
    if (this.el && this.el.isConnected) return this;
    const { h, render, createSignal } = window.MudraSolid;
    const cfg = await MudraConfig.all();

    const [data, setData] = createSignal({});
    const [command, setCommand] = createSignal(false); // command 输入行接管中
    const [cfgSig] = createSignal(cfg);
    this._setState = { data, setData, command, setCommand, cfg: cfgSig };

    const colors = (mode) => ({
      normal: { fg: cfg.statusFg, bg: cfg.statusBg },
      insert: { fg: cfg.insertFg, bg: cfg.insertBg },
      hint:   { fg: cfg.statusFg, bg: "#204080" },
    }[mode] || { fg: cfg.statusFg, bg: cfg.statusBg });

    // 胶囊串：tags 是路径数组（state::未读），复用 panel 的 Capsule 渲染逻辑。
    // 浏览器侧胶囊是只读显示（点击行为后续接菜单），先渲染段结构。
    const Capsule = (path) => {
      const segs = path.split("::");
      return h("span.capsule",
        segs.map((seg, i) => h("span", { class: "seg" + (i === segs.length - 1 ? " leaf" : "") }, seg)));
    };

    const Bar = () => {
      // Solid 组件体只运行一次：顶层读 data() 不被追踪，DOM 会停在首次渲染。
      // 动态内容必须以函数子节点传入 h()，由 Solid 建立 reactive insertion。
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
    // 状态栏要在最外层：经典滚动条画在所有元素之上（z-index 压不住），
    // 而滚动位置已在 bar 右侧显示 → 直接隐藏页面滚动条。
    const st = document.createElement("style");
    st.id = "mudra-scrollbar-style";
    st.textContent = "html { scrollbar-width: none !important; } html::-webkit-scrollbar { display: none !important; }";
    document.documentElement.appendChild(st);
    this.el = root;
    this._dispose = render(Bar, root);
    // 胶囊/模式段的样式与 panel 共用（styles.css 只在 panel 加载），这里内联注入
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

  // data: {ctx, mode, title, url, scroll, tags(path 数组), message, count}
  async render(data) {
    if (!this.el) return;
    // command 模式时 bar 是输入行，render 不覆盖（输入行由 openCommand 自己维护）
    if (document.getElementById("mudra-cmdinput")) return;
    this._setState.setData({ ...data });
  },

  // ---- command 模式：bar 整条变输入行（: 提示符 + 输入框占满），
  // 候选浮层在输入行上方，宽度 100%，最多 maxCandidates 条，超出滚动。----
  // onInput(query, api) 由宿主过滤候选；onPick(candidate, query, api) 处理选中；
  // candidate = {label, value}；Esc → onPick(null, ...)。
  async openCommand(onInput, onPick) {
    if (!this.el) await this.mount();
    const cfg = await MudraConfig.all();
    const { setCommand } = this._setState;

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

    // 输入行接管整条 bar：右槽隐藏（Solid 渲染），输入框 append 到 bar 内
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
      this._setState.setData((d) => ({ ...d })); // 恢复普通渲染（颜色由 mode signal 决定）
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
    if (this._dispose) this._dispose();
    this._dispose = null;
    if (this.el) this.el.remove();
    this.el = null;
  },
};
