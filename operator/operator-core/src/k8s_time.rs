// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The one boundary between the clock the operator computes with and the
//! clock Kubernetes' typed objects carry.
//!
//! The operator does its time arithmetic in `chrono` (lease staleness, grace
//! windows, status/annotation timestamps it formats itself). Since
//! k8s-openapi 0.27 / kube 3.0, `metav1::Time` and `metav1::MicroTime` wrap a
//! `jiff::Timestamp` instead of a `chrono::DateTime<Utc>`. Rather than port
//! every calculation to a second time library, the conversion happens HERE
//! and only here — at the moment a value goes into, or comes out of, a
//! Kubernetes type.
//!
//! What these conversions must never do is change a byte on the wire. The
//! apiserver parses `MicroTime` with Go's `RFC3339Micro`
//! (`2006-01-02T15:04:05.000000Z07:00`), whose fixed `.000000` rejects a
//! value without exactly six fractional digits, so a formatter that dropped
//! zero fractions would turn every Lease renewal that lands on a whole second
//! into a 400. The tests below pin the exact strings k8s-openapi 0.23 wrote
//! (chrono's `to_rfc3339_opts(Secs | Micros, true)`) against what the jiff
//! types write now.

use chrono::{DateTime, Utc};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, Time};
use k8s_openapi::jiff::Timestamp;

/// `chrono` → `jiff`, nanosecond-exact.
///
/// jiff's range is `-009999-01-02T01:59:59Z ..= 9999-12-30T22:00:00.999999999Z`
/// (a year of ±9999 either side of every UTC offset); chrono's is wider. A
/// value outside it saturates to the nearest end rather than panicking. Only
/// the last 26 hours of year 9999 are lost that way, and Go's `time.Time`
/// cannot marshal a year past 9999 at all, so nothing the apiserver would
/// accept is affected. Leap-second nanos (chrono encodes 23:59:60
/// as nanos >= 1e9) clamp to the last nanosecond of that second.
pub fn to_timestamp(t: DateTime<Utc>) -> Timestamp {
    let secs = t.timestamp();
    let nanos = i32::try_from(t.timestamp_subsec_nanos().min(999_999_999))
        .expect("clamped below 1e9, fits i32");
    Timestamp::new(secs, nanos).unwrap_or(if secs < 0 {
        Timestamp::MIN
    } else {
        Timestamp::MAX
    })
}

/// `jiff` → `chrono`, nanosecond-exact. Total: chrono's range covers jiff's.
pub fn from_timestamp(t: Timestamp) -> DateTime<Utc> {
    // jiff reports a pre-epoch instant as (seconds rounded toward zero,
    // NEGATIVE sub-second nanos); chrono wants non-negative nanos below 1e9.
    let (mut secs, mut nanos) = (t.as_second(), t.subsec_nanosecond());
    if nanos < 0 {
        secs -= 1;
        nanos += 1_000_000_000;
    }
    let nanos = u32::try_from(nanos).expect("normalised to 0..1e9");
    DateTime::from_timestamp(secs, nanos).expect("jiff's range is inside chrono's")
}

/// A `metav1.Time` (whole seconds on the wire) for `t`.
pub fn time(t: DateTime<Utc>) -> Time {
    Time(to_timestamp(t))
}

/// A `metav1.MicroTime` (six fractional digits on the wire) for `t`.
pub fn micro_time(t: DateTime<Utc>) -> MicroTime {
    MicroTime(to_timestamp(t))
}

/// The instant a `metav1.Time` carries.
pub fn from_time(t: &Time) -> DateTime<Utc> {
    from_timestamp(t.0)
}

/// The instant a `metav1.MicroTime` carries.
pub fn from_micro_time(t: &MicroTime) -> DateTime<Utc> {
    from_timestamp(t.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{SecondsFormat, TimeZone};
    use serde_json::{json, Value};

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("test instant is RFC 3339")
            .with_timezone(&Utc)
    }

    /// The exact JSON string a value serializes to.
    fn wire<T: serde::Serialize>(v: &T) -> String {
        match serde_json::to_value(v).expect("serializes") {
            Value::String(s) => s,
            other => panic!("expected a JSON string, got {other}"),
        }
    }

    // -----------------------------------------------------------------
    // Pinned wire strings. These are the literal bytes the apiserver sees.
    // -----------------------------------------------------------------

    #[test]
    fn time_writes_whole_seconds_truncating_the_fraction() {
        // metav1.Time is RFC 3339 at second precision; the fraction is
        // TRUNCATED (.999999999 does not round up into the next second).
        let t = utc("2026-09-22T17:53:31.999999999Z");
        assert_eq!(wire(&time(t)), "2026-09-22T17:53:31Z");
    }

    #[test]
    fn micro_time_writes_exactly_six_fractional_digits_truncating() {
        let t = utc("2026-09-22T17:53:31.123456789Z");
        assert_eq!(wire(&micro_time(t)), "2026-09-22T17:53:31.123456Z");
    }

    #[test]
    fn a_whole_second_micro_time_still_writes_six_zero_digits() {
        // The case a "%.f"-style formatter gets wrong: it would write
        // "…:31Z", which the apiserver's RFC3339Micro parse rejects. A Lease
        // renewal lands on a whole second whenever the clock does.
        let t = utc("2026-09-22T17:53:31Z");
        assert_eq!(wire(&micro_time(t)), "2026-09-22T17:53:31.000000Z");
        let t = utc("2026-09-22T17:53:31.000000999Z");
        assert_eq!(wire(&micro_time(t)), "2026-09-22T17:53:31.000000Z");
    }

    #[test]
    fn non_utc_inputs_are_written_in_utc_with_a_z_suffix() {
        let t = DateTime::parse_from_rfc3339("2026-09-22T20:53:31.5+03:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(wire(&time(t)), "2026-09-22T17:53:31Z");
        assert_eq!(wire(&micro_time(t)), "2026-09-22T17:53:31.500000Z");
    }

    /// Byte-identity with what k8s-openapi 0.23 wrote, over a sweep of
    /// instants rather than a handful: 0.23 serialized `Time` as chrono's
    /// `to_rfc3339_opts(SecondsFormat::Secs, true)` and `MicroTime` as
    /// `to_rfc3339_opts(SecondsFormat::Micros, true)`. Any instant where the
    /// jiff-backed types disagree is a changed byte on the wire.
    #[test]
    fn every_instant_serializes_byte_identically_to_the_chrono_era_types() {
        // A deterministic LCG walk over 1970..2200 with arbitrary nanos,
        // plus the edges a formatter tends to get wrong.
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut instants = vec![
            utc("1970-01-01T00:00:00Z"),
            utc("1999-12-31T23:59:59.999999999Z"),
            utc("2000-02-29T00:00:00.000001Z"),
            utc("2038-01-19T03:14:07.999999Z"),
            utc("2038-01-19T03:14:08Z"),
            utc("2262-04-11T23:47:16.854775807Z"), // i64-nanosecond ceiling
            utc("9999-12-30T22:00:00.999999999Z"), // jiff's ceiling
        ];
        for _ in 0..5000 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let secs = (seed >> 11) % 7_258_118_400; // 1970 .. 2200
            let nanos = (seed % 1_000_000_000) as u32;
            // Every tenth instant is a whole second: the zero-fraction path.
            let nanos = if seed.is_multiple_of(10) { 0 } else { nanos };
            instants.push(Utc.timestamp_opt(secs as i64, nanos).unwrap());
        }
        for t in instants {
            assert_eq!(
                wire(&time(t)),
                t.to_rfc3339_opts(SecondsFormat::Secs, true),
                "metav1.Time wire bytes changed for {t:?}"
            );
            assert_eq!(
                wire(&micro_time(t)),
                t.to_rfc3339_opts(SecondsFormat::Micros, true),
                "metav1.MicroTime wire bytes changed for {t:?}"
            );
        }
    }

    // -----------------------------------------------------------------
    // Round trips: what the apiserver sends back reads as the same instant
    // and re-serializes to the same bytes.
    // -----------------------------------------------------------------

    #[test]
    fn an_apiserver_micro_time_round_trips_to_the_same_bytes() {
        let raw = json!("2026-09-22T17:53:31.123456Z");
        let mt: MicroTime = serde_json::from_value(raw.clone()).expect("apiserver MicroTime");
        assert_eq!(from_micro_time(&mt), utc("2026-09-22T17:53:31.123456Z"));
        assert_eq!(serde_json::to_value(&mt).unwrap(), raw);
        assert_eq!(
            serde_json::to_value(micro_time(from_micro_time(&mt))).unwrap(),
            raw
        );
    }

    #[test]
    fn an_apiserver_time_round_trips_to_the_same_bytes() {
        let raw = json!("2026-09-22T17:53:31Z");
        let t: Time = serde_json::from_value(raw.clone()).expect("apiserver Time");
        assert_eq!(from_time(&t), utc("2026-09-22T17:53:31Z"));
        assert_eq!(serde_json::to_value(&t).unwrap(), raw);
        assert_eq!(serde_json::to_value(time(from_time(&t))).unwrap(), raw);
    }

    #[test]
    fn chrono_to_jiff_and_back_is_nanosecond_exact() {
        for s in [
            "2026-09-22T17:53:31.123456789Z",
            "1970-01-01T00:00:00Z",
            "1969-12-31T23:59:59.5Z", // pre-epoch: jiff's negative sub-second nanos
            "1900-01-01T00:00:00.000000001Z",
        ] {
            let t = utc(s);
            assert_eq!(from_timestamp(to_timestamp(t)), t, "{s}");
        }
    }

    #[test]
    fn out_of_jiff_range_saturates_instead_of_panicking() {
        let far = Utc.with_ymd_and_hms(20_000, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(to_timestamp(far), Timestamp::MAX);
        let before = Utc.with_ymd_and_hms(-20_000, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(to_timestamp(before), Timestamp::MIN);
    }
}
