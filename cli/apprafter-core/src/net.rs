// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Network calls with a bound on every step (D.3 overview §3.4): name resolution on a helper
//! thread abandoned at a deadline (`to_socket_addrs` has none), and a TCP connect probe.

use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::{CancellationToken, Cancelled};

/// A `ureq` resolver whose lookups give up after `timeout`. ureq hands a resolver no operation
/// to answer to, so its lookups are bounded by the deadline alone (overview §3.4).
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
        resolve_with_deadline(netloc, self.timeout, &CancellationToken::new())
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
/// `ErrorKind::TimedOut`; a tripped `cancel` ends the wait within 50 ms, as
/// `ErrorKind::Interrupted` carrying [`Cancelled`]. Either way the lookup thread is abandoned,
/// not joined (`to_socket_addrs` cannot be stopped).
pub fn resolve_with_deadline(
    netloc: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> io::Result<Vec<SocketAddr>> {
    #[cfg(test)]
    if let Some(stub) = LOOKUP_STUB.with(|s| s.borrow().clone()) {
        return resolve_on_thread(netloc, timeout, cancel, move |n| stub(n));
    }
    resolve_on_thread(netloc, timeout, cancel, |n| {
        n.to_socket_addrs().map(Iterator::collect)
    })
}

/// What a test resolves names with instead of the system's, on the thread that set it.
#[cfg(test)]
type Lookup = std::sync::Arc<dyn Fn(&str) -> io::Result<Vec<SocketAddr>> + Send + Sync>;

#[cfg(test)]
thread_local! {
    static LOOKUP_STUB: std::cell::RefCell<Option<Lookup>> = const { std::cell::RefCell::new(None) };
}

/// Make [`resolve_with_deadline`] on this thread answer with `lookup` (run on the lookup
/// thread, as the system's is) until the returned guard drops: a core test can hang a lookup,
/// or trip a token from inside one.
#[cfg(test)]
pub(crate) fn stub_lookup(
    lookup: impl Fn(&str) -> io::Result<Vec<SocketAddr>> + Send + Sync + 'static,
) -> impl Drop {
    struct Unstub;
    impl Drop for Unstub {
        fn drop(&mut self) {
            LOOKUP_STUB.with(|s| *s.borrow_mut() = None);
        }
    }
    LOOKUP_STUB.with(|s| *s.borrow_mut() = Some(std::sync::Arc::new(lookup)));
    Unstub
}

fn resolve_on_thread<F>(
    netloc: &str,
    timeout: Duration,
    cancel: &CancellationToken,
    lookup: F,
) -> io::Result<Vec<SocketAddr>>
where
    F: FnOnce(&str) -> io::Result<Vec<SocketAddr>> + Send + 'static,
{
    if let Ok(addr) = netloc.parse::<SocketAddr>() {
        return Ok(vec![addr]);
    }
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    let (tx, rx) = mpsc::channel();
    let owned = netloc.to_string();
    std::thread::Builder::new()
        .name("dns".into())
        .spawn(move || {
            let _ = tx.send(lookup(&owned));
        })?;
    // Wait in slices: an answer that has arrived wins, a tripped token ends the wait within one
    // slice, and either way the lookup thread is abandoned as it is at the deadline.
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left.min(CANCEL_SLICE)) {
            Ok(answer) => return answer,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("the DNS lookup ended without an answer"))
            }
            Err(mpsc::RecvTimeoutError::Timeout) if cancel.is_cancelled() => {
                return Err(cancelled())
            }
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "no DNS answer for {netloc} within {} ms",
                        timeout.as_millis()
                    ),
                ))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

/// How long a lookup's wait goes without looking at its token.
const CANCEL_SLICE: Duration = Duration::from_millis(50);

/// What a tripped token ends a lookup or a probe with: `ErrorKind::Interrupted`, carrying
/// [`Cancelled`].
fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, Cancelled)
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
/// A tripped `cancel` is `ErrorKind::Interrupted`, before the lookup or during it.
pub fn tcp_probe(
    host: &str,
    port: u16,
    timeout: Duration,
    cancel: &CancellationToken,
) -> io::Result<Duration> {
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    let started = Instant::now();
    let netloc = netloc(host, port);
    let mut last = io::Error::new(io::ErrorKind::NotFound, format!("{host} has no address"));
    for addr in resolve_with_deadline(&netloc, timeout, cancel)? {
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

    use crate::{CancellationToken, Cancelled};

    #[test]
    fn a_literal_address_needs_no_lookup() {
        let got = resolve_on_thread("127.0.0.1:443", Duration::from_millis(50), &never(), |_| {
            panic!("looked up")
        })
        .unwrap();
        assert_eq!(got, vec!["127.0.0.1:443".parse().unwrap()]);
    }

    #[test]
    fn a_lookup_that_hangs_is_abandoned_at_the_deadline() {
        let started = Instant::now();
        let err = resolve_on_thread(
            "slow.example:443",
            Duration::from_millis(100),
            &never(),
            |_| {
                std::thread::sleep(Duration::from_secs(5));
                Ok(vec![])
            },
        )
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
            resolve_on_thread("x:443", Duration::from_secs(1), &never(), move |_| Ok(
                vec![addr]
            ))
            .unwrap(),
            vec![addr]
        );
    }

    /// Review findings 2, 5 and 7: a lookup that hangs (a resolver that never answers, the very
    /// reason to run doctor) ends within one slice of the token tripping, as `Interrupted`
    /// carrying [`Cancelled`], not at the deadline.
    #[test]
    fn a_cancel_ends_a_hung_lookup_long_before_its_deadline() {
        let cancel = CancellationToken::new();
        let trip = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            trip.cancel();
        });
        let started = Instant::now();
        let err = resolve_on_thread("hung.example:443", Duration::from_secs(10), &cancel, |_| {
            std::thread::sleep(Duration::from_secs(10));
            Ok(vec![])
        })
        .unwrap_err();
        let took = started.elapsed();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted, "{err:?}");
        assert!(
            err.get_ref().is_some_and(|e| e.is::<Cancelled>()),
            "{err:?}"
        );
        assert!(took < Duration::from_secs(1), "{took:?}");
    }

    #[test]
    fn a_token_tripped_before_the_lookup_starts_none() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = resolve_on_thread("x.example:443", Duration::from_secs(1), &cancel, |_| {
            panic!("looked up")
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
    }

    /// A token that never trips, for the lookups these tests do not cancel.
    fn never() -> CancellationToken {
        CancellationToken::new()
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
        let got = resolve_on_thread(
            &netloc("::1", 22),
            Duration::from_millis(50),
            &never(),
            |_| panic!("looked up"),
        )
        .unwrap();
        assert_eq!(got, vec!["[::1]:22".parse().unwrap()]);
    }
}
