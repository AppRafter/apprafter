// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter migration …` thin wrappers. Track B.1.79 / 2.16b.
//!
//! `list` reads MigrationPlans across **all namespaces** (`-A`)
//! and formats them as a table — platform-scope plans live in
//! `apprafter-system`, app-scope plans (2.16b) live in the
//! Application's own namespace. `approve` resolves the plan's
//! namespace (explicit `-n` wins, else searched from the `-A`
//! listing) and patches `status.phase=approved`. `reject` stays
//! **platform-only** (`apprafter-system`): app-scope plans are
//! rejected by reverting the change in Git, not via this command
//! (approve-only, spec.md §3.8).

use cli_core::{CliError, Result};
use serde_json::Value;
use tabled::{Table, Tabled};

use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_get_json_cluster_wide, kubectl_merge_patch,
};

const MIGRATION_PLAN_NAMESPACE: &str = "apprafter-system";

#[derive(Tabled)]
struct PlanRow {
    #[tabled(rename = "NAMESPACE")]
    namespace: String,
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
    // 2.16b: app-scope plans live in the Application's namespace,
    // platform-scope plans in apprafter-system — list every ns.
    let json = kubectl_get_json_cluster_wide("migrationplan", None, kc.path())?;
    let items = json
        .as_ref()
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if items.is_empty() {
        println!("No MigrationPlans in any namespace.");
        return Ok(());
    }

    let rows: Vec<PlanRow> = items.iter().map(plan_row).collect();
    println!("{}", Table::new(&rows));
    Ok(())
}

/// Extract `(name, namespace)` from a MigrationPlan JSON object.
fn plan_name_ns(plan: &Value) -> (String, String) {
    let name = plan
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let namespace = plan
        .pointer("/metadata/namespace")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    (name, namespace)
}

fn plan_row(plan: &Value) -> PlanRow {
    let (name, namespace) = plan_name_ns(plan);
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
        namespace,
        name,
        scope,
        classification,
        phase,
    }
}

/// Resolve which namespace a MigrationPlan `name` lives in.
///
/// * An explicit `-n/--namespace` (`explicit`) always wins — the
///   caller asked for a specific namespace, so we honour it even
///   if the name isn't in the discovered listing (the plan may
///   have appeared between the list and the patch).
/// * Otherwise the `(name, namespace)` pairs from the `-A`
///   listing are searched:
///   * exactly one match → that namespace,
///   * zero matches → an actionable "not found" error,
///   * 2+ matches (same name in different namespaces) → an error
///     that lists the candidate namespaces and asks for `-n`.
///
/// Pure (no I/O) so it is unit-testable without a cluster.
fn resolve_plan_ns(
    explicit: Option<&str>,
    name: &str,
    plans: &[(String, String)],
) -> Result<String> {
    if let Some(ns) = explicit {
        return Ok(ns.to_string());
    }
    let matches: Vec<&str> = plans
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, ns)| ns.as_str())
        .collect();
    match matches.as_slice() {
        [] => Err(CliError::Other(format!(
            "no MigrationPlan named '{name}' found in any namespace; \
             run `apprafter migration list` to see available plans"
        ))),
        [ns] => Ok((*ns).to_string()),
        many => Err(CliError::Other(format!(
            "MigrationPlan '{name}' exists in multiple namespaces ({}); \
             disambiguate with `apprafter migration approve {name} -n <namespace>`",
            many.join(", ")
        ))),
    }
}

/// Collect `(name, namespace)` pairs for every MigrationPlan
/// across all namespaces. Used by `approve` to resolve the
/// namespace of a plan referenced by bare name.
fn list_plan_name_ns(kubeconfig_path: &std::path::Path) -> Result<Vec<(String, String)>> {
    let json = kubectl_get_json_cluster_wide("migrationplan", None, kubeconfig_path)?;
    let items = json
        .as_ref()
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(items.iter().map(plan_name_ns).collect())
}

pub fn approve(name: &str, namespace: Option<&str>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    // 2.16b: a plan named on the CLI may be platform-scope
    // (apprafter-system) or app-scope (the app's namespace).
    // Explicit `-n` wins; else find it in the cluster-wide list.
    let ns = if namespace.is_some() {
        resolve_plan_ns(namespace, name, &[])?
    } else {
        let plans = list_plan_name_ns(kc.path())?;
        resolve_plan_ns(None, name, &plans)?
    };
    kubectl_merge_patch(
        "migrationplan",
        name,
        Some(&ns),
        Some("status"),
        r#"{"status":{"phase":"approved"}}"#,
        kc.path(),
    )?;
    println!("Approved MigrationPlan {ns}/{name}.");
    println!(
        "MigrationController will transition through executing → completed; the \
         PlatformController's next reconcile sees completed and proceeds with the bump."
    );
    Ok(())
}

pub fn reject(name: &str) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    // 2.16b: `reject` targets platform plans only (apprafter-system).
    // App-scope plans are rejected by reverting the change in Git,
    // not via this command (approve-only, spec.md §3.8). If the
    // named plan isn't in apprafter-system but DOES exist in an
    // app namespace, guide the user to Git instead of a confusing
    // apiserver NotFound.
    let plans = list_plan_name_ns(kc.path())?;
    let in_platform_ns = plans
        .iter()
        .any(|(n, ns)| n == name && ns == MIGRATION_PLAN_NAMESPACE);
    let app_namespaces: Vec<&str> = plans
        .iter()
        .filter(|(n, ns)| n == name && ns != MIGRATION_PLAN_NAMESPACE)
        .map(|(_, ns)| ns.as_str())
        .collect();
    if !in_platform_ns && !app_namespaces.is_empty() {
        return Err(CliError::Other(format!(
            "MigrationPlan '{name}' is app-scope (namespace: {}); app-scope plans are \
             rejected by reverting the change in Git, not via this command",
            app_namespaces.join(", ")
        )));
    }

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
            "metadata": { "name": "platform-0-1-35-to-0-1-36", "namespace": "apprafter-system" },
            "spec": {
                "scope": { "type": "platform" },
                "risks": { "classification": "breaking" }
            },
            "status": { "phase": "rejected" }
        });
        let row = plan_row(&plan);
        assert_eq!(row.namespace, "apprafter-system");
        assert_eq!(row.name, "platform-0-1-35-to-0-1-36");
        assert_eq!(row.scope, "platform");
        assert_eq!(row.classification, "breaking");
        assert_eq!(row.phase, "rejected");
    }

    #[test]
    fn plan_row_extracts_app_scope_namespace() {
        // 2.16b: app-scope plans live in the app's namespace.
        let plan = json!({
            "metadata": { "name": "app-web-0-1-to-0-2", "namespace": "team-a" },
            "spec": {
                "scope": { "type": "application" },
                "risks": { "classification": "additive" }
            },
            "status": { "phase": "pending-approval" }
        });
        let row = plan_row(&plan);
        assert_eq!(row.namespace, "team-a");
        assert_eq!(row.scope, "application");
    }

    #[test]
    fn plan_row_renders_sourcecredential_scope_without_hard_assuming_application() {
        // 2.16b-sc: a sourcecredential-scope plan carries
        // `scope.type="sourcecredential"` + `scope.sourcecredential.ref`
        // and NO `scope.application` / `scope.environment`. The SCOPE
        // column reads `.spec.scope.type` generically, so it renders
        // `sourcecredential` and never touches the absent app fields —
        // no panic, no empty misrender. The plan lives in the
        // SourceCredential's namespace (apprafter-system).
        let plan = json!({
            "metadata": { "name": "srccred-acme-narrow-abc123", "namespace": "apprafter-system" },
            "spec": {
                "scope": {
                    "type": "sourcecredential",
                    "sourcecredential": {
                        "ref": { "name": "acme", "namespace": "apprafter-system" }
                    }
                },
                "risks": { "classification": "security-boundary" }
            },
            "status": { "phase": "pending-approval" }
        });
        let row = plan_row(&plan);
        assert_eq!(row.namespace, "apprafter-system");
        assert_eq!(row.name, "srccred-acme-narrow-abc123");
        assert_eq!(row.scope, "sourcecredential");
        assert_eq!(row.classification, "security-boundary");
        assert_eq!(row.phase, "pending-approval");
    }

    #[test]
    fn resolve_plan_ns_handles_sourcecredential_scope_plan() {
        // `approve` resolves the plan's namespace purely from the
        // `(name, namespace)` listing — it never reads `.spec.scope`,
        // so a sourcecredential-scope plan (which has no
        // `scope.application`) resolves its namespace exactly like any
        // other. A sourcecredential plan lives in apprafter-system.
        let plans = vec![
            (
                "srccred-acme-narrow-abc123".to_string(),
                "apprafter-system".to_string(),
            ),
            ("app-web-0-1-to-0-2".to_string(), "team-a".to_string()),
        ];
        assert_eq!(
            resolve_plan_ns(None, "srccred-acme-narrow-abc123", &plans).unwrap(),
            "apprafter-system"
        );
    }

    #[test]
    fn resolve_plan_namespace_prefers_explicit_then_searches() {
        // explicit -n wins even if the name isn't in the list
        assert_eq!(
            resolve_plan_ns(Some("team-a"), "p1", &[]).unwrap(),
            "team-a"
        );
        // else find by name across (name, ns) pairs
        let plans = vec![
            ("p1".to_string(), "team-a".to_string()),
            ("p2".to_string(), "apprafter-system".to_string()),
        ];
        assert_eq!(
            resolve_plan_ns(None, "p2", &plans).unwrap(),
            "apprafter-system"
        );
        // not found → Err
        assert!(resolve_plan_ns(None, "nope", &plans).is_err());
        // ambiguous (same name in 2 ns) → Err asking for -n
        let dup = vec![
            ("p1".to_string(), "team-a".to_string()),
            ("p1".to_string(), "team-b".to_string()),
        ];
        assert!(resolve_plan_ns(None, "p1", &dup).is_err());
    }

    #[test]
    fn resolve_plan_ns_ambiguous_error_lists_namespaces_and_flag() {
        let dup = vec![
            ("p1".to_string(), "team-a".to_string()),
            ("p1".to_string(), "team-b".to_string()),
        ];
        let err = resolve_plan_ns(None, "p1", &dup).unwrap_err().to_string();
        // actionable: names the plan, both namespaces, and the -n flag
        assert!(err.contains("p1"), "err should name the plan: {err}");
        assert!(err.contains("team-a"), "err should list ns: {err}");
        assert!(err.contains("team-b"), "err should list ns: {err}");
        assert!(err.contains("-n"), "err should point to -n: {err}");
    }

    #[test]
    fn resolve_plan_ns_not_found_error_names_the_plan() {
        let err = resolve_plan_ns(None, "ghost", &[]).unwrap_err().to_string();
        assert!(err.contains("ghost"), "err should name the plan: {err}");
    }
}
