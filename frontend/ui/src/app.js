// mudra panel — SolidJS hyperscript 版（零构建）。
// h() 返回的是惰性工厂（[$ELEMENT] 标记），插入 DOM 时才求值；
// 组件就是返回 h() 工厂的普通函数，Solid 会自动当 component 调用。
// Solid 由 /shared/vendor/solid-bundle.js 提供（window.MudraSolid，与扩展共用同一份）。
const { h, render, createSignal, createMemo, createEffect, For, Show } = window.MudraSolid;

const MUDRAD_HTTP = "http://127.0.0.1:8899"; // 配置等少量 HTTP 直读（WS 只承载 op）

// ---- WS client（请求/响应，id -> promise）----
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
        client.onEvent(m); // 服务端主动推送（pages_changed 等）
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
    onEvent: null, // 服务端主动推送（无 id 的消息）回调，App 层赋值
  };
  return client;
}
const client = makeClient();

// ---- state ----
const [contexts, setContexts] = createSignal([]);   // situation 叶名列表
const [ctx, setCtx] = createSignal("");             // 当前查看/切换的上下文
const [pages, setPages] = createSignal([]);   // flat
const [roots, setRoots] = createSignal([]);   // deep tag tree
const [sortNew, setSortNew] = createSignal(true);
const [filters, setFilters] = createSignal(new Set());
const [collapsed, setCollapsed] = createSignal(new Set());
const [shot, setShot] = createSignal(null);   // {x,y,url}
const [popup, setPopup] = createSignal(null); // 胶囊切换菜单 {x,y,items[],cb}

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
  await client.ready;           // 等 WS 连上再取数（避免初次 Forest 调用被拒）
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
  // 切换走后端（mudrad /ctx），后端广播 context_changed → 广播回来时已一致
  try { await client.call("set_ctx", { ctx: name }); } catch (e) { console.warn(e); }
}
createEffect(() => {
  indexTagTree(roots());
  loadPages();
});
load().catch((e) => console.warn(e));

// 服务端推送：mudrad 在 page 集变化（新开/关闭/标题更新）时广播 pages_changed，
// 前端收到后重取当前上下文页面——不做轮询。
client.onEvent = (ev) => {
  if (ev.event === "pages_changed") loadPages().catch(() => {});
  else if (ev.event === "context_changed" && ev.ctx) setCtx(ev.ctx);
};

// ---- page 树 ----
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
  if (s < 60) return "刚刚";
  if (s < 3600) return `${Math.floor(s / 60)}分前`;
  if (s < 86400) return `${Math.floor(s / 3600)}时前`;
  return `${Math.floor(s / 86400)}天前`;
};

// 页在某 rank 根下选中的节点
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

// 普通 tag（非 rank 根的叶）——按路径拆成胶囊
const normalTags = (page) =>
  page.tag_ids.map((t) => byId.get(t)).filter((n) => n && !n.root && n.rank === null);

// 段级切换：在该段父下选同级
function capsuleSwitch(page, tagNode, depth) {
  const parts = tagNode.path.split("::");
  const root = roots().find((r) => r.name === parts[0]);
  if (!root) return;
  let parentChildren = [];
  if (depth === 0) {
    parentChildren = root.children.filter((c) => c.rank === null); // 同级（同根下的普通 tag）
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
  const name = prompt("新建子 tag 名");
  if (!name) return;
  try {
    const { id } = await client.call("create_tag", { parent_id: parentId, name });
    await setTags(page, page.tag_ids.concat(id));
    load().catch(() => {});
  } catch (e) { alert("创建失败 " + e.message); }
}

async function addTagToPage(page) {
  setPopup({
    x: 0, y: 0, items: allLeaves.map((n) => ({ label: n.path, id: n.id })),
    place: "add",
    cb: async (id) => { await setTags(page, page.tag_ids.concat(id)); },
  });
}

// 聚焦 + 悬停截图
let shotTimer = null;
function hover(page, on, e) {
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

  // 头部：上下文 + 排序 + 过滤
  const hdrRow = () => h("div.hdr-row", [
    h("select", { value: ctx, onChange: (e) => switchCtx(e.currentTarget.value) },
      () => h(For, { each: contexts() }, (c) => h("option", { value: c }, c))),
    h("button.b", { onClick: () => setSortNew(!sortNew()) }, () => sortNew() ? "新→旧" : "旧→新"),
    h("span.count", () => `${pageSet().length} 页`),
  ]);
  const filts = () => h("div.filts",
    () => h(For, { each: allLeaves }, (n) =>
      h("button", {
        class: () => "chip" + (filters().has(n.id) ? " on" : ""),
        onClick: () => toggleFilter(n.id),
      }, n.path)));

  // 胶囊切换菜单（x/y 由 setPopup 调用方给定）
  const popupMenu = () => {
    const p = popup();
    return h("div.menu", {
      onClick: (e) => e.stopPropagation(),
      style: `left:${p.x}px;top:${p.y}px;`,
    }, [
      p.place === "add" ? h("div.menu-title", "指派 tag") : null,
      h(For, { each: p.items }, (it) => h("button.menu-item", {
        onClick: () => { const cb = p.cb; setPopup(null); cb(it.id); },
      }, it.label)),
    ]);
  };

  // 悬停截图浮窗
  const shotBox = () => {
    const s = shot();
    return h("div.shot", {
      style: `left:${Math.min(s.x + 16, innerWidth - 340)}px;top:${Math.min(s.y + 16, innerHeight - 220)}px;`,
    }, h("img", { src: s.data, alt: "" }));
  };

  // 配置占位：只读展示 mudrad 生效配置（GET /config），编辑后续实现
  const [cfgText, setCfgText] = createSignal(null);
  const toggleCfg = () => {
    if (cfgText() !== null) { setCfgText(null); return; }
    fetch(`${MUDRAD_HTTP}/config`)
      .then((r) => r.json())
      .then(({ ok, config, err }) =>
        setCfgText(ok ? JSON.stringify(config, null, 2) : `加载失败: ${err}`))
      .catch((e) => setCfgText(`mudrad 不在线: ${e}`));
  };
  const cfgPane = () => h("div.cfg", [
    h("div.cfg-note", "配置编辑占位 — 当前生效值（~/.config/mudra/config.kdl，只读）："),
    h("pre.cfg-body", () => cfgText() ?? ""),
  ]);

  // 开新窗口：底部地址栏已移除，用扩展的 :open 命令（console 角色过滤 page，兜底开 URL）
  const [showCfg, setShowCfg] = createSignal(false);
  return h("div.panel", {
    onClick: () => popup() && popup().place === undefined && setPopup(null),
  }, () => [
    h("header.hdr", [
      hdrRow(),
      h("button.b", { onClick: () => { setShowCfg(!showCfg()); toggleCfg(); } },
        () => showCfg() ? "收起配置" : "配置"),
      filts(),
    ]),
    h(Show, { when: showCfg() }, cfgPane),
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
      // 行1：关闭/打开 + 删除 + 标题 + 链接 + 折叠
      h("div.row1", [
        h("button.act", {
          title: () => (p().closed ? "打开" : "关闭窗口"),
          onClick: (e) => {
            e.stopPropagation();
            client.call(p().closed ? "reopen" : "close", { page_id: p().id }).catch(() => {});
          },
        }, () => (p().closed ? "↻" : "⨯")),
        h("button.act.del", {
          title: "删除（仅关闭状态可用）",
          // 打开状态灰占位，点击无效果；删除为软删
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
      // 行2：rank + 普通胶囊 + 添加
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
  // 普通 tag 胶囊：每段一级路径，点击段切换同级；头✕删，尾＋加子级
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
