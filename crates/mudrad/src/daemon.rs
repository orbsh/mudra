//! The daemon assembly — port of `mudrad.py main()` + `_start_services`:
//! the flock singleton lock, the control HTTP server, the panel static
//! server, the WS frame endpoint (bare `NestStorage` receiver + epoch
//! hint frames), and the instance-watcher scheduler.
//!
//! Lock discipline (the async+lock rule that shaped WatchSession): the
//! store mutex is held only for synchronous write bursts, never across
//! an await. Watchers await events unlocked, then step the session
//! inside a short lock; injections (CDP round trips) are awaited after
//! the lock is released. Verbs are fully synchronous by design, so the
//! per-verb lock never spans an await either.
//!
//! Blood lessons wired in:
//! - flock singleton: two mudrad daemons would double-sync pages and
//!   drift instance rows — refuse at startup, exit nonzero.
//! - session env: missing WAYLAND vars used to produce zombie chromium
//!   with stderr swallowed; `run` lists the gaps and refuses to start.
//! - never-ready: 12 attempts x 1s per instance (the Python budget),
//!   then mark_down and let the scheduler pick it up again later.
//! - CLOSE-WAIT pileup: panel ws mirrors the Python keepalive numbers
//!   (ping 15s / timeout 10s).

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch as tokio_watch, Mutex};
use tokio_tungstenite::tungstenite::Message;

use mudra_store::{MudraStore, TargetInfo};

use crate::cdp;
use crate::control::Controller;
use crate::spawn;
use crate::watch::WatchSession;

pub const CONTROL_PORT: u16 = 8899;
pub const PANEL_PORT: u16 = crate::control::PANEL_PORT;
pub const WS_PORT: u16 = PANEL_PORT + 1; // 9300, same layout as Python

/// Page-injection script (new-window interception): forwards window.open
/// and every left-click `<a href>` navigation to mudrad /open, so no
/// link escapes into a plain chromium window; same-page anchors jump in
/// place. Ported verbatim from mudrad.py `_INJECT_JS`.
pub const INJECT_JS: &str = r#"(() => {
  if (window.__mudraInjected) return; window.__mudraInjected = true;
  const CTX = "__CTX__";
  const EP = "http://127.0.0.1:8899/open";
  function openUrl(u) {
    if (!/^https?:/.test(u || "")) return;
    try { navigator.sendBeacon(EP, new Blob([JSON.stringify({url:u, ctx:CTX})], {type:"text/plain"})); } catch(e){}
  }
  const ow = window.open;
  window.open = function(u, n, f) {
    if (u && /^https?:/.test(u)) { openUrl(u); return null; }
    return ow.call(window, u, n, f);
  };
  document.addEventListener("click", function(e) {
    const a = e.target && e.target.closest ? e.target.closest("a") : null;
    if (!a || !a.href || !/^https?:/.test(a.href)) return;
    if (a.hash && a.pathname === location.pathname && a.search === location.search) return;
    e.preventDefault(); openUrl(a.href);
  }, true);
})();"#;

/// Shared daemon state: one store mutex (single-writer discipline), one
/// runtime, epoch fan-out, per-instance watcher bookkeeping.
pub struct Daemon {
    pub store: Arc<Mutex<MudraStore>>,
    pub rt: Arc<Mutex<crate::runtime::Real>>,
    pub epoch_tx: tokio_watch::Sender<u64>,
    /// instance_id -> watcher task's abort handle: never two watchers
    /// over one instance; a finished handle frees the slot for a retry.
    pub watched: Arc<Mutex<HashMap<u32, tokio::task::AbortHandle>>>,
    pub frontend_dir: std::path::PathBuf,
    /// Static root: the trunk build output (crates/mudra-panel/dist).
    /// R2 slice 2 switch: the served panel is the wasm bundle — the
    /// hyperscript tree it replaces is deleted in the same slice.
    pub panel_root: std::path::PathBuf,

    /// repo-shipped default `config.kdl` (Python DEFAULT_PATH == repo
    /// root == the home dir the NixOS install symlinks into the tree)
    pub config_default: std::path::PathBuf,
    /// Serve()-returned NestStorage: Send + Sync by construction
    /// (okm WS-CHANNEL); the WS adapter calls `apply` on shared handles.
    pub nest: Arc<okm_core::NestStorage<okm_core::FjallStore>>,
}

impl Daemon {
    fn publish_epoch(&self, epoch: u64) {
        let _ = self.epoch_tx.send(epoch);
    }
}

/// The flock singleton (port of `_acquire_lock`): LOCK_EX|LOCK_NB on
/// `<home>/mudrad.lock`; the returned handle keeps the lock alive for
/// the process lifetime (dropping it releases the lock).
///
/// `#[allow(unsafe_code)]` rationale: flock(2) — std exposes no
/// advisory-lock wrapper.
#[allow(unsafe_code)]
pub fn acquire_lock(dir: &std::path::Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let file = std::fs::File::options()
        .create(true)
        .write(true)
        .truncate(true) // lock file is a name-only artifact (same as Python's open("w"))
        .open(dir.join("mudrad.lock"))
        .map_err(|e| e.to_string())?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err("another daemon is already running".into());
    }
    Ok(file)
}

/// Boot check before ANY spawn: the Wayland session env. Missing vars
/// spawned zombie chromium with stderr swallowed; surface them at boot.
pub fn startup_env_gaps() -> Vec<&'static str> {
    spawn::session_env_missing(|k| std::env::var(k).ok())
}

// ================= control HTTP server =================

pub async fn serve_control(d: Arc<Daemon>) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", CONTROL_PORT)).await?;
    eprintln!("[mudrad] intercept server http://127.0.0.1:{CONTROL_PORT}");
    loop {
        let (sock, _) = listener.accept().await?;
        let d = Arc::clone(&d);
        tokio::spawn(async move {
            let _ = handle_control_conn(sock, d).await;
        });
    }
}

async fn handle_control_conn(mut sock: TcpStream, d: Arc<Daemon>) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = find_sub(&buf, b"\r\n\r\n") {
            break pos;
        }
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(()); // hung up mid-request
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > 64 * 1024 {
            return http_response(&mut sock, 431, b"oversized header", "text/plain").await;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
    let method = head.split_whitespace().next().unwrap_or("").to_string();
    let path = head
        .split_whitespace()
        .nth(1)
        .map(str::to_string)
        .unwrap_or_default();
    let want = buf[head_end + 4..].to_vec();
    let content_length = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
        .unwrap_or(0);
    let mut body = want;
    let mut sock = sock;
    while body.len() < content_length {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);

    if path == "/config" && method == "get" {
        let user = crate::config::user_config_path()
            .unwrap_or_else(|| std::path::PathBuf::from("/nonexistent"));
        let (status, payload) = config_response(&d.config_default, &user);
        return http_response(&mut sock, status, payload.as_bytes(), "application/json").await;
    }
    if method == "options" {
        // CORS preflight for the wasm panel: :9299 pages POST JSON to
        // :8899, and `Content-Type: application/json` is not a simple
        // request — the browser probes with OPTIONS first. The Python
        // panel never hit this (its writes rode the same-origin WS op
        // channel; the extension's fetch is exempt from CORS), so this
        // is the wasm panel's own new surface. 204 + the headers the
        // real POST already answers with (ACAO:*), plus the method and
        // header grants the probe checks.
        let head = "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\n\
                    Access-Control-Allow-Methods: POST, GET, OPTIONS\r\n\
                    Access-Control-Allow-Headers: Content-Type\r\n\
                    Access-Control-Max-Age: 86400\r\nConnection: close\r\n\r\n";
        sock.write_all(head.as_bytes()).await?;
        return sock.flush().await;
    }
    if method != "post" {
        return http_response(&mut sock, 404, br#"{"ok":false,"err":"not found"}"#, "application/json").await;
    }

    let req: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    let outcome = run_verb(&d, &path, req).await;
    let (status, payload) = match outcome {
        Ok(mut out) => {
            // Python's {"ok": True, **out}: merge the verb map at top level
            if let Some(obj) = out.as_object_mut() {
                obj.insert("ok".into(), json!(true));
            }
            (200, out)
        }
        Err(err) => {
            eprintln!("[mudrad] ctl err: {err}");
            (400, json!({"ok": false, "err": err}))
        }
    };
    http_response(&mut sock, status, payload.to_string().as_bytes(), "application/json").await
}

/// One verb against the shared store + runtime. Verbs are synchronous,
/// so the lock never spans an await (it's only taken for the call).
pub(crate) async fn run_verb(d: &Arc<Daemon>, path: &str, req: Value) -> Result<Value, String> {
    let mut store = d.store.lock().await;
    let mut rt = d.rt.lock().await;
    let out = {
        // store derefs by coercion (concrete field type); rt is generic,
        // so the explicit deref is required there.
        let mut c = Controller { store: &mut store, rt: &mut *rt };
        c.handle(path, &req)
    };
    if out.is_ok() {
        // the store is the epoch authority: publish whatever the verb
        // (or its runtime side effects) may have bumped
        d.publish_epoch(store.epoch());
    }
    out
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

async fn http_response(sock: &mut TcpStream, status: u16, body: &[u8], ctype: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        body.len()
    );
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(body).await?;
    sock.flush().await
}

/// The GET /config response: (status, json body). Two layers re-read
/// per request (Python parity: a config.kdl edit needs only a
/// panel/extension reload, not a daemon restart). A broken file is a
/// 500 with the error surfaced — never a silently empty config
/// (Python's `config: {e}` shape; the extension's syncConfig keeps its
/// chrome.storage values on any failure).
pub fn config_response(default_path: &std::path::Path, user_path: &std::path::Path) -> (u16, String) {
    match crate::config::load(default_path, user_path) {
        Ok(cfg) => (200, json!({"ok": true, "config": cfg}).to_string()),
        Err(e) => (500, json!({"ok": false, "err": format!("config: {e}")}).to_string()),
    }
}

// ================= panel static server =================

/// Route a request path under the wasm dist root. R2 slice 2 switch:
/// the panel is the trunk build (`crates/mudra-panel/dist`), served
/// flat — the old `/shared/*` → frontend remap died with the
/// hyperscript tree (the wasm bundle is self-contained; the extension
/// loads its shared libs over chrome-extension://, never HTTP).
///
/// Traversal denial: any `..` component is rejected outright
/// (canonicalize would chase symlinks out of the served tree).
/// Pure function so the routing contract has regression coverage
/// without binding port 9299 (the flock singleton would collide with
/// a live daemon).
pub fn resolve_static_path(panel_root: &std::path::Path, path: &str) -> Option<std::path::PathBuf> {
    if path.contains("..") {
        return None;
    }
    let rel = if path == "/" || path.is_empty() { "index.html" } else { path.trim_start_matches('/') };
    Some(panel_root.join(rel))
}

pub async fn serve_panel_static(d: Arc<Daemon>) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", PANEL_PORT)).await?;
    eprintln!("[mudrad] panel static http://127.0.0.1:{PANEL_PORT}");
    loop {
        let (mut sock, _) = listener.accept().await?;
        let panel = d.panel_root.clone();
        tokio::spawn(async move {
            let _ = handle_static_conn(&mut sock, &panel).await;
        });
    }
}

async fn handle_static_conn(sock: &mut TcpStream, panel_root: &std::path::Path) -> std::io::Result<()> {
    let mut buf = [0u8; 4096];
    let n = sock.read(&mut buf).await.unwrap_or(0);
    let head = String::from_utf8_lossy(&buf[..n]).to_string();
    let path = head
        .split_whitespace()
        .nth(1)
        .map(|p| p.split('?').next().unwrap_or(p).to_string())
        .unwrap_or_else(|| "/".into());

    let Some(rel) = resolve_static_path(panel_root, &path) else {
        return http_response(sock, 404, b"not found", "text/plain").await;
    };
    match tokio::fs::read(&rel).await {
        Ok(bytes) => {
            let ctype = match rel.extension().and_then(|e| e.to_str()) {
                Some("html") => "text/html; charset=utf-8",
                Some("js") => "text/javascript; charset=utf-8",
                Some("css") => "text/css",
                Some("json") => "application/json",
                Some("wasm") => "application/wasm", // browsers refuse any other mime for instantiation
                Some("png") => "image/png",
                Some("svg") => "image/svg+xml",
                _ => "application/octet-stream",
            };
            http_response(sock, 200, &bytes, ctype).await
        }
        Err(_) => http_response(sock, 404, b"not found", "text/plain").await,
    }
}

// ================= panel WS: bare NestStorage frames + epoch hints =================

pub async fn serve_panel_ws(d: Arc<Daemon>) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", WS_PORT)).await?;
    eprintln!("[mudrad] panel ws ws://127.0.0.1:{WS_PORT} (frames: bare NestStorage intake)");
    loop {
        let (sock, _) = listener.accept().await?;
        let d = Arc::clone(&d);
        tokio::spawn(async move { handle_ws_conn(sock, d).await });
    }
}

async fn handle_ws_conn(sock: TcpStream, d: Arc<Daemon>) {
    let Ok(ws) = tokio_tungstenite::accept_async(sock).await else { return };
    let (mut sink, mut stream) = ws.split();
    // keepalive mirrors the Python server (ping 15s / timeout 10s) —
    // dead peers get reaped instead of piling CLOSE-WAIT
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(15));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut epoch_rx = d.epoch_tx.subscribe();
    loop {
        tokio::select! {
            msg = stream.next() => {
                let Some(Ok(m)) = msg else { break };
                match m {
                    Message::Binary(bytes) => {
                        // transport-free intake: apply answers Some only
                        // for query ops (WS-CHANNEL shape A)
                        let resp = d.nest.apply(&bytes);
                        if let Some(resp) = resp
                            && sink.send(Message::Binary(resp.encode().into())).await.is_err()
                        {
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {} // pings/pongs handled inside tungstenite
                }
            }
            _ = ping.tick() => {
                if sink.send(Message::Ping("".into())).await.is_err() {
                    break; // dead peer
                }
            }
            changed = epoch_rx.changed() => {
                if changed.is_err() { break; }
                let hint = json!({"epoch": *epoch_rx.borrow()}).to_string();
                if sink.send(Message::Text(hint.into())).await.is_err() {
                    break;
                }
            }
        }
    }
}

// ================= watcher scheduler =================

/// One instance's watcher (port of `_watch`): retry-connect, baseline
/// snapshot (await only), short-locked write burst, then the event loop
/// — await unlocked, step locked. Stream end = teardown.
pub async fn watch_instance(d: Arc<Daemon>, instance_id: u32, port: u16, ctx: String) {
    let conn = match crate::watch::connect_ready(port, 12, std::time::Duration::from_secs(1)).await {
        Some(c) => Arc::new(c),
        None => {
            eprintln!("[mudrad] instance {instance_id} never ready on :{port}; marking down");
            teardown(&d, instance_id).await;
            return;
        }
    };

    let mut session = WatchSession::new(instance_id, port, &ctx);
    let infos: Vec<TargetInfo> = match session.start(&conn).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("[watcher] {ctx}: baseline failed: {e}");
            teardown(&d, instance_id).await;
            return;
        }
    };
    // injection is a CDP round trip: awaited OUTSIDE the store lock
    inject_pages(port, &ctx, &infos).await;
    {
        let mut store = d.store.lock().await;
        let mut bumped: Option<u64> = None;
        let mut noop = |_p: u16, _t: &str, _c: &str| {}; // pages injected above
        session.apply_baseline(&mut store, &infos, &mut noop, &mut |e| bumped = Some(e));
        if let Some(e) = bumped {
            d.publish_epoch(e);
        }
    }

    // event loop: await outside the lock, apply inside
    loop {
        let ev = match conn.next_event().await {
            Some(ev) => ev,
            None => break, // stream end = instance gone
        };
        let created: Vec<String> = {
            let mut store = d.store.lock().await;
            let mut injected = Vec::new();
            let mut bumped: Option<u64> = None;
            let mut inject = |_p: u16, t: &str, _c: &str| injected.push(t.to_string());
            session.on_event(&mut store, &ev, &mut inject, &mut |e| bumped = Some(e));
            if let Some(e) = bumped {
                d.publish_epoch(e);
            }
            injected
        };
        if !created.is_empty() {
            // created pages get the interception script; resolve ws urls
            // through /json like the Python _inject_page does
            if let Ok(rows) = cdp::list_targets(port).await {
                let infos: Vec<TargetInfo> = rows
                    .iter()
                    .filter(|t| {
                        t.get("id").and_then(Value::as_str).is_some_and(|id| created.iter().any(|c| c == id))
                    })
                    .map(cdp::target_info_of)
                    .collect();
                inject_pages(port, &ctx, &infos).await;
            }
        }
    }
    teardown(&d, instance_id).await;
}

/// Stream-end/never-ready teardown under one short lock.
async fn teardown(d: &Daemon, instance_id: u32) {
    let mut store = d.store.lock().await;
    let mut bumped: Option<u64> = None;
    WatchSession::teardown(&mut store, instance_id, &mut |e| bumped = Some(e));
    if let Some(e) = bumped {
        d.publish_epoch(e);
    }
    d.watched.lock().await.remove(&instance_id);
}

/// inject = per-page CDP round trip (awaited OUTSIDE the store lock):
/// resolve page ws urls via /json, arm the interception script for new
/// documents and run it into already-loaded pages.
pub async fn inject_pages(port: u16, ctx: &str, infos: &[TargetInfo]) {
    if infos.is_empty() {
        return;
    }
    let rows = match cdp::list_targets(port).await {
        Ok(r) => r,
        Err(_) => return, // instance vanished mid-sync; the loop ends soon
    };
    for t in infos {
        let Some(ws_url) = rows.iter().find_map(|r| {
            (r.get("id").and_then(Value::as_str) == Some(t.target_id.as_str())
                && r.get("type").and_then(Value::as_str) == Some("page"))
                .then(|| r.get("webSocketDebuggerUrl").and_then(Value::as_str).map(str::to_string))
                .flatten()
        }) else {
            continue;
        };
        let Ok(conn) = cdp::CdpConn::connect(&ws_url).await else { continue };
        let src = INJECT_JS.replace("__CTX__", ctx);
        let _ = conn
            .call("Page.addScriptToEvaluateOnNewDocument", json!({"source": src.clone()}))
            .await;
        let _ = conn.call("Runtime.evaluate", json!({"expression": src})).await;
    }
}

/// The scheduler loop (port of `run()`): every 2s, spawn watchers for
/// running, /proc-alive instances not already watched. The watcher owns
/// its teardown; a finished handle frees the slot for the next round.
pub async fn scheduler(d: Arc<Daemon>) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        ticker.tick().await;
        let candidates: Vec<(u32, u16, String)> = {
            let store = d.store.lock().await;
            store
                .instances
                .scan_keys()
                .into_iter()
                .filter_map(|k| store.instances.get(&k).map(|i| (k.id, i)))
                .filter(|(_, i)| i.running == 1 && spawn::pid_alive(i.pid) && i.port > 0)
                .map(|(id, i)| (id, i.port as u16, i.profile))
                .collect()
        };
        let mut watched = d.watched.lock().await;
        watched.retain(|_, h| !h.is_finished());
        for (id, port, ctx) in candidates {
            if watched.contains_key(&id) {
                continue;
            }
            let d2 = Arc::clone(&d);
            let handle = tokio::spawn(async move { watch_instance(d2, id, port, ctx).await });
            watched.insert(id, handle.abort_handle());
        }
    }
}

// ================= main =================

pub async fn run() -> Result<(), String> {
    let gaps = startup_env_gaps();
    if !gaps.is_empty() {
        return Err(format!(
            "session env incomplete: {gaps:?} — a daemon without WAYLAND spawns zombie chromium"
        ));
    }
    let home = spawn::mudra_home();
    let _lock = acquire_lock(&home)?; // held for the process lifetime

    let store = MudraStore::open(&home.join("store")).map_err(|e| format!("store open: {e}"))?;
    let current_epoch = store.epoch();
    let frontend = std::env::var_os("MUDRA_FRONTEND_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            // dev default: the repo checkout (source is the artifact, the
            // NixOS install symlinks into the tree, so repo-relative
            // paths resolve through the home symlink)
            home.join("frontend")
        });
    // R2 slice 2: the served panel = trunk build output. Same
    // dev/NixOS reasoning as `frontend`; MUDRA_PANEL_DIST overrides.
    let panel_root = std::env::var_os("MUDRA_PANEL_DIST")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join("crates/mudra-panel/dist"));
    let real = crate::runtime::Real {
        profiles_dir: home.join("profiles"),
        default_extensions: vec![frontend.display().to_string()],
        niri_socket: niri_socket(),
        // projection of the State DEV_MODE slot; the /dev verb keeps it
        // in sync after every switch (the store stays the fact source)
        dev_mode: store.state_text(mudra_store::state::DEV_MODE) == "1",
    };
    // bare receiver: panel frames execute byte-identical on the engine
    // (no prefix), SCHEMA's ns numbers are the shared contract
    let (host, _handle) = okm_core::NestStorage::new(store.engine(), &[]);
    // serve() pumps in its own thread; the returned Arc is Send+Sync
    // by construction (okm WS-CHANNEL), so the ws tasks share it directly.
    let nest = host.serve();

    let (epoch_tx, _) = tokio_watch::channel(current_epoch);
    let d = Arc::new(Daemon {
        store: Arc::new(Mutex::new(store)),
        rt: Arc::new(Mutex::new(real)),
        epoch_tx,
        watched: Arc::new(Mutex::new(HashMap::new())),
        frontend_dir: frontend,
        panel_root,
        config_default: home.join("config.kdl"),
        nest,
    });

    { let d2 = Arc::clone(&d); tokio::spawn(async move { serve_control(d2).await.expect("control server") }); }
    { let d2 = Arc::clone(&d); tokio::spawn(async move { serve_panel_static(d2).await.expect("static server") }); }
    { let d2 = Arc::clone(&d); tokio::spawn(async move { serve_panel_ws(d2).await.expect("ws server") }); }
    { let d2 = Arc::clone(&d); tokio::spawn(async move { scheduler(d2).await }); }
    { let d2 = Arc::clone(&d); tokio::spawn(async move { crate::host::supervise(d2).await }); }

    eprintln!("[mudrad] started");
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("signal")
        .recv()
        .await;
    let store = d.store.lock().await;
    let _ = store.persist();
    eprintln!("[mudrad] persisted, bye");
    Ok(())
}

/// NIRI_SOCKET discovery: env wins, else scan /run/user/<uid>/niri*.sock
/// (the agent-shell lesson: niri CLI needs the socket, real sessions
/// export it but shells started outside the session do not).
/// `#[allow(unsafe_code)]`: one read-only getuid(2); std exposes no uid getter.
#[allow(unsafe_code)]
fn niri_socket() -> Option<std::path::PathBuf> {
    if let Some(s) = std::env::var_os("NIRI_SOCKET") {
        return Some(std::path::PathBuf::from(s));
    }
    let uid = unsafe { libc::getuid() };
    let dir = std::path::Path::new("/run/user").join(uid.to_string());
    std::fs::read_dir(dir).ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .find(|n| n.starts_with("niri") && n.ends_with(".sock"))
        .map(|n| std::path::Path::new("/run/user").join(uid.to_string()).join(n))
}
