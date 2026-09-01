// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Undesigned reconcile failures, kept on the object (2.22h / D16).
//!
//! # What this is for
//!
//! DESIGNED failures already surface well — `AwaitingResourceClaim`,
//! `EnvSecretMissing`, `ImageResolved`, `MigrationPending` all reach
//! `status.conditions` and are rendered by `apprafter app status`. What
//! vanished was the failure nobody planned for: a `?` into `error_policy`, or
//! a `warn!`-and-continue at a site that must not abort the reconcile.
//!
//! Those are exactly the ones worth reporting, and they cost this project four
//! incidents — the ADR 0048 anchor 403, the 0.2.31 MigrationPlan GC, D15's
//! plan-rejection loop, and the 2.22b claim-prune 403 that opened D16. Every
//! one was a repeating error visible only in `kubectl logs`.
//!
//! # Why a ledger rather than an Event
//!
//! `error_policy` is a synchronous `fn` returning `Action`. It cannot await,
//! so it cannot publish an Event or write status. The one thing it can do is
//! mutate in-memory state — which is what [`ProblemLedger`] is.
//!
//! It also has to be callable from INSIDE an async reconcile, because the
//! incident that opened D16 never reaches `error_policy` at all:
//! `prune_orphaned_claims` warns on a failed delete and returns `Ok(())`, as
//! ADR 0048 requires. A design that hooked only `error_policy` would have
//! shipped D16 without covering D16.
//!
//! # Two things this has to get right
//!
//! A recurring error must DEDUPLICATE rather than spam: a 30-second retry
//! loop writing status every tick is a write loop, not a signal, and this
//! repository has already shipped one of those (D19). And a failure that
//! stops must AGE OUT on its own — a surface listing what was once broken is
//! one people learn not to read, which is worse than not having it.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How many DISTINCT reasons are kept per object.
///
/// Keyed on `reason` alone, never `reason`+`message`: a 403 whose message
/// names a different claim each tick would otherwise overflow the cap with
/// one logical problem and evict the real ones.
pub const RECENT_PROBLEMS_CAP: usize = 5;

/// How many objects the in-memory ledger tracks before evicting the
/// least-recently-seen. An object that is deleted never flushes again, so
/// without this its entry would leak for the operator's lifetime.
pub const MAX_TRACKED_OBJECTS: usize = 512;

/// How stale a written `lastSeen` may get before a still-recurring problem
/// earns a refresh write.
///
/// The number that decides whether this feature is a signal or a write loop.
/// At 900s a permanently-failing object costs 1 write on first sight and 4
/// writes an hour thereafter, against 120 failures an hour.
pub const PROBLEM_REFRESH_FLOOR_SECS: i64 = 900;

/// How long after its last sighting a problem is dropped. Overridable at
/// startup so a walk can prove ageing without waiting an hour.
pub const DEFAULT_PROBLEM_TTL_SECS: i64 = 3600;

/// Longest message kept. The full text stays in the log; this is the part a
/// human reads in `app status`.
const MAX_MESSAGE_LEN: usize = 512;

/// One recent undesigned failure, as it appears on `status.recentProblems`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct RecentProblem {
    /// Low-cardinality classifier, and the deduplication key.
    pub reason: String,
    /// The most recent detail — truncated, never the whole log line.
    pub message: String,
    #[serde(rename = "firstSeen")]
    pub first_seen: String,
    #[serde(rename = "lastSeen")]
    pub last_seen: String,
    pub count: i64,
}

#[derive(Clone, Debug)]
struct Entry {
    message: String,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    count: i64,
}

/// In-memory record of what has gone wrong, per object.
///
/// A `std::sync::Mutex` rather than tokio's, because `error_policy` is sync
/// and cannot await a lock. The guard is NEVER held across an `await`: the
/// only async caller takes a [`ProblemLedger::snapshot`], which clones out and
/// drops the guard before any I/O.
#[derive(Default)]
pub struct ProblemLedger {
    inner: Mutex<HashMap<(String, String), BTreeMap<String, Entry>>>,
}

impl ProblemLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Note that `reason` happened to `ns/name`, now.
    ///
    /// Synchronous and allocation-only, so it is callable from `error_policy`
    /// as well as from a `warn!`-and-continue site inside a reconcile. Never
    /// fails: a poisoned lock is dropped silently, because a failure to record
    /// a failure must not become a second failure.
    pub fn record(&self, ns: &str, name: &str, reason: &str, message: &str, now: DateTime<Utc>) {
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        if map.len() >= MAX_TRACKED_OBJECTS
            && !map.contains_key(&(ns.to_string(), name.to_string()))
        {
            evict_oldest_object(&mut map);
        }
        let per_object = map.entry((ns.to_string(), name.to_string())).or_default();
        let message = truncate(message);
        match per_object.get_mut(reason) {
            Some(e) => {
                e.message = message;
                e.last_seen = now;
                e.count += 1;
            }
            None => {
                per_object.insert(
                    reason.to_string(),
                    Entry {
                        message,
                        first_seen: now,
                        last_seen: now,
                        count: 1,
                    },
                );
                if per_object.len() > RECENT_PROBLEMS_CAP {
                    evict_oldest_reason(per_object);
                }
            }
        }
    }

    /// What the ledger currently holds for `ns/name`, oldest sighting first.
    pub fn snapshot(&self, ns: &str, name: &str) -> Vec<RecentProblem> {
        let Ok(map) = self.inner.lock() else {
            return Vec::new();
        };
        let Some(per_object) = map.get(&(ns.to_string(), name.to_string())) else {
            return Vec::new();
        };
        let mut rows: Vec<RecentProblem> = per_object
            .iter()
            .map(|(reason, e)| RecentProblem {
                reason: reason.clone(),
                message: e.message.clone(),
                first_seen: e.first_seen.to_rfc3339(),
                last_seen: e.last_seen.to_rfc3339(),
                count: e.count,
            })
            .collect();
        rows.sort_by(|a, b| a.first_seen.cmp(&b.first_seen));
        rows
    }

    /// Forget an object entirely — used when it is deleted.
    pub fn evict(&self, ns: &str, name: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(&(ns.to_string(), name.to_string()));
        }
    }
}

fn truncate(s: &str) -> String {
    if s.len() <= MAX_MESSAGE_LEN {
        return s.to_string();
    }
    // Cut on a char boundary so a multi-byte message cannot panic here.
    let mut end = MAX_MESSAGE_LEN;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn evict_oldest_object(map: &mut HashMap<(String, String), BTreeMap<String, Entry>>) {
    let oldest = map
        .iter()
        .filter_map(|(k, v)| {
            v.values()
                .map(|e| e.last_seen)
                .max()
                .map(|t| (k.clone(), t))
        })
        .min_by_key(|(_, t)| *t)
        .map(|(k, _)| k);
    if let Some(k) = oldest {
        map.remove(&k);
    }
}

fn evict_oldest_reason(per_object: &mut BTreeMap<String, Entry>) {
    let oldest = per_object
        .iter()
        .min_by_key(|(_, e)| e.last_seen)
        .map(|(r, _)| r.clone());
    if let Some(r) = oldest {
        per_object.remove(&r);
    }
}

/// Drop problems whose last sighting is older than `ttl`.
///
/// Ages on `lastSeen`, never `firstSeen`: a failure still recurring two hours
/// later must still be listed. Pure, so the rule is testable without a clock.
pub fn age_out(rows: &[RecentProblem], ttl_secs: i64, now: DateTime<Utc>) -> Vec<RecentProblem> {
    let cutoff = now - Duration::seconds(ttl_secs);
    rows.iter()
        .filter(|r| {
            DateTime::parse_from_rfc3339(&r.last_seen)
                // An unparseable timestamp is kept rather than silently
                // dropped — losing a problem is the failure mode this whole
                // module exists to prevent.
                .map(|t| t.with_timezone(&Utc) > cutoff)
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

/// Whether the computed problem list is worth a status write.
///
/// Pure, and the reason it is pure is the same reason
/// `size_write_is_worth_it` is: getting this wrong is not a wrong value, it is
/// a **write loop**. Every status write bumps `resourceVersion` and wakes the
/// controller, which fails again, which writes again — and the Application
/// controller's watch has no debounce, so a self-triggering write runs at
/// round-trip speed rather than at the 30-second requeue.
///
/// Gating BEFORE the request rather than relying on the apiserver to notice an
/// identical body also means this does not depend on whether a re-sent SSA
/// apply rewrites `managedFields[].time`. No request, no bump, no self-wake.
///
/// Writes when:
///  1. a reason is NEW,
///  2. an existing reason's message CHANGED (the detail moved),
///  3. an existing reason's written `lastSeen` is older than
///     [`PROBLEM_REFRESH_FLOOR_SECS`] (so a live problem does not go on
///     reading as stale), or
///  4. something written is no longer computed (an age-out is due).
///
/// A healthy object costs ZERO writes forever: once both lists are empty,
/// none of the four clauses can fire, and nothing on the healthy path depends
/// on `now()`.
pub fn problems_write_is_worth_it(
    written: &[RecentProblem],
    computed: &[RecentProblem],
    now: DateTime<Utc>,
) -> bool {
    for c in computed {
        match written.iter().find(|w| w.reason == c.reason) {
            None => return true,
            Some(w) => {
                if w.message != c.message {
                    return true;
                }
                let stale = DateTime::parse_from_rfc3339(&w.last_seen)
                    .map(|t| {
                        (now - t.with_timezone(&Utc)).num_seconds() >= PROBLEM_REFRESH_FLOOR_SECS
                    })
                    .unwrap_or(true);
                if stale {
                    return true;
                }
            }
        }
    }
    // Clause 4: an entry was written that is no longer computed.
    written
        .iter()
        .any(|w| !computed.iter().any(|c| c.reason == w.reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn row(reason: &str, message: &str, first: &str, last: &str, count: i64) -> RecentProblem {
        RecentProblem {
            reason: reason.into(),
            message: message.into(),
            first_seen: first.into(),
            last_seen: last.into(),
            count,
        }
    }

    // --- the ledger ---

    #[test]
    fn a_repeating_failure_accumulates_into_one_entry() {
        // The whole point: a 30-second retry loop is ONE problem with a rising
        // count, not N problems.
        let l = ProblemLedger::new();
        for i in 0..5 {
            l.record(
                "demo",
                "web",
                "ClaimPruneFailed",
                "forbidden",
                t("2026-09-01T10:00:00Z") + Duration::seconds(i * 30),
            );
        }
        let rows = l.snapshot("demo", "web");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].count, 5);
        assert_eq!(rows[0].first_seen, t("2026-09-01T10:00:00Z").to_rfc3339());
        assert_eq!(rows[0].last_seen, t("2026-09-01T10:02:00Z").to_rfc3339());
    }

    #[test]
    fn a_changing_message_does_not_create_a_second_entry() {
        // A 403 naming a different claim each tick is one logical problem.
        // Keying on reason+message would overflow the cap with it and evict
        // the real problems.
        let l = ProblemLedger::new();
        let now = t("2026-09-01T10:00:00Z");
        l.record("demo", "web", "ClaimPruneFailed", "cannot delete a", now);
        l.record("demo", "web", "ClaimPruneFailed", "cannot delete b", now);
        let rows = l.snapshot("demo", "web");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].message, "cannot delete b",
            "keeps the latest detail"
        );
        assert_eq!(rows[0].count, 2);
    }

    #[test]
    fn the_per_object_cap_evicts_the_least_recent_reason() {
        let l = ProblemLedger::new();
        let base = t("2026-09-01T10:00:00Z");
        for i in 0..(RECENT_PROBLEMS_CAP as i64 + 2) {
            l.record(
                "demo",
                "web",
                &format!("R{i}"),
                "m",
                base + Duration::seconds(i),
            );
        }
        let rows = l.snapshot("demo", "web");
        assert_eq!(rows.len(), RECENT_PROBLEMS_CAP);
        assert!(!rows.iter().any(|r| r.reason == "R0"), "{rows:?}");
        assert!(rows.iter().any(|r| r.reason == "R6"), "{rows:?}");
    }

    #[test]
    fn objects_are_separate_and_evictable() {
        let l = ProblemLedger::new();
        let now = t("2026-09-01T10:00:00Z");
        l.record("demo", "web", "A", "m", now);
        l.record("demo", "api", "B", "m", now);
        assert_eq!(l.snapshot("demo", "web").len(), 1);
        assert_eq!(l.snapshot("demo", "api").len(), 1);
        l.evict("demo", "web");
        assert!(l.snapshot("demo", "web").is_empty());
        assert_eq!(l.snapshot("demo", "api").len(), 1, "eviction is per object");
    }

    #[test]
    fn a_long_message_is_truncated_on_a_char_boundary() {
        let l = ProblemLedger::new();
        // Multi-byte, so a naive byte slice would panic.
        let long = "é".repeat(400);
        l.record("demo", "web", "R", &long, t("2026-09-01T10:00:00Z"));
        let rows = l.snapshot("demo", "web");
        assert!(rows[0].message.chars().count() < 400);
        assert!(rows[0].message.ends_with('…'));
    }

    // --- the deadband ---

    #[test]
    fn a_novel_problem_earns_a_write() {
        let now = t("2026-09-01T10:00:00Z");
        let computed = vec![row(
            "A",
            "m",
            "2026-09-01T10:00:00+00:00",
            "2026-09-01T10:00:00+00:00",
            1,
        )];
        assert!(problems_write_is_worth_it(&[], &computed, now));
    }

    #[test]
    fn a_repeating_problem_does_not_write_every_tick() {
        // THE clause that decides whether this is a signal or a write loop.
        // Every status write wakes the controller, which fails again — and the
        // watch has no debounce, so it would spin at round-trip speed.
        let now = t("2026-09-01T10:05:00Z");
        let written = vec![row(
            "A",
            "m",
            "2026-09-01T10:00:00+00:00",
            "2026-09-01T10:00:00+00:00",
            1,
        )];
        let computed = vec![row(
            "A",
            "m",
            "2026-09-01T10:00:00+00:00",
            "2026-09-01T10:05:00+00:00",
            11,
        )];
        assert!(!problems_write_is_worth_it(&written, &computed, now));
    }

    #[test]
    fn a_live_problem_refreshes_before_it_reads_as_stale() {
        let written = vec![row(
            "A",
            "m",
            "2026-09-01T10:00:00+00:00",
            "2026-09-01T10:00:00+00:00",
            1,
        )];
        let computed = written.clone();
        // Just under the floor: still quiet.
        assert!(!problems_write_is_worth_it(
            &written,
            &computed,
            t("2026-09-01T10:00:00+00:00") + Duration::seconds(PROBLEM_REFRESH_FLOOR_SECS - 1)
        ));
        // At the floor: refresh, so the reader is not told a live failure is
        // fifteen minutes old.
        assert!(problems_write_is_worth_it(
            &written,
            &computed,
            t("2026-09-01T10:00:00+00:00") + Duration::seconds(PROBLEM_REFRESH_FLOOR_SECS)
        ));
    }

    #[test]
    fn a_changed_detail_earns_a_write() {
        let now = t("2026-09-01T10:01:00Z");
        let written = vec![row(
            "A",
            "was",
            "2026-09-01T10:00:00+00:00",
            "2026-09-01T10:00:00+00:00",
            1,
        )];
        let computed = vec![row(
            "A",
            "now",
            "2026-09-01T10:00:00+00:00",
            "2026-09-01T10:01:00+00:00",
            3,
        )];
        assert!(problems_write_is_worth_it(&written, &computed, now));
    }

    #[test]
    fn an_age_out_earns_exactly_one_write_and_then_silence() {
        // A fixed problem must not linger as a scar — but clearing it must
        // also not become its own recurring write.
        let now = t("2026-09-01T12:00:00Z");
        let written = vec![row(
            "A",
            "m",
            "2026-09-01T10:00:00+00:00",
            "2026-09-01T10:00:00+00:00",
            1,
        )];
        assert!(
            problems_write_is_worth_it(&written, &[], now),
            "the clear is due"
        );
        assert!(
            !problems_write_is_worth_it(&[], &[], now),
            "a healthy object must cost ZERO writes, forever"
        );
    }

    // --- ageing ---

    #[test]
    fn ageing_is_on_last_seen_not_first_seen() {
        // A failure that started two hours ago and is STILL happening must
        // stay listed; one that stopped an hour ago must go.
        let now = t("2026-09-01T12:00:00Z");
        let still = row(
            "Still",
            "m",
            "2026-09-01T10:00:00+00:00",
            "2026-09-01T11:59:00+00:00",
            200,
        );
        let stopped = row(
            "Stopped",
            "m",
            "2026-09-01T10:00:00+00:00",
            "2026-09-01T10:30:00+00:00",
            3,
        );
        let kept = age_out(&[still.clone(), stopped], DEFAULT_PROBLEM_TTL_SECS, now);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].reason, "Still");
    }

    #[test]
    fn an_unparseable_timestamp_is_kept_rather_than_dropped() {
        // Losing a problem is the failure mode this module exists to prevent,
        // so a malformed row errs towards being visible.
        let now = t("2026-09-01T12:00:00Z");
        let bad = row("A", "m", "garbage", "garbage", 1);
        assert_eq!(age_out(&[bad], DEFAULT_PROBLEM_TTL_SECS, now).len(), 1);
    }

    #[test]
    fn a_short_ttl_ages_out_promptly_so_a_walk_can_prove_it() {
        let now = t("2026-09-01T10:05:00Z");
        let r = row(
            "A",
            "m",
            "2026-09-01T10:00:00+00:00",
            "2026-09-01T10:01:00+00:00",
            2,
        );
        assert_eq!(age_out(std::slice::from_ref(&r), 120, now).len(), 0);
        assert_eq!(age_out(&[r], 600, now).len(), 1);
    }
}
