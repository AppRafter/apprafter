// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! WI-417 (ATM GOTCHA-57): no request of the operator's client waits behind a
//! watch that is still streaming.
//!
//! On a pooled HTTP/1.1 client (kube's stock builder, hyper 1.11.1 and
//! hyper-util 0.1.20), a watcher's LIST → WATCH on a connection that has just
//! gone idle can put that connection back in the pool while the watch streams.
//! A later request checked out on it is queued and never written until the
//! watch ends. `apprafter_operator::build_client` keeps no idle connection.
//!
//! The shape is the moment after the operator takes the Lease: a fake
//! apiserver answers LISTs at once and holds every WATCH open with no events,
//! twenty watchers LIST then WATCH, and then twenty LISTs must each answer
//! within 2s. An iteration in which one does not has a request queued behind
//! a watch. The race is timing-dependent, so the test runs many iterations
//! and is `#[ignore]`d for its length (about a minute). Run it with:
//!
//! ```text
//! cargo test -p apprafter-operator --test watch_connection_stall_test -- --ignored
//! ```
//!
//! Recorded on 2026-10-04, 200 iterations each: with kube's stock client
//! (`kube::Client::try_from` in place of `build_client`) 5 iterations failed,
//! each with one LIST unanswered; with `build_client`, 0 failed.

use std::net::SocketAddr;
use std::time::Duration;

use apprafter_operator::{build_client, with_operator_client_defaults};
use futures::StreamExt;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::{Api, ListParams, WatchParams};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ITERATIONS: usize = 200;
const WATCHERS: usize = 20;
const LISTS: usize = 20;
const LIST_DEADLINE: Duration = Duration::from_secs(2);

const LIST: &str =
    r#"{"apiVersion":"v1","kind":"ConfigMapList","metadata":{"resourceVersion":"1"},"items":[]}"#;

/// Plain HTTP/1.1: a LIST is answered at once and the connection kept open
/// for the next request; a WATCH gets its response head and then no event,
/// until the client goes away.
async fn fake_apiserver() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.expect("accept");
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    // GETs only: a request ends with its head.
                    let end = loop {
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).to_string();
                    buf.drain(..end);
                    if head.lines().next().unwrap_or("").contains("watch=true") {
                        let _ = socket
                            .write_all(
                                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n",
                            )
                            .await;
                        loop {
                            match socket.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(_) => {}
                            }
                        }
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{LIST}",
                        LIST.len()
                    );
                    if socket.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "stress test, about a minute: run with --ignored"]
async fn no_list_waits_behind_a_streaming_watch() {
    let addr = fake_apiserver().await;
    let mut failed_iterations = 0;
    for iteration in 0..ITERATIONS {
        let config = kube::Config::new(format!("http://{addr}").parse().expect("url"));
        let client = build_client(with_operator_client_defaults(config)).expect("client");
        let configmaps: Api<ConfigMap> = Api::all(client);

        let mut watchers = Vec::new();
        for _ in 0..WATCHERS {
            let configmaps = configmaps.clone();
            watchers.push(tokio::spawn(async move {
                let list = configmaps
                    .list(&ListParams::default())
                    .await
                    .expect("watcher LIST");
                let version = list.metadata.resource_version.expect("resourceVersion");
                let mut events = configmaps
                    .watch(&WatchParams::default().timeout(290), &version)
                    .await
                    .expect("WATCH")
                    .boxed();
                while events.next().await.is_some() {}
            }));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;

        let mut lists = Vec::new();
        for _ in 0..LISTS {
            let configmaps = configmaps.clone();
            lists.push(tokio::spawn(async move {
                match tokio::time::timeout(LIST_DEADLINE, configmaps.list(&ListParams::default()))
                    .await
                {
                    Ok(Ok(_)) => true,
                    Ok(Err(err)) => panic!("a LIST failed instead of answering: {err}"),
                    Err(_) => false,
                }
            }));
        }
        let mut unanswered = 0;
        for list in lists {
            if !list.await.expect("LIST task") {
                unanswered += 1;
            }
        }
        if unanswered > 0 {
            failed_iterations += 1;
            println!(
                "iteration {iteration}: {unanswered} LIST(s) unanswered after {LIST_DEADLINE:?} \
                 (queued behind a watch)"
            );
        }

        for watcher in watchers {
            // A watcher holds its watch open until it is aborted here; one
            // that already ended failed its LIST or WATCH.
            assert!(
                !watcher.is_finished(),
                "a watcher ended before it was aborted: {:?}",
                watcher.await
            );
            watcher.abort();
        }
    }
    println!("iterations={ITERATIONS} failed_iterations={failed_iterations}");
    assert_eq!(
        failed_iterations, 0,
        "{failed_iterations} of {ITERATIONS} iterations had a LIST queued behind a streaming watch"
    );
}
