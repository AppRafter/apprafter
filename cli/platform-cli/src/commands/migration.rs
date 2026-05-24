// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter migration …` thin wrappers. Track B.1.79.
//!
//! `list` reads MigrationPlans from `apprafter-system` and
//! formats them as a table. `approve` patches
//! `status.phase=approved`. `reject` patches
//! `status.phase=rejected` — webhook denies for application-
//! scope plans (ADR 0027); the CLI surfaces the denial verbatim.

use cli_core::Result;
use serde_json::Value;
use tabled::{Table, Tabled};

use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_get_json, kubectl_merge_patch,
};

const MIGRATION_PLAN_NAMESPACE: &str = "apprafter-system";

#[derive(Tabled)]
struct PlanRow {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "SCOPE")]
    scope: String,
    #[tabled(rename = "CLASSIFICATION")]
    classification: String,
    #[tabled(rename = "PHASE")]
    phase: String,
}

pub fn list() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let json = kubectl_get_json(
        "migrationplan",
        None,
        Some(MIGRATION_PLAN_NAMESPACE),
        kc.path(),
    )?;
    let items = json
        .as_ref()
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if items.is_empty() {
        println!("No MigrationPlans in {MIGRATION_PLAN_NAMESPACE}.");
        return Ok(());
    }

    let rows: Vec<PlanRow> = items.iter().map(plan_row).collect();
    println!("{}", Table::new(&rows));
    Ok(())
}

fn plan_row(plan: &Value) -> PlanRow {
    let name = plan
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let scope = plan
        .pointer("/spec/scope/type")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let classification = plan
        .pointer("/spec/risks/classification")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let phase = plan
        .pointer("/status/phase")
        .and_then(Value::as_str)
        .unwrap_or("pending-approval")
        .to_string();
    PlanRow {
        name,
        scope,
        classification,
        phase,
    }
}

pub fn approve(name: &str) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    kubectl_merge_patch(
        "migrationplan",
        name,
        Some(MIGRATION_PLAN_NAMESPACE),
        Some("status"),
        r#"{"status":{"phase":"approved"}}"#,
        kc.path(),
    )?;
    println!("Approved MigrationPlan {MIGRATION_PLAN_NAMESPACE}/{name}.");
    println!(
        "MigrationController will transition through executing → completed; the \
         PlatformController's next reconcile sees completed and proceeds with the bump."
    );
    Ok(())
}

pub fn reject(name: &str) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    // Webhook denies application-scope rejects per ADR 0027;
    // platform-scope succeeds + PlatformMigrationStrategy.reject
    // reverts spec.pin to previousSpecSnapshot.pin. The CLI
    // just forwards the patch and surfaces whatever the
    // apiserver returns — the message body already contains
    // the ADR-0027 hint when the denial fires (walk-fix #2).
    kubectl_merge_patch(
        "migrationplan",
        name,
        Some(MIGRATION_PLAN_NAMESPACE),
        Some("status"),
        r#"{"status":{"phase":"rejected"}}"#,
        kc.path(),
    )?;
    println!("Rejected MigrationPlan {MIGRATION_PLAN_NAMESPACE}/{name}.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plan_row_defaults_to_pending_approval_when_status_missing() {
        // Freshly-created MigrationPlan has no status yet —
        // CLI displays `pending-approval` as the implicit
        // initial phase (matches what MigrationController
        // would treat the empty phase as).
        let plan = json!({
            "metadata": { "name": "x" },
            "spec": {
                "scope": { "type": "platform" },
                "risks": { "classification": "breaking" }
            }
        });
        let row = plan_row(&plan);
        assert_eq!(row.phase, "pending-approval");
    }

    #[test]
    fn plan_row_extracts_all_columns() {
        let plan = json!({
            "metadata": { "name": "platform-0-1-35-to-0-1-36" },
            "spec": {
                "scope": { "type": "platform" },
                "risks": { "classification": "breaking" }
            },
            "status": { "phase": "rejected" }
        });
        let row = plan_row(&plan);
        assert_eq!(row.name, "platform-0-1-35-to-0-1-36");
        assert_eq!(row.scope, "platform");
        assert_eq!(row.classification, "breaking");
        assert_eq!(row.phase, "rejected");
    }
}
