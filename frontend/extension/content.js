// mudra-keys content script: 模式机 normal/hint/insert/command（vimium 式），
// 字母 hints、滚动命令（含数字前缀百分比跳转）、insert 模式焦点管理、
// 状态栏、命令模式（:）、可配置键绑定。
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
  let mode = "normal"; // normal | hint | insert | command
  let ctx = "";
  let role = "page";   // page | console（mudrad 判定，前端不猜）
  let pageTags = [];   // 当前页已打 tag（mudrad 权威，本地只做显示缓存）
  let hintSession = null;   // {overlay, nodes} 链接 hints
  let inputHintSession = null; // {overlay, nodes} insert 模式的输入框选择
  let cmdApi = null;
  let numPrefix = "";       // 数字前缀（30g 百分比跳转）

  const setMode = async (m) => {
    mode = m;
    // 模式切换必须同步焦点：离开 insert 时 blur，防止焦点滞留输入框吞 Esc
    if (m !== "insert" && document.activeElement && isEditable(document.activeElement))
      document.activeElement.blur();
    await refreshBar();
  };

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
      count: numPrefix || undefined, // 数字前缀输入中（30g）
    });
  };

  // 状态数据从 mudrad 拉取（导航/定时），失败静默（mudrad 不在时 bar 仍可用）
  const syncStatus = async () => {
    const r = await send({ type: "status", url: pageUrl() });
    if (r && r.ok !== false) {
      ctx = r.ctx || "";
      role = r.role || "page";
      pageTags = r.tags || [];
      await refreshBar(); // 状态到手才渲染（boot 时先渲染的是无 ctx 版）
    } else {
      flashBar("status: " + ((r && r.err) || "no response"));
    }
  };

  // ---- open：按角色分叉（qutebrowser 式）----
  // page 角色：输入 URL → /open
  // console 角色：输入同时过滤现有 page 候选；Enter 选中候选 → focus_page，否则 → /open
  function openOnPage(arg) {
    if (!arg) return flashBar("usage: :open <url>");
    return send({ type: "open", url: arg }).then((r) =>
      flashBar(r.ok === false ? r.err || "open failed" : "open ok"));
  }
  async function openOnConsole(initial) {
    const r = await send({ type: "pages" });
    if (r.ok === false) return flashBar(r.err || "pages failed");
    const pages = r.pages || [];
    const filter = (q, api) => {
      const s = (q || "").toLowerCase();
      api.setItems(pages
        .filter((p) => !s || (p.title + " " + p.url + " " + p.ctx).toLowerCase().includes(s))
        .map((p) => ({ label: `[${p.ctx}] ${p.title.slice(0, 60)}`, value: p.id })));
    };
    const pick = async (cand, raw, api) => {
      cmdApi = null; await setMode("normal");
      const q = (raw || "").trim();
      if (cand) {
        // 有选中候选 → 跳页（Tab 补全或 ↑↓ 选中）
        const res = await send({ type: "focus_page", page_id: cand.value });
        return flashBar(res.ok === false ? (res.err || "focus failed") : "switched");
      }
      // 没有匹配候选 → 当 URL 开
      if (q) return openOnPage(q);
    };
    setMode("command");
    cmdApi = await MudraBar.openCommand(filter, pick);
    filter(initial || "", cmdApi);
    if (initial) {
      const input = document.getElementById("mudra-cmdinput");
      if (input) input.value = initial;
    }
  }

  // ---- commands ----
  // run(arg, opts)：opts = {count}（数字前缀）、{newTab}（F 变体）
  const COMMANDS = {
    hint:    { defaultKey: "f", desc: "link hints", run: (_a, o) => startHints(false, o) },
    hintNew: { defaultKey: "F", desc: "link hints (open in new window)", run: (_a, o) => startHints(true, o) },
    back:    { defaultKey: "a", desc: "history back", run: () => history.go(-1) },
    forward: { defaultKey: "d", desc: "history forward", run: () => history.go(1) },
    insert:  { defaultKey: "i", desc: "insert mode (focus input)", run: () => startInsert() },
    scrollDown:  { defaultKey: "j", desc: "scroll down", run: (_a, o) => scrollLines(1, o) },
    scrollUp:    { defaultKey: "k", desc: "scroll up", run: (_a, o) => scrollLines(-1, o) },
    scrollLeft:  { defaultKey: "h", desc: "scroll left", run: (_a, o) => scrollCols(-1, o) },
    scrollRight: { defaultKey: "l", desc: "scroll right", run: (_a, o) => scrollCols(1, o) },
    pageDown:    { defaultKey: "w", desc: "page down", run: (_a, o) => scrollPage(1, o) },
    pageUp:      { defaultKey: "s", desc: "page up", run: (_a, o) => scrollPage(-1, o) },
    scrollTop:   { defaultKey: "g", desc: "scroll to top / <N>% of page", run: (_a, o) => scrollToPctOrTop(o) },
    scrollBottom:{ defaultKey: "G", desc: "scroll to bottom", run: () => scrollTo({ top: document.documentElement.scrollHeight }) },
    refresh: { defaultKey: "r", desc: "reload page", run: () => location.reload() },
    tag:     { defaultKey: "t", desc: "toggle tag on this page", run: () => showTagPrompt() },
    open:    { defaultKey: "o", desc: "open url / filter pages (console)",
               run: (arg) => role === "console" ? openOnConsole(arg) : openOnPage(arg) },
    pages:   { defaultKey: "P", desc: "switch page (mudrad)", run: () => showPages() },
    set:     { defaultKey: null, desc: ":set <key> <value>（如 set scrollStepLines 5）", run: (arg) => runSet(arg) },
  };

  // ---- :set <key> <value>：写 chrome.storage.local（扩展本地配置，非 mudrad 全局）----
  // keybindings 例外：set keybindings.j=scrollDown 形式（点号定位子键）。
  async function runSet(arg) {
    // 三种形态：bare=显示配置；"key value" 或 "key=value"（keybindings.u=pageUp 常为单 token）
    arg = (arg || "").trim();
    if (!arg) {
      const cfg = await MudraConfig.all();
      const lines = Object.entries(cfg)
        .filter(([k]) => MudraConfig.defaults[k] !== undefined)
        .map(([k, v]) => `${k} = ${JSON.stringify(v)}`);
      return flashBar(lines.join("  ") || "no config");
    }
    const m = arg.match(/^(\S+?)(?:\s+|=)(.+)$/);
    if (!m) return flashBar(`usage: set <key> <value> | set keybindings.<key>=<cmd>`);
    const [, key, raw] = m;
    if (key.startsWith("keybindings.")) {
      const k = key.slice("keybindings.".length);
      const cfg = await MudraConfig.all();
      const kb = { ...(cfg.keybindings || {}) };
      if (raw === "-" || raw === "off") delete kb[k]; else kb[k] = raw;
      await MudraConfig.set({ keybindings: kb });
      return flashBar(`${k} -> ${kb[k] || "unbound"}`);
    }
    if (!(key in MudraConfig.defaults)) return flashBar(`unknown key: ${key}`);
    let val;
    try { val = JSON.parse(raw); } catch { val = raw; } // 数字/布尔/字符串自动判型
    await MudraConfig.set({ [key]: val });
    return flashBar(`${key} = ${JSON.stringify(val)}`);
  }

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

  // ---- 滚动实现 ----
  async function scrollLines(dir, o) {
    const cfg = await MudraConfig.all();
    const line = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    scrollBy({ top: dir * (o.count || cfg.scrollStepLines) * line });
    await refreshBar();
  }
  async function scrollCols(dir, o) {
    const cfg = await MudraConfig.all();
    const line = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    scrollBy({ left: dir * (o.count || cfg.scrollStepLines) * line });
    await refreshBar();
  }
  async function scrollPage(dir, o) {
    const cfg = await MudraConfig.all();
    const overlap = (o.count != null ? o.count : cfg.pageOverlapLines) *
      (parseFloat(getComputedStyle(document.documentElement).fontSize) || 16);
    scrollBy({ top: dir * (innerHeight - overlap) });
    await refreshBar();
  }
  // 数字前缀 + g：有前缀跳百分比（30g → 30%），无前缀回顶部
  async function scrollToPctOrTop(o) {
    if (o.count != null) {
      const h = document.documentElement.scrollHeight - innerHeight;
      scrollTo({ top: Math.min(h, h * o.count / 100) });
    } else scrollTo({ top: 0 });
    await refreshBar();
  }

  // ---- command mode（: 进入；bar 变输入栏，候选项向上展开过滤）----
  function filterCmd(q) {
    if (!cmdApi) return;
    q = (q || "").replace(/^:/, "");
    const [name, ...rest] = q.split(/\s+/);
    let items;
    if (!q) {
      items = Object.entries(COMMANDS).map(([n, c]) => ({ label: `:${n}  ${c.desc}`, value: n }));
    } else {
      items = Object.entries(COMMANDS)
        .filter(([n]) => n.startsWith(name))
        .map(([n, c]) => ({ label: `:${n}  ${c.desc}`, value: n }));
    }
    cmdApi.setItems(items);
  }

  async function pickCmd(cand, raw, api) {
    cmdApi = null;
    await setMode("normal");
    if (!cand) return; // Esc / 空
    const q = (raw || "").replace(/^:/, "").trim();
    const [name, ...rest] = q.split(/\s+/);
    const c = COMMANDS[cand.value] || COMMANDS[name];
    if (!c) return flashBar(`no such command: ${name}`);
    if (!c.run.length && rest.length) return flashBar(`:${cand.value} takes no argument`);
    await c.run(rest.join(" "), {});
    // 不统一 refreshBar：会盖掉命令自己的 flashBar 消息（如 :set 的配置回显）。
    // 需要刷新的命令（scroll 等）内部已自行调用。
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
        cmdApi = null; await setMode("normal");
        if (!cand) return;
        const res = await send({ type: "focus_page", page_id: cand.value });
        await flashBar(res.ok === false ? (res.err || "focus failed") : "switched");
      }
    );
    // 初始列表
    cmdApi.setItems(pages.map((p) => ({ label: `[${p.ctx}] ${p.title.slice(0, 60)}`, value: p.id })));
  }

  // ---- hints（链接/可点元素）----
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

  // vimium 式 hint：逐字符前缀过滤，唯一匹配立即激活
  async function startHints(newWindow) {
    const c = await MudraConfig.all();
    const els = clickable();
    if (!els.length) return flashBar("no clickable elements");
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
      return { el, seq: seqs[i], span: d };
    });
    document.documentElement.appendChild(overlay);
    hintSession = { overlay, nodes, typed: "", newWindow: !!newWindow };
    setMode("hint");
  }

  function hintFilter(key) {
    if (!hintSession) return;
    const { nodes } = hintSession;
    if (key === "Backspace") hintSession.typed = hintSession.typed.slice(0, -1);
    else if (/^[a-z]$/.test(key)) hintSession.typed += key;
    else return;
    const typed = hintSession.typed;
    const matches = nodes.filter((n) => n.seq.startsWith(typed));
    if (typed && matches.length === 1 && matches[0].seq === typed) {
      return activate(matches[0].el, hintSession.newWindow); // 唯一完整匹配 → 激活
    }
    if (matches.length === 0) return hideHints(); // 全部滤光 → 退出
    for (const n of nodes) {
      const on = n.seq.startsWith(typed);
      n.span.style.display = on ? "" : "none";
      // 高亮已敲入部分
      n.span.textContent = on ? n.seq : n.seq;
    }
  }

  function activate(el, newWindow) {
    hideHints();
    const a = el.closest("a");
    const href = a?.href;
    if (!newWindow && href && a.hash && a.pathname === location.pathname && a.search === location.search) {
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

  // ---- insert 模式：自动聚焦输入框；多个输入框用 hint 选择 ----
  async function startInsert() {
    const inputs = [...document.querySelectorAll("input:not([type=hidden]):not([type=submit]):not([type=button]), textarea, [contenteditable=true]")]
      .filter((el) => {
        const r = el.getBoundingClientRect();
        const st = getComputedStyle(el);
        return r.width > 0 && r.height > 0 && st.visibility !== "hidden" && st.display !== "none";
      });
    if (inputs.length === 0) return flashBar("no input on page");
    if (inputs.length === 1) return enterInsert(inputs[0]);
    // 多个输入框 → hint 选择
    const c = await MudraConfig.all();
    const seqs = hintStrings(inputs.length, c.hintChars.split(""));
    const overlay = document.createElement("div");
    overlay.id = "mudra-hints";
    overlay.style.cssText = "position:absolute;top:0;left:0;z-index:2147483646;pointer-events:none";
    const nodes = inputs.map((el, i) => {
      const r = el.getBoundingClientRect();
      const d = document.createElement("span");
      d.textContent = seqs[i];
      d.style.cssText = [
        "position:fixed", `left:${Math.max(0, r.left)}px`, `top:${Math.max(0, r.top)}px`,
        `font:${c.hintFontSize}px monospace`, `color:${c.hintFg}`, `background:#7ec8ff`,
        "padding:0 2px", "border-radius:2px", "white-space:pre",
      ].join(";");
      overlay.appendChild(d);
      return { el, seq: seqs[i], span: d };
    });
    document.documentElement.appendChild(overlay);
    inputHintSession = { overlay, nodes, typed: "" };
    setMode("hint");
  }

  function inputHintFilter(key) {
    if (!inputHintSession) return;
    if (key === "Backspace") inputHintSession.typed = inputHintSession.typed.slice(0, -1);
    else if (/^[a-z]$/.test(key)) inputHintSession.typed += key;
    else return;
    const typed = inputHintSession.typed;
    const matches = inputHintSession.nodes.filter((n) => n.seq.startsWith(typed));
    if (typed && matches.length === 1 && matches[0].seq === typed) {
      const el = matches[0].el;
      document.getElementById("mudra-hints")?.remove();
      inputHintSession = null;
      return enterInsert(el);
    }
    if (matches.length === 0) {
      document.getElementById("mudra-hints")?.remove();
      inputHintSession = null;
      return setMode("normal");
    }
    for (const n of inputHintSession.nodes)
      n.span.style.display = n.seq.startsWith(typed) ? "" : "none";
  }

  function enterInsert(el) {
    el.focus();
    setMode("insert");
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

  // ---- key handling：单一状态机，每模式一个分支 ----
  const isEditable = (t) =>
    t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));

  document.addEventListener("keydown", async (e) => {
    // command 模式：输入框自己处理（lib.js 内 stopPropagation），这里只兜底
    if (mode === "command") return;

    // insert 模式：Esc 返回 normal（blur 由 setMode 统一处理），其余全放行
    if (mode === "insert") {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); setMode("normal"); }
      return;
    }

    // 非 insert 模式下焦点落在可编辑元素：只拦 Esc（可能从 insert 残留），其余放行
    if (isEditable(e.target)) {
      if (e.key === "Escape") { e.target.blur(); setMode("normal"); }
      return;
    }

    // hint 选择态（链接 hint 或 输入框 hint）共用一套按键
    if (mode === "hint") {
      e.preventDefault();
      if (e.key === "Escape") {
        document.getElementById("mudra-hints")?.remove();
        hintSession = null; inputHintSession = null;
        hideTagPrompt();
        return setMode("normal");
      }
      if (hintSession) return hintFilter(e.key);
      if (inputHintSession) return inputHintFilter(e.key);
      if (tagPrompt && /^[a-zA-Z]$/.test(e.key)) return pickTag(e.key);
      return;
    }

    // ---- normal 模式 ----
    // 数字前缀累积（30g 百分比跳转）
    if (/^[0-9]$/.test(e.key)) {
      e.preventDefault();
      numPrefix = (numPrefix + e.key).replace(/^0+(?=.)/, "");
      await refreshBar(); // 状态栏显示已敲数字
      return;
    }

    // : 进入命令模式
    if (!e.ctrlKey && !e.metaKey && !e.altKey && e.key === ":") {
      e.preventDefault();
      return enterCommand();
    }

    // 键位 → 命令（可配置）；数字前缀作为 count 传给支持它的命令
    const plain = !e.ctrlKey && !e.metaKey && !e.altKey;
    if (plain) {
      const count = numPrefix ? parseInt(numPrefix, 10) : null;
      numPrefix = "";
      const name = await keyToCommand(e.key);
      if (name) {
        e.preventDefault();
        COMMANDS[name].run("", { count });
      } else {
        refreshBar(); // 无效键清掉数字前缀显示
      }
    }
  }, true);

  // ---- status bar ----
  const boot = async () => {
    await MudraBar.mount();
    await refreshBar();
    if (role !== "console") await syncStatus(); // console 页不属于任何 ctx，跳过反查
    addEventListener("scroll", refreshBar, { passive: true });
    // SPA 导航时 URL 变化 → 重拉状态
    let lastUrl = pageUrl();
    setInterval(async () => {
      if (pageUrl() !== lastUrl) { lastUrl = pageUrl(); if (role !== "console") await syncStatus(); }
    }, 1000);
  };
  if (document.readyState === "loading")
    document.addEventListener("DOMContentLoaded", boot);
  else boot();
})();
