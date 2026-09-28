//! mudra-panel — the console UI as leptos CSR wasm (R2 slice 1).
//!
//! Data plane: the bare `NestStorage` frame protocol. This crate encodes
//! okm keys/values with the shared `mudra-store` schema types
//! (`default-features = false` — schema bytes only, no fjall on wasm) and
//! ships `okm-wire` frames as opaque WebSocket binaries to mudrad :9300.
//! Server push is the epoch hint text frame ONLY — a changed number
//! triggers re-scan; no second semantic channel (PLAN §11).
//!
//! Slice 1 scope = skeleton + RemoteStore + a smoke view proving
//! wasm render + WS frame round trip. The old panel's views port
//! one behavior at a time in slice 2; the Python tree stays the
//! behavior spec until then.
//!
//! Threading note: `Rc<WsLink>` cannot enter leptos context (its bounds
//! demand Send+Sync) — views take the link by closure instead. Wasm is
//! main-thread only; the Rc/RefCell discipline is deliberate, not a
//! limitation to paper over with Arc/Mutex.

pub mod remote;
pub mod schema;

use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use remote::WsLink;

/// trunk entry point: render `App` into the document body (index.html
/// carries no wrapper element — the panel root is its own `.panel` div).
pub fn mount() {
    leptos::mount::mount_to_body(App);
}

/// Panel root: one shared WsLink, threaded to views by closure.
#[component]
pub fn App() -> impl IntoView {
    let link = WsLink::from_location();
    view! {
        <div class="panel">
            <header class="hdr">{status_line(link.clone())}</header>
            <main class="tree">{schema_probe(link)}</main>
        </div>
    }
}

/// Connection state from the link's push listeners (never polled).
fn status_line(link: Rc<WsLink>) -> impl IntoView {
    let (connected, set_connected) = signal(link.is_connected());
    link.on_connected(move |up| set_connected.set(up));
    move || {
        if connected.get() { "mudrad: connected" } else { "mudrad: offline" }.to_string()
    }
}

/// Slice-1 smoke against the live daemon: the state table's primary
/// keyspace scans byte-identical through the frame path (`schema::state`
/// proves the shared encoding), and the current context decodes from a
/// point get by the shared key type. Both are read-only — the panel has
/// no write surface until the verb bridge (slice 2, control verbs stay
/// HTTP on 8899 by design).
fn schema_probe(link: Rc<WsLink>) -> impl IntoView {
    let (rows, set_rows) = signal(String::new());

    // epoch re-scan: the ONLY push we honour; run the probe again
    link.on_epoch({
        let link = link.clone();
        move |_| spawn_local(probe(link.clone(), set_rows))
    });
    // also on first connect (daemon may have been offline at mount)
    link.on_connected({
        let link = link.clone();
        move |up| {
            if up {
                spawn_local(probe(link.clone(), set_rows));
            }
        }
    });

    view! { <div class="probe">{move || rows.get()}</div> }
}

async fn probe(link: Rc<WsLink>, set_rows: WriteSignal<String>) {
    // scan the whole State primary range: slot bytes are fixed u8 keys
    let prefix = schema::state_prefix();
    match remote::scan_prefix(&link, &prefix).await {
        Ok(resp) => {
            let mut out = format!("state rows scanned: {}\n", resp.suffixes.len());
            // current_context: point get through the shared StateKey encode
            let key = schema::state_key(mudra_store::state::CURRENT_CONTEXT);
            match remote::get_bytes(&link, &key).await {
                Ok(r) => {
                    let text = r
                        .value
                        .as_deref()
                        .map(schema::decode_state_text)
                        .unwrap_or_default();
                    out.push_str(&format!("current context: {text}"));
                }
                Err(e) => out.push_str(&format!("context get failed: {e}")),
            }
            set_rows.set(out);
        }
        Err(e) => set_rows.set(format!("scan failed: {e}")),
    }
}
