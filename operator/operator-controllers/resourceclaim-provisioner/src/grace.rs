// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure grace-period clock helpers for the RetainedClaim GC (2.4f).
//!
//! The 7-day retention timer is un-e2e-able (a test cannot wait a
//! week), so the grace math is factored into pure functions that take
//! the current instant as an INJECTED `now: DateTime<Utc>` parameter —
//! mirroring the `operator-core::leader` clock style (`acquire_or_renew`
//! takes `now`; `is_lease_stale` takes `now`). Production passes
//! `Utc::now()`; the unit tests below pass FIXED instants, so there is
//! no real wait and no env-gated silent skip.

use std::time::Duration;

use chrono::{DateTime, Utc};

/// 7-day retention grace before a deleted claim's role + database +
/// password Secret are GC'd. A configurable PlatformStack knob is
/// deferred to UX polish.
pub const GRACE_PERIOD: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// `retainUntil = deletion + grace`. Pure. Falls back to a literal
/// 7-day chrono duration if the `std::time::Duration` is somehow out of
/// `chrono::Duration`'s representable range (it never is for our 7-day
/// const, but `from_std` is fallible).
pub fn compute_retain_until(deletion: DateTime<Utc>, grace: Duration) -> DateTime<Utc> {
    deletion + chrono::Duration::from_std(grace).unwrap_or_else(|_| chrono::Duration::days(7))
}

/// Parse `spec.retainUntil` (RFC3339; tolerates a `Z` or `+00:00`
/// offset) into a UTC instant. The GC reconcile treats an `Err` as a
/// malformed snapshot — it logs + requeues rather than panicking.
pub fn parse_retain_until(s: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(s).map(|d| d.with_timezone(&Utc))
}

/// True iff `now >= retain_until`. Pure — drives the GC decision.
pub fn should_gc(retain_until: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now >= retain_until
}

/// Remaining grace, clamped to `>= 0` (`ZERO` if already expired).
/// Drives the requeue interval for a not-yet-expired RetainedClaim.
pub fn remaining_grace(retain_until: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    (retain_until - now).to_std().unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn grace_period_is_seven_days() {
        assert_eq!(GRACE_PERIOD, Duration::from_secs(7 * 24 * 60 * 60));
    }

    #[test]
    fn compute_retain_until_adds_seven_days_to_deletion() {
        let deletion = at("2026-06-03T00:00:00Z");
        let retain_until = compute_retain_until(deletion, GRACE_PERIOD);
        assert_eq!(retain_until, at("2026-06-10T00:00:00Z"));
    }

    #[test]
    fn should_gc_true_when_now_at_or_past_retain_until() {
        let retain_until = at("2026-06-10T00:00:00Z");
        // exactly at the boundary
        assert!(should_gc(retain_until, at("2026-06-10T00:00:00Z")));
        // past the boundary
        assert!(should_gc(retain_until, at("2026-06-11T12:00:00Z")));
    }

    #[test]
    fn should_gc_false_when_now_before_retain_until() {
        let retain_until = at("2026-06-10T00:00:00Z");
        assert!(!should_gc(retain_until, at("2026-06-09T23:59:59Z")));
    }

    #[test]
    fn remaining_grace_is_zero_when_expired() {
        let retain_until = at("2026-06-10T00:00:00Z");
        // now past retain_until → clamped to ZERO (not negative).
        assert_eq!(
            remaining_grace(retain_until, at("2026-06-11T00:00:00Z")),
            Duration::ZERO
        );
        // exactly at the boundary → ZERO.
        assert_eq!(
            remaining_grace(retain_until, at("2026-06-10T00:00:00Z")),
            Duration::ZERO
        );
    }

    #[test]
    fn remaining_grace_reports_the_correct_future_interval() {
        let retain_until = at("2026-06-10T00:00:00Z");
        // 1 day + 2 hours before retain_until.
        let now = at("2026-06-08T22:00:00Z");
        assert_eq!(
            remaining_grace(retain_until, now),
            Duration::from_secs(26 * 60 * 60)
        );
        assert!(!should_gc(retain_until, now));
    }

    #[test]
    fn parse_retain_until_accepts_z_and_offset_forms() {
        assert_eq!(
            parse_retain_until("2026-06-10T00:00:00Z").unwrap(),
            at("2026-06-10T00:00:00Z")
        );
        assert_eq!(
            parse_retain_until("2026-06-10T00:00:00+00:00").unwrap(),
            at("2026-06-10T00:00:00Z")
        );
    }

    #[test]
    fn parse_retain_until_errors_on_malformed_input() {
        // A malformed retainUntil must surface as Err so the GC
        // reconcile logs + requeues rather than panicking.
        assert!(parse_retain_until("not-a-timestamp").is_err());
        assert!(parse_retain_until("").is_err());
        assert!(parse_retain_until("2026-06-10").is_err());
    }
}
