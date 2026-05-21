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
}
