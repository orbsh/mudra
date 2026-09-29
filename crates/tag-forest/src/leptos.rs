//! Interactive tag-forest components for the leptos panel — the same
//! shapes the SSR functions render, with live callbacks instead of
//! static strings. Single source stays true two ways: the class grammar
//! (`capsule/seg/leaf/cp-x/cp-close`, `chip`, `rk`, `rank`) matches the
//! SSR byte contract and the shared styles.css, and the glyph table
//! (`root_axis`) is the crate's, not the panel's.
//!
//! Callbacks are `Arc<dyn Fn… + Send + Sync>`: leptos 0.8 bounds its
//! reactive closures (attribute getters, For each/children) with
//! Send+Sync even on wasm csr — the panel's signal payloads must be
//! shareable, so plain Rc would not compile inside a reactive tree.
//! The crate compiles this module only under the `leptos` feature —
//! mudrad's native SSR path never links leptos.

use std::sync::Arc;

use leptos::prelude::*;
use leptos::wasm_bindgen::JsCast;
use leptos::web_sys::HtmlElement;

/// Click a path segment: index + the clicked element (the host measures
/// it for menu placement — the port of openSegMenu's
/// `getBoundingClientRect`).
pub type SegCb = Arc<dyn Fn(usize, &HtmlElement) + Send + Sync>;
/// A fire-and-forget affordance (✕ remove, ＋ add child, chip toggle).
pub type VoidCb = Arc<dyn Fn() + Send + Sync>;
/// Rank pip clicked: 1..=5 (the host maps k -> tag, the crate does not
/// know the children).
pub type PickCb = Arc<dyn Fn(i32) + Send + Sync>;

/// Interactive capsule: leading ✕ (when a callback is given), one
/// clickable `span.seg` per path level (last also `leaf`), trailing ＋
/// (when given). Clicks stop propagation like the JS original, so row
/// handlers don't fire through the capsule.
pub fn capsule(path: String, on_seg: SegCb, on_remove: Option<VoidCb>, on_add_child: Option<VoidCb>) -> impl IntoView {
    let segs: Vec<String> = crate::TagPath(&path).segments().map(str::to_string).collect();
    let last = segs.len().saturating_sub(1);

    let remove = on_remove.map(|cb| {
        move |_: leptos::ev::MouseEvent| cb()
    });
    let add = on_add_child.map(|cb| {
        move |_: leptos::ev::MouseEvent| cb()
    });

    view! {
        <span class="capsule" on:click=|e: leptos::ev::MouseEvent| {
            // stop like tags.js: the row's own click must not fire
            e.stop_propagation();
        }>
            {remove.map(|cb| view! { <span class="cp-x" title="Delete this tag" on:click=cb>"✕"</span> })}
            {segs
                .iter()
                .enumerate()
                .map(|(i, seg)| {
                    let cls = if i == last { "seg leaf" } else { "seg" };
                    let seg = seg.clone();
                    let on_seg = Arc::clone(&on_seg);
                    view! {
                        <span class=cls on:click={move |e: leptos::ev::MouseEvent| {
                            if let Some(el) = e.current_target().and_then(|t| t.dyn_ref::<HtmlElement>().cloned()) {
                                (on_seg)(i, &el);
                            }
                        }}>{seg}</span>
                    }
                })
                .collect_view()}
            {add.map(|cb| view! { <span class="cp-close" title="Add child" on:click=cb>"＋"</span> })}
        </span>
    }
}

/// One selectable filter chip: the `on` state comes from the host
/// (reactive getter re-reads it on every epoch rebuild).
// Reactive getters (`class=` attributes) cross leptos' Send bound even
// on wasm — hosts pass closures over leptos signals (Send+Sync), never
// over Rc/RefCell.
pub fn chip(path: String, on: impl Fn() -> bool + 'static + Send + Sync, onclick: VoidCb) -> impl IntoView {
    view! {
        <button class=move || if on() { "chip on" } else { "chip" }
            on:click={move |_: leptos::ev::MouseEvent| onclick()}>{path}</button>
    }
}

/// Rank axis: five pips of `axis` glyph, lit up to the selected rank;
/// title `name（alias）` when an alias exists (SSR's `rank_axis_html`
/// shape, now clickable). `sel_rank` returns 0 for unselected.
pub fn rank_axis(
    name: String,
    alias: String,
    axis: &'static str,
    sel_rank: impl Fn() -> i32 + 'static + Clone + Send + Sync,
    on_pick: PickCb,
) -> impl IntoView {
    let title = if alias.is_empty() {
        name.clone()
    } else {
        format!("{name}（{alias}）")
    };
    // five closures each own a copy of the getter: hosts pass closures
    // over leptos signals (Clone + Send + Sync), never over Rc/RefCell
    view! {
        <span class="rank" title=title>
            {(1..=5i32)
                .map(|k| {
                    let sel = sel_rank.clone();
                    let cls = move || if k <= sel() { "rk on" } else { "rk" };
                    let on_pick = Arc::clone(&on_pick);
                    view! {
                        <span class=cls on:click={move |_: leptos::ev::MouseEvent| (on_pick)(k)}>{axis}</span>
                    }
                })
                .collect_view()}
        </span>
    }
}
