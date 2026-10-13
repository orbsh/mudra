//! The events view — the append-only log (ns 8) as a live tail.
//!
//! Read-only by construction: `/events {tail}` answers the newest rows,
//! so a re-pull overwrites the list (no cursor to keep in step, no
//! duplicate-append path).
//!
//! Refresh rides the SAME epoch bus as the tree. That is complete for
//! today's producers by an accident of the architecture worth naming: a
//! log write never bumps the epoch (the panel-invisible-collection
//! discipline), but every kind the daemon emits today (page_open /
//! page_close / tag_set) is emitted alongside a page or tag write, and
//! that write bumps. A future log-only kind would need its own answer
//! (a poll, or letting the log participate in the epoch) — never a
//! second push channel.

use std::sync::Arc;

use leptos::prelude::*;
use serde_json::Value;
use wasm_bindgen_futures::spawn_local;

use crate::http::{self, EventRow};

/// The view key the panel switches on (lib.rs's tab and branch read it).
pub const VIEW: &str = "events";

/// The tail window (the daemon caps at 500).
const TAIL: u64 = 200;

#[component]
pub fn EventsView(view: ReadSignal<String>, tick: ReadSignal<u64>) -> impl IntoView {
    let (rows, set_rows) = signal(Arc::new(Vec::<EventRow>::new()));
    let (head, set_head) = signal(0u64);
    let (sel, set_sel) = signal(Arc::new(Vec::<String>::new()));
    let (err, set_err) = signal(String::new());

    // the switch builds this component, so the first effect run IS the
    // entry pull; after that every epoch hint re-pulls. The `view` guard
    // covers the one racing case: a hint landing while the branch is
    // being torn down must not fire a fetch.
    Effect::new(move || {
        if view.get() != VIEW {
            return;
        }
        let _ = tick.get();
        spawn_local(async move {
            match http::events(TAIL).await {
                Ok(r) => {
                    set_head.set(r.head);
                    set_rows.set(Arc::new(r.events));
                    set_err.set(String::new());
                }
                Err(e) => set_err.set(e),
            }
        });
    });

    // kinds in first-seen order: the chips must not reshuffle between
    // re-pulls (the tail is ascending, so the order is stable)
    let kinds = move || {
        let mut out: Vec<String> = Vec::new();
        for e in rows.get().iter() {
            if !out.contains(&e.kind) {
                out.push(e.kind.clone());
            }
        }
        out
    };
    // newest first — a log reads top-down
    let shown = move || {
        let s = sel.get();
        (*rows.get())
            .iter()
            .rev()
            .filter(|e| s.is_empty() || s.contains(&e.kind))
            .cloned()
            .collect::<Vec<EventRow>>()
    };

    view! {
        <div class="evs">
            <div class="vhdr">
                <span class="count">
                    {move || format!(
                        "{} of {} events · head #{}",
                        shown().len(),
                        rows.get().len(),
                        head.get()
                    )}
                </span>
                <div class="filts">
                    <For
                        each=kinds
                        key=|k| k.clone()
                        children=move |k: String| {
                            // one clone per consumer: the lit state, the
                            // click handler, and the chip label itself
                            let for_click = k.clone();
                            let on = {
                                let k = k.clone();
                                move || sel.get().contains(&k)
                            };
                            let onclick: tag_forest::leptos::VoidCb = Arc::new(move || {
                                let k = for_click.clone();
                                set_sel.update(|s| {
                                    let mut n: Vec<String> = (**s).clone();
                                    match n.iter().position(|x| *x == k) {
                                        Some(i) => {
                                            n.remove(i);
                                        }
                                        None => n.push(k),
                                    }
                                    *s = Arc::new(n);
                                });
                            });
                            tag_forest::leptos::chip(k, on, onclick)
                        }
                    />
                </div>
            </div>
            <Show when=move || !err.get().is_empty() fallback=|| ()>
                <div class="err">{move || err.get()}</div>
            </Show>
            <For
                each=shown
                key=|e| e.id
                children=move |e: EventRow| {
                    // two bindings, not a clone-then-move: the macro may
                    // evaluate the attribute and the child in either order
                    let full = arg_summary(&e.args);
                    let title = full.clone();
                    let at = e.at;
                    view! {
                        <div class="ev">
                            <span class="ev-id">{format!("#{}", e.id)}</span>
                            <span class="ev-kind">{e.kind}</span>
                            <span class="ev-args" title=title>{full}</span>
                            <span class="meta">{crate::time_ago(at)}</span>
                        </div>
                    }
                }
            />
        </div>
    }
}

/// One-line summary of an event's args: the lifecycle kinds all carry
/// `url`, so that leads and the rest follows in a fixed order; anything
/// else falls back to compact JSON. Style truncates the line and the
/// row's `title` carries the whole snapshot.
fn arg_summary(args: &Value) -> String {
    if let Value::Object(map) = args {
        let mut parts: Vec<String> = Vec::new();
        for key in ["url", "ctx", "page_id", "tag_ids"] {
            if let Some(v) = map.get(key) {
                parts.push(format!("{key}={v}"));
            }
        }
        if !parts.is_empty() {
            return parts.join("  ");
        }
    }
    args.to_string()
}