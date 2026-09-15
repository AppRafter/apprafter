// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! One way to print a moment in time.
//!
//! Kubernetes and this CLI both store timestamps as RFC3339, which is the
//! right thing to store and the wrong thing to show: `2026-05-24T14:30:12Z`
//! makes a reader do two jobs at once — parse a machine format, and subtract
//! it from now to find out whether it matters. Nearly every question an
//! operator asks about a timestamp is the second one ("is this recent?"),
//! and the answer is the part the raw form hides.
//!
//! So the shipped rendering is absolute-plus-relative —
//! `2026-05-24 14:30 UTC (2 hours ago)` — with the absolute half kept
//! because the relative half is useless for an audit trail, and a zone
//! marker kept because a bare wall-clock time is ambiguous.
//!
//! This module exists because the rendering was reached by half the
//! surfaces that needed it and re-derived nowhere: several commands printed
//! the raw string straight from the object. Two adjacent columns in one
//! table disagreeing about what a time looks like is the kind of thing a
//! reader notices and cannot explain.

use chrono::{DateTime, Utc};

/// Render an RFC3339 timestamp as `2026-05-24 14:30 UTC (2 hours ago)`.
///
/// `now` is a parameter rather than a call to [`Utc::now`] so the output is
/// a pure function of its inputs and every case below is assertable without
/// a clock.
///
/// **An unparseable input is returned verbatim.** A timestamp this cannot
/// read is still information the reader may need — very often it is the
/// thing that explains whatever they are debugging — and dropping it, or
/// replacing it with a placeholder, destroys it. The only cost of a parse
/// failure is the missing relative suffix.
pub fn format_timestamp_with_relative(raw: &str, now: DateTime<Utc>) -> String {
    let Ok(parsed) = DateTime::parse_from_rfc3339(raw) else {
        return raw.to_string();
    };
    let utc = parsed.with_timezone(&Utc);
    let absolute = utc.format("%Y-%m-%d %H:%M UTC");
    let delta = now.signed_duration_since(utc);
    let relative = humanise_relative(delta);
    format!("{absolute} ({relative})")
}

/// The absolute half alone — `2026-05-24 14:30 UTC` — for a table column
/// where the relative suffix would not fit.
///
/// Same verbatim-passthrough rule as
/// [`format_timestamp_with_relative`], for the same reason.
pub fn format_timestamp(raw: &str) -> String {
    let Ok(parsed) = DateTime::parse_from_rfc3339(raw) else {
        return raw.to_string();
    };
    parsed
        .with_timezone(&Utc)
        .format("%Y-%m-%d %H:%M UTC")
        .to_string()
}

/// Render a signed duration as a short English phrase: `just now`,
/// `2 minutes ago`, `3 hours ago`, `5 days ago`, `in 3 minutes`.
///
/// Granularity matches what an operator acts on. Sub-minute precision is
/// noise on a platform event, and the rounding is deliberate: `90 seconds`
/// reads as `2 minutes ago`, not `1 minute ago`, because the reader is
/// placing the event, not measuring it.
pub fn humanise_relative(delta: chrono::Duration) -> String {
    let secs = delta.num_seconds();
    let abs = secs.unsigned_abs();
    let in_past = secs >= 0;

    let (unit, value) = if abs < 45 {
        return if in_past {
            "just now".to_string()
        } else {
            "in a few seconds".to_string()
        };
    } else if abs < 90 {
        ("minute", 1u64)
    } else if abs < 60 * 60 {
        ("minute", (abs as f64 / 60.0).round() as u64)
    } else if abs < 60 * 60 * 2 {
        ("hour", 1u64)
    } else if abs < 60 * 60 * 24 {
        ("hour", (abs as f64 / 3600.0).round() as u64)
    } else if abs < 60 * 60 * 24 * 2 {
        ("day", 1u64)
    } else if abs < 60 * 60 * 24 * 30 {
        ("day", (abs as f64 / 86_400.0).round() as u64)
    } else if abs < 60 * 60 * 24 * 60 {
        ("month", 1u64)
    } else if abs < 60 * 60 * 24 * 365 {
        ("month", (abs as f64 / 2_592_000.0).round() as u64)
    } else if abs < 60 * 60 * 24 * 365 * 2 {
        ("year", 1u64)
    } else {
        ("year", (abs as f64 / 31_536_000.0).round() as u64)
    };

    let plural = if value == 1 { "" } else { "s" };
    if in_past {
        format!("{value} {unit}{plural} ago")
    } else {
        format!("in {value} {unit}{plural}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-24T16:30:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn it_renders_the_absolute_moment_and_how_long_ago_it_was() {
        assert_eq!(
            format_timestamp_with_relative("2026-05-24T13:30:00Z", now()),
            "2026-05-24 13:30 UTC (3 hours ago)"
        );
    }

    #[test]
    fn a_non_utc_offset_is_normalised_rather_than_shown_as_written() {
        // The same instant written in two zones must render identically —
        // otherwise two objects stamped by different writers look like
        // different times.
        let z = format_timestamp_with_relative("2026-05-24T14:30:00Z", now());
        let offset = format_timestamp_with_relative("2026-05-24T16:30:00+02:00", now());
        assert_eq!(z, offset);
        assert!(z.contains("14:30 UTC"), "{z}");
    }

    #[test]
    fn an_unreadable_timestamp_survives_verbatim() {
        // Losing it would delete the only evidence of whatever wrote it.
        for raw in ["", "not-a-time", "2026-13-45", "0"] {
            assert_eq!(format_timestamp_with_relative(raw, now()), raw);
            assert_eq!(format_timestamp(raw), raw);
        }
    }

    #[test]
    fn the_absolute_only_form_matches_the_prefix_of_the_full_one() {
        // Two renderings of one moment in adjacent columns must agree on
        // the part they share.
        let raw = "2026-05-24T14:30:12Z";
        let full = format_timestamp_with_relative(raw, now());
        let short = format_timestamp(raw);
        assert!(
            full.starts_with(&short),
            "{short} is not a prefix of {full}"
        );
    }

    #[test]
    fn it_reads_the_future_as_the_future() {
        // `changedAt` on a cluster whose clock is ahead lands here, and
        // "in 5 minutes ago" would be the giveaway.
        let s = format_timestamp_with_relative("2026-05-24T16:35:00Z", now());
        assert!(s.contains("(in 5 minutes)"), "{s}");
        assert!(!s.contains("ago"), "{s}");
    }

    #[test]
    fn the_boundaries_round_the_way_a_reader_places_an_event() {
        // These pin the SHIPPED phrasing, moved here verbatim. Each unit
        // has a flat "1 <unit>" band covering its whole first span before
        // rounding starts, so the largest readings in a band understate:
        // 119 minutes is "1 hour ago" and 59 days is "1 month ago". That
        // is a deliberate floor, not an accident of the arithmetic — the
        // phrase places an event, it does not measure one — and changing
        // it is a product decision, not a tidy-up.
        let cases = [
            (0i64, "just now"),
            (44, "just now"),
            (45, "1 minute ago"),
            (90, "2 minutes ago"),
            (3_599, "60 minutes ago"),
            (3_600, "1 hour ago"),
            (7_188, "1 hour ago"),
            (7_200, "2 hours ago"),
            (86_400, "1 day ago"),
            (86_400 * 45, "1 month ago"),
            (86_400 * 90, "3 months ago"),
            (86_400 * 400, "1 year ago"),
            (86_400 * 900, "2 years ago"),
        ];
        for (secs, want) in cases {
            assert_eq!(
                humanise_relative(chrono::Duration::seconds(secs)),
                want,
                "at {secs}s"
            );
        }
    }

    #[test]
    fn one_of_anything_is_singular_and_everything_else_is_not() {
        assert_eq!(
            humanise_relative(chrono::Duration::seconds(3600)),
            "1 hour ago"
        );
        assert_eq!(
            humanise_relative(chrono::Duration::seconds(7200)),
            "2 hours ago"
        );
        assert_eq!(
            humanise_relative(chrono::Duration::seconds(-3600)),
            "in 1 hour"
        );
    }
}
