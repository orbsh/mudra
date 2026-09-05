import { render } from "solid-js/web";
import { createSignal, createMemo, createEffect, For, Show } from "solid-js";
import "./styles.css";

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
const [sessions, setSessions] = createSignal([]);
const [session, setSession] = createSignal("");
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
  setSessions(r.sessions);
  indexTagTree(r.forest);
  if (!session() && r.sessions.length) setSession(r.sessions[0].name);
}
async function loadPages() {
  if (!session()) { setPages([]); return; }
  const r = await client.call("pages", { session: session() });
  setPages(r.pages);
}
createEffect(() => {
  indexTagTree(roots());
  loadPages();
});
load().catch((e) => console.warn(e));

// 服务端推送：mudrad 在 page 集变化（新开/关闭/标题更新）时广播 pages_changed，
// 前端收到后重取当前会话页面——不做轮询。
client.onEvent = (ev) => {
  if (ev.event === "pages_changed") loadPages().catch(() => {});
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
  let parent = null;
  let parentChildren = [];
  // 定位 depth 段的父节点的直接子
  // path: r::a::b, depth 0 segment = a (根下), depth 1 = b
  let cur = parts[0];
  const root = roots().find((r) => r.name === cur);
  if (!root) return;
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

  return (
    <div class="panel" onClick={() => popup() && popup().place === undefined && setPopup(null)}>
      {/* 头部：会话 + 排序 + 过滤 */}
      <header class="hdr">
        <div class="hdr-row">
          <select value={session()} onChange={(e) => setSession(e.currentTarget.value)}>
            <For each={sessions()}>{(s) => <option value={s.name}>{s.name}</option>}</For>
          </select>
          <button class="b" onClick={() => setSortNew(!sortNew())}>{sortNew() ? "新→旧" : "旧→新"}</button>
          <span class="count">{pageSet().length} 页</span>
        </div>
        <div class="filts">
          <For each={allLeaves}>
            {(n) => (
              <button class={"chip" + (filters().has(n.id) ? " on" : "")}
                      onClick={() => toggleFilter(n.id)}>{n.path}</button>
            )}
          </For>
        </div>
      </header>

      <main class="tree">
        <For each={pageTree()}>
          {(nd) => <PageNode nd={nd} />}
        </For>
      </main>

      <Show when={popup()}>
        <div class="menu" onClick={(e) => e.stopPropagation()}
             style={`left:${popup().x}px;top:${popup().y}px;`}>
          {popup().place === "add" ? <div class="menu-title">指派 tag</div> : null}
          <For each={popup().items}>
            {(it) => <button class="menu-item" onClick={() => { const cb = popup().cb; setPopup(null); cb(it.id); }}>{it.label}</button>}
          </For>
        </div>
      </Show>

      <Show when={shot()}>
        <div class="shot"
             style={`left:${Math.min(shot().x + 16, innerWidth - 340)}px;top:${Math.min(shot().y + 16, innerHeight - 220)}px;`}>
          <img src={shot().data} alt="" />
        </div>
      </Show>
    </div>
  );
}

function PageNode(props) {
  const { p, lvl, kids, open } = props.nd;
  return (
    <div class="node">
      <div class="ncard" style={{ "margin-left": `${lvl * 16}px` }}>
        {/* 行1：标题 + 链接 + 折叠 */}
        <div class="row1">
          <button class="tw" onClick={() => setCollapsed((s) => {
            const n = new Set(s); n.has(p.id) ? n.delete(p.id) : n.add(p.id); return n;
          })}>
            {kids.some((k) => !filters().size) || kids.length > 0 ? (open && kids.length ? "▾" : "▸") : ""}
          </button>
          <a class="link"
             href={p.url}
             onClick={(e) => { e.preventDefault(); client.call("focus", { page_id: p.id }).catch(() => {}); }}
             onMouseMove={(e) => hover(p, true, e)}
             onMouseLeave={() => hover(p, false)}>
            {p.title}
          </a>
          <span class="meta">{timeAgo(p.opened_at)}</span>
        </div>
        {/* 行2：rank + 普通胶囊 + 添加 */}
        <div class="row2">
          <For each={rankRoots}>
            {(root) => <RankAxis page={p} root={root} />}
          </For>
          <For each={normalTags(p)}>
            {(t) => <Capsule page={p} t={t} />}
          </For>
          <button class="b add" onClick={(e) => { e.stopPropagation(); addTagToPage(p); }}>＋</button>
        </div>
      </div>
      <For each={kids}>{(k) => <PageNode nd={k} />}</For>
    </div>
  );
}

function RankAxis(props) {
  const { page, root } = props;
  const sel = rankSel(page, root);
  const n = sel ? sel.rank : 0;
  return (
    <span class="rank" title={root.name + (root.alias ? "（" + root.alias + "）" : "")}>
      {[1, 2, 3, 4, 5].map((k) => (
        <span class={"rk" + (k <= n ? " on" : "")} onClick={() => setRank(page, root, k)}>{root.rank_axis}</span>
      ))}
    </span>
  );
}

function Capsule(props) {
  const { page, t } = props;
  const parts = t.path.split("::");
  // 普通 tag 胶囊：每段一级路径，点击段切换同级；头✕删，尾＋加子级
  return (
    <span class="capsule" onClick={(e) => e.stopPropagation()}>
      <span class="cp-x" title="删除此标签" onClick={() => setTags(page, page.tag_ids.filter((x) => x !== t.id))}>✕</span>
      <For each={parts}>
        {(seg, i) => (
          <span class={"seg" + (i() === parts.length - 1 ? " leaf" : "")}
                onClick={(e) => {
                  const el = e.currentTarget.getBoundingClientRect();
                  // 打开同级切换（在 depth=i() 的父下）
                  openSegMenu(page, t, i(), el);
                }}>
            {seg}
          </span>
        )}
      </For>
      <span class="cp-close" title="添加子级" onClick={() => addChild(page, t.id)}>＋</span>
    </span>
  );
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

render(() => <App />, document.getElementById("root"));