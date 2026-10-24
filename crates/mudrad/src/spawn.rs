//! chromium instance launching — the port of `mudralib/spawn.py` plus the
//! process-lifecycle helpers the daemon watchers need.
//!
//! Blood-lesson inventory carried from the Python tree (PLAN §11 R1
//! deliverable; each maps to a test in `tests/spawn_test.rs`):
//!
//! - zombie pid: liveness must read `/proc/<pid>/stat` state after the
//!   (space-containing) comm field — `kill(pid, 0)` succeeds for zombies
//!   and would make /open join a dead instance.
//! - SingletonLock: a stale lock pointing at a dead pid makes a new
//!   spawn hand off to the corpse and exit silently.
//! - extension caches: five separate chromium cache points keep serving
//!   old extension code after source edits; dev_mode clears them pre-spawn.
//! - WAYLAND env: a daemon started without the session env spawns
//!   chromium that dies instantly (into zombies, with stderr swallowed) —
//!   so the missing vars are surfaced before any launch.
//! - PDEATHSIG: chromium children die with mudrad (SIGTERM), so a crashed
//!   daemon never leaves orphans holding profile singletons.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

/// The five cache points chromium keeps for `--load-extension` source dirs
/// (measured in the Python tree: bumping manifest version is NOT enough).
pub const EXTENSION_CACHE_POINTS: [&str; 5] = [
    "Default/Service Worker/ScriptCache",
    "Default/Code Cache",
    "Default/Cache",
    "Default/Extension Scripts",
    "Default/Extension Rules",
];

/// Script caches only — no HTTP cache. These are the ones that can hand a
/// window the content script it loaded last time: chromium caches content
/// scripts per profile, so a window opened before the extension sources changed
/// keeps running the old code even after a reload.
pub const EXTENSION_SCRIPT_CACHE_POINTS: [&str; 4] = [
    "Default/Service Worker/ScriptCache",
    "Default/Code Cache",
    "Default/Extension Scripts",
    "Default/Extension Rules",
];

/// Session env chromium needs on this desktop (niri/Wayland). Missing any
/// of these is the known cause of zombie-on-launch.
pub const SESSION_ENV: [&str; 4] = [
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
];

/// Session-env diagnostic as a pure lookup so it stays unit-testable
/// (callers pass `|k| std::env::var(k).ok()`). Returns the missing names.
pub fn session_env_missing(lookup: impl Fn(&str) -> Option<String>) -> Vec<&'static str> {
    SESSION_ENV
        .iter()
        .filter(|k| lookup(k).is_none())
        .copied()
        .collect()
}

/// First bindable loopback port in `[start, start+span)`.
/// (The pkill self-match lesson has no Rust analogue: ports are probed by
/// bind, never by pattern-matching command lines.)
pub fn free_port(start: u16, span: u16) -> io::Result<u16> {
    for offset in 0..span {
        let port = start + offset;
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(io::Error::other("no free port in range"))
}

/// Add https:// when the scheme is missing (bare hosts/domains -> https;
/// http/IP keeps requiring an explicit scheme). A single-label host gains
/// `.com`: an address bar's "bing" means bing.com, not the dead host
/// `https://bing` that used to be opened (and recorded) as a visit.
pub fn normalize_url(url: &str) -> String {
    if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{}", expand_bare_host(url))
    }
}

/// `.com` for a one-label host, everything else untouched: dotted hosts and
/// IPs, a port, userinfo and `localhost` are already addresses the user can
/// mean literally. The host is whatever precedes the first `/`, `?` or `#`
/// (an address-bar input may carry a path).
fn expand_bare_host(input: &str) -> String {
    let head_len = input.find(['/', '?', '#']).unwrap_or(input.len());
    let (head, tail) = input.split_at(head_len);
    let bare = !head.is_empty()
        && !head.contains('.')
        && !head.contains('@')
        && !head.contains(':')
        && !head.eq_ignore_ascii_case("localhost");
    if bare {
        format!("{head}.com{tail}")
    } else {
        input.to_string()
    }
}

/// `/proc`-backed liveness. A zombie (exited, unreaped child of mudrad) is
/// DEAD: its pid answers `kill(pid, 0)` but the instance is gone — exactly
/// the false positive that made /open join dead instances in Python.
pub fn pid_alive(pid: u32) -> bool {
    match std::fs::read(format!("/proc/{pid}/stat")) {
        Ok(stat) => {
            // `comm` is parenthesized and may contain spaces or ')'; the
            // state field is the first token after the LAST ')'.
            match stat.rsplit(|b| *b == b')').next() {
                Some(rest) if rest.len() > 2 => rest[1] != b'Z', // skip space, read state
                _ => false,
            }
        }
        Err(_) => false,
    }
}

/// The dead pid behind a profile's `SingletonLock`, if any. The link
/// target is `hostname-pid`; mudrad may only launch into a profile whose
/// lock is absent or stale-cleared.
pub fn stale_singleton_lock(profile_dir: &Path) -> Option<u32> {
    let link = profile_dir.join("SingletonLock");
    let target = std::fs::read_link(&link).ok()?;
    let name = target.file_name()?.to_str()?;
    let pid: u32 = name.rsplit('-').next()?.parse().ok()?;
    (!pid_alive(pid)).then_some(pid)
}

/// Remove the extension cache points under a profile dir (dev_mode: source
/// edits must be visible on the next cold start).
pub fn clear_extension_caches(profile_dir: &Path) -> usize {
    clear_caches_at(profile_dir, &EXTENSION_CACHE_POINTS)
}

/// The console panel's variant: the panel is a detached window that outlives
/// daemon restarts, so it can easily be older than the extension sources on
/// disk — and a stale console looks exactly like a broken key (the command
/// simply does not exist there). Clearing the script caches on every panel
/// spawn costs one extension parse and removes that whole class of confusion;
/// the HTTP cache is left alone.
pub fn clear_extension_script_caches(profile_dir: &Path) -> usize {
    clear_caches_at(profile_dir, &EXTENSION_SCRIPT_CACHE_POINTS)
}

fn clear_caches_at(profile_dir: &Path, points: &[&str]) -> usize {
    let mut cleared = 0;
    for sub in points {
        let p = profile_dir.join(sub);
        if p.exists() && std::fs::remove_dir_all(&p).is_ok() {
            cleared += 1;
        }
    }
    cleared
}

/// Flags shared by new-instance and join launches.
fn base_args(app_url: &str, profile_dir: &Path) -> Vec<String> {
    vec![
        format!("--app={app_url}"),
        format!("--user-data-dir={}", profile_dir.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--enable-extensions".into(),
        // chrome:// pages are a real destination (history, settings, downloads),
        // and an extension is otherwise refused there — the keyboard mode simply
        // does not exist on them. The switch is honoured by unbranded Chromium
        // only (Google Chrome ignores it); this build is unbranded, verified by
        // a content script landing on chrome://version. The manifest needs the
        // matching "chrome://*/*" entry: <all_urls> does not cover the scheme.
        "--extensions-on-chrome-urls".into(),
    ]
}

/// Build the chromium command. `debug_port` only on new instances (the
/// join path deliberately has none — it is a handoff to the running one).
/// `extensions = None` uses `default_extensions`, mirroring Python's
/// `None -> DEFAULT_EXTENSIONS` rule; `Some(vec![])` means no extension.
pub fn build_command(
    app_url: &str,
    profile_dir: &Path,
    debug_port: Option<u16>,
    proxy: Option<&str>,
    extensions: Option<&[String]>,
    default_extensions: &[String],
) -> std::process::Command {
    let mut cmd = std::process::Command::new("chromium");
    let mut args = base_args(app_url, profile_dir);
    if let Some(port) = debug_port {
        args.push(format!("--remote-debugging-port={port}"));
    }
    let exts: &[String] = extensions.unwrap_or(default_extensions);
    if !exts.is_empty() {
        args.push(format!("--load-extension={}", exts.join(",")));
    }
    if let Some(p) = proxy {
        args.push(format!("--proxy-server={p}"));
    }
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        // stderr is NOT devnull here: the Python tree swallowed it and every
        // spawn failure was silent. Kept for the log pump in daemon wiring.
        .stderr(std::process::Stdio::piped());
    cmd
}

/// Detach a launch from the daemon's lifecycle while keeping it waitable:
/// PR_SET_PDEATHSIG(SIGTERM) + setsid in the child between fork and exec.
/// Returns the child pid.
///
/// `#[allow(unsafe_code)]` rationale: `pre_exec` is an `unsafe fn` by
/// contract (runs after fork in a threaded process); the closure body is
/// limited to async-signal-safe `prctl`/`setsid` calls.
#[allow(unsafe_code)]
pub fn spawn_detached(cmd: &mut std::process::Command) -> io::Result<u32> {
    let child = unsafe {
        cmd.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })
        .spawn()?
    };
    Ok(child.id())
}

/// True when the process's /proc cmdline contains `needle` (panel window
/// identification by process identity, never title — the Python rule).
/// Substring, not whole-arg: the panel is found by a flag that carries a
/// path (`--user-data-dir=…/panel-profile`), which no argv entry ever
/// equals — an equality test here made Win+Q open a second panel every
/// time while the running one sat there unmatched.
pub fn cmdline_has(pid: u64, needle: &str) -> bool {
    let n = needle.as_bytes();
    if n.is_empty() {
        return false;
    }
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|raw| {
            raw.split(|b| *b == 0)
                .any(|arg| arg.windows(n.len()).any(|w| w == n))
        })
        .unwrap_or(false)
}

/// Detach WITHOUT PDEATHSIG: the panel window must outlive daemon
/// restarts (Python ui.launch used start_new_session only).
/// `#[allow(unsafe_code)]`: same fork-window prctl/setsid contract as
/// spawn_detached, minus the death signal.
#[allow(unsafe_code)]
pub fn spawn_detached_session(cmd: &mut std::process::Command) -> io::Result<u32> {
    let child = unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })
        .spawn()?
    };
    Ok(child.id())
}

/// Data directory layout: `MUDRA_HOME` overrides (tests), else
/// `~/.local/share/mudra` — same root the fjall store and profiles live under.
///
/// Fallible because the fallback IS the environment: a session with neither
/// variable set has no data directory to name, and inventing one (cwd-relative,
/// /tmp) would scatter state instead of reporting the gap.
pub fn mudra_home() -> Result<PathBuf, String> {
    if let Some(v) = std::env::var_os("MUDRA_HOME") {
        return Ok(PathBuf::from(v));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| "neither MUDRA_HOME nor HOME is set: no data directory".to_string())?;
    Ok(PathBuf::from(home).join(".local/share/mudra"))
}

pub fn profile_dir(name: &str) -> Result<PathBuf, String> {
    Ok(mudra_home()?.join("profiles").join(name))
}
