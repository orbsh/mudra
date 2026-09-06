// mudra panel -- SolidJS hyperscript edition (zero-build).
// h() returns a lazy factory ([$ELEMENT] marker) that is only evaluated when inserted into the DOM;
// a component is just a plain function returning an h() factory, which Solid calls as a component automatically.
// Solid comes from /shared/vendor/solid-bundle.js (window.MudraSolid, the same copy shared with the extension).
const { h, render, createSignal, createMemo, createEffect, For, Show } = window.MudraSolid;

// ---- WS client (request/response, id -> promise) ----
function makeClient() {
  const pending = new Map();
  let ws = null;
  let nextId = 1;
  let readyResolve = null;
  const ready = new Promise((res) => { readyResolve = res; });

  function connect() {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const port = Number(location.port) + 1; // HTTP :9299, WS :9300
    ws = new WebSocket(`${proto}://${location.hostname}:${port}/`);
    ws.onopen = () => readyResolve();
    ws.onmessage = (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id && pending.has(m.id)) {
        const { resolve, reject } = pending.get(m.id);
        pending.delete(m.id);
        m.ok ? resolve(m) : reject(new Error(m.err || "ws error"));
      } else if (!m.id && client.onEvent) {
        client.onEvent(m); // server-initiated push (pages_changed etc.)
      }
    };
    ws.onclose = () => setTimeout(connect, 800);
  }
  connect();
  function call(op, args) {
    if (ws.readyState !== 1) return Promise.reject(new Error("ws"));
    const id = String(nextId++);
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, op, ...args }));
    });
  }
  const client = {
    call,
    ready,
    onEvent: null, // callback for server-initiated pushes (messages without id), assigned by the App layer
  };
  return client;
}
const client = makeClient();

// ---- state ----
const [contexts, setContexts] = createSignal([]);   // situation leaf-name list
const [ctx, setCtx] = createSignal("");             // context currently viewed / switched to
const [pages, setPages] = createSignal([]);   // flat
const [roots, setRoots] = createSignal([]);   // deep tag tree
const [sortNew, setSortNew] = createSignal(true);
const [filters, setFilters] = createSignal(new Set());
const [collapsed, setCollapsed] = createSignal(new Set());
const [shot, setShot] = createSignal(null);   // {x,y,url}
const [popup, setPopup] = createSignal(null); // capsule switch menu {x,y,items[],cb}
const [thumbnails, setThumbnails] = createSignal(false); // hover-screenshot toggle (config.kdl ui.thumbnails, disabled by default)

let byId = new Map();
let rankRoots = [];
let allLeaves = [];

function indexTagTree(trees) {
  byId = new Map();
  rankRoots = [];
  allLeaves = [];
  function walk(nodes, isRankRoot) {
    for (const n of nodes) {
      byId.set(n.id, n);
      if (n.rank === null && !n.root) allLeaves.push(n);
      walk(n.children || [], isRankRoot || !!n.rank_axis);
    }
  }
  walk(trees, false);
  for (const r of trees) if (r.rank_axis) rankRoots.push(r);
}

async function load() {
  await client.ready;           // wait for the WS connection before fetching (so the initial Forest call is not rejected)
  const r = await client.call("forest");
  setRoots(r.forest);
  setContexts(r.contexts);
  indexTagTree(r.forest);
  if (!ctx() && r.current) setCtx(r.current);
  else if (!ctx() && r.contexts.length) setCtx(r.contexts[0]);
}
async function loadPages() {
  if (!ctx()) { setPages([]); return; }
  const r = await client.call("pages", { ctx: ctx() });
  setPages(r.pages);
}
async function switchCtx(name) {
  setCtx(name);
  // Switching goes through the backend (mudrad /ctx); the backend broadcasts context_changed -> state is consistent by the time the broadcast returns
  try { await client.call("set_ctx", { ctx: name }); } catch (e) { console.warn(e); }
}
createEffect(() => {
  indexTagTree(roots());
  loadPages();
});
load().catch((e) => console.warn(e));
// Hover-screenshot toggle: if config is unavailable (WS down / old backend), treat as disabled by default
client.call("config").then((r) => setThumbnails(!!r.config?.thumbnails)).catch(() => {});

// Server push: mudrad broadcasts pages_changed when the page set changes (open/close/title update);
// the frontend refetches the current context's pages on receipt -- no polling.
client.onEvent = (ev) => {
  if (ev.event === "pages_changed") loadPages().catch(() => {});
  else if (ev.event === "context_changed" && ev.ctx) setCtx(ev.ctx);
};

// ---- page tree ----
const pageTree = createMemo(() => {
  const list = pages();
  const byParent = new Map();
  const idSet = new Set(list.map((p) => p.id));
  for (const p of list) {
    let pid = p.parent_id;
    if (pid == null || pid === 0 || !idSet.has(pid)) pid = 0;
    if (!byParent.has(pid)) byParent.set(pid, []);
    byParent.get(pid).push(p);
  }
  const sort = (arr) => arr.slice().sort((a, b) =>
    sortNew() ? (b.opened_at || 0) - (a.opened_at || 0) : (a.opened_at || 0) - (b.opened_at || 0));
  const fl = [...filters()];
  const match = (p) => fl.length === 0 || fl.every((t) => p.tag_ids.includes(t));
  function build(pid, lvl) {
    return (byParent.get(pid) || []).filter(match).map((p) => {
      const kids = build(p.id, lvl + 1);
      const open = !collapsed().has(p.id);
      return { p, lvl, kids, open };
    });
  }
  return build(0, 0);
});

const timeAgo = (t) => {
  if (!t) return "";
  const s = Date.now() / 1000 - t;
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
};

// The node selected for a page under a given rank root
const rankSel = (page, root) => root.children.find((c) => page.tag_ids.includes(c.id));

async function setRank(page, root, k) {
  const others = root.children.map((c) => c.id);
  let next = page.tag_ids.filter((t) => !others.includes(t));
  if (k > 0) {
    const target = root.children.find((c) => c.rank === k);
    if (target) next = [...next, target.id];
  }
  await client.call("set_tags", { page_id: page.id, tag_ids: next });
  await loadPages();
}

async function setTags(page, ids) {
  await client.call("set_tags", { page_id: page.id, tag_ids: ids });
  await loadPages();
}

// Plain tag (a leaf not under a rank root) -- split its path into capsules
const normalTags = (page) =>
  page.tag_ids.map((t) => byId.get(t)).filter((n) => n && !n.root && n.rank === null);

// Segment-level switch: select a sibling under that segment's parent
function capsuleSwitch(page, tagNode, depth) {
  const parts = tagNode.path.split("::");
  const root = roots().find((r) => r.name === parts[0]);
  if (!root) return;
  let parentChildren = [];
  if (depth === 0) {
    parentChildren = root.children.filter((c) => c.rank === null); // siblings (plain tags under the same root)
  } else {
    let node = root;
    for (let i = 1; i <= depth; i++) {
      const child = (node.children || []).find((c) => c.name === parts[i]);
      if (!child) return;
      node = child;
    }
    parentChildren = (node.children || []).filter((c) => c.rank === null);
  }
  setPopup({
    x: 0, y: 0, items: parentChildren.map((c) => ({ label: c.path, id: c.id })),
    cb: (id) => setTags(page, page.tag_ids.filter((t) => t !== tagNode.id).concat(id)),
  });
}

async function addChild(page, parentId) {
  const name = prompt("New child tag name");
  if (!name) return;
  try {
    const { id } = await client.call("create_tag", { parent_id: parentId, name });
    await setTags(page, page.tag_ids.concat(id));
    load().catch(() => {});
  } catch (e) { alert("Failed to create: " + e.message); }
}

async function addTagToPage(page) {
  setPopup({
    x: 0, y: 0, items: allLeaves.map((n) => ({ label: n.path, id: n.id })),
    place: "add",
    cb: async (id) => { await setTags(page, page.tag_ids.concat(id)); },
  });
}

// Focus + hover screenshot (short-circuits when ui.thumbnails=false: no request, no render)
let shotTimer = null;
function hover(page, on, e) {
  if (!thumbnails()) return;
  clearTimeout(shotTimer);
  if (!on) { setShot(null); return; }
  const x = e.clientX, y = e.clientY;
  shotTimer = setTimeout(async () => {
    try {
      const { data } = await client.call("shot", { page_id: page.id });
      if (data) setShot({ x, y, data });
    } catch { }
  }, 250);
}

function App() {
  const pageSet = () => pages();
  const toggleFilter = (id) => setFilters((s) => {
    const n = new Set(s); n.has(id) ? n.delete(id) : n.add(id); return n;
  });

  // Header: context + sorting + filter
  const hdrRow = () => h("div.hdr-row", [
    h("select", { value: ctx, onChange: (e) => switchCtx(e.currentTarget.value) },
      () => h(For, { each: contexts() }, (c) => h("option", { value: c }, c))),
    h("button.b", { onClick: () => setSortNew(!sortNew()) }, () => sortNew() ? "new->old" : "old->new"),
    h("span.count", () => `${pageSet().length} pages`),
  ]);
  const filts = () => h("div.filts",
    () => h(For, { each: allLeaves }, (n) =>
      h("button", {
        class: () => "chip" + (filters().has(n.id) ? " on" : ""),
        onClick: () => toggleFilter(n.id),
      }, n.path)));

  // Capsule switch menu (x/y given by the setPopup caller)
  const popupMenu = () => {
    const p = popup();
    return h("div.menu", {
      onClick: (e) => e.stopPropagation(),
      style: `left:${p.x}px;top:${p.y}px;`,
    }, [
      p.place === "add" ? h("div.menu-title", "Assign tag") : null,
      h(For, { each: p.items }, (it) => h("button.menu-item", {
        onClick: () => { const cb = p.cb; setPopup(null); cb(it.id); },
      }, it.label)),
    ]);
  };

  // Hover screenshot popup
  const shotBox = () => {
    const s = shot();
    return h("div.shot", {
      style: `left:${Math.min(s.x + 16, innerWidth - 340)}px;top:${Math.min(s.y + 16, innerHeight - 220)}px;`,
    }, h("img", { src: s.data, alt: "" }));
  };

  // Open new window: the bottom address bar was removed; use the extension's :open command (the console role filters existing pages, falling back to opening the URL)
  return h("div.panel", {
    onClick: () => popup() && popup().place === undefined && setPopup(null),
  }, () => [
    h("header.hdr", [hdrRow(), filts()]),
    h("main.tree",
      () => h(For, { each: pageTree() }, (nd) => h(PageNode, { nd }))),
    h(Show, { when: popup() }, popupMenu),
    h(Show, { when: shot() }, shotBox),
  ]);
}

function PageNode(props) {
  const p = () => props.nd.p;
  return h("div.node", [
    h("div.ncard", { class: () => (p().closed ? "closed" : ""), style: { "margin-left": () => `${props.nd.lvl * 16}px` } }, [
      // Row 1: close/open + delete + title + link + collapse
      h("div.row1", [
        h("button.act", {
          title: () => (p().closed ? "Open" : "Close window"),
          onClick: (e) => {
            e.stopPropagation();
            client.call(p().closed ? "reopen" : "close", { page_id: p().id }).catch(() => {});
          },
        }, () => (p().closed ? "↻" : "⨯")),
        h("button.act.del", {
          title: "Delete (only while closed)",
          // Grayed placeholder while open, click does nothing; delete is a soft delete
          disabled: () => !p().closed,
          onClick: (e) => {
            e.stopPropagation();
            if (p().closed) client.call("delete", { page_id: p().id }).catch(() => {});
          },
        }, "🗑"),
        h("button.tw", {
          onClick: () => setCollapsed((s) => {
            const n = new Set(s); n.has(p().id) ? n.delete(p().id) : n.add(p().id); return n;
          }),
        }, () => props.nd.kids.some((k) => !filters().size) || props.nd.kids.length > 0
          ? (props.nd.open && props.nd.kids.length ? "▾" : "▸") : ""),
        h("a.link", {
          href: () => p().url,
          onClick: (e) => { e.preventDefault(); client.call("focus", { page_id: p().id }).catch(() => {}); },
          onMouseMove: (e) => hover(p(), true, e),
          onMouseLeave: () => hover(p(), false),
        }, () => p().title),
        h("span.meta", () => timeAgo(p().opened_at)),
      ]),
      // Row 2: rank + plain capsules + add
      h("div.row2", [
        h(For, { each: rankRoots }, (root) => h(RankAxis, { page: p(), root })),
        h(For, { each: () => normalTags(p()) }, (t) => h(Capsule, { page: p(), t })),
        h("button.b.add", {
          onClick: (e) => { e.stopPropagation(); addTagToPage(p()); },
        }, "＋"),
      ]),
    ]),
    h(For, { each: () => props.nd.kids }, (k) => h(PageNode, { nd: k })),
  ]);
}

function RankAxis(props) {
  return window.MudraTags.RankAxis({
    root: props.root,
    sel: rankSel(props.page, props.root),
    onPick: (sel, k) => setRank(props.page, props.root, k),
  });
}

function Capsule(props) {
  const t = () => props.t;
  // Plain tag capsule: each segment is one path level; clicking a segment switches to a sibling; leading x removes, trailing + adds a child
  return window.MudraTags.Capsule({
    tag: t(),
    onSeg: (tag, i, el) => openSegMenu(props.page, tag, i, el.getBoundingClientRect()),
    onRemove: (tag) => setTags(props.page, props.page.tag_ids.filter((x) => x !== tag.id)),
    onAddChild: (tag) => addChild(props.page, tag.id),
  });
}

function openSegMenu(page, t, depth, rect) {
  const parts = t.path.split("::");
  const root = roots().find((r) => r.name === parts[0]);
  if (!root) return;
  let parentChildren = [];
  if (depth === 0) {
    parentChildren = root.children.filter((c) => c.rank === null);
  } else {
    let node = root;
    for (let i = 1; i <= depth; i++) {
      const child = (node.children || []).find((c) => c.name === parts[i]);
      if (!child) return;
      node = child;
    }
    parentChildren = (node.children || []).filter((c) => c.rank === null);
  }
  setPopup({
    x: rect.left, y: rect.bottom,
    items: parentChildren.map((c) => ({ label: c.path, id: c.id })),
    cb: (id) => setTags(page, page.tag_ids.filter((x) => x !== t.id).concat(id)),
  });
}

render(() => h(App), document.getElementById("root"));
