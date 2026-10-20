// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Times in fixtures: offsets from the caller's `now`, written as the cluster writes a time.

use std::ops::RangeInclusive;

use chrono::{DateTime, Duration, SecondsFormat, Utc};

/// The minutes a problem entry may have been last seen: inside the 20 minutes the CLI prints
/// as "now" (`PROBLEM_LIVE_SECS`), at least nine from its edge — so no golden prints a
/// minute-granular age (`format_pod_age`).
pub const MINUTES: RangeInclusive<u8> = 0..=11;
/// Whole hours that sit in the middle of their band, [n − 30 min, n + 30 min).
pub const HOURS: RangeInclusive<u8> = 3..=23;
/// Whole days that sit in the middle of their band, [n − 12 h, n + 12 h).
pub const DAYS: RangeInclusive<u8> = 3..=29;

/// How long before (or after) `now` a fixture's time lies. Each offset sits in the middle of
/// the band `cli_core::timefmt::humanise_relative` gives it, so a reading made up to
/// [`Ago::margin`] later says the same words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ago {
    /// A problem entry's `lastSeen`, `n` minutes, n in [`MINUTES`].
    Minutes(u8),
    /// The middle of the flat "1 hour" band, [1 h, 2 h): 1 h 30 min.
    HourAndAHalf,
    /// `n` whole hours, n in [`HOURS`].
    Hours(u8),
    /// `n` whole days, n in [`DAYS`].
    Days(u8),
}

impl Ago {
    /// The offset. Panics outside each variant's range: a fixture time on a band's edge would
    /// make a golden depend on how long its run took.
    pub fn duration(self) -> Duration {
        match self {
            Ago::Minutes(n) => {
                assert!(
                    MINUTES.contains(&n),
                    "a problem minute stays inside the CLI's 20-minute now window: {n}"
                );
                Duration::minutes(i64::from(n))
            }
            Ago::HourAndAHalf => Duration::minutes(90),
            Ago::Hours(n) => {
                assert!(
                    HOURS.contains(&n),
                    "whole hours {HOURS:?} sit mid-band: {n}"
                );
                Duration::hours(i64::from(n))
            }
            Ago::Days(n) => {
                assert!(DAYS.contains(&n), "whole days {DAYS:?} sit mid-band: {n}");
                Duration::days(i64::from(n))
            }
        }
    }

    /// How much later a reading may be made and still say the same words.
    pub fn margin(self) -> Duration {
        match self {
            Ago::Minutes(_) => Duration::minutes(9),
            Ago::HourAndAHalf | Ago::Hours(_) => Duration::minutes(25),
            Ago::Days(_) => Duration::hours(11),
        }
    }

    /// `now` minus the offset, as the cluster writes it.
    pub fn before(self, now: DateTime<Utc>) -> String {
        stamp(now - self.duration())
    }

    /// `now` plus the offset: a clock ahead of this one, or the re-check's answer (B76).
    pub fn after(self, now: DateTime<Utc>) -> String {
        stamp(now + self.duration())
    }
}

/// A moment as the cluster writes it: `2026-10-16T14:30:00Z`.
pub fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cli_core::timefmt::humanise_relative;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("a test time")
            .with_timezone(&Utc)
    }

    /// RFC 3339, whole seconds, `Z`: the shape the apiserver writes and the CLI parses.
    #[test]
    fn times_are_written_as_the_cluster_writes_them() {
        let now = at("2026-10-16T12:00:00Z");
        assert_eq!(Ago::Hours(3).before(now), "2026-10-16T09:00:00Z");
        assert_eq!(Ago::Days(3).before(now), "2026-10-13T12:00:00Z");
        assert_eq!(Ago::Minutes(7).before(now), "2026-10-16T11:53:00Z");
        assert_eq!(Ago::HourAndAHalf.after(now), "2026-10-16T13:30:00Z");
        assert_eq!(stamp(now), "2026-10-16T12:00:00Z");
    }

    /// B55, checked without a clock: every offset in its range reads the same words when
    /// read up to its margin later — and so does one written ahead of the clock (B76's
    /// re-check time), whose distance shrinks as time passes.
    #[test]
    fn each_offset_reads_the_same_until_its_margin() {
        let mut offsets = vec![Ago::HourAndAHalf];
        offsets.extend(HOURS.map(Ago::Hours));
        offsets.extend(DAYS.map(Ago::Days));
        for ago in offsets {
            let d = ago.duration();
            for later in [Duration::zero(), Duration::seconds(1), ago.margin()] {
                assert_eq!(
                    humanise_relative(d + later),
                    humanise_relative(d),
                    "{ago:?}, read {later} later"
                );
                assert_eq!(
                    humanise_relative(-(d - later)),
                    humanise_relative(-d),
                    "{ago:?} ahead of the clock, read {later} later"
                );
            }
        }
    }

    /// How long the CLI prints a problem entry as "now": `PROBLEM_LIVE_SECS` in
    /// platform-cli's `commands/app.rs` (20 min, inclusive), which this crate cannot name.
    const NOW_WINDOW_MINUTES: i64 = 20;

    /// Critic round, finding 8: a problem minute's words are the CLI's "now" window, not
    /// `humanise_relative`'s bands, so its margin is checked against that window.
    #[test]
    fn each_problem_minute_reads_now_until_its_margin() {
        for n in MINUTES {
            let ago = Ago::Minutes(n);
            assert!(
                ago.duration() + ago.margin() <= Duration::minutes(NOW_WINDOW_MINUTES),
                "{ago:?} read {} later",
                ago.margin()
            );
        }
    }

    #[test]
    #[should_panic(expected = "now window")]
    fn a_problem_minute_outside_the_now_window_is_refused() {
        let _ = Ago::Minutes(12).duration();
    }

    #[test]
    #[should_panic(expected = "mid-band")]
    fn an_hour_on_a_band_edge_is_refused() {
        let _ = Ago::Hours(2).duration();
    }

    #[test]
    #[should_panic(expected = "mid-band")]
    fn a_day_on_a_band_edge_is_refused() {
        let _ = Ago::Days(30).duration();
    }
}
