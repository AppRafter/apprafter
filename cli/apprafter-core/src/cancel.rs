// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Per-operation cancellation (ADR 0067 §2).

use std::panic::{self, AssertUnwindSafe};
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
    /// in registration order, outside the token's lock. Later calls — from
    /// another thread, or from inside a callback — return at once, without
    /// waiting for the first call's callbacks to finish.
    ///
    /// A callback that panics does not stop the rest: each runs under
    /// `catch_unwind`, and once all have run the first panic is re-raised.
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
        let mut first_panic = None;
        for (_, f) in drained {
            if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(f)) {
                first_panic.get_or_insert(payload);
            }
        }
        if let Some(payload) = first_panic {
            panic::resume_unwind(payload);
        }
    }

    /// Run `f` when the token trips — at once, on this thread, if it already
    /// has. Dropping the returned [`Registration`] unregisters `f` if it has
    /// not run.
    ///
    /// The contract a callback is written against:
    /// - it runs synchronously on the thread that calls
    ///   [`cancel`](Self::cancel), so it must be quick: it signals a child
    ///   and hands slow work (deleting a helper pod) to another thread;
    /// - a concurrent second `cancel()` returns before the first one's
    ///   callbacks have finished, so "cancel returned" never means "the
    ///   children are gone";
    /// - dropping a `Registration` does not wait for a callback already
    ///   running on another thread, so the callback must stop its child
    ///   through a shared handle it owns, never through a saved raw pid that
    ///   the child's owner may already have reaped and the OS reused;
    /// - from inside a callback, `cancel()` returns at once, `on_cancel()`
    ///   runs its new callback inline, and dropping a `Registration` — its
    ///   own included — does not deadlock;
    /// - a panic in it is re-raised by `cancel()` after the other callbacks
    ///   have run.
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

    #[test]
    fn a_panicking_callback_does_not_stop_the_rest_and_cancel_re_panics_the_first() {
        use std::panic::{catch_unwind, AssertUnwindSafe};

        let t = CancellationToken::new();
        let ran = Arc::new(AtomicUsize::new(0));
        let r = ran.clone();
        let _r1 = t.on_cancel(|| panic!("first"));
        let _r2 = t.on_cancel(move || {
            r.fetch_add(1, Ordering::SeqCst);
        });
        let _r3 = t.on_cancel(|| panic!("third"));

        let payload = catch_unwind(AssertUnwindSafe(|| t.cancel()))
            .expect_err("cancel re-raises a callback's panic");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"first"));
        assert_eq!(ran.load(Ordering::SeqCst), 1, "the second callback ran");
        assert!(t.is_cancelled());
        t.cancel(); // tripped once; nothing left to run or re-raise
    }

    #[test]
    fn a_callback_that_cancels_again_returns() {
        let t = CancellationToken::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let (again, h) = (t.clone(), hits.clone());
        let _r = t.on_cancel(move || {
            again.cancel();
            h.fetch_add(1, Ordering::SeqCst);
        });
        t.cancel();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_callback_that_registers_runs_the_new_callback_inline() {
        let t = CancellationToken::new();
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (inner_token, outer_log) = (t.clone(), log.clone());
        let _r = t.on_cancel(move || {
            outer_log.lock().unwrap().push("outer begins");
            let inner_log = outer_log.clone();
            let _inner = inner_token.on_cancel(move || inner_log.lock().unwrap().push("inner"));
            outer_log.lock().unwrap().push("outer ends");
        });
        t.cancel();
        assert_eq!(
            *log.lock().unwrap(),
            vec!["outer begins", "inner", "outer ends"]
        );
    }

    #[test]
    fn a_callback_that_drops_its_own_registration_does_not_deadlock() {
        use std::sync::{mpsc, Mutex};
        use std::time::Duration;

        let t = CancellationToken::new();
        let slot: Arc<Mutex<Option<Registration>>> = Arc::new(Mutex::new(None));
        let hits = Arc::new(AtomicUsize::new(0));
        let (own, h) = (slot.clone(), hits.clone());
        let registration = t.on_cancel(move || {
            drop(own.lock().unwrap().take());
            h.fetch_add(1, Ordering::SeqCst);
        });
        *slot.lock().unwrap() = Some(registration);

        let (done_tx, done_rx) = mpsc::channel();
        let canceller = t.clone();
        std::thread::spawn(move || {
            canceller.cancel();
            done_tx.send(()).unwrap();
        });
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("cancel deadlocked on a callback dropping its own Registration");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(slot.lock().unwrap().is_none());
    }

    /// Forces the interleaving the sampled test above only hopes to hit:
    /// `cancel` trips the flag after `on_cancel` has read it and before
    /// `on_cancel` has queued the callback. `on_cancel` reads the flag under
    /// the callback lock, so `cancel` waits for the push and then drains it.
    ///
    /// The hook runs while `on_cancel` holds that lock, so against the real
    /// implementation the wait below reduces to `is_cancelled()`: the lock
    /// is always busy. The lock probe is there for the mutant that reads the
    /// flag outside the lock — then the hook runs unlocked, and releasing it
    /// before `cancel` holds (or waits on) the lock would let the push land
    /// first and the test pass for the wrong reason.
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
        // flag AND reached the callback lock: either the lock is busy (always
        // so here, where the registering thread's hook holds it; under the
        // mutant, because `cancel` holds or waits on it), or `cancel` has
        // already returned.
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
