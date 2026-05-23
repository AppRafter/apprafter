// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Build / update `PlatformStack.status` payloads. Condition
//! transitions follow k8s convention: `lastTransitionTime` moves
//! only when `status` flips, otherwise the prior timestamp is
//! preserved (matches `operator-controllers-application::
//! ready_condition`).

use chrono::Utc;

use operator_core::{PlatformStackCondition, PlatformStackStatus};

pub const COND_SYNCED: &str = "Synced";
pub const COND_UPGRADE_AVAILABLE: &str = "UpgradeAvailable";
pub const COND_MIGRATION_PENDING: &str = "MigrationPending";
pub const COND_UNAUTHORIZED_SOURCE_MODIFICATION: &str = "UnauthorizedSourceModification";
/// `Ready` mirrors `parent.status.health.status`. True iff the
/// parent platform Application reports Healthy (all child
/// Applications synced + their workloads at their chart-defined
/// health threshold). Surfaces alongside the other conditions
/// in `kubectl describe platformstack default`. Walk-fix B.1.74.
pub const COND_READY: &str = "Ready";

/// `YankedVersion=True` when the cluster's `currentVersion`
/// is marked `yanked: true` in the published compatibility
/// metadata. Informational (NOT `Ready=False`) — the platform
/// keeps running fine, the yank is a chart-author hint that
/// the operator should upgrade soon. `condition.message`
/// carries the verbatim `yankedReason` from
/// `compatibility.yaml`. Surfaces in `kubectl describe
/// platformstack default` + `apprafter platform status`.
/// Track B.1.74a.
pub const COND_YANKED_VERSION: &str = "YankedVersion";

/// Maximum entries kept in `PlatformStack.status.versionHistory`.
/// Ring-buffer behaviour: oldest entry drops when this cap is
/// exceeded. Per spec.md §3.11 ("recent N transitions"); 10 is
/// enough for audit + rollback decisions without bloating the
/// CR.
pub const VERSION_HISTORY_CAP: usize = 10;

/// Build a fresh condition or carry the prior transition time
/// forward if the status value has not changed. Use this for
/// every condition write; never set `lastTransitionTime`
/// directly.
pub fn condition(
    type_: &str,
    status: &str,
    reason: &str,
    message: &str,
    prior: &[PlatformStackCondition],
) -> PlatformStackCondition {
    let now = Utc::now().to_rfc3339();
    let last_time = prior
        .iter()
        .find(|c| c.type_ == type_)
        .filter(|c| c.status == status)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| now.clone());
    PlatformStackCondition {
        type_: type_.to_string(),
        status: status.to_string(),
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
        last_transition_time: last_time,
    }
}

/// Upsert a condition into a status's `conditions` slice. Preserves
/// other condition types; replaces the matching one.
pub fn upsert_condition(status: &mut PlatformStackStatus, c: PlatformStackCondition) {
    let mut conds = status.conditions.clone().unwrap_or_default();
    if let Some(existing) = conds.iter_mut().find(|x| x.type_ == c.type_) {
        *existing = c;
    } else {
        conds.push(c);
    }
    status.conditions = Some(conds);
}

/// Append a version-history entry as a ring buffer capped at
/// `VERSION_HISTORY_CAP`. The newest entry lives at the END of
/// the vector (chronological order); oldest entries drop from
/// the FRONT when the cap is exceeded.
///
/// Walk-fix B.1.74: tracks "what versions did PlatformController
/// successfully bring online, in what order?" — feeds rollback +
/// audit. Caller invokes only on a successful SSA patch that
/// actually changes `targetRevision`; status-only or values-only
/// patches don't constitute a version transition.
pub fn append_version_history(
    status: &mut PlatformStackStatus,
    entry: operator_core::PlatformStackVersionHistoryEntry,
) {
    let mut history = status.version_history.clone().unwrap_or_default();
    history.push(entry);
    if history.len() > VERSION_HISTORY_CAP {
        let drop = history.len() - VERSION_HISTORY_CAP;
        history.drain(0..drop);
    }
    status.version_history = Some(history);
}

/// Merge a `versionHistory` vector pulled from the apiserver
/// (`server`) with the in-memory reconcile state's vector
/// (`local`). Used by `reconcile::write_status` to prevent SSA
/// ownership relinquishment from clearing the field on
/// settled-state reconciles (walk-fix #5 post-B.1.77).
///
/// Semantics:
///
///   * Server entries are the authoritative baseline.
///   * Local entries that do NOT yet appear on the server
///     (matched by `(version, appliedAt)`) are appended in
///     the order they appear locally — preserves the
///     newly-appended entry from the current reconcile's
///     bump.
///   * Ring-buffer cap (`VERSION_HISTORY_CAP`) enforced; the
///     oldest entries drop when the merged vector exceeds
///     the cap.
///
/// Duplicate detection is `(version, appliedAt)` rather than
/// version alone because the same chart version may be
/// applied multiple times across rollback / restart, each
/// transition deserving its own audit entry.
pub fn merge_version_history(
    server: Vec<operator_core::PlatformStackVersionHistoryEntry>,
    local: Vec<operator_core::PlatformStackVersionHistoryEntry>,
) -> Vec<operator_core::PlatformStackVersionHistoryEntry> {
    let mut merged = server;
    for entry in local {
        let already_present = merged
            .iter()
            .any(|e| e.version == entry.version && e.applied_at == entry.applied_at);
        if !already_present {
            merged.push(entry);
        }
    }
    if merged.len() > VERSION_HISTORY_CAP {
        let drop = merged.len() - VERSION_HISTORY_CAP;
        merged.drain(0..drop);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn condition_preserves_transition_time_when_status_unchanged() {
        let prior = vec![PlatformStackCondition {
            type_: "Synced".into(),
            status: "True".into(),
            reason: None,
            message: None,
            last_transition_time: "2026-05-20T12:00:00+00:00".into(),
        }];
        let next = condition("Synced", "True", "Same", "msg", &prior);
        assert_eq!(next.last_transition_time, "2026-05-20T12:00:00+00:00");
    }

    #[test]
    fn condition_advances_transition_time_when_status_flips() {
        let prior = vec![PlatformStackCondition {
            type_: "Synced".into(),
            status: "False".into(),
            reason: None,
            message: None,
            last_transition_time: "2026-05-20T12:00:00+00:00".into(),
        }];
        let next = condition("Synced", "True", "Recovered", "msg", &prior);
        assert_ne!(next.last_transition_time, "2026-05-20T12:00:00+00:00");
    }

    #[test]
    fn upsert_replaces_existing_type_and_keeps_others() {
        let mut status = PlatformStackStatus {
            conditions: Some(vec![
                PlatformStackCondition {
                    type_: "Synced".into(),
                    status: "True".into(),
                    reason: None,
                    message: None,
                    last_transition_time: "t".into(),
                },
                PlatformStackCondition {
                    type_: "UpgradeAvailable".into(),
                    status: "False".into(),
                    reason: None,
                    message: None,
                    last_transition_time: "t".into(),
                },
            ]),
            ..PlatformStackStatus::default()
        };
        upsert_condition(
            &mut status,
            PlatformStackCondition {
                type_: "Synced".into(),
                status: "False".into(),
                reason: Some("Error".into()),
                message: Some("m".into()),
                last_transition_time: "t2".into(),
            },
        );
        let conds = status.conditions.unwrap();
        assert_eq!(conds.len(), 2);
        let synced = conds.iter().find(|c| c.type_ == "Synced").unwrap();
        assert_eq!(synced.status, "False");
        // Other condition preserved.
        let upgr = conds
            .iter()
            .find(|c| c.type_ == "UpgradeAvailable")
            .unwrap();
        assert_eq!(upgr.status, "False");
    }

    #[test]
    fn upsert_appends_when_type_absent() {
        let mut status = PlatformStackStatus::default();
        upsert_condition(
            &mut status,
            PlatformStackCondition {
                type_: "Synced".into(),
                status: "True".into(),
                reason: None,
                message: None,
                last_transition_time: "t".into(),
            },
        );
        assert_eq!(status.conditions.as_ref().unwrap().len(), 1);
    }

    fn entry(version: &str, applied_at: &str) -> operator_core::PlatformStackVersionHistoryEntry {
        operator_core::PlatformStackVersionHistoryEntry {
            version: version.into(),
            applied_at: applied_at.into(),
            outcome: "succeeded".into(),
        }
    }

    #[test]
    fn append_version_history_grows_to_cap() {
        let mut status = PlatformStackStatus::default();
        for i in 0..VERSION_HISTORY_CAP {
            append_version_history(
                &mut status,
                entry(
                    &format!("0.1.{i}"),
                    &format!("2026-05-22T00:0{i:01}:00+00:00"),
                ),
            );
        }
        let history = status.version_history.as_ref().expect("history populated");
        assert_eq!(history.len(), VERSION_HISTORY_CAP);
        // Newest entry is at the END of the vector.
        assert_eq!(
            history.last().unwrap().version,
            format!("0.1.{}", VERSION_HISTORY_CAP - 1)
        );
        assert_eq!(history.first().unwrap().version, "0.1.0");
    }

    #[test]
    fn append_version_history_caps_at_max_and_drops_oldest() {
        let mut status = PlatformStackStatus::default();
        // Push 13 entries — the first 3 must be dropped.
        for i in 0..(VERSION_HISTORY_CAP + 3) {
            append_version_history(
                &mut status,
                entry(
                    &format!("0.1.{i}"),
                    &format!("2026-05-22T00:00:{i:02}+00:00"),
                ),
            );
        }
        let history = status.version_history.as_ref().unwrap();
        assert_eq!(history.len(), VERSION_HISTORY_CAP);
        // Oldest survivor is at index 0; oldest 3 dropped.
        assert_eq!(history.first().unwrap().version, "0.1.3");
        // Newest entry at the back.
        assert_eq!(
            history.last().unwrap().version,
            format!("0.1.{}", VERSION_HISTORY_CAP + 2)
        );
    }

    #[test]
    fn merge_version_history_keeps_server_entries_when_local_is_empty() {
        // Settled-state reconcile path: local computed value
        // is empty (no append this cycle), but server has
        // entries from prior bumps. Merge must preserve
        // server entries verbatim — this is the load-bearing
        // case walk-fix #5 addresses.
        let server = vec![
            entry("0.1.27", "2026-05-22T22:00:00+00:00"),
            entry("0.1.29", "2026-05-22T22:30:00+00:00"),
        ];
        let local = vec![];
        let merged = merge_version_history(server.clone(), local);
        assert_eq!(merged, server);
    }

    #[test]
    fn merge_version_history_appends_local_only_entries() {
        // Bump-cycle reconcile path: local has an entry the
        // server doesn't (just-appended by the current
        // reconcile). Merged vector includes server's prior
        // entries PLUS the new local entry, in that order.
        let server = vec![entry("0.1.27", "2026-05-22T22:00:00+00:00")];
        let local = vec![
            entry("0.1.27", "2026-05-22T22:00:00+00:00"), // dup, dropped
            entry("0.1.29", "2026-05-22T22:30:00+00:00"), // new
        ];
        let merged = merge_version_history(server, local);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].version, "0.1.27");
        assert_eq!(merged[1].version, "0.1.29");
    }

    #[test]
    fn merge_version_history_dedupes_by_version_and_applied_at() {
        // Same (version, appliedAt) pair from both sides
        // dedupes к one entry. Same version + DIFFERENT
        // appliedAt is treated as separate transitions
        // (rollback к prior version still gets its own audit
        // entry).
        let ts_a = "2026-05-22T22:00:00+00:00";
        let ts_b = "2026-05-22T22:30:00+00:00";
        let server = vec![entry("0.1.27", ts_a)];
        let local = vec![
            entry("0.1.27", ts_a), // exact dup
            entry("0.1.27", ts_b), // same version, new timestamp → kept
        ];
        let merged = merge_version_history(server, local);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_version_history_caps_at_max() {
        // Ring-buffer cap enforcement. Merge that produces
        // VERSION_HISTORY_CAP+3 entries drops the oldest 3.
        let mut server: Vec<_> = (0..VERSION_HISTORY_CAP)
            .map(|i| {
                entry(
                    &format!("0.1.{i}"),
                    &format!("2026-05-22T22:00:0{i:01}+00:00"),
                )
            })
            .collect();
        let local = vec![
            entry(
                &format!("0.1.{}", VERSION_HISTORY_CAP),
                "2026-05-22T22:01:00+00:00",
            ),
            entry(
                &format!("0.1.{}", VERSION_HISTORY_CAP + 1),
                "2026-05-22T22:02:00+00:00",
            ),
            entry(
                &format!("0.1.{}", VERSION_HISTORY_CAP + 2),
                "2026-05-22T22:03:00+00:00",
            ),
        ];
        let mut server_clone = server.clone();
        server_clone.extend(local.clone());
        let merged = merge_version_history(server.clone(), local);
        assert_eq!(merged.len(), VERSION_HISTORY_CAP);
        // Newest entries survived; first 3 of original dropped.
        assert_eq!(
            merged.last().unwrap().version,
            format!("0.1.{}", VERSION_HISTORY_CAP + 2)
        );
        // Oldest survivor.
        server.drain(0..3);
        assert_eq!(merged[0].version, server[0].version);
    }

    #[test]
    fn append_version_history_starts_from_empty_status() {
        // Regression guard: even when `status.version_history`
        // is None (first reconcile), append() must initialize
        // it to `Some(vec![entry])`.
        let mut status = PlatformStackStatus::default();
        assert!(status.version_history.is_none());
        append_version_history(&mut status, entry("0.1.20", "2026-05-22T00:00:00+00:00"));
        assert_eq!(status.version_history.as_ref().unwrap().len(), 1);
    }
}
