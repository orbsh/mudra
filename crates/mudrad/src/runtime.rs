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
/// endpoint (chromium always closes the response → EOF frames the body).
pub fn devtools_json(port: u16, path: &str) -> Result<serde_json::Value, String> {
    let mut sock = std::net::TcpStream::connect(("127.0.0.1", port))
        .map_err(|e| format!("connect {port}: {e}"))?;
    sock.set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    sock.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    sock.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf);
    if !text.starts_with("HTTP/1.1 2") {
        return Err(format!("{path} -> {}", text.lines().next().unwrap_or("no response")));
    }
    serde_json::from_str(text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(""))
        .map_err(|e| format!("{path}: bad json: {e}"))
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
}
