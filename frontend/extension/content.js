// mudra-keys content script: mode machine normal/hint/insert/command (vimium-style),
// letter hints, scroll commands (including numeric-prefix percent jumps), insert-mode focus
// management, status bar, command mode (:), and configurable keybindings.
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
  let role = "page";   // page | console (decided by mudrad, the frontend does not guess)
  let pageTags = [];   // tags on the current page (mudrad is authoritative; local is display cache only)
  let hintSession = null;   // {overlay, nodes} link hints
  let inputHintSession = null; // {overlay, nodes} input picker in insert mode
  let cmdApi = null;
  let numPrefix = "";       // numeric prefix (30g percent jump)

  const setMode = async (m) => {
    mode = m;
    // Mode switches must sync focus: blur when leaving insert so a lingering input focus does not swallow Esc
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
      count: numPrefix || undefined, // numeric prefix being typed (30g)
    });
  };

  // Status data is fetched from mudrad (on navigation / timer); failures are silent (the bar still works without mudrad)
  const syncStatus = async () => {
    const r = await send({ type: "status", url: pageUrl() });
    if (r && r.ok !== false) {
      ctx = r.ctx || "";
      role = r.role || "page";
      pageTags = r.tags || [];
      await refreshBar(); // render only once status arrives (boot first renders a ctx-less version)
    } else {
      flashBar("status: " + ((r && r.err) || "no response"));
    }
  };

  // ---- open: branches by role (qutebrowser-style) ----
  // page role: type a URL -> /open
  // console role: typing also filters existing page candidates; Enter on a candidate -> focus_page, otherwise -> /open
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
        // a selected candidate -> jump to the page (via Tab completion or arrow selection)
        const res = await send({ type: "focus_page", page_id: cand.value });
        return flashBar(res.ok === false ? (res.err || "focus failed") : "switched");
      }
      // no matching candidate -> open as a URL
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
  // run(arg, opts): opts = {count} (numeric prefix), {newTab} (F variant)
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
    set:     { defaultKey: null, desc: ":set <key> <value> (e.g. set scrollStepLines 5)", run: (arg) => runSet(arg) },
  };

  // ---- :set <key> <value>: writes chrome.storage.local (extension-local config, not mudrad global) ----
  // keybindings is special-cased: set keybindings.j=scrollDown form (dot notation targets a subkey).
  async function runSet(arg) {
    // Three forms: bare = show config; "key value" or "key=value" (keybindings.u=pageUp is often a single token)
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
    try { val = JSON.parse(raw); } catch { val = raw; } // auto-type numbers/booleans/strings
    await MudraConfig.set({ [key]: val });
    return flashBar(`${key} = ${JSON.stringify(val)}`);
  }

  // Keybinding resolution: config JSON keybindings[key]=commandName overrides defaultKey
  async function keyToCommand(key) {
    const cfg = await MudraConfig.all();
    const map = cfg.keybindings || {};
    for (const [name, def] of Object.entries(COMMANDS)) {
      const bound = Object.keys(map).find((k) => map[k] === name);
      if ((bound || def.defaultKey) === key) return name;
    }
    return null;
  }

  // ---- scrolling implementation ----
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
  // Numeric prefix + g: with a prefix jump to a percentage (30g -> 30%), without it back to top
  async function scrollToPctOrTop(o) {
    if (o.count != null) {
      const h = document.documentElement.scrollHeight - innerHeight;
      scrollTo({ top: Math.min(h, h * o.count / 100) });
    } else scrollTo({ top: 0 });
    await refreshBar();
  }

  // ---- command mode (enter with :; bar becomes an input, candidates expand upward and filter) ----
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
    if (!cand) return; // Esc / empty
    const q = (raw || "").replace(/^:/, "").trim();
    const [name, ...rest] = q.split(/\s+/);
    const c = COMMANDS[cand.value] || COMMANDS[name];
    if (!c) return flashBar(`no such command: ${name}`);
    if (!c.run.length && rest.length) return flashBar(`:${cand.value} takes no argument`);
    await c.run(rest.join(" "), {});
    // No blanket refreshBar here: it would clobber the command's own flashBar message (e.g. the :set config echo).
    // Commands that need a refresh (scroll etc.) call it internally already.
  }

  const enterCommand = async () => {
    setMode("command");
    cmdApi = await MudraBar.openCommand(filterCmd, pickCmd);
    filterCmd(":");
  };

  // ---- pages command: mudrad page list -> candidate filtering -> focus_page ----
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
    // initial list
    cmdApi.setItems(pages.map((p) => ({ label: `[${p.ctx}] ${p.title.slice(0, 60)}`, value: p.id })));
  }

  // ---- hints (links / clickable elements) ----
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

  // Letter sequences: hintChars pool + short codes first (initials bias toward the pool head, the one-hand zone)
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

  // vimium-style hints: char-by-char prefix filtering; a unique match activates immediately
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
      return activate(matches[0].el, hintSession.newWindow); // unique full match -> activate
    }
    if (matches.length === 0) return hideHints(); // all filtered out -> exit
    for (const n of nodes) {
      const on = n.seq.startsWith(typed);
      n.span.style.display = on ? "" : "none";
      // highlight the already-typed portion
      n.span.textContent = on ? n.seq : n.seq;
    }
  }

  function activate(el, newWindow) {
    hideHints();
    const a = el.closest("a");
    const href = a?.href;
    if (!newWindow && href && a.hash && a.pathname === location.pathname && a.search === location.search) {
      location.hash = a.hash; // same-page anchor jump in place, no new window
    } else if (href && /^https?:/.test(href)) {
      send({ type: "open", url: href }); // mudrad pulls up a new --app window
    } else {
      el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    }
  }

  function hideHints() {
    document.getElementById("mudra-hints")?.remove();
    hintSession = null;
    setMode("normal");
  }

  // ---- insert mode: auto-focus the input; with multiple inputs, pick via hints ----
  async function startInsert() {
    const inputs = [...document.querySelectorAll("input:not([type=hidden]):not([type=submit]):not([type=button]), textarea, [contenteditable=true]")]
      .filter((el) => {
        const r = el.getBoundingClientRect();
        const st = getComputedStyle(el);
        return r.width > 0 && r.height > 0 && st.visibility !== "hidden" && st.display !== "none";
      });
    if (inputs.length === 0) return flashBar("no input on page");
    if (inputs.length === 1) return enterInsert(inputs[0]);
    // multiple inputs -> pick via hints
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

  // ---- tag prompt bar (summon with t, press a letter to tag, Esc cancels) ----
  let tagPrompt = null;
  async function showTagPrompt() {
    const roots = await send({ type: "tags" });
    if (!roots.tags || !roots.tags.length) { await flashBar("no tags"); return; }
    // Flatten top-level roots + their first-level children: roots uppercase, children lowercase (good enough; deep trees iterate later)
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

  // ---- key handling: a single state machine with one branch per mode ----
  const isEditable = (t) =>
    t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));

  document.addEventListener("keydown", async (e) => {
    // command mode: the input handles itself (stopPropagation inside lib.js); this is only a fallback
    if (mode === "command") return;

    // insert mode: Esc returns to normal (blur handled uniformly in setMode); everything else passes through
    if (mode === "insert") {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); setMode("normal"); }
      return;
    }

    // Focus on an editable element outside insert mode: intercept only Esc (possibly leftover from insert), let the rest through
    if (isEditable(e.target)) {
      if (e.key === "Escape") { e.target.blur(); setMode("normal"); }
      return;
    }

    // Hint-selection state (link hints or input hints) shares one key set
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

    // ---- normal mode ----
    // Numeric prefix accumulation (30g percent jump)
    if (/^[0-9]$/.test(e.key)) {
      e.preventDefault();
      numPrefix = (numPrefix + e.key).replace(/^0+(?=.)/, "");
      await refreshBar(); // show the typed digits in the status bar
      return;
    }

    // : enters command mode
    if (!e.ctrlKey && !e.metaKey && !e.altKey && e.key === ":") {
      e.preventDefault();
      return enterCommand();
    }

    // key -> command (configurable); the numeric prefix is passed as count to commands that support it
    const plain = !e.ctrlKey && !e.metaKey && !e.altKey;
    if (plain) {
      const count = numPrefix ? parseInt(numPrefix, 10) : null;
      numPrefix = "";
      const name = await keyToCommand(e.key);
      if (name) {
        e.preventDefault();
        COMMANDS[name].run("", { count });
      } else {
        refreshBar(); // an invalid key clears the numeric-prefix display
      }
    }
  }, true);

  // ---- status bar ----
  const boot = async () => {
    await MudraBar.mount();
    await refreshBar();
    if (role !== "console") await syncStatus(); // the console page belongs to no ctx; skip the reverse lookup
    addEventListener("scroll", refreshBar, { passive: true });
    // On SPA navigation the URL changes -> refetch status
    let lastUrl = pageUrl();
    setInterval(async () => {
      if (pageUrl() !== lastUrl) { lastUrl = pageUrl(); if (role !== "console") await syncStatus(); }
    }, 1000);
  };
  if (document.readyState === "loading")
    document.addEventListener("DOMContentLoaded", boot);
  else boot();
})();
