//! RemoteStore over the bare NestStorage WS frame protocol.
//!
//! One WebSocket message = one `okm_wire::OpFrame` (okm-wire's reuse
//! contract: "any transport that can carry one Vec<u8> per frame"). The
//! panel is a pure frame sender: keys/values are encoded with the shared
//! `mudra-store` schema types (same derives, same ns constants — byte
//! identical to mudrad) and decoded from raw bytes. okm's type layer is
//! bound to the synchronous `VirtualStorage`, which cannot cross an
//! async WS boundary in wasm (no `block_on`); the panel therefore speaks
//! the byte contract directly — this is the PLAN §11 boundary, not a
//! shortcut.
//!
//! Response correlation is FIFO: the daemon applies one connection's
//! frames sequentially (engine lock), so replies arrive in send order.
//! Query frames (get/scan) get a response; write frames do NOT — apply
//! answers `Some` only for query ops (WS-CHANNEL shape A) — so only
//! frames carrying a query push a waiter onto the queue.
//!
//! Server push is the epoch text frame ONLY (`{"epoch":N}`): a changed
//! number is the invalidation signal, the panel re-scans. No bespoke op
//! channel exists here and none may be revived (PLAN §11).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CloseEvent, ErrorEvent, MessageEvent, WebSocket};

use okm_wire::{OpFrame, OpResponse, OP_GET, OP_SCAN};

/// Query replies: transport failure is an explicit `Err` (the error
/// passthrough rule — a broken pipe never resolves to empty data).
pub type Reply = Result<OpResponse, String>;
type Waiter = Pin<Box<dyn Future<Output = Reply>>>;

/// The resolved value rides on the shared inner (boxed dyn futures drop
/// without carrying their value), so the resolver and the future agree
/// through it.
struct SlotInner {
    done: RefCell<Option<Reply>>,
    waker: RefCell<Option<Waker>>,
}

struct SlotFuture(Rc<SlotInner>);
impl Future for SlotFuture {
    type Output = Reply;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Reply> {
        if let Some(r) = self.0.done.borrow_mut().take() {
            return Poll::Ready(r);
        }
        *self.0.waker.borrow_mut() = Some(cx.waker().clone());
        Poll::Pending
    }
}

fn new_slot() -> (Rc<SlotInner>, Waiter) {
    let inner = Rc::new(SlotInner {
        done: RefCell::new(None),
        waker: RefCell::new(None),
    });
    (inner.clone(), Box::pin(SlotFuture(inner)))
}

fn resolve(inner: &Rc<SlotInner>, r: Reply) {
    *inner.done.borrow_mut() = Some(r);
    if let Some(w) = inner.waker.borrow_mut().take() {
        w.wake();
    }
}

/// Push-list callback bag (connection state / epoch hints).
type Listeners<T> = RefCell<Vec<Box<dyn Fn(T)>>>;

/// Frame transport + epoch listener. Rc/RefCell by construction: wasm is
/// main-thread only, no atomics — the same single-threaded discipline
/// mudrad applies to its engine.
pub struct WsLink {
    sock: RefCell<Option<WebSocket>>,
    /// FIFO query waiters (send order == reply order under sequential apply).
    waiters: Rc<RefCell<VecDeque<Rc<SlotInner>>>>,
    connected: RefCell<bool>,
    listeners: Listeners<bool>,
    epoch_listeners: Listeners<u64>,
    last_epoch: RefCell<u64>,
    url: String,
}

impl WsLink {
    pub fn connect(url: &str) -> Rc<Self> {
        let link = Rc::new(Self {
            sock: RefCell::new(None),
            waiters: Rc::new(RefCell::new(VecDeque::new())),
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
        let on_message = Closure::<dyn Fn(MessageEvent)>::new(move |ev| {
            if let Some(s) = weak.upgrade() {
                s.on_message(&ev);
            }
        })
        .into_js_value();
        sock.set_onmessage(Some(on_message.unchecked_ref()));
        let weak = Rc::downgrade(self);
        let on_close = Closure::<dyn Fn(CloseEvent)>::new(move |_| {
            if let Some(s) = weak.upgrade() {
                s.set_connected(false);
                s.schedule_reconnect();
            }
        })
        .into_js_value();
        sock.set_onclose(Some(on_close.unchecked_ref()));
        let weak = Rc::downgrade(self);
        let on_error = Closure::<dyn Fn(ErrorEvent)>::new(move |_| {
            if let Some(s) = weak.upgrade() {
                s.set_connected(false);
            }
        })
        .into_js_value();
        sock.set_onerror(Some(on_error.unchecked_ref()));
        *self.sock.borrow_mut() = Some(sock);
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
        if !up {
            let mut q = self.waiters.borrow_mut();
            while let Some(w) = q.pop_front() {
                resolve(&w, Err("ws closed".into()));
            }
        }
        for l in self.listeners.borrow().iter() {
            l(up);
        }
    }

    fn on_message(&self, ev: &MessageEvent) {
        if let Some(buf) = ev.data().dyn_ref::<js_sys::ArrayBuffer>() {
            let bytes = js_sys::Uint8Array::new(buf).to_vec();
            let reply = match OpResponse::decode(&bytes) {
                Some(resp) => Ok(resp),
                None => Err("malformed response frame".into()),
            };
            if let Some(w) = self.waiters.borrow_mut().pop_front() {
                resolve(&w, reply);
            }
            return;
        }
        // text frame = the only server push: {"epoch":N}
        if let Some(t) = ev.data().as_string()
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&t)
            && let Some(e) = v.get("epoch").and_then(|x| x.as_u64())
            && e != *self.last_epoch.borrow()
        {
            *self.last_epoch.borrow_mut() = e;
            for l in self.epoch_listeners.borrow().iter() {
                l(e);
            }
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

    /// Send one frame. Query frames return the reply future (FIFO waiter);
    /// write-only frames resolve `Ok(default)` on a successful send — no
    /// response exists for them by protocol, delivery is the guarantee.
    pub fn request(self: &Rc<Self>, frame: &OpFrame) -> Waiter {
        let expects_reply = frame
            .0
            .iter()
            .any(|(tag, _, _)| matches!(*tag, OP_GET | OP_SCAN));
        let (slot, fut): (Option<Rc<SlotInner>>, Waiter) = if expects_reply {
            let (inner, w) = new_slot();
            self.waiters.borrow_mut().push_back(inner.clone());
            (Some(inner), w)
        } else {
            (None, Box::pin(std::future::ready(Ok(OpResponse::default()))))
        };
        let sent = self
            .sock
            .borrow()
            .as_ref()
            .map(|s| {
                let bytes = js_sys::Uint8Array::from(&frame.encode()[..]);
                s.send_with_array_buffer(&bytes.buffer())
            });
        match sent {
            Some(Ok(())) => fut,
            _ => {
                if let Some(inner) = slot {
                    self.waiters
                        .borrow_mut()
                        .retain(|w| !Rc::ptr_eq(w, &inner));
                    resolve(&inner, Err("ws send failed".into()));
                }
                Box::pin(std::future::ready(Err("ws send failed".into())))
            }
        }
    }
}

/// Minimal typed helpers: the panel speaks okm bytes through the shared
/// schema types (markers carry SLOT / KEY_LEN / decode), never restating
/// layout. Response semantics per op are decided by the receiver; these
/// wrap frames only for the query shapes slice 1 exercises.
pub async fn get_bytes(link: &Rc<WsLink>, key: &[u8]) -> Reply {
    link.request(&OpFrame::one(OP_GET, key.to_vec(), Vec::new())).await
}

/// Legacy prefix scan (empty value segment): the receiver answers with
/// prefix-relative suffixes, byte-for-byte the local engine's contract.
pub async fn scan_prefix(link: &Rc<WsLink>, prefix: &[u8]) -> Reply {
    link.request(&OpFrame::one(OP_SCAN, prefix.to_vec(), Vec::new())).await
}
