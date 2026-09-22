// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Fire-and-forget failure webhook for the in-cluster backup runner.
//!
//! A webhook failure must NEVER affect the backup run itself — every error from
//! the HTTP call is swallowed silently, and the call is bounded in time.  The
//! caller does not need to handle the return value.

use std::time::Duration;

/// The most one failure notification may take, end to end: connecting,
/// sending the request, and reading the response.
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
///
/// One gap ureq cannot close: name resolution runs through the system
/// resolver, which the timeout cannot interrupt. The resolver's own timeouts
/// (`resolv.conf`) bound that step.
pub const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(30);

/// POST a small JSON failure notification to `url` and discard all errors.
///
/// # Arguments
/// * `url`        — the destination URL (any scheme ureq supports).
/// * `cluster_id` — the cluster identifier to include in the payload.
/// * `phase`      — the backup phase that failed (e.g. `"backup"`, `"restore"`).
/// * `error`      — a human-readable error string.
///
/// # Guarantee
/// This function **never panics**, **never returns an error**, and returns
/// within [`WEBHOOK_TIMEOUT`] (plus name resolution, see there).  Network
/// failures, invalid URLs, non-2xx responses, serialisation errors and an
/// endpoint that never answers are all silently swallowed so a webhook outage
/// cannot impact the backup run.
pub fn post_failure(url: &str, cluster_id: &str, phase: &str, error: &str) {
    post_failure_within(url, cluster_id, phase, error, WEBHOOK_TIMEOUT);
}

/// [`post_failure`] with the time bound as a parameter, so the tests can
/// exercise it without waiting out the real one.
fn post_failure_within(url: &str, cluster_id: &str, phase: &str, error: &str, timeout: Duration) {
    let payload = serde_json::json!({
        "cluster":    cluster_id,
        "when_phase": phase,
        "error":      error,
    });
    // `ureq` returns `Err` on network errors, non-2xx responses and an
    // exceeded timeout; we discard all of them via `let _ =` so the caller is
    // unaffected. `timeout` is ureq's OVERALL bound: it overrides the
    // unbounded read and write defaults and caps the connect as well.
    let _ = ureq::post(url).timeout(timeout).send_json(payload);
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
}
