//! RemoteStore over the bare NestStorage WS frame protocol — the thin
//! wasm transport around okm's `WireClient` (ADR-0028).
//!
//! One WebSocket message = one `okm_wire::OpFrame` (okm-wire's reuse
//! contract: "any transport that can carry one Vec<u8> per frame"). The
//! FIFO pairing, the explicit-Err teardown, and the write-frame silence
//! are no longer hand-rolled here: they live in `okm_core::WireClient`,
//! which this module feeds with a `WireTransport` whose `post` writes
//! the socket's send. The panel was the discovery ground for those
//! rules; the crate is their home now.
//!
//! What stays local is everything JS-lifecycle: socket construction,
//! the 800ms reconnect backoff, and the TWO frame shapes multiplexed on
//! one socket — binary = wire frames (delivered to the client), text =
//! the epoch hint (the only push; a changed number is the invalidation
//! signal, the panel re-scans). No bespoke op channel exists here and
//! none may be revived (PLAN §11).
//!
//! Each connection owns its WireClient: a reconnect rebinds a fresh
//! client, so the dead-connection stickiness is per-socket — the old
//! connection's in-flight waiters were already failed on close.
//!
//! okm's type layer is bound to the synchronous `VirtualStorage`, which
//! cannot cross an async WS boundary in wasm (no `block_on`); the panel
//! therefore speaks the byte contract through the async `WireClient` —
//! this is the PLAN §11 boundary, not a shortcut.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CloseEvent, ErrorEvent, MessageEvent, WebSocket};

use okm_core::{MemBatch, VirtualStorageAsync, WireClient, WireTransport};
use okm_wire::{OpFrame, OpResponse, OP_DELETE, OP_GET, OP_PUT, OP_SCAN, OP_SCAN_STREAM};

/// Query replies: transport failure is an explicit `Err` (the error
/// passthrough rule — a broken pipe never resolves to empty data).
pub type Reply = Result<OpResponse, String>;

/// The injected transport: sends one frame per WS message. Browser
/// queueing is preserved: CONNECTING (0) and OPEN (1) both accept send
/// (the JS layer buffers until open); CLOSING/CLOSED reject, which the
/// WireClient turns into an explicit-Err + dead marker — the following
/// close event then rebinds a fresh client anyway.
#[derive(Clone)]
struct WsSocket {
    sock: Rc<RefCell<Option<WebSocket>>>,
}

impl WireTransport for WsSocket {
    fn post(&self, frame: &[u8]) -> Result<(), String> {
        let Some(sock) = self.sock.borrow().as_ref().cloned() else {
            return Err("ws has no socket".into());
        };
        if !matches!(sock.ready_state(), 0 | 1) {
            return Err("ws not open".into());
        }
        let bytes = js_sys::Uint8Array::from(frame);
        sock.send_with_array_buffer(&bytes.buffer())
            .map_err(|e| format!("ws send: {e:?}"))
    }
}

/// Push-list callback bag (connection state / epoch hints).
type Listeners<T> = RefCell<Vec<Box<dyn Fn(T)>>>;

/// Connection lifecycle + the per-connection `WireClient` + the epoch
/// bus. Rc/RefCell by construction: wasm is main-thread only, no
/// atomics — the same single-threaded discipline mudrad applies to its
/// engine.
pub struct WsLink {
    client: RefCell<Option<WireClient<WsSocket>>>,
    connected: RefCell<bool>,
    listeners: Listeners<bool>,
    epoch_listeners: Listeners<u64>,
    last_epoch: RefCell<u64>,
    url: String,
}

impl WsLink {
    pub fn connect(url: &str) -> Rc<Self> {
        let link = Rc::new(Self {
            client: RefCell::new(None),
            connected: RefCell::new(false),
            listeners: RefCell::new(Vec::new()),
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
        sock.set_binary_type(web_sys::BinaryType::Arraybuffer);
        // Closures live as long as the page: the panel is one long-lived
        // connection; reconnects are rare and each leaks one 4-closure
        // set (documented, not unbounded — the epoch rescan path is).
        let ws = WsSocket {
            sock: Rc::new(RefCell::new(Some(sock.clone()))),
        };
        let fresh = WireClient::new(ws);
        *self.client.borrow_mut() = Some(fresh.clone());
        let weak = Rc::downgrade(self);
        let on_open = {
            let weak = weak.clone();
            Closure::<dyn Fn()>::new(move || {
                if let Some(s) = weak.upgrade() {
                    s.set_connected(true);
                }
            })
            .into_js_value()
        };
        sock.set_onopen(Some(on_open.unchecked_ref()));
        let weak = Rc::downgrade(self);
        let on_message = {
            // binary answers route to THIS connection's client (a stale
            // socket's answer would find its own dead client's queue
            // empty — harmless by contract); text is the epoch bus.
            let client = fresh.clone();
            Closure::<dyn Fn(MessageEvent)>::new(move |ev: MessageEvent| {
                if let Some(buf) = ev.data().dyn_ref::<js_sys::ArrayBuffer>() {
                    let bytes = js_sys::Uint8Array::new(buf).to_vec();
                    client.deliver(&bytes);
                    return;
                }
                if let Some(s) = weak.upgrade() {
                    s.on_epoch_text(&ev);
                }
            })
            .into_js_value()
        };
        sock.set_onmessage(Some(on_message.unchecked_ref()));
        let weak = Rc::downgrade(self);
        let on_close = {
            let client = fresh.clone();
            Closure::<dyn Fn(CloseEvent)>::new(move |_| {
                // every outstanding waiter gets an explicit Err — never
                // a park (WireClient::fail; the rule the panel learned
                // live and the crate now owns)
                client.fail("ws closed");
                if let Some(s) = weak.upgrade() {
                    s.set_connected(false);
                    s.schedule_reconnect();
                }
            })
            .into_js_value()
        };
        sock.set_onclose(Some(on_close.unchecked_ref()));
        let weak = Rc::downgrade(self);
        let on_error = Closure::<dyn Fn(ErrorEvent)>::new(move |_| {
            if let Some(s) = weak.upgrade() {
                s.set_connected(false);
            }
        })
        .into_js_value();
        sock.set_onerror(Some(on_error.unchecked_ref()));
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

    fn set_connected(&self, up: bool) {
        if *self.connected.borrow() == up {
            return;
        }
        *self.connected.borrow_mut() = up;
        for l in self.listeners.borrow().iter() {
            l(up);
        }
    }

    /// The text-frame half of the multiplexer: `{"epoch":N}` deduped
    /// against the last seen number.
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

    pub fn is_connected(&self) -> bool {
        *self.connected.borrow()
    }

    pub fn last_epoch(&self) -> u64 {
        *self.last_epoch.borrow()
    }

    pub fn on_connected(&self, f: impl Fn(bool) + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    pub fn on_epoch(&self, f: impl Fn(u64) + 'static) {
        self.epoch_listeners.borrow_mut().push(Box::new(f));
    }

    fn with_client<R>(
        &self,
        run: impl FnOnce(WireClient<WsSocket>) -> R,
    ) -> Result<R, String> {
        let client = self
            .client
            .borrow()
            .clone()
            .ok_or("ws not open")?;
        Ok(run(client))
    }

    /// Send one frame. Query frames return the reply (FIFO pairing is
    /// WireClient's); write-only frames ride ONE `commit_batch` = one
    /// frame = one receiver pass, answering nothing by protocol —
    /// delivery is the guarantee, so the reply is the default response
    /// on a successful post (the retired `request` contract, kept).
    pub async fn request(link: &Rc<Self>, frame: &OpFrame) -> Reply {
        if frame
            .0
            .iter()
            .any(|(tag, _, _)| matches!(*tag, OP_GET | OP_SCAN | OP_SCAN_STREAM))
        {
            let client = link.with_client(|c| c)?;
            return client.query(frame).await;
        }
        let mut batch = MemBatch::default();
        for (tag, key, value) in &frame.0 {
            match *tag {
                OP_PUT => batch.ops.push((key.clone(), Some(value.clone()))),
                OP_DELETE => batch.ops.push((key.clone(), None)),
                _ => return Err("write frame carries a query op".into()),
            }
        }
        let client = link.with_client(|c| c)?;
        let mut writer = client;
        // the retired `request` contract: a successfully posted write
        // frame answers with the default response (delivery = success)
        writer.commit_batch(batch).await.map(|()| OpResponse::default())
    }
}

/// Minimal typed helpers: the panel speaks okm bytes through the shared
/// schema types (markers carry SLOT / KEY_LEN / decode), never restating
/// layout. Response semantics per op are decided by the receiver; these
/// wrap frames only for the query shapes slice 1 exercises.
pub async fn get_bytes(link: &Rc<WsLink>, key: &[u8]) -> Reply {
    let frame = OpFrame::one(OP_GET, key.to_vec(), Vec::new());
    WsLink::request(link, &frame).await
}

/// Legacy prefix scan (empty value segment): the receiver answers with
/// prefix-relative suffixes, byte-for-byte the local engine's contract.
pub async fn scan_prefix(link: &Rc<WsLink>, prefix: &[u8]) -> Reply {
    let frame = OpFrame::one(OP_SCAN, prefix.to_vec(), Vec::new());
    WsLink::request(link, &frame).await
}
