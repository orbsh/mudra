// mudra-keys content script: letter hints, H/L history, insert mode, status bar,
// command mode (`:`), configurable keybindings, pages command.
// Link opening / tag / page ops go through sw.js -> mudrad (tabId routing, ctx resolved there).
(() => {
  if (window.__mudraKeys) return;
  window.__mudraKeys = true;

  const send = (msg) =>
    new Promise((res) => {
      try { chrome.runtime.sendMessage(msg, (r) => res(r || {})); }
      catch (e) { res({ ok: false, err: String(e) }); }
    });

  // ---- state ----
  let mode = "normal"; // normal | insert | hint | command
  let ctx = "";
  let pageTags = [];   // 当前页已打 tag（mudrad 权威，本地只做显示缓存）
  let hintSession = null;
  let cmdApi = null;
  // 控制台页面（mudra ui 总控面板）：扩展行为切换的开关
  const isConsole = () => location.host === "127.0.0.1:9299" || location.host === "localhost:9299";

  const setMode = async (m) => { mode = m; if (m !== "command") await refreshBar(); };
  const scrollPct = () => {
    const h = document.documentElement.scrollHeight - innerHeight;
    return h > 0 ? `${Math.round((scrollY / h) * 100)}%` : "all";
  };
  const pageUrl = () => location.origin + location.pathname + location.search;

  const refreshBar = async () => {
    await MudraBar.render({
      ctx, mode, tags: pageTags,
      title: document.title.slice(0, 60),
      url: location.host + location.pathname,
      scroll: scrollPct(),
    });
  };

  // 状态数据从 mudrad 拉取（导航/定时），失败静默（mudrad 不在时 bar 仍可用）
  const syncStatus = async () => {
    const r = await send({ type: "status", url: pageUrl() });
    if (r && r.ok !== false) {
      ctx = r.ctx || "";
      pageTags = r.tags || [];
    } else {
      flashBar("status: " + ((r && r.err) || "no response"));
    }
  };

  // ---- commands（: 命令模式的目标；也是键绑定的目标）----
  const COMMANDS = {
    hint:    { defaultKey: "f", desc: "link hints", run: () => setMode("hint").then(() => showHints().then((s) => { hintSession = s; })) },
    back:    { defaultKey: "h", desc: "history back", run: () => history.go(-1) },
    forward: { defaultKey: "l", desc: "history forward", run: () => history.go(1) },
    insert:  { defaultKey: "i", desc: "insert mode", run: () => setMode("insert") },
    tag:     { defaultKey: "t", desc: "toggle tag on this page", run: () => showTagPrompt() },
    refresh: { defaultKey: "r", desc: "reload page", run: () => location.reload() },
    open:    { defaultKey: "o", desc: "open url (mudrad)", run: (arg) => arg && send({ type: "open", url: arg }).then((r) => flashBar(r.ok === false ? r.err || "open failed" : "open ok")) },
    pages:   { defaultKey: "P", desc: "switch page (mudrad)", run: () => showPages() },
    top:     { defaultKey: "g", desc: "scroll top", run: () => scrollTo({ top: 0 }) },
    bottom:  { defaultKey: "G", desc: "scroll bottom", run: () => scrollTo({ top: document.body.scrollHeight }) },
  };

  // 键绑定解析：配置 JSON 的 keybindings[key]=commandName 覆盖 defaultKey
  async function keyToCommand(key) {
    const cfg = await MudraConfig.all();
    const map = cfg.keybindings || {};
    for (const [name, def] of Object.entries(COMMANDS)) {
      const bound = Object.keys(map).find((k) => map[k] === name);
      if ((bound || def.defaultKey) === key) return name;
    }
    return null;
  }

  // ---- command mode（: 进入；bar 变输入栏，候选项向上展开过滤）----
  function filterCmd(q) {
    if (!cmdApi) return;
    q = (q || "").replace(/^:/, "");
    const [name, ...rest] = q.split(/\s+/);
    let items;
    if (!q) {
      items = Object.entries(COMMANDS).map(([n, c]) => ({ label: `:${n}  ${c.desc}`, value: n }));
    } else if (COMMANDS[name] && rest.length !== undefined && q.includes(" ")) {
      // 参数补全：pages 无参数；open 补 URL；其余无参
      items = [];
    } else {
      items = Object.entries(COMMANDS)
        .filter(([n]) => n.startsWith(name))
        .map(([n, c]) => ({ label: `:${n}  ${c.desc}`, value: n }));
    }
    cmdApi.setItems(items);
  }

  async function pickCmd(cand, raw, api) {
    api.close();
    cmdApi = null;
    setMode("normal");
    if (!cand) return; // Esc / 空
    const q = (raw || "").replace(/^:/, "").trim();
    const [name, ...rest] = q.split(/\s+/);
    const c = COMMANDS[cand.value] || COMMANDS[name];
    if (!c) return flashBar(`no such command: ${name}`);
    if (!c.run.length && rest.length) return flashBar(`:${cand.value} takes no argument`);
    await c.run(rest.join(" "));
    await refreshBar();
  }

  const enterCommand = async () => {
    setMode("command");
    cmdApi = await MudraBar.openCommand(filterCmd, pickCmd);
    filterCmd(":");
  };

  // ---- pages 命令：mudrad 页列表 → 候选过滤 → focus_page ----
  async function showPages() {
    const r = await send({ type: "pages" });
    if (r.ok === false) return flashBar(r.err || "pages failed");
    const pages = r.pages || [];
    setMode("command");
    cmdApi = await MudraBar.openCommand(
      (q, api) => {
        const s = (q || "").toLowerCase();
        api.setItems(pages
          .filter((p) => !s || (p.title + " " + p.url + " " + p.ctx).toLowerCase().includes(s))
          .map((p) => ({ label: `[${p.ctx}] ${p.title.slice(0, 60)}`, value: p.id })));
      },
      async (cand, _raw, api) => {
        api.close(); cmdApi = null; setMode("normal");
        if (!cand) return;
        const res = await send({ type: "focus_page", page_id: cand.value });
        await flashBar(res.ok === false ? (res.err || "focus failed") : "switched");
      }
    );
    // 初始列表
    cmdApi.setItems(pages.map((p) => ({ label: `[${p.ctx}] ${p.title.slice(0, 60)}`, value: p.id })));
  }

  // ---- hints ----
  const HINT_ATTR = "data-mudra-hint";
  const clickable = () => {
    const els = document.querySelectorAll(
      "a[href], button, input[type=submit], input[type=button], [role=button], [onclick]"
    );
    return [...els].filter((el) => {
      const r = el.getBoundingClientRect();
      const st = getComputedStyle(el);
      return r.width > 0 && r.height > 0 && st.visibility !== "hidden" &&
             st.display !== "none" && r.bottom > 0 && r.top < innerHeight;
    });
  };

  // 字母序列：hintChars 池 + 短码优先（首字母尽量落在池首，单手区）
  function hintStrings(n, chars) {
    const out = [];
    let len = 1;
    while (out.length < n) {
      const total = Math.pow(chars.length, len);
      for (let i = 0; i < total && out.length < n; i++) {
        let s = "", x = i;
        for (let j = 0; j < len; j++) { s = chars[x % chars.length] + s; x = Math.floor(x / chars.length); }
        out.push(s);
      }
      len++;
    }
    return out;
  }

  async function showHints() {
    const c = await MudraConfig.all();
    const els = clickable();
    const seqs = hintStrings(els.length, c.hintChars.split(""));
    const overlay = document.createElement("div");
    overlay.id = "mudra-hints";
    overlay.style.cssText = "position:absolute;top:0;left:0;z-index:2147483646;pointer-events:none";
    const nodes = els.map((el, i) => {
      const r = el.getBoundingClientRect();
      const d = document.createElement("span");
      d.textContent = seqs[i];
      d.setAttribute(HINT_ATTR, seqs[i]);
      d.style.cssText = [
        "position:fixed", `left:${Math.max(0, r.left)}px`, `top:${Math.max(0, r.top)}px`,
        `font:${c.hintFontSize}px monospace`, `color:${c.hintFg}`, `background:${c.hintBg}`,
        "padding:0 2px", "border-radius:2px", "white-space:pre",
      ].join(";");
      overlay.appendChild(d);
      return { el, seq: seqs[i] };
    });
    document.documentElement.appendChild(overlay);
    return { overlay, nodes };
  }

  function hintFilter(typed) {
    if (!hintSession) return;
    const { overlay, nodes } = hintSession;
    let alive = 0;
    for (const n of nodes) {
      const label = n.el.querySelector?.(`[${HINT_ATTR}]`);
      const match = n.seq.startsWith(typed);
      if (label) label.style.display = match ? "" : "none";
      if (match) alive++;
    }
    if (alive === 1) {
      const hit = nodes.find((n) => n.seq === typed || n.seq.startsWith(typed) &&
        nodes.filter((m) => m.seq.startsWith(typed)).length === 1);
      if (hit && typed === hit.seq) activate(hit.el);
    }
    if (alive === 0) hideHints();
  }

  function activate(el) {
    hideHints();
    const a = el.closest("a");
    const href = a?.href;
    if (href && a.hash && a.pathname === location.pathname && a.search === location.search) {
      location.hash = a.hash; // 同页锚点原地跳，不开新窗
    } else if (href && /^https?:/.test(href)) {
      send({ type: "open", url: href }); // mudrad 拉新 --app 窗
    } else {
      el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    }
  }

  function hideHints() {
    document.getElementById("mudra-hints")?.remove();
    hintSession = null;
    setMode("normal");
  }

  // ---- tag 提示条（按 t 呼出，选字母打 tag，Esc 取消）----
  let tagPrompt = null;
  async function showTagPrompt() {
    const roots = await send({ type: "tags" });
    if (!roots.tags || !roots.tags.length) { await flashBar("no tags"); return; }
    // 一级根 + 各自首层子节点平铺：根大写、子小写（够用即可，深树后面迭代）
    const items = [];
    for (const root of roots.tags) {
      items.push({ seq: root[0].toUpperCase(), tag: root });
      const kids = await send({ type: "tags", parent: root });
      for (const k of (kids.tags || [])) {
        if (!items.some((it) => it.seq === k[0])) items.push({ seq: k[0], tag: k });
      }
    }
    const c = await MudraConfig.all();
    const p = document.createElement("div");
    p.id = "mudra-tagprompt";
    p.style.cssText = [
      "position:fixed", "left:0", `bottom:${c.statusHeight}px`, "z-index:2147483647",
      `font:${c.statusFont}`, `color:${c.statusFg}`, `background:${c.statusBg}`,
      "padding:2px 8px", "max-width:100%", "box-sizing:border-box", "overflow:hidden",
    ].join(";");
    p.textContent = "tag: " + items.map((it) => `${it.seq}=${it.tag}`).join("  ");
    document.documentElement.appendChild(p);
    tagPrompt = { items };
    setMode("hint");
  }
  function hideTagPrompt() {
    document.getElementById("mudra-tagprompt")?.remove();
    tagPrompt = null;
    setMode("normal");
  }
  async function pickTag(seq) {
    const hit = tagPrompt?.items.find((it) => it.seq === seq);
    hideTagPrompt();
    if (!hit) return;
    const r = await send({ type: "tag", url: pageUrl(), tag: hit.tag });
    await flashBar(r.action ? `${hit.tag} ${r.action}` : (r.err || "tag failed"));
    await syncStatus();
  }
  async function flashBar(text) {
    await MudraBar.render({ ctx, mode, tags: pageTags, message: text });
    setTimeout(refreshBar, 1500);
  }

  // ---- key handling ----
  const isEditable = (t) =>
    t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));

  document.addEventListener("keydown", async (e) => {
    if (mode === "command") return; // 命令输入由 input 自己处理
    if (mode === "insert") {
      if (e.key === "Escape") { e.preventDefault(); setMode("normal"); }
      return; // 插入模式全放行
    }
    if (isEditable(e.target) && mode !== "hint") return; // 输入框聚焦时不劫持

    if (tagPrompt) {
      e.preventDefault();
      if (e.key === "Escape") return hideTagPrompt();
      if (/^[a-zA-Z]$/.test(e.key)) pickTag(e.key);
      return;
    }
    if (mode === "hint") {
      e.preventDefault();
      if (e.key === "Escape") return hideHints();
      if (/^[a-z]$/.test(e.key)) hintFilter(e.key);
      return;
    }
    // normal: : 进入命令模式
    if (!e.ctrlKey && !e.metaKey && !e.altKey && e.key === ":") {
      e.preventDefault();
      return enterCommand();
    }
    // normal: 键位 → 命令（可配置）
    const plain = !e.ctrlKey && !e.metaKey && !e.altKey;
    if (plain) {
      const name = await keyToCommand(e.key);
      if (name) {
        e.preventDefault();
        COMMANDS[name].run("");
      }
    }
  }, true);

  // 插入模式仅显式 i 进入 / Esc 退出；不随 focusin 自动切（页面 autofocus 会让 normal 永远不可达）。
  // 输入框有焦点时按键不劫持（isEditable 检查已覆盖），打字天然可用。

  // ---- status bar ----
  const boot = async () => {
    await MudraBar.mount();
    await refreshBar();
    if (!isConsole()) await syncStatus(); // console ui 页不属于任何 ctx，跳过反查
    addEventListener("scroll", refreshBar, { passive: true });
    // SPA 导航时 URL 变化 → 重拉状态
    let lastUrl = pageUrl();
    setInterval(async () => {
      if (pageUrl() !== lastUrl) { lastUrl = pageUrl(); if (!isConsole()) await syncStatus(); }
    }, 1000);
  };
  if (document.readyState === "loading")
    document.addEventListener("DOMContentLoaded", boot);
  else boot();
})();
