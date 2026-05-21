// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Main reconcile loop for `PlatformStack/default`.
//!
//! The controller watches the singleton `PlatformStack` CR in
//! `apprafter-system`, computes the desired
//! `Application.spec.source.{targetRevision, helm.valuesObject}`,
//! and SSA-patches the parent `platform` Argo CD Application in
//! the `argocd` namespace with field manager
//! `platform-controller`. Argo CD propagates the change to
//! children via its own reconcile cycle.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use kube::api::{Api, ApiResource, DynamicObject, Patch, PatchParams};
use kube::core::GroupVersionKind;
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Client, ResourceExt};
use serde_json::{json, Value};
use thiserror::Error;
use tracing::{info, warn};

use operator_core::{Metrics, PlatformStack, PlatformStackStatus};

use crate::compatibility::{fetch_change_class, ChangeClass};
use crate::desired::{build as build_desired, DesiredSource};
use crate::oci::{latest_in_channel, Channel};
use crate::policy::{NoOpHooks, PolicyHooks};
use crate::status::{
    condition, upsert_condition, COND_MIGRATION_PENDING, COND_SYNCED,
    COND_UNAUTHORIZED_SOURCE_MODIFICATION, COND_UPGRADE_AVAILABLE,
};
use crate::{FIELD_MANAGER, SINGLETON_NAME, SINGLETON_NAMESPACE};

const PARENT_APPLICATION_NAME: &str = "platform";
const PARENT_APPLICATION_NAMESPACE: &str = "argocd";

/// Backoff when the parent Application is mid-sync; the loop
/// re-evaluates after this delay rather than cancelling the
/// in-flight sync.
const IN_FLIGHT_REQUEUE: Duration = Duration::from_secs(30);

/// Default cadence when `spec.source.checkInterval` parsing fails.
const DEFAULT_REQUEUE: Duration = Duration::from_secs(3600);

#[derive(Debug, Error)]
pub enum Error {
    #[error("kube-rs error: {0}")]
    Kube(#[from] kube::Error),
    #[error("oci poll error: {0}")]
    Oci(#[from] crate::oci::OciError),
    #[error("compatibility fetch error: {0}")]
    Compatibility(#[from] crate::compatibility::CompatError),
    #[error("policy hook error: {0}")]
    Policy(#[from] crate::policy::PolicyError),
    #[error("serde_json error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("unparseable check interval {0:?}")]
    CheckInterval(String),
}

struct Context {
    client: Client,
    #[allow(dead_code)]
    metrics: Arc<Metrics>,
    hooks: Arc<dyn PolicyHooks>,
    app_api_resource: ApiResource,
}

pub async fn run(client: Client, metrics: Arc<Metrics>) -> Result<(), Error> {
    let stacks: Api<PlatformStack> = Api::namespaced(client.clone(), SINGLETON_NAMESPACE);
    let app_api_resource = ApiResource::from_gvk(&GroupVersionKind {
        group: "argoproj.io".into(),
        version: "v1alpha1".into(),
        kind: "Application".into(),
    });
    let ctx = Arc::new(Context {
        client,
        metrics,
        hooks: Arc::new(NoOpHooks),
        app_api_resource,
    });

    Controller::new(stacks, watcher::Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            if let Err(e) = res {
                warn!(error = %e, "PlatformController reconcile error");
            }
        })
        .await;
    Ok(())
}

async fn reconcile(stack: Arc<PlatformStack>, ctx: Arc<Context>) -> Result<Action, Error> {
    // Filter to the singleton coordinates per webhook contract.
    if stack.name_any() != SINGLETON_NAME {
        warn!(name = %stack.name_any(), "ignoring non-singleton PlatformStack");
        return Ok(Action::await_change());
    }

    let spec = &stack.spec;
    let prior_conds = stack
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();

    // 1. Resolve desired version (pin OR channel-latest).
    let desired_version = match &spec.pin {
        Some(p) => p.clone(),
        None => {
            let channel = Channel::parse(&spec.channel).unwrap_or(Channel::Stable);
            let v = latest_in_channel(&spec.source.upstream, channel).await?;
            v.to_string()
        }
    };

    // 2. Read parent Application state via dynamic API.
    let apps: Api<DynamicObject> = Api::namespaced_with(
        ctx.client.clone(),
        PARENT_APPLICATION_NAMESPACE,
        &ctx.app_api_resource,
    );
    let parent = apps.get(PARENT_APPLICATION_NAME).await?;
    let parent_json = serde_json::to_value(&parent)?;
    let current_target = parent_json
        .pointer("/spec/source/targetRevision")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let in_flight = is_in_flight(&parent_json);

    // 3. Desired source payload.
    let desired = build_desired(spec, &desired_version);

    // 4. Decide action.
    let mut new_status: PlatformStackStatus = stack.status.clone().unwrap_or_default();
    new_status.target_version = Some(desired_version.clone());
    new_status.current_version = Some(current_target.clone());
    new_status.last_upstream_check = Some(Utc::now().to_rfc3339());
    new_status.available_version = Some(desired_version.clone());

    let needs_patch = current_target != desired.target_revision
        || values_differ(&parent_json, &desired.helm_values);

    if needs_patch && in_flight {
        info!(
            "parent Application in flight; requeuing reconcile in {:?}",
            IN_FLIGHT_REQUEUE
        );
        write_status(&stack, &ctx, new_status).await?;
        return Ok(Action::requeue(IN_FLIGHT_REQUEUE));
    }

    if needs_patch {
        // Policy gate: pin set → always allowed.
        // pin unset + autoUpgrade false → status-only update.
        let pin_set = spec.pin.is_some();
        if !pin_set && !spec.auto_upgrade {
            // Surface availability without bumping.
            let cond = condition(
                COND_UPGRADE_AVAILABLE,
                "True",
                "ManualApprovalRequired",
                &format!(
                    "upstream has {desired_version} (channel {ch}) but autoUpgrade=false",
                    ch = spec.channel
                ),
                &prior_conds,
            );
            upsert_condition(&mut new_status, cond);
            write_status(&stack, &ctx, new_status).await?;
            return Ok(Action::requeue(parse_check_interval(
                &spec.source.check_interval,
            )));
        }

        // Pin OR autoUpgrade=true → classify diff.
        let class =
            fetch_change_class(&spec.source.upstream, &desired.target_revision).await?;
        if matches!(class, ChangeClass::Breaking | ChangeClass::DataMigration) {
            // Defer to MigrationPlan (1.74). 1.73: push condition, no auto-bump.
            ctx.hooks
                .request_migration_plan(&current_target, &desired.target_revision, class)
                .await?;
            let cond = condition(
                COND_MIGRATION_PENDING,
                "True",
                format!("{class:?}").as_str(),
                &format!(
                    "change from {current_target} → {desired_version} classified as {class:?}; manual approval required (1.74 MigrationPlan)"
                ),
                &prior_conds,
            );
            upsert_condition(&mut new_status, cond);
            write_status(&stack, &ctx, new_status).await?;
            return Ok(Action::requeue(parse_check_interval(
                &spec.source.check_interval,
            )));
        }

        // Safe / requires-restart → SSA patch parent.
        patch_application(&apps, &desired, false).await?;
        let cond = condition(
            COND_SYNCED,
            "True",
            "Patched",
            &format!("targetRevision → {desired_version}"),
            &prior_conds,
        );
        upsert_condition(&mut new_status, cond);
    } else {
        // No diff to apply.
        let cond = condition(
            COND_SYNCED,
            "True",
            "Reconciled",
            "Application.spec.source matches PlatformStack",
            &prior_conds,
        );
        upsert_condition(&mut new_status, cond);
    }

    // 5. Outside-writer detection: any non-platform-controller
    //    + non-argocd field manager owning
    //    `f:spec.f:source.f:targetRevision` (or f:helm) on the
    //    parent is a violation.
    if let Some(foreign) = detect_outside_writer(&parent_json) {
        // Force-revert with our field manager.
        patch_application(&apps, &desired, true).await?;
        let cond = condition(
            COND_UNAUTHORIZED_SOURCE_MODIFICATION,
            "True",
            "ForeignFieldManager",
            &format!("detected external write by field manager {foreign:?}; reverted"),
            &prior_conds,
        );
        upsert_condition(&mut new_status, cond);
    } else {
        let cond = condition(
            COND_UNAUTHORIZED_SOURCE_MODIFICATION,
            "False",
            "Clean",
            "no foreign writer detected on spec.source",
            &prior_conds,
        );
        upsert_condition(&mut new_status, cond);
    }

    // 6. Status write + requeue on cadence.
    write_status(&stack, &ctx, new_status).await?;
    Ok(Action::requeue(parse_check_interval(
        &spec.source.check_interval,
    )))
}

fn is_in_flight(parent: &Value) -> bool {
    let sync = parent
        .pointer("/status/sync/status")
        .and_then(Value::as_str)
        .unwrap_or("");
    let phase = parent
        .pointer("/status/operationState/phase")
        .and_then(Value::as_str)
        .unwrap_or("");
    sync == "OutOfSync" || phase == "Running"
}

fn values_differ(parent: &Value, desired: &Value) -> bool {
    let current = parent
        .pointer("/spec/source/helm/valuesObject")
        .cloned()
        .unwrap_or(Value::Null);
    &current != desired
}

fn detect_outside_writer(parent: &Value) -> Option<String> {
    let entries = parent
        .pointer("/metadata/managedFields")
        .and_then(Value::as_array)?;
    for entry in entries {
        let manager = entry.get("manager").and_then(Value::as_str)?;
        if manager == FIELD_MANAGER || manager == "argocd-application-controller" {
            continue;
        }
        let fields = entry
            .get("fieldsV1")
            .and_then(|v| v.get("f:spec"))
            .and_then(|s| s.get("f:source"));
        if fields.is_some_and(|s| {
            s.get("f:targetRevision").is_some() || s.get("f:helm").is_some()
        }) {
            return Some(manager.to_string());
        }
    }
    None
}

async fn patch_application(
    apps: &Api<DynamicObject>,
    desired: &DesiredSource,
    force: bool,
) -> Result<(), Error> {
    let payload = json!({
        "apiVersion": "argoproj.io/v1alpha1",
        "kind": "Application",
        "metadata": { "name": PARENT_APPLICATION_NAME },
        "spec": {
            "source": {
                "targetRevision": desired.target_revision,
                "helm": { "valuesObject": desired.helm_values },
            }
        }
    });
    let mut params = PatchParams::apply(FIELD_MANAGER);
    if force {
        params = params.force();
    }
    apps.patch(PARENT_APPLICATION_NAME, &params, &Patch::Apply(&payload))
        .await?;
    Ok(())
}

async fn write_status(
    stack: &PlatformStack,
    ctx: &Context,
    new_status: PlatformStackStatus,
) -> Result<(), Error> {
    let api: Api<PlatformStack> = Api::namespaced(ctx.client.clone(), SINGLETON_NAMESPACE);
    let patch = json!({ "status": new_status });
    api.patch_status(
        &stack.name_any(),
        &PatchParams::apply(FIELD_MANAGER),
        &Patch::Apply(&patch),
    )
    .await?;
    Ok(())
}

fn parse_check_interval(s: &str) -> Duration {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return DEFAULT_REQUEUE;
    }
    let (digits, unit) = bytes.split_at(bytes.len() - 1);
    let Ok(num_str) = std::str::from_utf8(digits) else {
        return DEFAULT_REQUEUE;
    };
    let Ok(value) = num_str.parse::<u64>() else {
        return DEFAULT_REQUEUE;
    };
    let secs = match unit[0] {
        b's' => value,
        b'm' => value * 60,
        b'h' => value * 3600,
        _ => return DEFAULT_REQUEUE,
    };
    Duration::from_secs(secs)
}

fn error_policy(_: Arc<PlatformStack>, err: &Error, _: Arc<Context>) -> Action {
    warn!(error = %err, "PlatformController reconcile failed");
    Action::requeue(Duration::from_secs(60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_check_interval_with_h_m_s_units() {
        assert_eq!(parse_check_interval("6h"), Duration::from_secs(6 * 3600));
        assert_eq!(parse_check_interval("30m"), Duration::from_secs(30 * 60));
        assert_eq!(parse_check_interval("3600s"), Duration::from_secs(3600));
    }

    #[test]
    fn parses_check_interval_defaults_on_garbage() {
        assert_eq!(parse_check_interval(""), DEFAULT_REQUEUE);
        assert_eq!(parse_check_interval("abc"), DEFAULT_REQUEUE);
        assert_eq!(parse_check_interval("10x"), DEFAULT_REQUEUE);
    }

    #[test]
    fn is_in_flight_detects_progressing_phase() {
        let parent = json!({
            "status": {
                "sync": { "status": "Synced" },
                "operationState": { "phase": "Running" }
            }
        });
        assert!(is_in_flight(&parent));
    }

    #[test]
    fn is_in_flight_detects_outofsync() {
        let parent = json!({
            "status": { "sync": { "status": "OutOfSync" } }
        });
        assert!(is_in_flight(&parent));
    }

    #[test]
    fn is_in_flight_false_for_synced_succeeded() {
        let parent = json!({
            "status": {
                "sync": { "status": "Synced" },
                "operationState": { "phase": "Succeeded" }
            }
        });
        assert!(!is_in_flight(&parent));
    }

    #[test]
    fn values_differ_returns_false_when_equal() {
        let parent = json!({
            "spec": { "source": { "helm": { "valuesObject": {"tier": 1} } } }
        });
        let desired = json!({"tier": 1});
        assert!(!values_differ(&parent, &desired));
    }

    #[test]
    fn values_differ_returns_true_when_changed() {
        let parent = json!({
            "spec": { "source": { "helm": { "valuesObject": {"tier": 1} } } }
        });
        let desired = json!({"tier": 2});
        assert!(values_differ(&parent, &desired));
    }

    #[test]
    fn detect_outside_writer_skips_known_managers() {
        let parent = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "platform-controller",
                        "fieldsV1": {"f:spec": {"f:source": {"f:targetRevision": {}}}}
                    },
                    {
                        "manager": "argocd-application-controller",
                        "fieldsV1": {"f:status": {}}
                    }
                ]
            }
        });
        assert!(detect_outside_writer(&parent).is_none());
    }

    #[test]
    fn detect_outside_writer_flags_unknown_manager_on_source() {
        let parent = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "kubectl-client-side-apply",
                        "fieldsV1": {"f:spec": {"f:source": {"f:targetRevision": {}}}}
                    }
                ]
            }
        });
        assert_eq!(
            detect_outside_writer(&parent),
            Some("kubectl-client-side-apply".to_string())
        );
    }

    #[test]
    fn detect_outside_writer_flags_unknown_manager_on_helm_values() {
        let parent = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "helm",
                        "fieldsV1": {"f:spec": {"f:source": {"f:helm": {}}}}
                    }
                ]
            }
        });
        assert_eq!(detect_outside_writer(&parent), Some("helm".to_string()));
    }

    #[test]
    fn detect_outside_writer_ignores_unknown_manager_on_unrelated_fields() {
        let parent = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "kubectl-edit",
                        "fieldsV1": {"f:metadata": {"f:annotations": {}}}
                    }
                ]
            }
        });
        assert!(detect_outside_writer(&parent).is_none());
    }
}
