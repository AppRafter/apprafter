// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The runner's kube-rs client, built with its rustls crypto provider in place.
//!
//! kube-rs builds its TLS config with `rustls::ClientConfig::builder()`, which
//! needs a PROCESS-LEVEL `CryptoProvider`. When none has been installed, rustls
//! picks one from its own crate features — and that works only while exactly
//! ONE provider (`ring` or `aws-lc-rs`) is compiled in. With none, or with both,
//! the first client construction panics:
//!
//! ```text
//! Could not automatically determine the process-level CryptoProvider from
//! Rustls crate features.
//! ```
//!
//! The operator hit exactly this in v0.1.61 (CrashLoopBackOff on its first real
//! cluster, because no test ever built a TLS client). Here the workspace
//! compiles ring alone today, so crate-feature selection would work — but that
//! is a property of the whole dependency graph, and one new dependency that
//! enables rustls' default features would bring aws-lc-rs in beside it and turn
//! the scheduled backup into a panic on start. Installing ring explicitly makes
//! the runner independent of that: an explicitly installed provider wins over
//! crate-feature selection, however many providers are compiled in.
//!
//! The same constructor also sets the two client defaults kube 4.0 moved —
//! the 295 s read timeout goes back, kube 4's in-call retries stay — see
//! [`runner_client_config`] for why each way.
//!
//! [`kube_client`] is the ONLY way `main` builds its client, so neither the
//! install nor those settings can be forgotten on the path that ships.

/// Install `ring` as the process-level rustls `CryptoProvider`. Idempotent.
///
/// `install_default` errs only when a provider is already installed — which is
/// the state this function exists to reach — so that error is not a failure.
pub fn install_rustls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// The client read timeout kube-client applied by default up to 3.x.
pub const CLIENT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(295);

/// The runner's settings for the two kube-client defaults kube 4.0 moved.
///
/// * `read_timeout`: `Some(295s)` on kube 0.95, `None` on kube 4. **Put
///   back.** It is an idle timeout on the socket (kube wraps the connector, so
///   it counts how long a pending read has gone without a byte arriving).
///   Without it a GET, LIST or PATCH that the apiserver accepts and never
///   answers — a half-open connection after a control-plane network blip —
///   blocks the run for ever: no exit, no `lastFailure`, no failure webhook,
///   and because the CronJob is `concurrencyPolicy: Forbid`, no later
///   scheduled backup either. The `pods/exec` WebSocket is the same socket
///   after the upgrade, so the timeout covers the `pg_dump` / `tar` streams
///   too, as it did on 0.95 — but kube 4 pings an exec stream every 60 s and
///   the apiserver's pongs count as reads. Observed on kind: under a 30 s
///   timeout, shorter than the first ping, a command silent for 50 s was cut
///   at 30 s; under a 90 s timeout, a command silent for 150 s finished. So a
///   quiet command, a `pg_dump` waiting on a lock say, outlives the timeout,
///   and only a connection that stops answering is cut. 0.95 sent no pings,
///   so it cut any exec that stayed silent for 295 s.
/// * `default_retry`: absent on kube 0.95, on by default in kube 4. **Kept
///   on, on purpose.** kube 4 retries a 429, 503 or 504 inside the call — up
///   to 15 times, exponential backoff from 5 ms (each delay in `[b, 3b)`,
///   about 3 to 8 minutes in all), or the server's `Retry-After` when that is
///   longer. The operator turns this off because its Lease must see every
///   failure; the runner has no Lease and counts no failures against a clock.
///   Every request it sends is safe to repeat: server-side-apply PATCHes,
///   GETs, LISTs, a best-effort DELETE, and the exec upgrade (a refused
///   upgrade never started the command). So a transient 429 from API
///   Priority and Fairness, or a 503, no longer fails the night's backup; a
///   persistent one still fails it, only later. Retries fire on a response,
///   never on a timeout, so the read timeout still bounds every attempt.
///
/// Set, not inherited, so a later kube default cannot flip either silently.
pub fn runner_client_config(mut config: kube::Config) -> kube::Config {
    config.read_timeout = Some(CLIENT_READ_TIMEOUT);
    config.default_retry = true;
    config
}

/// Build the kube client from `config`, with the crypto provider installed
/// first and the runner's client settings applied
/// ([`runner_client_config`]).
///
/// Must be called inside a Tokio runtime context: the client wraps its service
/// in a `tower::Buffer`, which spawns a worker task.
pub fn kube_client(config: kube::Config) -> kube::Result<kube::Client> {
    install_rustls_crypto_provider();
    kube::Client::try_from(runner_client_config(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use k8s_openapi::api::core::v1::ConfigMap;
    use kube::Api;

    fn config_for(url: &str) -> kube::Config {
        kube::Config::new(url.parse().expect("test url"))
    }

    #[test]
    fn the_runner_client_has_the_pre_kube4_read_timeout_and_keeps_kube4_retries() {
        let base = config_for("http://127.0.0.1:1");
        // What kube 4.x hands us, so a default that moves again shows up here
        // rather than as a hung CronJob.
        assert_eq!(base.read_timeout, None, "kube's default moved again");
        assert!(base.default_retry, "kube's default moved again");

        let ours = runner_client_config(base.clone());
        assert_eq!(
            ours.read_timeout,
            Some(Duration::from_secs(295)),
            "kube 0.95's read timeout, which kube 4 dropped"
        );
        assert!(ours.default_retry, "the runner keeps kube 4's retries");

        // Set, not inherited: a config that arrives with retries off still
        // leaves with them on.
        let mut off = base;
        off.default_retry = false;
        assert!(runner_client_config(off).default_retry);
    }

    /// A stub apiserver on a plain OS thread, not a Tokio task, so nothing it
    /// does depends on the Tokio clock the silent-server test pauses.
    ///
    /// It answers the n-th request with `answers[n]` (status line + JSON body,
    /// closing the connection after each), and holds the first request past
    /// the end of `answers` open without sending a byte back — the half-open
    /// connection of a control-plane blip — until the client hangs up. Returns
    /// the URL, the number of requests seen, and a receiver that fires once
    /// that unanswered request has arrived.
    fn stub_apiserver(
        answers: Vec<(&'static str, &'static str)>,
    ) -> (String, Arc<AtomicUsize>, tokio::sync::oneshot::Receiver<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let (unanswered_tx, unanswered_rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let read_request_head = |stream: &TcpStream| {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                loop {
                    line.clear();
                    let n = reader.read_line(&mut line).expect("read request");
                    if n == 0 || line == "\r\n" {
                        break;
                    }
                }
            };
            for answer in answers {
                let (mut stream, _) = listener.accept().expect("accept");
                read_request_head(&stream);
                counter.fetch_add(1, Ordering::SeqCst);
                let (status, body) = answer;
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).expect("write reply");
            }
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            read_request_head(&stream);
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = unanswered_tx.send(());
            // Say nothing; return once the client gives up and closes.
            let _ = std::io::copy(&mut stream, &mut std::io::sink());
        });
        (url, hits, unanswered_rx)
    }

    const PROBE_CM: &str = r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"probe","namespace":"default"}}"#;

    /// The finding, end to end through the one constructor `main` uses: the
    /// apiserver takes a GET and never answers. On kube 0.95 the call failed
    /// after 295 s and the run exited 1; on a bare kube 4 client it blocks
    /// for ever — no exit, no `lastFailure`, no failure webhook, and with the
    /// CronJob's `concurrencyPolicy: Forbid` no later backup either.
    ///
    /// The Tokio clock is paused once the request is on the wire (not before:
    /// a paused clock would skip straight past the 30 s CONNECT timeout while
    /// the loopback connect is still in flight), and then auto-advances to
    /// whichever timer is next — the read timeout if there is one, else the
    /// hour-long guard, which turns a hang into a failure instead of a stuck
    /// test run.
    #[tokio::test]
    async fn a_request_the_apiserver_never_answers_fails_at_295s_instead_of_hanging() {
        let (url, hits, unanswered) = stub_apiserver(vec![]);
        let client = kube_client(config_for(&url)).expect("client");
        let api: Api<ConfigMap> = Api::namespaced(client, "default");
        let call = tokio::spawn(async move { api.get("probe").await });

        unanswered.await.expect("the request reached the stub");
        tokio::time::pause();
        let paused_at = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(Duration::from_secs(3600), call)
            .await
            .expect("the GET was still waiting for a reply an hour later: no read timeout")
            .expect("the GET task panicked");
        let waited = paused_at.elapsed();

        let err = outcome.expect_err("a server that never answers cannot produce a ConfigMap");
        let mut timed_out = false;
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&err);
        while let Some(e) = source {
            if let Some(io) = e.downcast_ref::<std::io::Error>() {
                timed_out |= io.kind() == std::io::ErrorKind::TimedOut;
            }
            source = e.source();
        }
        assert!(timed_out, "expected an I/O timeout, got {err:?}");
        // Either side of 295 s by a hair: the read timer may start a moment
        // before the pause, and Tokio's timer wheel rounds a deadline up to
        // the next millisecond.
        assert!(
            waited > Duration::from_secs(290) && waited < Duration::from_secs(296),
            "the read timeout fired after {waited:?}, not at 295 s"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "a timeout is not retried: exactly one request"
        );
    }

    /// The retry half of the decision, observed through a real client (the
    /// retry layer lives in the `Config` builder): a 503 on the first attempt
    /// is retried inside the call and the caller sees the success.
    #[tokio::test]
    async fn a_transient_503_is_retried_inside_the_call() {
        let (url, hits, _unanswered) = stub_apiserver(vec![
            (
                "503 Service Unavailable",
                r#"{"kind":"Status","apiVersion":"v1","status":"Failure","message":"etcd leader changed","reason":"ServiceUnavailable","code":503}"#,
            ),
            ("200 OK", PROBE_CM),
        ]);
        let client = kube_client(config_for(&url)).expect("client");
        let api: Api<ConfigMap> = Api::namespaced(client, "default");
        let cm = api
            .get("probe")
            .await
            .expect("the retry turns the 503 into the 200");
        assert_eq!(cm.metadata.name.as_deref(), Some("probe"));
        assert_eq!(hits.load(Ordering::SeqCst), 2, "one 503, one retry");
    }
}
