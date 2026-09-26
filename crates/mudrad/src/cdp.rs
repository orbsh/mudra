//! CDP client over WebSocket — the port of `mudralib/cdp.py`.
//!
//! Blood-lesson inventory (each maps to a test in `tests/cdp_test.rs`):
//!
//! - single-reader routing: the Python probe lesson — `Runtime.evaluate`
//!   responses and `Target.*` events share one ws; a second pump task
//!   steals the response and the call hangs forever (misdiagnosed as
//!   "the page is stuck"). One reader task owns the socket's read half
//!   and dispatches: messages with `id` go to their waiter, the rest go
//!   to the event stream. Callers never touch the socket directly.
//! - `/json` and `/json/version` are plain HTTP on the debug port; the
//!   raw minimal GET avoids an HTTP client dependency (the Python tree
//!   used stdlib urllib for exactly this), and the response is read to
//!   EOF with `Connection: close`.
//!
//! Timestamps/ids are u64; the CDP envelope JSON stays `serde_json::Value`
//! — the daemon only reads `id`/`method`/`params`/`result`/`error`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;

pub type CdpResult<T> = Result<T, CdpError>;

#[derive(Debug)]
pub enum CdpError {
    /// Transport-level failure (socket closed, connect refused…).
    Io(String),
    /// The CDP envelope answered with an `error` object.
    Api(Value),
}

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CdpError::Io(e) => write!(f, "cdp io: {e}"),
            CdpError::Api(v) => write!(f, "cdp api: {v}"),
        }
    }
}

impl std::error::Error for CdpError {}

impl From<std::io::Error> for CdpError {
    fn from(e: std::io::Error) -> Self {
        CdpError::Io(e.to_string())
    }
}

/// Browser-level CDP websocket url: `ws://127.0.0.1:<port>/devtools/browser/<id>`
pub async fn browser_ws(port: u16) -> CdpResult<String> {
    let body = http_get_json(port, "/json/version").await?;
    body.get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| CdpError::Io("/json/version lacks webSocketDebuggerUrl".into()))
}

/// The `/json` target list (id/type/url/title + webSocketDebuggerUrl per page).
pub async fn list_targets(port: u16) -> CdpResult<Vec<Value>> {
    let body = http_get_json(port, "/json").await?;
    body.as_array()
        .cloned()
        .ok_or_else(|| CdpError::Io("/json is not an array".into()))
}

/// Minimal HTTP GET on the debug port: no keep-alive, read to EOF, return
/// the parsed JSON body. (Chromium's local devtools endpoints always
/// close the response, so EOF is the body terminator.)
async fn http_get_json(port: u16, path: &str) -> CdpResult<Value> {
    let mut sock = TcpStream::connect(("127.0.0.1", port)).await?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    sock.write_all(req.as_bytes()).await?;
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await?;
    let text = String::from_utf8_lossy(&raw);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or(text.as_ref());
    serde_json::from_str(body).map_err(|e| CdpError::Io(format!("bad json from {path}: {e}")))
}

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;
type SplitSink = futures_util::stream::SplitSink<WsStream, Message>;

/// A live browser-level CDP connection. One reader task owns the socket's
/// read half; commands go out through the sink, events arrive on a
/// channel — never two pumps on one socket.
pub struct CdpConn {
    write: Mutex<SplitSink>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    events: Mutex<mpsc::UnboundedReceiver<Value>>,
    next_id: AtomicU64,
    /// The routing task; aborts when the socket dies or `close` runs.
    reader: tokio::task::JoinHandle<()>,
    /// Teardown signal: the reader's exit (or `close`) stores the permit.
    closed: Arc<tokio::sync::Notify>,
}

impl CdpConn {
    /// Connect and arm the single reader task.
    pub async fn connect(url: &str) -> CdpResult<Self> {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| CdpError::Io(e.to_string()))?;
        let (sink, mut stream) = ws.split();

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(tokio::sync::Notify::new());

        // The ONE reader: route by envelope shape — `id` -> its waiter,
        // otherwise -> the event stream. A response whose waiter timed
        // out is dropped silently, never mistaken for an event. When the
        // socket ends, `notify_one` stores the teardown permit so even a
        // later `wait_closed` returns at once.
        let p2 = Arc::clone(&pending);
        let closed2 = Arc::clone(&closed);
        let reader = tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                let Ok(Message::Text(t)) = msg else { break };
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                match v.get("id").and_then(Value::as_u64) {
                    Some(id) => {
                        let waiter = p2.lock().await.remove(&id);
                        if let Some(tx) = waiter {
                            let _ = tx.send(v);
                        }
                    }
                    None => {
                        let _ = events_tx.send(v);
                    }
                }
            }
            closed2.notify_one();
        });

        Ok(Self {
            write: Mutex::new(sink),
            pending,
            events: Mutex::new(events_rx),
            next_id: AtomicU64::new(1),
            reader,
            closed,
        })
    }

    /// Send a command and await its response (the reader task fills the
    /// oneshot). On timeout the waiter is unregistered here (a late
    /// response is then dropped by the router, never mistaken for an
    /// event).
    pub async fn call(&self, method: &str, params: Value) -> CdpResult<Value> {
        self.call_with_timeout(method, params, std::time::Duration::from_secs(10))
            .await
    }

    /// `call` with an explicit response timeout (tests and non-default
    /// probes; production callers use the 10s default).
    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> CdpResult<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let frame = json!({"id": id, "method": method, "params": params});
        self.write
            .lock()
            .await
            .send(Message::Text(frame.to_string().into()))
            .await
            .map_err(|e| CdpError::Io(e.to_string()))?;
        let got = tokio::time::timeout(timeout, rx).await;
        if got.is_err() {
            self.pending.lock().await.remove(&id);
            return Err(CdpError::Io(format!("{method} timed out")));
        }
        let resp = got
            .unwrap()
            .map_err(|_| CdpError::Io(format!("{method}: reader gone")))?;
        if let Some(err) = resp.get("error") {
            return Err(CdpError::Api(err.clone()));
        }
        Ok(resp.get("result").cloned().unwrap_or_else(|| json!({})))
    }

    /// The event stream (Target.* / Inspector.* / page-level events).
    /// Exactly one consumer expected (the instance watcher).
    pub async fn next_event(&self) -> Option<Value> {
        self.events.lock().await.recv().await
    }

    /// Close the socket and stop the reader.
    pub async fn close(self) {
        let _ = self.write.lock().await.send(Message::Close(None)).await;
        self.reader.abort();
        // aborted readers skip their last line — signal teardown anyway
        self.closed.notify_one();
    }

    /// Resolves when the reader task ends (socket died or `close` ran) —
    /// the watcher's teardown trigger, without consuming the connection.
    /// Mirrors the Python loop's recv-exception path. The reader signals
    /// `notify_one` on its last line; the stored permit makes this return
    /// even when the connection died before the call.
    pub async fn wait_closed(&self) {
        self.closed.notified().await;
    }
}

/// Convert a `Target.getTargets` / event `targetInfo` JSON into the
/// store-side snapshot (filters keep living in the watcher).
pub fn target_info_of(v: &Value) -> mudra_store::TargetInfo {
    let s = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| v.get(*k).and_then(Value::as_str))
            .unwrap_or_default()
            .to_string()
    };
    mudra_store::TargetInfo {
        target_id: s(&["targetId", "id"]),
        url: s(&["url"]),
        title: s(&["title"]),
        opener_id: s(&["openerId"]),
    }
}
