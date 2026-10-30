// mudra-keys service worker: a page's address bar -> mudrad's control API.
// mudrad (127.0.0.1:8899) is the only control point: it owns instance lifecycle,
// page bookkeeping and the tag-forest.
//
// Why every verb goes through here: a content script cannot reach 127.0.0.1 on
// an https page (the page's mixed-content / private-network rules block both a
// fetch and a socket — measured), and MV3 gives content scripts no cross-origin
// privilege. The extension's own context is the only exempt one.
//
// The link is a Port per page (`content.js`), not one-shot messages: replies
// pair by `id`, a disconnect fails in-flight calls immediately, and the page
// pings every 20s so this worker never idles out while a page is open. That
// matters because a worker whose script cache was evicted under it does not come
// back — every later message answers "Receiving end does not exist" until the
// browser exits. mudrad stopped doing that eviction: it no longer clears a
// profile's extension caches while a browser holds the profile.

const MUDRAD = "http://127.0.0.1:8899";

async function post(path, body) {
  const r = await fetch(MUDRAD + path, {
    method: "POST",
    headers: { "Content-Type": "text/plain" },
    body: JSON.stringify(body || {}),
  });
  const text = await r.text();
  let out = null;
  try { out = JSON.parse(text); } catch { /* not JSON: the status line is all we have */ }
  // the daemon's own `err` is the useful sentence ("no bitwarden entry for …"),
  // so it must survive a non-200 instead of being replaced by the status code
  if (!r.ok) throw new Error((out && out.err) || `mudrad ${path}: HTTP ${r.status}`);
  return out || {};
}

const tabIdOf = (sender) => (sender && sender.tab ? sender.tab.id : undefined);

// ---- the page link: one port per page, request/response by id -------------
chrome.runtime.onConnect.addListener((port) => {
  if (port.name !== "mudra") return;
  const tabId = tabIdOf(port.sender);
  port.onMessage.addListener(async (msg) => {
    if (!msg || msg.type === "ping") return; // the traffic is the point, not an answer
    let reply;
    try {
      // inside the try on purpose: a throw up here happens before any answer,
      // and the page would sit out its timeout instead of seeing the reason
      console.log("[mudra-keys] call", msg.type, "tab", tabId);
      reply = await handle(msg, tabId);
    } catch (e) {
      reply = { ok: false, err: String((e && e.message) || e) };
    }
    reply.id = msg.id;
    try {
      port.postMessage(reply);
    } catch { /* page gone mid-call: nothing to answer */ }
  });
});

// One verb against mudrad. A throw becomes `{ok: false, err}` at the port, and
// the bar prints it verbatim — a wrong verb is the one failure that must not be
// silent.
async function handle(msg, tabId) {
  switch (msg.type) {
    case "new": {
      // hints `F` / the console page's open: a separate --app window in this
      // page's ctx. `from` is the opener's own url — mudrad turns it into the
      // page-tree parent link (chrome tab ids are not CDP target ids, so tabId
      // alone cannot name the page that asked).
      const out = await post("/open", { url: msg.url, tabId, from: msg.from, ctx: msg.ctx });
      return { ok: true, ...out };
    }
    case "visit": {
      // the `open` verb: resolve the address and record the visit; the content
      // script navigates itself to the returned url
      return post("/visit", { url: msg.url }); // {url}
    }
    case "status": {
      // status bar data: ctx + page tags, one round trip
      return post("/ctx_status", { tabId, url: msg.url }); // {ctx, tags}
    }
    case "tag": {
      // toggle a tag on the current page
      return post("/tag", { tabId, url: msg.url, tag: msg.tag }); // {tag, action}
    }
    case "tags": {
      // tag tree listing (for completion): {parent?} -> {tags: [...]}
      return post("/tags", { parent: msg.parent });
    }
    case "pages": {
      // open page list across contexts (command `pages`)
      return post("/pages", { ctx: msg.ctx });
    }
    case "history": {
      // address-bar completion: ranked open-history candidates
      return post("/history", { query: msg.query, limit: msg.limit });
    }
    case "fill_password": {
      // answered by a guest, not by mudrad: `/fill_password` writes the event row
      // and waits for a declared extension (bw-fill → rbw) to reply
      return post("/fill_password", { url: msg.url, ctx: msg.ctx, user_hint: msg.user_hint });
    }
    case "focus_page": {
      // switch to another page via mudrad (CDP activate + WM raise)
      return post("/focus_page", { page_id: msg.page_id });
    }
    case "close": {
      // `:q` — the sender's own tab, no URL matching. tabs.remove needs no
      // permission; on an --app window (one tab) it closes the window.
      if (tabId == null) return { ok: false, err: "no tab to close" };
      await chrome.tabs.remove(tabId);
      return { ok: true };
    }
    default:
      return { ok: false, err: `unknown type ${msg.type}` };
  }
}

// ---- config fetch: GET /config at extension startup -> chrome.storage.local ----
// The config file is the source of truth; a key the user pinned with :set is the
// exception (storage had to be authoritative for runtime changes). So every key
// the file carries is re-applied on each sync unless it is in `setKeys` — that
// way editing ~/.config/mudra/config.kdl actually changes the bar instead of
// being shadowed by a value some earlier sync wrote once.
async function syncConfig() {
  try {
    const r = await fetch(MUDRAD + "/config");
    if (!r.ok) return;
    const { ok, config } = await r.json();
    if (!ok || !config) return;
    const stored = await chrome.storage.local.get(null);
    const pinned = new Set(stored.setKeys || []);
    const patch = {};
    for (const [k, v] of Object.entries(config)) {
      if (pinned.has(k)) continue; // :set wins over the file
      if (!(k in stored) || stored[k] !== v) patch[k] = v;
    }
    if (Object.keys(patch).length) await chrome.storage.local.set(patch);
    await chrome.storage.local.set({ configSyncedAt: Date.now() });
  } catch { /* mudrad offline: silently fall back to local defaults */ }
}

chrome.runtime.onInstalled.addListener(syncConfig);
chrome.runtime.onStartup.addListener(syncConfig);
syncConfig();

// ---- chrome:// pages: injected from here, not declaratively ----------------
// A "chrome://*/*" match pattern in content_scripts makes Chrome reject the
// WHOLE extension whenever the launcher does not pass --extensions-on-chrome-urls
// ("Invalid value for 'content_scripts[0].matches[1]': Invalid scheme."), which
// is an extension-less browser and an error bubble on startup. The same match is
// legal in host_permissions, so the scripts are injected per committed tab
// instead: without the switch the executeScript call is the only thing that
// fails, and the extension still loads.
const CHROME_INJECTED = ["shared/lib.js", "extension/content.js"];

chrome.webNavigation.onCommitted.addListener(async (d) => {
  if (d.frameId !== 0 || !d.url.startsWith("chrome://")) return;
  try {
    // two files, order matters: content.js reads the Mudra* globals lib.js defines
    await chrome.scripting.executeScript({ target: { tabId: d.tabId }, files: CHROME_INJECTED });
  } catch (e) {
    console.log("[mudra-keys] chrome:// inject skipped:", (e && e.message) || e);
  }
});