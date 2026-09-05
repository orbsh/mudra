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

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  (async () => {
    try {
      switch (msg.type) {
        case "open": {
          // hints / link activation: mudrad resolves ctx from sender tab id
          const tabId = sender.tab ? sender.tab.id : msg.tabId;
          const out = await post("/open", { url: msg.url, tabId });
          sendResponse({ ok: true, ...out });
          break;
        }
        case "back":
        case "forward":
          // history navigation happens page-side; SW only relays nothing.
          // Kept as explicit no-op so content.js stays transport-uniform.
          sendResponse({ ok: true });
          break;
        case "status": {
          // status bar push: page info -> mudrad log/tag endpoints later
          const tabId = sender.tab ? sender.tab.id : msg.tabId;
          await post("/ping", { tabId, title: msg.title, url: msg.url });
          sendResponse({ ok: true });
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
