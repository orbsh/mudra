//! The live-test guest: an Induction guest hand-written against
//! `induction-protocol` (no guest facade — the wire shapes stay visible),
//! declared as a bin of this package so cargo links it for the live test and
//! exports its path (`CARGO_BIN_EXE_…`) — the test can never run a stale
//! fixture.
//!
//! Behavior: declares `mudra:page_open` in the hello (§3, the
//! script-carried schema); on the first delivered call it records the
//! received frame, routes one `host:invoke` frame at the mudra verb surface
//! (§5 — effects execute in mudra), persists the `host_reply`, answers the
//! delivered call with a `result` frame, and exits. ONE stdin lock for the
//! whole session: `Stdin::lock` is a non-reentrant mutex, so a second
//! `stdin().lock()` inside the loop would deadlock against the loop's own
//! lock (this cost the first live run its reply).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unwrap_in_result, clippy::panic_in_result_fn)]

use std::io::{BufRead, Write};

fn main() {
    // absolute (CARGO_MANIFEST_DIR pins both sides to the same files — a
    // relative path would resolve against the fixture's cwd). The dir sits
    // under the WORKSPACE target/ (gitignored), not the crate dir — a
    // crate-local target/ would not be ignored and its artifacts would
    // pollute `git status`.
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/induction-live"
    );
    std::fs::create_dir_all(dir).ok();
    let seen_path = format!("{dir}/seen.json");
    let out_path = format!("{dir}/out.json");

    let hello = induction_protocol::Hello::from_on(
        "probe",
        [
            "mudra:page_open".to_string(),
            "mudra:before_navigate".to_string(),
            "mudra:fill_password".to_string(),
        ],
    );
    println!("{}", hello.to_line());
    let nav_path = format!("{dir}/nav.json");

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    loop {
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => break, // EOF or read error: done
            Ok(_) => {}
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else { continue };
        if v["kind"] != "call" {
            continue;
        }
        // §4 intercept: answer the verdict call — echoing the id the HOST
        // assigned — and STAY ALIVE (the delivery call below is the one that
        // exits). The verdict is a pure function of the url here; a real
        // extension would consult its own state.
        if v["event"] == "mudra:before_navigate" {
            std::fs::write(&nav_path, line.trim()).ok();
            let url = v["args"]["url"].as_str().unwrap_or_default();
            let verdict = if url.contains("blocked") {
                serde_json::json!({ "verdict": "deny" })
            } else if url.contains("redirect-me") {
                serde_json::json!({ "verdict": "redirect", "url": "https://safe.test/" })
            } else {
                serde_json::json!({ "verdict": "allow" })
            };
            let result = induction_protocol::ResultFrame::new(v["id"].clone(), verdict);
            println!("{}", result.to_line());
            std::io::stdout().flush().ok();
            continue;
        }
        // §4b the ask face: an event whose consumer answers it. The row id IS
        // the call id, so echoing `id` is the whole contract.
        if v["event"] == "mudra:fill_password" {
            let answer = serde_json::json!({
                "ok": true,
                "user": "probe-user",
                "password": "probe-pass",
                "matched": 1,
            });
            let result = induction_protocol::ResultFrame::new(v["id"].clone(), answer);
            println!("{}", result.to_line());
            std::io::stdout().flush().ok();
            continue;
        }
        // what the guest received — the cursor rule's assertion input
        std::fs::write(&seen_path, line.trim()).ok();

        // §5: one effect, through the host's verb surface (the read-only
        // replay window verb, cursor 0 — the guest owns its cursor)
        let host = serde_json::json!({
            "host": { "type": "invoke", "verb": "/events", "args": { "cursor": 0, "limit": 5 } }
        });
        println!("{host}");
        std::io::stdout().flush().ok();

        let mut reply = String::new();
        match input.read_line(&mut reply) {
            Ok(_) if !reply.trim().is_empty() => {
                std::fs::write(&out_path, reply.trim()).ok();
            }
            _ => {}
        }

        // the delivered call's answer: a `result` frame (the host consumes
        // it quietly — no intercept plane in v1)
        let result = induction_protocol::ResultFrame::new(
            serde_json::json!(1),
            serde_json::json!({ "done": true }),
        );
        println!("{}", result.to_line());
        std::io::stdout().flush().ok();
        return; // exit: the supervisor would respawn; the test ends here
    }
}
