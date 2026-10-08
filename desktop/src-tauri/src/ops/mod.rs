// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Operations as the shell runs them: core events in, [`OpEvent`]s out.
//!
//! [`text`] turns a tool's raw output bytes into text, [`reporter`] coalesces that text and
//! converts every core event, [`replay`] keeps a bounded history so a page that reloads
//! or re-attaches can catch up, and [`manager`] holds plans and runs operations.
//!
//! [`OpEvent`]: apprafter_desktop_ipc::OpEvent

pub mod manager;
pub mod replay;
pub mod reporter;
pub mod text;

use std::panic::{self, AssertUnwindSafe};
use std::sync::OnceLock;
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use apprafter_core::CancellationToken;

pub use manager::{EventSink, Executor, OperationManager, PlanParts, PLAN_TTL_MS};
pub use replay::ReplayBuffer;
pub use reporter::OpReporter;
pub use text::Utf8Stream;

/// Time, injected so tests drive it by hand.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch: what the webview shows, and what file names carry.
    /// It can step either way (a time sync, the owner changing the date).
    fn now_ms(&self) -> u64;
    /// Milliseconds on a clock that never steps back, from an arbitrary start: what a
    /// deadline is measured on.
    fn monotonic_ms(&self) -> u64;
}

/// The operating system's clocks.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| saturating_ms(since.as_millis()))
    }

    /// Since the first reading in this process.
    fn monotonic_ms(&self) -> u64 {
        static ANCHOR: OnceLock<Instant> = OnceLock::new();
        saturating_ms(ANCHOR.get_or_init(Instant::now).elapsed().as_millis())
    }
}

fn saturating_ms(ms: u128) -> u64 {
    u64::try_from(ms).unwrap_or(u64::MAX)
}

/// Trip `token` on a short-lived thread named `name`. `CancellationToken::cancel` runs every
/// callback on the calling thread and then re-raises the first callback's panic, so it must
/// never run on an async worker, under a lock its callbacks may take, or on a command's
/// thread. Whoever closes a prompt this way decides what its answer counts for under its own
/// lock first: the token only closes the OS dialog, some time after this returns.
pub(crate) fn trip(token: CancellationToken, name: &str) {
    #[cfg(test)]
    let Some(token) = test_trips::hold(token) else {
        return;
    };
    let spawned = thread::Builder::new().name(name.into()).spawn({
        let token = token.clone();
        move || token.cancel()
    });
    if spawned.is_err() {
        // No thread to be had: trip it here rather than not at all, and keep a callback's
        // panic from reaching the caller.
        let _ = panic::catch_unwind(AssertUnwindSafe(|| token.cancel()));
    }
}

/// Holding [`trip`]s back, so a test can make an answer arrive before the thread that closes
/// its prompt has run — the window a scheduler opens only sometimes.
#[cfg(test)]
pub(crate) mod test_trips {
    use std::cell::RefCell;

    use apprafter_core::CancellationToken;

    thread_local! {
        static HELD: RefCell<Option<Vec<CancellationToken>>> = const { RefCell::new(None) };
    }

    /// Run `f`; every token it trips on this thread comes back untripped instead.
    pub(crate) fn held<T>(f: impl FnOnce() -> T) -> (T, Vec<CancellationToken>) {
        HELD.with(|held| *held.borrow_mut() = Some(Vec::new()));
        let value = f();
        let tokens = HELD.with(|held| held.borrow_mut().take().unwrap_or_default());
        (value, tokens)
    }

    /// `None` when the token was held back.
    pub(super) fn hold(token: CancellationToken) -> Option<CancellationToken> {
        HELD.with(|held| match held.borrow_mut().as_mut() {
            Some(tokens) => {
                tokens.push(token);
                None
            }
            None => Some(token),
        })
    }
}

#[cfg(test)]
pub(crate) mod test_clock {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::Clock;

    /// Where a [`ManualClock`]'s monotonic reading starts: far from any wall time a test
    /// uses, so a deadline measured on the wrong clock is obvious.
    const MONOTONIC_START: u64 = 1_000;

    /// A clock that only moves when a test moves it.
    #[derive(Debug)]
    pub(crate) struct ManualClock {
        wall: AtomicU64,
        monotonic: AtomicU64,
    }

    impl ManualClock {
        pub(crate) fn at(ms: u64) -> Self {
            Self {
                wall: AtomicU64::new(ms),
                monotonic: AtomicU64::new(MONOTONIC_START),
            }
        }

        /// Time passes until the wall clock reads `ms`: the monotonic clock moves as far.
        /// An `ms` in the past steps the wall clock back alone, as [`set_wall`] does.
        ///
        /// [`set_wall`]: Self::set_wall
        pub(crate) fn set(&self, ms: u64) {
            let was = self.wall.swap(ms, Ordering::SeqCst);
            self.monotonic
                .fetch_add(ms.saturating_sub(was), Ordering::SeqCst);
        }

        /// `ms` pass, on both clocks.
        pub(crate) fn advance(&self, ms: u64) {
            self.wall.fetch_add(ms, Ordering::SeqCst);
            self.monotonic.fetch_add(ms, Ordering::SeqCst);
        }

        /// The wall clock steps to `ms` (a time sync, the owner changing the date) while no
        /// time passes.
        pub(crate) fn set_wall(&self, ms: u64) {
            self.wall.store(ms, Ordering::SeqCst);
        }
    }

    impl Clock for ManualClock {
        fn now_ms(&self) -> u64 {
            self.wall.load(Ordering::SeqCst)
        }

        fn monotonic_ms(&self) -> u64 {
            self.monotonic.load(Ordering::SeqCst)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use apprafter_core::CancellationToken;

    use super::test_clock::ManualClock;
    use super::{test_trips, trip, Clock, SystemClock};

    #[test]
    fn the_system_clock_reads_the_epoch_and_never_steps_back() {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(SystemClock.now_ms().abs_diff(epoch) < 60_000);
        let mut last = SystemClock.monotonic_ms();
        for _ in 0..1_000 {
            let now = SystemClock.monotonic_ms();
            assert!(now >= last, "{now} after {last}");
            last = now;
        }
    }

    #[test]
    fn a_manual_clock_moves_both_readings_unless_only_the_wall_steps() {
        let clock = ManualClock::at(5_000);
        let start = clock.monotonic_ms();
        clock.advance(10);
        clock.set(5_100);
        assert_eq!((clock.now_ms(), clock.monotonic_ms()), (5_100, start + 100));
        clock.set_wall(1);
        assert_eq!((clock.now_ms(), clock.monotonic_ms()), (1, start + 100));
        clock.set(3);
        assert_eq!((clock.now_ms(), clock.monotonic_ms()), (3, start + 102));
        clock.set(0);
        assert_eq!(
            (clock.now_ms(), clock.monotonic_ms()),
            (0, start + 102),
            "a set into the past steps the wall clock alone"
        );
    }

    #[test]
    fn trip_cancels_on_a_named_thread_and_keeps_a_callback_panic_to_itself() {
        let token = CancellationToken::new();
        let (tx, rx) = mpsc::channel();
        let _named = token.on_cancel(move || {
            let _ = tx.send(std::thread::current().name().map(str::to_owned));
        });
        let _boom = token.on_cancel(|| panic!("a cancel callback panicked"));
        trip(token.clone(), "test-cancel");
        let name = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(name.as_deref(), Some("test-cancel"));
        assert!(token.is_cancelled());
    }

    #[test]
    fn a_held_trip_leaves_the_token_untripped_until_the_test_trips_it() {
        let runs = Arc::new(AtomicUsize::new(0));
        let token = CancellationToken::new();
        let _counted = {
            let runs = runs.clone();
            token.on_cancel(move || {
                runs.fetch_add(1, SeqCst);
            })
        };
        let ((), held) = test_trips::held(|| trip(token.clone(), "test-cancel"));
        assert_eq!(held.len(), 1);
        assert!(!token.is_cancelled());
        held[0].cancel();
        assert!(token.is_cancelled());
        assert_eq!(runs.load(SeqCst), 1);
        // Outside `held`, a trip goes through again.
        let other = CancellationToken::new();
        let (tx, rx) = mpsc::channel();
        let _seen = other.on_cancel(move || {
            let _ = tx.send(());
        });
        trip(other, "test-cancel");
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }
}
