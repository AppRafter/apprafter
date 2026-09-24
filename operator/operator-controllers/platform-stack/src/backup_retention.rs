// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `BackupRetention`: whether the backup repository's retention is enforced
//! in the cluster, and how big the repository is (WI-389).
//!
//! # Who prunes
//!
//! `spec.backup.retention.enforce` decides; absent, the chart's default,
//! `check`:
//!
//! - `check` — the weekly check Job prunes after a check that passed, as far
//!   as the cluster's S3 key may delete. The scoped key ADR 0050 recommends
//!   may not: the runner stops at the first refused delete, deletes nothing,
//!   and records `lastPruneResult: not-permitted`. A check that does not pass
//!   never prunes.
//! - `cluster` — the backup Job prunes after every backup.
//! - `operator` — nothing in the cluster prunes; `apprafter backup prune`,
//!   run outside it with full credentials, does, and stamps
//!   `apprafter.io/last-prune` on the stack.
//!
//! # The verdict
//!
//! Built from the runner's own record (`apprafter-backup-status`: `lastCheck*`,
//! `lastPrune*`, `repo*`), which is the only place a prune's outcome exists.
//!
//! - **Absent** while backups are disabled.
//! - **True** (`Pruned`, `NothingToPrune`) when the prune the mode promises
//!   ran after the latest check (or backup, under `cluster`) and did its job.
//! - **False** when retention is not being enforced in the cluster:
//!   `PruneNotPermitted` (the key may not delete — nothing was deleted),
//!   `PruneFailed`, `CheckFailed` (a check that did not pass never prunes),
//!   `CheckOff` (`enforce: check` with the weekly check turned off), and
//!   `EnforcedOutsideCluster` (`enforce: operator`, chosen: honest, not a
//!   failure). None of these marks backups as failing: that is
//!   `BackupHealthy`'s question, and a scoped key is the recommended setup.
//! - **Unknown** when there is nothing to judge yet (`NoCheckYet`,
//!   `NoPruneYet`) or the record could not be read (`RecordUnreadable`).
//!
//! Every message that has them carries the repository's size and counts from
//! the last check and how they moved since the check before, so a repository
//! nothing prunes is seen growing. Messages are built from recorded values
//! only, never from "how long ago", so re-reading an unchanged record gives
//! a byte-identical condition.

use operator_core::platform_stack::{resolve_retention_enforce, BackupConfig};
use serde_json::Value;

use crate::backup_health::{Observed, Verdict};

pub const REASON_PRUNED: &str = "Pruned";
pub const REASON_NOTHING_TO_PRUNE: &str = "NothingToPrune";
/// The cluster's key may not delete: the prune deleted nothing. The one
/// retention reason that says "run `apprafter backup prune` yourself".
pub const REASON_NOT_PERMITTED: &str = "PruneNotPermitted";
pub const REASON_PRUNE_FAILED: &str = "PruneFailed";
pub const REASON_CHECK_FAILED: &str = "CheckFailed";
pub const REASON_CHECK_OFF: &str = "CheckOff";
pub const REASON_OUTSIDE_CLUSTER: &str = "EnforcedOutsideCluster";
pub const REASON_NO_CHECK_YET: &str = "NoCheckYet";
pub const REASON_NO_PRUNE_YET: &str = "NoPruneYet";
pub const REASON_UNREADABLE: &str = "RecordUnreadable";

/// Longest piece of the runner's prune detail quoted in a message.
const DETAIL_QUOTE_MAX: usize = 300;

/// The `BackupRetention` verdict for a stack with this backup config and
/// `apprafter.io/last-prune` annotation, given what `backup_health::observe`
/// returned.
pub fn assess(
    backup: Option<&BackupConfig>,
    last_prune_annotation: Option<&str>,
    observed: Result<&Observed, &str>,
) -> Verdict {
    let Some(backup) = backup.filter(|b| b.enabled) else {
        return Verdict::Absent;
    };
    let observed = match observed {
        Ok(o) => o,
        Err(e) => {
            return unknown(
                REASON_UNREADABLE,
                format!("could not read the backup objects: {e}"),
            )
        }
    };
    if let Some(e) = &observed.runner_status_error {
        return unknown(
            REASON_UNREADABLE,
            format!("could not read the backup runner's record: {e}"),
        );
    }
    let record = Record(observed.runner_status.as_ref());
    let repo = record.repository();
    let outside = match last_prune_annotation {
        Some(t) => format!(" `apprafter backup prune` last ran against this cluster at {t}."),
        None => " `apprafter backup prune` has not been run against this cluster.".to_string(),
    };

    match resolve_retention_enforce(backup) {
        "operator" => Verdict::Condition {
            status: "False",
            reason: REASON_OUTSIDE_CLUSTER,
            message: format!(
                "spec.backup.retention.enforce is operator: nothing in the cluster deletes a \
                 snapshot, and the repository grows until `apprafter backup prune` runs with \
                 full credentials.{outside}{repo}"
            ),
        },
        "cluster" => match record.prune() {
            Some(prune) => prune.verdict(&outside, &repo),
            None => unknown(
                REASON_NO_PRUNE_YET,
                format!(
                    "spec.backup.retention.enforce is cluster: the backup Job prunes after each \
                     backup, and none has recorded a prune yet.{repo}"
                ),
            ),
        },
        // `check`, the default. The CRD's enum admits nothing else.
        _ => {
            if backup.check_schedule.trim().is_empty() {
                return Verdict::Condition {
                    status: "False",
                    reason: REASON_CHECK_OFF,
                    message: format!(
                        "spec.backup.retention.enforce is check, which prunes after the weekly \
                         check, and the weekly check is off (spec.backup.checkSchedule is \
                         empty): nothing in the cluster prunes the repository.{outside}{repo}"
                    ),
                };
            }
            let Some(check) = record.check() else {
                return unknown(
                    REASON_NO_CHECK_YET,
                    format!(
                        "the weekly check prunes after a check that passes, and no check has \
                         recorded a result yet (schedule \"{}\").{repo}",
                        backup.check_schedule
                    ),
                );
            };
            // The newest of the two decides: a prune recorded after the
            // latest check is that check's prune.
            let prune = record.prune().filter(|p| not_before(p.at, check.at));
            if !check.passed && prune.is_none() {
                return Verdict::Condition {
                    status: "False",
                    reason: REASON_CHECK_FAILED,
                    message: format!(
                        "the weekly check at {} did not pass, and a check that does not pass \
                         never prunes: retention waits for one that passes.{repo}",
                        check.at
                    ),
                };
            }
            match prune {
                Some(p) => p.verdict(&outside, &repo),
                None => unknown(
                    REASON_NO_PRUNE_YET,
                    format!(
                        "the weekly check at {} passed, and no prune after it is recorded.{repo}",
                        check.at
                    ),
                ),
            }
        }
    }
}

/// Is `a` at or after `b`? Both are the runner's RFC 3339 times; one that
/// does not parse is compared as text, which orders the runner's own format
/// correctly too.
fn not_before(a: &str, b: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a >= b,
        _ => a >= b,
    }
}

fn unknown(reason: &'static str, message: String) -> Verdict {
    Verdict::Condition {
        status: "Unknown",
        reason,
        message,
    }
}

/// The runner's record, `data` of
/// [`crate::backup_health::RUNNER_STATUS_CONFIGMAP`].
struct Record<'a>(Option<&'a Value>);

struct Check<'a> {
    at: &'a str,
    passed: bool,
}

struct Prune<'a> {
    at: &'a str,
    result: &'a str,
    detail: String,
    /// "the weekly check" or "the backup".
    after: &'static str,
}

impl<'a> Record<'a> {
    fn field(&self, key: &str) -> Option<&'a str> {
        self.0?
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    fn check(&self) -> Option<Check<'a>> {
        Some(Check {
            at: self.field("lastCheck")?,
            passed: self.field("lastCheckResult") == Some("passed"),
        })
    }

    fn prune(&self) -> Option<Prune<'a>> {
        let detail = self.field("lastPruneDetail").unwrap_or("");
        let mut quoted: String = detail.chars().take(DETAIL_QUOTE_MAX).collect();
        if quoted.len() < detail.len() {
            quoted.push('…');
        }
        Some(Prune {
            at: self.field("lastPrune")?,
            result: self.field("lastPruneResult")?,
            detail: quoted,
            after: match self.field("lastPruneBy") {
                Some("backup") => "the backup",
                _ => "the weekly check",
            },
        })
    }

    /// " The repository held … at …; … since …." — or "" before the first
    /// reading.
    fn repository(&self) -> String {
        let num = |key: &str| self.field(key).and_then(|v| v.parse::<i64>().ok());
        let (Some(at), Some(bytes)) = (self.field("repoStatsAt"), num("repoBytes")) else {
            return String::new();
        };
        let mut counts = Vec::new();
        if let Some(n) = num("repoSnapshots") {
            counts.push(format!("{n} snapshot(s)"));
        }
        if let Some(n) = num("repoBlobs") {
            counts.push(format!("{n} blob(s)"));
        }
        let counts = if counts.is_empty() {
            String::new()
        } else {
            format!(" in {}", counts.join(" and "))
        };
        let mut s = format!(
            " The repository held {}{counts} at {at}",
            human_bytes(bytes)
        );
        if let (Some(prev_at), Some(prev_bytes)) =
            (self.field("repoPrevStatsAt"), num("repoPrevBytes"))
        {
            let mut moved = vec![signed_bytes(bytes - prev_bytes)];
            if let (Some(n), Some(p)) = (num("repoSnapshots"), num("repoPrevSnapshots")) {
                moved.push(format!("{:+} snapshot(s)", n - p));
            }
            if let (Some(n), Some(p)) = (num("repoBlobs"), num("repoPrevBlobs")) {
                moved.push(format!("{:+} blob(s)", n - p));
            }
            s.push_str(&format!("; {} since {prev_at}", moved.join(", ")));
        }
        s.push('.');
        s
    }
}

impl Prune<'_> {
    fn verdict(&self, outside: &str, repo: &str) -> Verdict {
        let (at, after, detail) = (self.at, self.after, &self.detail);
        match self.result {
            "pruned" => Verdict::Condition {
                status: "True",
                reason: REASON_PRUNED,
                message: format!("the prune after {after} at {at}: {detail}.{repo}"),
            },
            "nothing-to-prune" => Verdict::Condition {
                status: "True",
                reason: REASON_NOTHING_TO_PRUNE,
                message: format!("the prune after {after} at {at}: {detail}.{repo}"),
            },
            "not-permitted" => Verdict::Condition {
                status: "False",
                reason: REASON_NOT_PERMITTED,
                message: format!(
                    "retention is not enforced: the cluster's S3 key may not delete, and the \
                     prune after {after} at {at} was {detail}. Run `apprafter backup prune` \
                     with full credentials.{outside}{repo}"
                ),
            },
            "failed" => Verdict::Condition {
                status: "False",
                reason: REASON_PRUNE_FAILED,
                message: format!("the prune after {after} at {at} failed: {detail}.{repo}"),
            },
            other => unknown(
                REASON_UNREADABLE,
                format!(
                    "the backup runner recorded a prune result this operator does not know, \
                     \"{other}\", at {at}.{repo}"
                ),
            ),
        }
    }
}

/// `1.2 GiB`, `155.0 MiB`, `512 B`.
fn human_bytes(bytes: i64) -> String {
    const UNITS: [(&str, i64); 4] = [
        ("TiB", 1 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
    ];
    for (unit, scale) in UNITS {
        if bytes.abs() >= scale {
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    format!("{bytes} B")
}

/// `+155.0 MiB`, `-1.2 GiB`, `+0 B`.
fn signed_bytes(delta: i64) -> String {
    let s = human_bytes(delta.abs());
    if delta < 0 {
        format!("-{s}")
    } else {
        format!("+{s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn backup(enforce: Option<&str>, check_schedule: &str) -> BackupConfig {
        serde_json::from_value(json!({
            "enabled": true, "schedule": "0 3 * * *", "bucket": "s3:x",
            "credentialRef": {"name": "c"}, "stagingMode": "monolithic",
            "checkSchedule": check_schedule, "checkReadData": false,
            "retention": match enforce {
                Some(e) => json!({"enforce": e}),
                None => json!({}),
            },
        }))
        .unwrap()
    }

    fn observed(record: Value) -> Observed {
        Observed {
            runner_status: Some(record),
            ..Observed::default()
        }
    }

    fn cond(v: &Verdict) -> (&'static str, &'static str, String) {
        match v {
            Verdict::Condition {
                status,
                reason,
                message,
            } => (status, reason, message.clone()),
            Verdict::Absent => panic!("expected a condition"),
        }
    }

    const CHECK_T: &str = "2026-09-20T06:00:30+00:00";
    const PRUNE_T: &str = "2026-09-20T06:00:41+00:00";

    /// A week of the recommended setup: the check passed, the prune was
    /// refused, and the repository grew.
    fn scoped_week() -> Value {
        json!({
            "lastSuccess": "2026-09-20T03:01:00+00:00",
            "lastCheck": CHECK_T, "lastCheckResult": "passed", "lastCheckError": "",
            "lastPrune": PRUNE_T, "lastPruneResult": "not-permitted", "lastPruneBy": "check",
            "lastPruneDetail": "not permitted: the storage refused to delete snapshot ecd0be32 \
                (Remove(<snapshot/ecd0be3219>) failed: client.RemoveObject: Access Denied.), so \
                nothing was deleted; 9 snapshot(s) of 9 run(s) are past the keep policy",
            "repoStatsAt": "2026-09-20T06:00:44+00:00", "repoBytes": "1288490189",
            "repoSnapshots": "42", "repoBlobs": "310512",
            "repoPrevStatsAt": "2026-09-13T06:00:40+00:00", "repoPrevBytes": "1125908480",
            "repoPrevSnapshots": "35", "repoPrevBlobs": "299492",
        })
    }

    #[test]
    fn a_key_that_may_not_delete_is_a_loud_warning_with_the_growth() {
        let o = observed(scoped_week());
        let v = assess(Some(&backup(None, "0 6 * * 0")), None, Ok(&o));
        let (status, reason, message) = cond(&v);
        assert_eq!((status, reason), ("False", REASON_NOT_PERMITTED));
        assert!(
            message.starts_with("retention is not enforced"),
            "{message}"
        );
        assert!(message.contains("was not permitted"), "{message}");
        assert!(message.contains("nothing was deleted"), "{message}");
        assert!(message.contains("Access Denied"), "{message}");
        assert!(
            message.contains("Run `apprafter backup prune` with full credentials"),
            "{message}"
        );
        assert!(
            message.contains("The repository held 1.2 GiB in 42 snapshot(s) and 310512 blob(s)"),
            "{message}"
        );
        assert!(
            message.contains(
                "+155.1 MiB, +7 snapshot(s), +11020 blob(s) since 2026-09-13T06:00:40+00:00"
            ),
            "{message}"
        );
        assert!(
            message.contains("`apprafter backup prune` has not been run against this cluster"),
            "{message}"
        );
        // Re-reading the same record gives the same message: no status write.
        assert_eq!(assess(Some(&backup(None, "0 6 * * 0")), None, Ok(&o)), v);
    }

    #[test]
    fn a_prune_after_the_check_is_enforced_retention() {
        let mut r = scoped_week();
        r["lastPruneResult"] = json!("pruned");
        r["lastPruneDetail"] = json!(
            "forgot 9 snapshot(s) of 9 run(s) and pruned the data only they used; 17 run(s) kept"
        );
        r["repoPrevBytes"] = json!("1400000000");
        let o = observed(r);
        let (status, reason, message) = cond(&assess(
            Some(&backup(Some("check"), "0 6 * * 0")),
            None,
            Ok(&o),
        ));
        assert_eq!((status, reason), ("True", REASON_PRUNED));
        assert!(
            message.starts_with(&format!(
                "the prune after the weekly check at {PRUNE_T}: forgot 9"
            )),
            "{message}"
        );
        assert!(message.contains("-106.3 MiB"), "{message}");

        let mut r = scoped_week();
        r["lastPruneResult"] = json!("nothing-to-prune");
        let o = observed(r);
        let (status, reason, _) = cond(&assess(Some(&backup(None, "0 6 * * 0")), None, Ok(&o)));
        assert_eq!((status, reason), ("True", REASON_NOTHING_TO_PRUNE));
    }

    /// The owner's rule, read back: a check that did not pass never prunes,
    /// and the condition says so rather than showing last week's prune.
    #[test]
    fn a_check_that_did_not_pass_is_retention_waiting_not_last_weeks_prune() {
        let mut r = scoped_week();
        r["lastPruneResult"] = json!("pruned");
        r["lastCheck"] = json!("2026-09-27T06:00:30+00:00");
        r["lastCheckResult"] = json!("failed");
        r["lastCheckError"] = json!("pack 5e1f0a2b contains 1 error");
        let o = observed(r);
        let (status, reason, message) =
            cond(&assess(Some(&backup(None, "0 6 * * 0")), None, Ok(&o)));
        assert_eq!((status, reason), ("False", REASON_CHECK_FAILED));
        assert!(
            message.contains("2026-09-27T06:00:30+00:00 did not pass"),
            "{message}"
        );
    }

    #[test]
    fn a_passed_check_with_no_prune_after_it_is_not_known_yet() {
        let mut r = scoped_week();
        r["lastCheck"] = json!("2026-09-27T06:00:30+00:00");
        let o = observed(r);
        let (status, reason, _) = cond(&assess(Some(&backup(None, "0 6 * * 0")), None, Ok(&o)));
        assert_eq!((status, reason), ("Unknown", REASON_NO_PRUNE_YET));
    }

    #[test]
    fn no_check_yet_is_unknown_and_names_the_schedule() {
        let o = observed(json!({"lastSuccess": "2026-09-20T03:01:00+00:00"}));
        let (status, reason, message) =
            cond(&assess(Some(&backup(None, "0 6 * * 0")), None, Ok(&o)));
        assert_eq!((status, reason), ("Unknown", REASON_NO_CHECK_YET));
        assert!(message.contains("\"0 6 * * 0\""), "{message}");
        let empty = Observed::default();
        let (_, reason, _) = cond(&assess(Some(&backup(None, "0 6 * * 0")), None, Ok(&empty)));
        assert_eq!(reason, REASON_NO_CHECK_YET);
    }

    /// `enforce: check` with the check turned off prunes nothing, and must
    /// not read as a check still to come.
    #[test]
    fn the_default_mode_with_the_check_off_says_nothing_prunes() {
        let o = observed(scoped_week());
        let (status, reason, message) = cond(&assess(
            Some(&backup(None, "")),
            Some("2026-09-01T10:00:00Z"),
            Ok(&o),
        ));
        assert_eq!((status, reason), ("False", REASON_CHECK_OFF));
        assert!(message.contains("checkSchedule is empty"), "{message}");
        assert!(
            message.contains("last ran against this cluster at 2026-09-01T10:00:00Z"),
            "{message}"
        );
    }

    /// The explicit opt-out: never pruned in the cluster, said plainly, with
    /// the growth and the last prune from outside.
    #[test]
    fn enforce_operator_is_retention_outside_the_cluster() {
        let o = observed(scoped_week());
        let (status, reason, message) = cond(&assess(
            Some(&backup(Some("operator"), "0 6 * * 0")),
            Some("2026-09-01T10:00:00Z"),
            Ok(&o),
        ));
        assert_eq!((status, reason), ("False", REASON_OUTSIDE_CLUSTER));
        assert!(message.contains("enforce is operator"), "{message}");
        assert!(message.contains("2026-09-01T10:00:00Z"), "{message}");
        assert!(message.contains("The repository held"), "{message}");
    }

    #[test]
    fn enforce_cluster_reads_the_backup_jobs_prune() {
        let r = json!({
            "lastPrune": "2026-09-20T03:02:00+00:00", "lastPruneResult": "pruned",
            "lastPruneBy": "backup", "lastPruneDetail": "forgot 1 snapshot(s) of 1 run(s)",
        });
        let o = observed(r);
        let (status, reason, message) = cond(&assess(
            Some(&backup(Some("cluster"), "0 6 * * 0")),
            None,
            Ok(&o),
        ));
        assert_eq!((status, reason), ("True", REASON_PRUNED));
        assert!(
            message.starts_with("the prune after the backup at"),
            "{message}"
        );
        let none = observed(json!({}));
        let (status, reason, _) = cond(&assess(
            Some(&backup(Some("cluster"), "0 6 * * 0")),
            None,
            Ok(&none),
        ));
        assert_eq!((status, reason), ("Unknown", REASON_NO_PRUNE_YET));
    }

    #[test]
    fn a_failed_prune_quotes_the_runner() {
        let mut r = scoped_week();
        r["lastPruneResult"] = json!("failed");
        r["lastPruneDetail"] = json!("restic prune: exit status 1: Fatal: unable to create lock");
        let o = observed(r);
        let (status, reason, message) =
            cond(&assess(Some(&backup(None, "0 6 * * 0")), None, Ok(&o)));
        assert_eq!((status, reason), ("False", REASON_PRUNE_FAILED));
        assert!(message.contains("unable to create lock"), "{message}");
    }

    #[test]
    fn nothing_is_said_while_backups_are_off_and_nothing_guessed_when_unreadable() {
        let o = observed(scoped_week());
        let mut off = backup(None, "0 6 * * 0");
        off.enabled = false;
        assert_eq!(assess(Some(&off), None, Ok(&o)), Verdict::Absent);
        assert_eq!(assess(None, None, Ok(&o)), Verdict::Absent);

        let (status, reason, _) = cond(&assess(
            Some(&backup(None, "0 6 * * 0")),
            None,
            Err("forbidden"),
        ));
        assert_eq!((status, reason), ("Unknown", REASON_UNREADABLE));
        let unreadable = Observed {
            runner_status_error: Some("configmaps is forbidden".into()),
            ..Observed::default()
        };
        let (status, reason, message) = cond(&assess(
            Some(&backup(None, "0 6 * * 0")),
            None,
            Ok(&unreadable),
        ));
        assert_eq!((status, reason), ("Unknown", REASON_UNREADABLE));
        assert!(message.contains("forbidden"), "{message}");
    }

    /// Times with and without fractional seconds, and in other offsets:
    /// what orders a prune against a check is the instant, not the text.
    #[test]
    fn a_prune_is_ordered_against_the_check_by_instant() {
        assert!(not_before(
            "2026-09-20T06:00:30.5+00:00",
            "2026-09-20T06:00:30+00:00"
        ));
        assert!(!not_before(
            "2026-09-20T06:00:30+00:00",
            "2026-09-20T06:00:30.5+00:00"
        ));
        assert!(not_before(
            "2026-09-20T08:00:31+02:00",
            "2026-09-20T06:00:30Z"
        ));
        assert!(!not_before(
            "2026-09-20T07:59:00+02:00",
            "2026-09-20T06:00:30Z"
        ));
    }

    #[test]
    fn sizes_read_as_people_read_them() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(1_288_490_189), "1.2 GiB");
        assert_eq!(signed_bytes(162_581_709), "+155.1 MiB");
        assert_eq!(signed_bytes(-1_288_490_189), "-1.2 GiB");
        assert_eq!(signed_bytes(0), "+0 B");
    }
}
