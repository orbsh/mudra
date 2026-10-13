//! The hooks view — ns 9's rule set, governed through the same three
//! verbs the CLI uses (no second channel: a hook write is a `/hook_set`
//! or `/hook_delete` POST, exactly like `mudra hook set|rm`).
//!
//! Refresh is the view's own: a hook write never bumps the epoch (the
//! panel-invisible-collection discipline), so every mutation here bumps
//! a local generation counter that re-reads the list. The epoch hint is
//! read as well — that is what surfaces a hook edited from the CLI in an
//! already-open panel.
//!
//! Boundary, stated where the buttons are: disable and delete stop
//! FUTURE documents. Code already running in a live page cannot be
//! recalled — the page re-arms from the current set on reload.

use std::sync::{Arc, Mutex};

use leptos::prelude::*;
use serde_json::Value;
use wasm_bindgen_futures::spawn_local;

use crate::http::{self, HookRow};

/// The view key the panel switches on (lib.rs's tab and branch read it).
pub const VIEW: &str = "hooks";

/// What the editor holds. Deliberately a plain mutex'd draft and NOT a
/// signal: binding the fields reactively would write the value back into
/// the textarea on every keystroke, and a code body re-writing itself
/// moves the caret. The form is uncontrolled — the DOM is built from the
/// draft when the editor opens, the input handlers write back, save reads.
#[derive(Default, Clone)]
struct Draft {
    name: String,
    /// A new hook: the name is the upsert identity, so it is only
    /// editable before the first save.
    fresh: bool,
    matches: String,
    code: String,
}

#[component]
pub fn HooksView(view: ReadSignal<String>, tick: ReadSignal<u64>) -> impl IntoView {
    let (rows, set_rows) = signal(Arc::new(Vec::<HookRow>::new()));
    let (err, set_err) = signal(String::new());
    let (open, set_open) = signal(false);
    let (seq, set_seq) = signal(0u64);
    let draft = Arc::new(Mutex::new(Draft::default()));

    // the switch builds this component (entry read), `seq` re-reads after
    // our own writes, `tick` catches writes made outside the panel
    Effect::new(move || {
        if view.get() != VIEW {
            return;
        }
        let _ = tick.get();
        let _ = seq.get();
        spawn_local(async move {
            match http::hook_list().await {
                Ok(r) => {
                    set_rows.set(Arc::new(r.hooks));
                    set_err.set(String::new());
                }
                Err(e) => set_err.set(e),
            }
        });
    });

    // one write path for every mutation: post the verb, then re-read.
    // A failed write re-reads too — the list shows the truth, the error
    // line shows the reason (error passthrough, no swallowed messages).
    let post: Arc<dyn Fn(&'static str, Value) + Send + Sync> = Arc::new(move |path, body| {
        spawn_local(async move {
            match http::verb(path, body).await {
                Ok(()) => set_err.set(String::new()),
                Err(e) => set_err.set(e),
            }
            set_seq.update(|g| *g += 1);
        });
    });

    let toggle: Arc<dyn Fn(HookRow) + Send + Sync> = {
        let post = Arc::clone(&post);
        Arc::new(move |h: HookRow| {
            post(
                "/hook_set",
                serde_json::json!({ "name": h.name, "enabled": if h.enabled == 1 { 0 } else { 1 } }),
            );
        })
    };
    let remove: Arc<dyn Fn(HookRow) + Send + Sync> = {
        let post = Arc::clone(&post);
        Arc::new(move |h: HookRow| {
            let ok = web_sys::window()
                .and_then(|w| w.confirm_with_message(&format!("delete hook {:?}?", h.name)).ok())
                .unwrap_or(false);
            if ok {
                post("/hook_delete", serde_json::json!({ "name": h.name }));
            }
        })
    };
    let edit: Arc<dyn Fn(HookRow) + Send + Sync> = {
        let draft = Arc::clone(&draft);
        Arc::new(move |h: HookRow| {
            *draft.lock().unwrap() = Draft {
                name: h.name,
                fresh: false,
                matches: h.matches,
                code: h.code,
            };
            set_open.set(true);
        })
    };
    let new_hook = {
        let draft = Arc::clone(&draft);
        move |_: leptos::ev::MouseEvent| {
            *draft.lock().unwrap() = Draft { fresh: true, ..Default::default() };
            set_open.set(true);
        }
    };
    // an Arc handle, not a bare closure: the editor below lives in a Show
    // children closure (Fn), which may only clone it, never move it out
    let save: Arc<dyn Fn(leptos::ev::MouseEvent) + Send + Sync> = {
        let draft = Arc::clone(&draft);
        let post = Arc::clone(&post);
        Arc::new(move |_: leptos::ev::MouseEvent| {
            let d = draft.lock().unwrap().clone();
            let name = d.name.trim().to_string();
            if name.is_empty() || d.code.trim().is_empty() {
                set_err.set("a hook needs a name and code".into());
                return;
            }
            // `matches` and `code` are always sent (an emptied field must
            // mean "every page" / "empty body", not "keep the old one");
            // `enabled` is left out so a save never flips the switch
            post(
                "/hook_set",
                serde_json::json!({ "name": name, "matches": d.matches, "code": d.code }),
            );
            set_open.set(false);
        })
    };

    // everything the editor needs is (re)built inside its block: the Show
    // children closure is `Fn`, so nothing may be moved out of this
    // environment into it

    view! {
        <div class="hks">
            <div class="vhdr">
                <span class="count">{move || format!("{} hooks", rows.get().len())}</span>
                <button class="b" on:click=new_hook>"＋ new hook"</button>
            </div>
            <Show when=move || !err.get().is_empty() fallback=|| ()>
                <div class="err">{move || err.get()}</div>
            </Show>
            // Show's children run on every mount, so the plain field
            // values below are read from the draft exactly when the
            // editor opens — an uncontrolled form by construction
            <Show when=move || open.get() fallback=|| ()>
                {
                    let d = draft.lock().unwrap().clone();
                    // per-mount handlers + a per-mount save clone: the
                    // form is uncontrolled (fields are initial values,
                    // typing only reaches the draft) and nothing here
                    // moves out of the component's environment
                    let (dn, dm, dc) = (Arc::clone(&draft), Arc::clone(&draft), Arc::clone(&draft));
                    let on_name =
                        move |ev: leptos::ev::Event| dn.lock().unwrap().name = event_target_value(&ev);
                    let on_matches =
                        move |ev: leptos::ev::Event| dm.lock().unwrap().matches = event_target_value(&ev);
                    let on_code =
                        move |ev: leptos::ev::Event| dc.lock().unwrap().code = event_target_value(&ev);
                    let save = {
                        let save = Arc::clone(&save);
                        move |ev: leptos::ev::MouseEvent| save(ev)
                    };
                    let name_field = if d.fresh {
                        view! {
                            <input class="hk-in" type="text" placeholder="name" value=d.name.clone() on:input=on_name />
                        }
                        .into_any()
                    } else {
                        view! {
                            <span class="hk-name" title="the upsert identity">{d.name.clone()}</span>
                        }
                        .into_any()
                    };
                    view! {
                        <div class="hk-edit">
                            <div class="hk-f">{name_field}</div>
                            <div class="hk-f">
                                <input class="hk-in" type="text"
                                    placeholder="matches: comma-separated URL substrings (empty = every page)"
                                    value=d.matches.clone()
                                    on:input=on_matches
                                />
                            </div>
                            <div class="hk-f">
                                <textarea class="hk-code-in" spellcheck="false"
                                    placeholder="code (guarded by `matches`, wrapped in its own scope)"
                                    on:input=on_code
                                >{d.code.clone()}</textarea>
                            </div>
                            <div class="hk-f">
                                <button class="b" on:click=save>"save"</button>
                                <button class="b" on:click=move |_| set_open.set(false)>"cancel"</button>
                            </div>
                        </div>
                    }
                }
            </Show>
            <Show when=move || !rows.get().is_empty() fallback=|| ()>
                <For
                    each=move || (*rows.get()).clone()
                    // the key carries the payload, so a toggle or an edit
                    // rebuilds the row (the tree's pointer-identity rule,
                    // spelled out for value rows)
                    key=|h| (h.id, h.enabled, h.updated_at, h.matches.clone(), h.code.clone())
                    children={
                        let (toggle, remove, edit) =
                            (Arc::clone(&toggle), Arc::clone(&remove), Arc::clone(&edit));
                        move |h: HookRow| {
                            let (toggle, remove, edit) =
                                (Arc::clone(&toggle), Arc::clone(&remove), Arc::clone(&edit));
                            let on = h.enabled == 1;
                            let code = h.code.clone();
                            let preview = code.lines().next().unwrap_or_default().trim().to_string();
                            let matches = if h.matches.trim().is_empty() {
                                "*".to_string()
                            } else {
                                h.matches.clone()
                            };
                            let at = h.updated_at;
                            let (h1, h2, h3) = (h.clone(), h.clone(), h.clone());
                            view! {
                                <div class="ncard hk">
                                    <div class="row1">
                                        <button
                                            class=if on { "b hk-on" } else { "b" }
                                            title="enabled: injected into matching pages (disable stops future documents only)"
                                            on:click=move |_| (toggle)(h1.clone())
                                        >{if on { "on" } else { "off" }}</button>
                                        <span class="hk-name">{h.name.clone()}</span>
                                        <span class="hk-match">{matches}</span>
                                        <span class="meta">{crate::time_ago(at)}</span>
                                        <span class="spacer"></span>
                                        <button class="b" on:click=move |_| (edit)(h2.clone())>"edit"</button>
                                        <button class="act del" title="Delete" on:click=move |_| (remove)(h3.clone())>"🗑"</button>
                                    </div>
                                    <div class="hk-code" title=code.clone()>{preview}</div>
                                </div>
                            }
                        }
                    }
                />
            </Show>
        </div>
    }
}