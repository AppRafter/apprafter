// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Network calls with a bound on every step (D.3 overview §3.4): name resolution on a helper
//! thread abandoned at a deadline (`to_socket_addrs` has none), and a TCP connect probe.

use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::CancellationToken;

/// A `ureq` resolver whose lookups give up after `timeout`.
#[derive(Debug, Clone, Copy)]
pub struct DeadlineResolver {
    timeout: Duration,
}

impl DeadlineResolver {
    pub fn new(timeout: Duration) -> Self {
        DeadlineResolver { timeout }
    }
}

impl ureq::Resolver for DeadlineResolver {
    fn resolve(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        #[cfg(test)]
        LOOKUPS_ON_THIS_THREAD.with(|n| n.set(n.get() + 1));
        resolve_with_deadline(netloc, self.timeout)
    }
}

#[cfg(test)]
thread_local! {
    /// How many names a [`DeadlineResolver`] was asked for on this thread (ureq resolves on the
    /// thread making the request): the context tests prove the core's agent resolves through it.
    pub(crate) static LOOKUPS_ON_THIS_THREAD: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// `netloc` (`host:port`, an IPv6 host in brackets) resolved within `timeout`, else
/// `ErrorKind::TimedOut`; the lookup thread is abandoned, not joined.
pub fn resolve_with_deadline(netloc: &str, timeout: Duration) -> io::Result<Vec<SocketAddr>> {
    resolve_on_thread(netloc, timeout, |n| {
        n.to_socket_addrs().map(Iterator::collect)
    })
}

fn resolve_on_thread<F>(netloc: &str, timeout: Duration, lookup: F) -> io::Result<Vec<SocketAddr>>
where
    F: FnOnce(&str) -> io::Result<Vec<SocketAddr>> + Send + 'static,
{
    if let Ok(addr) = netloc.parse::<SocketAddr>() {
        return Ok(vec![addr]);
    }
    let (tx, rx) = mpsc::channel();
    let owned = netloc.to_string();
    std::thread::Builder::new()
        .name("dns".into())
        .spawn(move || {
            let _ = tx.send(lookup(&owned));
        })?;
    match rx.recv_timeout(timeout) {
        Ok(answer) => answer,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "no DNS answer for {netloc} within {} ms",
                timeout.as_millis()
            ),
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("the DNS lookup ended without an answer"))
        }
    }
}

/// `host:port`, with an IPv6 literal host in brackets (`[::1]:22`) unless it already has them.
fn netloc(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Resolve `host` and connect to `port` within `timeout` in total; the time the connect took.
/// A tripped `cancel` is `ErrorKind::Interrupted`, before any lookup.
pub fn tcp_probe(
    host: &str,
    port: u16,
    timeout: Duration,
    cancel: &CancellationToken,
) -> io::Result<Duration> {
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    let started = Instant::now();
    let netloc = netloc(host, port);
    let mut last = io::Error::new(io::ErrorKind::NotFound, format!("{host} has no address"));
    for addr in resolve_with_deadline(&netloc, timeout)? {
        let left = timeout.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "no connection to {netloc} within {} ms",
                    timeout.as_millis()
                ),
            ));
        }
        match TcpStream::connect_timeout(&addr, left) {
            Ok(_) => return Ok(started.elapsed()),
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::net::SocketAddr;
    use std::time::{Duration, Instant};

    use crate::CancellationToken;

    #[test]
    fn a_literal_address_needs_no_lookup() {
        let got = resolve_on_thread("127.0.0.1:443", Duration::from_millis(50), |_| {
            panic!("looked up")
        })
        .unwrap();
        assert_eq!(got, vec!["127.0.0.1:443".parse().unwrap()]);
    }

    #[test]
    fn a_lookup_that_hangs_is_abandoned_at_the_deadline() {
        let started = Instant::now();
        let err = resolve_on_thread("slow.example:443", Duration::from_millis(100), |_| {
            std::thread::sleep(Duration::from_secs(5));
            Ok(vec![])
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_lookup_answer_comes_back() {
        let addr: SocketAddr = "10.0.0.1:443".parse().unwrap();
        assert_eq!(
            resolve_on_thread("x:443", Duration::from_secs(1), move |_| Ok(vec![addr])).unwrap(),
            vec![addr]
        );
    }

    #[test]
    fn a_tcp_probe_connects_or_fails_and_honours_cancel() {
        let open = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = open.local_addr().unwrap().port();
        let cancel = CancellationToken::new();
        assert!(tcp_probe("127.0.0.1", port, Duration::from_secs(2), &cancel).is_ok());
        drop(open);
        // Not `port`: a listener this test drops may still accept. A test that spawns through
        // `pre_exec` (process::) forks, and its child holds a copy of every socket until its
        // exec, so the listener can outlive `drop`. Nothing listens on port 1 of loopback (the
        // dead API base of the other tests here).
        assert!(tcp_probe("127.0.0.1", 1, Duration::from_secs(2), &cancel).is_err());
        cancel.cancel();
        assert_eq!(
            tcp_probe("127.0.0.1", port, Duration::from_secs(2), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }

    /// GOTCHA-96: std's `ToSocketAddrs` resolves `::1:22` as well as `[::1]:22`, so only a test
    /// of the spelling itself catches a lost bracket; the bracketed form is also the one the
    /// literal fast path parses without a lookup thread.
    #[test]
    fn an_ipv6_host_is_bracketed_and_parses_without_a_lookup() {
        assert_eq!(netloc("::1", 22), "[::1]:22");
        assert_eq!(netloc("[::1]", 22), "[::1]:22");
        assert_eq!(netloc("203.0.113.10", 22), "203.0.113.10:22");
        assert_eq!(netloc("api.hetzner.cloud", 443), "api.hetzner.cloud:443");
        let got = resolve_on_thread(&netloc("::1", 22), Duration::from_millis(50), |_| {
            panic!("looked up")
        })
        .unwrap();
        assert_eq!(got, vec!["[::1]:22".parse().unwrap()]);
    }
}
