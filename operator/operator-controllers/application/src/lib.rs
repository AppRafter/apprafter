// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs Controller for the v1alpha1 `Application` CRD.
//!
//! v0.1.31 (sub-phase 1.9b) wires the v0.1.30 renderer into
//! `reconcile` via server-side apply with field manager
//! `apprafter-operator` and updates the Application's `status`
//! subresource (phase, observedGeneration, conditions, endpointURL).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{LocalObjectReference, Secret, Service};
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Client, ResourceExt};
use serde_json::{json, Value};
use thiserror::Error;
use tracing::{info, warn};

mod pull_secret;
use pull_secret::{app_pull_secret_name, pick_pull_credential};

use operator_controllers_sourcecredential::pull_secret_name;
use operator_core::{
    Application, ApplicationBaseSpec, ApplicationCondition, ApplicationStatus, Metrics,
    MigrationPlan, ResourceClaim, SourceCredential, COND_MIGRATION_PENDING,
    COND_RESOURCE_CLAIM_PENDING, PHASE_AWAITING_MIGRATION_APPROVAL, PHASE_AWAITING_RESOURCE_CLAIM,
};
// `ApplicationMigrationStrategy` is wired through Cargo.toml
// dep so future Phase 2 commits can flip on detection +
// auto-plan-creation by editing one call site here; the
// strategy struct + its concrete `detect_destructive` /
// `create_plan_for` fns ship in
// `operator-controllers-migration` (B.1.77). The unused-import
// guard turns the dep into a "feature flag" — Phase 2 simply
// uncomments the `use` line.
use operator_rendering::{effective_spec, render_application_for_env};

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
    // 2.4d: watch child ResourceClaims so the provisioner flipping a
    // claim ready re-enqueues the owning Application immediately
    // (resume from the AwaitingResourceClaim pause). Clone the client
    // BEFORE it moves into Context.
    let claims: Api<ResourceClaim> = Api::all(client.clone());
    let context = Arc::new(Context {
        client,
        metrics,
        env_name,
    });

    Controller::new(apps, watcher::Config::default())
        .owns(claims, watcher::Config::default())
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

    // Deletion gate (1.79c walk-fix #4). Once the Application carries
    // a `deletionTimestamp` (e.g. Argo CD is cascade-deleting it via
    // the `resources-finalization.argocd.argoproj.io` finalizer the
    // CLI sets at `app add`), the operator must NOT re-apply children:
    // re-creating the Deployment that the cascade is removing keeps
    // the managed-resource tree non-empty, so Argo's finalizer never
    // clears and the Application hangs in `Terminating` forever. The
    // ownerReference cascade handles child cleanup; there is nothing
    // for us to reconcile on a dying object.
    if is_deletion_marked(&app) {
        info!(%name, %namespace, "Application is being deleted; skipping reconcile");
        return Ok(Action::await_change());
    }

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

    let pp = PatchParams::apply(FIELD_MANAGER).force();

    // ---- 2.4d: generate ResourceClaims for needs, pause until ready ----
    // Runs AFTER the migration gate, BEFORE the render. Generates one
    // child ResourceClaim per `needs` entry (SSA, owner-ref → this
    // Application for cascade) writing spec+metadata only — never
    // status (the scheduler owns status.provider/Scheduled, the
    // provisioner owns status.ready/connectionSecretRef/Ready). Pauses
    // in `AwaitingResourceClaim` until every claim reports
    // status.ready + connectionSecretRef; the `.owns(ResourceClaim)`
    // watch re-enqueues this Application the moment the provisioner
    // flips one ready. DSN injection into the Deployment is 2.4e — on
    // resume the Deployment renders WITHOUT DATABASE_URL until then.
    // 2.4e: resolved `needs-type → connectionSecretRef` map, threaded
    // into the render so a ready needs.pg workload gets DATABASE_URL
    // injected. Built from the SAME `current` ready claims the gate
    // below validated, AFTER the gate passes — so it is empty unless
    // execution falls through to the render (all claims ready).
    let mut needs_secrets: BTreeMap<String, String> = BTreeMap::new();
    let effective = effective_spec(&app, ctx.env_name.as_deref());
    let has_needs = effective.needs.as_ref().is_some_and(|n| !n.is_empty());
    if has_needs {
        let app_uid = app.metadata.uid.clone().unwrap_or_default();
        let payloads = generate_resource_claims(&effective, &name, &app_uid, &namespace);
        let claim_api: Api<ResourceClaim> = Api::namespaced(ctx.client.clone(), &namespace);
        for (claim_name, payload) in &payloads {
            claim_api
                .patch(claim_name, &pp, &Patch::Apply(payload))
                .await?;
        }
        // Re-fetch to read provisioner-written status.
        let mut current = Vec::with_capacity(payloads.len());
        for (claim_name, _) in &payloads {
            if let Ok(c) = claim_api.get(claim_name).await {
                current.push(c);
            }
        }
        let unready = unready_claim_names(&current);
        if !unready.is_empty() || current.len() != payloads.len() {
            let names = if unready.is_empty() {
                payloads.iter().map(|(n, _)| n.clone()).collect()
            } else {
                unready
            };
            let status = build_resource_claim_paused_status(&app, &names);
            apply_status(&ctx.client, &namespace, &name, &status, &pp).await?;
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, &namespace, "paused"])
                .inc();
            return Ok(Action::requeue(Duration::from_secs(30)));
        }
        // Gate passed — every claim is ready with a connection Secret.
        // Resolve the SAME `current` claims into the DSN map for render.
        needs_secrets = resolve_needs_secrets(&current);
    }

    let mut rendered = render_application_for_env(
        &app,
        ctx.env_name.as_deref(),
        if needs_secrets.is_empty() {
            None
        } else {
            Some(&needs_secrets)
        },
    );

    // Seam A (1.79c S3): if a SourceCredential covers this image's
    // registry, project its derived pull-secret into the workload
    // namespace and attach it to the Deployment's imagePullSecrets.
    attach_pull_secret(&ctx.client, &namespace, &mut rendered.deployment, &pp).await?;

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

/// Namespace SourceCredentials and their canonical derived
/// pull-secrets live in.
const SOURCECRED_NAMESPACE: &str = "apprafter-system";

/// The effective container image of the rendered Deployment.
fn deployment_image(deployment: &Deployment) -> Option<String> {
    deployment
        .spec
        .as_ref()?
        .template
        .spec
        .as_ref()?
        .containers
        .first()?
        .image
        .clone()
}

/// Seam A: host-match the rendered image to a SourceCredential, project
/// its derived `dockerconfigjson` into the workload namespace, and set
/// the Deployment's `imagePullSecrets`. A no-op (leaves the Deployment
/// untouched) when the image is public or no covering credential's
/// pull-secret has been derived yet — the next reconcile retries.
async fn attach_pull_secret(
    client: &Client,
    namespace: &str,
    deployment: &mut Deployment,
    pp: &PatchParams,
) -> Result<(), ReconcileError> {
    let Some(image) = deployment_image(deployment) else {
        return Ok(());
    };
    let creds = list_source_credentials(client).await?;
    let Some(cred) = pick_pull_credential(&image, &creds) else {
        return Ok(());
    };
    let cred_name = cred.name_any();

    // The canonical dockerconfigjson the SourceCredential controller
    // derived. Absent if it has not reconciled yet — skip, retry later.
    let canonical = pull_secret_name(&cred_name);
    let Some(dockercfg) = read_dockercfgjson(client, SOURCECRED_NAMESPACE, &canonical).await?
    else {
        info!(%cred_name, "covering SourceCredential found but pull-secret not derived yet; deferring attach");
        return Ok(());
    };

    let copy_name = app_pull_secret_name(&cred_name);
    apply_pull_secret_copy(client, namespace, &copy_name, &dockercfg, pp).await?;
    set_image_pull_secret(deployment, &copy_name);
    info!(%cred_name, %copy_name, %namespace, "attached pull-secret to workload");
    Ok(())
}

async fn list_source_credentials(client: &Client) -> Result<Vec<SourceCredential>, ReconcileError> {
    let api: Api<SourceCredential> = Api::namespaced(client.clone(), SOURCECRED_NAMESPACE);
    Ok(api.list(&Default::default()).await?.items)
}

/// Read the `.dockerconfigjson` value of a dockerconfigjson Secret.
/// Returns `None` if the Secret does not exist.
async fn read_dockercfgjson(
    client: &Client,
    namespace: &str,
    name: &str,
) -> Result<Option<String>, ReconcileError> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let Some(secret) = api.get_opt(name).await? else {
        return Ok(None);
    };
    let value = secret
        .data
        .as_ref()
        .and_then(|d| d.get(".dockerconfigjson"))
        .map(|b| String::from_utf8_lossy(&b.0).into_owned());
    Ok(value)
}

/// SSA-apply a per-workload copy of the derived pull-secret.
async fn apply_pull_secret_copy(
    client: &Client,
    namespace: &str,
    name: &str,
    dockercfg: &str,
    pp: &PatchParams,
) -> Result<(), ReconcileError> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let payload = json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": { "apprafter.io/managed-by": "apprafter" }
        },
        "type": "kubernetes.io/dockerconfigjson",
        "stringData": { ".dockerconfigjson": dockercfg }
    });
    api.patch(name, pp, &Patch::Apply(&payload)).await?;
    Ok(())
}

/// Set the Deployment's `imagePullSecrets` to the projected copy.
fn set_image_pull_secret(deployment: &mut Deployment, secret_name: &str) {
    if let Some(spec) = deployment.spec.as_mut() {
        if let Some(pod) = spec.template.spec.as_mut() {
            pod.image_pull_secrets = Some(vec![LocalObjectReference {
                name: secret_name.to_string(),
            }]);
        }
    }
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

/// True once the Application is marked for deletion. The reconcile loop
/// skips deletion-marked objects so it never re-applies children that a
/// cascade delete (Argo CD finalizer) is trying to remove.
fn is_deletion_marked(app: &Application) -> bool {
    app.metadata.deletion_timestamp.is_some()
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

// ---- 2.4d: pure ResourceClaim generation + pause helpers ----

/// Derive a DNS-1123-safe `metadata.name` for a child ResourceClaim
/// from the owning Application name + the need's service type:
/// `{app}-{type}`, non-alphanumerics folded to `-`, lowercased,
/// truncated to 63 bytes, trailing `-` trimmed. Mirrors
/// `resourceclaim-provisioner::cnpg::k8s_name`'s fold (without the
/// `claim-` prefix — the `{app}-` prefix already guarantees a
/// leading alphanumeric for any valid Application name).
fn claim_name(app: &str, service_type: &str) -> String {
    let raw = format!("{app}-{service_type}");
    let mut out = String::with_capacity(raw.len().min(63));
    for ch in raw.chars() {
        out.push(if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        });
    }
    out.truncate(63);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Default selector injected into a generated ResourceClaim when the
/// need omits one — the integrated (in-cluster) tier. The 2.3
/// scheduler matches this against `ServiceProvider.metadata.labels`.
fn default_integrated_selector() -> BTreeMap<String, String> {
    BTreeMap::from([("tier".to_string(), "integrated".to_string())])
}

/// Build one SSA apply payload per `needs` entry of the effective
/// spec. Returns `(claim_name, apply_payload)` pairs in deterministic
/// (BTreeMap) order. The payload carries spec + metadata only —
/// **never `status`** — because the scheduler owns
/// `status.provider`/`Scheduled` and the provisioner owns
/// `status.ready`/`connectionSecretRef`/`Ready` (SSA split under the
/// `apprafter-operator` field manager). A default
/// `{tier: integrated}` selector is injected when the need omits one;
/// `size` is emitted only when present.
fn generate_resource_claims(
    spec: &ApplicationBaseSpec,
    app_name: &str,
    app_uid: &str,
    namespace: &str,
) -> Vec<(String, Value)> {
    let Some(needs) = spec.needs.as_ref() else {
        return Vec::new();
    };
    needs
        .iter()
        .map(|(service_type, need)| {
            let name = claim_name(app_name, service_type);
            let selector = need
                .selector
                .clone()
                .unwrap_or_else(default_integrated_selector);
            let mut claim_spec = json!({
                "type": service_type,
                "selector": selector,
            });
            if let Some(size) = &need.size {
                claim_spec["size"] = json!(size);
            }
            let payload = json!({
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "ResourceClaim",
                "metadata": {
                    "name": name,
                    "namespace": namespace,
                    "ownerReferences": [{
                        "apiVersion": "apprafter.io/v1alpha1",
                        "kind": "Application",
                        "name": app_name,
                        "uid": app_uid,
                        "controller": true,
                        "blockOwnerDeletion": true,
                    }],
                },
                "spec": claim_spec,
            });
            (name, payload)
        })
        .collect()
}

/// Names of claims that are NOT yet ready. A claim is ready only when
/// `status.ready == Some(true)` AND `status.connectionSecretRef`
/// is set — the provisioner writes both together (2.4c), so the
/// AND closes the half-ready resume race at zero cost. Returns the
/// unready names in the claims' iteration order.
fn unready_claim_names(claims: &[ResourceClaim]) -> Vec<String> {
    claims
        .iter()
        .filter(|c| {
            let ready = c
                .status
                .as_ref()
                .map(|s| s.ready == Some(true) && s.connection_secret_ref.is_some())
                .unwrap_or(false);
            !ready
        })
        .map(|c| c.name_any())
        .collect()
}

/// Resolve ready claims into a `needs-type → connectionSecretRef`
/// map for the renderer's 2.4e DSN injection. Keyed on
/// `spec.type_` (the need key), valued by `status.connectionSecretRef`;
/// claims without a resolved connection Secret are skipped (defensive
/// — post-gate every claim has one). Pure: the operator only READS
/// provisioner-owned claim status here, never writes it (the SSA
/// split is preserved). The caller threads the result into
/// `render_application_for_env` AFTER the 2.4d readiness gate passes,
/// building it from the SAME `current` claims the gate validated.
fn resolve_needs_secrets(claims: &[ResourceClaim]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for claim in claims {
        if let Some(secret) = claim
            .status
            .as_ref()
            .and_then(|s| s.connection_secret_ref.clone())
        {
            map.insert(claim.spec.type_.clone(), secret);
        }
    }
    map
}

/// Build the Application status payload for the ResourceClaim pause
/// path. Mirrors [`build_paused_status`]: preserves
/// `observedGeneration` + `endpointURL`, flips `phase` to
/// `AwaitingResourceClaim`, and emits two conditions —
/// `Ready=False/ResourceClaimPending` plus a positive
/// `ResourceClaimPending=True` naming the unready claim(s).
fn build_resource_claim_paused_status(app: &Application, unready: &[String]) -> ApplicationStatus {
    let previous_conditions = app
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[]);
    let previous_endpoint = app.status.as_ref().and_then(|s| s.endpoint_url.clone());

    let ready = ready_condition(
        "False",
        "ResourceClaimPending",
        &format!(
            "paused awaiting ResourceClaim provisioning: {}",
            unready.join(", ")
        ),
        previous_conditions,
    );
    let pending = resource_claim_pending_condition(unready, previous_conditions);

    ApplicationStatus {
        phase: Some(PHASE_AWAITING_RESOURCE_CLAIM.to_string()),
        observed_generation: app.metadata.generation,
        conditions: Some(vec![ready, pending]),
        endpoint_url: previous_endpoint,
    }
}

/// The `ResourceClaimPending=True` condition. `lastTransitionTime`
/// is preserved when the prior `ResourceClaimPending` was already
/// `True` (mirror [`migration_pending_condition`]), bumped otherwise.
fn resource_claim_pending_condition(
    unready: &[String],
    previous: &[ApplicationCondition],
) -> ApplicationCondition {
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == COND_RESOURCE_CLAIM_PENDING && c.status == "True")
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    ApplicationCondition {
        type_: COND_RESOURCE_CLAIM_PENDING.to_string(),
        status: "True".to_string(),
        last_transition_time,
        reason: "ResourceClaimPending".to_string(),
        message: format!("awaiting ResourceClaim(s): {}", unready.join(", ")),
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
        ApplicationBaseSpec, ApplicationSpec, MigrationApplicationRef, MigrationApplicationScope,
        MigrationPlanScope, MigrationPlanSpec, MigrationPlanStatus, MigrationTrigger,
    };
    use std::collections::BTreeMap;

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

    #[test]
    fn is_deletion_marked_detects_deletion_timestamp() {
        // Regression guard (1.79c walk-fix #4): the reconcile loop must
        // skip Applications carrying a deletionTimestamp, otherwise it
        // re-applies the Deployment a cascade delete is removing and the
        // Argo CD finalizer hangs forever.
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
        let mut app = Application::new("web", ApplicationSpec::default());
        assert!(!is_deletion_marked(&app));
        app.metadata.deletion_timestamp = Some(Time(Utc::now()));
        assert!(is_deletion_marked(&app));
    }

    // ---- 2.4d: pure ResourceClaim generation + pause helpers ----

    use operator_core::{
        ResourceClaim, ResourceClaimSpec, ResourceClaimStatus, ServiceNeed,
        COND_RESOURCE_CLAIM_PENDING, PHASE_AWAITING_RESOURCE_CLAIM,
    };

    fn base_with_needs(needs: BTreeMap<String, ServiceNeed>) -> ApplicationBaseSpec {
        ApplicationBaseSpec {
            image: Some("ghcr.io/acme/web:1.0".into()),
            needs: Some(needs),
            ..Default::default()
        }
    }

    #[test]
    fn claim_name_joins_app_and_type_and_is_dns1123_safe() {
        assert_eq!(claim_name("parser", "pg"), "parser-pg");
        // Non-alphanumerics fold to `-`, lowercase, trailing `-` trimmed.
        let n = claim_name("My_App.", "Redis_Cache_");
        assert_eq!(n, "my-app--redis-cache");
        // DNS-1123 validity: lowercased, no `_`, start/end alphanumeric.
        assert!(!n.contains('_'));
        assert_eq!(n, n.to_lowercase());
        assert!(n.chars().next().unwrap().is_ascii_alphanumeric());
        assert!(n.chars().last().unwrap().is_ascii_alphanumeric());
        // Truncates to 63 bytes.
        let long = claim_name(&"a".repeat(80), &"b".repeat(80));
        assert!(long.len() <= 63, "len was {}", long.len());
        assert!(long.chars().last().unwrap().is_ascii_alphanumeric());
    }

    #[test]
    fn default_integrated_selector_is_tier_integrated() {
        let sel = default_integrated_selector();
        assert_eq!(sel.get("tier").map(String::as_str), Some("integrated"));
        assert_eq!(sel.len(), 1);
    }

    #[test]
    fn generate_resource_claims_injects_default_selector_and_owner_ref_no_status() {
        let mut needs = BTreeMap::new();
        needs.insert("pg".to_string(), ServiceNeed::default());
        let spec = base_with_needs(needs);
        let payloads = generate_resource_claims(&spec, "parser", "uid-123", "demo");
        assert_eq!(payloads.len(), 1);
        let (name, payload) = &payloads[0];
        assert_eq!(name, "parser-pg");
        assert_eq!(payload["metadata"]["name"], json!("parser-pg"));
        assert_eq!(payload["metadata"]["namespace"], json!("demo"));
        assert_eq!(payload["apiVersion"], json!("apprafter.io/v1alpha1"));
        assert_eq!(payload["kind"], json!("ResourceClaim"));
        assert_eq!(payload["spec"]["type"], json!("pg"));
        // Default selector injected when the need omits it.
        assert_eq!(payload["spec"]["selector"], json!({ "tier": "integrated" }));
        // No size key when absent.
        assert!(payload["spec"].get("size").is_none());
        // ownerRef → Application, controller + blockOwnerDeletion.
        let owner = &payload["metadata"]["ownerReferences"][0];
        assert_eq!(owner["apiVersion"], json!("apprafter.io/v1alpha1"));
        assert_eq!(owner["kind"], json!("Application"));
        assert_eq!(owner["name"], json!("parser"));
        assert_eq!(owner["uid"], json!("uid-123"));
        assert_eq!(owner["controller"], json!(true));
        assert_eq!(owner["blockOwnerDeletion"], json!(true));
        // SSA split guard: the apply payload must carry NO status key.
        assert!(
            payload.get("status").is_none(),
            "claim apply payload must not write status (scheduler/provisioner own it)"
        );
    }

    #[test]
    fn generate_resource_claims_passes_through_selector_and_size() {
        let mut needs = BTreeMap::new();
        needs.insert(
            "pg".to_string(),
            ServiceNeed {
                selector: Some(BTreeMap::from([(
                    "tier".to_string(),
                    "managed".to_string(),
                )])),
                size: Some("small".into()),
            },
        );
        let spec = base_with_needs(needs);
        let payloads = generate_resource_claims(&spec, "parser", "uid-1", "demo");
        let (_, payload) = &payloads[0];
        assert_eq!(payload["spec"]["selector"], json!({ "tier": "managed" }));
        assert_eq!(payload["spec"]["size"], json!("small"));
    }

    #[test]
    fn generate_resource_claims_yields_one_payload_per_need_in_deterministic_order() {
        let mut needs = BTreeMap::new();
        needs.insert("redis".to_string(), ServiceNeed::default());
        needs.insert("pg".to_string(), ServiceNeed::default());
        let spec = base_with_needs(needs);
        let payloads = generate_resource_claims(&spec, "app", "u", "ns");
        assert_eq!(payloads.len(), 2);
        // BTreeMap iteration → deterministic: pg before redis.
        assert_eq!(payloads[0].0, "app-pg");
        assert_eq!(payloads[1].0, "app-redis");
    }

    fn ready_claim(name: &str, ready: Option<bool>, secret: Option<&str>) -> ResourceClaim {
        let mut c = ResourceClaim::new(
            name,
            ResourceClaimSpec {
                type_: "pg".into(),
                selector: BTreeMap::from([("tier".to_string(), "integrated".to_string())]),
                size: None,
            },
        );
        c.metadata.namespace = Some("demo".into());
        c.status = Some(ResourceClaimStatus {
            provider: None,
            connection_secret_ref: secret.map(String::from),
            ready,
            conditions: None,
        });
        c
    }

    #[test]
    fn unready_claim_names_requires_ready_true_and_connection_secret() {
        // ready + secret → ready (absent from unready list).
        let ready = vec![ready_claim("a-pg", Some(true), Some("a-pg-conn"))];
        assert!(unready_claim_names(&ready).is_empty());
        // ready but no secret → unready (half-ready resume race).
        let half = vec![ready_claim("a-pg", Some(true), None)];
        assert_eq!(unready_claim_names(&half), vec!["a-pg".to_string()]);
        // not ready (with secret) → unready.
        let not_ready = vec![ready_claim("a-pg", Some(false), Some("a-pg-conn"))];
        assert_eq!(unready_claim_names(&not_ready), vec!["a-pg".to_string()]);
        // status missing entirely → unready.
        let no_status = vec![ready_claim("a-pg", None, None)];
        assert_eq!(unready_claim_names(&no_status), vec!["a-pg".to_string()]);
        // multi-claim partial → returns only the unready name(s).
        let partial = vec![
            ready_claim("a-pg", Some(true), Some("a-pg-conn")),
            ready_claim("a-redis", Some(false), None),
        ];
        assert_eq!(unready_claim_names(&partial), vec!["a-redis".to_string()]);
    }

    #[test]
    fn build_resource_claim_paused_status_sets_phase_and_conditions() {
        let mut app = Application::new("parser", ApplicationSpec::default());
        app.metadata.generation = Some(4);
        app.status = Some(ApplicationStatus {
            phase: Some("Ready".into()),
            observed_generation: Some(3),
            conditions: None,
            endpoint_url: Some("http://parser.demo.svc.cluster.local:80".into()),
        });
        let status = build_resource_claim_paused_status(&app, &["parser-pg".to_string()]);
        assert_eq!(status.phase.as_deref(), Some(PHASE_AWAITING_RESOURCE_CLAIM));
        // observedGeneration + endpointURL preserved (mirror migration gate).
        assert_eq!(status.observed_generation, Some(4));
        assert_eq!(
            status.endpoint_url.as_deref(),
            Some("http://parser.demo.svc.cluster.local:80")
        );
        let conds = status.conditions.as_ref().expect("conditions");
        let ready = conds.iter().find(|c| c.type_ == "Ready").expect("ready");
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "ResourceClaimPending");
        let pending = conds
            .iter()
            .find(|c| c.type_ == COND_RESOURCE_CLAIM_PENDING)
            .expect("resource claim pending");
        assert_eq!(pending.status, "True");
        assert!(pending.message.contains("parser-pg"));
    }

    #[test]
    fn build_resource_claim_paused_status_preserves_endpoint_when_status_absent() {
        let app = Application::new("parser", ApplicationSpec::default());
        let status = build_resource_claim_paused_status(&app, &["parser-pg".to_string()]);
        assert!(status.endpoint_url.is_none());
        assert_eq!(status.phase.as_deref(), Some(PHASE_AWAITING_RESOURCE_CLAIM));
    }

    #[test]
    fn resource_claim_pending_condition_preserves_transition_time_when_already_true() {
        let prior = vec![ApplicationCondition {
            type_: COND_RESOURCE_CLAIM_PENDING.into(),
            status: "True".into(),
            last_transition_time: "2026-06-01T12:00:00+00:00".into(),
            reason: "ResourceClaimPending".into(),
            message: "old".into(),
            observed_generation: None,
        }];
        let next = resource_claim_pending_condition(&["parser-pg".to_string()], &prior);
        assert_eq!(next.last_transition_time, "2026-06-01T12:00:00+00:00");
        assert!(next.message.contains("parser-pg"));
    }

    #[test]
    fn resource_claim_pending_condition_bumps_transition_time_when_status_changes() {
        // Prior condition was False (or absent) → fresh timestamp.
        let prior = vec![ApplicationCondition {
            type_: COND_RESOURCE_CLAIM_PENDING.into(),
            status: "False".into(),
            last_transition_time: "2026-06-01T12:00:00+00:00".into(),
            reason: "ResourceClaimPending".into(),
            message: "old".into(),
            observed_generation: None,
        }];
        let next = resource_claim_pending_condition(&["parser-pg".to_string()], &prior);
        assert_ne!(next.last_transition_time, "2026-06-01T12:00:00+00:00");
        assert_eq!(next.status, "True");
    }

    /// Rebuild a ResourceClaim from a generated apply payload so the
    /// generate → re-fetch → readiness → pause composition can be
    /// exercised purely (no kube Client). Mirrors what the apiserver
    /// would hand back, plus the provisioner-written status.
    fn claim_from_payload(payload: &Value, status: Option<ResourceClaimStatus>) -> ResourceClaim {
        let name = payload["metadata"]["name"].as_str().unwrap().to_string();
        let type_ = payload["spec"]["type"].as_str().unwrap().to_string();
        let selector: BTreeMap<String, String> =
            serde_json::from_value(payload["spec"]["selector"].clone()).unwrap();
        let mut c = ResourceClaim::new(
            &name,
            ResourceClaimSpec {
                type_,
                selector,
                size: None,
            },
        );
        c.metadata.namespace = Some("demo".into());
        c.status = status;
        c
    }

    #[test]
    fn generated_unready_claim_composes_into_pause_status_with_name_in_message() {
        // 2.4d gate composition: generate a claim, simulate the
        // apiserver handing it back WITHOUT provisioner status
        // (status.ready unset), prove the readiness predicate flags
        // it unready and the resulting pause status carries the
        // AwaitingResourceClaim phase + the claim name in the
        // pending-condition message. The full generate → provision →
        // resume loop is the 2.4g real-cluster walk, not a unit.
        let mut needs = BTreeMap::new();
        needs.insert("pg".to_string(), ServiceNeed::default());
        let spec = base_with_needs(needs);
        let payloads = generate_resource_claims(&spec, "parser", "uid-1", "demo");
        assert_eq!(payloads.len(), 1);

        // SSA-split guard, re-asserted at the gate-composition layer:
        // the apply payload must never carry a status key.
        assert!(
            payloads[0].1.get("status").is_none(),
            "claim apply payload must not write status"
        );

        // Fresh claim, no provisioner status yet → unready.
        let fresh = vec![claim_from_payload(&payloads[0].1, None)];
        let unready = unready_claim_names(&fresh);
        assert_eq!(unready, vec!["parser-pg".to_string()]);

        let app = Application::new("parser", ApplicationSpec::default());
        let status = build_resource_claim_paused_status(&app, &unready);
        assert_eq!(status.phase.as_deref(), Some(PHASE_AWAITING_RESOURCE_CLAIM));
        let pending = status
            .conditions
            .as_ref()
            .unwrap()
            .iter()
            .find(|c| c.type_ == COND_RESOURCE_CLAIM_PENDING)
            .expect("pending condition");
        assert!(pending.message.contains("parser-pg"));
    }

    #[test]
    fn generated_ready_claim_clears_the_gate() {
        // The resume half: a claim the provisioner flipped ready
        // (status.ready==true AND connectionSecretRef set) drops out
        // of the unready set, so the gate would NOT pause.
        let mut needs = BTreeMap::new();
        needs.insert("pg".to_string(), ServiceNeed::default());
        let spec = base_with_needs(needs);
        let payloads = generate_resource_claims(&spec, "parser", "uid-1", "demo");
        let provisioned = vec![claim_from_payload(
            &payloads[0].1,
            Some(ResourceClaimStatus {
                provider: Some("pg-integrated".into()),
                connection_secret_ref: Some("parser-pg-conn".into()),
                ready: Some(true),
                conditions: None,
            }),
        )];
        assert!(unready_claim_names(&provisioned).is_empty());
    }

    // ---- 2.4e: resolve ready claims → needs-type → connectionSecretRef ----

    #[test]
    fn resolve_needs_secrets_maps_type_to_connection_secret_ref() {
        // A ready pg claim with a connection secret resolves to
        // {"pg":"parser-pg-conn"}; the map key is `spec.type_`, the
        // value is `status.connectionSecretRef`.
        let claims = vec![ready_claim("parser-pg", Some(true), Some("parser-pg-conn"))];
        let map = resolve_needs_secrets(&claims);
        assert_eq!(map.get("pg").map(String::as_str), Some("parser-pg-conn"));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn resolve_needs_secrets_skips_claim_without_connection_secret_ref() {
        // A claim with no connectionSecretRef (status absent or
        // half-ready) must NOT appear in the map — render then skips
        // its injection. (Post-gate this should not happen, but the
        // resolver stays defensive + pure.)
        let claims = vec![
            ready_claim("parser-pg", Some(true), Some("parser-pg-conn")),
            ready_claim("parser-redis", Some(true), None),
        ];
        let map = resolve_needs_secrets(&claims);
        assert_eq!(map.get("pg").map(String::as_str), Some("parser-pg-conn"));
        assert!(!map.contains_key("redis"));
        assert_eq!(map.len(), 1);
    }
}
