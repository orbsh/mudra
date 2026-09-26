//! Watcher tests — the port of `mudrad.py::_watch`'s contracts against a
//! scripted mock CDP endpoint (real sockets; the mock answers getTargets/
//! setDiscoverTargets, pushes lifecycle events, and optionally hangs up).
//! Run: `cargo test -p mudrad --test watcher_test`.

use futures_util::{SinkExt, StreamExt};
use mudrad::watch::{connect_ready, page_infos, run_watcher};
use mudra_store::MudraStore;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Scripted mock. One accepted ws: answers `Target.getTargets` with
/// `target_infos`; on `Target.setDiscoverTargets` answers, pushes the
/// scripted `events`, then stays open forever (the caller parks the
/// watcher under a timeout) unless `hangup_after` elapses and closes it.
async fn start_scripted_cdp(
    target_infos: Vec<Value>,
    events: Vec<Value>,
    hangup_after: Option<Duration>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let target_infos = target_infos.clone();
            let events = events.clone();
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else { return };
                while let Some(Ok(Message::Text(t))) = ws.next().await {
                    let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                    let Some(id) = v.get("id").and_then(Value::as_u64) else { continue };
                    match v.get("method").and_then(Value::as_str) {
                        Some("Target.getTargets") => {
                            let r = json!({"id": id, "result": {"targetInfos": target_infos}});
                            let _ = ws.send(Message::Text(r.to_string().into())).await;
                        }
                        Some("Target.setDiscoverTargets") => {
                            let r = json!({"id": id, "result": {}});
                            let _ = ws.send(Message::Text(r.to_string().into())).await;
                            for e in events.clone() {
                                let _ = ws.send(Message::Text(e.to_string().into())).await;
                            }
                            if let Some(d) = hangup_after {
                                tokio::time::sleep(d).await;
                                let _ = ws.close(None).await;
                                return;
                            }
                            // stay open: the caller's timeout parks the watcher
                            std::future::pending::<()>().await;
                        }
                        _ => {}
                    }
                }
            });
        }
    });
    addr
}

fn page_target(id: &str, url: &str, title: &str, opener: Option<&str>) -> Value {
    json!({ "targetId": id, "type": "page", "url": url, "title": title, "openerId": opener.unwrap_or("") })
}

fn mk_store() -> (tempfile::TempDir, MudraStore) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = MudraStore::open(dir.path()).unwrap(); // borrow before the move
    (dir, store)
}

// ================= never-ready retry ring =================

#[tokio::test]
async fn connect_ready_gives_up_after_the_retry_budget() {
    // Contract (the never-ready lesson): a port that never binds within
    // attempts*delay -> None, so the caller marks down and the main loop
    // stays free to retry later. Port 1: nothing listens.
    let got = connect_ready(1, 2, Duration::from_millis(30)).await;
    assert!(got.is_none(), "no endpoint must yield no connection");
}

#[tokio::test]
async fn connect_ready_succeeds_once_the_endpoint_appears() {
    // The retry ring exists for the spawn race: the debug port shows up
    // between attempts. Two listeners: the ws side is always alive (like
    // chromium, where /json/version and the ws share one port, but the
    // test splits them); the HTTP side binds late.
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = probe.local_addr().unwrap();
    drop(probe); // freed until the late re-bind: first attempts get refused

    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr = ws_listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((sock, _)) = ws_listener.accept().await {
            tokio::spawn(async move {
                // complete the handshake, then idle (never answer commands —
                // this test only exercises connection establishment)
                if let Ok(ws) = tokio_tungstenite::accept_async(sock).await {
                    let _ = ws;
                    std::future::pending::<()>().await;
                }
            });
        }
    });

    let late = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(120)).await;
        let l = TcpListener::bind(http_addr).await.expect("late rebind");
        while let Ok((mut sock, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let body = json!({"webSocketDebuggerUrl": format!("ws://{ws_addr}/")}).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    let conn = connect_ready(http_addr.port(), 8, Duration::from_millis(50))
        .await
        .expect("a later attempt finds the late endpoint");
    drop(conn);
    late.abort();
}

// ================= filtering =================

#[test]
fn page_infos_drops_non_page_targets() {
    // Contract: workers/iframes never enter the pages table.
    let arr = json!([
        page_target("T1", "https://a.test", "A", None),
        {"targetId": "W1", "type": "service_worker", "url": "chrome-extension://x"},
        {"targetId": "F1", "type": "iframe", "url": "about:blank"}
    ]);
    let pages = page_infos(&arr);
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].target_id, "T1");
}

// ================= baseline + events (mock stays open; timeout parks) =================

#[tokio::test]
async fn baseline_sync_injects_pages_and_notifies_epoch_once() {
    // Contract: getTargets first (baseline), inject per page, one epoch
    // notify for the whole batch; openerId backfills through the batch map.
    let (_d, mut store) = mk_store();
    let inst = store.launch_started(None, "work", 9210, 4242, None, None);
    let addr = start_scripted_cdp(
        vec![
            page_target("T1", "https://a.test", "A", None),
            page_target("T2", "https://b.test", "B", Some("T1")),
        ],
        vec![],
        None,
    )
    .await;
    let conn = mudrad::cdp::CdpConn::connect(&format!("ws://{addr}/")).await.unwrap();

    let mut injected: Vec<String> = Vec::new();
    let mut epochs: Vec<u64> = Vec::new();
    let _ = tokio::time::timeout(
        Duration::from_millis(600),
        run_watcher(
            &mut store,
            inst.id,
            addr.port(),
            "work",
            &conn,
            &mut |_p, tid, _ctx| injected.push(tid.to_string()),
            &mut |e| epochs.push(e),
        ),
    )
    .await; // timeout = still parked on the open stream: expected

    assert_eq!(injected, vec!["T1", "T2"]); // baseline injects every page
    assert_eq!(epochs.len(), 1, "batch = one epoch bump");
    assert_eq!(store.pages_of_instance(inst.id, false).len(), 2);
    let (_, t2) = store.page_by_target("T2").unwrap();
    let (t1k, _) = store.page_by_target("T1").unwrap();
    assert_eq!(t2.parent_id, t1k.id); // openerId resolved batch-locally
}

#[tokio::test]
async fn created_changed_destroyed_events_drive_the_store() {
    // Contract: targetCreated (page) -> inject + sync + notify;
    // infoChanged -> sync only (no re-inject); non-page created ->
    // ignored; targetDestroyed -> close_target + notify.
    let (_d, mut store) = mk_store();
    let inst = store.launch_started(None, "work", 9211, 4242, None, None);
    let addr = start_scripted_cdp(
        vec![page_target("T1", "https://a.test", "A", None)],
        vec![
            json!({"method": "Target.targetCreated", "params": {"targetInfo": page_target("T2", "https://c.test", "C", None)}}),
            json!({"method": "Target.targetCreated", "params": {"targetInfo": {"targetId": "W9", "type": "service_worker", "url": "chrome-extension://x"}}}),
            json!({"method": "Target.targetInfoChanged", "params": {"targetInfo": page_target("T1", "https://a.test/next", "A new", None)}}),
            json!({"method": "Target.targetDestroyed", "params": {"targetId": "T2"}}),
        ],
        None,
    )
    .await;
    let conn = mudrad::cdp::CdpConn::connect(&format!("ws://{addr}/")).await.unwrap();

    let mut injected: Vec<String> = Vec::new();
    let mut epochs: Vec<u64> = Vec::new();
    let _ = tokio::time::timeout(
        Duration::from_millis(800),
        run_watcher(
            &mut store,
            inst.id,
            addr.port(),
            "work",
            &conn,
            &mut |_p, tid, _ctx| injected.push(tid.to_string()),
            &mut |e| epochs.push(e),
        ),
    )
    .await;

    assert_eq!(injected, vec!["T1", "T2"]); // worker skipped, infoChanged not re-injected
    let (_, t1) = store.page_by_target("T1").expect("changed row");
    assert_eq!(t1.url, "https://a.test/next"); // infoChanged refreshed
    assert!(store.page_by_target("T2").is_none()); // destroyed -> out of live view
    assert!(store
        .pages_of_instance(inst.id, true)
        .iter()
        .any(|(_, p)| p.target_id == "T2" && p.closed_at != 0)); // still in history
    // epochs: baseline, T2 create, T1 change, T2 destroy — consecutive
    assert_eq!(epochs.len(), 4, "{epochs:?}");
    assert!(epochs.windows(2).all(|w| w[1] == w[0] + 1));
}

// ================= teardown (mock hangs up; watcher returns) =================

#[tokio::test]
async fn disconnect_marks_down_and_notifies_teardown() {
    // Contract (the recv-exception -> finally teardown path): when the
    // socket dies, the watcher marks the instance down, closes every open
    // page, notifies that final epoch, and RETURNS.
    let (_d, mut store) = mk_store();
    let inst = store.launch_started(None, "work", 9212, 4242, None, None);
    let addr = start_scripted_cdp(
        vec![page_target("T1", "https://a.test", "A", None)],
        vec![],
        Some(Duration::from_millis(120)), // mock hangs up after discovery
    )
    .await;
    let conn = mudrad::cdp::CdpConn::connect(&format!("ws://{addr}/")).await.unwrap();

    let mut injected = Vec::new();
    let mut epochs: Vec<u64> = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(6),
        run_watcher(
            &mut store,
            inst.id,
            addr.port(),
            "work",
            &conn,
            &mut |_p, tid, _ctx| injected.push(tid.to_string()),
            &mut |e| epochs.push(e),
        ),
    )
    .await
    .expect("watcher returns on hangup");

    let (_, inst_row) = store.instance_for_context("work").unwrap();
    assert_eq!(inst_row.running, 0, "disconnect marks the instance down");
    assert!(store.pages_of_instance(inst.id, false).iter().all(|(_, p)| p.closed_at != 0));
    assert_eq!(injected, vec!["T1"]);
    assert_eq!(epochs.len(), 2, "{epochs:?}"); // baseline + teardown
    assert_eq!(store.epoch(), *epochs.last().unwrap());
}

#[tokio::test]
async fn command_failure_before_baseline_marks_down_immediately() {
    // Contract (the never-ready variant after connect): the endpoint
    // dies between handshake and baseline command. A ws that completes
    // the handshake then drops: getTargets fails -> the watcher tears down
    // and returns without ever touching the pages table.
    let (_d, mut store) = mk_store();
    let inst = store.launch_started(None, "work", 9213, 4242, None, None);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                if let Ok(ws) = tokio_tungstenite::accept_async(sock).await {
                    drop(ws); // handshake, then hang up: every command fails
                }
            });
        }
    });
    let conn = mudrad::cdp::CdpConn::connect(&format!("ws://{addr}/")).await.unwrap();

    let mut injected = Vec::new();
    let mut epochs = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(6),
        run_watcher(
            &mut store,
            inst.id,
            addr.port(),
            "work",
            &conn,
            &mut |_p, tid, _ctx| injected.push(tid.to_string()),
            &mut |e| epochs.push(e),
        ),
    )
    .await
    .expect("watcher returns fast on a dead baseline");

    let (_, inst_row) = store.instance_for_context("work").unwrap();
    assert_eq!(inst_row.running, 0);
    assert!(injected.is_empty());
    assert_eq!(epochs.len(), 1); // teardown epoch only
    assert!(store.pages_of_instance(inst.id, true).is_empty());
}
