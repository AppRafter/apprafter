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
use semver::Version;
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

    // 1. Always query channel-latest from upstream. Two reasons:
    //    (a) it's `status.availableVersion` regardless of whether
    //        pin is set;
    //    (b) the `UpgradeAvailable` condition is a semver
    //        comparison of channel-latest against the actual
    //        deployed target, NOT against the operator's desired
    //        intent. Walk-found bug v0.1.116 → v0.1.117: the
    //        old logic conflated "values_differ" with "newer
    //        version exists" and fired UpgradeAvailable=True on
    //        first reconcile because the loader created the
    //        parent App without `helm.valuesObject` (null vs
    //        `{tier: 1}` looked like a diff).
    let channel = Channel::parse(&spec.channel).unwrap_or(Channel::Stable);
    let channel_latest = latest_in_channel(&spec.source.upstream, channel).await?;
    let channel_latest_str = channel_latest.to_string();

    // 2. Policy target — what PlatformController wants the
    //    parent's `spec.source.targetRevision` to be. Pin wins
    //    over channel-latest.
    let policy_target = match &spec.pin {
        Some(p) => p.clone(),
        None => channel_latest_str.clone(),
    };

    // 3. Read parent Application state via dynamic API.
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

    // 4. Desired SSA payload.
    let desired = build_desired(spec, &policy_target);

    let target_changed = current_target != desired.target_revision;
    let values_changed = values_differ(&parent_json, &desired.helm_values);

    // 5. Pre-build a status skeleton; conditions filled in below.
    let mut new_status: PlatformStackStatus = stack.status.clone().unwrap_or_default();
    new_status.last_upstream_check = Some(Utc::now().to_rfc3339());
    new_status.available_version = Some(channel_latest_str.clone());

    // 6. In-flight gating. We do NOT fight an in-progress sync —
    //    Argo CD's app-controller is mid-apply; another patch
    //    from us would race with that.
    if (target_changed || values_changed) && in_flight {
        info!(
            "parent Application in flight; requeuing reconcile in {:?}",
            IN_FLIGHT_REQUEUE
        );
        new_status.target_version = Some(current_target.clone());
        new_status.current_version = Some(current_target.clone());
        write_status(&stack, &ctx, new_status).await?;
        return Ok(Action::requeue(IN_FLIGHT_REQUEUE));
    }

    // 7. Decide what target_revision to put into the SSA patch.
    //
    //    `helm.valuesObject` is ALWAYS owned by PlatformController
    //    once it touches the resource — values are runtime config,
    //    not a version bump, and the pin/autoUpgrade policy does
    //    not gate them. `targetRevision` IS gated by policy.
    //
    //    If policy forbids the target change, the SSA patch still
    //    includes `targetRevision = current_target` so
    //    PlatformController takes ownership of the field without
    //    actually changing it. This lets `detect_outside_writer`
    //    catch any subsequent foreign write reliably.
    let pin_set = spec.pin.is_some();
    let allow_target_bump = pin_set || spec.auto_upgrade;

    let mut migration_pending: Option<ChangeClass> = None;
    let target_for_patch = if target_changed && allow_target_bump {
        // Pin OR autoUpgrade=true → classify diff. Safe /
        // requires-restart bump; breaking / data-migration stays
        // on current_target and surfaces MigrationPending=True.
        let class = fetch_change_class(&spec.source.upstream, &desired.target_revision).await?;
        if matches!(class, ChangeClass::Breaking | ChangeClass::DataMigration) {
            ctx.hooks
                .request_migration_plan(&current_target, &desired.target_revision, class)
                .await?;
            migration_pending = Some(class);
            current_target.clone()
        } else {
            desired.target_revision.clone()
        }
    } else {
        // Either no change needed OR policy forbids bump
        // (pin unset + autoUpgrade=false). Keep current; the
        // UpgradeAvailable condition will reflect whether a
        // newer version exists upstream.
        current_target.clone()
    };

    // 8. SSA patch parent App. Always run when ANY field changes
    //    (values OR target). On steady state (no diff) we skip the
    //    patch to avoid unnecessary churn through Argo CD.
    let patch_payload = DesiredSource {
        target_revision: target_for_patch.clone(),
        helm_values: desired.helm_values.clone(),
    };
    let patched_this_cycle =
        target_changed || values_changed || !platform_controller_owns_source(&parent_json);
    if patched_this_cycle {
        patch_application(&apps, &patch_payload, false).await?;
    }

    // 9. Outside-writer detection — any non-`platform-controller` +
    //    non-`argocd-application-controller` field manager owning
    //    `f:spec.f:source.f:targetRevision` (or f:helm) is treated
    //    as an unauthorized modification and force-reverted.
    let foreign_writer = detect_outside_writer(&parent_json);
    if let Some(foreign) = &foreign_writer {
        warn!(manager = %foreign, "foreign field manager detected; force-reverting parent App");
        patch_application(&apps, &patch_payload, true).await?;
    }

    // 10. Conditions. `Synced` reflects whether PlatformController
    //     achieved its desired state on the parent.
    //     `UpgradeAvailable` is the semver comparison of
    //     channel-latest against the deployed target —
    //     independent of values diffs and policy gates.
    let upgrade_available = semver_gt(&channel_latest_str, &target_for_patch);
    let cond_upgrade = if upgrade_available {
        condition(
            COND_UPGRADE_AVAILABLE,
            "True",
            "ManualApprovalRequired",
            &format!(
                "channel {ch} latest is {channel_latest_str}; deployed target is {target}; \
                 set spec.autoUpgrade=true or spec.pin to advance",
                ch = spec.channel,
                target = target_for_patch
            ),
            &prior_conds,
        )
    } else {
        condition(
            COND_UPGRADE_AVAILABLE,
            "False",
            "UpToDate",
            &format!(
                "deployed target {target} is the latest in channel {ch}",
                ch = spec.channel,
                target = target_for_patch
            ),
            &prior_conds,
        )
    };
    upsert_condition(&mut new_status, cond_upgrade);

    let cond_migration = match migration_pending {
        Some(class) => condition(
            COND_MIGRATION_PENDING,
            "True",
            &format!("{class:?}"),
            &format!(
                "change from {current_target} → {desired_target} classified as {class:?}; \
                 manual approval required (1.74 MigrationPlan)",
                desired_target = desired.target_revision
            ),
            &prior_conds,
        ),
        None => condition(
            COND_MIGRATION_PENDING,
            "False",
            "Clean",
            "no destructive diff pending",
            &prior_conds,
        ),
    };
    upsert_condition(&mut new_status, cond_migration);

    let cond_synced = if patched_this_cycle {
        condition(
            COND_SYNCED,
            "True",
            "Patched",
            &format!(
                "PlatformController patched parent Application (target={target_for_patch}); \
                 values_changed={values_changed}, target_changed={target_changed}"
            ),
            &prior_conds,
        )
    } else {
        condition(
            COND_SYNCED,
            "True",
            "Reconciled",
            "parent Application matches PlatformStack desired state",
            &prior_conds,
        )
    };
    upsert_condition(&mut new_status, cond_synced);

    let cond_unauthorized = match &foreign_writer {
        Some(foreign) => condition(
            COND_UNAUTHORIZED_SOURCE_MODIFICATION,
            "True",
            "ForeignFieldManager",
            &format!(
                "detected external write to spec.source by field manager {foreign:?}; \
                 PlatformController force-reverted"
            ),
            &prior_conds,
        ),
        None => condition(
            COND_UNAUTHORIZED_SOURCE_MODIFICATION,
            "False",
            "Clean",
            "no foreign writer detected on spec.source",
            &prior_conds,
        ),
    };
    upsert_condition(&mut new_status, cond_unauthorized);

    new_status.current_version = Some(target_for_patch.clone());
    new_status.target_version = Some(target_for_patch);
    write_status(&stack, &ctx, new_status).await?;
    Ok(Action::requeue(parse_check_interval(
        &spec.source.check_interval,
    )))
}

/// Strict semver comparison: returns true iff `a > b`. Falls back
/// to `false` when either side is unparseable — fail-safe (better
/// to not fire `UpgradeAvailable` than to flap on garbage).
fn semver_gt(a: &str, b: &str) -> bool {
    match (Version::parse(a), Version::parse(b)) {
        (Ok(av), Ok(bv)) => av > bv,
        _ => false,
    }
}

/// Has PlatformController already taken SSA ownership of any
/// `spec.source` field? Used to decide whether to send a no-op
/// SSA patch on the first reconcile (so future foreign writes
/// get caught by `detect_outside_writer`). Without this the
/// initial reconcile would skip the patch entirely whenever
/// `target_changed==false && values_changed==false` and never
/// register the field manager.
fn platform_controller_owns_source(parent: &Value) -> bool {
    let Some(entries) = parent
        .pointer("/metadata/managedFields")
        .and_then(Value::as_array)
    else {
        return false;
    };
    entries.iter().any(|e| {
        e.get("manager").and_then(Value::as_str) == Some(FIELD_MANAGER)
            && e.get("fieldsV1")
                .and_then(|v| v.get("f:spec"))
                .and_then(|s| s.get("f:source"))
                .is_some()
    })
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
        if fields.is_some_and(|s| s.get("f:targetRevision").is_some() || s.get("f:helm").is_some())
        {
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
    let payload = build_application_patch(desired);
    let mut params = PatchParams::apply(FIELD_MANAGER);
    if force {
        params = params.force();
    }
    apps.patch(PARENT_APPLICATION_NAME, &params, &Patch::Apply(&payload))
        .await?;
    Ok(())
}

fn build_application_patch(desired: &DesiredSource) -> Value {
    // apiVersion + kind + metadata.name are REQUIRED in every SSA
    // patch body — the apiserver uses them to resolve the target
    // resource's schema. Same TypeMeta contract that
    // `build_status_patch` enforces for PlatformStack writes.
    json!({
        "apiVersion": "argoproj.io/v1alpha1",
        "kind": "Application",
        "metadata": { "name": PARENT_APPLICATION_NAME },
        "spec": {
            "source": {
                "targetRevision": desired.target_revision,
                "helm": { "valuesObject": desired.helm_values },
            }
        }
    })
}

async fn write_status(
    stack: &PlatformStack,
    ctx: &Context,
    new_status: PlatformStackStatus,
) -> Result<(), Error> {
    let api: Api<PlatformStack> = Api::namespaced(ctx.client.clone(), SINGLETON_NAMESPACE);
    let name = stack.name_any();
    // SSA REQUIRES apiVersion + kind + metadata.name in the patch
    // body — the apiserver uses them to look up the resource's
    // OpenAPI schema before merging. Walk-found bug v0.1.115 →
    // v0.1.116: a `{"status": {...}}` patch alone hits the
    // apiserver with `invalid object type: /, Kind=` (empty
    // GroupVersion, empty Kind) and every reconcile retry loops
    // on that error. Mirror'ed from
    // `operator_controllers_application::apply_status` which has
    // always carried the TypeMeta.
    let patch = build_status_patch(&name, &new_status);
    api.patch_status(
        &name,
        &PatchParams::apply(FIELD_MANAGER),
        &Patch::Apply(&patch),
    )
    .await?;
    Ok(())
}

fn build_status_patch(name: &str, new_status: &PlatformStackStatus) -> Value {
    json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "PlatformStack",
        "metadata": { "name": name },
        "status": new_status,
    })
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

    #[test]
    fn build_status_patch_includes_apiversion_kind_and_name() {
        // Regression guard for walk-fix v0.1.115 → v0.1.116. SSA
        // requires the patch body to carry apiVersion + kind +
        // metadata.name; a bare `{"status": {...}}` body fails
        // with `invalid object type: /, Kind=` from the apiserver
        // (empty GroupVersion, empty Kind) and loops every
        // reconcile retry on the same error.
        let status = PlatformStackStatus {
            current_version: Some("0.1.17".into()),
            ..Default::default()
        };
        let patch = build_status_patch("default", &status);
        let map = patch.as_object().expect("patch is JSON object");
        assert_eq!(
            map.get("apiVersion").and_then(Value::as_str),
            Some("apprafter.io/v1alpha1")
        );
        assert_eq!(
            map.get("kind").and_then(Value::as_str),
            Some("PlatformStack")
        );
        assert_eq!(
            patch.pointer("/metadata/name").and_then(Value::as_str),
            Some("default")
        );
        assert_eq!(
            patch
                .pointer("/status/currentVersion")
                .and_then(Value::as_str),
            Some("0.1.17")
        );
    }

    #[test]
    fn semver_gt_compares_strictly_greater() {
        assert!(semver_gt("0.1.19", "0.1.18"));
        assert!(semver_gt("0.2.0", "0.1.99"));
        assert!(semver_gt("1.0.0", "0.99.99"));
    }

    #[test]
    fn semver_gt_returns_false_for_equal() {
        // Critical regression guard: v0.1.116 wrongly fired
        // UpgradeAvailable=True for equal versions (because the
        // old logic used values_differ instead of semver
        // comparison).
        assert!(!semver_gt("0.1.18", "0.1.18"));
    }

    #[test]
    fn semver_gt_returns_false_for_lesser() {
        assert!(!semver_gt("0.1.17", "0.1.18"));
        assert!(!semver_gt("0.1.0", "1.0.0"));
    }

    #[test]
    fn semver_gt_handles_prereleases() {
        // 0.2.0-rc.1 < 0.2.0 per semver precedence.
        assert!(semver_gt("0.2.0", "0.2.0-rc.1"));
        assert!(!semver_gt("0.2.0-rc.1", "0.2.0"));
    }

    #[test]
    fn semver_gt_returns_false_on_unparseable_input() {
        // Fail-safe — bogus version strings must NOT trigger
        // UpgradeAvailable=True. Prefer quiet "no upgrade" to a
        // flapping condition on garbage input.
        assert!(!semver_gt("not-a-version", "0.1.18"));
        assert!(!semver_gt("0.1.18", "garbage"));
        assert!(!semver_gt("", "0.1.18"));
    }

    #[test]
    fn platform_controller_owns_source_finds_own_manager() {
        let parent = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "platform-controller",
                        "fieldsV1": {"f:spec": {"f:source": {"f:targetRevision": {}}}}
                    }
                ]
            }
        });
        assert!(platform_controller_owns_source(&parent));
    }

    #[test]
    fn platform_controller_owns_source_false_when_only_argocd_present() {
        let parent = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "argocd-application-controller",
                        "fieldsV1": {"f:status": {}}
                    },
                    {
                        "manager": "kubectl-client-side-apply",
                        "fieldsV1": {"f:spec": {"f:source": {"f:targetRevision": {}}}}
                    }
                ]
            }
        });
        assert!(!platform_controller_owns_source(&parent));
    }

    #[test]
    fn platform_controller_owns_source_false_when_metadata_missing() {
        assert!(!platform_controller_owns_source(&json!({})));
    }

    #[test]
    fn build_application_patch_includes_apiversion_kind_name_and_source() {
        // SSA TypeMeta contract for the parent Application
        // patch — same shape requirement as status patch. Carried
        // forward from the v0.1.114 closure (TypeMeta was correct
        // here; this test pins the contract so future refactors
        // can't silently strip it the way write_status did).
        let desired = DesiredSource {
            target_revision: "0.1.17".into(),
            helm_values: json!({"tier": 1}),
        };
        let patch = build_application_patch(&desired);
        assert_eq!(
            patch.get("apiVersion").and_then(Value::as_str),
            Some("argoproj.io/v1alpha1")
        );
        assert_eq!(
            patch.get("kind").and_then(Value::as_str),
            Some("Application")
        );
        assert_eq!(
            patch.pointer("/metadata/name").and_then(Value::as_str),
            Some(PARENT_APPLICATION_NAME)
        );
        assert_eq!(
            patch
                .pointer("/spec/source/targetRevision")
                .and_then(Value::as_str),
            Some("0.1.17")
        );
        assert_eq!(
            patch
                .pointer("/spec/source/helm/valuesObject/tier")
                .and_then(Value::as_i64),
            Some(1)
        );
    }
}
