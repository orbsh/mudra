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
        case "open": {
          // hints / link activation: mudrad resolves ctx from sender tab id
          const out = await post("/open", { url: msg.url, tabId: tabIdOf(sender) });
          sendResponse({ ok: true, ...out });
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
    if (Object.keys(patch).length) await chrome.storage.local.set(patch);
    await chrome.storage.local.set({ configSyncedAt: Date.now() });
  } catch { /* mudrad offline: silently fall back to local defaults */ }
}

chrome.runtime.onInstalled.addListener(syncConfig);
chrome.runtime.onStartup.addListener(syncConfig);
syncConfig(); // re-sync on every SW cold start (MV3 SWs sleep often; onStartup is not guaranteed to fire)
