//! CDP client tests against mock devtools endpoints on real sockets —
//! a raw HTTP mock for `/json*` helpers and a tungstenite mock for the
//! browser-level ws, exercising the single-reader routing contract.
//! Run: `cargo test -p mudrad --test cdp_test`.

use mudrad::cdp::*;
use mudrad::watch::connect_ready;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

// ================= raw HTTP mock (the debug-port endpoints) =================

/// Serve ONE request: any GET -> `http_body` as the whole response.
async fn start_http_mock(http_body: Value) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await; // consume the request (framing irrelevant)
            let body = http_body.to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (addr, task)
}

// ================= ws mock (the browser-level endpoint) =================

/// Accept ws connections; push `prelude` events immediately (before any
/// command — the interleaving that broke the naive Python pumps), then
/// answer every framed command with `{"id", "result": {"echo": params}}`
/// except method `Never.answer` (no reply -> the client's timeout path).
async fn start_ws_mock(prelude: Vec<Value>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let prelude = prelude.clone();
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else { return };
                for p in prelude {
                    let _ = ws.send(Message::Text(p.to_string().into())).await;
                }
                while let Some(Ok(Message::Text(t))) = ws.next().await {
                    let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                    let Some(id) = v.get("id").and_then(Value::as_u64) else { continue };
                    if v.get("method").and_then(Value::as_str) == Some("Never.answer") {
                        continue; // silence on purpose: the timeout contract
                    }
                    let reply = json!({
                        "id": id,
                        "result": {"echo": v.get("params").cloned().unwrap_or(json!({}))}
                    });
                    let _ = ws.send(Message::Text(reply.to_string().into())).await;
                }
            });
        }
    });
    (addr, task)
}

// ================= routing contracts =================

#[tokio::test]
async fn prelude_events_never_satisfy_a_command_waiter() {
    // Contract (the probe lesson): events sharing the socket with
    // responses must land on the event stream, not in a call's waiter —
    // even when the event arrives BEFORE the response is awaited.
    let prelude = vec![json!({"method": "Target.targetCreated", "params": {"targetInfo": {"targetId": "T1"}}})];
    let (addr, _task) = start_ws_mock(prelude).await;
    let conn = CdpConn::connect(&format!("ws://{addr}/devtools/browser/mock"))
        .await
        .expect("connect");

    let result = conn
        .call_with_timeout(
            "Target.getTargets",
            json!({}),
            std::time::Duration::from_secs(5),
        )
        .await
        .expect("response routed to the waiter");
    assert_eq!(result["echo"], json!({})); // the mock echoes params

    // the prelude event is still on the stream, unread and unconsumed
    let ev = tokio::time::timeout(std::time::Duration::from_secs(5), conn.next_event())
        .await
        .expect("event arrives")
        .expect("stream open");
    assert_eq!(ev["method"], "Target.targetCreated");
}

#[tokio::test]
async fn concurrent_calls_match_their_own_responses_by_id() {
    // Contract: two commands in flight on one socket — each waiter gets
    // exactly its own id (the second pump stealing a response was the
    // production symptom; the router makes it structurally impossible).
    let (addr, _task) = start_ws_mock(vec![]).await;
    let conn = std::sync::Arc::new(
        CdpConn::connect(&format!("ws://{addr}/devtools/browser/mock"))
            .await
            .expect("connect"),
    );

    let a = {
        let c = std::sync::Arc::clone(&conn);
        tokio::spawn(async move { c.call("A.method", json!({"which": "a"})).await })
    };
    let b = {
        let c = std::sync::Arc::clone(&conn);
        tokio::spawn(async move { c.call("B.method", json!({"which": "b"})).await })
    };
    let ra = a.await.unwrap().expect("a answered");
    let rb = b.await.unwrap().expect("b answered");
    assert_eq!(ra["echo"]["which"], "a");
    assert_eq!(rb["echo"]["which"], "b");
}

#[tokio::test]
async fn unanswered_command_times_out_without_poisoning_the_connection() {
    // Contract: a timed-out waiter is unregistered; a LATE response with
    // the same id must be dropped by the router, never mistaken for an
    // event, and later calls still work.
    let (addr, _task) =
        start_ws_mock_with_late_answer().await;
    let conn = CdpConn::connect(&format!("ws://{addr}/devtools/browser/mock"))
        .await
        .expect("connect");

    let err = conn
        .call_with_timeout("Never.answer", json!({}), std::time::Duration::from_millis(100))
        .await
        .expect_err("no reply = timeout");
    assert!(err.to_string().contains("timed out"), "{err}");

    // the mock answers the next command normally
    let ok = conn.call("After.timeout", json!({"x": 1})).await.expect("alive after timeout");
    assert_eq!(ok["echo"]["x"], 1);
}

/// ws mock that ignores `Never.answer` but pushes a late response for the
/// ignored id 0.3s later (post-timeout), then resumes normal service.
async fn start_ws_mock_with_late_answer() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else { return };
                while let Some(Ok(Message::Text(t))) = ws.next().await {
                    let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                    let Some(id) = v.get("id").and_then(Value::as_u64) else { continue };
                    if v.get("method").and_then(Value::as_str) == Some("Never.answer") {
                        // answer late — after the client's waiter expired
                        let late = json!({"id": id, "result": {"late": true}});
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        let _ = ws.send(Message::Text(late.to_string().into())).await;
                        continue;
                    }
                    let reply = json!({
                        "id": id,
                        "result": {"echo": v.get("params").cloned().unwrap_or(json!({}))}
                    });
                    let _ = ws.send(Message::Text(reply.to_string().into())).await;
                }
            });
        }
    });
    (addr, task)
}

// ================= HTTP helper contracts =================

/// Serve any GET -> the JSON body, framed by Content-Length, then KEEP
/// THE CONNECTION OPEN forever (never shutdown, never close). This is the
/// real chromium DevTools behavior the EOF-read port hung on: the answer
/// says `Connection: close` but the socket stays alive — a reader waiting
/// for EOF waits forever.
async fn start_keepalive_http_mock(http_body: Value) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body = http_body.to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=UTF-8\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            // and hold the socket open — the bug this mock exists to catch
            std::mem::forget(sock);
        }
    });
    (addr, task)
}

#[tokio::test]
async fn http_helpers_finish_on_content_length_without_eof() {
    // THE chromium blood lesson: /json/version and /json must complete
    // even though the server never closes the connection. The EOF-read
    // version hung the first connect_ready attempt forever — silently
    // defeating the whole never-ready retry budget (no logs, no pages,
    // no mark-down; the symptom was a watcher that simply never acted).
    let version = json!({"webSocketDebuggerUrl": "ws://127.0.0.1:9201/devtools/browser/abc"});
    let (addr, _t) = start_keepalive_http_mock(version).await;
    let url = tokio::time::timeout(std::time::Duration::from_secs(3), browser_ws(addr.port()))
        .await
        .expect("must not hang: framing ends at Content-Length")
        .expect("version parses");
    assert_eq!(url, "ws://127.0.0.1:9201/devtools/browser/abc");

    let list = json!([{"id": "T1", "type": "page", "url": "u", "title": "t"}]);
    let (addr2, _t2) = start_keepalive_http_mock(list).await;
    let rows = tokio::time::timeout(std::time::Duration::from_secs(3), list_targets(addr2.port()))
        .await
        .expect("must not hang")
        .expect("list parses");
    assert_eq!(rows[0]["id"], "T1");
}

#[test]
fn blocking_devtools_json_survives_a_keepalive_server() {
    // runtime.rs's synchronous GET carries the same lesson; the Fake
    // Runtime never exercised it against a real socket — bind one here.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let body = json!([{"id": "T9", "type": "page"}]).to_string();
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        // accept forever; after answering, consume-and-never-drop keeps
        // the connection open, like chromium's DevTools server does
        for sock in listener.incoming().flatten() {
            let mut sock = sock;
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf);
            let _ = sock.write_all(resp.as_bytes());
            std::mem::forget(sock);
        }
    });
    let v = mudrad::runtime::devtools_json(addr.port(), "/json").expect("framed read completes");
    assert_eq!(v[0]["id"], "T9");
}

#[tokio::test]
async fn connect_ready_gives_up_when_an_attempt_hangs() {
    // The budget must be enforceable even against a server whose first
    // byte never arrives in the shape the client waits for: here the
    // endpoint accepts TCP and answers nothing at all. The per-attempt
    // timeout turns the hang into a counted failure, so give-up still
    // happens (the EOF-hang bug made `attempts` unreachable).
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _h = tokio::spawn(async move {
        while let Ok((_sock, _)) = listener.accept().await {
            // hold every connection open, say nothing
        }
    });
    let start = std::time::Instant::now();
    let got = connect_ready(addr.port(), 2, std::time::Duration::from_millis(50)).await;
    assert!(got.is_none(), "hung attempts must exhaust the budget, not park");
    assert!(start.elapsed() < std::time::Duration::from_secs(5), "bounded by the retry budget");
}

#[tokio::test]
async fn browser_ws_reads_the_debugger_url_from_json_version() {
    let body = json!({"webSocketDebuggerUrl": "ws://127.0.0.1:9201/devtools/browser/abc"});
    let (addr, _t) = start_http_mock(body).await;
    assert_eq!(
        browser_ws(addr.port()).await.expect("url"),
        "ws://127.0.0.1:9201/devtools/browser/abc"
    );

    // a list-shaped answer to /json/version is a clean error, not a panic
    let (addr2, _t2) = start_http_mock(json!([])).await;
    let err = browser_ws(addr2.port()).await.unwrap_err();
    assert!(err.to_string().contains("lacks webSocketDebuggerUrl"), "{err}");
}

#[tokio::test]
async fn list_targets_returns_the_page_rows() {
    let body = json!([
        {"id": "T1", "type": "page", "url": "https://a.test", "title": "A"},
        {"id": "T2", "type": "service_worker", "url": "chrome-extension://x"}
    ]);
    let (addr, _t) = start_http_mock(body).await;
    let rows = list_targets(addr.port()).await.expect("targets");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["url"], "https://a.test");
}

#[tokio::test]
async fn helpers_fail_cleanly_when_no_daemon_listens() {
    // A dead instance: connect refused -> Err (the watcher's never-ready
    // retry loop keys off this, no panic path).
    let dead = 1u16; // port 1 is not bound on this box
    assert!(browser_ws(dead).await.is_err());
    assert!(list_targets(dead).await.is_err());
}

// ================= snapshot mapping =================

#[tokio::test]
async fn wait_closed_fires_when_the_peer_drops_the_socket() {
    // Contract: connection lost is the watcher's teardown trigger,
    // observable WITHOUT consuming the conn. The ws mock closes the
    // stream when its accepted-task loop ends; here a throwaway server
    // that disconnects immediately after the handshake.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let Ok(ws) = tokio_tungstenite::accept_async(sock).await else { return };
                // drop the stream right away: the peer hangs up
                drop(ws);
            });
        }
    });
    let conn = CdpConn::connect(&format!("ws://{addr}/")).await.expect("connect");
    // wait_closed must resolve (permit stored by the exiting reader)
    tokio::time::timeout(std::time::Duration::from_secs(5), conn.wait_closed())
        .await
        .expect("teardown signal arrives after disconnect");
}

#[test]
fn target_info_maps_both_id_spellings() {
    // Contract: events carry `targetId`, /json rows carry `id`; the
    // snapshot helper normalizes both into store TargetInfo.
    let from_event = json!({
        "targetId": "T1", "type": "page", "url": "u", "title": "t", "openerId": "T0"
    });
    let ti = target_info_of(&from_event);
    assert_eq!(ti.target_id, "T1");
    assert_eq!(ti.url, "u");
    assert_eq!(ti.title, "t");
    assert_eq!(ti.opener_id, "T0");

    let from_list = json!({"id": "T2", "url": "", "title": ""});
    let ti = target_info_of(&from_list);
    assert_eq!(ti.target_id, "T2");
    assert!(ti.opener_id.is_empty());
}
