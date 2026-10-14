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

pub use manager::{EventSink, Executor, OperationManager, PlanParts, SecretGesture, PLAN_TTL_MS};
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

    /// Milliseconds since boot on a clock that counts suspend (Linux `CLOCK_BOOTTIME`, macOS
    /// `CLOCK_MONOTONIC_RAW`, Windows `GetTickCount64`): the first of O4's pair (D.4a).
    /// Never `SystemTime`, which steps, nor `Instant`, whose counting of suspend std leaves
    /// unspecified. No default: every clock says what it reads (the final verifier's finding
    /// 2 — a default of `monotonic_ms` would see no sleep, silently).
    fn with_sleep_ms(&self) -> u64;

    /// The same, suspend excluded (Linux `CLOCK_MONOTONIC`, macOS `CLOCK_UPTIME_RAW`, Windows
    /// `QueryUnbiasedInterruptTime`): the second of O4's pair. No default either.
    fn awake_ms(&self) -> u64;
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

    fn with_sleep_ms(&self) -> u64 {
        sleep_clocks::with_sleep_ms()
    }

    fn awake_ms(&self) -> u64 {
        sleep_clocks::awake_ms()
    }
}

fn saturating_ms(ms: u128) -> u64 {
    u64::try_from(ms).unwrap_or(u64::MAX)
}

/// O4's rule, the CLI holder's (`window::SLEEP_GAP`): between two looks the clock that counts
/// suspend ran more than this ahead of the one that does not — the machine slept.
pub const SLEEP_GAP_MS: u64 = 5_000;

/// One look at O4's two clocks ([`Clock::with_sleep_ms`], [`Clock::awake_ms`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockPair {
    pub with_sleep_ms: u64,
    pub awake_ms: u64,
}

impl ClockPair {
    pub fn now(clock: &dyn Clock) -> Self {
        Self {
            with_sleep_ms: clock.with_sleep_ms(),
            awake_ms: clock.awake_ms(),
        }
    }

    /// Whether the machine slept since `before`: more than [`SLEEP_GAP_MS`] passed on the clock
    /// that counts suspend beyond the one that does not. Each look measures from the previous
    /// one, so two short naps never add up (the holder's rule).
    pub fn slept_since(&self, before: ClockPair) -> bool {
        let passed = self.with_sleep_ms.saturating_sub(before.with_sleep_ms);
        let awake = self.awake_ms.saturating_sub(before.awake_ms);
        passed.saturating_sub(awake) > SLEEP_GAP_MS
    }
}

/// The OS's two clocks for O4, read as the CLI's holder reads them (its `os::*`; HG4 keeps the
/// desktop from naming that code, so it is mirrored here, with its tests).
mod sleep_clocks {
    #[cfg(target_os = "linux")]
    pub(super) const WITH_SLEEP: libc::clockid_t = libc::CLOCK_BOOTTIME;
    #[cfg(target_os = "linux")]
    pub(super) const AWAKE: libc::clockid_t = libc::CLOCK_MONOTONIC;
    /// Raw, as `AWAKE` is: `CLOCK_MONOTONIC` is slewed, and its readings drift from a raw
    /// clock's (e's CI finding).
    #[cfg(target_os = "macos")]
    pub(super) const WITH_SLEEP: libc::clockid_t = libc::CLOCK_MONOTONIC_RAW;
    #[cfg(target_os = "macos")]
    pub(super) const AWAKE: libc::clockid_t = libc::CLOCK_UPTIME_RAW;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn read(id: libc::clockid_t) -> u64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock_gettime writes one timespec; both clocks exist on this OS.
        unsafe { libc::clock_gettime(id, &mut ts) };
        (ts.tv_sec as u64)
            .saturating_mul(1000)
            .saturating_add(ts.tv_nsec as u64 / 1_000_000)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn with_sleep_ms() -> u64 {
        read(WITH_SLEEP)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn awake_ms() -> u64 {
        read(AWAKE)
    }

    /// Milliseconds since boot, suspend included, at the system tick's resolution (the
    /// holder's reader; 15.6 ms is nothing against a 5 s gap).
    #[cfg(windows)]
    pub(super) fn with_sleep_ms() -> u64 {
        // SAFETY: no arguments.
        unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() }
    }

    /// The interrupt time the system spent in the working state, in 100 ns units.
    #[cfg(windows)]
    pub(super) fn awake_ms() -> u64 {
        let mut t = 0u64;
        // SAFETY: writes one u64.
        unsafe {
            windows_sys::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime(&mut t)
        };
        t / 10_000
    }
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
        /// O4's pair: the clock that counts suspend, and the one that does not.
        with_sleep: AtomicU64,
        awake: AtomicU64,
    }

    impl ManualClock {
        pub(crate) fn at(ms: u64) -> Self {
            Self {
                wall: AtomicU64::new(ms),
                monotonic: AtomicU64::new(MONOTONIC_START),
                with_sleep: AtomicU64::new(MONOTONIC_START),
                awake: AtomicU64::new(MONOTONIC_START),
            }
        }

        /// Time passes until the wall clock reads `ms`: the monotonic clock, and O4's two, move
        /// as far.
        /// An `ms` in the past steps the wall clock back alone, as [`set_wall`] does.
        ///
        /// [`set_wall`]: Self::set_wall
        pub(crate) fn set(&self, ms: u64) {
            let was = self.wall.swap(ms, Ordering::SeqCst);
            self.monotonic
                .fetch_add(ms.saturating_sub(was), Ordering::SeqCst);
            self.with_sleep
                .fetch_add(ms.saturating_sub(was), Ordering::SeqCst);
            self.awake
                .fetch_add(ms.saturating_sub(was), Ordering::SeqCst);
        }

        /// `ms` pass, on every clock.
        pub(crate) fn advance(&self, ms: u64) {
            self.wall.fetch_add(ms, Ordering::SeqCst);
            self.monotonic.fetch_add(ms, Ordering::SeqCst);
            self.with_sleep.fetch_add(ms, Ordering::SeqCst);
            self.awake.fetch_add(ms, Ordering::SeqCst);
        }

        /// The wall clock steps to `ms` (a time sync, the owner changing the date) while no
        /// time passes.
        pub(crate) fn set_wall(&self, ms: u64) {
            self.wall.store(ms, Ordering::SeqCst);
        }

        /// The machine sleeps for `ms`: the wall clock and the clock that counts suspend move,
        /// the monotonic and the awake ones stand still (O4's tests).
        pub(crate) fn sleep(&self, ms: u64) {
            self.wall.fetch_add(ms, Ordering::SeqCst);
            self.with_sleep.fetch_add(ms, Ordering::SeqCst);
        }
    }

    impl Clock for ManualClock {
        fn now_ms(&self) -> u64 {
            self.wall.load(Ordering::SeqCst)
        }

        fn monotonic_ms(&self) -> u64 {
            self.monotonic.load(Ordering::SeqCst)
        }

        fn with_sleep_ms(&self) -> u64 {
            self.with_sleep.load(Ordering::SeqCst)
        }

        fn awake_ms(&self) -> u64 {
            self.awake.load(Ordering::SeqCst)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use apprafter_core::CancellationToken;

    use super::test_clock::ManualClock;
    use super::{elapsed_ms, test_trips, trip, Clock, ClockPair, Stamp, SystemClock, SLEEP_GAP_MS};

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

    /// D.4a O4: the pair O4 compares, read as the CLI's holder reads it (e's
    /// `the_system_clocks_advance_together_while_awake`): both advance while awake, the one
    /// that counts suspend is never behind, and they stay well under 5 s apart then. Which clock
    /// each reads is the next three tests', and that the first counts suspend the walk's.
    #[test]
    fn the_sleep_clocks_advance_together_while_awake() {
        let c = SystemClock;
        let (s0, a0) = (c.with_sleep_ms(), c.awake_ms());
        thread::sleep(Duration::from_millis(50));
        let (s1, a1) = (c.with_sleep_ms(), c.awake_ms());
        assert!(s1 > s0 && a1 > a0, "{s0} {s1} {a0} {a1}");
        // 100 ms covers Windows' tick granularity (GetTickCount64).
        assert!(s1 + 100 >= a1, "{s1} {a1}");
        assert!((s1 - s0).abs_diff(a1 - a0) < SLEEP_GAP_MS);
    }

    /// The final verifier's finding 2: `SystemClock` reads the two clocks it names, each within
    /// 50 ms of a direct read of that OS clock — never `Instant` (`monotonic_ms`, anchored at
    /// this process's first reading) in their place. The clock ids are written out here, not
    /// taken from `sleep_clocks`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn the_system_clock_reads_the_named_clocks() {
        fn direct(id: libc::clockid_t) -> u64 {
            let mut ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: clock_gettime writes one timespec.
            assert_eq!(unsafe { libc::clock_gettime(id, &mut ts) }, 0);
            ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
        }
        #[cfg(target_os = "linux")]
        let (with_sleep, awake) = (libc::CLOCK_BOOTTIME, libc::CLOCK_MONOTONIC);
        #[cfg(target_os = "macos")]
        let (with_sleep, awake) = (libc::CLOCK_MONOTONIC_RAW, libc::CLOCK_UPTIME_RAW);
        let c = SystemClock;
        let (s, d) = (c.with_sleep_ms(), direct(with_sleep));
        assert!(
            s.abs_diff(d) < 50,
            "with_sleep_ms {s}, the clock that counts suspend {d}"
        );
        let (a, d) = (c.awake_ms(), direct(awake));
        assert!(
            a.abs_diff(d) < 50,
            "awake_ms {a}, the clock that does not {d}"
        );
    }

    /// The same on Windows, on the holder's pair: the tick count since boot (suspend
    /// included) and the unbiased interrupt time (100 ns units, suspend excluded).
    #[cfg(windows)]
    #[test]
    fn the_system_clock_reads_the_named_clocks() {
        use windows_sys::Win32::System::SystemInformation::GetTickCount64;
        use windows_sys::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime;
        let mut awake = 0u64;
        // SAFETY: no arguments; then one u64 written.
        let with_sleep = unsafe { GetTickCount64() };
        unsafe { QueryUnbiasedInterruptTime(&mut awake) };
        let c = SystemClock;
        let s = c.with_sleep_ms();
        assert!(
            s.abs_diff(with_sleep) < 50,
            "with_sleep_ms {s}, the tick count {with_sleep}"
        );
        let (a, d) = (c.awake_ms(), awake / 10_000);
        assert!(
            a.abs_diff(d) < 50,
            "awake_ms {a}, the unbiased interrupt time {d}"
        );
    }

    /// What a direct read cannot tell apart on a machine that has not slept since it booted —
    /// the clock that counts suspend from the one that does not — is pinned in the source:
    /// each reader of `sleep_clocks` reads the clock its name says, on every OS. It reads text,
    /// so it runs on every OS, Windows' readers included (the final verifier's finding 2). The
    /// needles are split with `concat!`, so this test's own text is never one of them.
    #[test]
    fn each_sleep_clock_reader_reads_the_clock_it_names() {
        fn bodies<'a>(src: &'a str, signature: &str) -> Vec<&'a str> {
            src.match_indices(signature)
                .map(|(at, _)| {
                    let rest = &src[at..];
                    &rest[..rest.find("\n    }\n").unwrap_or(rest.len())]
                })
                .collect()
        }
        let src = include_str!("mod.rs");
        let with_sleep = bodies(src, concat!("pub(super) fn with_", "sleep_ms() -> u64 {"));
        let awake = bodies(src, concat!("pub(super) fn awake", "_ms() -> u64 {"));
        assert_eq!(
            (with_sleep.len(), awake.len()),
            (2, 2),
            "one reader each, Unix and Windows"
        );
        let counts = concat!("read(WITH", "_SLEEP)");
        let counts_windows = concat!("GetTick", "Count64()");
        let stands = concat!("read(AW", "AKE)");
        let stands_windows = concat!("QueryUnbiasedInterrupt", "Time(&mut t)");
        for body in &with_sleep {
            assert!(
                body.contains(counts) || body.contains(counts_windows),
                "{body}"
            );
            assert!(
                !body.contains(stands) && !body.contains(stands_windows),
                "{body}"
            );
        }
        for body in &awake {
            assert!(
                body.contains(stands) || body.contains(stands_windows),
                "{body}"
            );
            assert!(
                !body.contains(counts) && !body.contains(counts_windows),
                "{body}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_sleep_clocks_are_the_ones_that_count_suspend() {
        assert_eq!(
            (super::sleep_clocks::WITH_SLEEP, super::sleep_clocks::AWAKE),
            (libc::CLOCK_BOOTTIME, libc::CLOCK_MONOTONIC)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_sleep_clocks_are_the_ones_that_count_suspend() {
        assert_eq!(
            (super::sleep_clocks::WITH_SLEEP, super::sleep_clocks::AWAKE),
            (libc::CLOCK_MONOTONIC_RAW, libc::CLOCK_UPTIME_RAW)
        );
    }

    /// A test's sleep moves the wall clock and the clock that counts suspend, never the
    /// monotonic or the awake one; a wall step moves the wall alone. A gap over 5 s is a sleep.
    #[test]
    fn a_manual_sleep_moves_only_the_clocks_that_count_it() {
        let clock = ManualClock::at(T0);
        let before = ClockPair::now(&clock);
        clock.set_wall(T0 + DAY);
        assert!(!ClockPair::now(&clock).slept_since(before));
        clock.sleep(SLEEP_GAP_MS);
        assert!(!ClockPair::now(&clock).slept_since(before));
        clock.sleep(1);
        assert!(ClockPair::now(&clock).slept_since(before));
        assert_eq!(clock.monotonic_ms(), ManualClock::at(T0).monotonic_ms());
    }
}
