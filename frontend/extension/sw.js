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
