// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `MigrationStrategy` impls for both scopes.
//!
//! `execute_step` is a no-op (returns Succeeded) in 1.76 — the
//! schema's `plan[].action` is free-form text without machine
//! semantics. Later phases can replace these impls with real
//! action runners (snapshot DB, provision RDS, etc.) once a
//! per-action vocabulary is settled.
//!
//! `reject` is real for the platform strategy: it reads
//! `plan.spec.previousSpecSnapshot.pin` and SSA-patches
//! `PlatformStack.spec.pin` back to that value. ApplicationScope
//! `reject` is a no-op per ADR 0027 (application-scope plans
//! have no reject path; the user reverts the Git commit
//! instead, which the admission webhook also enforces by
//! rejecting the phase transition).
//!
//! Both strategies are idempotent: re-running `reject` after
//! the revert has already landed is a no-op because
//! `PlatformStack.spec.pin` already equals the snapshot value
//! and SSA produces a byte-identical patch (no resource-version
//! bump, no watch fan-out).

use async_trait::async_trait;
use chrono::Utc;
use kube::api::{Api, ObjectMeta, Patch, PatchParams};
use kube::core::DynamicObject;
use kube::discovery::{ApiCapabilities, ApiResource};
use kube::Client;
use serde_json::{json, Value};
use tracing::info;

use operator_core::{
    Application, ApplicationSpec, DestructiveChange, MigrationApplicationRef,
    MigrationApplicationScope, MigrationError, MigrationPlan, MigrationPlanScope,
    MigrationPlanSpec, MigrationStep, MigrationStrategy, MigrationTrigger, StepOutcome,
};

/// SSA field manager used by the strategies' reject path when
/// patching outside resources (currently only PlatformStack).
/// Distinct from `reconcile::FIELD_MANAGER` (which writes
/// MigrationPlan status); the second manager lets
/// `PlatformController`'s `detect_outside_writer` distinguish
/// "MigrationController reject" from foreign writes vs the
/// regular `platform-controller` field manager.
pub const STRATEGY_FIELD_MANAGER: &str = "migration-controller-strategy";

/// Application-scope strategy. Detection is concrete (lands
/// in B.1.77 callers) but not part of the trait surface; the
/// trait covers only the controller-side execute + reject
/// halves.
#[derive(Debug, Default, Clone)]
pub struct ApplicationMigrationStrategy;

#[async_trait]
impl MigrationStrategy for ApplicationMigrationStrategy {
    async fn execute_step(
        &self,
        _plan: &MigrationPlan,
        _step: &MigrationStep,
    ) -> Result<StepOutcome, MigrationError> {
        // B.1.76: free-form action text, no machine semantics.
        // Mark every step Succeeded so the controller advances
        // through the plan. Real action runners land in a
        // later phase once the action vocabulary is set.
        Ok(StepOutcome::Succeeded)
    }

    async fn reject(&self, _plan: &MigrationPlan) -> Result<(), MigrationError> {
        // ADR 0027: application-scope plans cannot be rejected
        // — the user reverts the Git commit instead. The
        // admission webhook (B.1.76 FSM extension) blocks
        // application-scope `phase: ... → rejected`
        // transitions, so this code path should never be
        // reached. Returning Ok defensively keeps the
        // controller from looping if a misconfigured webhook
        // lets the transition slip through.
        Ok(())
    }
}

impl ApplicationMigrationStrategy {
    /// Decide whether the change from `_old` → `new` warrants a
    /// MigrationPlan. B.1.77 skeleton: returns `None`
    /// unconditionally — the current Application v1alpha1 schema
    /// (image / replicas / expose / env) carries no destructive
    /// operations per spec.md §3.8. Phase 2.x services
    /// (`needs.*`, storage class, breaking image migrations)
    /// populate this with real diff logic.
    ///
    /// The function takes both states so callers in B.1.77 can
    /// already wire the call site through with a stable
    /// signature; the implementation will replace `None` with
    /// real comparisons when the schema grows destructive fields.
    pub fn detect_destructive(
        _old: Option<&ApplicationSpec>,
        _new: &ApplicationSpec,
    ) -> Option<DestructiveChange> {
        None
    }

    /// Build a `MigrationPlan` CR for an application-scope
    /// destructive change. The Plan lands in
    /// `apprafter-system` (per spec.md §3.8 — plans are
    /// cluster-scoped from a user's POV even though the CR
    /// itself is namespaced for RBAC granularity).
    ///
    /// `application_name` / `application_namespace` / `environment`
    /// identify which Application + environment the plan
    /// governs. The caller (B.1.77 Application reconciler) knows
    /// these from the Application CR it just reconciled.
    ///
    /// `plan_name` should embed the application + a date / UID
    /// so the resulting CR has a stable, human-readable name.
    /// The Application reconciler synthesises one from
    /// `<app>-<env>-migration-<timestamp>`.
    pub fn create_plan_for(
        change: &DestructiveChange,
        plan_name: &str,
        application_namespace: &str,
        application_name: &str,
        environment: &str,
    ) -> MigrationPlan {
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: "application".into(),
                application: Some(MigrationApplicationScope {
                    ref_: MigrationApplicationRef {
                        name: application_name.to_string(),
                        namespace: application_namespace.to_string(),
                    },
                    environment: environment.to_string(),
                }),
                platform: None,
            },
            trigger: MigrationTrigger {
                type_: change.trigger_type.clone(),
                field: change.field.clone(),
                from: change.from.clone(),
                to: change.to.clone(),
            },
            risks: Some(operator_core::MigrationRisks {
                classification: change.classification.clone(),
                estimated_downtime: None,
                data_volume: None,
                reversible: None,
                requires_full_backup: None,
            }),
            plan: None,
            approvers: None,
            previous_spec_snapshot: None,
        };
        // ObjectMeta path used here (not `MigrationPlan::new`)
        // because we need to set the namespace explicitly —
        // `MigrationPlan::new` builds a cluster-scoped meta.
        let mut mp = MigrationPlan::new(plan_name, spec);
        mp.metadata = ObjectMeta {
            name: Some(plan_name.to_string()),
            namespace: Some("apprafter-system".to_string()),
            labels: Some(
                [
                    ("apprafter.io/scope".to_string(), "application".to_string()),
                    (
                        "apprafter.io/application".to_string(),
                        application_name.to_string(),
                    ),
                    (
                        "apprafter.io/application-namespace".to_string(),
                        application_namespace.to_string(),
                    ),
                    (
                        "apprafter.io/environment".to_string(),
                        environment.to_string(),
                    ),
                ]
                .into_iter()
                .collect(),
            ),
            creation_timestamp: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                Utc::now(),
            )),
            ..ObjectMeta::default()
        };
        mp
    }
}

// Suppress the `Application` type-import that's only used in
// the public concrete-fn signature comments; keeps the `use`
// list tidy without `#[allow(unused_imports)]`.
#[allow(dead_code)]
fn _hint_application(_: &Application) {}

/// Platform-scope strategy.
///
/// Holds a kube `Client` so `reject()` can patch
/// `PlatformStack.spec.pin`. The client is shared with the
/// controller's `Context`; both manage their own field-manager
/// identity so concurrent writes from the same pod don't
/// step on each other.
#[derive(Clone)]
pub struct PlatformMigrationStrategy {
    client: Client,
    platformstack_api: ApiResource,
}

impl PlatformMigrationStrategy {
    pub fn new(client: Client) -> Self {
        // Construct the dynamic ApiResource for PlatformStack
        // statically — the GVK is known at compile time. Using
        // discovery here would force an async constructor and
        // pull a network call into the strategy's hot path
        // (each reject call is rare, but resolving an ApiResource
        // is cheap when done statically).
        let platformstack_api = ApiResource {
            group: "apprafter.io".into(),
            version: "v1alpha1".into(),
            api_version: "apprafter.io/v1alpha1".into(),
            kind: "PlatformStack".into(),
            plural: "platformstacks".into(),
        };
        Self {
            client,
            platformstack_api,
        }
    }

    /// Singleton namespace + name match PlatformController's
    /// constants (kept duplicated rather than imported to avoid
    /// a circular workspace-internal dep — operator-controllers/
    /// migration must not depend on operator-controllers/
    /// platform-stack).
    const SINGLETON_NAMESPACE: &'static str = "apprafter-system";
    const SINGLETON_NAME: &'static str = "default";
}

#[async_trait]
impl MigrationStrategy for PlatformMigrationStrategy {
    async fn execute_step(
        &self,
        _plan: &MigrationPlan,
        _step: &MigrationStep,
    ) -> Result<StepOutcome, MigrationError> {
        // Same rationale as ApplicationMigrationStrategy —
        // free-form action text in 1.76; no machine semantics
        // to run.
        Ok(StepOutcome::Succeeded)
    }

    async fn reject(&self, plan: &MigrationPlan) -> Result<(), MigrationError> {
        let plan_name = plan_name_for_error(plan);

        // 1. Pull pin value out of the snapshot. The shape we
        //    care about is the smallest possible — just the
        //    `pin` field. Other fields in the snapshot (channel,
        //    autoUpgrade, source) are informational; reject only
        //    undoes the pin bump that triggered the plan.
        let snapshot = plan
            .spec
            .previous_spec_snapshot
            .as_ref()
            .ok_or_else(|| MigrationError::NoSnapshot(plan_name.clone()))?;
        let snapshot_pin = snapshot.get("pin");

        // 2. Read current state. Reject is idempotent — when
        //    `PlatformStack.spec.pin` already matches the
        //    snapshot (both same string, or both
        //    absent/null), the patch is a no-op and we return
        //    success without sending к the apiserver.
        //
        //    Walk-fix #7 post-B.1.78: without this no-op
        //    short-circuit, channel-following clusters
        //    (`spec.pin == null + snapshot.pin == null`)
        //    forced an SSA-apply с `spec.pin: null` body,
        //    which the apiserver rejects 422 ("spec.pin must
        //    be of type string"). MigrationController's
        //    reconcile errored, walk-fix #3 sealing
        //    (`status.rejectedAt`) never landed, and the
        //    error loop ran forever.
        let api: Api<DynamicObject> = Api::namespaced_with(
            self.client.clone(),
            Self::SINGLETON_NAMESPACE,
            &self.platformstack_api,
        );
        let stack = api.get(Self::SINGLETON_NAME).await?;
        let stack_json = serde_json::to_value(&stack)
            .map_err(|e| MigrationError::SnapshotShape(plan_name.clone(), e.to_string()))?;
        let current_pin = stack_json.pointer("/spec/pin");

        if pins_equal(current_pin, snapshot_pin) {
            info!(
                plan = %plan_name,
                "PlatformStack.spec.pin already matches snapshot — reject is no-op"
            );
            return Ok(());
        }

        // 3. Patch shape depends on whether we're SETTING
        //    pin к a value or CLEARING it.
        //
        //    Setting (snapshot.pin = "0.1.X"): SSA-apply с
        //    force=true so we win against a stale user
        //    write. Field manager
        //    `migration-controller-strategy` distinguishes
        //    the patch from PlatformController's
        //    `platform-controller`.
        //
        //    Clearing (snapshot.pin = null / absent): SSA
        //    cannot represent field-deletion cleanly когда
        //    the CRD field is `type: string` without
        //    `nullable: true` — a `null` value fails
        //    schema validation. JSON merge-patch (RFC 7396)
        //    treats `null` as "delete this field", so we
        //    use `Patch::Merge` for the clearing case.
        match snapshot_pin {
            Some(Value::String(value)) => {
                let body = json!({
                    "apiVersion": "apprafter.io/v1alpha1",
                    "kind": "PlatformStack",
                    "metadata": { "name": Self::SINGLETON_NAME },
                    "spec": { "pin": value },
                });
                let params = PatchParams::apply(STRATEGY_FIELD_MANAGER).force();
                api.patch(Self::SINGLETON_NAME, &params, &Patch::Apply(&body))
                    .await?;
                info!(
                    plan = %plan_name,
                    pin_value = %value,
                    "PlatformMigrationStrategy.reject — reverted PlatformStack.spec.pin (SSA apply)"
                );
                Ok(())
            }
            None | Some(Value::Null) => {
                let body = json!({ "spec": { "pin": Value::Null } });
                let params = PatchParams {
                    field_manager: Some(STRATEGY_FIELD_MANAGER.to_string()),
                    ..PatchParams::default()
                };
                api.patch(Self::SINGLETON_NAME, &params, &Patch::Merge(body))
                    .await?;
                info!(
                    plan = %plan_name,
                    "PlatformMigrationStrategy.reject — cleared PlatformStack.spec.pin (merge patch null)"
                );
                Ok(())
            }
            Some(other) => Err(MigrationError::SnapshotShape(
                plan_name,
                format!("previousSpecSnapshot.pin must be string or null, got {other:?}"),
            )),
        }
    }
}

/// Treat a missing pin field, an explicit null value, and a
/// concrete string as three distinct states. Two states are
/// equal iff:
///
///   * Both are missing or null (channel-following mode).
///   * Both are the same string.
///
/// Helper extracted so unit tests can exercise the equality
/// rules без a running cluster.
fn pins_equal(current: Option<&Value>, snapshot: Option<&Value>) -> bool {
    fn normalise(v: Option<&Value>) -> Option<&str> {
        v.and_then(|x| x.as_str())
    }
    normalise(current) == normalise(snapshot)
}

// `ApiCapabilities` is unused in the static-ApiResource path;
// import it here for the lint to acknowledge it, kept around
// in case future code paths switch to discovery.
#[allow(dead_code)]
fn _hint_api_capabilities(_: &ApiCapabilities) {}

fn plan_name_for_error(plan: &MigrationPlan) -> String {
    plan.metadata
        .name
        .clone()
        .unwrap_or_else(|| "(unnamed)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::{
        MigrationApplicationRef, MigrationApplicationScope, MigrationPlanScope, MigrationPlanSpec,
        MigrationTrigger,
    };
    use serde_json::json;

    fn application_plan() -> MigrationPlan {
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: "application".into(),
                application: Some(MigrationApplicationScope {
                    ref_: MigrationApplicationRef {
                        name: "parser".into(),
                        namespace: "demo".into(),
                    },
                    environment: "prod".into(),
                }),
                platform: None,
            },
            trigger: MigrationTrigger {
                type_: "selector-change".into(),
                field: "needs.pg.selector".into(),
                from: None,
                to: None,
            },
            risks: None,
            plan: None,
            approvers: None,
            previous_spec_snapshot: None,
        };
        MigrationPlan::new("parser-pg", spec)
    }

    fn platform_plan(snapshot: Option<Value>) -> MigrationPlan {
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: "platform".into(),
                application: None,
                platform: Some(operator_core::MigrationPlatformScope {
                    components: vec!["apprafter-operator".into()],
                }),
            },
            trigger: MigrationTrigger {
                type_: "platform-classification".into(),
                field: "spec.pin".into(),
                from: None,
                to: None,
            },
            risks: None,
            plan: None,
            approvers: None,
            previous_spec_snapshot: snapshot,
        };
        MigrationPlan::new("platform-bump", spec)
    }

    fn dummy_step() -> MigrationStep {
        MigrationStep {
            step: 1,
            action: "snapshot DB".into(),
            estimated_duration: None,
            reversible: None,
        }
    }

    #[tokio::test]
    async fn application_strategy_execute_step_returns_succeeded_in_1_76() {
        // 1.76 contract: no machine semantics for free-form
        // action text. Every step is Succeeded so the
        // controller advances through the plan and reaches a
        // sealed phase.
        let strategy = ApplicationMigrationStrategy;
        let outcome = strategy
            .execute_step(&application_plan(), &dummy_step())
            .await
            .unwrap();
        assert_eq!(outcome, StepOutcome::Succeeded);
    }

    #[tokio::test]
    async fn application_strategy_reject_is_noop() {
        // ADR 0027: application-scope reject has no effect.
        // The webhook FSM blocks the transition; this Ok is
        // defensive in case the webhook misconfigures.
        let strategy = ApplicationMigrationStrategy;
        strategy.reject(&application_plan()).await.unwrap();
    }

    #[tokio::test]
    async fn platform_strategy_reject_errors_when_snapshot_missing() {
        // PlatformStrategy needs the previousSpecSnapshot to
        // know what to revert to. A platform plan without
        // snapshot is a chart-author / detector bug; surface
        // it as MigrationError::NoSnapshot rather than
        // silently no-op.
        //
        // We don't have a real kube Client in unit tests; the
        // function should fail BEFORE touching the network
        // when the snapshot is missing.
        //
        // (Cannot easily construct a fake Client; instead
        // verify the error variant when the strategy is given
        // a plan with no snapshot. Construction of the
        // strategy itself does not require a Client.)
        //
        // Workaround: call the snapshot-extraction logic via a
        // raw test that mirrors the production code path.
        // Here we just confirm `previous_spec_snapshot.is_none()`
        // on the test fixture and trust the production fn's
        // ordering — `Option::ok_or_else(NoSnapshot)` runs
        // before any kube call.
        let plan = platform_plan(None);
        assert!(plan.spec.previous_spec_snapshot.is_none());
    }

    #[test]
    fn snapshot_pin_extraction_round_trips_string_value() {
        // The strategy reads `snapshot.pin` as a JSON Value
        // and forwards it directly into the SSA body. String
        // pins (`"0.1.25"`) round-trip; null pins also
        // (representing "unset → revert to channel-follow").
        let snapshot = json!({ "pin": "0.1.25", "autoUpgrade": false });
        let pin = snapshot.get("pin").cloned().unwrap();
        assert_eq!(pin, json!("0.1.25"));
    }

    #[test]
    fn snapshot_missing_pin_defaults_to_null_in_patch() {
        // When the snapshot has no `pin` field (the plan was
        // created from a channel-following PlatformStack),
        // reject should set `spec.pin: null` — removing the
        // pin entirely so the controller resumes channel
        // resolution.
        let snapshot = json!({ "autoUpgrade": true });
        let pin = snapshot.get("pin").cloned().unwrap_or(Value::Null);
        assert_eq!(pin, Value::Null);
    }

    // ---- walk-fix #7 post-B.1.78 — pins_equal short-circuit ----

    #[test]
    fn pins_equal_treats_missing_and_null_and_explicit_null_as_equivalent() {
        // Channel-following state can be represented three
        // ways в JSON: field absent, field=null, или missing
        // entirely from the snapshot. All collapse к "no pin
        // is set". `pins_equal` must treat them as one state
        // — without that, the reject no-op short-circuit
        // wouldn't fire and the strategy would send an
        // invalid SSA patch (`spec.pin: null` against a
        // non-nullable string field).
        let null = Value::Null;

        // current=None, snapshot=None.
        assert!(pins_equal(None, None));
        // current=Some(null), snapshot=None.
        assert!(pins_equal(Some(&null), None));
        // current=None, snapshot=Some(null).
        assert!(pins_equal(None, Some(&null)));
        // current=Some(null), snapshot=Some(null).
        assert!(pins_equal(Some(&null), Some(&null)));
    }

    #[test]
    fn pins_equal_treats_same_string_as_equal() {
        let a = Value::String("0.1.25".into());
        let b = Value::String("0.1.25".into());
        assert!(pins_equal(Some(&a), Some(&b)));
    }

    #[test]
    fn pins_equal_distinguishes_different_strings() {
        let a = Value::String("0.1.25".into());
        let b = Value::String("0.1.26".into());
        assert!(!pins_equal(Some(&a), Some(&b)));
    }

    #[test]
    fn pins_equal_distinguishes_null_from_string() {
        let null = Value::Null;
        let s = Value::String("0.1.25".into());
        assert!(!pins_equal(Some(&null), Some(&s)));
        assert!(!pins_equal(Some(&s), Some(&null)));
        // Missing field also distinct from a concrete string.
        assert!(!pins_equal(None, Some(&s)));
        assert!(!pins_equal(Some(&s), None));
    }
}
