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

use std::any::Any;
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
    /// Milliseconds on a clock that never steps back, from an arbitrary start. It may stand
    /// still while the machine sleeps, so a deadline is measured on both clocks: see
    /// [`elapsed_ms`].
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

/// What a caught panic said, for an error message or a log line.
pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message")
}

/// One moment, read on both of a [`Clock`]'s clocks, to measure [`elapsed_ms`] from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub wall_ms: u64,
    pub monotonic_ms: u64,
}

impl Stamp {
    pub fn now(clock: &dyn Clock) -> Self {
        Self {
            wall_ms: clock.now_ms(),
            monotonic_ms: clock.monotonic_ms(),
        }
    }

    /// [`elapsed_ms`] since this moment.
    pub fn elapsed_ms(&self, clock: &dyn Clock) -> u64 {
        elapsed_ms(self.wall_ms, self.monotonic_ms, clock)
    }
}

/// How much time has passed since the moment `clock` read as `start_wall` (wall) and
/// `start_mono` (monotonic): the larger of the monotonic and the forward wall elapsed time.
/// Every expiry and idle time in the shell is measured with it, and with nothing else.
///
/// Neither clock is enough alone. The monotonic one misses a suspend: `Instant` does not
/// advance while the machine sleeps on Linux and macOS, so a plan shown before an overnight
/// lid-close would still be fresh on wake. The wall one can be stepped back (a time sync, the
/// owner changing the date) to keep a plan alive or an idle app unlocked. So a wall step back
/// counts as nothing elapsed, never as negative, and a wall step forward counts in full: a
/// deadline can come early, never late.
pub fn elapsed_ms(start_wall: u64, start_mono: u64, clock: &dyn Clock) -> u64 {
    let monotonic = clock.monotonic_ms().saturating_sub(start_mono);
    let wall = clock.now_ms().saturating_sub(start_wall);
    monotonic.max(wall)
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
    use super::{elapsed_ms, test_trips, trip, Clock, Stamp, SystemClock};

    const T0: u64 = 1_700_000_000_000;
    const DAY: u64 = 24 * 60 * 60 * 1000;

    #[test]
    fn time_that_passes_on_both_clocks_is_elapsed_once() {
        let clock = ManualClock::at(T0);
        let start = Stamp::now(&clock);
        assert_eq!(start.elapsed_ms(&clock), 0);
        clock.advance(250);
        assert_eq!(start.elapsed_ms(&clock), 250);
        assert_eq!(
            elapsed_ms(start.wall_ms, start.monotonic_ms, &clock),
            250,
            "the stamp and the function are one rule"
        );
    }

    #[test]
    fn a_suspend_counts_the_wall_time_the_monotonic_clock_missed() {
        // From inside, a suspend is the wall clock jumping while the monotonic one stood still.
        let clock = ManualClock::at(T0);
        let start = Stamp::now(&clock);
        clock.advance(10);
        clock.set_wall(T0 + DAY);
        assert_eq!(start.elapsed_ms(&clock), DAY);
    }

    #[test]
    fn a_wall_step_back_counts_as_nothing_and_never_as_negative() {
        let clock = ManualClock::at(T0);
        let start = Stamp::now(&clock);
        clock.set_wall(T0 - DAY);
        assert_eq!(start.elapsed_ms(&clock), 0, "no time passed");
        clock.advance(300);
        assert_eq!(
            start.elapsed_ms(&clock),
            300,
            "the monotonic time still counts in full"
        );
        // Below the epoch is no special case either.
        let clock = ManualClock::at(5);
        let start = Stamp::now(&clock);
        clock.set_wall(0);
        assert_eq!(start.elapsed_ms(&clock), 0);
    }

    #[test]
    fn the_larger_of_the_two_is_what_passed() {
        let clock = ManualClock::at(T0);
        let start = Stamp::now(&clock);
        clock.advance(1_000);
        // A time sync pulls the wall clock back a little: the monotonic reading wins.
        clock.set_wall(T0 + 400);
        assert_eq!(start.elapsed_ms(&clock), 1_000);
        // And pushes it ahead: the wall reading wins.
        clock.set_wall(T0 + 5_000);
        assert_eq!(start.elapsed_ms(&clock), 5_000);
    }

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
