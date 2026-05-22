// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs Controller for the v1alpha1 `Application` CRD.
//!
//! v0.1.31 (sub-phase 1.9b) wires the v0.1.30 renderer into
//! `reconcile` via server-side apply with field manager
//! `apprafter-operator` and updates the Application's `status`
//! subresource (phase, observedGeneration, conditions, endpointURL).

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Service;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Client, ResourceExt};
use serde_json::{json, Value};
use thiserror::Error;
use tracing::{info, warn};

use operator_core::{
    Application, ApplicationCondition, ApplicationStatus, Metrics, MigrationPlan,
    COND_MIGRATION_PENDING, PHASE_AWAITING_MIGRATION_APPROVAL,
};
// `ApplicationMigrationStrategy` is wired through Cargo.toml
// dep so future Phase 2 commits can flip on detection +
// auto-plan-creation by editing one call site here; the
// strategy struct + its concrete `detect_destructive` /
// `create_plan_for` fns ship in
// `operator-controllers-migration` (B.1.77). The unused-import
// guard turns the dep into a "feature flag" — Phase 2 simply
// uncomments the `use` line.
use operator_rendering::render_application_for_env;

/// Resource kind label used for every metric tagged with `kind`.
const KIND: &str = "Application";

/// SSA field manager. Each apply / status-patch tags the fields it
/// owns under this name, so future controllers (e.g. operators
/// extending Applications) can co-own without conflicts.
pub const FIELD_MANAGER: &str = "apprafter-operator";

/// Per-controller reconcile context.
pub struct Context {
    pub client: Client,
    pub metrics: Arc<Metrics>,
    /// Active environment name — when `Some(...)`, reconcile
    /// applies the matching `spec.environments[env_name]` override
    /// on top of `spec.base`. Sourced from `APPRAFTER_ENV` env var
    /// in the binary; `None` falls back to `spec.base` only.
    pub env_name: Option<String>,
}

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("kube-rs error: {0}")]
    Kube(#[from] kube::Error),

    #[error("serde_json error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Spawn the Application Controller. Watches `apprafter.io/v1alpha1`
/// `Application` resources cluster-wide and reconciles them through
/// [`reconcile`]. Errors from individual reconcile calls go through
/// [`error_policy`].
pub async fn run(
    client: Client,
    metrics: Arc<Metrics>,
    env_name: Option<String>,
) -> Result<(), ReconcileError> {
    let apps: Api<Application> = Api::all(client.clone());
    let context = Arc::new(Context {
        client,
        metrics,
        env_name,
    });

    Controller::new(apps, watcher::Config::default())
        .run(reconcile, error_policy, context)
        .for_each(|res| async move {
            match res {
                Ok((obj_ref, _action)) => {
                    info!(?obj_ref, "controller step ok");
                }
                Err(err) => {
                    warn!(%err, "controller step error");
                }
            }
        })
        .await;
    Ok(())
}

/// Reconcile fn — renders the Application, applies the children
/// via SSA, updates the Application status. Returns Action::requeue
/// 60s on success.
///
/// **Pause gate (B.1.77 / ADR 0027).** Before patching any child
/// resource, the reconciler checks for an unsealed MigrationPlan
/// gating this Application + environment pair. If one exists,
/// the reconciler:
///
///   * skips Deployment / Service / HTTPRoute patches — child
///     resources keep running the previously-deployed spec;
///   * writes `status.phase = AwaitingMigrationApproval` plus
///     a `MigrationPending=True` condition naming the gating
///     plan;
///   * requeues in 30s so a phase change on the plan is picked
///     up promptly.
///
/// "Unsealed" = phase is empty / `pending-approval` / `approved`
/// / `executing` / `failed`. Plans in `completed` or `rejected`
/// no longer gate (user has either accepted the change or
/// reverted the source).
///
/// **Detection (`detect_destructive`).** Wired through
/// `ApplicationMigrationStrategy::detect_destructive` but NOT
/// invoked from this reconcile loop in B.1.77 — the current
/// v1alpha1 Application schema (image / replicas / expose /
/// env) carries no destructive operations per spec.md §3.8, so
/// detection would always return `None`. Phase 2.x services
/// (`needs.*`, storage classes, breaking image migrations)
/// extend both the schema and the detection logic; the call
/// site lands then.
pub async fn reconcile(app: Arc<Application>, ctx: Arc<Context>) -> Result<Action, ReconcileError> {
    let name = app.name_any();
    let namespace = app.namespace().unwrap_or_default();
    let _timer = ctx
        .metrics
        .reconcile_duration
        .with_label_values(&[KIND])
        .start_timer();

    info!(%name, %namespace, env = ?ctx.env_name, "reconciling Application");

    // B.1.77 pause gate. Must run BEFORE child patches —
    // otherwise we'd race the user's "I just pushed a
    // destructive change, please pause" flow.
    if let Some(plan) =
        find_blocking_migration_plan(&ctx.client, &name, &namespace, ctx.env_name.as_deref())
            .await?
    {
        let plan_name = plan.name_any();
        info!(
            %name, %namespace, plan = %plan_name,
            "MigrationPlan pending — pausing Application reconcile"
        );
        let pp = PatchParams::apply(FIELD_MANAGER).force();
        let status = build_paused_status(&app, &plan_name);
        apply_status(&ctx.client, &namespace, &name, &status, &pp).await?;
        ctx.metrics
            .reconcile_total
            .with_label_values(&[KIND, &namespace, "paused"])
            .inc();
        return Ok(Action::requeue(Duration::from_secs(30)));
    }

    let rendered = render_application_for_env(&app, ctx.env_name.as_deref());
    let pp = PatchParams::apply(FIELD_MANAGER).force();

    apply_deployment(&ctx.client, &namespace, &rendered.deployment, &pp).await?;

    if let Some(service) = &rendered.service {
        apply_service(&ctx.client, &namespace, service, &pp).await?;
    }

    let endpoint_url = rendered
        .service
        .as_ref()
        .and_then(|s| s.metadata.name.as_deref())
        .map(|svc_name| cluster_internal_endpoint_url(svc_name, &namespace, 80));

    // Preserve lastTransitionTime if the previous Ready condition
    // already had the same `status` value — per k8s convention the
    // timestamp moves only when `status` transitions. Without this,
    // each reconcile bumps the timestamp → status diff → watch
    // event on our own write → fresh reconcile → hot loop spinning
    // the operator's CPU.
    let previous_conditions = app
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[]);
    let conditions = vec![ready_condition(
        "True",
        "ReconcileSucceeded",
        "Reconcile completed; child Deployment and Service applied.",
        previous_conditions,
    )];
    let status = build_status(&app, "Ready", conditions, endpoint_url);
    apply_status(&ctx.client, &namespace, &name, &status, &pp).await?;

    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, &namespace, "ok"])
        .inc();
    Ok(Action::requeue(Duration::from_secs(60)))
}

/// Error policy — logs the error, increments the error counters,
/// and requeues with a fixed 30s delay. Phase 1.9c will distinguish
/// transient vs terminal errors and wire up exponential backoff.
pub fn error_policy(app: Arc<Application>, err: &ReconcileError, ctx: Arc<Context>) -> Action {
    let name = app.name_any();
    let namespace = app.namespace().unwrap_or_default();
    warn!(%name, %namespace, %err, "reconcile error");
    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, &namespace, "error"])
        .inc();
    ctx.metrics
        .reconcile_errors
        .with_label_values(&[KIND])
        .inc();
    Action::requeue(Duration::from_secs(30))
}

async fn apply_deployment(
    client: &Client,
    namespace: &str,
    deployment: &Deployment,
    pp: &PatchParams,
) -> Result<(), ReconcileError> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let name = deployment
        .metadata
        .name
        .as_deref()
        .unwrap_or_default()
        .to_string();
    let payload = into_apply_payload("apps/v1", "Deployment", deployment)?;
    api.patch(&name, pp, &Patch::Apply(&payload)).await?;
    Ok(())
}

async fn apply_service(
    client: &Client,
    namespace: &str,
    service: &Service,
    pp: &PatchParams,
) -> Result<(), ReconcileError> {
    let api: Api<Service> = Api::namespaced(client.clone(), namespace);
    let name = service
        .metadata
        .name
        .as_deref()
        .unwrap_or_default()
        .to_string();
    let payload = into_apply_payload("v1", "Service", service)?;
    api.patch(&name, pp, &Patch::Apply(&payload)).await?;
    Ok(())
}

async fn apply_status(
    client: &Client,
    namespace: &str,
    name: &str,
    status: &ApplicationStatus,
    pp: &PatchParams,
) -> Result<(), ReconcileError> {
    let api: Api<Application> = Api::namespaced(client.clone(), namespace);
    let payload = json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "Application",
        "metadata": { "name": name },
        "status": status,
    });
    api.patch_status(name, pp, &Patch::Apply(&payload)).await?;
    Ok(())
}

/// Serialize an arbitrary k8s-openapi type and inject the
/// `apiVersion` + `kind` fields the apiserver requires for SSA.
fn into_apply_payload<T: serde::Serialize>(
    api_version: &str,
    kind: &str,
    obj: &T,
) -> Result<Value, serde_json::Error> {
    let mut value = serde_json::to_value(obj)?;
    if let Value::Object(map) = &mut value {
        map.insert(
            "apiVersion".to_string(),
            Value::String(api_version.to_string()),
        );
        map.insert("kind".to_string(), Value::String(kind.to_string()));
    }
    Ok(value)
}

/// Cluster-internal FQDN for the rendered Service.
fn cluster_internal_endpoint_url(service: &str, namespace: &str, port: i32) -> String {
    format!("http://{service}.{namespace}.svc.cluster.local:{port}")
}

/// Build a fresh `ApplicationStatus` from the Application's
/// observed generation + reconcile result.
fn build_status(
    app: &Application,
    phase: &str,
    conditions: Vec<ApplicationCondition>,
    endpoint_url: Option<String>,
) -> ApplicationStatus {
    ApplicationStatus {
        phase: Some(phase.to_string()),
        observed_generation: app.metadata.generation,
        conditions: Some(conditions),
        endpoint_url,
    }
}

/// Namespace where MigrationPlan CRs live (see spec.md §3.8).
/// Plans are namespaced for RBAC granularity but treated as
/// cluster-scoped in user terms — the reconciler always looks
/// in this one namespace.
const MIGRATION_PLAN_NAMESPACE: &str = "apprafter-system";

/// Find an unsealed MigrationPlan gating this Application +
/// environment pair, if any. Lists all MigrationPlans in
/// `apprafter-system` and filters in-memory — the namespace
/// is small and the list is bounded.
///
/// "Unsealed" = phase is missing OR one of
/// `pending-approval | approved | executing | failed`. Plans
/// in `completed` or `rejected` no longer gate.
async fn find_blocking_migration_plan(
    client: &Client,
    app_name: &str,
    app_namespace: &str,
    environment: Option<&str>,
) -> Result<Option<MigrationPlan>, ReconcileError> {
    let api: Api<MigrationPlan> = Api::namespaced(client.clone(), MIGRATION_PLAN_NAMESPACE);
    let list = api.list(&Default::default()).await?;
    Ok(pick_blocking_plan(
        list.items,
        app_name,
        app_namespace,
        environment,
    ))
}

/// Pure scope-matching logic for `find_blocking_migration_plan`.
/// Extracted so unit tests can exercise the filter without a
/// kube `Client`.
fn pick_blocking_plan(
    plans: Vec<MigrationPlan>,
    app_name: &str,
    app_namespace: &str,
    environment: Option<&str>,
) -> Option<MigrationPlan> {
    plans.into_iter().find(|plan| {
        if plan.spec.scope.type_ != "application" {
            return false;
        }
        let Some(app_scope) = &plan.spec.scope.application else {
            return false;
        };
        if app_scope.ref_.name != app_name || app_scope.ref_.namespace != app_namespace {
            return false;
        }
        if let Some(e) = environment {
            if app_scope.environment != e {
                return false;
            }
        }
        plan_is_blocking(plan)
    })
}

fn plan_is_blocking(plan: &MigrationPlan) -> bool {
    let phase = plan
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("pending-approval");
    !matches!(phase, "completed" | "rejected")
}

/// Build the Application status payload for the pause path.
/// Preserves `observedGeneration` + `endpointURL` from the
/// previous reconcile (child resources keep running, so the
/// endpoint URL remains live), flips `phase` to
/// `AwaitingMigrationApproval`, and emits two conditions:
///
///   * `Ready=False/MigrationPending` so consumers (Argo CD
///     health Lua, alertmanager) see the platform halted.
///   * `MigrationPending=True` carrying the plan name in
///     `message` for direct `kubectl describe` discovery.
fn build_paused_status(app: &Application, plan_name: &str) -> ApplicationStatus {
    let previous_conditions = app
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[]);
    let previous_endpoint = app.status.as_ref().and_then(|s| s.endpoint_url.clone());

    let ready = ready_condition(
        "False",
        "MigrationPending",
        &format!(
            "paused awaiting approval of MigrationPlan \
             {MIGRATION_PLAN_NAMESPACE}/{plan_name}"
        ),
        previous_conditions,
    );
    let pending = migration_pending_condition(plan_name, previous_conditions);

    ApplicationStatus {
        phase: Some(PHASE_AWAITING_MIGRATION_APPROVAL.to_string()),
        observed_generation: app.metadata.generation,
        conditions: Some(vec![ready, pending]),
        endpoint_url: previous_endpoint,
    }
}

fn migration_pending_condition(
    plan_name: &str,
    previous: &[ApplicationCondition],
) -> ApplicationCondition {
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == COND_MIGRATION_PENDING && c.status == "True")
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    ApplicationCondition {
        type_: COND_MIGRATION_PENDING.to_string(),
        status: "True".to_string(),
        last_transition_time,
        reason: "MigrationPlanPending".to_string(),
        message: format!(
            "MigrationPlan {MIGRATION_PLAN_NAMESPACE}/{plan_name} is awaiting approval"
        ),
        observed_generation: None,
    }
}

fn ready_condition(
    status: &str,
    reason: &str,
    message: &str,
    previous: &[ApplicationCondition],
) -> ApplicationCondition {
    // Per k8s convention, `lastTransitionTime` moves only when the
    // condition's `status` field changes (False → True etc.). If
    // we just observed a Ready condition with the same `status`
    // value, reuse its timestamp. This is what stops the v0.1.61
    // hot-reconcile loop: identical status output ⇒ SSA patch is
    // a no-op ⇒ no watch event on our own write ⇒ no spurious
    // re-reconcile.
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == "Ready" && c.status == status)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    ApplicationCondition {
        type_: "Ready".to_string(),
        status: status.to_string(),
        last_transition_time,
        reason: reason.to_string(),
        message: message.to_string(),
        observed_generation: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::{
        ApplicationSpec, MigrationApplicationRef, MigrationApplicationScope, MigrationPlanScope,
        MigrationPlanSpec, MigrationPlanStatus, MigrationTrigger,
    };

    fn app_plan(
        name: &str,
        target_app: &str,
        target_ns: &str,
        environment: &str,
        phase: Option<&str>,
    ) -> MigrationPlan {
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: "application".into(),
                application: Some(MigrationApplicationScope {
                    ref_: MigrationApplicationRef {
                        name: target_app.into(),
                        namespace: target_ns.into(),
                    },
                    environment: environment.into(),
                }),
                platform: None,
            },
            trigger: MigrationTrigger {
                type_: "t".into(),
                field: "f".into(),
                from: None,
                to: None,
            },
            risks: None,
            plan: None,
            approvers: None,
            previous_spec_snapshot: None,
        };
        let mut plan = MigrationPlan::new(name, spec);
        plan.metadata.namespace = Some(MIGRATION_PLAN_NAMESPACE.into());
        if let Some(p) = phase {
            plan.status = Some(MigrationPlanStatus {
                phase: Some(p.into()),
                ..MigrationPlanStatus::default()
            });
        }
        plan
    }

    fn platform_plan(name: &str) -> MigrationPlan {
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: "platform".into(),
                application: None,
                platform: Some(operator_core::MigrationPlatformScope {
                    components: vec!["apprafter-operator".into()],
                }),
            },
            trigger: MigrationTrigger {
                type_: "t".into(),
                field: "f".into(),
                from: None,
                to: None,
            },
            risks: None,
            plan: None,
            approvers: None,
            previous_spec_snapshot: None,
        };
        let mut plan = MigrationPlan::new(name, spec);
        plan.metadata.namespace = Some(MIGRATION_PLAN_NAMESPACE.into());
        plan
    }

    #[test]
    fn pick_blocking_plan_finds_matching_pending_plan() {
        // Baseline: a plan in `pending-approval` whose scope
        // matches the Application's name + namespace +
        // environment must be picked up.
        let plans = vec![app_plan(
            "parser-pg",
            "parser",
            "demo",
            "prod",
            Some("pending-approval"),
        )];
        let plan = pick_blocking_plan(plans, "parser", "demo", Some("prod"));
        assert!(plan.is_some());
        assert_eq!(plan.unwrap().metadata.name.as_deref(), Some("parser-pg"));
    }

    #[test]
    fn pick_blocking_plan_skips_completed_plan() {
        // Completed plans no longer gate the Application — the
        // operator approved + ran them; future reconciles
        // resume normal flow.
        let plans = vec![app_plan(
            "parser-pg",
            "parser",
            "demo",
            "prod",
            Some("completed"),
        )];
        let plan = pick_blocking_plan(plans, "parser", "demo", Some("prod"));
        assert!(plan.is_none());
    }

    #[test]
    fn pick_blocking_plan_skips_rejected_plan() {
        // Application-scope plans can't be rejected per the
        // webhook FSM, but the pause path treats `rejected` as
        // resumable defensively (matches Phase 2 platform-scope
        // semantics).
        let plans = vec![app_plan(
            "parser-pg",
            "parser",
            "demo",
            "prod",
            Some("rejected"),
        )];
        let plan = pick_blocking_plan(plans, "parser", "demo", Some("prod"));
        assert!(plan.is_none());
    }

    #[test]
    fn pick_blocking_plan_treats_missing_phase_as_blocking() {
        // A plan just created — no status.phase yet. The
        // reconciler must pause; the plan is on its way.
        let plans = vec![app_plan("parser-pg", "parser", "demo", "prod", None)];
        let plan = pick_blocking_plan(plans, "parser", "demo", Some("prod"));
        assert!(plan.is_some());
    }

    #[test]
    fn pick_blocking_plan_blocks_on_executing_plan() {
        // Mid-execution plans gate the Application — the
        // MigrationController is running steps, the user shouldn't
        // get fresh child patches stepping on its work.
        let plans = vec![app_plan(
            "parser-pg",
            "parser",
            "demo",
            "prod",
            Some("executing"),
        )];
        let plan = pick_blocking_plan(plans, "parser", "demo", Some("prod"));
        assert!(plan.is_some());
    }

    #[test]
    fn pick_blocking_plan_blocks_on_failed_plan() {
        // A failed plan needs operator action; resuming child
        // patches would silently override the intent. Better
        // to keep paused until the user explicitly resolves
        // (delete plan, create new one, etc.).
        let plans = vec![app_plan(
            "parser-pg",
            "parser",
            "demo",
            "prod",
            Some("failed"),
        )];
        let plan = pick_blocking_plan(plans, "parser", "demo", Some("prod"));
        assert!(plan.is_some());
    }

    #[test]
    fn pick_blocking_plan_ignores_platform_scope_plans() {
        // Platform-scope plans are observed by PlatformController,
        // not Application reconciler. Filter must exclude them.
        let plans = vec![platform_plan("p-1")];
        let plan = pick_blocking_plan(plans, "parser", "demo", None);
        assert!(plan.is_none());
    }

    #[test]
    fn pick_blocking_plan_filters_by_application_namespace() {
        // Same name in a different namespace must NOT match.
        let plans = vec![app_plan(
            "parser-pg",
            "parser",
            "other-ns",
            "prod",
            Some("pending-approval"),
        )];
        let plan = pick_blocking_plan(plans, "parser", "demo", Some("prod"));
        assert!(plan.is_none());
    }

    #[test]
    fn pick_blocking_plan_filters_by_environment_when_set() {
        // Environments are scoped — a `dev` plan must not gate
        // the `prod` reconcile.
        let plans = vec![app_plan(
            "parser-pg",
            "parser",
            "demo",
            "dev",
            Some("pending-approval"),
        )];
        let plan = pick_blocking_plan(plans, "parser", "demo", Some("prod"));
        assert!(plan.is_none());
    }

    #[test]
    fn pick_blocking_plan_ignores_environment_when_caller_passes_none() {
        // The reconciler's `APPRAFTER_ENV` may be unset (the
        // single-env case). Environment becomes a wildcard
        // matcher then — any matching app + namespace blocks.
        let plans = vec![app_plan(
            "parser-pg",
            "parser",
            "demo",
            "prod",
            Some("pending-approval"),
        )];
        let plan = pick_blocking_plan(plans, "parser", "demo", None);
        assert!(plan.is_some());
    }

    #[test]
    fn build_paused_status_sets_awaiting_migration_phase_and_pending_condition() {
        // Pause-path status contract: phase flips to
        // `AwaitingMigrationApproval`, conditions include
        // `Ready=False/MigrationPending` plus a positive
        // `MigrationPending=True` carrying the plan name.
        let mut app = Application::new("web", ApplicationSpec::default());
        app.metadata.generation = Some(7);
        app.status = Some(ApplicationStatus {
            phase: Some("Ready".into()),
            observed_generation: Some(6),
            conditions: None,
            endpoint_url: Some("http://web.demo.svc.cluster.local:80".into()),
        });
        let status = build_paused_status(&app, "web-prod-migration-1");

        assert_eq!(
            status.phase.as_deref(),
            Some(PHASE_AWAITING_MIGRATION_APPROVAL)
        );
        // observedGeneration carries the *current* generation —
        // the controller observed this revision but chose to
        // pause. Argo CD diffs read this to surface "sync paused
        // on rev=N".
        assert_eq!(status.observed_generation, Some(7));
        // EndpointURL preserved — children still running.
        assert_eq!(
            status.endpoint_url.as_deref(),
            Some("http://web.demo.svc.cluster.local:80")
        );

        let conds = status.conditions.as_ref().expect("conditions");
        let ready = conds.iter().find(|c| c.type_ == "Ready").expect("ready");
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "MigrationPending");

        let pending = conds
            .iter()
            .find(|c| c.type_ == COND_MIGRATION_PENDING)
            .expect("migration pending");
        assert_eq!(pending.status, "True");
        assert!(pending.message.contains("web-prod-migration-1"));
    }

    #[test]
    fn build_paused_status_preserves_endpoint_when_status_absent() {
        // First reconcile that pauses before ever succeeding:
        // app.status is None. Endpoint stays None too.
        let app = Application::new("web", ApplicationSpec::default());
        let status = build_paused_status(&app, "plan-1");
        assert!(status.endpoint_url.is_none());
        assert_eq!(
            status.phase.as_deref(),
            Some(PHASE_AWAITING_MIGRATION_APPROVAL)
        );
    }

    #[test]
    fn migration_pending_condition_preserves_transition_time_when_already_true() {
        // Same k8s convention as ready_condition: timestamp
        // moves only when status flips. The condition starts
        // at True the moment a plan appears; subsequent
        // reconciles must NOT bump the timestamp until the
        // plan resolves and we drop the condition.
        let prior = vec![ApplicationCondition {
            type_: COND_MIGRATION_PENDING.into(),
            status: "True".into(),
            last_transition_time: "2026-05-22T12:00:00+00:00".into(),
            reason: "MigrationPlanPending".into(),
            message: "old message".into(),
            observed_generation: None,
        }];
        let next = migration_pending_condition("plan-1", &prior);
        assert_eq!(next.last_transition_time, "2026-05-22T12:00:00+00:00");
    }

    #[test]
    fn endpoint_url_uses_cluster_local_fqdn() {
        let url = cluster_internal_endpoint_url("web", "default", 80);
        assert_eq!(url, "http://web.default.svc.cluster.local:80");
    }

    #[test]
    fn into_apply_payload_injects_apiversion_and_kind() {
        let value = into_apply_payload(
            "apps/v1",
            "Deployment",
            &serde_json::json!({"metadata": {"name": "x"}}),
        )
        .unwrap();
        let map = value.as_object().unwrap();
        assert_eq!(
            map.get("apiVersion").and_then(|v| v.as_str()),
            Some("apps/v1")
        );
        assert_eq!(map.get("kind").and_then(|v| v.as_str()), Some("Deployment"));
        // The original metadata.name still survives.
        assert_eq!(
            map.get("metadata")
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str()),
            Some("x")
        );
    }

    #[test]
    fn build_status_carries_observed_generation_phase_and_endpoint() {
        let mut app = Application::new("web", ApplicationSpec::default());
        app.metadata.generation = Some(7);
        let conds = vec![ready_condition("True", "Ok", "ok", &[])];
        let s = build_status(&app, "Ready", conds, Some("http://x:80".into()));
        assert_eq!(s.phase.as_deref(), Some("Ready"));
        assert_eq!(s.observed_generation, Some(7));
        assert_eq!(s.endpoint_url.as_deref(), Some("http://x:80"));
        let cs = s.conditions.expect("conditions");
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].type_, "Ready");
        assert_eq!(cs[0].status, "True");
    }

    #[test]
    fn ready_condition_preserves_transition_time_when_status_unchanged() {
        // Regression guard for v0.1.63: the operator wrote status
        // on every reconcile, and the previous `ready_condition`
        // always set `Utc::now()` for lastTransitionTime — so each
        // write produced a status-diff, the apiserver fired a
        // watch event on our own update, and the controller looped
        // hot. Per k8s convention `lastTransitionTime` moves only
        // when the condition's `status` value changes. With the
        // fix in place, a second `ready_condition` call carrying
        // the previous condition slice must return the SAME
        // timestamp string.
        let previous = vec![ApplicationCondition {
            type_: "Ready".to_string(),
            status: "True".to_string(),
            last_transition_time: "2026-05-11T00:21:18.194260724+00:00".to_string(),
            reason: "ReconcileSucceeded".to_string(),
            message: "first reconcile".to_string(),
            observed_generation: None,
        }];
        let next = ready_condition(
            "True",
            "ReconcileSucceeded",
            "Reconcile completed; child Deployment and Service applied.",
            &previous,
        );
        assert_eq!(
            next.last_transition_time, previous[0].last_transition_time,
            "lastTransitionTime must be preserved when status is unchanged (k8s convention)"
        );
        assert_eq!(next.status, "True");
    }

    #[test]
    fn ready_condition_bumps_transition_time_when_status_flips() {
        // The other half of the invariant: when status DOES change
        // (True ↔ False), the timestamp MUST move forward so
        // downstream tooling (alertmanager, dashboards, audit
        // logs) sees a real transition event.
        let previous = vec![ApplicationCondition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            last_transition_time: "2026-05-11T00:00:00+00:00".to_string(),
            reason: "ApplyFailed".to_string(),
            message: "old".to_string(),
            observed_generation: None,
        }];
        let next = ready_condition("True", "ReconcileSucceeded", "now ok", &previous);
        assert_ne!(
            next.last_transition_time, previous[0].last_transition_time,
            "lastTransitionTime must change when status flips False → True"
        );
        assert_eq!(next.status, "True");
    }

    #[test]
    fn ready_condition_has_rfc3339_timestamp_and_required_fields() {
        let c = ready_condition(
            "False",
            "ApplyFailed",
            "the deployment apply step returned an error",
            &[],
        );
        assert_eq!(c.type_, "Ready");
        assert_eq!(c.status, "False");
        assert_eq!(c.reason, "ApplyFailed");
        assert!(c.message.contains("apply step"));
        // RFC3339 timestamps look like `2026-05-08T12:34:56.789+00:00`
        // — assert the structural anchors.
        assert!(
            c.last_transition_time.contains('T'),
            "{}",
            c.last_transition_time
        );
        assert!(
            c.last_transition_time.ends_with("+00:00") || c.last_transition_time.ends_with('Z'),
            "{}",
            c.last_transition_time
        );
    }
}
