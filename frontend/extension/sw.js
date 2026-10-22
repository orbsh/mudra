// mudra-keys service worker: content script messages -> mudrad HTTP control API.
// mudrad (127.0.0.1:8899) is the only control point: it owns instance lifecycle,
// page bookkeeping and the tag-forest. This SW is a thin bridge.

const MUDRAD = "http://127.0.0.1:8899";

async function post(path, body) {
  const r = await fetch(MUDRAD + path, {
    method: "POST",
    headers: { "Content-Type": "text/plain" },
    body: JSON.stringify(body || {}),
  });
  if (!r.ok) throw new Error(`mudrad ${path}: HTTP ${r.status}`);
  return r.json();
}

const tabIdOf = (sender) => (sender && sender.tab ? sender.tab.id : undefined);

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  console.log("[mudra-keys] msg", msg.type, "tab", sender.tab && sender.tab.id);
  (async () => {
    try {
      switch (msg.type) {
        case "new": {
          // hints `F` / the console page's open: a separate --app window in
          // the sender's ctx (mudrad resolves ctx from the tab id)
          const out = await post("/open", { url: msg.url, tabId: tabIdOf(sender) });
          sendResponse({ ok: true, ...out });
          break;
        }
        case "visit": {
          // the `open` verb: resolve the address and record the visit; the
          // content script navigates itself to the returned url
          const out = await post("/visit", { url: msg.url });
          sendResponse(out); // {url}
          break;
        }
        case "status": {
          // status bar data: ctx + page tags, one round trip
          const out = await post("/ctx_status", {
            tabId: tabIdOf(sender), url: msg.url,
          });
          sendResponse(out); // {ctx, tags}
          break;
        }
        case "tag": {
          // toggle a tag on the current page
          const out = await post("/tag", {
            tabId: tabIdOf(sender), url: msg.url, tag: msg.tag,
          });
          sendResponse(out); // {tag, action: added|removed}
          break;
        }
        case "tags": {
          // tag tree listing (for completion): {parent?} -> {tags: [...]}
          const out = await post("/tags", { parent: msg.parent });
          sendResponse(out);
          break;
        }
        case "pages": {
          // open page list across contexts (command `pages`)
          const out = await post("/pages", { ctx: msg.ctx });
          sendResponse(out);
          break;
        }
        case "history": {
          // address-bar completion: ranked open-history candidates
          const out = await post("/history", { query: msg.query, limit: msg.limit });
          sendResponse(out);
          break;
        }
        case "focus_page": {
          // switch to another page via mudrad (CDP activate + WM raise)
          const out = await post("/focus_page", { page_id: msg.page_id });
          sendResponse(out);
          break;
        }
        default:
          sendResponse({ ok: false, err: `unknown type ${msg.type}` });
      }
    } catch (e) {
      sendResponse({ ok: false, err: String(e && e.message || e) });
    }
  })();
  return true; // async sendResponse
});

// ---- config fetch: GET /config at extension startup -> chrome.storage.local ----
// storage takes precedence over fetched values (runtime keys changed via :set are not clobbered):
// only write keys that exist in defaults and were never locally changed via :set
// (configSyncedAt marks whether the first round already ran).
//
// The shipped look changes between releases. Because syncConfig only fills keys
// that are absent, a colour stored by an older release would pin the old bar
// forever — so CONFIG_REV re-applies the look keys once per revision. A :set made
// after that bump still wins, since this only runs when the stored rev is behind.
const CONFIG_REV = 2;
const LOOK_KEYS = ["statusFg", "statusBg", "insertFg", "insertBg", "hintFg", "hintBg", "statusBlur"];

async function syncConfig() {
  try {
    const r = await fetch(MUDRAD + "/config");
    if (!r.ok) return;
    const { ok, config } = await r.json();
    if (!ok || !config) return;
    const stored = await chrome.storage.local.get(null);
    const patch = {};
    for (const [k, v] of Object.entries(config)) {
      if (!(k in stored) || stored[k] === undefined) patch[k] = v;
    }
    if ((stored.configRev || 1) < CONFIG_REV) {
      for (const k of LOOK_KEYS) if (config[k] !== undefined) patch[k] = config[k];
      patch.configRev = CONFIG_REV;
    }
    if (Object.keys(patch).length) await chrome.storage.local.set(patch);
    await chrome.storage.local.set({ configSyncedAt: Date.now() });
  } catch { /* mudrad offline: silently fall back to local defaults */ }
}

chrome.runtime.onInstalled.addListener(syncConfig);
chrome.runtime.onStartup.addListener(syncConfig);
syncConfig(); // re-sync on every SW cold start (MV3 SWs sleep often; onStartup is not guaranteed to fire)
