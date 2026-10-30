// mudra-keys content script: mode machine normal/hint/insert/command (vimium-style),
// letter hints, scroll commands (including numeric-prefix percent jumps), insert-mode focus
// management, status bar, command mode (:), and configurable keybindings.
// Link opening / tag / page ops go through sw.js -> mudrad (tabId routing, ctx resolved there).
(() => {
  if (window.__mudraKeys) return;
  window.__mudraKeys = true;

  // Before anything else: the page is still arriving at document_start, and the
  // load bar has to be there from the first frame to mean anything.
  MudraProgress.mount();

  // The service worker is the only bridge to mudrad, and a message it never
  // answers resolves to `undefined` — which used to read as success (`r.ok`
  // undefined is not false) and printed "opened in a new window" for a window
  // that never existed. No answer is a failure, and it has to say so.
  // ---- transport: one port to the extension worker ----
  // A content script cannot reach 127.0.0.1 itself on an https page: the page's
  // mixed-content / private-network rules block both fetch and a socket
  // (measured on github.com: fetch → TypeError, ws → error in 2ms), and MV3
  // gives content scripts no cross-origin privilege. The extension's own
  // context is the only exempt one, so every verb rides this port.
  //
  // The port is also the keepalive. A service worker is terminated after ~30s
  // idle and should be woken by the next message; the failure that kills the
  // bar is different — a worker whose script cache was evicted under it never
  // comes back, and every message then answers "Receiving end does not exist"
  // for the rest of the page's life. Two defenses, one on each side:
  //   - this side pings every 20s, so a worker with a page open never idles out;
  //   - `mudrad` no longer deletes a profile's extension caches while a browser
  //     holds that profile (that deletion is exactly how the script vanishes).
  const PORT_NAME = "mudra";
  const HEARTBEAT_MS = 20000;
  const CALL_TIMEOUT_MS = 2500;
  let workerPort = null;
  let portSeq = 0;
  const portWaiting = new Map(); // id -> resolve(reply)

  function portOpen() {
    if (workerPort) return workerPort;
    let port;
    try {
      port = chrome.runtime.connect({ name: PORT_NAME });
    } catch (e) {
      return null;
    }
    port.onMessage.addListener((r) => {
      const done = portWaiting.get(r && r.id);
      if (!done) return;
      portWaiting.delete(r.id);
      done(r);
    });
    port.onDisconnect.addListener(() => {
      void chrome.runtime.lastError; // the disconnect itself is the message
      if (workerPort === port) workerPort = null;
      // fail what is in flight now instead of letting each call sit out its
      // timeout: the worker closed the link, so nothing is coming
      for (const [id, done] of portWaiting) done({ ok: false, err: "extension worker closed the link" });
      portWaiting.clear();
    });
    workerPort = port;
    return port;
  }

  if (typeof setInterval === "function") {
    setInterval(() => {
      if (!workerPort) return;
      try {
        workerPort.postMessage({ id: 0, type: "ping" });
      } catch {
        workerPort = null; // a dead port must not be pinged forever
      }
    }, HEARTBEAT_MS);
  }

  // One call, one reply, paired by id. No answer is a failure and says so.
  function send(msg) {
    const port = portOpen();
    if (!port) return Promise.resolve({ ok: false, err: `no link to the extension worker (${msg.type})` });
    const id = ++portSeq;
    return new Promise((res) => {
      const timer = setTimeout(() => {
        portWaiting.delete(id);
        res({ ok: false, err: `extension worker timeout (${msg.type})` });
      }, CALL_TIMEOUT_MS);
      portWaiting.set(id, (r) => { clearTimeout(timer); res(r); });
      try {
        port.postMessage({ ...msg, id });
      } catch (e) {
        portWaiting.delete(id);
        clearTimeout(timer);
        workerPort = null;
        res({ ok: false, err: String(e) });
      }
    });
  }

  // ---- state ----
  let mode = "normal";      // state machine: normal / hint / insert / command
  let modeLabel = "normal"; // what the bar prints (hint vs hintnew)
  let ctx = "";
  let role = "page";   // page | console (decided by mudrad, the frontend does not guess)
  let pageTags = [];   // tags on the current page (mudrad is authoritative; local is display cache only)
  let pageCapsules;  // server-rendered capsule HTML (tag-forest crate); undefined = old mudrad, JS fallback stays
  let hintSession = null;   // {overlay, nodes} link hints
  let inputHintSession = null; // {overlay, nodes} input picker in insert mode
  let cmdApi = null;
  let numPrefix = "";       // numeric prefix (30g percent jump)

  const setMode = async (m, label) => {
    mode = m;
    modeLabel = label || m;
    // Mode switches must sync focus: blur when leaving insert so a lingering input focus does not swallow Esc
    if (m !== "insert" && document.activeElement && isEditable(document.activeElement))
      document.activeElement.blur();
    // An overlay belongs to the mode it was opened in: any switch closes the
    // waiting sequence menu and the keymap with it.
    if (m !== "normal") closeOverlay();
    await refreshBar();
  };

  // ---- shadow-DOM traversal ---------------------------------------------
  // Chrome's own pages (settings, history, downloads, flags) and a growing
  // share of the web keep their content inside shadow roots. A plain
  // document.querySelectorAll sees only the light DOM and reports "no links"
  // on a page full of them, so element queries and the scroll target both go
  // through the helpers below.
  const deepQueryAll = (selector, root = document) => {
    const out = [...root.querySelectorAll(selector)];
    for (const el of root.querySelectorAll("*"))
      if (el.shadowRoot) out.push(...deepQueryAll(selector, el.shadowRoot));
    return out;
  };

  // The deepest painted node under a point: document.elementFromPoint stops at
  // the shadow host, so descend through each open root the same way.
  const deepHit = (x, y) => {
    let el = document.elementFromPoint(x, y);
    while (el && el.shadowRoot) {
      const inner = el.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === el) break;
      el = inner;
    }
    return el;
  };

  const scrollsY = (el) => {
    if (!el || !el.scrollHeight) return false;
    if (el.scrollHeight - el.clientHeight <= 4) return false;
    return /(auto|scroll|overlay)/.test(getComputedStyle(el).overflowY);
  };

  // The scroller is not always the window: a WebUI page scrolls a container
  // inside its shadow root (settings' #container), where window.scrollBy()
  // moves nothing. Fast path first — the document scrolls on nearly every
  // site; otherwise hit-test the viewport centre and climb the flat tree
  // (parentElement, then the shadow host) to the first container that really
  // scrolls. Resolved at most every 500ms: the chip re-renders on every scroll
  // event, and a hit test plus getComputedStyle per event is the wrong price.
  let _scrollTarget = null;
  let _scrollTargetAt = 0;
  const scrollTarget = () => {
    const now = Date.now();
    if (_scrollTarget && _scrollTarget.isConnected && now - _scrollTargetAt < 500)
      return _scrollTarget;
    const doc = document.scrollingElement || document.documentElement;
    // The document element reports overflow:visible even while the viewport
    // scrolls (overflow propagates), so it is judged by its own box instead.
    let t = document.documentElement.scrollHeight - innerHeight > 4 ? doc : null;
    if (!t) {
      let el = deepHit(Math.round(innerWidth / 2), Math.round(innerHeight / 2));
      while (el) {
        if (scrollsY(el)) { t = el; break; }
        el = el.parentElement || (el.getRootNode && el.getRootNode().host) || null;
      }
    }
    _scrollTarget = t || doc;
    _scrollTargetAt = now;
    return _scrollTarget;
  };

  const scrollPct = () => {
    const t = scrollTarget();
    const h = t.scrollHeight - (t.clientHeight || innerHeight);
    return h > 0 ? `${Math.round((t.scrollTop / h) * 100)}%` : "all";
  };
  const pageUrl = () => location.origin + location.pathname + location.search;

  // The console page is the UI: it never navigates itself away. `role` comes
  // from mudrad's /ctx_status, but a failed status round trip must not turn the
  // panel into an ordinary browsing page — the panel's own HTML declares it
  // (`<meta name="mudra-console">`) and that declaration is enough.
  const isConsolePage = () =>
    role === "console" || !!document.querySelector('meta[name="mudra-console"]');

  const refreshBar = async () => {
    await MudraBar.render({
      ctx, mode, modeLabel, tags: pageTags, capsules: pageCapsules,
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
      if (r.capsules != null) pageCapsules = r.capsules; // old mudrad: keep JS fallback active
      await refreshBar(); // render only once status arrives (boot first renders a ctx-less version)
    } else {
      flashBar("status: " + ((r && r.err) || "no response"));
    }
  };

  // ---- the two opening verbs ----
  // open = navigate THIS page in place (Enter in the address picker, hints `w`);
  // new  = a separate page/window (hints `W`). The daemon resolves the address
  // for both (`/visit`, `/open` share spawn::normalize_url), so "bing" means
  // bing.com whichever verb carries it.
  async function openCurrent(url) {
    const text = (url || "").trim();
    if (!text) return flashBar("usage: :open <url>");
    // The console page is the UI: it never navigates itself away.
    if (isConsolePage()) {
      const r = await send({ type: "new", url: text, from: location.href, ctx });
      return flashBar(r.ok === false ? r.err || "open failed" : "open ok");
    }
    // /visit resolves the input and writes the History row — an in-place
    // navigation only refreshes the existing page row, so the watcher never
    // sees a new visit.
    const r = await send({ type: "visit", url: text });
    const target = (r && r.url) || (/^[a-z]+:\/\//i.test(text) ? text : `https://${text}`);
    location.href = target;
  }

  // `new` never touches this page: mudrad spawns another --app window in the
  // sender's ctx. The console role reaches this path through openCurrent.
  async function openNew(url) {
    const text = (url || "").trim();
    if (!text) return flashBar("usage: :new <url>");
    const r = await send({ type: "new", url: text, from: location.href, ctx });
    return flashBar(r.ok === false ? r.err || "open failed" : "opened in a new window");
  }

  // ---- :q — close this page ----
  // The service worker closes the sender's own tab, which is exact: no URL
  // matching (mudrad's /close_page takes a substring), and it works the same on
  // the console, which is just another tab. mudrad's watcher sees
  // Target.targetDestroyed and marks the page row closed, so the bookkeeping
  // needs no cooperation from here.
  async function closeSelf() {
    const r = await send({ type: "close" });
    if (r.ok === false) return flashBar(r.err || "close failed");
    // Nothing to confirm: the page (and this bar) is on its way out.
  }
  // ---- one surface for both verbs: the palette IS the address bar ----
  // `o`/`O` only type `open `/`new ` into the command line, so there is no
  // second picker with its own prompt to keep in sync. filterCmd sees the URL
  // verb and lists live pages (console, `open` only) + history rows for the
  // argument; Enter takes the highlighted row, or the typed text when nothing
  // is highlighted. Tab moves the highlight (lib.js) — the list never vanishes
  // under a keystroke, and the line stays one thing you can backspace through.
  const URL_VERBS = new Set(["open", "new"]);
  let livePages = [];   // console role: jump targets inside this instance
  let historySeq = 0;   // filters await the SW; only the newest answer may land
  let historyTimer = 0; // the pending keystroke's query (see HISTORY_DEBOUNCE_MS)
  // One round trip per typing pause, not per keystroke. The daemon answers in
  // tens of ms now that its ranking is resident, but a keypress every 30ms
  // would still queue one query per key, each holding the store for its own
  // pass. 120ms is below the point where the list reads as late, and it
  // collapses a whole word into one query.
  const HISTORY_DEBOUNCE_MS = 120;
  // Candidates to fetch: display depth x 3. The list scrolls, and the row being
  // scanned for often sits a few entries below the top — depth costs one bigger
  // response, not another round trip.
  const HISTORY_LIMIT = 30;

  async function filterAddress(verb, arg) {
    const mine = ++historySeq;
    clearTimeout(historyTimer);
    const s = (arg || "").toLowerCase();
    const pageC = verb === "open" && role === "console"
      ? livePages
          .filter((p) => !s || (p.title + " " + p.url + " " + p.ctx).toLowerCase().includes(s))
          .map((p) => ({ label: `[${p.ctx}] ${p.title.slice(0, 60)}`, value: p.id, kind: "page" }))
      : [];
    // live pages are local state: they paint on the keystroke, no round trip
    if (pageC.length && cmdApi) cmdApi.setItems(pageC);
    const fire = async () => {
      const hr = await send({ type: "history", query: arg || "", limit: HISTORY_LIMIT });
      if (mine !== historySeq || !cmdApi) return; // a newer keystroke already asked
      if (hr.ok === false) {
        // An empty list would read as "nothing matches what you typed"; the truth
        // is the bridge is down (dead service worker, mudrad restart). Say which.
        return cmdApi.setItems([{ label: `! ${hr.err || "history failed"}`, value: "", kind: "error" }]);
      }
      const hist = (hr.candidates || [])
        .filter((h) => !livePages.some((p) => p.url === h.url)) // a live page wins over its history row
        .map((h) => ({ label: h.url, desc: h.title, value: h.url, kind: "url" }));
      cmdApi.setItems([...pageC, ...hist]);
    };
    // the bar's first state (an empty line) has no next keystroke coming: ask now
    if (!(arg || "").trim()) await fire();
    else historyTimer = setTimeout(fire, HISTORY_DEBOUNCE_MS);
  }

  // ---- clipboard -----------------------------------------------------------------
  // navigator.clipboard needs a focused document and a user gesture; a keydown
  // in the page is both. The textarea fallback covers documents where the async
  // API is refused (some chrome:// and file:// pages) — copying is one of the
  // few things that must not fail silently, because the user cannot tell a
  // successful copy from a broken one.
  async function copyText(text) {
    // Real key presses carry user activation, which is what the async API wants;
    // programmatic callers (tests, page scripts) have none, so the fallback is
    // not dead code.
    if (navigator.clipboard && navigator.clipboard.writeText) {
      try {
        await navigator.clipboard.writeText(text);
        return true;
      } catch { /* not focused / not activated / refused: try the legacy path */ }
    }
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.setAttribute("aria-hidden", "true");
    ta.style.cssText = "position:fixed;top:0;left:0;width:1px;height:1px;opacity:0;pointer-events:none";
    document.documentElement.appendChild(ta);
    ta.focus();
    ta.select();
    let ok = false;
    try { ok = document.execCommand("copy"); } catch { ok = false; }
    ta.remove();
    return ok;
  }

  async function yank(what) {
    const text = what === "title" ? document.title || "" : location.href;
    const ok = await copyText(text);
    const short = text.length > 60 ? text.slice(0, 57) + "…" : text;
    await flashBar(ok ? `copied ${what}: ${short}` : `copy failed: ${what}`);
  }

  // ---- key sequences ---------------------------------------------------------
  // A key that starts a sequence does nothing on its own: it opens a menu above
  // the status bar and waits seqTimeout ms for the second key. The menu is the
  // point — a sequence nobody can discover is a secret, not a binding.
  const SEQUENCES = {
    y: {
      desc: "yank",
      items: [
        { key: "y", desc: "copy page url", run: () => yank("url") },
        { key: "t", desc: "copy page title", run: () => yank("title") },
      ],
    },
  };

  // ---- commands ----
  // run(arg, opts): opts = {count} (numeric prefix), {newTab} (F variant)
  //
  // The layout keeps the hands on the home row: the vertical pair sits one key
  // right of WASD (w→e, s→d), so the fingers that type also scroll, and the
  // freed `s`/`f` take the browser's left/right — history back and forward.
  // `aliases` are a second key for the same verb (o/O are the mnemonic twins of
  // a/A); an explicit `keybindings` entry always beats both.
  const COMMANDS = {
    hint:    { defaultKey: "w", desc: "link hints — open (this page)", run: (_a, o) => startHints(false, o) },
    hintNew: { defaultKey: "W", desc: "link hints — new (another page)", run: (_a, o) => startHints(true, o) },
    halfPageUp:  { defaultKey: "e", desc: "scroll up half a page", run: (_a, o) => scrollHalfPage(-1, o) },
    halfPageDown: { defaultKey: "d", desc: "scroll down half a page", run: (_a, o) => scrollHalfPage(1, o) },
    back:    { defaultKey: "s", desc: "history back", run: () => history.go(-1) },
    forward: { defaultKey: "f", desc: "history forward", run: () => history.go(1) },
    insert:  { defaultKey: "i", desc: "insert mode (focus input)", run: () => startInsert() },
    scrollDown:  { defaultKey: "j", desc: "scroll down one line", run: (_a, o) => scrollLines(1, o) },
    scrollUp:    { defaultKey: "k", desc: "scroll up one line", run: (_a, o) => scrollLines(-1, o) },
    scrollLeft:  { defaultKey: "h", desc: "scroll left", run: (_a, o) => scrollCols(-1, o) },
    scrollRight: { defaultKey: "l", desc: "scroll right", run: (_a, o) => scrollCols(1, o) },
    // A whole page lost its keys to the half page; still bindable
    pageDown:    { defaultKey: null, desc: "page down (bindable)", run: (_a, o) => scrollPage(1, o) },
    pageUp:      { defaultKey: null, desc: "page up (bindable)", run: (_a, o) => scrollPage(-1, o) },
    scrollTop:   { defaultKey: "g", desc: "scroll to top / <N>% of page", run: (_a, o) => scrollToPctOrTop(o) },
    scrollBottom:{ defaultKey: "G", desc: "scroll to bottom", run: () => scrollToBottom() },
    refresh: { defaultKey: "r", desc: "reload page", run: () => location.reload() },
    tag:     { defaultKey: "t", desc: "toggle tag on this page", run: () => showTagPrompt() },
    // Both verbs have two entry points: the key types the verb into the line
    // (`a` ≡ `:open `, which is why completion and the key share one surface),
    // and pickCmd fires the verb once the line carries an address.
    open:    { defaultKey: "a", aliases: ["o"], desc: "open url / filter pages + history",
               run: (arg) => (arg.trim() ? openCurrent(arg) : enterCommand("open ")) },
    new:     { defaultKey: "A", aliases: ["O"], desc: "open url in a new window",
               run: (arg) => (arg.trim() ? openNew(arg) : enterCommand("new ")) },
    pages:   { defaultKey: "P", desc: "switch page (mudrad)", run: () => showPages() },
    q:       { defaultKey: null, desc: "close this page", run: () => closeSelf() },
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
    if (raw === "-") {
      await MudraConfig.unpin(key);
      return flashBar(`${key} -> config file`);
    }
    let val;
    try { val = JSON.parse(raw); } catch { val = raw; } // auto-type numbers/booleans/strings
    await MudraConfig.set({ [key]: val });
    return flashBar(`${key} = ${JSON.stringify(val)}`);
  }

  // Keybinding resolution: config JSON keybindings[key]=commandName overrides
  // defaultKey; `aliases` are the command's extra keys (a second home-row key
  // for one verb), and an explicit keybinding beats both.
  async function keyToCommand(key) {
    const cfg = await MudraConfig.all();
    const map = cfg.keybindings || {};
    for (const [name, def] of Object.entries(COMMANDS)) {
      const bound = Object.keys(map).find((k) => map[k] === name);
      if ((bound || def.defaultKey) === key) return name;
      if (key !== bound && (def.aliases || []).includes(key)) return name;
    }
    return null;
  }

  // ---- scrolling implementation ----
  // All of these move scrollTarget(), not the window: on a WebUI page the
  // window does not scroll at all (see scrollTarget above).
  const lineHeight = () => parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
  async function scrollLines(dir, o) {
    const cfg = await MudraConfig.all();
    scrollTarget().scrollTop += dir * (o.count || cfg.scrollStepLines) * lineHeight();
    await refreshBar();
  }
  async function scrollCols(dir, o) {
    const cfg = await MudraConfig.all();
    const t = scrollTarget();
    t.scrollLeft += dir * (o.count || cfg.scrollStepLines) * lineHeight();
    await refreshBar();
  }
  // Half a viewport, and no overlap: `pageOverlapLines` exists so a full page
  // does not replace all the context at once; half a page already leaves half.
  // A count multiplies halves (`3e` = one and a half screens).
  async function scrollHalfPage(dir, o) {
    const t = scrollTarget();
    t.scrollTop += dir * (o.count || 1) * ((t.clientHeight || innerHeight) / 2);
    await refreshBar();
  }
  async function scrollPage(dir, o) {
    const cfg = await MudraConfig.all();
    const t = scrollTarget();
    const overlap = (o.count != null ? o.count : cfg.pageOverlapLines) * lineHeight();
    t.scrollTop += dir * ((t.clientHeight || innerHeight) - overlap);
    await refreshBar();
  }
  // Numeric prefix + g: with a prefix jump to a percentage (30g -> 30%), without it back to top
  async function scrollToPctOrTop(o) {
    const t = scrollTarget();
    if (o.count != null) {
      const h = t.scrollHeight - (t.clientHeight || innerHeight);
      t.scrollTop = Math.min(h, h * o.count / 100);
    } else t.scrollTop = 0;
    await refreshBar();
  }
  async function scrollToBottom() {
    const t = scrollTarget();
    t.scrollTop = t.scrollHeight;
    await refreshBar();
  }

  // ---- command mode (enter with :; the centred panel holds the line and the candidate list) ----
  function filterCmd(q) {
    if (!cmdApi) return;
    // A stray leading colon is tolerated (the line has no ":" marker any more,
    // but the habit is older than the panel): strip it before parsing.
    const text = (q || "").replace(/^:/, "");
    const [name, ...rest] = text.split(/\s+/);
    const arg = rest.join(" ");
    // A URL verb with the separator typed is an address line: list the places
    // it can take you, not command names.
    if (URL_VERBS.has(name) && text.includes(" ")) return filterAddress(name, arg);
    historySeq++; // an address filter may still be in flight; it must not repaint this list
    const items = Object.entries(COMMANDS)
      .filter(([n]) => !name || n.startsWith(name))
      .map(([n, c]) => ({ label: n, desc: c.desc, value: n }));
    cmdApi.setItems(items);
  }

  async function pickCmd(cand, raw, api, reason) {
    const q = (raw || "").replace(/^:/, "").trim();
    const [name, ...rest] = q.split(/\s+/);
    const arg = rest.join(" ");
    cmdApi = null;
    await setMode("normal");
    if (reason === "cancel") return; // Esc never fires the typed text (that was a bug once)
    if (cand && cand.kind === "error") return; // the row is a diagnosis, not a target
    if (cand && cand.kind === "page") {
      const res = await send({ type: "focus_page", page_id: cand.value });
      return flashBar(res.ok === false ? (res.err || "focus failed") : "switched");
    }
    if (URL_VERBS.has(name)) {
      // The highlighted history row wins, else what was typed. A bare `:open`
      // with no address arms the line (`:open `) instead of re-entering itself.
      const url = cand && cand.kind === "url" ? cand.value : arg;
      if (!url) return enterCommand(`${name} `);
      return name === "new" ? openNew(url) : openCurrent(url);
    }
    if (!cand) return; // empty line, nothing highlighted
    const c = COMMANDS[cand.value] || COMMANDS[name];
    if (!c) return flashBar(`no such command: ${name}`);
    if (!c.run.length && rest.length) return flashBar(`${cand.value} takes no argument`);
    await c.run(cand.value === name ? arg : "", {});
    // No blanket refreshBar here: it would clobber the command's own flashBar message (e.g. the :set config echo).
    // Commands that need a refresh (scroll etc.) call it internally already.
  }

  const enterCommand = async (initial) => {
    setMode("command");
    // Backspace on an empty line leaves command mode: without a hook the key
    // was dead, and a dead Backspace reads as "the space won't delete".
    cmdApi = await MudraBar.openCommand(filterCmd, pickCmd, () => {
      const api = cmdApi;
      cmdApi = null;
      api?.close();
      setMode("normal");
    });
    if (initial) cmdApi.setValue(initial);
    else filterCmd("");
    // Live pages are console-role jump targets for `open`; they ride in the
    // background so the first keystrokes are never swallowed by a round trip.
    if (role === "console") send({ type: "pages" }).then((r) => {
      if (r.ok === false || !cmdApi) return;
      livePages = r.pages || [];
      const now = (cmdApi.value() || "").split(/\s+/)[0];
      if (URL_VERBS.has(now)) filterCmd(cmdApi.value());
    }).catch(() => {});
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
    // Shadow roots included: WebUI pages (and much of the modern web) put their
    // links and buttons in there, where querySelectorAll on the document sees
    // none of them.
    const els = deepQueryAll(
      "a[href], button, input[type=submit], input[type=button], [role=button], [onclick]"
    );
    return els.filter((el) => {
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
        // centred on the element: the half-extent shift comes from the span's
        // own box, so no text measuring is needed
        "position:fixed",
        `left:${Math.max(0, r.left + r.width / 2)}px`, `top:${Math.max(0, r.top + r.height / 2)}px`,
        "transform:translate(-50%,-50%)",
        `font:${c.hintFontSize}px monospace`,
        // frost + the hint alpha: the label sits on page text, so it blurs what
        // is under it instead of painting an opaque block over it
        `color:${MudraConfig.fgFor(c.hintBg, c.hintFg)}`,
        `background:${MudraConfig.withAlpha(c.hintBg, c.hintOpacity)}`,
        ...MudraConfig.frost(c.statusBlur),
        "padding:0 2px", "border-radius:2px", "white-space:pre",
      ].join(";");
      overlay.appendChild(d);
      return { el, seq: seqs[i], span: d };
    });
    document.documentElement.appendChild(overlay);
    hintSession = { overlay, nodes, typed: "", newWindow: !!newWindow, chars: c.hintChars };
    // `W` is the same hint state machine with a different verb; the bar must
    // say so, otherwise the two keys are indistinguishable until you click.
    setMode("hint", newWindow ? "hintnew" : "hint");
  }

  function hintFilter(key) {
    if (!hintSession) return;
    const { nodes } = hintSession;
    if (key === "Backspace") hintSession.typed = hintSession.typed.slice(0, -1);
    // Membership in the configured pool, not [a-z]: the pool is user text and
    // may hold punctuation (default has ";"), and any letter it assigns must be
    // typeable — a rejection here reads as a dead key on the 10th hint.
    else if (key.length === 1 && hintSession.chars.includes(key)) hintSession.typed += key;
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
    const samePageAnchor = !!href && !!a.hash &&
      a.pathname === location.pathname && a.search === location.search;
    if (samePageAnchor && !newWindow) {
      location.hash = a.hash; // same-page anchor jump in place, no new page
    } else if (href && /^https?:/.test(href)) {
      // f = open (this page navigates in place), F = new (a separate --app window)
      if (newWindow) send({ type: "new", url: href, from: location.href, ctx });
      else openCurrent(href);
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
    const inputs = deepQueryAll("input:not([type=hidden]):not([type=submit]):not([type=button]), textarea, [contenteditable=true], [contenteditable=plaintext-only]")
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
        "position:fixed",
        `left:${Math.max(0, r.left + r.width / 2)}px`, `top:${Math.max(0, r.top + r.height / 2)}px`,
        "transform:translate(-50%,-50%)",
        `font:${c.hintFontSize}px monospace`,
        `color:${MudraConfig.fgFor(c.hintBg, c.hintFg)}`,
        `background:${MudraConfig.withAlpha(c.hintBg, c.hintOpacity)}`,
        ...MudraConfig.frost(c.statusBlur),
        "padding:0 2px", "border-radius:2px", "white-space:pre",
      ].join(";");
      overlay.appendChild(d);
      return { el, seq: seqs[i], span: d };
    });
    document.documentElement.appendChild(overlay);
    inputHintSession = { overlay, nodes, typed: "", chars: c.hintChars };
    setMode("hint");
  }

  function inputHintFilter(key) {
    if (!inputHintSession) return;
    if (key === "Backspace") inputHintSession.typed = inputHintSession.typed.slice(0, -1);
    else if (key.length === 1 && inputHintSession.chars.includes(key)) inputHintSession.typed += key;
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

  // ---- tag picker (summon with t): the command popup with tree drilling ----
  // Candidates are one layer of the tag tree (label shows the full "a::b" path);
  // Tab drills into a branch, Backspace on an empty input goes back up, Enter toggles a leaf.
  // /tags and /tag both address nodes by bare name (not path), so the layer stack keeps
  // {path, name} per level: path is display-only, name feeds the API.
  let tagStack = []; // [{path, name}] from root down; empty = root layer, session-only
  const tagParentName = () => (tagStack.length ? tagStack[tagStack.length - 1].name : undefined);
  const tagLayerPath = () => tagStack.map((s) => s.path).join("::");
  async function tagLayerItems() {
    const r = await send({ type: "tags", parent: tagParentName() });
    const tags = r.tags || [];
    const base = tagLayerPath();
    return tags.map((name) => ({ label: base ? `${base}::${name}` : name, value: name }));
  }
  const tagFilter = (q, api) => {
    const s = (q || "").replace(/^:/, "").replace(/::$/, "").toLowerCase();
    api.setItems(tagLayerCache.filter((it) => !s || it.label.toLowerCase().includes(s)));
  };
  let tagLayerCache = [];
  async function tagToggle(cand) {
    cmdApi = null; await setMode("normal");
    const r = await send({ type: "tag", url: pageUrl(), tag: cand.value });
    await flashBar(r.action ? `${cand.label} ${r.action}` : (r.err || "tag failed"));
    await syncStatus();
  }
  async function tagDrill(cand, api) {
    const kids = await send({ type: "tags", parent: cand.value });
    if (!(kids.tags && kids.tags.length)) return false;
    tagStack.push({ path: cand.label, name: cand.value });
    cmdApi = api;
    await tagRender();
    return true;
  }
  async function tagRender() {
    const api = cmdApi;
    tagLayerCache = await tagLayerItems();
    if (!tagLayerCache.length && !tagStack.length) {
      api.close(); cmdApi = null; await setMode("normal");
      return flashBar("no tags");
    }
    api.setItems(tagLayerCache);
    // no path prefix in the input: rows already show full paths, and an empty input
    // is what enables the Backspace layer-up hook
  }
  async function showTagPrompt() {
    tagStack = [];
    setMode("command");
    cmdApi = await MudraBar.openCommand(
      tagFilter,
      // Enter: drill into a branch, toggle a leaf. (Tab used to drill; it now
      // moves the selection like everywhere else, so "choose" carries it.)
      async (cand, _raw, api) => {
        if (!cand) return;
        if (await tagDrill(cand, api)) return;
        await tagToggle(cand);
      },
      // Backspace on empty input: back up one layer
      async (_api) => {
        if (!tagStack.length) return;
        tagStack.pop();
        await tagRender();
      }
    );
    await tagRender();
  }
  async function flashBar(text) {
    await MudraBar.render({ ctx, mode, modeLabel, tags: pageTags, capsules: pageCapsules, message: text });
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
        return setMode("normal");
      }
      if (hintSession) return hintFilter(e.key);
      if (inputHintSession) return inputHintFilter(e.key);
      return;
    }

    // ---- normal mode ----
    // An open overlay takes the key first: the sequence completes, or is dropped
    // and the key acts as if it had never started; the keymap just closes. A
    // keystroke is never swallowed silently — that is how a keymap starts
    // feeling broken.
    if (pending) {
      const hit = pending.items.find((it) => it.key === e.key);
      closeOverlay();
      if (hit) { e.preventDefault(); await hit.run(); return; }
      if (e.key === "Escape") { e.preventDefault(); return; }
      return dispatchNormal(e);
    }
    if (keymapOpen) {
      closeOverlay();
      if (e.key === "Escape" || e.key === "?") { e.preventDefault(); return; }
      return dispatchNormal(e);
    }
    return dispatchNormal(e);
  }, true);

  // ---- overlays: a sequence waiting for its second key, and the ? keymap ----
  // Both are MudraBar.showMenu, so they wear the same pale frosted surface and
  // neither can outlive the mode it belongs to.
  let pending = null;   // {key, items, timer} — a sequence waiting for its second key
  let menu = null;      // the open overlay, whichever of the two it is
  let keymapOpen = false;

  function closeOverlay() {
    if (pending && pending.timer) clearTimeout(pending.timer);
    pending = null;
    keymapOpen = false;
    if (menu) { menu.close(); menu = null; }
    else MudraBar.hideMenu();
  }

  async function openPending(key) {
    const seq = SEQUENCES[key];
    closeOverlay();
    const cfg = await MudraConfig.all();
    pending = { key, items: seq.items, timer: null };
    menu = await MudraBar.showMenu({
      rows: seq.items.map((it) => ({ key: `${key} ${it.key}`, desc: it.desc })),
    });
    // The wait is finite: a menu that stays is a mode, and a mode nobody asked
    // for swallows the next key.
    pending.timer = setTimeout(closeOverlay, cfg.seqTimeout);
  }

  // Every binding in one list, including the sequences and the two openers that
  // are not commands (":" and "?"). Height is the viewport's problem, not the
  // list's: the menu scrolls.
  async function openKeymap() {
    closeOverlay();
    const cfg = await MudraConfig.all();
    const map = cfg.keybindings || {};
    const rows = [];
    for (const [name, def] of Object.entries(COMMANDS)) {
      const bound = Object.keys(map).find((k) => map[k] === name);
      const k = bound || def.defaultKey;
      // a command with aliases shows both keys: one row, so the list stays a map
      const keys = [k, ...(def.aliases || [])].filter(Boolean).join(" / ");
      if (keys) rows.push({ key: keys, desc: def.desc });
    }
    for (const [sk, seq] of Object.entries(SEQUENCES))
      for (const it of seq.items) rows.push({ key: `${sk} ${it.key}`, desc: it.desc });
    rows.push({ key: ":", desc: "command line" });
    rows.push({ key: "?", desc: "this list" });
    rows.push({ key: "Esc", desc: "cancel" });
    rows.push({ key: "0-9", desc: "count prefix (5j, 30g)" });
    rows.sort((a, b) => a.key.localeCompare(b.key));
    menu = await MudraBar.showMenu({ rows });
    keymapOpen = true;
  }

  // One normal-mode key, in one place: the sequence menu and the keymap re-enter
  // here after they are done with a key, so no path can skip the numeric prefix
  // or the ":" / "?" openers.
  async function dispatchNormal(e) {
    // Numeric prefix accumulation (30g percent jump)
    if (/^[0-9]$/.test(e.key)) {
      e.preventDefault();
      numPrefix = (numPrefix + e.key).replace(/^0+(?=.)/, "");
      await refreshBar(); // show the typed digits in the status bar
      return;
    }
    const plain = !e.ctrlKey && !e.metaKey && !e.altKey;
    if (plain && e.key === ":") { e.preventDefault(); return enterCommand(); }
    if (plain && e.key === "?") { e.preventDefault(); return openKeymap(); }
    if (!plain) return;

    // key -> command (configurable); the numeric prefix is passed as count to commands that support it
    const count = numPrefix ? parseInt(numPrefix, 10) : null;
    numPrefix = "";
    const name = await keyToCommand(e.key);
    if (name) {
      e.preventDefault();
      COMMANDS[name].run("", { count });
      return;
    }
    // A sequence opener shows its menu and waits; any other key is dead and only
    // clears the numeric-prefix display.
    if (SEQUENCES[e.key]) { e.preventDefault(); return openPending(e.key); }
    refreshBar();
  }

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
