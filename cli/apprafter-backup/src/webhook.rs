// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Fire-and-forget failure webhook for the in-cluster backup runner.
//!
//! A webhook failure must NEVER affect the backup run itself — every error from
//! the HTTP call is swallowed silently, and the call is bounded in time.  The
//! caller does not need to handle the return value.

use std::time::Duration;

/// The most one failure notification may take once its host name has
/// resolved: connecting, sending the request, and reading the response.
///
/// ureq 2's defaults bound only the connect (30 s); reads and writes may block
/// for ever. The runner posts AFTER it has recorded the failure and just
/// before it exits, so an endpoint that accepts the connection and never
/// answers kept the Job running with the outcome already written — and, the
/// CronJob being `concurrencyPolicy: Forbid`, kept every later scheduled
/// backup from starting.
///
/// Thirty seconds matches ureq's own connect default. A receiver that takes
/// longer to acknowledge a JSON POST is not going to, and the notification is
/// best-effort either way.
pub const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(30);

/// The most the TCP connect may take, inside [`WEBHOOK_TIMEOUT`].
///
/// ureq 2 times the connect against this setting ALONE, from the moment the
/// connect starts — not against the request's overall timeout — so it has to
/// be set below that timeout for the overall one to hold (ureq 2.12.1
/// `stream::connect_host`). Ten seconds is ample for a TCP handshake.
pub const WEBHOOK_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// POST a small JSON failure notification to `url` and discard all errors.
///
/// # Arguments
/// * `url`        — the destination URL (any scheme ureq supports).
/// * `cluster_id` — the cluster identifier to include in the payload.
/// * `phase`      — the backup phase that failed (e.g. `"backup"`, `"restore"`).
/// * `error`      — a human-readable error string.
///
/// # Guarantee
/// This function **never panics** and **never returns an error**. Network
/// failures, invalid URLs, non-2xx responses, redirects, serialisation errors
/// and an endpoint that never answers are all silently swallowed so a webhook
/// outage cannot impact the backup run.
///
/// Its time bound: [`WEBHOOK_TIMEOUT`] from the start of the request, or
/// [`WEBHOOK_CONNECT_TIMEOUT`] after the host name has resolved if that ends
/// later. Name resolution itself runs through the system resolver, which ureq
/// cannot interrupt; the resolver's own timeouts (`resolv.conf`) bound it.
///
/// Redirects are NOT followed. ureq starts a fresh connect timer for every hop
/// it follows, so one `302` to an address that never answers stretched a
/// "30 s" call to 58 s (measured on ureq 2.12.1), and a failure notification
/// has no business being forwarded somewhere else anyway. A `3xx` is the
/// endpoint's answer, and it is discarded like any other.
pub fn post_failure(url: &str, cluster_id: &str, phase: &str, error: &str) {
    post_failure_within(url, cluster_id, phase, error, WEBHOOK_TIMEOUT);
}

/// The agent every notification is sent through: `timeout` overall, a connect
/// capped at [`WEBHOOK_CONNECT_TIMEOUT`] (and never above `timeout`), and no
/// redirects. See [`post_failure`] for why each is set.
fn webhook_agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(timeout)
        .timeout_connect(WEBHOOK_CONNECT_TIMEOUT.min(timeout))
        .redirects(0)
        .build()
}

/// [`post_failure`] with the time bound as a parameter, so the tests can
/// exercise it without waiting out the real one.
fn post_failure_within(url: &str, cluster_id: &str, phase: &str, error: &str, timeout: Duration) {
    let payload = serde_json::json!({
        "cluster":    cluster_id,
        "when_phase": phase,
        "error":      error,
    });
    // `ureq` returns `Err` on network errors, 4xx/5xx responses and an
    // exceeded timeout, and `Ok` for a 2xx or an unfollowed 3xx; we discard
    // all of them via `let _ =` so the caller is unaffected.
    let _ = webhook_agent(timeout).post(url).send_json(payload);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    /// Posting to an unreachable host must never panic (fire-and-forget
    /// guarantee).
    #[test]
    fn post_failure_swallows_errors_on_unreachable_host() {
        post_failure("http://127.0.0.1:1/nope", "c", "backup", "boom");
        // reaching here means no panic — the test passes.
    }

    #[test]
    fn the_webhook_bound_is_thirty_seconds() {
        assert_eq!(WEBHOOK_TIMEOUT, Duration::from_secs(30));
    }

    /// An endpoint that accepts the connection, reads the whole request and
    /// never answers — a hung receiver, or a proxy holding the connection —
    /// must cost the runner the timeout and no more.
    #[test]
    fn an_endpoint_that_never_answers_costs_the_timeout_and_no_more() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/hook", listener.local_addr().expect("addr"));

        let (got_request, request_seen) = mpsc::channel::<String>();
        let (release, released) = mpsc::channel::<()>();
        let server = thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            // Read until the JSON body has arrived, then go silent: no status
            // line, no close, the connection held open.
            while !String::from_utf8_lossy(&request).contains("\"when_phase\"") {
                let n = conn.read(&mut buf).expect("read the request");
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            got_request
                .send(String::from_utf8_lossy(&request).into_owned())
                .expect("report the request");
            // Hold the socket until the test is over, however it ends.
            let _ = released.recv();
            drop(conn);
        });

        let bound = Duration::from_secs(2);
        let (done, returned) = mpsc::channel::<Duration>();
        let caller = thread::spawn(move || {
            let started = Instant::now();
            post_failure_within(&url, "c1", "backup", "boom", bound);
            let _ = done.send(started.elapsed());
        });

        let request = request_seen
            .recv_timeout(Duration::from_secs(10))
            .expect("the POST never reached the listener");
        // A watchdog far past the bound, so a missing timeout FAILS the test
        // instead of hanging it.
        let outcome = returned.recv_timeout(Duration::from_secs(20));
        let _ = release.send(());
        server.join().expect("server thread");
        let elapsed =
            outcome.expect("post_failure is still blocked 20 s into a 2 s bound: no timeout");
        caller.join().expect("post_failure panicked");

        assert!(request.starts_with("POST /hook "), "{request}");
        assert!(request.contains("\"when_phase\":\"backup\""), "{request}");
        assert!(
            elapsed >= bound - Duration::from_millis(100),
            "returned in {elapsed:?}, before the bound: it never waited on the answer"
        );
        assert!(
            elapsed < bound + Duration::from_secs(3),
            "returned in {elapsed:?}, well past the {bound:?} bound"
        );
    }

    /// Run `post_failure_within` on a thread; `None` when it is still running
    /// after `watchdog`, so a missing bound FAILS the test instead of hanging.
    fn post_with_watchdog(url: String, bound: Duration, watchdog: Duration) -> Option<Duration> {
        let (done, returned) = mpsc::channel::<Duration>();
        thread::spawn(move || {
            let started = Instant::now();
            post_failure_within(&url, "c1", "backup", "boom", bound);
            let _ = done.send(started.elapsed());
        });
        returned.recv_timeout(watchdog).ok()
    }

    /// A hook that reads one request and answers `302 Found` to `location`.
    fn redirecting_hook(location: String) -> (String, thread::JoinHandle<()>) {
        use std::io::Write;

        let hook = TcpListener::bind("127.0.0.1:0").expect("bind the hook");
        let url = format!("http://{}/hook", hook.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut conn, _) = hook.accept().expect("accept");
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            while !String::from_utf8_lossy(&request).contains("\"when_phase\"") {
                let n = conn.read(&mut buf).expect("read the request");
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            let _ = conn.write_all(
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\
                     Connection: close\r\n\r\n"
                )
                .as_bytes(),
            );
        });
        (url, server)
    }

    /// A loopback address whose connect neither succeeds nor fails: a
    /// listener whose accept queue is full, so the kernel drops further SYNs.
    /// Nothing ever accepts; keep the returned streams alive while it is used.
    fn loopback_blackhole() -> (std::net::SocketAddr, TcpListener, Vec<std::net::TcpStream>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let mut held = Vec::new();
        while std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300))
            .map(|s| held.push(s))
            .is_ok()
        {
            assert!(
                held.len() < 20_000,
                "the accept queue never filled; no blackhole to test against"
            );
        }
        (addr, listener, held)
    }

    /// A `302` is the endpoint's answer, not an instruction. Following it is
    /// how one notification connected twice, each connect on a fresh timer:
    /// 58 s against a 30 s bound on ureq 2.12.1, redirected to a blackhole.
    #[test]
    fn a_redirect_to_a_blackhole_costs_nothing() {
        let (blackhole, _listener, _held) = loopback_blackhole();
        let (url, server) = redirecting_hook(format!("http://{blackhole}/x"));
        let bound = Duration::from_secs(5);
        let elapsed = post_with_watchdog(url, bound, Duration::from_secs(20))
            .expect("post_failure is still blocked 20 s into a 5 s bound");
        server.join().expect("hook thread");
        assert!(
            elapsed < Duration::from_secs(2),
            "took {elapsed:?} for a hook that answered at once: the redirect was followed"
        );
    }

    #[test]
    fn a_redirect_is_never_followed() {
        // Where the redirect points. Anything arriving here is a follow.
        let target = TcpListener::bind("127.0.0.1:0").expect("bind the target");
        target.set_nonblocking(true).expect("non-blocking target");
        let (url, server) =
            redirecting_hook(format!("http://{}/elsewhere", target.local_addr().unwrap()));
        post_with_watchdog(url, Duration::from_secs(5), Duration::from_secs(20))
            .expect("post_failure is still blocked 20 s into a 5 s bound");
        server.join().expect("hook thread");
        // Give a follow that is already on its way the time to land.
        thread::sleep(Duration::from_millis(500));
        match target.accept() {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok((_, from)) => panic!("the redirect was followed: {from} connected to its target"),
            Err(e) => panic!("accept on the target: {e}"),
        }
    }

    /// The connect is timed on its own clock, not the request's: it must be
    /// capped at the overall bound, or a host that never completes the
    /// handshake costs ureq's 30 s connect default whatever the bound says.
    #[test]
    fn a_connect_that_never_completes_costs_the_bound_and_no_more() {
        let (addr, _listener, held) = loopback_blackhole();

        let bound = Duration::from_secs(3);
        let url = format!("http://{addr}/hook");
        let elapsed = post_with_watchdog(url, bound, Duration::from_secs(20)).expect(
            "post_failure is still blocked 20 s into a 3 s bound: the connect is unbounded",
        );
        assert!(
            elapsed >= bound - Duration::from_millis(100),
            "returned in {elapsed:?}: the connect failed instead of hanging, so this proves nothing"
        );
        assert!(
            elapsed < bound + Duration::from_secs(2),
            "returned in {elapsed:?}, past the {bound:?} bound"
        );
        drop(held);
    }

    #[test]
    fn the_connect_is_capped_at_ten_seconds_inside_the_thirty() {
        assert_eq!(WEBHOOK_CONNECT_TIMEOUT, Duration::from_secs(10));
        assert!(WEBHOOK_CONNECT_TIMEOUT < WEBHOOK_TIMEOUT);
    }
}
