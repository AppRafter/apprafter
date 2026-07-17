// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Status ConfigMap payload builder and in-cluster upsert for the backup runner.

use crate::orchestrate::RunOutcome;

// ---------------------------------------------------------------------------
// Merge helper
// ---------------------------------------------------------------------------

/// Merge the new run's status fields onto the old ConfigMap `data` object.
///
/// The caller passes the `data` object from the *live* CM (if any) as `old`,
/// and the `data` section that `status_configmap` emitted for this run as
/// `new`.  The result starts from `old` and overlays every key from `new`,
/// so that:
///
/// * A success run overlays `lastSuccess` + `lastRunFormat` while keeping
///   `lastFailure` / `lastError` that a prior failure had written.
/// * A failure run overlays `lastFailure` + `lastError` + `lastRunFormat`
///   while keeping `lastSuccess` from a prior success.
///
/// Both arguments must be JSON objects; non-object inputs fall back to the
/// new data alone.
pub fn merge_status_data(old: &serde_json::Value, new: &serde_json::Value) -> serde_json::Value {
    let mut merged = match old.as_object() {
        Some(o) => o.clone(),
        None => return new.clone(),
    };
    if let Some(n) = new.as_object() {
        for (k, v) in n {
            merged.insert(k.clone(), v.clone());
        }
    }
    serde_json::Value::Object(merged)
}

// ---------------------------------------------------------------------------
// Kube upsert
// ---------------------------------------------------------------------------

/// Write (create-or-update) the `apprafter-backup-status` ConfigMap in
/// `apprafter-system`, merging the new run's fields with whatever the live CM
/// already holds.
///
/// Uses server-side apply with field manager `apprafter-backup` so the write
/// is idempotent even under concurrent runners.  Returns the kube-rs error as
/// a [`cli_core::CliError`] — the caller decides whether to propagate it or
/// treat it as best-effort.
pub async fn write_status(
    client: &kube::Client,
    outcome: &crate::orchestrate::RunOutcome,
    format: &str,
    now: &str,
) -> cli_core::Result<()> {
    use k8s_openapi::api::core::v1::ConfigMap;
    use kube::api::{Api, Patch, PatchParams};

    const CM_NAME: &str = "apprafter-backup-status";
    const NS: &str = "apprafter-system";

    let api: Api<ConfigMap> = Api::namespaced(client.clone(), NS);

    // Build the partial CM for this run (only the current run's fields).
    let this_run_cm = status_configmap(outcome, format, now);
    let this_run_data = &this_run_cm["data"];

    // Read the live CM (if it already exists) and merge so that BOTH
    // lastSuccess and lastFailure survive across alternating runs.
    let merged_data = match api
        .get_opt(CM_NAME)
        .await
        .map_err(|e| cli_core::CliError::Other(format!("get status CM {NS}/{CM_NAME}: {e}")))?
    {
        Some(live_cm) => {
            let old_data = live_cm
                .data
                .map(|m| {
                    let obj: serde_json::Map<_, _> = m
                        .into_iter()
                        .map(|(k, v)| (k, serde_json::Value::String(v)))
                        .collect();
                    serde_json::Value::Object(obj)
                })
                .unwrap_or(serde_json::Value::Object(Default::default()));
            merge_status_data(&old_data, this_run_data)
        }
        None => this_run_data.clone(),
    };

    // Rebuild the full patch document with the merged data.
    let patch_doc = serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": CM_NAME,
            "namespace": NS,
        },
        "data": merged_data,
    });

    let pp = PatchParams::apply("apprafter-backup").force();
    api.patch(CM_NAME, &pp, &Patch::Apply(patch_doc))
        .await
        .map_err(|e| cli_core::CliError::Other(format!("SSA status CM {NS}/{CM_NAME}: {e}")))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Pure CM builder
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

    // -- merge_status_data ----------------------------------------------------

    #[test]
    fn merge_keeps_prior_lastfailure_on_a_success_run() {
        let old = serde_json::json!({
            "lastFailure": "t0",
            "lastError": "boom",
            "lastRunFormat": "monolithic"
        });
        let new = serde_json::json!({
            "lastSuccess": "t1",
            "lastRunFormat": "monolithic"
        });
        let m = merge_status_data(&old, &new);
        assert_eq!(m["lastSuccess"], "t1");
        assert_eq!(m["lastFailure"], "t0"); // preserved
        assert_eq!(m["lastError"], "boom"); // preserved
        assert_eq!(m["lastRunFormat"], "monolithic");
    }

    #[test]
    fn merge_overwrites_same_keys() {
        let old = serde_json::json!({"lastSuccess": "t0"});
        let new = serde_json::json!({"lastSuccess": "t1", "lastRunFormat": "seq"});
        let m = merge_status_data(&old, &new);
        assert_eq!(m["lastSuccess"], "t1");
        assert_eq!(m["lastRunFormat"], "seq");
    }

    #[test]
    fn merge_keeps_prior_lastsuccess_on_a_failure_run() {
        let old = serde_json::json!({
            "lastSuccess": "t0",
            "lastRunFormat": "sequential"
        });
        let new = serde_json::json!({
            "lastFailure": "t1",
            "lastError": "oops",
            "lastRunFormat": "sequential"
        });
        let m = merge_status_data(&old, &new);
        assert_eq!(m["lastSuccess"], "t0"); // preserved
        assert_eq!(m["lastFailure"], "t1");
        assert_eq!(m["lastError"], "oops");
    }

    #[test]
    fn merge_with_empty_old_returns_new() {
        let old = serde_json::json!({});
        let new = serde_json::json!({"lastSuccess": "t1"});
        let m = merge_status_data(&old, &new);
        assert_eq!(m["lastSuccess"], "t1");
    }

    // -- status_configmap -----------------------------------------------------

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
