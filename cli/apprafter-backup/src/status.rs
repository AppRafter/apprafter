// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Status ConfigMap payload builder for the in-cluster backup runner.

use crate::orchestrate::RunOutcome;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a Kubernetes ConfigMap JSON payload that records the outcome of one
/// backup run.
///
/// The returned value is a partial patch document — it only includes the fields
/// for *this* run.  The caller (a future `write_status` function) will merge it
/// with the live ConfigMap so that the previous run's fields survive (e.g., a
/// failure run keeps `lastSuccess` from the prior successful run).
///
/// # Fields emitted
/// Always: `lastRunFormat`.
/// On success: `lastSuccess` (set to `now`).
/// On failure: `lastFailure` (set to `now`) + `lastError`.
pub fn status_configmap(outcome: &RunOutcome, format: &str, now: &str) -> serde_json::Value {
    let mut data = serde_json::json!({
        "lastRunFormat": format,
    });

    match outcome {
        RunOutcome::Success { .. } => {
            data["lastSuccess"] = serde_json::Value::String(now.to_string());
        }
        RunOutcome::Failure { error } => {
            data["lastFailure"] = serde_json::Value::String(now.to_string());
            data["lastError"] = serde_json::Value::String(error.clone());
        }
    }

    serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": "apprafter-backup-status",
            "namespace": "apprafter-system",
        },
        "data": data,
    })
}

// ---------------------------------------------------------------------------
// Tests (written first — TDD red phase)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_cm_success_fields() {
        let cm = status_configmap(
            &crate::orchestrate::RunOutcome::Success {
                snapshot: Some("s".into()),
            },
            "monolithic",
            "2026-07-17T03:00:00Z",
        );
        assert_eq!(cm["metadata"]["name"], "apprafter-backup-status");
        assert_eq!(cm["metadata"]["namespace"], "apprafter-system");
        assert_eq!(cm["kind"], "ConfigMap");
        assert_eq!(cm["data"]["lastSuccess"], "2026-07-17T03:00:00Z");
        assert_eq!(cm["data"]["lastRunFormat"], "monolithic");
        assert!(cm["data"].get("lastError").is_none());
        assert!(cm["data"].get("lastFailure").is_none());
    }

    #[test]
    fn status_cm_failure_fields() {
        let cm = status_configmap(
            &crate::orchestrate::RunOutcome::Failure {
                error: "boom".into(),
            },
            "sequential",
            "2026-07-17T03:05:00Z",
        );
        assert_eq!(cm["data"]["lastFailure"], "2026-07-17T03:05:00Z");
        assert_eq!(cm["data"]["lastError"], "boom");
        assert!(cm["data"].get("lastSuccess").is_none());
        assert_eq!(cm["data"]["lastRunFormat"], "sequential");
    }
}
