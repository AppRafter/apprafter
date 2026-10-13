// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The unlock window (ADR 0068 §4; D.4a T56, O3, O4, holder §5). N minutes after the last served
//! item — a `Status` look or a refusal restarts nothing — and over at once when the machine
//! slept: the holder reads a clock that counts suspend and one that does not, and a gap of more
//! than [`SLEEP_GAP`] between two looks is a sleep. Both are read at every look and every wake,
//! so load cannot fake the gap. `std::time::Instant` counts no suspend on Linux or macOS, so the
//! window never uses it (HG1).

use std::time::Duration;

/// The two clocks are read one after the other, microseconds apart when the machine did not
/// sleep; more than this is a sleep (O4).
pub const SLEEP_GAP: Duration = Duration::from_secs(5);

/// What the window reads time from: [`SystemClock`] in a holder, a fake in tests.
pub trait HolderClock {
    /// Time since boot, suspend included (Linux `CLOCK_BOOTTIME`, macOS `CLOCK_MONOTONIC`,
    /// Windows `GetTickCount64`).
    fn with_sleep(&self) -> Duration;
    /// Time since boot, suspend excluded (Linux `CLOCK_MONOTONIC`, macOS `CLOCK_UPTIME_RAW`,
    /// Windows `QueryUnbiasedInterruptTime`).
    fn awake_only(&self) -> Duration;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    Open { left: Duration },
    Expired,
    Slept,
}

#[derive(Debug, Clone)]
pub struct Window {
    length: Duration,
    last_use: Duration,
    last_look: (Duration, Duration),
}

impl Window {
    /// A window of `length`, open from now.
    pub fn open(length: Duration, clock: &impl HolderClock) -> Window {
        let now = (clock.with_sleep(), clock.awake_only());
        Window {
            length,
            last_use: now.0,
            last_look: now,
        }
    }

    /// `minutes`, or a test's `secs` (T58: release CLIs never send seconds).
    pub fn length(minutes: u32, test_secs: Option<u32>) -> Duration {
        match test_secs {
            Some(secs) => Duration::from_secs(u64::from(secs)),
            None => Duration::from_secs(u64::from(minutes) * 60),
        }
    }

    /// Where the window stands now; every look also measures the sleep since the previous one.
    pub fn check(&mut self, clock: &impl HolderClock) -> WindowState {
        let now = (clock.with_sleep(), clock.awake_only());
        let passed = now.0.saturating_sub(self.last_look.0);
        let awake = now.1.saturating_sub(self.last_look.1);
        self.last_look = now;
        if passed.saturating_sub(awake) > SLEEP_GAP {
            return WindowState::Slept;
        }
        let used = now.0.saturating_sub(self.last_use);
        if used >= self.length {
            WindowState::Expired
        } else {
            WindowState::Open {
                left: self.length - used,
            }
        }
    }

    /// A served item restarts the window.
    pub fn touch(&mut self, clock: &impl HolderClock) {
        self.last_use = clock.with_sleep();
    }
}

/// The running OS's two clocks (`os::current`).
pub struct SystemClock;

impl HolderClock for SystemClock {
    fn with_sleep(&self) -> Duration {
        super::os::current::clock_with_sleep()
    }
    fn awake_only(&self) -> Duration {
        super::os::current::clock_awake()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// Two clocks the test moves: one that counts sleep, one that does not.
    struct Fake {
        with_sleep: Cell<Duration>,
        awake: Cell<Duration>,
    }

    impl Fake {
        fn new() -> Fake {
            Fake {
                with_sleep: Cell::new(Duration::from_secs(1000)),
                awake: Cell::new(Duration::from_secs(500)),
            }
        }
        /// Time passes awake.
        fn pass(&self, d: Duration) {
            self.with_sleep.set(self.with_sleep.get() + d);
            self.awake.set(self.awake.get() + d);
        }
        /// The machine sleeps.
        fn sleep(&self, d: Duration) {
            self.with_sleep.set(self.with_sleep.get() + d);
        }
    }

    impl HolderClock for Fake {
        fn with_sleep(&self) -> Duration {
            self.with_sleep.get()
        }
        fn awake_only(&self) -> Duration {
            self.awake.get()
        }
    }

    const S: Duration = Duration::from_secs(1);

    /// T56: a window lasts N minutes after its last served item, to the second.
    #[test]
    fn a_window_expires_exactly_n_minutes_after_its_last_use() {
        let c = Fake::new();
        let mut w = Window::open(Window::length(5, None), &c);
        c.pass(299 * S);
        assert_eq!(w.check(&c), WindowState::Open { left: S });
        c.pass(S);
        assert_eq!(w.check(&c), WindowState::Expired);
    }

    /// T56: only a served item restarts it; a look (`check`, which `Status` makes) does not.
    #[test]
    fn a_served_item_restarts_the_window_and_a_look_does_not() {
        let c = Fake::new();
        let mut w = Window::open(Window::length(0, Some(10)), &c);
        c.pass(8 * S);
        assert_eq!(w.check(&c), WindowState::Open { left: 2 * S });
        w.touch(&c);
        c.pass(8 * S);
        assert_eq!(w.check(&c), WindowState::Open { left: 2 * S });
        c.pass(2 * S);
        assert_eq!(w.check(&c), WindowState::Expired);
    }

    /// O4: a gap of more than 5 s between the two clocks since the last look means the machine
    /// slept: the window ends whatever time it had left. 5 s is not yet a sleep.
    #[test]
    fn sleep_longer_than_five_seconds_ends_the_window() {
        let c = Fake::new();
        let mut w = Window::open(Window::length(60, None), &c);
        c.sleep(SLEEP_GAP);
        assert!(
            matches!(w.check(&c), WindowState::Open { .. }),
            "exactly 5 s"
        );
        c.sleep(SLEEP_GAP + Duration::from_millis(1));
        assert_eq!(w.check(&c), WindowState::Slept);
    }

    /// Each look measures the gap since the previous one: two short naps never add up.
    #[test]
    fn the_sleep_gap_is_measured_between_two_looks() {
        let c = Fake::new();
        let mut w = Window::open(Window::length(60, None), &c);
        for _ in 0..3 {
            c.sleep(3 * S);
            c.pass(S);
            assert!(matches!(w.check(&c), WindowState::Open { .. }));
        }
    }

    /// O3: the CLI caps a window at 1440 minutes; the length is minutes, or a test's seconds.
    #[test]
    fn the_length_is_minutes_or_a_tests_seconds() {
        assert_eq!(Window::length(1440, None), Duration::from_secs(86_400));
        assert_eq!(Window::length(5, Some(3)), Duration::from_secs(3));
    }

    /// T56, HG1: a holder's clocks — one that counts suspend, one that does not — both advance
    /// as time passes awake, and the first is never behind the second. That they count
    /// suspend as named is what the owner's suspend walk row proves (h); the constants are
    /// pinned in `os::*`'s own tests.
    #[test]
    fn the_system_clocks_advance_together_while_awake() {
        let c = SystemClock;
        let (s0, a0) = (c.with_sleep(), c.awake_only());
        std::thread::sleep(Duration::from_millis(50));
        let (s1, a1) = (c.with_sleep(), c.awake_only());
        assert!(s1 > s0 && a1 > a0);
        // Suspend only adds to the first clock; 100 ms covers Windows' tick granularity.
        assert!(s1 + Duration::from_millis(100) >= a1, "{s1:?} {a1:?}");
        let gap = (s1 - s0).abs_diff(a1 - a0);
        assert!(gap < SLEEP_GAP, "{gap:?}");
    }
}
