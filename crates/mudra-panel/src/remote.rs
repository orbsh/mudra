//! The panel's epoch bus: one WebSocket to mudrad's frame endpoint,
//! used for its ONLY remaining push — the `{"epoch":N}` text hint that
//! says "a shaped read may be stale; re-pull it" (PLAN §11's two-channel
//! division: WS = invalidation hints, HTTP :8899 = every read and write).
//!
//! The KV query plane that used to ride this socket (slice 1's bare
//! `OpFrame` get/scan over FIFO pairing) is retired: slice 2b moved all
//! shaped reads to the control verbs and all writes ride them too, so
//! the socket never carries a binary frame anymore. The correlation
//! machinery the panel originally hand-rolled lives on in okm as
//! `WireClient` (ADR-0028) with its own test baseline — a future
//! in-browser KV consumer starts from the crate, not from here.
//!
//! What stays is everything JS-lifecycle: socket construction, the
//! 800ms reconnect backoff (mirroring the retired JS client), and the
//! text-frame dedupe against the last seen number. No connection-state
//! surface is exported: nothing in the panel branches on it — the socket
//! simply is, reconnects, and dedupes.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CloseEvent, MessageEvent, WebSocket};

/// Push-list callback bag (epoch hints).
type Listeners<T> = RefCell<Vec<Box<dyn Fn(T)>>>;

/// Connection lifecycle + the epoch bus. Rc/RefCell by construction:
/// wasm is main-thread only, no atomics.
pub struct WsLink {
    epoch_listeners: Listeners<u64>,
    last_epoch: RefCell<u64>,
    url: String,
}

impl WsLink {
    pub fn connect(url: &str) -> Rc<Self> {
        let link = Rc::new(Self {
            epoch_listeners: RefCell::new(Vec::new()),
            last_epoch: RefCell::new(0),
            url: url.to_string(),
        });
        link.open();
        link
    }

    /// The panel page is served by mudrad on PANEL_PORT; the WS frame
    /// endpoint is PANEL_PORT+1 — same layout rule the JS client used
    /// (`Number(location.port) + 1`).
    pub fn from_location() -> Rc<Self> {
        let win = web_sys::window().expect("no window");
        let loc = win.location();
        let proto = if loc.protocol().unwrap_or_default() == "https:" { "wss" } else { "ws" };
        let host = loc.hostname().unwrap_or_else(|_| "127.0.0.1".into());
        let port: u16 = loc
            .port()
            .unwrap_or_default()
            .trim_start_matches(':')
            .parse()
            .unwrap_or(9299);
        Self::connect(&format!("{proto}://{host}:{}/", port + 1))
    }

    fn open(self: &Rc<Self>) {
        let Ok(sock) = WebSocket::new(&self.url) else {
            self.schedule_reconnect();
            return;
        };
        // Closures live as long as the page: the panel is one long-lived
        // connection; reconnects are rare and each leaks one 3-closure
        // set (documented, not unbounded — the epoch rescan path is).
        let weak = Rc::downgrade(self);
        let on_message = Closure::<dyn Fn(MessageEvent)>::new(move |ev: MessageEvent| {
            // the only inbound shape today is the epoch hint; any
            // other frame (including binary) is not this socket's
            // contract anymore — ignore rather than half-parse
            if let Some(s) = weak.upgrade() {
                s.on_epoch_text(&ev);
            }
        })
        .into_js_value();
        sock.set_onmessage(Some(on_message.unchecked_ref()));
        let weak = Rc::downgrade(self);
        let on_close = Closure::<dyn Fn(CloseEvent)>::new(move |_| {
            // an error is always followed by a close event on the JS
            // side — this arm owns the retry; no onerror handler needed
            if let Some(s) = weak.upgrade() {
                s.schedule_reconnect();
            }
        })
        .into_js_value();
        sock.set_onclose(Some(on_close.unchecked_ref()));
    }

    fn schedule_reconnect(self: &Rc<Self>) {
        // 800ms backoff, mirroring the retired JS client
        let weak = Rc::downgrade(self);
        let cb = Closure::<dyn Fn()>::new(move || {
            if let Some(s) = weak.upgrade() {
                s.open();
            }
        });
        let win = web_sys::window().expect("no window");
        if win
            .set_timeout_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), 800)
            .is_err()
        {
            cb.forget(); // timer never armed: do not drop a leaked JS reference
        }
    }

    /// `{"epoch":N}` deduped against the last seen number.
    fn on_epoch_text(&self, ev: &MessageEvent) {
        let Some(t) = ev.data().as_string() else { return };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else {
            return;
        };
        let Some(e) = v.get("epoch").and_then(|x| x.as_u64()) else {
            return;
        };
        if e == *self.last_epoch.borrow() {
            return;
        }
        *self.last_epoch.borrow_mut() = e;
        for l in self.epoch_listeners.borrow().iter() {
            l(e);
        }
    }

    pub fn on_epoch(&self, f: impl Fn(u64) + 'static) {
        self.epoch_listeners.borrow_mut().push(Box::new(f));
    }
}
