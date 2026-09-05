// mudra-keys content script: letter hints, H/L history, status bar.
// Link opening goes through sw.js -> mudrad /open (tabId routing, ctx resolved there).
(() => {
  if (window.__mudraKeys) return;
  window.__mudraKeys = true;

  const send = (msg) =>
    new Promise((res) => chrome.runtime.sendMessage(msg, (r) => res(r || {})));

  // ---- state ----
  let mode = "normal"; // normal | insert | hint
  let ctx = "";        // filled from first /open response or status poll
  let hintSession = null;

  const setMode = async (m) => { mode = m; await refreshBar(); };
  const refreshBar = async () => {
    await MudraBar.render({
      ctx, mode,
      title: document.title.slice(0, 60),
      url: location.host + location.pathname,
      scroll: scrollPct(),
    });
  };
  const scrollPct = () => {
    const h = document.documentElement.scrollHeight - innerHeight;
    return h > 0 ? `${Math.round((scrollY / h) * 100)}%` : "all";
  };

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

  // 字母序列生成：hintChars 池 + 短码优先（同 vimium 风格的字母前缀序）
  function hintStrings(n, chars) {
    const out = [];
    let len = 1;
    while (out.length < n) {
      const total = Math.pow(chars.length, len);
      for (let i = 0; i < total && out.length < n; i++) {
        // 高频位放低位字母：反转序让首字母尽量是池首（单手区）
        let s = "", x = i;
        for (let j = 0; j < len; j++) { s = chars[x % chars.length] + s; x = Math.floor(x / chars.length); }
        out.push(s);
      }
      len++;
    }
    return out;
  }

  function showHints() {
    const cfg = MudraConfig.all();
    return cfg.then((c) => {
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
    });
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

  // ---- history ----
  const goHistory = (d) => history.go(d);

  // ---- key handling ----
  const isEditable = (t) =>
    t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));

  document.addEventListener("keydown", (e) => {
    if (mode === "insert") {
      if (e.key === "Escape") { e.preventDefault(); setMode("normal"); }
      return; // 插入模式全放行
    }
    if (isEditable(e.target)) return; // 焦点在输入框时不劫持

    if (mode === "hint") {
      e.preventDefault();
      if (e.key === "Escape") return hideHints();
      if (/^[a-z]$/.test(e.key)) hintFilter(e.key);
      return;
    }
    // normal
    if (e.key === "f" && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      setMode("hint").then(async () => { hintSession = await showHints(); });
    } else if (e.key === "h" && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault(); goHistory(-1);
    } else if (e.key === "l" && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault(); goHistory(1);
    } else if (e.key === "i" && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault(); setMode("insert");
    }
  }, true);

  // 插入模式仅显式 i 进入 / Esc 退出；不随 focusin 自动切（页面 autofocus 会让 normal 永远不可达）。
  // 输入框有焦点时按键不劫持（isEditable 检查已覆盖），打字天然可用。

  // ---- status bar ----
  const boot = async () => {
    await MudraBar.mount();
    await refreshBar();
    addEventListener("scroll", refreshBar, { passive: true });
    // ctx: 问 mudrad 当前上下文（通过 SW 的 status ping 回传? 首版：留空，ctx 由面板/CLI 推送）
  };
  if (document.readyState === "loading")
    document.addEventListener("DOMContentLoaded", boot);
  else boot();
})();
