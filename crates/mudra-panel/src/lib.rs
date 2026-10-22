//! mudra-panel — the console UI as leptos CSR wasm (R2 slice 2).
//!
//! Two channels, one daemon: the bare `NestStorage` frame WS (:9300) is
//! the epoch bus — the ONLY push; a changed number re-pulls the shaped
//! reads. Every read that needs server-side tree shaping (/forest,
//! /ctx_pages, /shot) and every write rides mudrad's control HTTP
//! (:8899, `http` module). The frame channel stays a read-only KV data
//! plane by design (方案 A): a PUT frame would bypass the Collection's
//! index/epoch maintenance, and the retired bespoke WS op set must not
//! be revived (PLAN §11).
//!
//! Behavior spec: `frontend/ui/src/app.js` (the zero-build Solid panel)
//! at the retired Python tree's last commit (history). Local view
//! state (filters/collapsed/sortNew) rides okm's `LocalStorageStore`
//! (ADR-0028 consumer half, `uistate.rs`) — reload keeps it, the node
//! stays the only business-data owner.
//!
//! Three surfaces share the shell: the page tree (default), the event
//! log (`events.rs`, ns 8) and the hook list (`hooks.rs`, ns 9). Only
//! the tree has panel-visible state — both other collections are
//! epoch-silent, so those views carry their own re-read on top of the
//! shared epoch hint.
//!
//! Reactivity contract (the Solid lesson, in leptos terms): a re-pull
//! replaces the whole signal payload with fresh allocations, so `For`
//! keys on the `Arc` pointer identity make every row rebuild; row
//! closures therefore compute their content from the snapshot they were
//! built with (the JS panel's memo-rebuild did the same).

pub mod events;
pub mod hooks;
pub mod http;
pub mod remote;
pub mod schema;
pub mod uistate;

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::wasm_bindgen::JsCast;
use leptos::wasm_bindgen::closure::Closure;
use leptos::web_sys::HtmlElement;
use wasm_bindgen_futures::spawn_local;

use http::{PageInfo, TagNode};
use remote::WsLink;
use tag_forest::leptos as tf;

/// trunk entry point: render `App` into the document body (index.html
/// carries no wrapper element — the panel root is its own `.panel` div).
pub fn mount() {
    leptos::mount::mount_to_body(App);
}

type Pages = Arc<Vec<std::sync::Arc<PageInfo>>>;
type Roots = Arc<Vec<TagNode>>;
type IdMap = Arc<HashMap<u32, std::sync::Arc<TagNode>>>;
type Leaves = Arc<Vec<(u32, String)>>;
type Axes = Arc<Vec<Axis>>;

/// One row of the built page tree: the page, its nesting level, its
/// children (already filtered/sorted), whether its subtree is expanded.
#[derive(Clone)]
struct Node {
    p: std::sync::Arc<PageInfo>,
    kids: Vec<Node>,
    open: bool,
}

/// A rank root flattened for the per-row axis. The glyph rides the
/// crate's `root_axis` table through /forest's `rank_axis` field — the
/// panel never restates ROOT_AXIS. `children` are (rank, tag_id) pairs
/// so a pip click translates to a tag swap.
#[derive(Clone)]
struct Axis {
    name: String,
    alias: String,
    glyph: &'static str,
    children: Arc<Vec<(i32, u32)>>,
}

/// Capsule/popup menu payload (JS setPopup shapes). `place` marks the
/// "add tag" variant: it shows the "Assign tag" title and a background
/// click does NOT close it (the JS `place === undefined` rule).
#[derive(Clone)]
struct Menu {
    x: f64,
    y: f64,
    items: Arc<Vec<(u32, String)>>,
    place: bool,
    cb: Arc<dyn Fn(u32) + Send + Sync>,
}

/// Every shared setter/callback a page row needs. Clone-cheap by
/// construction (signals are Copy, callbacks Arc); recursion into child
/// rows just clones it. Callbacks take plain values (PageInfo/TagNode
/// by value) because leptos' reactive closures carry Send bounds even
/// on wasm — `Arc`/refs must not appear in the signatures.
#[derive(Clone)]
struct RowCtx {
    collapsed: WriteSignal<Arc<Vec<u64>>>,
    ui: crate::uistate::UiState,
    axes: ReadSignal<Axes>,
    by_id: ReadSignal<IdMap>,
    shot: WriteSignal<Option<(f64, f64, String)>>,
    thumbnails: ReadSignal<bool>,
    set_tags: Arc<dyn Fn(u64, Vec<u32>) + Send + Sync>,
    set_rank: Arc<dyn Fn(PageInfo, Arc<Axis>, i32) + Send + Sync>,
    open_seg: Arc<dyn Fn(PageInfo, TagNode, usize, f64, f64) + Send + Sync>,
    add_tag: Arc<dyn Fn(u64, Vec<u32>) + Send + Sync>,
    hover: Arc<dyn Fn(u64, f64, f64) + Send + Sync>,
}

/// Panel root: the frame link (epoch bus) + all view state.
#[component]
pub fn App() -> impl IntoView {
    let link = WsLink::from_location();
    // panel-local UI state on localStorage (ADR-0028 consumer half):
    // loads seed the signals, every write site persists its new value.
    // Stateless when the store is unavailable — same parity as the
    // thumbnails read (cache never breaks the view).
    let ui = uistate::UiState::open();

    // Text filter, two copies on purpose: `qtext` is what the box holds and
    // is applied on the client on every keystroke (no round trip), `qrun` is
    // what Enter committed to the daemon (the store's page recall). Typing
    // never changes which rows the server sent; Enter replaces them.
    let (qtext, set_qtext) = signal(String::new());
    let (qrun, set_qrun) = signal(String::new());
    let (roots, set_roots) = signal(Roots::default());
    let (by_id, set_by_id) = signal(IdMap::default());
    let (leaves, set_leaves) = signal(Leaves::default());
    let (axes, set_axes) = signal(Axes::default());
    let (pages, set_pages) = signal(Pages::default());

    let (filters, set_filters) = signal(Arc::new(ui.filters()));
    let (collapsed, set_collapsed) = signal(Arc::new(ui.collapsed()));
    let (menu, set_menu) = signal(None::<Menu>);
    let (shot, set_shot) = signal(None::<(f64, f64, String)>);
    let (thumbnails, set_thumbnails) = signal(false);
    // one nesting level of the page tree, em (config.kdl ui.indent)
    let (indent, set_indent) = signal(1.4_f64);
    // shared hover-shot timer state (JS shotTimer port): `generation` bumps on
    // every movement so an already-armed timeout becomes a stale no-op;
    // `pending` carries (page_id, x, y, generation) for the one callback.
    let (pending, set_pending) = signal(None::<(u64, f64, f64, u64)>);
    let (generation, set_generation) = signal(0u64);
    let (timer_id, set_timer_id) = signal(0i32);
    // the surface switch (page tree / event log / hook list) and the
    // epoch hint as a counter: the log and hook views re-read on it, and
    // neither brings a push channel of its own
    let (view, set_view) = signal("pages".to_string());
    let (tick, set_tick) = signal(0u64);

    // ---- load path ----
    // /forest: shaped tree + contexts + current in one round trip; the
    // derived indexes (JS indexTagTree's byId/allLeaves/rankRoots
    // triple) rebuild in the same pass.
    let do_forest = {
        let set_roots = set_roots;
        move |_: ()| {
            spawn_local(async move {
                match http::forest().await {
                    Ok(f) => {
                        set_roots.set(Arc::new(f.forest.clone()));
                        let mut map: HashMap<u32, Arc<TagNode>> = HashMap::new();
                        let mut plain: Vec<(u32, String)> = Vec::new();
                        fn walk(
                            n: &TagNode,
                            map: &mut HashMap<u32, Arc<TagNode>>,
                            plain: &mut Vec<(u32, String)>,
                        ) {
                            map.insert(n.id, Arc::new(n.clone()));
                            if !n.root
                                && n.rank.is_none()
                                && n.children.is_empty()
                                && n.path.is_some()
                            {
                                plain.push((n.id, n.path.clone().unwrap()));
                            }
                            for c in &n.children {
                                walk(c, map, plain);
                            }
                        }
                        let mut axs: Vec<Axis> = Vec::new();
                        for r in &f.forest {
                            walk(r, &mut map, &mut plain);
                            if let Some(g) =
                                r.rank_axis.as_deref().and_then(tag_forest::root_axis)
                            {
                                let children: Vec<(i32, u32)> = r
                                    .children
                                    .iter()
                                    .filter_map(|c| c.rank.map(|k| (k, c.id)))
                                    .collect();
                                axs.push(Axis {
                                    name: r.name.clone(),
                                    alias: r.alias.clone(),
                                    glyph: g,
                                    children: Arc::new(children),
                                });
                            }
                        }
                        set_by_id.update(|m| *m = Arc::new(map));
                        set_leaves.update(|l| *l = Arc::new(plain));
                        set_axes.update(|a| *a = Arc::new(axs));
                    }
                    Err(e) => web_sys::console::warn_1(&format!("forest: {e}").into()),
                }
            });
        }
    };
    let do_pages = move || {
        let q = qrun.get_untracked();
        spawn_local(async move {
            match http::pages(&q).await {
                Ok(r) => set_pages.update(|p| {
                    *p = Arc::new(r.pages.into_iter().map(Arc::new).collect())
                }),
                Err(e) => web_sys::console::warn_1(&format!("pages: {e}").into()),
            }
        });
    };
    do_forest(());

    // the first pull, and every Enter afterwards: the effect watches the
    // COMMITTED query (`qrun`), not the box's text — typing must stay local
    {
        let last = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
        Effect::new(move || {
            let q = qrun.get();
            let mut l = last.borrow_mut();
            if l.as_deref() != Some(q.as_str()) {
                *l = Some(q);
                do_pages();
            }
        });
    }

    // the epoch hint is the ONLY push: re-pull both shaped reads (the
    // JS re-fetched forest on connect and pages on `pages_changed`;
    // one hint covers the pair now, and /ctx round trips come back
    // through it too)
    link.on_epoch({
        move |_: u64| {
            do_forest(());
            do_pages();
            // the log and hook views read the hint as a counter (their
            // collections never bump the epoch themselves)
            set_tick.update(|t| *t += 1);
        }
    });
    // keep the link's Rc alive for the page's lifetime. Every WS closure
    // holds only a Weak<WsLink> (and the epoch listener lives *inside*
    // the link), so without a strong owner App()'s local drops last and
    // the upgrade returns None forever: epoch frames arrive but never
    // fan out, and the reconnect chain dies too. The same leaked-for-
    // page-lifetime policy the socket closure sets already follow;
    // forgetting one strong count buys the whole bus back.
    std::mem::forget(Rc::clone(&link));

    // hover-screenshot toggle: one-shot read of config.kdl; any failure
    // reads as disabled (Python parity: no config, no shots)
    spawn_local(async move {
        let ui = http::ui().await;
        set_thumbnails.set(ui.thumbnails);
        set_indent.set(ui.indent);
    });

    // ---- derived page tree ----
    let (tree, set_tree) = signal(Vec::<Node>::new());
    // how many rows survive the filters (the count in the bar shows this
    // against the loaded total, so a filter that hides everything says so)
    let (shown, set_shown) = signal(0usize);
    {
        let (pages, filters, collapsed, qtext) = (pages, filters, collapsed, qtext);
        Effect::new(move || {
            let list = pages.get();
            let fl = filters.get();
            let coll = collapsed.get();
            let text = qtext.get().trim().to_lowercase();
            // one visibility rule, applied to tags AND text; the text reads
            // the same three fields the bar's `:open` filter matches on
            let hit = |p: &PageInfo| {
                (fl.is_empty() || fl.iter().all(|t| p.tag_ids.contains(t)))
                    && (text.is_empty()
                        || format!("{} {} {}", p.title, p.url, p.ctx)
                            .to_lowercase()
                            .contains(&text))
            };
            let visible: Vec<u64> = list.iter().filter(|p| hit(p)).map(|p| p.id).collect();
            fn build(pid: u64, list: &Pages, vis: &[u64], coll: &[u64]) -> Vec<Node> {
                let mut rows: Vec<Arc<PageInfo>> = list
                    .iter()
                    .filter(|p| vis.contains(&p.id))
                    .filter(|p| {
                        let mut parent = p.parent_id;
                        if parent == 0 || !vis.contains(&parent) {
                            // a filtered-out ancestor must not take its match
                            // with it: the surviving row becomes a root
                            parent = 0;
                        }
                        parent == pid
                    })
                    .cloned()
                    .collect();
                // newest first, always — the sort toggle is gone
                rows.sort_by(|a, b| b.opened_at.cmp(&a.opened_at));
                rows.into_iter()
                    .map(|p| {
                        let kids = build(p.id, list, vis, coll);
                        Node { open: !coll.contains(&p.id), p, kids }
                    })
                    .collect()
            }
            set_shown.update(|n| *n = visible.len());
            set_tree.update(|t| *t = build(0, &list, &visible, &coll));
        });
    }

    // ---- actions (all write verbs; the epoch hint brings data back) ----
    // setTags: whole-set replace (rank picks, capsule switches,
    // add/remove all land here — the JS setTags)
    let set_tags: Arc<dyn Fn(u64, Vec<u32>) + Send + Sync> =
        Arc::new(move |page_id: u64, ids: Vec<u32>| {
            spawn_local(async move {
                if let Err(e) = http::verb(
                    "/set_tags",
                    serde_json::json!({"page_id": page_id, "tag_ids": ids}),
                )
                .await
                {
                    web_sys::console::warn_1(&format!("set_tags: {e}").into());
                }
            });
        });
    // setRank (JS setRank): drop this axis's children from the set, add
    // the rank-k one (k=0 clears the axis — the click-the-same-pip rule
    // the pips carry through the host)
    let set_rank: Arc<dyn Fn(PageInfo, Arc<Axis>, i32) + Send + Sync> = {
        let set_tags = Arc::clone(&set_tags);
        Arc::new(move |p: PageInfo, axis: Arc<Axis>, k: i32| {
            let others: Vec<u32> = axis.children.iter().map(|(_, id)| *id).collect();
            let mut next: Vec<u32> =
                p.tag_ids.iter().filter(|t| !others.contains(t)).cloned().collect();
            if k > 0
                && let Some((_, id)) = axis.children.iter().find(|(r, _)| *r == k)
            {
                next.push(*id);
            }
            set_tags(p.id, next);
        })
    };
    // segment click: sibling-switch menu (JS openSegMenu's drill by the
    // path); x/y come from the clicked segment's client rect
    let open_seg: Arc<dyn Fn(PageInfo, TagNode, usize, f64, f64) + Send + Sync> = {
        let (roots, set_menu, set_tags) = (roots, set_menu, Arc::clone(&set_tags));
        Arc::new(move |p: PageInfo, node: TagNode, depth: usize, x: f64, y: f64| {
            // clone the handle per call: this closure is `Fn`, the menu
            // callback below owns its own clone
            let set_tags = Arc::clone(&set_tags);
            let parts: Vec<String> = node
                .path
                .clone()
                .unwrap_or_default()
                .split("::")
                .map(str::to_string)
                .collect();
            let root = match roots.get().iter().find(|r| r.name == parts[0]) {
                Some(r) => r.clone(),
                None => return,
            };
            let mut cursor = root;
            for seg in parts.iter().skip(1).take(depth) {
                match cursor.children.iter().find(|c| &c.name == seg) {
                    Some(c) => cursor = c.clone(),
                    None => return,
                }
            }
            let siblings: Vec<(u32, String)> = cursor
                .children
                .iter()
                .filter(|c| c.rank.is_none())
                .filter_map(|c| c.path.clone().map(|pa| (c.id, pa)))
                .collect();
            let tag_id = node.id;
            let cur = p.tag_ids.clone();
            let page_id = p.id;
            set_menu.set(Some(Menu {
                x,
                y,
                items: Arc::new(siblings),
                place: false,
                cb: Arc::new(move |id: u32| {
                    let mut ids: Vec<u32> =
                        cur.iter().copied().filter(|t| *t != tag_id).collect();
                    ids.push(id);
                    set_tags(page_id, ids);
                }),
            }));
        })
    };
    // ＋ on the row: add-tag popup over every plain leaf (JS addTagToPage)
    let add_tag: Arc<dyn Fn(u64, Vec<u32>) + Send + Sync> = {
        let (leaves, set_menu, set_tags) = (leaves, set_menu, Arc::clone(&set_tags));
        Arc::new(move |page_id: u64, cur: Vec<u32>| {
            let set_tags = Arc::clone(&set_tags);
            let items = (*leaves.get()).clone();
            set_menu.set(Some(Menu {
                x: 0.0,
                y: 0.0,
                items: Arc::new(items),
                place: true,
                cb: Arc::new(move |id: u32| {
                    let mut ids = cur.clone();
                    ids.push(id);
                    set_tags(page_id, ids);
                }),
            }));
        })
    };

    // ---- the shared hover-shot timer ----
    // one long-lived callback (leaked once by design — one page, one
    // timer); movement bumps `generation` and rearms the 250ms window timeout,
    // so a shot only fires on a still pointer (the JS shotTimer rule,
    // which also bounds captureScreenshot volume)
    let fire = Closure::<dyn Fn()>::new(move || {
        let Some((pid, x, y, g)) = pending.get_untracked() else { return };
        if g != generation.get_untracked() {
            return; // superseded by a newer hover
        }
        spawn_local(async move {
            match http::shot(pid).await {
                Ok(Some(data)) => set_shot.set(Some((x, y, data))),
                Ok(None) => {}
                Err(e) => web_sys::console::warn_1(&format!("shot: {e}").into()),
            }
        });
    });
    let fire_fn: js_sys::Function = fire.as_ref().unchecked_ref::<js_sys::Function>().clone();
    fire.forget();
    let hover: Arc<dyn Fn(u64, f64, f64) + Send + Sync> = {
        let (set_pending, generation, set_generation, timer_id, set_timer_id, fire_fn) = (
            set_pending, generation, set_generation, timer_id, set_timer_id, fire_fn,
        );
        Arc::new(move |pid: u64, x: f64, y: f64| {
            let Some(win) = web_sys::window() else { return };
            set_generation.update(|g| *g += 1);
            let g = generation.get();
            set_pending.set(Some((pid, x, y, g)));
            let last = timer_id.get();
            if last != 0 {
                win.clear_timeout_with_handle(last);
            }
            if let Ok(id) =
                win.set_timeout_with_callback_and_timeout_and_arguments_0(&fire_fn, 250)
            {
                set_timer_id.set(id);
            }
        })
    };

    let cx = RowCtx {
        collapsed: set_collapsed,
        ui: ui.clone(),
        axes,
        by_id,
        shot: set_shot,
        thumbnails,
        set_tags: Arc::clone(&set_tags),
        set_rank: Arc::clone(&set_rank),
        open_seg: Arc::clone(&open_seg),
        add_tag: Arc::clone(&add_tag),
        hover,
    };

    // header: one row. The surface switch, the tree's own controls (context
    // select, sort toggle, count) and the filter chips share the line — the
    // chips used to get a row of their own, which is a whole line of a
    // 16px-tall bar spent on nothing.
    // The surface switch is a left sidebar: three words that never change, so
    // they cost a column instead of a row of the working area. Their text runs
    // bottom-to-top (`vertical-lr` turned 180°) — the direction a sheet fed
    // into a printer reads, not the one a book spine does.
    let side = view! {
        <nav class="side">
            <ViewTab name="pages" label="pages" view=view set_view=set_view />
            <ViewTab name=events::VIEW label="events" view=view set_view=set_view />
            <ViewTab name=hooks::VIEW label="hooks" view=view set_view=set_view />
        </nav>
    };

    // the filter row belongs to the tree; the other two surfaces carry
    // their own header inside their view, so an empty strip here would be
    // a border with nothing under it
    let hdr = view! {
        <Show when=move || view.get() == "pages" fallback=|| ()>
            <header class="hdr">
                {
                    // the tree's controls need the panel-state handle by
                    // value in their own closures; Show's children are
                    // `Fn` (rebuilt on every mount), so each mount takes a
                    // fresh clone instead of moving the component's
                    let ui = ui.clone();
                    view! {
                        <div class="hdr-row filters">
                        <input
                            class="q"
                            type="text"
                            placeholder="filter — Enter searches the store"
                            prop:value=move || qtext.get()
                            on:input={
                                move |ev: leptos::ev::Event| {
                                    if let Some(t) = ev
                                        .target()
                                        .and_then(|t| t.dyn_ref::<web_sys::HtmlInputElement>().cloned())
                                    {
                                        set_qtext.set(t.value());
                                    }
                                }
                            }
                            on:keydown={
                                move |ev: leptos::ev::KeyboardEvent| match ev.key().as_str() {
                                    // Enter commits the text to the daemon: the store's
                                    // recall rebuilds the rows (typing above only
                                    // filtered what was already loaded)
                                    "Enter" => set_qrun.set(qtext.get_untracked()),
                                    // Escape drops the filter on both sides
                                    "Escape" => {
                                        set_qtext.set(String::new());
                                        set_qrun.set(String::new());
                                    }
                                    _ => {}
                                }
                            }
                        />
                        <span class="count">
                            {move || format!("{} / {} pages", shown.get(), pages.get().len())}
                        </span>
                        <div class="filts">
                            <For
                                each=move || (*leaves.get()).clone()
                                key=|(id, _)| *id
                                children=move |(id, path): (u32, String)| {
                                    let on = move || filters.get().contains(&id);
                                    let onclick: tf::VoidCb = {
                                        let ui = ui.clone();
                                        Arc::new(move || {
                                            let mut next: Vec<u32> = Vec::new();
                                            set_filters.update(|f| {
                                                let mut n: Vec<u32> = (**f).clone();
                                                match n.iter().position(|t| *t == id) {
                                                    Some(i) => {
                                                        n.remove(i);
                                                    }
                                                    None => n.push(id),
                                                }
                                                next = n.clone();
                                                *f = Arc::new(n);
                                            });
                                            ui.save_filters(&next);
                                        })
                                    };
                                    tf::chip(path, on, onclick)
                                }
                            />
                        </div>
                        </div>
                    }
                }
            </header>
        </Show>
    };

    // main: one surface at a time. The tree's `For` keeps the Arc
    // pointer-identity key (every re-pull replaces the signal with fresh
    // allocations, so changed rows rebuild — the Solid memo-rebuild
    // semantics the JS panel relied on); the other two views are
    // components of their own, so this file stays the shell.
    let main = view! {
        <main class="tree">
            <Show when=move || view.get() == "pages" fallback=|| ()>
                <For
                    each=move || tree.get()
                    key=|n| Arc::as_ptr(&n.p) as usize
                    children={
                        let cx = cx.clone();
                        move |nd: Node| page_node(nd, cx.clone())
                    }
                />
            </Show>
            <Show when=move || view.get() == events::VIEW fallback=|| ()>
                <events::EventsView view=view tick=tick />
            </Show>
            <Show when=move || view.get() == hooks::VIEW fallback=|| ()>
                <hooks::HooksView view=view tick=tick />
            </Show>
        </main>
    };

    // popups: segment/add menu + hover screenshot
    let popups = view! {
        <Show when=move || menu.get().is_some() fallback=|| ()>
            <div
                class="menu"
                style=move || {
                    menu.get()
                        .map(|m| format!("left:{}px;top:{}px", m.x, m.y))
                        .unwrap_or_default()
                }
                on:click=|e: leptos::ev::MouseEvent| e.stop_propagation()
            >
                <Show
                    when=move || menu.get().is_some_and(|m| m.place)
                    fallback=|| ()
                >
                    <div class="menu-title">"Assign tag"</div>
                </Show>
                <For
                    each=move || {
                        menu.get()
                            .map(|m| (*m.items).clone())
                            .unwrap_or_default()
                    }
                    key=|(id, _)| *id
                    children=move |(id, label): (u32, String)| view! {
                        <button class="menu-item" on:click={move |_| {
                            if let Some(m) = menu.get_untracked() {
                                (m.cb)(id);
                            }
                            set_menu.set(None);
                        }}>{label}</button>
                    }
                />
            </div>
        </Show>
        <Show when=move || shot.get().is_some() fallback=|| ()>
            <div class="shot" style=move || {
                let (x, y, _) = shot.get().unwrap_or((0.0, 0.0, String::new()));
                let (iw, ih) = web_sys::window()
                    .map(|w| {
                        (
                            w.inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(800.0),
                            w.inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(600.0),
                        )
                    })
                    .unwrap_or((800.0, 600.0));
                format!(
                    "left:{}px;top:{}px",
                    (x + 16.0).min(iw - 340.0),
                    (y + 16.0).min(ih - 220.0)
                )
            }>
                <img
                    src=move || shot.get().map(|(_, _, d)| d).unwrap_or_default()
                    alt=""
                />
            </div>
        </Show>
    };

    view! {
        <div class="panel"
            style=move || format!("--indent:{}em", indent.get())
            on:click=move |_| {
            // JS rule: a background click closes the segment menu; the
            // place-flagged add menu stays open until a pick
            set_menu.update(|m| {
                if m.as_ref().is_some_and(|m| !m.place) {
                    *m = None;
                }
            });
        }>
            {side}
            <div class="col">
                {hdr}
                {main}
            </div>
            {popups}
        </div>
    }
}

/// One surface-switch button. `name` is the view key (`events::VIEW` /
/// `hooks::VIEW` keep the tabs and the branches on one spelling), `label`
/// is what the operator reads.
#[component]
fn ViewTab(
    name: &'static str,
    label: &'static str,
    view: ReadSignal<String>,
    set_view: WriteSignal<String>,
) -> impl IntoView {
    view! {
        <button
            class=move || if view.get() == name { "b v on" } else { "b v" }
            on:click=move |_| set_view.set(name.to_string())
        >{label}</button>
    }
}

/// One page row (JS `PageNode` port). Returns `AnyView`: recursion with
/// `impl IntoView` cannot name its own type; boxed views break the cycle.
fn page_node(nd: Node, cx: RowCtx) -> AnyView {
    let p = Arc::clone(&nd.p);
    let closed = p.closed;
    let page_id = p.id;
    let cur_tags = std::sync::Arc::new(p.tag_ids.clone());
    let page = (*p).clone();
    let node_cls = if closed { "node closed" } else { "node" };

    // ⨯/↻ — window state toggle from this row's snapshot (the re-pull
    // rebuilds the row when `closed` flips)
    let toggle = view! {
        <button class="act"
            title={if closed { "Open" } else { "Close window" }}
            on:click={move |e: leptos::ev::MouseEvent| {
                e.stop_propagation();
                let path = if closed { "/reopen" } else { "/close" };
                let body = serde_json::json!({"page_id": page_id});
                spawn_local(async move {
                    let _ = http::verb(path, body).await;
                });
            }}
        >{if closed { "↻" } else { "⨯" }}</button>
    };
    // 🗑 — grey placeholder while open (JS disabled rule)
    let del = view! {
        <button class="act del"
            title="Delete (only while closed)"
            disabled=!closed
            on:click={move |e: leptos::ev::MouseEvent| {
                e.stop_propagation();
                if closed {
                    let body = serde_json::json!({"page_id": page_id});
                    spawn_local(async move {
                        let _ = http::verb("/delete", body).await;
                    });
                }
            }}
        >"🗑"</button>
    };
    // ▾/▸ collapse twisty (local UI state: persisted to localStorage,
    // never to the store — the node owns business data, the panel owns
    // its own cache, ADR-0028's boundary)
    let tw = if nd.kids.is_empty() { "" } else if nd.open { "▾" } else { "▸" };
    let on_tw = {
        let collapsed = cx.collapsed; // Copy: no cx partial move
        let ui = cx.ui.clone();
        move |_| {
            let mut next: Vec<u64> = Vec::new();
            collapsed.update(|c| {
                let mut n: Vec<u64> = (**c).clone();
                match n.iter().position(|x| *x == page_id) {
                    Some(i) => {
                        n.remove(i);
                    }
                    None => n.push(page_id),
                }
                next = n.clone();
                *c = Arc::new(n);
            });
            ui.save_collapsed(&next);
        }
    };
    let twbtn = view! { <button class="tw" on:click=on_tw>{tw}</button> };
    // title link: focus verb + hover-shot wiring (JS onMouseMove/Leave)
    let title = p.title.clone();
    let hover_cb = Arc::clone(&cx.hover);
    let thumbs = cx.thumbnails;
    let shot_sig = cx.shot;
    let url = p.url.clone();
    let link = view! {
        <a class="link" href=url
            on:click={move |e: leptos::ev::MouseEvent| {
                e.prevent_default();
                let body = serde_json::json!({"page_id": page_id});
                spawn_local(async move {
                    let _ = http::verb("/focus_page", body).await;
                });
            }}
            on:mousemove={move |e: leptos::ev::MouseEvent| {
                if !thumbs.get_untracked() {
                    return; // ui.thumbnails=false: no request, no render
                }
                hover_cb(page_id, e.client_x() as f64, e.client_y() as f64);
            }}
            on:mouseleave={move |_| shot_sig.set(None)}
        >{title}</a>
    };
    let meta = view! { <span class="meta">{time_ago(p.opened_at)}</span> };

    // row-2: one rank axis per rank root + one capsule per plain tag + ＋
    let axes_row = cx.axes;
    let by_id_row = cx.by_id;
    let row2 = view! {
        <div class="tags">
            <For
                each=move || (*axes_row.get()).clone()
                key=|a| a.name.clone()
                children={
                    let page = page.clone();
                    let tags = cur_tags.clone();
                    let set_rank = Arc::clone(&cx.set_rank);
                    move |axis: Axis| {
                        let axis = Arc::new(axis);
                        // JS rankSel: the page tag that is one of this
                        // axis's children; its rank lights the pips
                        let sel = axis
                            .children
                            .iter()
                            .find(|(_, id)| tags.contains(id))
                            .map(|(k, _)| *k)
                            .unwrap_or(0);
                        let page2 = page.clone();
                        let axis2 = Arc::clone(&axis);
                        let set_rank = Arc::clone(&set_rank);
                        let on_pick: tf::PickCb =
                            Arc::new(move |k: i32| {
                                // same pip clicked while selected: k ->
                                // rank-0 clear (the JS onPick passes the
                                // toggled value; re-clicking the current
                                // level empties the axis)
                                let next = if sel == k && k != 0 { 0 } else { k };
                                set_rank(page2.clone(), Arc::clone(&axis2), next);
                            });
                        tf::rank_axis(
                            axis.name.clone(),
                            axis.alias.clone(),
                            axis.glyph,
                            move || sel,
                            on_pick,
                        )
                    }
                }
            />
            <For
                each={
                    let cur_tags = std::sync::Arc::clone(&cur_tags);
                    move || {
                    let m = by_id_row.get();
                    cur_tags
                        .iter()
                        .filter_map(|id| m.get(id).cloned())
                        // JS normalTags: plain tags only (no root, no rank)
                        .filter(|n| !n.root && n.rank.is_none())
                        .collect::<Vec<Arc<TagNode>>>()
                    }
                }
                key=|n| n.id
                children={
                    let page = page.clone();
                    let open_seg = Arc::clone(&cx.open_seg);
                    let set_tags = Arc::clone(&cx.set_tags);
                    let cur_tags = Arc::clone(&cur_tags);
                    move |t: Arc<TagNode>| {
                        // per-item clones: the outer closure is Fn, so
                        // captured Arcs/Vecs must be cloned into each
                        // callback rather than moved
                        let _ = &cur_tags;
                        let open_seg = Arc::clone(&open_seg);
                        let set_tags = Arc::clone(&set_tags);
                        let path = t.path.clone().unwrap_or_default();
                        let tag = (*t).clone();
                        let page2 = page.clone();
                        let page3 = page.clone();
                        let ids = page.tag_ids.clone();
                        let ids2 = page.tag_ids.clone();
                        let on_seg: tf::SegCb = {
                            let tag2 = tag.clone();
                            Arc::new(move |depth: usize, el: &HtmlElement| {
                                let r = el.get_bounding_client_rect();
                                open_seg(page2.clone(), tag2.clone(), depth, r.left(), r.bottom());
                            })
                        };
                        let on_remove: tf::VoidCb = {
                            let tid = tag.id;
                            Arc::new(move || {
                                let next: Vec<u32> =
                                    ids.iter().copied().filter(|x| *x != tid).collect();
                                set_tags(page3.id, next);
                            })
                        };
                        // addChild (JS addChild): prompt -> create_tag
                        // -> append the new id (the epoch hint re-pulls)
                        let on_add_child: tf::VoidCb = {
                            let page4 = page.clone();
                            let ids4 = std::sync::Arc::new(ids2.clone());
                            let tid = tag.id;
                            Arc::new(move || {
                                let Some(name) = web_sys::window()
                                    .and_then(|w| {
                                        w.prompt_with_message_and_default(
                                            "New child tag name",
                                            "",
                                        )
                                        .ok()
                                    })
                                    .flatten()
                                else {
                                    return;
                                };
                                let page5 = page4.clone();
                                let ids6 = Arc::clone(&ids4);
                                spawn_local(async move {
                                    match http::create_tag(tid, &name).await {
                                        Ok(id) => {
                                            let mut next = (*ids6).clone();
                                            next.push(id);
                                            let _ = http::verb(
                                                "/set_tags",
                                                serde_json::json!({
                                                    "page_id": page5.id,
                                                    "tag_ids": next,
                                                }),
                                            )
                                            .await;
                                        }
                                        Err(e) => web_sys::console::warn_1(
                                            &format!("create_tag: {e}").into(),
                                        ),
                                    }
                                });
                            })
                        };
                        tf::capsule(path, on_seg, Some(on_remove), Some(on_add_child))
                    }
                }
            />
            <button class="b add" on:click={
                let add_tag = Arc::clone(&cx.add_tag);
                let cur_tags = Arc::clone(&cur_tags);
                move |e: leptos::ev::MouseEvent| {
                    e.stop_propagation();
                    (add_tag)(page_id, (*cur_tags).clone());
                }
            }>"＋"</button>
        </div>
    };

    let kids = nd.kids.clone();
    // One container per page, children nested inside it: a page's kids sit in
    // their own `.kids` block, so the indent is the nesting itself (each level
    // adds one `--indent`, config.kdl `ui.indent`, em) instead of a level
    // number multiplied by a hardcoded pixel step.
    view! {
        <div class=node_cls>
                <div class="ncard">
                // the page's tags live on the link's own line: a page is one
                // line (its `＋` under the title read as a second page)
                <div class="row1">{toggle}{del}{twbtn}{link}{meta}{row2}</div>
            </div>
            <div class="kids">
                <For
                    each=move || kids.clone()
                    key=|n| Arc::as_ptr(&n.p) as usize
                    children={
                        let cx = cx.clone();
                        move |k: Node| page_node(k, cx.clone())
                    }
                />
            </div>
        </div>
    }
    .into_any()
}

pub(crate) fn time_ago(millis: u64) -> String {
    if millis == 0 {
        return String::new();
    }
    let now = js_sys::Date::now() as u64;
    let s = now.saturating_sub(millis) / 1000;
    if s < 60 {
        "just now".into()
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 86400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86400)
    }
}
