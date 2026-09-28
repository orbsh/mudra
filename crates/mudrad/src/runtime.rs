//! Runtime side-effect seam — everything `control` may DO to the outside
//! world (processes, chromium devtools endpoints, the window manager).
//!
//! Why sync: the control verbs only need three effects beyond process
//! management — close a target, activate a target, read the focused
//! window — and chromium's devtools HTTP endpoints expose exactly the
//! first two as GETs (`/json/close/<id>`, `/json/activate/<id>`). The
//! ws protocol (cdp.rs) is reserved for the watchers; commands never
//! need the event stream here. So the seam stays plain blocking code,
//! the trait has no async methods, and tests can fake it with closures
//! or record calls without a runtime.

use std::io::{Read, Write};
use std::process::Command;
use std::time::Duration;

/// Minimal blocking GET returning the parsed JSON body of a local devtools
/// endpoint. Blood lesson shared with cdp.rs: chromium's DevTools server
/// answers with Content-Length and KEEPS THE CONNECTION OPEN — reading to
/// EOF used to hang the full 5s timeout and fail even though the response
/// body was already in the buffer. Frame by Content-Length; EOF only when
/// no length header exists.
pub fn devtools_json(port: u16, path: &str) -> Result<serde_json::Value, String> {
    let mut sock = std::net::TcpStream::connect(("127.0.0.1", port))
        .map_err(|e| format!("connect {port}: {e}"))?;
    sock.set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    sock.write_all(req.as_bytes()).map_err(|e| format!("{path}: {e}"))?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let body: Vec<u8> = loop {
        match crate::cdp::body_complete(&buf) {
            crate::cdp::BodyState::Done(b) => break b.to_vec(),
            crate::cdp::BodyState::More => {
                let n = sock.read(&mut tmp).map_err(|e| format!("{path}: {e}"))?;
                if n == 0 {
                    return Err(format!("{path}: closed before body complete"));
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            crate::cdp::BodyState::ToEof => {
                sock.read_to_end(&mut buf).map_err(|e| format!("{path}: {e}"))?;
                let text = String::from_utf8_lossy(&buf);
                let b = text.split_once("\r\n\r\n").map(|(_, x)| x).unwrap_or(text.as_ref());
                break b.as_bytes().to_vec();
            }
        }
    };
    let text = String::from_utf8_lossy(&body);
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{path}: bad json: {e}"))?;
    Ok(json)
}

/// Everything control can do. `Real` talks to the machine; tests use
/// `Fake` (see tests/control_test.rs).
pub trait Runtime {
    /// Launch a NEW debug-port instance; returns the child pid.
    fn launch_new(&mut self, ctx: &str, url: &str, port: u16, proxy: Option<&str>, extensions: Option<&str>) -> Result<u32, String>;
    /// Join a live instance (--app window, no debug port).
    fn launch_join(&mut self, ctx: &str, url: &str, proxy: Option<&str>, extensions: Option<&str>) -> Result<(), String>;
    /// Ask chromium to close a target; the watcher's destroyed event
    /// performs the row teardown (single close path, like Python).
    fn close_target(&mut self, port: u16, target_id: &str) -> Result<(), String>;
    /// Bring a target to the front inside its window.
    fn activate_target(&mut self, port: u16, target_id: &str) -> Result<(), String>;
    /// True if the instance currently has this CDP target (extension
    /// tabId reverse lookup — the Python `_port_has_target` HTTP path).
    fn target_exists(&mut self, port: u16, target_id: &str) -> bool;
    /// SIGTERM the instance process (close_ctx; watcher teardown follows).
    fn kill(&mut self, pid: u32) -> Result<(), String>;
    /// Bring the instance's window to the front (niri; failure must not
    /// block the CDP activate — Python contract: best effort).
    fn focus_window(&mut self, pid: u32, title: &str, url: &str);
    /// Apply a remembered site width when the new window lands (spawn
    /// side effect; blocking wait included, same as _apply_site_width).
    fn apply_site_width(&mut self, pid: u32, proportion: f64);
    /// Push an epoch invalidation hint (the daemon wires the panel's
    /// live ws; a no-op when no panel is connected).
    fn notify(&mut self, epoch: u64);
    /// Screenshot a live target via page-level CDP (port of
    /// ctl.screenshot): returns the base64 PNG data URL, or Ok(None)
    /// when the capture is unavailable (page gone, no url). The Python
    /// path swallowed errors to None; the Rust seam keeps Err for
    /// transport failures so the verb can surface them in the panel log.
    fn screenshot(&mut self, port: u16, target_id: &str) -> Result<Option<String>, String>;
}

/// The machine-facing runtime: chromium spawn (PDEATHSIG-detached),
/// devtools HTTP verbs, `niri msg` for window management.
pub struct Real {
    pub profiles_dir: std::path::PathBuf,
    pub default_extensions: Vec<String>,
    pub niri_socket: Option<std::path::PathBuf>,
}

impl Real {
    fn niri(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        let mut cmd = Command::new("niri");
        cmd.args(args);
        if let Some(sock) = &self.niri_socket {
            cmd.env("NIRI_SOCKET", sock);
        }
        cmd.output()
    }

    fn launch(&self, ctx: &str, url: &str, port: Option<u16>, proxy: Option<&str>, extensions: Option<&str>) -> Result<u32, String> {
        let profile = self.profiles_dir.join(ctx);
        // extensions=None uses the defaults; Some(csv) splits into paths.
        let ext_list: Vec<String> = match extensions {
            Some(csv) => csv.split(',').map(str::to_string).collect(),
            None => self.default_extensions.clone(),
        };
        let mut cmd = crate::spawn::build_command(
            &crate::spawn::normalize_url(url),
            &profile,
            port,
            proxy,
            Some(&ext_list),
            &self.default_extensions,
        );
        crate::spawn::spawn_detached(&mut cmd).map_err(|e| e.to_string())
    }
}

impl Runtime for Real {
    fn launch_new(&mut self, ctx: &str, url: &str, port: u16, proxy: Option<&str>, extensions: Option<&str>) -> Result<u32, String> {
        self.launch(ctx, url, Some(port), proxy, extensions)
    }

    fn launch_join(&mut self, ctx: &str, url: &str, proxy: Option<&str>, extensions: Option<&str>) -> Result<(), String> {
        self.launch(ctx, url, None, proxy, extensions).map(|_| ())
    }

    fn close_target(&mut self, port: u16, target_id: &str) -> Result<(), String> {
        devtools_json(port, &format!("/json/close/{target_id}")).map(|_| ())
    }

    fn activate_target(&mut self, port: u16, target_id: &str) -> Result<(), String> {
        devtools_json(port, &format!("/json/activate/{target_id}")).map(|_| ())
    }

    fn target_exists(&mut self, port: u16, target_id: &str) -> bool {
        devtools_json(port, "/json")
            .ok()
            .and_then(|v| {
                v.as_array().map(|rows| {
                    rows.iter()
                        .any(|t| t["id"].as_str() == Some(target_id) && t["type"].as_str() == Some("page"))
                })
            })
            .unwrap_or(false)
    }

    fn kill(&mut self, pid: u32) -> Result<(), String> {
        // #[allow(unsafe_code)] rationale: bare kill(2) — the process was
        // spawned detached on purpose (the daemon must not reap it), so
        // std offers no handle; signalling by pid is the contract.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc == 0 {
            Ok(())
        } else {
            Err(format!("kill {pid}: {}", std::io::Error::last_os_error()))
        }
    }

    fn focus_window(&mut self, pid: u32, title: &str, url: &str) {
        // niri: find the instance's windows; prefer title/domain match,
        // fall back to the first (the Python _focus_instance_window rule).
        let Ok(out) = self.niri(&["-j", "windows"]) else { return };
        let Ok(ws) = serde_json::from_str::<Vec<serde_json::Value>>(&String::from_utf8_lossy(&out.stdout)) else { return };
        let domain = url.split("//").nth(1).map(|r| r.split('/').next().unwrap_or("").to_string()).unwrap_or_default();
        let mine: Vec<&serde_json::Value> = ws.iter().filter(|w| w["pid"].as_u64() == Some(pid as u64)).collect();
        let pick = mine.iter()
            .find(|w| (!title.is_empty() && w["title"].as_str() == Some(title)) || (!domain.is_empty() && w["title"].as_str().is_some_and(|t| t.contains(&domain))))
            .or_else(|| mine.first())
            .copied();
        if let Some(w) = pick
            && let Some(id) = w["id"].as_u64()
        {
            let _ = self.niri(&["action", "focus-window", "--id", &id.to_string()]);
        }
    }

    fn apply_site_width(&mut self, pid: u32, proportion: f64) {
        let pct = (proportion * 100.0).clamp(1.0, 99.0).round() as u32;
        let target = format!("{pct}%");
        // Wait for the new window to take focus (new windows grab focus),
        // then set its column width — same 30x100ms budget as Python.
        for _ in 0..30 {
            if let Ok(out) = self.niri(&["-j", "windows"])
                && let Ok(ws) = serde_json::from_str::<Vec<serde_json::Value>>(&String::from_utf8_lossy(&out.stdout))
                && ws.iter().any(|w| w["pid"].as_u64() == Some(pid as u64) && w["is_focused"].as_bool() == Some(true))
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = self.niri(&["action", "set-column-width", &target]);
    }

    fn notify(&mut self, _epoch: u64) {
        // The daemon wiring step replaces this with the panel ws push;
        // standing alone, mudrad's control plane has no panel attached.
    }

    fn screenshot(&mut self, port: u16, target_id: &str) -> Result<Option<String>, String> {
        // Port of ctl.screenshot: resolve the page-level ws url from
        // /json, one CDP round trip, base64 PNG -> data URL. The
        // blocking tungstenite client keeps the Runtime seam sync (the
        // hover-shot verb runs on the control thread under the store
        // lock discipline: verbs are fully synchronous by design).
        //
        // Lock discipline makes the read timeout load-bearing: `run_verb`
        // holds the store mutex for the whole verb, so a hung recv would
        // starve every other write. tungstenite's own `connect` hides the
        // TcpStream behind MaybeTlsStream (no way to set the timeout), so
        // for a plain ws:// devtools url we open the socket ourselves and
        // hand it to `tungstenite::client`.
        use tungstenite::{Message, client};
        let url = match devtools_json(port, "/json").ok().and_then(|v| {
            v.as_array().and_then(|rows| {
                rows.iter().find(|t| {
                    t["id"].as_str() == Some(target_id) && t["type"].as_str() == Some("page")
                })
                .and_then(|t| t["webSocketDebuggerUrl"].as_str())
                .map(str::to_string)
            })
        }) {
            Some(u) => u,
            // the target is not a live page right now: Python answered
            // None, not an error
            None => return Ok(None),
        };
        let host_port = url
            .strip_prefix("ws://")
            .and_then(|r| r.split('/').next())
            .ok_or_else(|| format!("unexpected devtools ws url: {url}"))?;
        let (host, p) = host_port
            .rsplit_once(':')
            .ok_or_else(|| format!("ws url lacks port: {host_port}"))?;
        let sock = std::net::TcpStream::connect((host, p.parse::<u16>().map_err(|e| e.to_string())?))
            .map_err(|e| format!("screenshot connect: {e}"))?;
        sock.set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|e| format!("screenshot timeout set: {e}"))?;
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("screenshot timeout set: {e}"))?;
        let (mut ws, _resp) =
            client(url.as_str(), sock).map_err(|e| format!("screenshot handshake: {e}"))?;
        let frame = serde_json::json!({"id": 1, "method": "Page.captureScreenshot", "params": {"format": "png"}});
        ws.send(Message::Text(frame.to_string().into()))
            .map_err(|e| format!("screenshot send: {e}"))?;
        // single-reader by construction here: one socket, one command —
        // route by envelope id, page-level events just get skipped.
        loop {
            let msg = ws.read().map_err(|e| format!("screenshot recv: {e}"))?;
            let Message::Text(t) = msg else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else { continue };
            if v["id"].as_u64() != Some(1) {
                continue;
            }
            let data = v["result"]["data"].as_str().map(str::to_string);
            let _ = ws.close(None);
            return Ok(data.map(|d| format!("data:image/png;base64,{d}")));
        }
    }
}
