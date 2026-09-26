//! spawn tests — the blood-lesson port from the Python tree, each test
//! demonstrating one lifecycle contract (PLAN §11 R1 deliverable).
//! Real processes and real files; no mocks.

use mudrad::spawn::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

// ================= zombie liveness (the /proc lesson) =================

#[test]
fn zombie_pid_is_dead() {
    // Contract: an exited-but-unreaped child answers kill(pid,0) and
    // /proc — but it is DEAD. The Python bug: /open joined instances
    // whose pid had become a zombie. Unreaped here via mem::forget, so
    // the child parks in Z state.
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit"])
        .spawn()
        .unwrap();
    let pid = child.id();
    std::mem::forget(child); // never wait() -> the exit becomes a zombie

    for _ in 0..100 {
        if let Ok(stat) = std::fs::read(format!("/proc/{pid}/stat"))
            && stat.windows(3).any(|w| w == b") Z")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!pid_alive(pid), "zombie pid must count as dead");
}

#[test]
#[allow(unsafe_code)] // test cleanup: signal the sh babysitter directly
fn comm_containing_parens_does_not_shift_the_state_field() {
    // Contract: parse from the LAST ')' — comm is `basename(1)` of the
    // executable and may itself contain ')' (Python split at the first,
    // working only by accident). A binary literally named `z)ombie)`
    // proves the robust parse: zombie -> dead, never misread.
    //
    // Shape: an sh child backgrounds the named binary (exits instantly)
    // and then sleeps without reaping -> stable zombie whose parent is
    // sh, not the test process (the Rust runtime's SIGCHLD reaper can
    // never race us, and no raw fork inside a threaded test binary).
    let dir = std::env::temp_dir().join(format!("mudra-spawn-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("z)ombie)");
    std::fs::copy("/bin/sh", &exe).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = dir.join("pid");

    let script = format!(
        "'{exe}' -c 'exit 0' & echo $! > '{report}'; sleep 60",
        exe = exe.display(),
        report = report.display(),
    );
    let mut sh = std::process::Command::new("/bin/sh")
        .args(["-c", &script])
        .spawn()
        .expect("spawn sh");

    let zpid = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(s) = std::fs::read_to_string(&report) {
                let t = s.trim();
                if !t.is_empty() {
                    break t.parse::<u32>().expect("pid reported");
                }
            }
            assert!(deadline.elapsed() < std::time::Duration::from_secs(5), "sh never reported the child pid");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    };

    // $! is reported when BACKGROUNDED, not when exited: poll until the
    // named child finishes and parks in Z (sh never waits, so the zombie
    // persists once formed).
    let stat = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let s = std::fs::read(format!("/proc/{zpid}/stat")).unwrap_or_default();
            if s.windows(3).any(|w| w == b") Z") {
                break s;
            }
            assert!(
                deadline.elapsed() < std::time::Duration::from_secs(3),
                "test setup: the named child must sit in Z (comm has a paren)"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(
            stat.windows(3).any(|w| w == b") Z"),
            "test setup: the named child must sit in Z (comm has a paren)"
        );
        assert!(!pid_alive(zpid), "paren-containing comm must not fool the parser");
    }));

    // cleanup: kill sh; the zombie reparents to init and gets reaped
    unsafe {
        libc::kill(sh.id() as libc::pid_t, libc::SIGKILL);
        let _ = sh.wait();
    }
    let _ = std::fs::remove_dir_all(&dir);
    outcome.unwrap_or_else(|e| std::panic::resume_unwind(e));
}

#[test]
fn own_pid_is_alive_and_missing_pid_is_dead() {
    assert!(pid_alive(std::process::id()));
    // pid 0 is never a real instance pid; an absurdly high pid is absent
    assert!(!pid_alive(0));
    assert!(!pid_alive(u32::MAX));
}

// ================= SingletonLock (the silent handoff lesson) =================

#[test]
fn stale_singleton_lock_is_reported_but_live_one_is_not() {
    // Contract: a profile whose SingletonLock points at a dead pid must
    // be surfaced BEFORE launching — launching into it is the silent
    // handoff + instant exit the Python tree hit.
    let dir = std::env::temp_dir().join(format!("mudra-lock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // dead pid: nothing owns pid u32::MAX
    std::os::unix::fs::symlink(format!("testhost-{}", u32::MAX), dir.join("SingletonLock"))
        .unwrap();
    assert_eq!(stale_singleton_lock(&dir), Some(u32::MAX));

    // live pid: this process itself
    std::fs::remove_file(dir.join("SingletonLock")).unwrap();
    std::os::unix::fs::symlink(
        format!("testhost-{}", std::process::id()),
        dir.join("SingletonLock"),
    )
    .unwrap();
    assert_eq!(stale_singleton_lock(&dir), None);

    // absent lock: nothing to report
    std::fs::remove_file(dir.join("SingletonLock")).unwrap();
    assert_eq!(stale_singleton_lock(&dir), None);
    let _ = std::fs::remove_dir_all(&dir);
}

// ================= extension cache points (dev_mode lesson) =================

#[test]
fn clear_extension_caches_removes_all_five_points() {
    // Contract: dev_mode clears exactly the five measured cache points;
    // bumping the manifest version is not enough (chromium serves old
    // SW/content code from them).
    let dir = std::env::temp_dir().join(format!("mudra-cache-{}", std::process::id()));
    for sub in EXTENSION_CACHE_POINTS {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
        std::fs::write(dir.join(sub).join("blob"), b"cached").unwrap();
    }
    assert_eq!(EXTENSION_CACHE_POINTS.len(), 5, "the measured five, no fewer");

    let cleared = clear_extension_caches(&dir);
    assert_eq!(cleared, 5);
    for sub in EXTENSION_CACHE_POINTS {
        assert!(!dir.join(sub).exists(), "{sub} must be gone");
    }

    // missing dirs are a no-op (not an error)
    assert_eq!(clear_extension_caches(&dir), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

// ================= session env (WAYLAND lesson) =================

#[test]
fn session_env_gaps_are_listed_before_launch() {
    // Contract: the missing-vars diagnostic is a pure lookup — the
    // zombie-on-launch failure needs the gap surfaced first, not a
    // post-mortem. (Production callers pass std::env::var.)
    let full = |k: &str| (k != "NOPE").then(|| format!("<{k}>"));
    assert!(session_env_missing(full).is_empty());

    let gaps = session_env_missing(|k| (k != "WAYLAND_DISPLAY" && k != "DBUS_SESSION_BUS_ADDRESS").then(|| "x".into()));
    assert_eq!(gaps, ["WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS"]);

    // the real environment on a desktop session may legitimately vary,
    // but the four names are the fixed set
    assert_eq!(SESSION_ENV.len(), 4);
}

// ================= url / port helpers =================

#[test]
fn normalize_url_adds_https_only_when_scheme_missing() {
    assert_eq!(normalize_url("example.com"), "https://example.com");
    assert_eq!(normalize_url("http://10.0.0.1:8080"), "http://10.0.0.1:8080");
    assert_eq!(normalize_url("https://a.test/x"), "https://a.test/x");
}

#[test]
fn free_port_skips_occupied_ports() {
    // Bind one port to force the scan to move on; returned port must be
    // bindable by the real caller afterwards (prototype TOCTOU accepted,
    // same contract as the Python probe-by-bind).
    let first = free_port(19_800, 50).expect("range has slack");
    let occupied = std::net::TcpListener::bind(("127.0.0.1", first)).unwrap();
    let next = free_port(19_800, 50).expect("range has slack");
    assert_ne!(next, first);
    // range exhaustion is an error, not a fallback port
    drop(occupied);
    let listener = std::net::TcpListener::bind(("127.0.0.1", next)).unwrap();
    drop(listener);
}

// ================= command assembly =================

fn args_of(cmd: &std::process::Command) -> Vec<String> {
    cmd.get_args().map(|a| a.to_str().unwrap().to_string()).collect()
}

#[test]
fn new_instance_command_carries_debug_port_and_extension_defaults() {
    let profile = Path::new("/tmp/prof/work");
    let exts = vec!["/repo/frontend".to_string()];
    let cmd = build_command(
        "https://a.test",
        profile,
        Some(9201),
        Some("http://127.0.0.1:7890"),
        None, // None -> default extensions (the join path's recorded ext may be None too)
        &exts,
    );
    let args = args_of(&cmd);
    assert!(args.contains(&"--app=https://a.test".to_string()));
    assert!(args.contains(&"--user-data-dir=/tmp/prof/work".to_string()));
    assert!(args.iter().any(|a| a == "--remote-debugging-port=9201"));
    assert!(args.iter().any(|a| a == "--load-extension=/repo/frontend"));
    assert!(args.iter().any(|a| a == "--proxy-server=http://127.0.0.1:7890"));
    // (stderr policy is fixed inside build_command — std exposes no Stdio
    // query; the piped-not-null choice is a code fact, not a data fact.)
}

#[test]
fn join_command_omits_debug_port_and_explicit_empty_extensions_suppress_flag() {
    let cmd = build_command(
        "https://b.test",
        Path::new("/tmp/prof/inbox"),
        None,
        None,
        Some(&[]), // Some(empty): "no extensions", distinct from None=default
        &["/default/ext".to_string()],
    );
    let args = args_of(&cmd);
    assert!(!args.iter().any(|a| a.starts_with("--remote-debugging-port")));
    assert!(!args.iter().any(|a| a.starts_with("--load-extension")));
    assert!(!args.iter().any(|a| a.starts_with("--proxy-server")));
}

// ================= PDEATHSIG (the orphan lesson) =================

#[test]
#[allow(unsafe_code)] // test harness: bare fork to exercise parent-death signal
fn detached_child_dies_when_its_parent_thread_exits() {
    // Contract: spawn_detached arms PR_SET_PDEATHSIG(SIGTERM); a crashed
    // or restarted daemon leaves no chromium orphans holding profile
    // singletons. Test shape: fork a sub-parent that spawns `sleep 30`
    // through spawn_detached and reports the grandchild pid, then exits;
    // the grandchild must be gone shortly after.
    let report = std::env::temp_dir().join(format!("mudra-pdeath-{}", std::process::id()));
    let _ = std::fs::remove_file(&report);

    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // sub-parent: spawn detached, report, exit -> grandchild gets SIGTERM
        let mut cmd = std::process::Command::new("sleep"); // PATH lookup (NixOS: no /bin/sleep)
        cmd.arg("30");
        match spawn_detached(&mut cmd) {
            Ok(gp) => {
                let _ = std::fs::write(&report, gp.to_string());
                unsafe { libc::_exit(0) };
            }
            Err(e) => {
                let _ = std::fs::write(&report, format!("ERR {e}"));
                unsafe { libc::_exit(1) };
            }
        }
    }
    assert!(pid > 0, "fork failed");
    let mut status = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };

    let grand: u32 = std::fs::read_to_string(&report)
        .expect("report written")
        .trim()
        .parse()
        .expect("pid reported");
    let _ = std::fs::remove_file(&report);

    // poll up to 3s for the death signal to land
    for _ in 0..300 {
        if !pid_alive(grand) {
            return; // PDEATHSIG fired: the contract holds
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    unsafe { libc::kill(grand as libc::pid_t, libc::SIGKILL) }; // cleanup on failure
    panic!("detached child survived its parent: PDEATHSIG not armed");
}
