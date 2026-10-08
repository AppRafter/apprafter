// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Per-operation cancellation (ADR 0067 §2).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

/// Returned by [`CancellationToken::check`] once the token has tripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("operation cancelled")
    }
}

impl std::error::Error for Cancelled {}

type Callback = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Callbacks {
    next_id: u64,
    entries: Vec<(u64, Callback)>,
}

/// A test-only hook, see [`Inner::after_check`].
#[cfg(test)]
type Hook = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Inner {
    cancelled: AtomicBool,
    callbacks: Mutex<Callbacks>,
    /// Runs in `on_cancel` after it reads the flag and before it acts on
    /// it, so a test can land a `cancel` exactly in that window. Absent
    /// from non-test builds.
    #[cfg(test)]
    after_check: Mutex<Option<Hook>>,
}

#[cfg(test)]
impl Inner {
    fn run_after_check(&self) {
        let hook = self
            .after_check
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// One operation's cancellation. Clones share the same state.
///
/// It replaces the CLI's process-wide interrupt flag: the token is checked
/// before every spawn ([`CancellationToken::check`]), and whoever owns a
/// child process or a helper pod registers how to stop it
/// ([`CancellationToken::on_cancel`]).
#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// `Ok(())` until the token trips. Call it before starting anything new.
    pub fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    /// Trip the token. Every registered callback runs once, on this thread,
    /// in registration order. Later calls do nothing.
    pub fn cancel(&self) {
        if self.inner.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        let drained = {
            let mut cbs = self
                .inner
                .callbacks
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            std::mem::take(&mut cbs.entries)
        };
        for (_, f) in drained {
            f();
        }
    }

    /// Run `f` when the token trips — at once if it already has. Dropping
    /// the returned [`Registration`] unregisters `f` if it has not run.
    pub fn on_cancel(&self, f: impl FnOnce() + Send + 'static) -> Registration {
        let mut cbs = self
            .inner
            .callbacks
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let id = cbs.next_id;
        cbs.next_id += 1;
        let cancelled = self.is_cancelled();
        #[cfg(test)]
        self.inner.run_after_check();
        if cancelled {
            drop(cbs);
            f();
        } else {
            cbs.entries.push((id, Box::new(f)));
        }
        Registration {
            inner: Arc::downgrade(&self.inner),
            id,
        }
    }
}

/// Keeps a callback registered; dropping it unregisters the callback.
#[must_use = "dropping a Registration unregisters its callback at once"]
pub struct Registration {
    inner: Weak<Inner>,
    id: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            let mut cbs = inner.callbacks.lock().unwrap_or_else(|p| p.into_inner());
            cbs.entries.retain(|(id, _)| *id != self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn a_fresh_token_passes_check() {
        let t = CancellationToken::new();
        assert!(!t.is_cancelled());
        assert_eq!(t.check(), Ok(()));
    }

    #[test]
    fn cancel_trips_every_clone_and_check_fails() {
        let t = CancellationToken::new();
        let c = t.clone();
        t.cancel();
        assert!(c.is_cancelled());
        assert_eq!(c.check(), Err(Cancelled));
    }

    #[test]
    fn callbacks_run_once_in_registration_order() {
        let t = CancellationToken::new();
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (l1, l2) = (log.clone(), log.clone());
        let _r1 = t.on_cancel(move || l1.lock().unwrap().push(1));
        let _r2 = t.on_cancel(move || l2.lock().unwrap().push(2));
        t.cancel();
        t.cancel();
        assert_eq!(*log.lock().unwrap(), vec![1, 2]);
    }

    #[test]
    fn registering_after_cancel_runs_immediately() {
        let t = CancellationToken::new();
        t.cancel();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let _r = t.on_cancel(move || {
            h.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dropping_the_registration_unregisters() {
        let t = CancellationToken::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let r = t.on_cancel(move || {
            h.fetch_add(1, Ordering::SeqCst);
        });
        drop(r);
        t.cancel();
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cancel_from_another_thread_reaches_a_waiting_check() {
        let t = CancellationToken::new();
        let remote = t.clone();
        std::thread::spawn(move || remote.cancel()).join().unwrap();
        assert_eq!(t.check(), Err(Cancelled));
    }

    #[test]
    fn concurrent_register_and_cancel_never_lose_or_double_a_callback() {
        for _ in 0..200 {
            let t = CancellationToken::new();
            let hits = Arc::new(AtomicUsize::new(0));
            let (t2, h2) = (t.clone(), hits.clone());
            let reg = std::thread::spawn(move || {
                let h = h2.clone();
                t2.on_cancel(move || {
                    h.fetch_add(1, Ordering::SeqCst);
                })
            });
            let canceller = {
                let t = t.clone();
                std::thread::spawn(move || t.cancel())
            };
            let registration = reg.join().unwrap();
            canceller.join().unwrap();
            assert_eq!(hits.load(Ordering::SeqCst), 1);
            drop(registration);
        }
    }

    /// Forces the interleaving the sampled test above only hopes to hit:
    /// `cancel` trips the flag after `on_cancel` has read it and before
    /// `on_cancel` has queued the callback. `on_cancel` reads the flag under
    /// the callback lock, so `cancel` waits for the push and then drains it.
    #[test]
    fn a_cancel_landing_between_check_and_push_still_runs_the_callback_once() {
        use std::sync::{mpsc, Mutex, TryLockError};
        use std::time::{Duration, Instant};

        let t = CancellationToken::new();
        let (checked_tx, checked_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = Mutex::new(go_rx);
        *t.inner.after_check.lock().unwrap() = Some(Arc::new(move || {
            checked_tx.send(()).unwrap();
            go_rx.lock().unwrap().recv().unwrap();
        }));

        let hits = Arc::new(AtomicUsize::new(0));
        let (ta, h) = (t.clone(), hits.clone());
        let registering = std::thread::spawn(move || {
            ta.on_cancel(move || {
                h.fetch_add(1, Ordering::SeqCst);
            })
        });
        checked_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("on_cancel never reached its flag check");

        let tb = t.clone();
        let cancelling = std::thread::spawn(move || tb.cancel());

        // Hold the registration in its window until `cancel` has tripped the
        // flag AND reached the callback lock: either it is waiting on (or
        // holding) that lock, or it has already returned. Releasing earlier
        // would let the registration push before `cancel` drains, which
        // passes even when the flag is read outside the lock.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let lock_busy = matches!(t.inner.callbacks.try_lock(), Err(TryLockError::WouldBlock));
            if t.is_cancelled() && (lock_busy || cancelling.is_finished()) {
                break;
            }
            assert!(Instant::now() < deadline, "cancel never tripped the flag");
            std::thread::yield_now();
        }
        go_tx.send(()).unwrap();

        let registration = registering.join().unwrap();
        cancelling.join().unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        drop(registration);
    }
}
