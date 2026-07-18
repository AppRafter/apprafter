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

use chrono::{DateTime, Utc};
use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{LocalObjectReference, ObjectReference, Secret, Service};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, Patch, PatchParams};
use kube::core::GroupVersionKind;
use kube::runtime::controller::{Action, Controller};
use kube::runtime::events::{Event as KubeEvent, EventType, Recorder, Reporter};
use kube::runtime::watcher;
use kube::{Client, Resource, ResourceExt};
use serde_json::{json, Value};
use thiserror::Error;
use tracing::{debug, info, warn};

mod oci_resolve;
mod pull_secret;
use pull_secret::{app_pull_secret_name, pick_pull_credential};

use operator_controllers_sourcecredential::pull_secret_name;
use operator_core::{
    image_repo, resolve_egress_profile, Application, ApplicationBaseSpec, ApplicationCondition,
    ApplicationSpec, ApplicationStatus, DestructiveChange, DiskClaim, EgressProfile, EnvValue,
    Metrics, MigrationPlan, Needs, PlatformStack, PlatformStackValues, ResourceClaim,
    SourceCredential, StatusImage, COND_IMAGE_RESOLVED, COND_MIGRATION_PENDING,
    COND_PUBLIC_ROUTE_READY, COND_RESOURCE_CLAIM_PENDING, PHASE_AWAITING_MIGRATION_APPROVAL,
    PHASE_AWAITING_RESOURCE_CLAIM, PHASE_ENV_SECRET_MISSING,
};
// 2.16b Task 11: the app-scope migration classifier is now WIRED into
// `reconcile_application`'s state machine (was a deferred "feature flag"
// import through B.1.77). `detect_destructive` diffs the effective spec
// against the stamped `status.lastAppliedSpec` baseline; `create_plan_for`
// builds the gating MigrationPlan in the app's own namespace.
use operator_controllers_migration::ApplicationMigrationStrategy;
use operator_rendering::{
    default_target, effective_spec, owner_reference, render_application_for_env, ConnectionTarget,
    DiskMount,
};

/// Resource kind label used for every metric tagged with `kind`.
const KIND: &str = "Application";

/// SSA field manager. Each apply / status-patch tags the fields it
/// owns under this name, so future controllers (e.g. operators
/// extending Applications) can co-own without conflicts.
pub const FIELD_MANAGER: &str = "apprafter-operator";

/// `reportingController` stamped on the Kubernetes Events this controller
/// publishes (2.16b `SoftDestructiveChange`). Mirrors the pattern in the
/// scheduler / provisioner / platform-stack controllers.
const EVENT_REPORTER_CONTROLLER: &str = "apprafter-application-controller";

/// Minimum interval between registry HEAD probes for the SAME tag
/// (2.4h Fix 1 / ADR 0040). The controller reconciles on a 60s requeue
/// AND on `.owns(ResourceClaim)` / `.owns(Deployment)` watch events, so
/// without this throttle it would issue a registry HEAD on every step.
/// Mirrors `MIN_OCI_POLL_INTERVAL_SECS` in the PlatformController. A tag
/// change or an elapsed interval re-arms resolution; intermediate
/// reconciles reuse the cached `status.image.resolved`.
const MIN_IMAGE_RESOLVE_INTERVAL_SECS: i64 = 60;

/// Per-controller reconcile context.
pub struct Context {
    pub client: Client,
    pub metrics: Arc<Metrics>,
    /// HTTP seam for the 2.4h OCI tag→digest resolution. A single
    /// `ReqwestHttp` built at startup and reused — its inner
    /// `reqwest::Client` pools connections across reconciles.
    pub oci_http: oci_resolve::ReqwestHttp,
    /// Whether the `ciliumnetworkpolicies.cilium.io` CRD is served on this
    /// cluster (2.10 / ADR 0045). Probed ONCE at operator startup (see
    /// `apprafter-operator`'s `cilium_available`) and stored here so the
    /// reconcile loop can gate the egress-CNP SSA apply: e2e / kindnet
    /// clusters have no Cilium, and applying a `CiliumNetworkPolicy` there
    /// 404s every reconcile. When `false`, the controller renders the CNP
    /// the same way but skips the apply.
    pub cilium_available: bool,
    /// Whether the `httproutes.gateway.networking.k8s.io` CRD is served on this
    /// cluster (1.83b). Probed ONCE at startup; gates the HTTPRoute SSA apply +
    /// prune (a non-Gateway-API cluster renders the route but skips the apply).
    pub gateway_api_available: bool,
}

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("kube-rs error: {0}")]
    Kube(#[from] kube::Error),

    #[error("serde_json error: {0}")]
    Serde(#[from] serde_json::Error),

    /// 2.16b Task 11: an Application without a `metadata.uid` reached the
    /// MigrationPlan-creation path. The uid is required for the plan's
    /// controller `ownerReference` (so the plan cascades on Application
    /// delete); the apiserver always assigns one, so this is defensive —
    /// surface it rather than emit an owner-less plan.
    #[error("Application {0} has no metadata.uid; cannot own a MigrationPlan")]
    MissingUid(String),
}

/// Spawn the Application Controller. Watches `apprafter.io/v1alpha1`
/// `Application` resources cluster-wide and reconciles them through
/// [`reconcile`]. Errors from individual reconcile calls go through
/// [`error_policy`].
pub async fn run(
    client: Client,
    metrics: Arc<Metrics>,
    cilium_available: bool,
    gateway_api_available: bool,
) -> Result<(), ReconcileError> {
    let apps: Api<Application> = Api::all(client.clone());
    // 2.4d: watch child ResourceClaims so the provisioner flipping a
    // claim ready re-enqueues the owning Application immediately
    // (resume from the AwaitingResourceClaim pause). Clone the client
    // BEFORE it moves into Context.
    let claims: Api<ResourceClaim> = Api::all(client.clone());
    // 2.16b (R3-mn-a): watch the app-scope MigrationPlan children so a plan
    // reaching `completed` (Task 8: a same-ns child with a controlling
    // ownerRef → the Application) re-fires the owning Application reconcile
    // IMMEDIATELY (instant consume → ConsumeApply), instead of waiting for
    // the 30s paused-arm requeue. Mirrors the `.owns(claims)` shape below.
    let plans: Api<MigrationPlan> = Api::all(client.clone());
    let context = Arc::new(Context {
        client,
        metrics,
        oci_http: oci_resolve::ReqwestHttp::new(),
        cilium_available,
        gateway_api_available,
    });

    Controller::new(apps, watcher::Config::default())
        .owns(claims, watcher::Config::default())
        .owns(plans, watcher::Config::default())
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

    info!(%name, %namespace, env = ?app.spec.environment, "reconciling Application");

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

    let pp = PatchParams::apply(FIELD_MANAGER).force();

    // ---- 2.16b Task 11: app-scope migration state machine ----
    // Wired at the former B.1.77 pause-gate site: runs BEFORE the 2.4d
    // needs gate + the render, so a destructive edit pauses the app
    // before any child is re-applied. The machine is a pure function of
    // "did we detect a destructive change vs the stamped baseline?" ×
    // the live plan's `PlanState` (see `decide`); this block only does
    // the async I/O each arm dictates.
    //
    // The (app, env) key + a fresh plan name. `env` is the app's active
    // environment (`spec.environment`) or the empty string for the
    // base/default env — the same value `create_plan_for` / the plan
    // scope carry, and a wildcard-compatible key for the
    // `spec.environment.as_deref()` (None) search below.
    let env_owned = app.spec.environment.clone().unwrap_or_default();
    let env = env_owned.as_str();
    let key: PlanKey = (name.clone(), env_owned.clone());

    // 1. Effective diff, EACH SIDE UNDER ITS OWN ENVIRONMENT (H4/R2-M2).
    // A missing baseline (never applied yet, or a pre-2.16b app) does
    // NOT gate — detection is skipped and the render stamps the first
    // baseline below.
    let new_eff = effective_spec(&app, app.spec.environment.as_deref());
    let baseline_spec = app
        .status
        .as_ref()
        .and_then(|s| s.last_applied_spec.clone());
    // 2.16b (R3-mn-d): SOFT-destructive notes for the *non-gated* path. When
    // there IS a baseline but NO hard change, an edit may still be
    // soft-destructive (env literal removal, selector/size change, image tag
    // change, scale-down) — those roll through un-gated but earn a
    // `SoftDestructiveChange` Event so operators notice. Computed here where
    // both effective specs are in scope; emitted best-effort after the apply
    // succeeds (only when `change.is_none()`).
    let mut change: Option<DestructiveChange> = None;
    let mut soft_notes: Vec<String> = Vec::new();
    if let Some(baseline_spec) = &baseline_spec {
        let old_eff = effective_baseline(baseline_spec);
        change = ApplicationMigrationStrategy::detect_destructive(&old_eff, &new_eff);
        if change.is_none() {
            soft_notes = soft_destructive_notes(&old_eff, &new_eff);
        }
    }

    // 2. Find the (at most one) key-matching plan of ANY phase and bucket
    // it against the change. `find_any_key_plan` (no blocking filter) is
    // load-bearing: the state machine must SEE a `completed` plan (→
    // ConsumeApply) and a `rejected`/relic plan (→ cleanup), not just live
    // gating ones. It searches the app namespace (2.16b) and wildcards on a
    // `None` environment. `plan_state` needs a `DestructiveChange` to
    // compare triggers, so the detect=None arm buckets by presence only
    // (`plan_state_no_change`).
    let plan = find_any_key_plan(
        &ctx.client,
        &name,
        &namespace,
        app.spec.environment.as_deref(),
    )
    .await?;
    let state = match &change {
        Some(c) => plan_state(plan.as_ref(), c),
        None => plan_state_no_change(plan.as_ref()),
    };

    // 3. Decide + act. The render arms (`Render` / `ConsumeApply` /
    // `DeleteThenRender`) fall through to the 2.4d needs gate + render
    // below and ALWAYS stamp the baseline after a successful apply; the
    // paused arms write a paused status and return here. `consume_plan`
    // names the plan to delete AFTER the render+stamp (crash-ordering:
    // render → stamp → delete) — set only by `ConsumeApply`.
    let mut consume_plan: Option<String> = None;
    match decide(change.is_some(), state) {
        MigrationDecision::Render => {
            // No change + no plan → render normally and stamp the
            // (possibly first) baseline below.
        }
        MigrationDecision::CreatePlan => {
            let change = change.expect("CreatePlan implies a detected change");
            let mp = ApplicationMigrationStrategy::create_plan_for(
                &change,
                &plan_name(&name, env, Utc::now()),
                &namespace,
                &name,
                env,
                app_uid_or(&app)?,
            );
            let plan_name = mp.name_any();
            info!(
                %name, %namespace, plan = %plan_name, trigger = %change.trigger_type,
                "destructive change detected — creating gating MigrationPlan"
            );
            ssa_apply_plan(&ctx.client, &namespace, &mp, &pp).await?;
            let status = build_paused_status(&app, &namespace, &plan_name);
            apply_status(&ctx.client, &namespace, &name, &status, &pp).await?;
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, &namespace, "paused"])
                .inc();
            return Ok(Action::requeue(Duration::from_secs(30)));
        }
        MigrationDecision::NoOp => {
            // Change + a matching blocking plan already gates → stay
            // paused, do not re-apply children.
            let plan_name = plan.as_ref().map(|p| p.name_any()).unwrap_or_default();
            info!(
                %name, %namespace, plan = %plan_name,
                "destructive change already gated by a matching MigrationPlan — staying paused"
            );
            let status = build_paused_status(&app, &namespace, &plan_name);
            apply_status(&ctx.client, &namespace, &name, &status, &pp).await?;
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, &namespace, "paused"])
                .inc();
            return Ok(Action::requeue(Duration::from_secs(30)));
        }
        MigrationDecision::DeleteThenCreate => {
            // The lingering plan gates a DIFFERENT change (stale gate /
            // relic) → delete every key plan, then create the right one.
            let change = change.expect("DeleteThenCreate implies a detected change");
            delete_all_key_plans_except(&ctx.client, &namespace, &key, None).await?;
            let mp = ApplicationMigrationStrategy::create_plan_for(
                &change,
                &plan_name(&name, env, Utc::now()),
                &namespace,
                &name,
                env,
                app_uid_or(&app)?,
            );
            let plan_name = mp.name_any();
            info!(
                %name, %namespace, plan = %plan_name, trigger = %change.trigger_type,
                "superseding stale/relic MigrationPlan with a fresh gating plan"
            );
            ssa_apply_plan(&ctx.client, &namespace, &mp, &pp).await?;
            let status = build_paused_status(&app, &namespace, &plan_name);
            apply_status(&ctx.client, &namespace, &name, &status, &pp).await?;
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, &namespace, "paused"])
                .inc();
            return Ok(Action::requeue(Duration::from_secs(30)));
        }
        MigrationDecision::ConsumeApply => {
            // The change's plan completed → consume the migration
            // result: render + stamp, THEN delete the plan (crash
            // ordering: a crash after stamp re-enters as
            // detect=None×completed → DeleteThenRender cleanup; a crash
            // before stamp re-enters as Some×completed-match → idempotent
            // re-apply).
            consume_plan = plan.as_ref().and_then(|p| p.metadata.name.clone());
            info!(
                %name, %namespace, plan = ?consume_plan,
                "MigrationPlan completed — consuming result and applying children"
            );
        }
        MigrationDecision::DeleteThenRender => {
            // No change but a plan lingers → delete the stale plan(s),
            // then render normally and re-stamp the baseline below.
            info!(
                %name, %namespace,
                "no destructive change but a stale MigrationPlan lingers — cleaning up before render"
            );
            delete_all_key_plans_except(&ctx.client, &namespace, &key, None).await?;
        }
        MigrationDecision::BlockFailed => {
            // The change's plan is `failed` → keep gating; surface a
            // `MigrationFailed=True` condition requiring manual delete.
            let plan_name = plan.as_ref().map(|p| p.name_any()).unwrap_or_default();
            warn!(
                %name, %namespace, plan = %plan_name,
                "gating MigrationPlan is in phase=failed — staying paused, manual delete required"
            );
            let status = build_migration_failed_status(&app, &plan_name);
            apply_status(&ctx.client, &namespace, &name, &status, &pp).await?;
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, &namespace, "paused"])
                .inc();
            return Ok(Action::requeue(Duration::from_secs(30)));
        }
    }

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
    let mut needs_secrets: BTreeMap<(String, Option<String>), String> = BTreeMap::new();
    // 2.6b (ADR 0043): ready `needs.disk` claims resolved into renderer
    // mount input, built from the SAME `current` ready claims the gate
    // below validated (AFTER the gate) — empty unless execution falls
    // through to the render with every claim ready.
    let mut disk_mounts: Vec<DiskMount> = Vec::new();
    let effective = effective_spec(&app, app.spec.environment.as_deref());
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
        // 2.6b: resolve ready disk claims into renderer mount input (PVC
        // name from each claim's status.volumeClaimRef × the entry's
        // mountPath/readOnly). Disk claims carry no connection Secret, so
        // this is a parallel resolution off the SAME `current` claims.
        disk_mounts = resolve_disk_mounts(&effective, &name, &current);
    }

    // ---- 2.12e (ADR 0046 Decision #4): env secret-ref existence check ----
    // After the claim gate (all claim Secrets guaranteed ready), verify every
    // `env` `secret` ref points at an existing Secret+key in the app namespace.
    // Claim refs and literals are skipped — they're either gated already or have
    // no Secret dependency. If any secret ref is unresolvable, set
    // `Ready=False/EnvSecretMissing` and requeue in 30s. Do NOT render or apply
    // children in this case — the missing Secret may not exist yet / ever.
    if let Some(env) = effective.env.as_ref() {
        let missing = check_env_secret_refs(env, &ctx.client, &namespace).await?;
        if !missing.is_empty() {
            info!(
                %name, %namespace,
                missing = ?missing,
                "env secret refs unresolved — setting Ready=False/EnvSecretMissing"
            );
            let status = build_env_secret_missing_status(&app, &missing);
            apply_status(&ctx.client, &namespace, &name, &status, &pp).await?;
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, &namespace, "paused"])
                .inc();
            return Ok(Action::requeue(Duration::from_secs(30)));
        }
    }

    // ---- 2.4h-d (ADR 0040): resolve base.image's tag → registry digest ----
    // so a moved tag auto-rolls the Deployment. Best-effort: ANY failure
    // (registry unreachable, missing credential, malformed reference)
    // renders the verbatim tag + sets ImageResolved=False and NEVER blocks
    // the rollout. `imagePolicy.resolve: "off"` skips the poll entirely —
    // no I/O, no ImageResolved condition.
    //
    // 2.4h Fix 1 (C): list SourceCredentials ONCE here and thread the
    // result into both the resolution auth-pick AND `attach_pull_secret`
    // below — the old code listed twice per reconcile. Best-effort
    // (`unwrap_or_default`): a cred-list failure must not break resolution
    // nor block the rollout, so it degrades to an empty list (anonymous
    // resolve + no pull-secret attach, retried next reconcile).
    let source_creds = list_source_credentials(&ctx.client)
        .await
        .unwrap_or_default();

    let mut image_status: Option<StatusImage> = None;
    let mut image_resolved_cond: Option<(bool, String)> = None; // (ok, reason/msg)
    let resolved_image: Option<String> = match effective.image.as_deref() {
        Some(tag) if image_resolution_enabled(&effective) => {
            // 2.4h Fix 1 (A): throttle the registry HEAD. The controller
            // reconciles on a 60s requeue AND on child-watch events, so a
            // HEAD per reconcile would hammer the registry. Skip the poll
            // when the prior status.image already resolved THIS tag within
            // the throttle window — reuse the cached digest and carry the
            // prior status (resolvedAt) forward unchanged.
            let prior_image = app.status.as_ref().and_then(|s| s.image.as_ref());
            if !should_resolve_image(
                prior_image,
                tag,
                Utc::now(),
                MIN_IMAGE_RESOLVE_INTERVAL_SECS,
            ) {
                let prior = prior_image.expect("should_resolve_image=false implies Some(prior)");
                image_status = Some(prior.clone());
                image_resolved_cond = Some((true, "Resolved".into()));
                ctx.metrics
                    .image_resolve_total
                    .with_label_values(&["cached"])
                    .inc();
                prior.resolved.clone()
            } else {
                let auth = match pick_pull_credential(tag, &source_creds) {
                    Some(cred) => read_cred_auth(&ctx.client, cred, tag).await,
                    None => oci_resolve::RegistryAuth::Anonymous,
                };
                match oci_resolve::resolve_digest(&ctx.oci_http, tag, &auth).await {
                    Ok(resolved) => {
                        image_status = Some(StatusImage {
                            tag: Some(tag.to_string()),
                            resolved: Some(resolved.clone()),
                            resolved_at: Some(Utc::now().to_rfc3339()),
                        });
                        image_resolved_cond = Some((true, "Resolved".into()));
                        ctx.metrics
                            .image_resolve_total
                            .with_label_values(&["ok"])
                            .inc();
                        Some(resolved)
                    }
                    Err(e) => {
                        warn!(image = tag, error = %e, "image digest resolution failed; rendering verbatim tag");
                        // 2.4h Fix 1 (B): record the attempted tag (with no
                        // resolved digest) for auditability per ADR 0040 —
                        // status.image surfaces WHAT we tried even on failure.
                        image_status = Some(StatusImage {
                            tag: Some(tag.to_string()),
                            resolved: None,
                            resolved_at: None,
                        });
                        image_resolved_cond = Some((false, format!("ResolveFailed: {e}")));
                        ctx.metrics
                            .image_resolve_total
                            .with_label_values(&["failed"])
                            .inc();
                        None // fall back to verbatim tag — rollout proceeds
                    }
                }
            }
        }
        // No image, or `resolve: off` — render the verbatim reference,
        // emit no ImageResolved condition.
        _ => None,
    };

    // ---- 2.10 (ADR 0045): resolve the egress CNP render inputs ----
    // The cluster-wide profile from the singleton PlatformStack (absent /
    // unreadable → the documented `Internet` default) and the static
    // connection-target catalog (namespace overrides from
    // ServiceProvider.spec.config — an empty map + the static defaults at
    // launch). The catalog covers ONLY the effective needs' network types
    // (disk entries carry no target). Both are threaded into the pure
    // renderer, which emits the CNP into `rendered.network_policy`; the
    // controller SSA-applies it below ONLY when Cilium is present.
    let egress_profile = read_egress_profile(&ctx.client).await;
    let service_types: Vec<String> = effective
        .needs
        .as_ref()
        .map(|n| {
            n.entries()
                .into_iter()
                .filter(|(_, entry)| entry.disk.is_none())
                .map(|(ty, _)| ty)
                .collect()
        })
        .unwrap_or_default();
    let needs_targets = resolve_needs_targets(&service_types, &BTreeMap::new());

    let mut rendered = render_application_for_env(
        &app,
        app.spec.environment.as_deref(),
        if needs_secrets.is_empty() {
            None
        } else {
            Some(&needs_secrets)
        },
        resolved_image.as_deref(),
        if disk_mounts.is_empty() {
            None
        } else {
            Some(&disk_mounts)
        },
        egress_profile,
        Some(&needs_targets),
    );

    // Seam A (1.79c S3): if a SourceCredential covers this image's
    // registry, project its derived pull-secret into the workload
    // namespace and attach it to the Deployment's imagePullSecrets.
    // Threads the single hoisted `source_creds` list (Fix 1 (C)).
    attach_pull_secret(
        &ctx.client,
        &namespace,
        &mut rendered.deployment,
        &source_creds,
        &pp,
    )
    .await?;

    apply_deployment(&ctx.client, &namespace, &rendered.deployment, &pp).await?;

    if let Some(service) = &rendered.service {
        apply_service(&ctx.client, &namespace, service, &pp).await?;
    }

    // 2.10 (ADR 0045): SSA-apply the per-Application egress CNP — but ONLY
    // when Cilium is present. The probe ran once at startup; on a
    // non-Cilium cluster (e2e / kindnet) the `ciliumnetworkpolicies.cilium.io`
    // CRD is unserved and the apply would 404 every reconcile. The CNP
    // carries the SAME Application ownerRef the Deployment/Service do, so it
    // cascades on Application delete.
    if ctx.cilium_available {
        if let Some(cnp) = rendered.network_policy.take() {
            let owner = owner_reference(&app);
            apply_network_policy(&ctx.client, &namespace, &owner, cnp).await?;
        }
    } else {
        debug!(
            %name, %namespace,
            "Cilium not detected on this cluster; skipping egress CiliumNetworkPolicy apply"
        );
    }

    // 1.83b: SSA-apply the per-Application HTTPRoute when the app is public —
    // but ONLY when the Gateway-API CRDs are served (the probe ran once at
    // startup; a non-Gateway-API cluster would 404). When NOT public (or
    // unset) PRUNE any stale route (the app flipped public → internal). The
    // route carries the SAME Application ownerRef → cascades on app delete.
    if ctx.gateway_api_available {
        if let Some(route) = rendered.httproute.take() {
            let owner = owner_reference(&app);
            apply_http_route(&ctx.client, &namespace, &owner, route).await?;
        } else {
            prune_http_route(&ctx.client, &namespace, &name).await?;
        }
    }

    // 1.83b: a public app's endpoint is its public HTTPS URL (the first
    // hostname); otherwise the internal cluster-DNS Service URL.
    let public_hostnames: Vec<String> = effective
        .expose
        .as_ref()
        .filter(|e| e.network.as_deref() == Some("public"))
        .and_then(|e| e.hostname.as_ref())
        .map(|h| h.as_slice_vec())
        .unwrap_or_default();
    let endpoint_url = if let Some(first) = public_hostnames.first() {
        Some(format!("https://{first}/"))
    } else {
        rendered
            .service
            .as_ref()
            .and_then(|s| s.metadata.name.as_deref())
            .map(|svc_name| cluster_internal_endpoint_url(svc_name, &namespace, 80))
    };

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
    let mut conditions = vec![ready_condition(
        "True",
        "ReconcileSucceeded",
        "Reconcile completed; child Deployment and Service applied.",
        previous_conditions,
    )];
    // 2.4h-d: emit ImageResolved only when resolution actually ran
    // (`imagePolicy.resolve` != "off" and an image is set). `resolve: off`
    // leaves both `image_resolved_cond` and `image_status` None → no
    // condition, no status.image.
    if let Some((ok, reason)) = &image_resolved_cond {
        conditions.push(image_resolved_condition(*ok, reason, previous_conditions));
    }
    // 1.83b: soft PublicRouteReady condition for a public app — zone coverage
    // + the route's own Accepted/ResolvedRefs. NEVER gates Ready; the route is
    // always emitted regardless of the verdict.
    if !public_hostnames.is_empty() {
        let allowed_domains = read_allowed_domains(&ctx.client).await;
        let route_status = if ctx.gateway_api_available {
            read_http_route_status(&ctx.client, &namespace, &name).await
        } else {
            None
        };
        conditions.push(public_route_ready_condition(
            &public_hostnames,
            &allowed_domains,
            route_status.as_ref(),
            previous_conditions,
        ));
    }
    let mut status = build_status(&app, "Ready", conditions, endpoint_url);
    status.image = image_status;
    // 2.16b Task 11: stamp `status.lastAppliedSpec = spec` (the RAW current
    // spec) after the successful render+apply. Only the render arms
    // (`Render` / `ConsumeApply` / `DeleteThenRender`) reach here — every
    // paused arm returned early above — so an unconditional stamp is
    // correct: a gated app never reaches this line and keeps its prior
    // baseline. The classifier diffs the next reconcile's effective spec
    // against this baseline. Folding the stamp into THIS status write means
    // the child apply (above) precedes the stamp — satisfying the
    // crash-order render → stamp → delete for `ConsumeApply`. A crash before
    // this write re-enters as an idempotent re-apply of the same spec.
    status = with_stamped_baseline(status, &app.spec);
    apply_status(&ctx.client, &namespace, &name, &status, &pp).await?;

    // 2.16b (R3-mn-d): a non-gated but SOFT-destructive edit rolled through —
    // publish ONE `SoftDestructiveChange` Event on the Application so
    // operators notice. `soft_notes` is populated ONLY on the `change.is_none()`
    // path (see the diff block above), so reaching here with a non-empty list
    // means a soft-destructive, un-gated edit was applied. Best-effort: a
    // publish failure is logged, never fatal.
    if !soft_notes.is_empty() {
        let note = format!(
            "soft-destructive changes applied (not gated): {}",
            soft_notes.join("; ")
        );
        let recorder = build_recorder(&ctx.client, &app);
        let ev = KubeEvent {
            type_: EventType::Normal,
            reason: "SoftDestructiveChange".into(),
            note: Some(note),
            action: "Reconcile".into(),
            secondary: None,
        };
        if let Err(e) = recorder.publish(ev).await {
            warn!(%name, %namespace, error = %e, "failed to publish SoftDestructiveChange event (continuing)");
        }
    }

    // 2.16b Task 11: `ConsumeApply` — delete the completed plan AFTER the
    // render + baseline stamp landed (crash-order render → stamp → delete).
    // A crash between the stamp and this delete re-enters as
    // detect=None × completed-plan → `DeleteThenRender`, which cleans the
    // relic up; so the delete is safe to be the last step. Best-effort
    // (404-tolerant) via `delete_all_key_plans_except`.
    if let Some(plan_to_delete) = &consume_plan {
        delete_all_key_plans_except(
            &ctx.client,
            &namespace,
            &key,
            // keep = None → delete every key plan incl. the consumed one.
            None,
        )
        .await?;
        info!(%name, %namespace, plan = %plan_to_delete, "consumed MigrationPlan deleted after apply");
    }

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

    // One-time selector migration. `spec.selector` is IMMUTABLE, so a
    // Deployment created under an older selector label-set can never be
    // SSA-updated to a new one — the apiserver 422s ("field is immutable")
    // every reconcile and the workload is wedged (no image roll, no spec
    // change). Walk-found: 2.9 widened the selector label-set and stranded
    // every pre-2.9 Deployment. When the live selector differs from the
    // rendered (now stable+minimal) one, delete the Deployment (cascade) so
    // the `.owns(Deployment)` watch re-fires and the next reconcile recreates
    // it cleanly. One-time, brief downtime, only for mismatched Deployments.
    if let Some(existing) = api.get_opt(&name).await? {
        if selector_needs_migration(&existing, deployment) {
            warn!(
                deployment = %name,
                namespace = %namespace,
                "Deployment selector changed (immutable field) — deleting to recreate with the stable minimal selector (one-time migration)"
            );
            api.delete(&name, &DeleteParams::default()).await?;
            // Skip the apply this cycle: recreating while the old object is
            // terminating races the name. The delete fires the
            // `.owns(Deployment)` watch → the next reconcile recreates it.
            return Ok(());
        }
    }

    let payload = into_apply_payload("apps/v1", "Deployment", deployment)?;
    api.patch(&name, pp, &Patch::Apply(&payload)).await?;
    Ok(())
}

/// True when an existing Deployment's IMMUTABLE `spec.selector.matchLabels`
/// differs from the rendered one — so it must be delete+recreated (the
/// selector cannot change in place). Walk-found: 2.9 widened the selector
/// label-set, wedging every Deployment created before it.
fn selector_needs_migration(existing: &Deployment, desired: &Deployment) -> bool {
    let match_labels = |d: &Deployment| {
        d.spec
            .as_ref()
            .and_then(|s| s.selector.match_labels.clone())
            .unwrap_or_default()
    };
    match_labels(existing) != match_labels(desired)
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

// ---- 2.10 (ADR 0045): egress CiliumNetworkPolicy apply + profile read ----

/// Namespace + name of the singleton `PlatformStack` (CLI-bootstrap-seeded
/// in `apprafter-system`; see `cluster_bootstrap.rs`). Duplicated rather
/// than imported from the platform-stack controller crate to avoid a
/// circular workspace-internal dep.
const PLATFORM_STACK_NAMESPACE: &str = "apprafter-system";
const PLATFORM_STACK_NAME: &str = "default";

/// `ApiResource` for the externally-installed Cilium `CiliumNetworkPolicy`
/// CRD (group `cilium.io`, version `v2`). Cilium is a bootstrap dependency
/// installed before the operator, so on a Cilium cluster the CRD is already
/// Established; the [`Context::cilium_available`] probe gates the apply on
/// non-Cilium clusters.
fn cnp_api_resource() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk(
        "cilium.io",
        "v2",
        "CiliumNetworkPolicy",
    ))
}

/// Read the cluster-wide egress profile from the singleton PlatformStack
/// (2.10 / ADR 0045). Best-effort: a missing CR (`get_opt` → `None`) or any
/// read error degrades to the documented default `EgressProfile::Internet`
/// — the egress posture must never break a reconcile, and "absent" is the
/// documented default, not "empty". This is a READ only; the controller
/// never writes PlatformStack spec/status here (the PlatformController owns
/// it under its own field manager).
async fn read_egress_profile(client: &Client) -> EgressProfile {
    let api: Api<PlatformStack> = Api::namespaced(client.clone(), PLATFORM_STACK_NAMESPACE);
    match api.get_opt(PLATFORM_STACK_NAME).await {
        Ok(Some(ps)) => resolve_egress_profile(&ps.spec),
        Ok(None) => EgressProfile::Internet,
        Err(e) => {
            warn!(error = %e, "could not read PlatformStack egress profile; defaulting to internet");
            EgressProfile::Internet
        }
    }
}

/// SSA-apply the per-Application egress `CiliumNetworkPolicy` as a
/// `DynamicObject` (cilium.io/v2). Sets `metadata.namespace` (the app's
/// namespace) and `metadata.ownerReferences` to the Application (the SAME
/// ownerRef the Deployment/Service carry, so the CNP cascades on Application
/// delete) on the rendered body, then applies under field manager
/// [`FIELD_MANAGER`] (`apprafter-operator` — the operator owns this child,
/// so writing it here keeps the SSA split intact; we never touch
/// ResourceClaim / PlatformStack status from this path).
async fn apply_network_policy(
    client: &Client,
    namespace: &str,
    owner: &OwnerReference,
    mut body: Value,
) -> Result<(), ReconcileError> {
    let name = body
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    body["metadata"]["namespace"] = json!(namespace);
    body["metadata"]["ownerReferences"] = json!([owner]);

    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), namespace, &cnp_api_resource());
    api.patch(
        &name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&body),
    )
    .await?;
    Ok(())
}

// ---- 1.83b: per-Application HTTPRoute apply / prune / status read ----

/// `ApiResource` for the externally-installed Gateway-API `HTTPRoute` CRD
/// (group `gateway.networking.k8s.io`, version `v1`). 1.83b. Gated by
/// [`Context::gateway_api_available`] on clusters without the Gateway-API CRDs.
fn httproute_api_resource() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk(
        "gateway.networking.k8s.io",
        "v1",
        "HTTPRoute",
    ))
}

/// SSA-apply the per-Application `HTTPRoute` as a `DynamicObject`
/// (gateway.networking.k8s.io/v1), mirroring `apply_network_policy`: inject
/// `metadata.namespace` (the app's) + `ownerReferences` (the SAME Application
/// ownerRef the Deployment/Service carry → cascade on delete), then apply under
/// [`FIELD_MANAGER`]. 1.83b.
async fn apply_http_route(
    client: &Client,
    namespace: &str,
    owner: &OwnerReference,
    mut body: Value,
) -> Result<(), ReconcileError> {
    let name = body
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    body["metadata"]["namespace"] = json!(namespace);
    body["metadata"]["ownerReferences"] = json!([owner]);

    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), namespace, &httproute_api_resource());
    api.patch(
        &name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&body),
    )
    .await?;
    Ok(())
}

/// Best-effort delete of a stale `HTTPRoute` named `name` (the app flipped
/// `public → internal`, or removed `expose`). 404-tolerant (a missing route is
/// a no-op — it may never have existed, or already cascaded). 1.83b — the
/// Service has no analogous prune, but the design requires the route disappears
/// when the app stops being public.
async fn prune_http_route(
    client: &Client,
    namespace: &str,
    name: &str,
) -> Result<(), ReconcileError> {
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), namespace, &httproute_api_resource());
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Read the applied `HTTPRoute`'s `status` value for the soft
/// `PublicRouteReady` condition (best-effort; `None` when absent / not yet
/// populated). 1.83b.
async fn read_http_route_status(client: &Client, namespace: &str, name: &str) -> Option<Value> {
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), namespace, &httproute_api_resource());
    api.get_opt(name)
        .await
        .ok()
        .flatten()
        .and_then(|o| o.data.get("status").cloned())
}

/// Read `spec.values.gateway.allowedDomains[].domain` from the singleton
/// PlatformStack (1.83b). Best-effort: a missing CR / read error → empty (the
/// route is still emitted; the soft condition reports `NoMatchingZone`).
async fn read_allowed_domains(client: &Client) -> Vec<String> {
    let api: Api<PlatformStack> = Api::namespaced(client.clone(), PLATFORM_STACK_NAMESPACE);
    match api.get_opt(PLATFORM_STACK_NAME).await {
        Ok(Some(ps)) => allowed_domains_from_values(&ps.spec.values),
        _ => Vec::new(),
    }
}

/// Resolve the connection-target catalog for the effective needs' network
/// service types (2.10 / ADR 0045 §B). For each type, look up its static
/// [`default_target`] (a `None` — e.g. `disk` or an unknown type — yields no
/// entry), then apply a per-type namespace override from `namespace_overrides`
/// when present (the namespace the provisioner reads from
/// `ServiceProvider.spec.config`; an empty map + the static defaults is the
/// launch slice). Pure — the controller threads the result into the renderer.
fn resolve_needs_targets(
    service_types: &[String],
    namespace_overrides: &BTreeMap<String, String>,
) -> BTreeMap<String, ConnectionTarget> {
    let mut targets = BTreeMap::new();
    for service_type in service_types {
        let Some(mut target) = default_target(service_type) else {
            continue;
        };
        if let Some(ns) = namespace_overrides.get(service_type) {
            target.namespace = ns.clone();
        }
        targets.insert(service_type.clone(), target);
    }
    targets
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
///
/// 2.4h Fix 1 (C): takes the pre-listed `creds` slice (the reconcile loop
/// lists SourceCredentials once and threads it here and into the image
/// resolution above) rather than re-listing the API internally.
async fn attach_pull_secret(
    client: &Client,
    namespace: &str,
    deployment: &mut Deployment,
    creds: &[SourceCredential],
    pp: &PatchParams,
) -> Result<(), ReconcileError> {
    let Some(image) = deployment_image(deployment) else {
        return Ok(());
    };
    let Some(cred) = pick_pull_credential(&image, creds) else {
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

/// Load the registry `RegistryAuth` for `image` from a covering
/// SourceCredential's derived `dockerconfigjson` (the canonical
/// `pull_secret_name(cred)` Secret in `apprafter-system`). Best-effort:
/// if the Secret has not been derived yet, can't be read, or has no
/// matching host entry, falls back to `Anonymous` so the resolve still
/// attempts an unauthenticated HEAD (public-image path). Never errors —
/// 2.4h-d treats resolution as non-blocking.
async fn read_cred_auth(
    client: &Client,
    cred: &SourceCredential,
    image: &str,
) -> oci_resolve::RegistryAuth {
    let cred_name = cred.name_any();
    let canonical = pull_secret_name(&cred_name);
    let dcj = match read_dockercfgjson(client, SOURCECRED_NAMESPACE, &canonical).await {
        Ok(Some(dcj)) => dcj,
        _ => return oci_resolve::RegistryAuth::Anonymous,
    };
    let host = match oci_resolve::parse_image_ref(image) {
        Ok(r) => r.host,
        Err(_) => return oci_resolve::RegistryAuth::Anonymous,
    };
    oci_resolve::auth_from_dockerconfigjson(dcj.as_bytes(), &host)
        .unwrap_or(oci_resolve::RegistryAuth::Anonymous)
}

/// 2.12e (ADR 0046 Decision #4): check every `env` `Secret` ref for existence.
///
/// Iterates the env map via [`unresolved_env_secret_refs`] with a real
/// `Api::<Secret>` lookup: for each `secret` ref `<name>/<key>`, reads the
/// Secret in the app namespace with `get_opt` and checks that `key` is present
/// in `.data` or `.string_data`. Returns one message per missing ref in
/// BTreeMap key order (deterministic). An empty vec means all refs resolve.
///
/// `Literal` and `Claim` refs are ignored (see [`unresolved_env_secret_refs`]).
async fn check_env_secret_refs(
    env: &std::collections::BTreeMap<String, operator_core::EnvValue>,
    client: &Client,
    namespace: &str,
) -> Result<Vec<String>, ReconcileError> {
    use operator_core::{EnvRef, EnvValue};
    // Collect the (var_name, secret_name, key) tuples we need to check.
    // Same iteration order as unresolved_env_secret_refs (BTreeMap).
    let mut checks: Vec<(String, String, String)> = Vec::new();
    for (var_name, value) in env.iter() {
        let path = match value {
            EnvValue::Ref(EnvRef::Secret(p)) => p,
            _ => continue,
        };
        let Some(slash) = path.find('/') else {
            continue; // malformed — webhook already rejected these
        };
        checks.push((
            var_name.clone(),
            path[..slash].to_string(),
            path[slash + 1..].to_string(),
        ));
    }
    if checks.is_empty() {
        return Ok(Vec::new());
    }
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let mut missing: Vec<String> = Vec::new();
    for (var_name, secret_name, key) in &checks {
        let exists = match api.get_opt(secret_name).await? {
            Some(secret) => {
                let in_data = secret
                    .data
                    .as_ref()
                    .map(|d| d.contains_key(key.as_str()))
                    .unwrap_or(false);
                let in_string_data = secret
                    .string_data
                    .as_ref()
                    .map(|d| d.contains_key(key.as_str()))
                    .unwrap_or(false);
                in_data || in_string_data
            }
            None => false,
        };
        if !exists {
            missing.push(format!(
                "env {} → secret \"{}/{}\": Secret \"{}\" not found or missing key \"{}\"",
                var_name, secret_name, key, secret_name, key
            ));
        }
    }
    Ok(missing)
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
        image: None,
        // 2.9 (ADR 0044): surface the per-CR active environment so
        // consumers see which `spec.environment` override resolved.
        environment: app.spec.environment.clone(),
        // 2.16b Task 7: `None` here omits `lastAppliedSpec` from this SSA
        // payload (skip_serializing_if), so this status write does not
        // clobber a baseline stamped by the classifier's own apply. The
        // stamp itself lands with a later 2.16b reconcile task.
        last_applied_spec: None,
    }
}

/// 2.16b Task 9 (R1-H1): app-scope MigrationPlans are created in
/// the Application's own namespace (Task 8), so the blocking-plan
/// finder searches *there*, not the platform `apprafter-system`.
/// Pure + trivial for unit-testability of the namespace choice.
fn blocking_plan_namespace(app_ns: &str) -> String {
    app_ns.to_string()
}

/// 2.16b (R3-mn-d): the human notes for SOFT-destructive changes from
/// `old` → `new` (two **effective** specs). Soft-destructive changes are
/// NOT gated (no MigrationPlan) but SHOULD surface a `SoftDestructiveChange`
/// Kubernetes Event so operators notice a potentially-disruptive-but-allowed
/// edit rolled through.
///
/// This is the deliberate COMPLEMENT of
/// [`ApplicationMigrationStrategy::detect_destructive`]: every op the
/// classifier treats as HARD (env-ref removal, needs-removal, image-*repo*
/// change, scale-to-zero, public-visibility/domain change) is intentionally
/// absent here — those pause the app and already surface a MigrationPlan.
/// The soft set is:
///
///   * env **literal** removal → `removed env <KEY> (literal)`
///   * `needs.*.selector` change (deferred — 2.4d) → `changed <key> selector`
///   * `needs.*.size` change (provisioner-guarded — V14) → `changed <key> size`
///   * image **tag** change (repo unchanged) → `image tag <old>→<new>`
///   * scale-DOWN N→M with M>0 → `scaled down <N>→<M>`
///
/// Order is deterministic (env keys by BTreeMap order, then needs by
/// `Needs::entries`' fixed order, then image, then scale) so the joined
/// Event note is stable across reconciles.
fn soft_destructive_notes(old: &ApplicationBaseSpec, new: &ApplicationBaseSpec) -> Vec<String> {
    let mut notes: Vec<String> = Vec::new();

    // Env LITERAL removal (a ref removal is HARD — handled by the classifier).
    let new_env_keys: Vec<&String> = new
        .env
        .as_ref()
        .map(|m| m.keys().collect())
        .unwrap_or_default();
    if let Some(old_env) = old.env.as_ref() {
        for (key, value) in old_env {
            if new_env_keys.contains(&key) {
                continue;
            }
            if let EnvValue::Literal(_) = value {
                notes.push(format!("removed env {key} (literal)"));
            }
        }
    }

    // needs.*.selector / needs.*.size changes on a need present on BOTH sides.
    // (A removed need is HARD; an added need is neutral — neither is soft.)
    let old_map = needs_by_key(old.needs.as_ref());
    let new_map = needs_by_key(new.needs.as_ref());
    for (key, old_need) in &old_map {
        let Some(new_need) = new_map.get(key) else {
            continue; // removed — hard, not soft
        };
        if old_need.selector != new_need.selector {
            notes.push(format!("changed {key} selector"));
        }
        if old_need.size != new_need.size {
            notes.push(format!("changed {key} size"));
        }
    }

    // Image TAG change (repo unchanged; a repo change is HARD). Only when both
    // sides carry an image AND the repository is identical — the differing
    // suffix is the tag.
    if let (Some(old_img), Some(new_img)) = (old.image.as_deref(), new.image.as_deref()) {
        if old_img != new_img && image_repo(old_img) == image_repo(new_img) {
            notes.push(format!("image tag {old_img}→{new_img}"));
        }
    }

    // Scale-DOWN N→M, M>0 (scale-to-zero is HARD; scale-up is neutral).
    // `replicas` resolves to 1 at render time (application.cue), matching the
    // classifier's REPLICAS_RENDER_DEFAULT.
    const REPLICAS_RENDER_DEFAULT: i32 = 1;
    let old_r = old.replicas.unwrap_or(REPLICAS_RENDER_DEFAULT);
    let new_r = new.replicas.unwrap_or(REPLICAS_RENDER_DEFAULT);
    if new_r > 0 && new_r < old_r {
        notes.push(format!("scaled down {old_r}→{new_r}"));
    }

    notes
}

/// 2.16b: build a per-reconcile `Recorder` publishing Events against the
/// given `Application`. Constructing per-reconcile keeps `reconcile` pure;
/// `Recorder::new` is cheap (wires an Api + reference). Mirrors the scheduler
/// / provisioner / platform-stack `build_recorder`.
fn build_recorder(client: &Client, app: &Application) -> Recorder {
    let reporter = Reporter {
        controller: EVENT_REPORTER_CONTROLLER.into(),
        instance: std::env::var("POD_NAME").ok(),
    };
    let reference: ObjectReference = app.object_ref(&());
    Recorder::new(client.clone(), reporter, reference)
}

/// 2.16b: flatten a `Needs` block into a `key → ServiceNeed` map keyed on
/// the stable `needs.<type>[.<name>]` key (matching the classifier's
/// `needs_keys`). Disk entries carry no `ServiceNeed` (`selector`/`size`
/// live on services) so they're skipped — a disk `size` change is handled
/// by the 2.6b PVC-expansion path, not here.
fn needs_by_key(needs: Option<&Needs>) -> BTreeMap<String, operator_core::ServiceNeed> {
    let mut out = BTreeMap::new();
    let Some(needs) = needs else {
        return out;
    };
    for (ty, entry) in needs.entries() {
        let Some(service) = entry.service else {
            continue;
        };
        let key = match entry.name {
            Some(name) => format!("needs.{ty}.{name}"),
            None => format!("needs.{ty}"),
        };
        out.insert(key, service);
    }
    out
}

/// 2.16b Task 11: delete every app-namespace MigrationPlan gating the
/// `(app_name, env)` key EXCEPT `keep` (best-effort). Lists the plans in
/// the app namespace, filters to the key via [`plans_to_delete`], and
/// deletes each — a 404 is tolerated (the plan already cascaded / a
/// concurrent reconcile removed it). This enforces "≤1 live plan per key"
/// (R2-mn4 / R3-M1) and is the supersede (`keep = Some(new_plan)`) /
/// consume / cleanup (`keep = None`) delete. A list failure propagates
/// (the reconcile retries); an individual delete failure that is NOT a 404
/// propagates too so a genuine RBAC / apiserver fault surfaces rather than
/// silently leaving a stale gate.
async fn delete_all_key_plans_except(
    client: &Client,
    app_ns: &str,
    key: &PlanKey,
    keep: Option<&str>,
) -> Result<(), ReconcileError> {
    let (app_name, env) = key;
    let api: Api<MigrationPlan> = Api::namespaced(client.clone(), &blocking_plan_namespace(app_ns));
    let list = api.list(&Default::default()).await?;
    for name in plans_to_delete(&list.items, app_name, env, keep) {
        match api.delete(&name, &DeleteParams::default()).await {
            Ok(_) => {
                info!(%app_name, %env, plan = %name, "deleted superseded/consumed MigrationPlan");
            }
            Err(kube::Error::Api(e)) if e.code == 404 => {
                debug!(plan = %name, "MigrationPlan already gone on delete (404, tolerated)");
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// 2.16b Task 11: SSA-apply a freshly-built MigrationPlan into the app's
/// own namespace (2.16b co-locates the plan with the Application it gates).
/// The plan already carries its `metadata.name`/`namespace` +
/// controller-ownerRef (from [`ApplicationMigrationStrategy::create_plan_for`]);
/// this serializes it, injects the `apiVersion`/`kind` SSA requires, and
/// applies under [`FIELD_MANAGER`] (`apprafter-operator` — the operator owns
/// the plan). The RBAC `migrationplans` ClusterRole grants `create`/`patch`
/// cluster-wide, so the write into any app namespace succeeds.
async fn ssa_apply_plan(
    client: &Client,
    app_ns: &str,
    plan: &MigrationPlan,
    pp: &PatchParams,
) -> Result<(), ReconcileError> {
    let name = plan
        .metadata
        .name
        .as_deref()
        .unwrap_or_default()
        .to_string();
    let payload = into_apply_payload("apprafter.io/v1alpha1", "MigrationPlan", plan)?;
    let api: Api<MigrationPlan> = Api::namespaced(client.clone(), app_ns);
    api.patch(&name, pp, &Patch::Apply(&payload)).await?;
    Ok(())
}

/// Pure application-scope match: the plan targets this exact
/// `(app_name, app_namespace)` and — when `environment` is `Some` —
/// this environment (a `None` environment wildcards). The blocking-vs-not
/// distinction is NOT applied here — the state machine (`plan_state`) needs
/// completed/rejected/failed plans too, and applies `plan_is_blocking` when
/// it buckets. Pure — unit testable without a client.
fn plan_scope_matches(
    plan: &MigrationPlan,
    app_name: &str,
    app_namespace: &str,
    environment: Option<&str>,
) -> bool {
    if plan.spec.scope.type_ != "application" {
        return false;
    }
    let Some(app_scope) = &plan.spec.scope.application else {
        return false;
    };
    if app_scope.ref_.name != app_name || app_scope.ref_.namespace != app_namespace {
        return false;
    }
    match environment {
        Some(e) => app_scope.environment == e,
        None => true,
    }
}

/// 2.16b Task 11: the (at most one) MigrationPlan for this key REGARDLESS
/// of phase — the state machine's bucketer (`plan_state`) needs to see a
/// `completed` plan (→ `ConsumeApply`) and a `rejected`/relic plan (→
/// cleanup). Pure scope match (no blocking filter) — unit testable without
/// a client; [`find_any_key_plan`] wraps it.
fn pick_any_key_plan(
    plans: Vec<MigrationPlan>,
    app_name: &str,
    app_namespace: &str,
    environment: Option<&str>,
) -> Option<MigrationPlan> {
    plans
        .into_iter()
        .find(|plan| plan_scope_matches(plan, app_name, app_namespace, environment))
}

/// 2.16b Task 11: find the (at most one) key-matching MigrationPlan of ANY
/// phase in the app namespace (2.16b app-scope plans live there). Returns
/// completed / rejected / failed plans too (no blocking filter), so
/// [`plan_state`] can bucket them for the `ConsumeApply` / cleanup arms of
/// the state machine.
async fn find_any_key_plan(
    client: &Client,
    app_name: &str,
    app_namespace: &str,
    environment: Option<&str>,
) -> Result<Option<MigrationPlan>, ReconcileError> {
    let api: Api<MigrationPlan> =
        Api::namespaced(client.clone(), &blocking_plan_namespace(app_namespace));
    let list = api.list(&Default::default()).await?;
    Ok(pick_any_key_plan(
        list.items,
        app_name,
        app_namespace,
        environment,
    ))
}

fn plan_is_blocking(plan: &MigrationPlan) -> bool {
    let phase = plan
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("pending-approval");
    !matches!(phase, "completed" | "rejected")
}

/// 2.16b Task 10 (R2-H2): the finer bucket a reconcile needs to
/// pick a decision. Coarser `plan_is_blocking` only answers
/// "does this pause the app"; the state machine also needs to
/// know whether a live/terminal plan MATCHES the current change.
#[derive(Debug, PartialEq)]
pub enum PlanState {
    /// No plan exists for this app+env.
    None,
    /// A blocking (not completed/rejected) plan whose trigger
    /// `(type, field)` equals the current change's — the app is
    /// legitimately paused on THIS change; leave it be.
    BlockingMatch,
    /// A blocking plan whose trigger is for a DIFFERENT change —
    /// stale gate; supersede it with a fresh plan.
    BlockingMismatch,
    /// Phase `failed` — needs operator/user action; keep gating.
    Failed,
    /// Phase `completed` AND trigger matches the current change —
    /// the migration ran, so the render may now consume + apply.
    CompletedMatch,
    /// Any other terminal/stale plan (completed-mismatch,
    /// rejected, unknown phase) — a relic to clean up.
    Relic,
}

/// 2.16b Task 10 (R2-H2 / R3-M1): the reconcile decision for one
/// Application, as a pure function of "did we detect a
/// destructive change this reconcile?" × the live `PlanState`.
/// Total over the (bool × PlanState) product so Task 11's async
/// wiring can never fall through an unhandled cell.
#[derive(Debug, PartialEq)]
pub enum MigrationDecision {
    /// No change + no plan → render children normally.
    Render,
    /// Change detected + no plan → create the gating plan.
    CreatePlan,
    /// Change detected + a matching blocking plan already gates →
    /// nothing to do; stay paused.
    NoOp,
    /// Change detected but the blocking/relic plan is for a
    /// different change → delete it, then create the right plan.
    DeleteThenCreate,
    /// Change detected + its plan already completed → consume the
    /// migration result and apply children.
    ConsumeApply,
    /// No change but a plan lingers (any state) → delete the stale
    /// plan, then render normally.
    DeleteThenRender,
    /// Change detected + its plan is `failed` → keep gating, do
    /// not silently re-plan; surface the failure.
    BlockFailed,
}

/// Pure decision table (2.16b spec state-machine section).
/// See `MigrationDecision` for what each arm means.
pub fn decide(has_change: bool, state: PlanState) -> MigrationDecision {
    use MigrationDecision::*;
    match (has_change, state) {
        // No destructive change this reconcile.
        (false, PlanState::None) => Render,
        // Any lingering plan (blocking/terminal/relic) with no
        // current change → supersede/cleanup, then render.
        (false, _) => DeleteThenRender,
        // Destructive change detected.
        (true, PlanState::None) => CreatePlan,
        (true, PlanState::BlockingMatch) => NoOp,
        (true, PlanState::BlockingMismatch) => DeleteThenCreate,
        (true, PlanState::Failed) => BlockFailed,
        (true, PlanState::CompletedMatch) => ConsumeApply,
        (true, PlanState::Relic) => DeleteThenCreate,
    }
}

/// Bucket a plan (if any) against the current change into a
/// `PlanState`. Blocking/terminal is decided by phase (via
/// `plan_is_blocking`); "match" compares the plan's trigger
/// `(type, field)` to the current change's `(trigger_type,
/// field)` — the two-tuple that identifies WHICH destructive
/// change a plan was cut for.
pub fn plan_state(plan: Option<&MigrationPlan>, current_trigger: &DestructiveChange) -> PlanState {
    let Some(plan) = plan else {
        return PlanState::None;
    };
    let phase = plan
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("pending-approval");
    let trigger_matches = plan.spec.trigger.type_ == current_trigger.trigger_type
        && plan.spec.trigger.field == current_trigger.field;

    if phase == "failed" {
        return PlanState::Failed;
    }
    if plan_is_blocking(plan) {
        // Not completed/rejected/failed → live gate.
        return if trigger_matches {
            PlanState::BlockingMatch
        } else {
            PlanState::BlockingMismatch
        };
    }
    // Terminal (completed | rejected).
    if phase == "completed" && trigger_matches {
        PlanState::CompletedMatch
    } else {
        PlanState::Relic
    }
}

/// 2.16b Task 11: bucket a plan (if any) when NO destructive change was
/// detected this reconcile. `decide` ignores the finer `PlanState`
/// distinctions in the `has_change == false` rows — it only cares
/// "no plan" (`None` → `Render`) vs "some lingering plan" (`_` →
/// `DeleteThenRender`). This helper therefore maps "no plan" → `None`
/// and "any live/terminal/relic plan" → `Relic`, so the caller can feed
/// a single `PlanState` into `decide(false, state)` without needing a
/// `DestructiveChange` to compare triggers against (which it lacks when
/// detection returned `None`).
fn plan_state_no_change(plan: Option<&MigrationPlan>) -> PlanState {
    match plan {
        None => PlanState::None,
        Some(_) => PlanState::Relic,
    }
}

/// 2.16b Task 11: the (app, env) identity key a MigrationPlan gates,
/// used to scope the "≤1 live plan per key" delete set. Held as an owned
/// pair so the async delete helpers can move it around freely.
type PlanKey = (String, String);

/// 2.16b Task 11: synthesize a stable, human-readable MigrationPlan name
/// for an (app, env) pair — `<app>-<env>-migration-<unix-secs>`. The
/// timestamp gives each superseding plan a fresh name so a delete-then-
/// create never collides with the object it just deleted (which may still
/// be terminating). An empty `env` (the base/default env) collapses to
/// `<app>-migration-<secs>`. Folded to a DNS-1123 name via [`claim_name`]'s
/// per-char fold so any app/env string yields a valid `metadata.name`.
fn plan_name(app: &str, env: &str, now: DateTime<Utc>) -> String {
    let secs = now.timestamp();
    let raw = if env.is_empty() {
        format!("{app}-migration-{secs}")
    } else {
        format!("{app}-{env}-migration-{secs}")
    };
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

/// 2.16b Task 11: the effective baseline spec — reconstruct an
/// `Application` from the stamped `status.lastAppliedSpec` snapshot and run
/// it through the SAME `effective_spec` unifier the render path uses, under
/// the baseline's OWN `spec.environment` (H4/R2-M2: each side is diffed
/// under its own environment). `effective_spec` reads only `spec.base` +
/// `spec.environments` (never `metadata`), so a `default()` meta on the
/// reconstructed object is sufficient and keeps the diff a pure function of
/// the two specs. Pure — a seam so the reconstruction is unit-testable
/// without a client.
fn effective_baseline(baseline_spec: &ApplicationSpec) -> ApplicationBaseSpec {
    let baseline_app = Application {
        metadata: Default::default(),
        spec: baseline_spec.clone(),
        status: None,
    };
    effective_spec(&baseline_app, baseline_spec.environment.as_deref())
}

/// 2.16b Task 11: return the names of app-namespace MigrationPlans that
/// gate the given `(app_name, env)` key and are NOT `keep` — the set a
/// supersede / consume / cleanup delete must remove to enforce "≤1 live
/// plan per key" (R2-mn4 / R3-M1). Scope-matching mirrors
/// [`pick_blocking_plan`]: application-scope, same app name + the plan's
/// `environment` equals `env`. Pure so the filter is unit-testable without
/// a client; the async [`delete_all_key_plans_except`] wraps it.
fn plans_to_delete(
    plans: &[MigrationPlan],
    app_name: &str,
    env: &str,
    keep: Option<&str>,
) -> Vec<String> {
    plans
        .iter()
        .filter(|plan| {
            if plan.spec.scope.type_ != "application" {
                return false;
            }
            let Some(app_scope) = &plan.spec.scope.application else {
                return false;
            };
            app_scope.ref_.name == app_name && app_scope.environment == env
        })
        .filter_map(|plan| plan.metadata.name.clone())
        .filter(|name| Some(name.as_str()) != keep)
        .collect()
}

/// 2.16b Task 11: the `status` payload with `lastAppliedSpec` stamped to
/// `spec` (the RAW current spec snapshot the classifier diffs against),
/// preserving every other field of `base`. Pure seam over the SSA stamp
/// path so the field-set behaviour is unit-testable; the async caller
/// applies the result via [`apply_status`]. `base` carries the status the
/// happy-path reconcile just built (phase / conditions / endpoint / image
/// / environment) — this only fills the one new field.
fn with_stamped_baseline(base: ApplicationStatus, spec: &ApplicationSpec) -> ApplicationStatus {
    ApplicationStatus {
        last_applied_spec: Some(spec.clone()),
        ..base
    }
}

/// 2.16b Task 11: the Application's `metadata.uid` for the MigrationPlan
/// controller ownerRef, or a clear [`ReconcileError::MissingUid`] when it
/// is absent (the apiserver always assigns one, so this only fires on a
/// hand-built object — defensively surfaced rather than emitting an
/// owner-less plan).
fn app_uid_or(app: &Application) -> Result<&str, ReconcileError> {
    app.metadata
        .uid
        .as_deref()
        .ok_or_else(|| ReconcileError::MissingUid(app.name_any()))
}

/// True once the Application is marked for deletion. The reconcile loop
/// skips deletion-marked objects so it never re-applies children that a
/// cascade delete (Argo CD finalizer) is trying to remove.
fn is_deletion_marked(app: &Application) -> bool {
    app.metadata.deletion_timestamp.is_some()
}

/// 2.16b (walk-found): carry the stamped `lastAppliedSpec` migration baseline
/// forward on every pause/awaiting status write. [`apply_status`] is a
/// server-side apply under a SINGLE field manager ([`FIELD_MANAGER`],
/// `.force()`), so a status payload that OMITS `lastAppliedSpec` makes the
/// apiserver PRUNE it — the manager relinquishes the field, and no one else
/// owns it. This is NOT "leave it untouched" (the merge-patch mental model the
/// old `last_applied_spec: None` comments assumed). Emitting `None` on a paused
/// write therefore wiped the baseline, so the NEXT reconcile read no baseline,
/// skipped destructive detection, and the gate deleted its own MigrationPlan
/// (the pause self-cancelled in ~200ms). Re-sending the existing baseline keeps
/// the manager's ownership so it survives the pause. Only the render path
/// ([`with_stamped_baseline`]) deliberately overrides it with the new spec.
fn existing_baseline(app: &Application) -> Option<ApplicationSpec> {
    app.status
        .as_ref()
        .and_then(|s| s.last_applied_spec.clone())
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
fn build_paused_status(app: &Application, plan_ns: &str, plan_name: &str) -> ApplicationStatus {
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
             {plan_ns}/{plan_name}"
        ),
        previous_conditions,
    );
    let pending = migration_pending_condition(plan_ns, plan_name, previous_conditions);

    ApplicationStatus {
        phase: Some(PHASE_AWAITING_MIGRATION_APPROVAL.to_string()),
        observed_generation: app.metadata.generation,
        conditions: Some(vec![ready, pending]),
        endpoint_url: previous_endpoint,
        image: None,
        environment: app.spec.environment.clone(),
        // 2.16b (walk-found): carry the baseline forward — omitting it under
        // SSA prunes it, self-cancelling the gate. See `existing_baseline`.
        last_applied_spec: existing_baseline(app),
    }
}

/// 2.16b Task 11: condition type for a gating MigrationPlan stuck in
/// phase `failed`. Kept local to the controller (not a schema constant)
/// — it is an operator-emitted status condition, not part of the CRD's
/// declared vocabulary.
const COND_MIGRATION_FAILED: &str = "MigrationFailed";

/// 2.16b Task 11: build the paused status for the `BlockFailed` decision
/// arm — the change's gating MigrationPlan is in phase `failed` and needs
/// manual resolution. Mirrors [`build_paused_status`] (preserves
/// `observedGeneration` + `endpointURL`, `phase =
/// AwaitingMigrationApproval`) but emits a `MigrationFailed=True`
/// condition instead of `MigrationPending`, so consumers can distinguish
/// "awaiting approval" from "failed, manual delete required". `Ready`
/// stays `False`.
fn build_migration_failed_status(app: &Application, plan_name: &str) -> ApplicationStatus {
    let previous_conditions = app
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[]);
    let previous_endpoint = app.status.as_ref().and_then(|s| s.endpoint_url.clone());

    let ready = ready_condition(
        "False",
        "MigrationFailed",
        &format!("paused: MigrationPlan {plan_name} failed — manual delete required"),
        previous_conditions,
    );
    let failed = migration_failed_condition(plan_name, previous_conditions);

    ApplicationStatus {
        phase: Some(PHASE_AWAITING_MIGRATION_APPROVAL.to_string()),
        observed_generation: app.metadata.generation,
        conditions: Some(vec![ready, failed]),
        endpoint_url: previous_endpoint,
        image: None,
        environment: app.spec.environment.clone(),
        // 2.16b: a failed gate does NOT re-stamp the baseline — the app is
        // still on the prior applied spec. CARRY it forward: apply_status is
        // SSA, so omitting the field prunes it (walk-found) rather than leaving
        // it untouched. See `existing_baseline`.
        last_applied_spec: existing_baseline(app),
    }
}

/// 2.16b Task 11: the `MigrationFailed=True` condition. `lastTransitionTime`
/// preserved when the prior `MigrationFailed` was already `True` (mirrors
/// [`migration_pending_condition`]).
fn migration_failed_condition(
    plan_name: &str,
    previous: &[ApplicationCondition],
) -> ApplicationCondition {
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == COND_MIGRATION_FAILED && c.status == "True")
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    ApplicationCondition {
        type_: COND_MIGRATION_FAILED.to_string(),
        status: "True".to_string(),
        last_transition_time,
        reason: "MigrationPlanFailed".to_string(),
        message: format!(
            "MigrationPlan {plan_name} failed — manual delete required to unblock the Application"
        ),
        observed_generation: None,
    }
}

fn migration_pending_condition(
    plan_ns: &str,
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
        // 2.16b (Task 11 review Risk 2): app-scope plans live in the APP
        // namespace (Task 8), not `apprafter-system` — surface the plan's
        // real namespace so `kubectl describe migrationplan -n <ns>` lands.
        message: format!("MigrationPlan {plan_ns}/{plan_name} is awaiting approval"),
        observed_generation: None,
    }
}

// ---- 2.4d: pure ResourceClaim generation + pause helpers ----

/// Derive a DNS-1123-safe `metadata.name` for a child ResourceClaim
/// from the owning Application name, the need's service type, and the
/// optional `(type, name)` entry name (2.6b / ADR 0043):
///
///   * `name == None` (the unnamed default claim) → `{app}-{type}`
///     (unchanged — zero migration for pre-2.6b single claims);
///   * `name == Some(n)` (a named array entry) → `{app}-{type}-{n}`.
///
/// The whole join is folded: non-alphanumerics → `-`, lowercased,
/// truncated to 63 bytes, trailing `-` trimmed. An empty / all-`-`
/// `name` collapses to the unnamed form (the trailing-`-` trim drops
/// the dangling separator). Mirrors
/// `resourceclaim-provisioner::cnpg::k8s_name`'s fold (without the
/// `claim-` prefix — the `{app}-` prefix already guarantees a leading
/// alphanumeric for any valid Application name).
fn claim_name(app: &str, service_type: &str, name: Option<&str>) -> String {
    let raw = match name {
        Some(n) if !n.is_empty() => format!("{app}-{service_type}-{n}"),
        _ => format!("{app}-{service_type}"),
    };
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
    // 2.6b (ADR 0043): walk the flattened `Needs::entries()` in its
    // deterministic order and emit ONE claim per `(type, name)` entry —
    // a scalar yields one, an array yields N. The unnamed default
    // (`name: None`) keeps `<app>-<type>` (zero migration); a named
    // entry yields `<app>-<type>-<name>` and carries `spec.name` so the
    // provisioner can disambiguate sibling claims.
    //
    // 2.6b-4: `disk` entries now generate a `type: disk` claim too (the
    // `Backend::Disk` provisioner landed in 2.6b-3). A disk entry carries
    // `disk = Some(DiskClaim)` / `service = None`; only its `size` (and
    // the integrated-tier selector matching the seeded `disk-local`
    // provider) reach the claim — `mountPath`/`readOnly`/`class` stay
    // render-side. The claim name `<app>-disk[-<name>]` matches what
    // `resolve_disk_mounts` looks up.
    let mut out: Vec<(String, Value)> = Vec::new();
    for (service_type, entry) in needs.entries() {
        let name = claim_name(app_name, &service_type, entry.name.as_deref());

        // A disk entry has `service = None` + `disk = Some(..)`; emit a
        // minimal `type: disk` claim and skip the service-only fields.
        if let Some(disk) = entry.disk {
            // 2.6c (T9): a referenced disk (`needs.disk.ref`) binds an
            // existing SharedVolume. Emit a `shared-disk` ResourceClaim
            // carrying ONLY the binding label `apprafter.io/shared-volume`
            // (the key the T6/T7 provisioner reads to find the SharedVolume
            // and write back `status.volumeClaimRef`) + the integrated-tier
            // selector + the Application ownerRef. NO `size`, NO `spec.name`
            // — the reference shape is discriminated purely by `ref`, and
            // `resolve_disk_mounts` pairs the mount back by that same label.
            if let Some(reference) = disk.reference.as_deref() {
                let ref_name = claim_name(app_name, "shared-disk", Some(reference));
                let payload = json!({
                    "apiVersion": "apprafter.io/v1alpha1",
                    "kind": "ResourceClaim",
                    "metadata": {
                        "name": ref_name,
                        "namespace": namespace,
                        "labels": { "apprafter.io/shared-volume": reference },
                        "ownerReferences": [{
                            "apiVersion": "apprafter.io/v1alpha1",
                            "kind": "Application",
                            "name": app_name,
                            "uid": app_uid,
                            "controller": true,
                            "blockOwnerDeletion": true,
                        }],
                    },
                    "spec": {
                        "type": "shared-disk",
                        "selector": default_integrated_selector(),
                    },
                });
                out.push((ref_name, payload));
                continue;
            }
            let mut claim_spec = json!({
                "type": service_type,
                "selector": default_integrated_selector(),
                "size": disk.size,
            });
            if let Some(entry_name) = entry.name.as_deref() {
                if !entry_name.is_empty() {
                    claim_spec["name"] = json!(entry_name);
                }
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
            out.push((name, payload));
            continue;
        }

        let Some(need) = entry.service else {
            continue;
        };

        let selector = need
            .selector
            .clone()
            .unwrap_or_else(default_integrated_selector);
        let mut claim_spec = json!({
            "type": service_type,
            "selector": selector,
        });
        // `(type, name)` identity (2.6b): a named array entry carries
        // `spec.name` so the provisioner derives distinct DBs/users/secrets
        // for sibling claims of one app. The unnamed default omits it.
        if let Some(entry_name) = entry.name.as_deref() {
            if !entry_name.is_empty() {
                claim_spec["name"] = json!(entry_name);
            }
        }
        if let Some(size) = &need.size {
            claim_spec["size"] = json!(size);
        }
        // Persistence passthrough (ADR 0042): the dragonfly provisioner
        // reads `spec.persistent` to route the claim to a persistent vs
        // ephemeral pool instance. Mirrors the `size` passthrough.
        if let Some(persistent) = need.persistent {
            claim_spec["persistent"] = json!(persistent);
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
        out.push((name, payload));
    }
    out
}

/// Names of claims that are NOT yet ready. A claim is ready only when
/// `status.ready == Some(true)` AND it carries its backend OUTPUT ref —
/// `connectionSecretRef` for a service claim (pg/redis/…; the renderer
/// injects its env) OR `volumeClaimRef` for a `disk` claim (2.6b /
/// ADR 0043; the renderer mounts that PVC, there is no connection
/// Secret). The provisioner writes `ready` + the output ref together,
/// so the AND closes the half-ready resume race at zero cost. Returns
/// the unready names in the claims' iteration order.
fn unready_claim_names(claims: &[ResourceClaim]) -> Vec<String> {
    claims
        .iter()
        .filter(|c| {
            let ready = c
                .status
                .as_ref()
                .map(|s| {
                    s.ready == Some(true)
                        && (s.connection_secret_ref.is_some() || s.volume_claim_ref.is_some())
                })
                .unwrap_or(false);
            !ready
        })
        .map(|c| c.name_any())
        .collect()
}

/// Resolve ready claims into a `(type, name) → connectionSecretRef`
/// map for the renderer's DSN injection. Keyed on the `(spec.type_,
/// spec.name)` claim identity (2.6b / ADR 0043) — `name == None` is the
/// unnamed/default claim of a type (renders the base env NAME, e.g.
/// `DATABASE_URL`), `Some(name)` is a named array entry (renders
/// `<VAR>_<fold(name)>`). Valued by `status.connectionSecretRef`; claims
/// without a resolved connection Secret are skipped (defensive — post-gate
/// every claim has one). Pure: the operator only READS provisioner-owned
/// claim status here, never writes it (the SSA split is preserved). The
/// caller threads the result into `render_application_for_env` AFTER the
/// 2.4d readiness gate passes, building it from the SAME `current` claims
/// the gate validated.
fn resolve_needs_secrets(claims: &[ResourceClaim]) -> BTreeMap<(String, Option<String>), String> {
    let mut map = BTreeMap::new();
    for claim in claims {
        if let Some(secret) = claim
            .status
            .as_ref()
            .and_then(|s| s.connection_secret_ref.clone())
        {
            let name = claim.spec.name.clone().filter(|n| !n.is_empty());
            map.insert((claim.spec.type_.clone(), name), secret);
        }
    }
    map
}

/// Derive a `needs.disk` entry's `(disk, name)` identity (2.6b / ADR
/// 0043): the explicit `name` when set, else the last path segment of
/// `mountPath` folded to a DNS-1123 label (`/var/lib/uploads` →
/// `uploads`). It becomes both the `disk-<name>` volume name and the
/// `<app>-disk-<name>` claim suffix, so it MUST match the webhook's
/// derivation and `claim_name`'s fold. The webhook guarantees a
/// non-empty, DNS-1123-valid result; the fold here is defensive (a valid
/// input passes through unchanged).
fn disk_identity_name(disk: &DiskClaim) -> String {
    let raw = match disk.name.as_deref().filter(|n| !n.is_empty()) {
        Some(explicit) => explicit.to_string(),
        None => disk
            .mount_path
            .rsplit('/')
            .find(|seg| !seg.is_empty())
            .unwrap_or("disk")
            .to_string(),
    };
    // Fold to a DNS-1123 label (lowercase alphanumeric + `-`), trimming
    // leading/trailing `-`. Mirrors `claim_name`'s per-char fold.
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        out.push(if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        });
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "disk".to_string()
    } else {
        trimmed
    }
}

/// Resolve ready `needs.disk` claims into renderer [`DiskMount`] input
/// (2.6b / ADR 0043). For each `needs.disk` entry of the effective spec,
/// match the ready disk ResourceClaim by its k8s name
/// (`<app>-disk[-<name>]`, via [`claim_name`]) or `spec.name`, and pair
/// the entry's `mountPath`/`readOnly`/derived volume name with the
/// claim's `status.volumeClaimRef` (the provisioned PVC). A disk entry
/// whose claim is absent / not ready / lacks a `volumeClaimRef` is
/// skipped (defensive — post-gate every claim is ready). Pure: only READS
/// claim status, never writes. The caller threads the result into
/// `render_application_for_env` AFTER the 2.4d readiness gate, building it
/// from the SAME `current` ready claims the gate validated.
fn resolve_disk_mounts(
    spec: &ApplicationBaseSpec,
    app: &str,
    claims: &[ResourceClaim],
) -> Vec<DiskMount> {
    let Some(needs) = spec.needs.as_ref() else {
        return Vec::new();
    };
    let Some(disks) = needs.disk.as_ref() else {
        return Vec::new();
    };
    let mut mounts: Vec<DiskMount> = Vec::new();
    for disk in disks.as_slice_vec() {
        // The VOLUME name uses the derived identity (`disk-<name>`); the
        // CLAIM name uses the disk entry's RAW name exactly as claim-gen
        // does (`<app>-disk` for a derived-default entry, `<app>-disk-<n>`
        // for an explicit name) — so a derived-default entry matches its
        // `<app>-disk` claim (whose `spec.name` is None).
        let identity = disk_identity_name(&disk);
        // 2.6c (T9): a referenced disk pairs with its `shared-disk` claim
        // by the `apprafter.io/shared-volume` label (== `ref`) — the SAME
        // binding key the T6/T7 provisioner uses — NOT by k8s/spec name
        // (the reference claim carries neither a size nor a spec.name). The
        // owned path below keeps its name-based pairing.
        let claim = if let Some(reference) = disk.reference.as_deref() {
            claims.iter().find(|c| {
                c.spec.type_ == "shared-disk"
                    && c.metadata
                        .labels
                        .as_ref()
                        .and_then(|l| l.get("apprafter.io/shared-volume"))
                        .map(String::as_str)
                        == Some(reference)
            })
        } else {
            let want_spec_name = disk.name.as_deref().filter(|n| !n.is_empty());
            let want_claim_name = claim_name(app, "disk", want_spec_name);
            // Match by k8s claim name (robust for both named + derived-default
            // entries) OR by spec.name (the explicit (disk,name) identity).
            claims.iter().find(|c| {
                c.name_any() == want_claim_name
                    || (want_spec_name.is_some() && c.spec.name.as_deref() == want_spec_name)
            })
        };
        let Some(pvc_name) = claim
            .and_then(|c| c.status.as_ref())
            .and_then(|s| s.volume_claim_ref.clone())
        else {
            continue;
        };
        mounts.push(DiskMount {
            volume_name: format!("disk-{identity}"),
            mount_path: disk.mount_path.clone(),
            read_only: disk.read_only.unwrap_or(false),
            pvc_name,
            owned: !disk.is_reference(),
        });
    }
    mounts
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
        image: None,
        environment: app.spec.environment.clone(),
        // 2.16b (walk-found): carry the baseline forward — omitting it under
        // SSA prunes it. See `existing_baseline`.
        last_applied_spec: existing_baseline(app),
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

// ---- 2.12e: env secret-ref existence check (ADR 0046 Decision #4) ----

/// Scan `env` for every `EnvValue::Ref(EnvRef::Secret(…))` entry whose
/// backing Secret / key is absent (as reported by `secret_has_key`).
/// Returns one human-readable message per missing ref, in BTreeMap key
/// order (deterministic). `secret_has_key(name, key) → bool` abstracts
/// the cluster lookup so the function can be unit-tested without a client.
///
/// **Only `Secret` refs are checked here.** `Claim` refs cannot reach this
/// path — the `AwaitingResourceClaim` gate (2.4d) holds the Application
/// until all claim Secrets are ready before the render proceeds.
/// `Literal` values carry no Secret dependency and are skipped.
pub fn unresolved_env_secret_refs(
    env: &std::collections::BTreeMap<String, operator_core::EnvValue>,
    secret_has_key: &dyn Fn(&str, &str) -> bool,
) -> Vec<String> {
    use operator_core::{EnvRef, EnvValue};
    env.iter()
        .filter_map(|(var_name, value)| {
            let path = match value {
                EnvValue::Ref(EnvRef::Secret(p)) => p,
                _ => return None, // Literal + Claim: skip
            };
            // Parse `"<name>/<key>"` on the first `/`.
            let slash = path.find('/')?; // malformed (no `/`) → skip defensively
            let secret_name = &path[..slash];
            let key = &path[slash + 1..];
            if secret_has_key(secret_name, key) {
                None
            } else {
                Some(format!(
                    "env {} → secret \"{}\": Secret \"{}\" not found or missing key \"{}\"",
                    var_name, path, secret_name, key
                ))
            }
        })
        .collect()
}

/// Build the Application status payload for the env-secret-missing path
/// (2.12 / ADR 0046 Decision #4). Mirrors `build_resource_claim_paused_status`:
/// preserves `observedGeneration` + `endpointURL`, sets `phase` to
/// `EnvSecretMissing`, and emits a single `Ready=False/EnvSecretMissing`
/// condition whose `message` carries the joined per-ref diagnostic.
fn build_env_secret_missing_status(app: &Application, messages: &[String]) -> ApplicationStatus {
    let previous_conditions = app
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[]);
    let previous_endpoint = app.status.as_ref().and_then(|s| s.endpoint_url.clone());

    let message = messages.join("; ");
    let ready = ready_condition("False", "EnvSecretMissing", &message, previous_conditions);

    ApplicationStatus {
        phase: Some(PHASE_ENV_SECRET_MISSING.to_string()),
        observed_generation: app.metadata.generation,
        conditions: Some(vec![ready]),
        endpoint_url: previous_endpoint,
        image: None,
        environment: app.spec.environment.clone(),
        // 2.16b (walk-found): carry the baseline forward — omitting it under
        // SSA prunes it. See `existing_baseline`.
        last_applied_spec: existing_baseline(app),
    }
}

/// Whether this reconcile should issue a registry HEAD to resolve the
/// current `tag` → digest, or reuse the prior `status.image.resolved`
/// (2.4h Fix 1 (A); mirrors the PlatformController OCI-poll throttle).
///
/// Returns `false` (SKIP the HEAD, reuse cache) **only** when ALL hold:
///
///   * a prior `status.image` exists, AND
///   * its `tag` equals the current spec tag (a moved tag re-arms), AND
///   * it carries a `resolved` digest (a recorded-but-failed attempt —
///     `resolved: None`, Fix 1 (B) — re-arms), AND
///   * `resolvedAt` parses as RFC3339 AND is within the throttle window
///     (`now - resolvedAt < min_interval`; an unparseable or stale
///     timestamp re-arms).
///
/// Otherwise returns `true` (resolve this cycle). Throttle-only: it never
/// blocks the rollout — the caller still renders the verbatim tag on any
/// resolution failure.
fn should_resolve_image(
    prior: Option<&StatusImage>,
    current_tag: &str,
    now: DateTime<Utc>,
    min_interval_secs: i64,
) -> bool {
    let Some(prior) = prior else {
        return true;
    };
    if prior.tag.as_deref() != Some(current_tag) {
        return true;
    }
    if prior.resolved.is_none() {
        return true;
    }
    let Some(resolved_at) = prior
        .resolved_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
    else {
        return true;
    };
    (now - resolved_at).num_seconds() >= min_interval_secs
}

/// Whether the controller should resolve `base.image` to a registry
/// digest this reconcile (ADR 0040). Default — absent `imagePolicy` or
/// `resolve: "digest"` — is yes; only `resolve: "off"` disables it
/// (verbatim tag, no registry poll, no ImageResolved condition).
fn image_resolution_enabled(spec: &ApplicationBaseSpec) -> bool {
    let resolve = spec
        .image_policy
        .as_ref()
        .and_then(|p| p.resolve.as_deref());
    !matches!(resolve, Some("off"))
}

/// Build the `ImageResolved` condition (2.4h-d). `ok` maps to
/// `status=True/reason=Resolved` after a successful tag→digest lookup,
/// or `status=False/reason=ResolveFailed` on a failure that fell back to
/// the verbatim tag. `lastTransitionTime` follows the same k8s
/// convention as `ready_condition` — preserved when the prior
/// `ImageResolved` already carried the same `status`, bumped on a flip.
fn image_resolved_condition(
    ok: bool,
    reason: &str,
    previous: &[ApplicationCondition],
) -> ApplicationCondition {
    let status = if ok { "True" } else { "False" };
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == COND_IMAGE_RESOLVED && c.status == status)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let (reason_field, message) = if ok {
        (
            "Resolved".to_string(),
            "image tag resolved to a registry digest".to_string(),
        )
    } else {
        ("ResolveFailed".to_string(), reason.to_string())
    };
    ApplicationCondition {
        type_: COND_IMAGE_RESOLVED.to_string(),
        status: status.to_string(),
        last_transition_time,
        reason: reason_field,
        message,
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

// ---- 1.83b: PublicRouteReady soft condition (pure helpers) ----

/// A hostname is covered by a zone when it is the zone apex (exact) or a
/// single-label subdomain (`<label>.zone`, no further dots — the Gateway
/// wildcard listener matches one level only). 1.83b.
fn hostname_covered_by_zone(host: &str, zone: &str) -> bool {
    if host == zone {
        return true;
    }
    match host.strip_suffix(&format!(".{zone}")) {
        Some(prefix) => !prefix.is_empty() && !prefix.contains('.'),
        None => false,
    }
}

/// Read `Accepted` / `ResolvedRefs` from an HTTPRoute `status` value
/// (`status.parents[].conditions[]`). Across all parents (any parent that
/// reports the condition `True`).
fn route_accepted_resolved(status: &Value) -> (bool, bool) {
    let mut accepted = false;
    let mut resolved = false;
    if let Some(parents) = status.get("parents").and_then(Value::as_array) {
        for p in parents {
            if let Some(conds) = p.get("conditions").and_then(Value::as_array) {
                for c in conds {
                    let t = c.get("type").and_then(Value::as_str);
                    let s = c.get("status").and_then(Value::as_str);
                    if t == Some("Accepted") && s == Some("True") {
                        accepted = true;
                    }
                    if t == Some("ResolvedRefs") && s == Some("True") {
                        resolved = true;
                    }
                }
            }
        }
    }
    (accepted, resolved)
}

/// Compute the soft `PublicRouteReady` verdict (status, reason, message) for a
/// public app's hostnames against the registered zones and the route's own
/// status. The route is ALWAYS emitted — this is informational only. 1.83b.
fn evaluate_public_route(
    hostnames: &[String],
    allowed_domains: &[String],
    route_status: Option<&Value>,
) -> (&'static str, String, String) {
    if let Some(uncovered) = hostnames.iter().find(|h| {
        !allowed_domains
            .iter()
            .any(|z| hostname_covered_by_zone(h, z))
    }) {
        return (
            "False",
            "NoMatchingZone".to_string(),
            format!(
                "hostname {uncovered:?} is not under any registered allowedDomains zone; the HTTPRoute is emitted and will attach once the zone is added (apprafter target domain add)"
            ),
        );
    }
    match route_status.map(route_accepted_resolved) {
        Some((true, true)) => (
            "True",
            "Accepted".to_string(),
            "HTTPRoute accepted by the platform Gateway (Accepted, ResolvedRefs).".to_string(),
        ),
        _ => (
            "False",
            "Pending".to_string(),
            "HTTPRoute applied; awaiting Gateway acceptance.".to_string(),
        ),
    }
}

/// Extract `spec.values.gateway.allowedDomains[].domain` from a PlatformStack
/// values block (the 1.83a gateway schema). Empty when the gateway key /
/// allowedDomains is absent. 1.83b.
fn allowed_domains_from_values(values: &PlatformStackValues) -> Vec<String> {
    values
        .extras
        .get("gateway")
        .and_then(|g| g.get("allowedDomains"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.get("domain").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Build the soft `PublicRouteReady` condition, preserving `lastTransitionTime`
/// across status-unchanged reconciles (the k8s convention used by
/// `ready_condition`). 1.83b.
fn public_route_ready_condition(
    hostnames: &[String],
    allowed_domains: &[String],
    route_status: Option<&Value>,
    previous: &[ApplicationCondition],
) -> ApplicationCondition {
    let (status, reason, message) = evaluate_public_route(hostnames, allowed_domains, route_status);
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == COND_PUBLIC_ROUTE_READY && c.status == status)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    ApplicationCondition {
        type_: COND_PUBLIC_ROUTE_READY.to_string(),
        status: status.to_string(),
        last_transition_time,
        reason,
        message,
        observed_generation: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Walk-found: 2.9 widened the Deployment selector label-set, and
    // because spec.selector is immutable every Deployment created under the
    // prior set 422'd forever. The fix delete+recreates only when the live
    // selector differs from the rendered (stable minimal) one.
    #[test]
    fn selector_needs_migration_detects_widened_selector() {
        let mk = |labels: serde_json::Value| -> Deployment {
            serde_json::from_value(json!({
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "metadata": {"name": "web"},
                "spec": {
                    "selector": {"matchLabels": labels},
                    "template": {"spec": {"containers": []}}
                }
            }))
            .unwrap()
        };
        // Pre-2.9 (no apprafter.io/application) vs rendered minimal → migrate.
        let existing = mk(json!({
            "app.kubernetes.io/name": "web",
            "app.kubernetes.io/managed-by": "apprafter-operator",
            "apprafter": "true"
        }));
        let desired = mk(json!({"apprafter.io/application": "web"}));
        assert!(selector_needs_migration(&existing, &desired));
        // Post-migration (already minimal) == rendered → steady, no delete.
        assert!(!selector_needs_migration(&desired, &desired));
    }
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
        // 2.16b: app-scope plans are co-located in the APP namespace (Task 8),
        // so the fixture's metadata.namespace mirrors the app's own ns.
        plan.metadata.namespace = Some(target_ns.into());
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
        // Platform-scope plans live in the platform `apprafter-system` ns.
        plan.metadata.namespace = Some("apprafter-system".into());
        plan
    }

    // 2.16b Task 11: `pick_any_key_plan` returns the (at most one)
    // key-matching plan of ANY phase. The blocking-vs-not distinction
    // that the old `pick_blocking_plan` applied here now lives in
    // `plan_state` (covered by `plan_state_buckets_by_phase_and_trigger_match`),
    // because the state machine must SEE completed / rejected / failed
    // plans (ConsumeApply / cleanup arms). These tests therefore assert
    // the SCOPE match only — and that phase does NOT filter.
    #[test]
    fn pick_any_key_plan_finds_matching_pending_plan() {
        // Baseline: a plan whose scope matches the Application's name +
        // namespace + environment is picked up.
        let plans = vec![app_plan(
            "parser-pg",
            "parser",
            "demo",
            "prod",
            Some("pending-approval"),
        )];
        let plan = pick_any_key_plan(plans, "parser", "demo", Some("prod"));
        assert!(plan.is_some());
        assert_eq!(plan.unwrap().metadata.name.as_deref(), Some("parser-pg"));
    }

    #[test]
    fn pick_any_key_plan_returns_completed_and_rejected_plans() {
        // Phase does NOT filter here (unlike the old blocking finder) —
        // the state machine needs to consume a `completed` plan and clean
        // up a `rejected` relic, so both must be returned.
        for phase in ["completed", "rejected", "failed", "executing"] {
            let plans = vec![app_plan("parser-pg", "parser", "demo", "prod", Some(phase))];
            assert!(
                pick_any_key_plan(plans, "parser", "demo", Some("prod")).is_some(),
                "phase {phase} must still be returned by the any-phase finder"
            );
        }
        // A just-created plan (no status.phase yet) is returned too.
        let plans = vec![app_plan("parser-pg", "parser", "demo", "prod", None)];
        assert!(pick_any_key_plan(plans, "parser", "demo", Some("prod")).is_some());
    }

    #[test]
    fn pick_any_key_plan_ignores_platform_scope_plans() {
        // Platform-scope plans are observed by PlatformController,
        // not the Application reconciler. Scope filter must exclude them.
        let plans = vec![platform_plan("p-1")];
        let plan = pick_any_key_plan(plans, "parser", "demo", None);
        assert!(plan.is_none());
    }

    #[test]
    fn pick_any_key_plan_filters_by_application_namespace() {
        // Same name in a different namespace must NOT match.
        let plans = vec![app_plan(
            "parser-pg",
            "parser",
            "other-ns",
            "prod",
            Some("pending-approval"),
        )];
        let plan = pick_any_key_plan(plans, "parser", "demo", Some("prod"));
        assert!(plan.is_none());
    }

    #[test]
    fn pick_any_key_plan_filters_by_environment_when_set() {
        // Environments are scoped — a `dev` plan must not match the
        // `prod` reconcile.
        let plans = vec![app_plan(
            "parser-pg",
            "parser",
            "demo",
            "dev",
            Some("pending-approval"),
        )];
        let plan = pick_any_key_plan(plans, "parser", "demo", Some("prod"));
        assert!(plan.is_none());
    }

    #[test]
    fn pick_any_key_plan_ignores_environment_when_caller_passes_none() {
        // The Application's `spec.environment` may be unset (the
        // base-only / single-env case). Environment becomes a wildcard
        // matcher then — any matching app + namespace matches.
        let plans = vec![app_plan(
            "parser-pg",
            "parser",
            "demo",
            "prod",
            Some("pending-approval"),
        )];
        let plan = pick_any_key_plan(plans, "parser", "demo", None);
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
            image: None,
            environment: None,
            last_applied_spec: None,
        });
        let status = build_paused_status(&app, "team-a", "web-prod-migration-1");

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
        // 2.16b (Task 11 review Risk 2): the message references the plan's
        // REAL namespace (the app namespace), not the platform
        // `apprafter-system` — an app-scope plan lives in the app's own ns.
        assert!(pending.message.contains("team-a/web-prod-migration-1"));
        assert!(!pending.message.contains("apprafter-system"));
        // The Ready condition message carries the same real namespace.
        assert!(ready.message.contains("team-a/web-prod-migration-1"));
        assert!(!ready.message.contains("apprafter-system"));
    }

    #[test]
    fn build_paused_status_preserves_endpoint_when_status_absent() {
        // First reconcile that pauses before ever succeeding:
        // app.status is None. Endpoint stays None too.
        let app = Application::new("web", ApplicationSpec::default());
        let status = build_paused_status(&app, "team-a", "plan-1");
        assert!(status.endpoint_url.is_none());
        assert_eq!(
            status.phase.as_deref(),
            Some(PHASE_AWAITING_MIGRATION_APPROVAL)
        );
    }

    #[test]
    fn pause_status_builders_carry_the_baseline_forward() {
        // 2.16b WALK-FOUND regression: apply_status is SSA under a single
        // field manager, so a paused status payload that OMITS
        // `lastAppliedSpec` makes the apiserver PRUNE the stamped baseline
        // (the manager relinquishes it). That wiped the baseline on the very
        // next reconcile → detection was skipped → the gate deleted its own
        // MigrationPlan (the pause self-cancelled in ~200ms). Every
        // pause/awaiting builder MUST re-send the existing baseline so the
        // SSA manager keeps ownership and it survives the pause.
        let baseline = ApplicationSpec {
            environment: Some("dev".into()),
            ..Default::default()
        };
        let mut app = Application::new("web", ApplicationSpec::default());
        app.status = Some(ApplicationStatus {
            phase: Some("Ready".into()),
            observed_generation: Some(3),
            conditions: None,
            endpoint_url: None,
            image: None,
            environment: Some("dev".into()),
            last_applied_spec: Some(baseline.clone()),
        });

        // All four production pause/awaiting builders must preserve it.
        for (label, got) in [
            (
                "paused",
                build_paused_status(&app, "team-a", "web-dev-migration-1"),
            ),
            (
                "migration-failed",
                build_migration_failed_status(&app, "web-dev-migration-1"),
            ),
            (
                "resource-claim",
                build_resource_claim_paused_status(&app, &["pg".to_string()]),
            ),
            (
                "env-secret-missing",
                build_env_secret_missing_status(&app, &["DB missing".to_string()]),
            ),
        ] {
            assert_eq!(
                got.last_applied_spec.as_ref().and_then(|s| s.environment.as_deref()),
                Some("dev"),
                "{label} status dropped the migration baseline — SSA will prune it and the gate self-cancels"
            );
        }

        // And `existing_baseline` returns None when there is no prior stamp
        // (first reconcile) — the render path stamps it, the pause path must
        // not fabricate one.
        let fresh = Application::new("web", ApplicationSpec::default());
        assert!(existing_baseline(&fresh).is_none());
    }

    // ---- 2.16b (R3-mn-d): soft-destructive notes (SoftDestructiveChange) ----

    #[test]
    fn soft_destructive_notes_lists_soft_ops() {
        use operator_core::OneOrMany;

        // old: env X=literal + needs.pg selector {tier: a}
        let mut env = std::collections::BTreeMap::new();
        env.insert("X".to_string(), EnvValue::Literal("hi".into()));
        let old = ApplicationBaseSpec {
            env: Some(env),
            needs: Some(Needs {
                pg: Some(OneOrMany::One(pg_selector_need("a"))),
                ..Default::default()
            }),
            ..Default::default()
        };

        // new: env X removed (soft literal removal) + needs.pg selector → {tier: b}
        let new = ApplicationBaseSpec {
            needs: Some(Needs {
                pg: Some(OneOrMany::One(pg_selector_need("b"))),
                ..Default::default()
            }),
            ..Default::default()
        };

        let notes = soft_destructive_notes(&old, &new);
        // literal-env removal note names the KEY.
        assert!(
            notes.iter().any(|n| n.contains("env") && n.contains("X")),
            "expected an env-X removal note, got {notes:?}"
        );
        // selector-change note mentions "selector".
        assert!(
            notes.iter().any(|n| n.to_lowercase().contains("selector")),
            "expected a selector-change note, got {notes:?}"
        );
    }

    /// A `needs.pg` `ServiceNeed` carrying only `selector: {tier: <tier>}`.
    fn pg_selector_need(tier: &str) -> operator_core::ServiceNeed {
        operator_core::ServiceNeed {
            selector: Some(
                [("tier".to_string(), tier.to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        }
    }

    #[test]
    fn soft_destructive_notes_empty_when_no_soft_change() {
        use operator_core::{OneOrMany, ServiceNeed};
        let s = ApplicationBaseSpec {
            needs: Some(Needs {
                pg: Some(OneOrMany::One(ServiceNeed::default())),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(soft_destructive_notes(&s, &s).is_empty());
    }

    #[test]
    fn soft_destructive_notes_covers_image_tag_and_scale_down_and_size() {
        use operator_core::{OneOrMany, ServiceNeed};
        let pg_sized = |size: &str| ServiceNeed {
            size: Some(size.into()),
            ..Default::default()
        };
        // image tag change (repo unchanged) → soft; scale down 3→2; pg size change.
        let old = ApplicationBaseSpec {
            image: Some("ghcr.io/acme/api:v1".into()),
            replicas: Some(3),
            needs: Some(Needs {
                pg: Some(OneOrMany::One(pg_sized("10Gi"))),
                ..Default::default()
            }),
            ..Default::default()
        };
        let new = ApplicationBaseSpec {
            image: Some("ghcr.io/acme/api:v2".into()),
            replicas: Some(2), // scale down N->M, M>0 → soft
            needs: Some(Needs {
                pg: Some(OneOrMany::One(pg_sized("20Gi"))),
                ..Default::default()
            }),
            ..Default::default()
        };

        let notes = soft_destructive_notes(&old, &new);
        assert!(
            notes.iter().any(|n| n.contains("v1") && n.contains("v2")),
            "expected an image-tag note, got {notes:?}"
        );
        assert!(
            notes.iter().any(|n| n.contains("3") && n.contains("2")),
            "expected a scale-down note, got {notes:?}"
        );
        assert!(
            notes.iter().any(|n| n.to_lowercase().contains("size")),
            "expected a size note, got {notes:?}"
        );
    }

    #[test]
    fn soft_destructive_notes_ignores_hard_and_neutral_changes() {
        use operator_core::{EnvRef, OneOrMany, ServiceNeed};
        // env REF removal is hard-gated, not soft → not listed.
        let mut env = std::collections::BTreeMap::new();
        env.insert(
            "DB".to_string(),
            EnvValue::Ref(EnvRef::Claim("pg.url".into())),
        );
        let old = ApplicationBaseSpec {
            env: Some(env),
            ..Default::default()
        };
        let new = ApplicationBaseSpec::default();
        assert!(soft_destructive_notes(&old, &new).is_empty());

        // scale UP (2->3) and scale-to-ZERO are NOT soft-destructive here.
        let up = soft_destructive_notes(
            &ApplicationBaseSpec {
                replicas: Some(2),
                ..Default::default()
            },
            &ApplicationBaseSpec {
                replicas: Some(3),
                ..Default::default()
            },
        );
        assert!(
            up.is_empty(),
            "scale-up must not be a soft note, got {up:?}"
        );
        let to_zero = soft_destructive_notes(
            &ApplicationBaseSpec {
                replicas: Some(2),
                ..Default::default()
            },
            &ApplicationBaseSpec {
                replicas: Some(0),
                ..Default::default()
            },
        );
        assert!(
            to_zero.is_empty(),
            "scale-to-zero is HARD-gated, not a soft note, got {to_zero:?}"
        );

        // image REPO change is hard-gated, not soft.
        let repo = soft_destructive_notes(
            &ApplicationBaseSpec {
                image: Some("ghcr.io/acme/api:v1".into()),
                ..Default::default()
            },
            &ApplicationBaseSpec {
                image: Some("ghcr.io/acme/other:v1".into()),
                ..Default::default()
            },
        );
        assert!(repo.is_empty(), "image-repo change is hard, got {repo:?}");

        // adding a need is neutral (needs-removal is hard, needs-add is neutral).
        let add = soft_destructive_notes(
            &ApplicationBaseSpec::default(),
            &ApplicationBaseSpec {
                needs: Some(Needs {
                    pg: Some(OneOrMany::One(ServiceNeed::default())),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        assert!(add.is_empty(), "needs-add is neutral, got {add:?}");
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
        let next = migration_pending_condition("team-a", "plan-1", &prior);
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
    fn build_status_surfaces_spec_environment() {
        // 2.9 (ADR 0044): the per-CR `spec.environment` is mirrored into
        // `status.environment` so consumers see which env this
        // Application resolves against.
        let mut app = Application::new("web", ApplicationSpec::default());
        app.spec.environment = Some("prod".into());
        let st = build_status(&app, "Ready", vec![], None);
        assert_eq!(st.environment.as_deref(), Some("prod"));
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

    /// Build an `ApplicationBaseSpec` from a `type → ServiceNeed` map.
    /// 2.6b: `needs` is a closed struct, so the test-input map is folded
    /// into a `Needs` (each type → a scalar `OneOrMany::One`). Unknown
    /// keys panic — tests only use the six service types.
    fn base_with_needs(needs: BTreeMap<String, ServiceNeed>) -> ApplicationBaseSpec {
        use operator_core::{Needs, OneOrMany};
        let mut n = Needs::default();
        for (ty, need) in needs {
            let one = Some(OneOrMany::One(need));
            match ty.as_str() {
                "pg" => n.pg = one,
                "jetstream" => n.jetstream = one,
                "clickhouse" => n.clickhouse = one,
                "redis" => n.redis = one,
                "s3" => n.s3 = one,
                "notifications" => n.notifications = one,
                other => panic!("base_with_needs: unknown service type {other}"),
            }
        }
        ApplicationBaseSpec {
            image: Some("ghcr.io/acme/web:1.0".into()),
            needs: Some(n),
            ..Default::default()
        }
    }

    #[test]
    fn claim_name_joins_app_and_type_and_is_dns1123_safe() {
        // Unnamed default (name=None): unchanged `<app>-<type>`.
        assert_eq!(claim_name("parser", "pg", None), "parser-pg");
        // Non-alphanumerics fold to `-`, lowercase, trailing `-` trimmed.
        let n = claim_name("My_App.", "Redis_Cache_", None);
        assert_eq!(n, "my-app--redis-cache");
        // DNS-1123 validity: lowercased, no `_`, start/end alphanumeric.
        assert!(!n.contains('_'));
        assert_eq!(n, n.to_lowercase());
        assert!(n.chars().next().unwrap().is_ascii_alphanumeric());
        assert!(n.chars().last().unwrap().is_ascii_alphanumeric());
        // Truncates to 63 bytes.
        let long = claim_name(&"a".repeat(80), &"b".repeat(80), None);
        assert!(long.len() <= 63, "len was {}", long.len());
        assert!(long.chars().last().unwrap().is_ascii_alphanumeric());
    }

    #[test]
    fn claim_name_appends_folded_name_when_present() {
        // 2.6b: a named entry yields `<app>-<type>-<fold(name)>`.
        assert_eq!(
            claim_name("parser", "pg", Some("analytics")),
            "parser-pg-analytics"
        );
        // The name is DNS-1123-folded too (uppercase + `_`/`.` → `-`).
        assert_eq!(
            claim_name("parser", "pg", Some("Analytics_DB")),
            "parser-pg-analytics-db"
        );
        // An empty name behaves like None (no trailing `-`).
        assert_eq!(claim_name("parser", "pg", Some("")), "parser-pg");
        // Still DNS-1123-safe + truncated to 63 bytes even with a long name.
        let long = claim_name(&"a".repeat(40), "pg", Some(&"b".repeat(40)));
        assert!(long.len() <= 63, "len was {}", long.len());
        assert!(!long.contains('_'));
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
                name: None,
                selector: Some(BTreeMap::from([(
                    "tier".to_string(),
                    "managed".to_string(),
                )])),
                size: Some("small".into()),
                persistent: None,
            },
        );
        let spec = base_with_needs(needs);
        let payloads = generate_resource_claims(&spec, "parser", "uid-1", "demo");
        let (_, payload) = &payloads[0];
        assert_eq!(payload["spec"]["selector"], json!({ "tier": "managed" }));
        assert_eq!(payload["spec"]["size"], json!("small"));
    }

    #[test]
    fn generate_resource_claims_passes_through_persistent() {
        // ADR 0042: `needs.<type>.persistent` is copied onto the generated
        // claim's spec so the dragonfly provisioner can route persistent vs
        // ephemeral. Mirrors the `size` passthrough.
        let mut needs = BTreeMap::new();
        needs.insert(
            "redis".to_string(),
            ServiceNeed {
                name: None,
                selector: None,
                size: None,
                persistent: Some(true),
            },
        );
        // A second need WITHOUT persistent must omit the key entirely.
        needs.insert("pg".to_string(), ServiceNeed::default());
        let spec = base_with_needs(needs);
        let payloads = generate_resource_claims(&spec, "parser", "uid-1", "demo");
        // BTreeMap order: pg, redis.
        let pg = &payloads[0].1;
        let redis = &payloads[1].1;
        assert_eq!(redis["spec"]["persistent"], json!(true));
        assert!(
            pg["spec"].get("persistent").is_none(),
            "absent persistent must not emit the key"
        );
    }

    #[test]
    fn generate_resource_claims_emits_one_claim_per_named_array_entry() {
        // 2.6b: a `needs.pg` array of two named entries → two claims
        // `app-pg-a` + `app-pg-b`, each carrying `spec.type=pg`,
        // `spec.name=<entry name>`, ownerRef → app.
        use operator_core::{Needs, OneOrMany};
        let n = Needs {
            pg: Some(OneOrMany::Many(vec![
                ServiceNeed {
                    name: Some("a".into()),
                    ..Default::default()
                },
                ServiceNeed {
                    name: Some("b".into()),
                    ..Default::default()
                },
            ])),
            ..Default::default()
        };
        let spec = ApplicationBaseSpec {
            image: Some("ghcr.io/acme/web:1.0".into()),
            needs: Some(n),
            ..Default::default()
        };
        let payloads = generate_resource_claims(&spec, "app", "uid-1", "demo");
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0].0, "app-pg-a");
        assert_eq!(payloads[1].0, "app-pg-b");
        // Each claim carries spec.type=pg and spec.name = the entry name so
        // the provisioner can disambiguate sibling claims of one app.
        for (idx, expected_name) in ["a", "b"].iter().enumerate() {
            let payload = &payloads[idx].1;
            assert_eq!(payload["spec"]["type"], json!("pg"));
            assert_eq!(payload["spec"]["name"], json!(expected_name));
            let owner = &payload["metadata"]["ownerReferences"][0];
            assert_eq!(owner["name"], json!("app"));
            assert_eq!(owner["uid"], json!("uid-1"));
        }
    }

    #[test]
    fn generate_resource_claims_scalar_unnamed_omits_spec_name() {
        // The scalar (unnamed default) form is unchanged: ONE `app-pg`
        // claim, and `spec.name` must NOT be emitted (only named array
        // entries carry it).
        let mut needs = BTreeMap::new();
        needs.insert("pg".to_string(), ServiceNeed::default());
        let spec = base_with_needs(needs);
        let payloads = generate_resource_claims(&spec, "app", "uid-1", "demo");
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].0, "app-pg");
        assert!(
            payloads[0].1["spec"].get("name").is_none(),
            "unnamed default claim must not carry spec.name"
        );
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

    #[test]
    fn generate_resource_claims_emits_disk_claim_unnamed_default() {
        // 2.6b-4: a scalar `needs.disk` entry (no explicit name) →
        // ONE `<app>-disk` claim carrying spec.type=disk + spec.size, the
        // integrated-tier selector, NO spec.name (unnamed default), and
        // NO status. The claim name must match what resolve_disk_mounts
        // looks up (`<app>-disk`).
        let spec = base_with_disk(None, "/data", None);
        let payloads = generate_resource_claims(&spec, "demo-app", "uid-1", "demo");
        assert_eq!(payloads.len(), 1);
        let (name, payload) = &payloads[0];
        assert_eq!(name, "demo-app-disk");
        assert_eq!(payload["metadata"]["name"], json!("demo-app-disk"));
        assert_eq!(payload["spec"]["type"], json!("disk"));
        assert_eq!(payload["spec"]["size"], json!("1Gi"));
        // disk-local provider carries the integrated-tier selector.
        assert_eq!(payload["spec"]["selector"], json!({ "tier": "integrated" }));
        // Unnamed default → no spec.name.
        assert!(
            payload["spec"].get("name").is_none(),
            "unnamed default disk claim must not carry spec.name"
        );
        // SSA split guard: no status on the apply payload.
        assert!(payload.get("status").is_none());
        // ownerRef → Application.
        let owner = &payload["metadata"]["ownerReferences"][0];
        assert_eq!(owner["name"], json!("demo-app"));
        assert_eq!(owner["uid"], json!("uid-1"));
    }

    #[test]
    fn generate_resource_claims_emits_named_disk_claim_with_spec_name() {
        // 2.6b-4: a named disk array entry → `<app>-disk-<name>` carrying
        // spec.name = the entry name so resolve_disk_mounts (which builds
        // the same `<app>-disk-<name>`) pairs the claim with the entry.
        use operator_core::{Needs, OneOrMany};
        let spec = ApplicationBaseSpec {
            image: Some("ghcr.io/acme/web:1.0".into()),
            needs: Some(Needs {
                disk: Some(OneOrMany::Many(vec![DiskClaim {
                    name: Some("data".into()),
                    size: Some("2Gi".into()), // 2.6c: owned-disk path; reference handling in T9/T10
                    reference: None,
                    mount_path: "/var/lib/data".into(),
                    class: None,
                    read_only: None,
                }])),
                ..Default::default()
            }),
            ..Default::default()
        };
        let payloads = generate_resource_claims(&spec, "demo-app", "uid-1", "demo");
        assert_eq!(payloads.len(), 1);
        let (name, payload) = &payloads[0];
        assert_eq!(name, "demo-app-disk-data");
        assert_eq!(payload["spec"]["type"], json!("disk"));
        assert_eq!(payload["spec"]["name"], json!("data"));
        assert_eq!(payload["spec"]["size"], json!("2Gi"));
    }

    fn ready_claim(name: &str, ready: Option<bool>, secret: Option<&str>) -> ResourceClaim {
        let mut c = ResourceClaim::new(
            name,
            ResourceClaimSpec {
                type_: "pg".into(),
                name: None,
                selector: BTreeMap::from([("tier".to_string(), "integrated".to_string())]),
                size: None,
                persistent: None,
            },
        );
        c.metadata.namespace = Some("demo".into());
        c.status = Some(ResourceClaimStatus {
            provider: None,
            connection_secret_ref: secret.map(String::from),
            ready,
            conditions: None,
            ..Default::default()
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

    /// Build a disk-backed ResourceClaim status (no connectionSecretRef;
    /// its output ref is volumeClaimRef — 2.6b / ADR 0043).
    fn disk_claim(name: &str, ready: Option<bool>, vcr: Option<&str>) -> ResourceClaim {
        let mut c = ResourceClaim::new(
            name,
            ResourceClaimSpec {
                type_: "disk".into(),
                name: None,
                selector: BTreeMap::from([("tier".to_string(), "integrated".to_string())]),
                size: Some("1Gi".into()),
                persistent: None,
            },
        );
        c.metadata.namespace = Some("demo".into());
        c.status = Some(ResourceClaimStatus {
            provider: None,
            connection_secret_ref: None,
            volume_claim_ref: vcr.map(String::from),
            ready,
            conditions: None,
            ..Default::default()
        });
        c
    }

    #[test]
    fn unready_claim_names_disk_ready_via_volume_claim_ref() {
        // A disk claim has NO connectionSecretRef — readiness is gated on
        // volumeClaimRef instead (the renderer mounts that PVC).
        let ready = vec![disk_claim(
            "app-disk",
            Some(true),
            Some("claim-demo-app-disk"),
        )];
        assert!(
            unready_claim_names(&ready).is_empty(),
            "ready disk claim with volumeClaimRef must count as ready"
        );
        // ready but no volumeClaimRef (and no secret) → still unready.
        let half = vec![disk_claim("app-disk", Some(true), None)];
        assert_eq!(unready_claim_names(&half), vec!["app-disk".to_string()]);
        // a disk + a pg claim, both ready via their own output refs → empty.
        let mixed = vec![
            disk_claim("app-disk", Some(true), Some("claim-demo-app-disk")),
            ready_claim("app-pg", Some(true), Some("app-pg-conn")),
        ];
        assert!(unready_claim_names(&mixed).is_empty());
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
            image: None,
            environment: None,
            last_applied_spec: None,
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
                name: None,
                selector,
                size: None,
                persistent: None,
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
                ..Default::default()
            }),
        )];
        assert!(unready_claim_names(&provisioned).is_empty());
    }

    // ---- 2.4e: resolve ready claims → needs-type → connectionSecretRef ----

    #[test]
    fn resolve_needs_secrets_maps_type_to_connection_secret_ref() {
        // A ready unnamed pg claim with a connection secret resolves to
        // {("pg", None): "parser-pg-conn"}; the map key is the
        // `(spec.type_, spec.name)` identity, the value is
        // `status.connectionSecretRef`.
        let claims = vec![ready_claim("parser-pg", Some(true), Some("parser-pg-conn"))];
        let map = resolve_needs_secrets(&claims);
        assert_eq!(
            map.get(&("pg".to_string(), None)).map(String::as_str),
            Some("parser-pg-conn")
        );
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn resolve_needs_secrets_keys_named_claim_by_spec_name() {
        // 2.6b: a ready NAMED pg claim (spec.name = "analytics") resolves
        // to the `(pg, Some("analytics"))` key so the renderer suffixes
        // its env NAME (DATABASE_URL_ANALYTICS), while the unnamed sibling
        // keeps `(pg, None)` → DATABASE_URL.
        let default = ready_claim("parser-pg", Some(true), Some("parser-pg-conn"));
        let mut named = ready_claim(
            "parser-pg-analytics",
            Some(true),
            Some("parser-pg-analytics-conn"),
        );
        named.spec.name = Some("analytics".to_string());
        let map = resolve_needs_secrets(&[default, named]);
        assert_eq!(
            map.get(&("pg".to_string(), None)).map(String::as_str),
            Some("parser-pg-conn")
        );
        assert_eq!(
            map.get(&("pg".to_string(), Some("analytics".to_string())))
                .map(String::as_str),
            Some("parser-pg-analytics-conn")
        );
        assert_eq!(map.len(), 2);
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
        assert_eq!(
            map.get(&("pg".to_string(), None)).map(String::as_str),
            Some("parser-pg-conn")
        );
        assert!(!map.contains_key(&("redis".to_string(), None)));
        assert_eq!(map.len(), 1);
    }

    // ---- 2.6b-4: resolve ready disk claims → DiskMount render input ----

    /// Helper: a ready disk ResourceClaim with a `volumeClaimRef`. `name`
    /// is the claim's `spec.name` (the `(disk, name)` identity); `vcr` the
    /// provisioned PVC name in `status.volumeClaimRef`.
    fn ready_disk_claim(k8s_name: &str, name: Option<&str>, vcr: Option<&str>) -> ResourceClaim {
        let mut c = ResourceClaim::new(
            k8s_name,
            ResourceClaimSpec {
                type_: "disk".into(),
                name: name.map(String::from),
                selector: BTreeMap::from([("tier".to_string(), "integrated".to_string())]),
                size: Some("1Gi".into()),
                persistent: None,
            },
        );
        c.metadata.namespace = Some("demo".into());
        c.status = Some(ResourceClaimStatus {
            provider: None,
            connection_secret_ref: None,
            ready: Some(true),
            conditions: None,
            volume_claim_ref: vcr.map(String::from),
            ..Default::default()
        });
        c
    }

    /// Helper: base spec with a single disk need.
    fn base_with_disk(
        name: Option<&str>,
        mount_path: &str,
        read_only: Option<bool>,
    ) -> ApplicationBaseSpec {
        use operator_core::{Needs, OneOrMany};
        ApplicationBaseSpec {
            image: Some("ghcr.io/acme/web:1.0".into()),
            needs: Some(Needs {
                disk: Some(OneOrMany::One(DiskClaim {
                    name: name.map(String::from),
                    size: Some("1Gi".into()), // 2.6c: owned-disk path; reference handling in T9/T10
                    reference: None,
                    mount_path: mount_path.to_string(),
                    class: None,
                    read_only,
                })),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn disk_identity_name_explicit_wins_else_mountpath_last_segment() {
        // Explicit name passes through (folded).
        assert_eq!(
            disk_identity_name(&DiskClaim {
                name: Some("data".into()),
                size: Some("1Gi".into()), // 2.6c: owned-disk path; reference handling in T9/T10
                reference: None,
                mount_path: "/var/data".into(),
                class: None,
                read_only: None,
            }),
            "data"
        );
        // No name → last non-empty mountPath segment.
        assert_eq!(
            disk_identity_name(&DiskClaim {
                name: None,
                size: Some("1Gi".into()), // 2.6c: owned-disk path; reference handling in T9/T10
                reference: None,
                mount_path: "/var/lib/uploads".into(),
                class: None,
                read_only: None,
            }),
            "uploads"
        );
        // Trailing slash tolerated.
        assert_eq!(
            disk_identity_name(&DiskClaim {
                name: None,
                size: Some("1Gi".into()), // 2.6c: owned-disk path; reference handling in T9/T10
                reference: None,
                mount_path: "/data/".into(),
                class: None,
                read_only: None,
            }),
            "data"
        );
    }

    #[test]
    fn resolve_disk_mounts_pairs_entry_with_ready_claim_volume_claim_ref() {
        // An app `demo-app` with needs.disk.data {mountPath:/data} + a
        // ready claim (k8s name demo-app-disk-data, spec.name=data,
        // volumeClaimRef=claim-demo-app-disk-data) → one DiskMount
        // disk-data@/data → that PVC, readOnly false.
        let spec = base_with_disk(Some("data"), "/data", None);
        let claims = vec![ready_disk_claim(
            "demo-app-disk-data",
            Some("data"),
            Some("claim-demo-app-disk-data"),
        )];
        let mounts = resolve_disk_mounts(&spec, "demo-app", &claims);
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].volume_name, "disk-data");
        assert_eq!(mounts[0].mount_path, "/data");
        assert!(!mounts[0].read_only);
        assert_eq!(mounts[0].pvc_name, "claim-demo-app-disk-data");
    }

    #[test]
    fn resolve_disk_mounts_derives_name_from_mountpath_and_matches_default_claim() {
        // A disk entry with NO explicit name → derived identity `uploads`,
        // matched against the default disk claim (k8s name demo-app-disk,
        // spec.name=None). readOnly true threads through.
        let spec = base_with_disk(None, "/var/lib/uploads", Some(true));
        let claims = vec![ready_disk_claim(
            "demo-app-disk",
            None,
            Some("claim-demo-app-disk"),
        )];
        let mounts = resolve_disk_mounts(&spec, "demo-app", &claims);
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].volume_name, "disk-uploads");
        assert_eq!(mounts[0].mount_path, "/var/lib/uploads");
        assert!(mounts[0].read_only);
        assert_eq!(mounts[0].pvc_name, "claim-demo-app-disk");
    }

    #[test]
    fn resolve_disk_mounts_skips_claim_without_volume_claim_ref() {
        // A disk claim with no volumeClaimRef (not yet provisioned) is
        // skipped — render then omits the mount. (Post-gate this should
        // not happen, but the resolver stays defensive.)
        let spec = base_with_disk(Some("data"), "/data", None);
        let claims = vec![ready_disk_claim("demo-app-disk-data", Some("data"), None)];
        assert!(resolve_disk_mounts(&spec, "demo-app", &claims).is_empty());
    }

    #[test]
    fn resolve_disk_mounts_empty_when_no_disk_need() {
        // An app with a non-disk need (pg) yields no disk mounts.
        let mut needs = BTreeMap::new();
        needs.insert("pg".to_string(), ServiceNeed::default());
        let spec = base_with_needs(needs);
        let claims = vec![ready_claim("demo-app-pg", Some(true), Some("conn"))];
        assert!(resolve_disk_mounts(&spec, "demo-app", &claims).is_empty());
    }

    // ---- 2.6c (T9): reference disk (needs.disk.ref) — SharedVolume bind ----

    /// Helper: base spec with a single REFERENCED disk need
    /// (`needs.disk.ref = <ref>`, `mountPath = <mount_path>`, no `size`).
    fn base_with_disk_ref(reference: &str, mount_path: &str) -> ApplicationBaseSpec {
        use operator_core::{Needs, OneOrMany};
        ApplicationBaseSpec {
            image: Some("ghcr.io/acme/web:1.0".into()),
            needs: Some(Needs {
                disk: Some(OneOrMany::One(DiskClaim {
                    name: None,
                    size: None,
                    reference: Some(reference.to_string()),
                    mount_path: mount_path.to_string(),
                    class: None,
                    read_only: None,
                })),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Helper: a ready `shared-disk` reference ResourceClaim as the
    /// provisioner's T7 arm writes it — type=`shared-disk`, the binding
    /// label `apprafter.io/shared-volume=<ref>`, `status.ready=true`,
    /// `status.volumeClaimRef=<pvc_name>` (no connectionSecretRef).
    fn ready_reference_claim(reference: &str, pvc_name: &str) -> ResourceClaim {
        let mut c = ResourceClaim::new(
            "demo-app-shared-disk",
            ResourceClaimSpec {
                type_: "shared-disk".into(),
                name: None,
                selector: BTreeMap::from([("tier".to_string(), "integrated".to_string())]),
                size: None,
                persistent: None,
            },
        );
        c.metadata.namespace = Some("demo".into());
        c.metadata.labels = Some(BTreeMap::from([(
            "apprafter.io/shared-volume".to_string(),
            reference.to_string(),
        )]));
        c.status = Some(ResourceClaimStatus {
            provider: None,
            connection_secret_ref: None,
            ready: Some(true),
            conditions: None,
            volume_claim_ref: Some(pvc_name.to_string()),
            ..Default::default()
        });
        c
    }

    #[test]
    fn reference_disk_emits_shared_disk_claim_with_label() {
        // A `needs.disk.ref = shared` entry → ONE `shared-disk` claim
        // carrying the binding label + NO size + NO spec.name + the
        // integrated-tier selector + an Application ownerRef.
        let spec = base_with_disk_ref("shared", "/data");
        let claims = generate_resource_claims(&spec, "demo-app", "uid-1", "demo");
        assert_eq!(claims.len(), 1);
        let (name, claim) = claims
            .iter()
            .find(|(_, c)| c["spec"]["type"] == json!("shared-disk"))
            .expect("a shared-disk claim is emitted");
        assert_eq!(name, "demo-app-shared-disk-shared");
        assert_eq!(
            claim["metadata"]["name"],
            json!("demo-app-shared-disk-shared")
        );
        assert_eq!(
            claim["metadata"]["labels"]["apprafter.io/shared-volume"],
            json!("shared")
        );
        assert_eq!(claim["spec"]["selector"], json!({ "tier": "integrated" }));
        // No size + no spec.name on the reference claim.
        assert!(claim["spec"].get("size").is_none());
        assert!(claim["spec"].get("name").is_none());
        // SSA split guard: no status on the apply payload.
        assert!(claim.get("status").is_none());
        // ownerRef → Application (controller + blockOwnerDeletion).
        let owner = &claim["metadata"]["ownerReferences"][0];
        assert_eq!(owner["name"], json!("demo-app"));
        assert_eq!(owner["uid"], json!("uid-1"));
        assert_eq!(owner["controller"], json!(true));
        assert_eq!(owner["blockOwnerDeletion"], json!(true));
    }

    #[test]
    fn reference_disks_array_emits_distinct_claims_per_ref() {
        // Regression: an app with TWO reference disks pointing at DIFFERENT
        // SharedVolumes must emit TWO `shared-disk` claims with DISTINCT k8s
        // names (`<app>-shared-disk-<ref>`).  The old code used
        // `claim_name(app, "shared-disk", None)` → both collapsed to
        // `demo-app-shared-disk` (last-write-wins SSA bug).
        use operator_core::{Needs, OneOrMany};
        let spec = ApplicationBaseSpec {
            image: Some("ghcr.io/acme/web:1.0".into()),
            needs: Some(Needs {
                disk: Some(OneOrMany::Many(vec![
                    DiskClaim {
                        name: None,
                        size: None,
                        reference: Some("shared-a".into()),
                        mount_path: "/a".into(),
                        class: None,
                        read_only: None,
                    },
                    DiskClaim {
                        name: None,
                        size: None,
                        reference: Some("shared-b".into()),
                        mount_path: "/b".into(),
                        class: None,
                        read_only: None,
                    },
                ])),
                ..Default::default()
            }),
            ..Default::default()
        };
        let claims = generate_resource_claims(&spec, "demo-app", "uid-1", "demo");
        assert_eq!(claims.len(), 2, "one claim per referenced SharedVolume");
        let names: Vec<&str> = claims.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"demo-app-shared-disk-shared-a"),
            "expected demo-app-shared-disk-shared-a in {names:?}"
        );
        assert!(
            names.contains(&"demo-app-shared-disk-shared-b"),
            "expected demo-app-shared-disk-shared-b in {names:?}"
        );
        // Each carries its own apprafter.io/shared-volume label.
        let label_for = |ref_val: &str| {
            claims.iter().any(|(_, c)| {
                c["metadata"]["labels"]["apprafter.io/shared-volume"] == json!(ref_val)
            })
        };
        assert!(
            label_for("shared-a"),
            "missing shared-volume=shared-a label"
        );
        assert!(
            label_for("shared-b"),
            "missing shared-volume=shared-b label"
        );
    }

    #[test]
    fn resolve_disk_mounts_marks_reference_mount_unowned() {
        // A referenced disk entry pairs with the ready `shared-disk` claim
        // by the `apprafter.io/shared-volume` label, reads its
        // volumeClaimRef as the PVC, and yields a DiskMount with
        // owned=false.
        let spec = base_with_disk_ref("shared", "/data");
        let claims = vec![ready_reference_claim("shared", "sv-ns-shared")];
        let mounts = resolve_disk_mounts(&spec, "demo-app", &claims);
        assert_eq!(mounts.len(), 1);
        assert!(!mounts[0].owned);
        assert_eq!(mounts[0].pvc_name, "sv-ns-shared");
        assert_eq!(mounts[0].mount_path, "/data");
        assert!(!mounts[0].read_only);
    }

    #[test]
    fn resolve_disk_mounts_skips_reference_claim_without_volume_claim_ref() {
        // A `shared-disk` claim not yet bound (no volumeClaimRef) is
        // skipped — render omits the reference mount until the bind lands.
        let spec = base_with_disk_ref("shared", "/data");
        let mut claim = ready_reference_claim("shared", "ignored");
        claim.status.as_mut().unwrap().volume_claim_ref = None;
        assert!(resolve_disk_mounts(&spec, "demo-app", &[claim]).is_empty());
    }

    // ---- 2.4h-d: image-resolution policy gate + ImageResolved condition ----

    use operator_core::ImagePolicy;

    fn base_spec(resolve: Option<&str>) -> ApplicationBaseSpec {
        ApplicationBaseSpec {
            image: Some("ghcr.io/acme/web:1.0".into()),
            image_policy: resolve.map(|r| ImagePolicy {
                resolve: Some(r.to_string()),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn resolve_is_enabled_default_and_off() {
        // Absent imagePolicy => resolution ON (default digest, ADR 0040).
        assert!(image_resolution_enabled(&base_spec(None)));
        // resolve: "digest" => ON.
        assert!(image_resolution_enabled(&base_spec(Some("digest"))));
        // resolve: "off" => OFF (verbatim tag, no registry poll).
        assert!(!image_resolution_enabled(&base_spec(Some("off"))));
        // An empty/unknown policy object (resolve unset) defaults ON.
        let mut spec = base_spec(None);
        spec.image_policy = Some(ImagePolicy { resolve: None });
        assert!(image_resolution_enabled(&spec));
    }

    #[test]
    fn image_resolved_condition_ok_is_true_resolved() {
        let c = image_resolved_condition(true, "Resolved", &[]);
        assert_eq!(c.type_, COND_IMAGE_RESOLVED);
        assert_eq!(c.status, "True");
        assert_eq!(c.reason, "Resolved");
    }

    #[test]
    fn image_resolved_condition_failure_is_false_with_message() {
        // The failure path carries the error string into `message` so
        // `kubectl describe` surfaces WHY the verbatim tag was rendered.
        let c = image_resolved_condition(false, "ResolveFailed: registry returned status 404", &[]);
        assert_eq!(c.status, "False");
        assert_eq!(c.reason, "ResolveFailed");
        assert!(c.message.contains("404"));
    }

    #[test]
    fn image_resolved_condition_preserves_transition_time_when_status_unchanged() {
        // Same k8s convention as ready_condition: timestamp moves only
        // when `status` flips. A second resolve that still succeeds must
        // NOT bump the timestamp (else the operator hot-loops on its own
        // status write).
        let prior = vec![ApplicationCondition {
            type_: COND_IMAGE_RESOLVED.into(),
            status: "True".into(),
            last_transition_time: "2026-06-05T10:00:00+00:00".into(),
            reason: "Resolved".into(),
            message: "old".into(),
            observed_generation: None,
        }];
        let next = image_resolved_condition(true, "Resolved", &prior);
        assert_eq!(next.last_transition_time, "2026-06-05T10:00:00+00:00");
    }

    #[test]
    fn image_resolved_condition_bumps_transition_time_when_status_flips() {
        // True (resolved) → False (registry went down this cycle): the
        // transition timestamp MUST advance so downstream tooling sees a
        // real event.
        let prior = vec![ApplicationCondition {
            type_: COND_IMAGE_RESOLVED.into(),
            status: "True".into(),
            last_transition_time: "2026-06-05T10:00:00+00:00".into(),
            reason: "Resolved".into(),
            message: "ok".into(),
            observed_generation: None,
        }];
        let next = image_resolved_condition(false, "ResolveFailed: timeout", &prior);
        assert_eq!(next.status, "False");
        assert_ne!(next.last_transition_time, "2026-06-05T10:00:00+00:00");
    }

    // ---- 2.4h Fix 1 (A): registry HEAD throttle (should_resolve_image) ----

    fn status_image(tag: Option<&str>, resolved: Option<&str>, at: Option<&str>) -> StatusImage {
        StatusImage {
            tag: tag.map(String::from),
            resolved: resolved.map(String::from),
            resolved_at: at.map(String::from),
        }
    }

    #[test]
    fn should_resolve_image_when_no_prior_status() {
        // First reconcile — no prior status.image → must resolve.
        let now = Utc::now();
        assert!(should_resolve_image(
            None,
            "ghcr.io/acme/web:1.0",
            now,
            MIN_IMAGE_RESOLVE_INTERVAL_SECS
        ));
    }

    #[test]
    fn should_resolve_image_when_tag_changed() {
        // The spec tag moved (1.0 → 2.0) since the last resolution —
        // the cached digest is for the old tag, so we MUST re-resolve
        // even if the interval has not elapsed.
        let now = Utc::now();
        let prior = status_image(
            Some("ghcr.io/acme/web:1.0"),
            Some("ghcr.io/acme/web@sha256:aaa"),
            Some(&now.to_rfc3339()),
        );
        assert!(should_resolve_image(
            Some(&prior),
            "ghcr.io/acme/web:2.0",
            now,
            MIN_IMAGE_RESOLVE_INTERVAL_SECS
        ));
    }

    #[test]
    fn should_resolve_image_when_resolved_at_is_stale() {
        // Same tag, already resolved, but the last resolution is older
        // than the throttle interval → re-resolve (a moved tag would be
        // missed otherwise).
        let now = Utc::now();
        let stale =
            (now - chrono::Duration::seconds(MIN_IMAGE_RESOLVE_INTERVAL_SECS + 5)).to_rfc3339();
        let prior = status_image(
            Some("ghcr.io/acme/web:1.0"),
            Some("ghcr.io/acme/web@sha256:aaa"),
            Some(&stale),
        );
        assert!(should_resolve_image(
            Some(&prior),
            "ghcr.io/acme/web:1.0",
            now,
            MIN_IMAGE_RESOLVE_INTERVAL_SECS
        ));
    }

    #[test]
    fn should_skip_resolution_when_recent_same_tag_resolved() {
        // The hot-path guard: same tag, already resolved, within the
        // throttle window → SKIP the registry HEAD (reuse the cached
        // digest). This is what stops a HEAD on every 60s requeue /
        // every child-watch event.
        let now = Utc::now();
        let recent = (now - chrono::Duration::seconds(5)).to_rfc3339();
        let prior = status_image(
            Some("ghcr.io/acme/web:1.0"),
            Some("ghcr.io/acme/web@sha256:aaa"),
            Some(&recent),
        );
        assert!(!should_resolve_image(
            Some(&prior),
            "ghcr.io/acme/web:1.0",
            now,
            MIN_IMAGE_RESOLVE_INTERVAL_SECS
        ));
    }

    #[test]
    fn should_resolve_image_when_prior_never_resolved() {
        // Prior status recorded the attempted tag but resolution failed
        // (resolved=None, resolvedAt=None — Fix 1 (B)). The next
        // reconcile must retry, not skip.
        let now = Utc::now();
        let prior = status_image(Some("ghcr.io/acme/web:1.0"), None, None);
        assert!(should_resolve_image(
            Some(&prior),
            "ghcr.io/acme/web:1.0",
            now,
            MIN_IMAGE_RESOLVE_INTERVAL_SECS
        ));
    }

    #[test]
    fn should_resolve_image_when_resolved_at_unparseable() {
        // Defensive: a corrupt/non-RFC3339 resolvedAt can't be aged →
        // treat as stale and re-resolve.
        let now = Utc::now();
        let prior = status_image(
            Some("ghcr.io/acme/web:1.0"),
            Some("ghcr.io/acme/web@sha256:aaa"),
            Some("not-a-timestamp"),
        );
        assert!(should_resolve_image(
            Some(&prior),
            "ghcr.io/acme/web:1.0",
            now,
            MIN_IMAGE_RESOLVE_INTERVAL_SECS
        ));
    }

    // ---- 2.10 (ADR 0045): resolve_needs_targets + CNP composition ----

    #[test]
    fn resolve_needs_targets_uses_static_defaults_and_skips_disk() {
        // pg + redis resolve to their static catalog defaults; disk (and
        // any unknown type) has no network target → no entry. No override
        // map, so the namespaces are the provisioner defaults.
        let types = vec![
            "pg".to_string(),
            "redis".to_string(),
            "disk".to_string(),
            "clickhouse".to_string(),
        ];
        let targets = resolve_needs_targets(&types, &BTreeMap::new());
        // Only pg + redis have targets.
        assert_eq!(targets.len(), 2);
        let pg = targets.get("pg").expect("pg target");
        assert_eq!(pg.namespace, "cnpg-system");
        assert_eq!(pg.port, 5432);
        let redis = targets.get("redis").expect("redis target");
        assert_eq!(redis.namespace, "dragonfly-system");
        assert_eq!(redis.port, 6379);
        // disk + unknown contribute no catalog entry.
        assert!(!targets.contains_key("disk"));
        assert!(!targets.contains_key("clickhouse"));
    }

    #[test]
    fn resolve_needs_targets_applies_namespace_override() {
        // A per-type namespace override (the namespace the provisioner reads
        // from ServiceProvider.spec.config) replaces the static default; the
        // selector + port stay the catalog defaults.
        let overrides = BTreeMap::from([("pg".to_string(), "custom-pg-ns".to_string())]);
        let targets = resolve_needs_targets(&["pg".to_string()], &overrides);
        let pg = targets.get("pg").expect("pg target");
        assert_eq!(pg.namespace, "custom-pg-ns");
        assert_eq!(pg.port, 5432);
        assert_eq!(
            pg.pod_selector.get("cnpg.io/cluster").map(String::as_str),
            Some("platform-postgres")
        );
    }

    #[test]
    fn render_for_env_with_pg_need_and_target_emits_cnp_with_pg_rule() {
        // 2.10 composition (pure, no client): an app with needs.pg + a
        // non-empty pg target → render_application_for_env yields a CNP with
        // the pg egress rule. Mirrors what the reconcile loop threads in.
        use operator_core::{Needs, OneOrMany, ServiceNeed};
        let spec = ApplicationSpec {
            base: Some(ApplicationBaseSpec {
                image: Some("ghcr.io/acme/web:1.0".into()),
                needs: Some(Needs {
                    pg: Some(OneOrMany::One(ServiceNeed::default())),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            environments: None,
            environment: None,
        };
        let mut app = Application::new("web", spec);
        app.metadata.namespace = Some("demo".into());
        app.metadata.uid = Some("uid-1".into());

        let targets = resolve_needs_targets(&["pg".to_string()], &BTreeMap::new());
        assert!(!targets.is_empty(), "pg yields a non-empty target catalog");

        let rendered = render_application_for_env(
            &app,
            None,
            None,
            None,
            None,
            EgressProfile::Internet,
            Some(&targets),
        );
        let cnp = rendered
            .network_policy
            .as_ref()
            .expect("CNP rendered when needs_targets is threaded");
        assert_eq!(cnp["kind"], json!("CiliumNetworkPolicy"));
        assert_eq!(cnp["apiVersion"], json!("cilium.io/v2"));
        assert_eq!(cnp["metadata"]["name"], json!("web-egress"));
        let rules = cnp["spec"]["egress"].as_array().expect("egress rules");
        // DNS + same-ns + world (internet baseline) + pg = 4.
        assert_eq!(rules.len(), 4);
        // The pg rule targets cnpg-system on 5432.
        let pg_rule = rules
            .iter()
            .find(|r| {
                r["toEndpoints"][0]["matchLabels"]["io.kubernetes.pod.namespace"] == "cnpg-system"
            })
            .expect("pg egress rule present");
        assert_eq!(pg_rule["toPorts"][0]["ports"][0]["port"], "5432");
    }

    #[test]
    fn apply_network_policy_sets_namespace_and_owner_reference() {
        // The apply helper's pure body-mutation half: it must stamp
        // metadata.namespace + metadata.ownerReferences (the cascading-delete
        // ownerRef) onto the rendered CNP before SSA. We exercise the body
        // mutation directly (the network call needs a cluster) by replicating
        // the two assignments the helper makes.
        let owner = owner_reference(&{
            let mut app = Application::new("web", ApplicationSpec::default());
            app.metadata.uid = Some("uid-1".into());
            app
        });
        let mut body = json!({
            "apiVersion": "cilium.io/v2",
            "kind": "CiliumNetworkPolicy",
            "metadata": { "name": "web-egress" },
            "spec": { "egress": [] }
        });
        body["metadata"]["namespace"] = json!("demo");
        body["metadata"]["ownerReferences"] = json!([owner]);

        assert_eq!(body["metadata"]["namespace"], json!("demo"));
        let or = &body["metadata"]["ownerReferences"][0];
        assert_eq!(or["apiVersion"], json!("apprafter.io/v1alpha1"));
        assert_eq!(or["kind"], json!("Application"));
        assert_eq!(or["name"], json!("web"));
        assert_eq!(or["uid"], json!("uid-1"));
        assert_eq!(or["controller"], json!(true));
        assert_eq!(or["blockOwnerDeletion"], json!(true));
    }

    // ---- 2.12e: env secret-ref existence check (pure helper) ----

    use operator_core::{EnvRef, EnvValue};

    #[test]
    fn flags_missing_env_secret_refs() {
        // A secret ref whose Secret+key does not exist → flagged.
        // Literal + Claim refs are never checked here.
        let mut env = std::collections::BTreeMap::new();
        env.insert(
            "K".to_string(),
            EnvValue::Ref(EnvRef::Secret("stripe/api-key".into())),
        );
        env.insert("LOG".to_string(), EnvValue::Literal("info".into()));
        // Claim refs are NOT checked (gated by the AwaitingResourceClaim path):
        env.insert(
            "DB".to_string(),
            EnvValue::Ref(EnvRef::Claim("pg.url".into())),
        );

        // Nothing exists → one missing message (only the secret ref is checked).
        let missing = unresolved_env_secret_refs(&env, &|_n, _k| false);
        assert_eq!(missing.len(), 1, "expected 1 missing, got: {:?}", missing);
        assert!(
            missing[0].contains("K"),
            "message should name the env var: {}",
            missing[0]
        );
        assert!(
            missing[0].contains("stripe/api-key"),
            "message should include the ref path: {}",
            missing[0]
        );

        // Secret exists with the right key → empty (no missing).
        let ok = unresolved_env_secret_refs(&env, &|n, k| n == "stripe" && k == "api-key");
        assert!(
            ok.is_empty(),
            "all resolved — expected empty, got: {:?}",
            ok
        );
    }

    #[test]
    fn multiple_secret_refs_all_missing() {
        // Two secret refs, neither exists → two messages in BTreeMap order.
        let mut env = std::collections::BTreeMap::new();
        env.insert(
            "A_KEY".to_string(),
            EnvValue::Ref(EnvRef::Secret("sa/key-a".into())),
        );
        env.insert(
            "B_KEY".to_string(),
            EnvValue::Ref(EnvRef::Secret("sb/key-b".into())),
        );
        let missing = unresolved_env_secret_refs(&env, &|_n, _k| false);
        assert_eq!(missing.len(), 2);
        // BTreeMap order: A_KEY before B_KEY.
        assert!(
            missing[0].contains("A_KEY"),
            "first should be A_KEY: {:?}",
            missing
        );
        assert!(
            missing[1].contains("B_KEY"),
            "second should be B_KEY: {:?}",
            missing
        );
    }

    #[test]
    fn partial_secret_refs_one_missing() {
        // Three secret refs, only the middle one missing.
        let mut env = std::collections::BTreeMap::new();
        env.insert(
            "A_KEY".to_string(),
            EnvValue::Ref(EnvRef::Secret("sa/key-a".into())),
        );
        env.insert(
            "B_KEY".to_string(),
            EnvValue::Ref(EnvRef::Secret("sb/key-b".into())),
        );
        env.insert(
            "C_KEY".to_string(),
            EnvValue::Ref(EnvRef::Secret("sc/key-c".into())),
        );
        // Only B_KEY is missing.
        let missing = unresolved_env_secret_refs(&env, &|n, _k| n != "sb");
        assert_eq!(missing.len(), 1);
        assert!(
            missing[0].contains("B_KEY"),
            "only B_KEY missing: {:?}",
            missing
        );
    }

    #[test]
    fn no_secret_refs_returns_empty() {
        // Env with only literals + claim refs → no secret check, always empty.
        let mut env = std::collections::BTreeMap::new();
        env.insert("LOG".to_string(), EnvValue::Literal("debug".into()));
        env.insert(
            "DB".to_string(),
            EnvValue::Ref(EnvRef::Claim("pg.url".into())),
        );
        let missing = unresolved_env_secret_refs(&env, &|_n, _k| false);
        assert!(missing.is_empty());
    }

    #[test]
    fn empty_env_returns_empty() {
        let env = std::collections::BTreeMap::new();
        let missing = unresolved_env_secret_refs(&env, &|_n, _k| false);
        assert!(missing.is_empty());
    }

    #[test]
    fn build_env_secret_missing_status_sets_phase_and_ready_false() {
        // Status builder contract: phase = EnvSecretMissing, Ready=False,
        // reason = EnvSecretMissing, message contains the diagnostics,
        // observedGeneration + endpointURL preserved.
        let mut app = Application::new("web", ApplicationSpec::default());
        app.metadata.generation = Some(5);
        app.status = Some(ApplicationStatus {
            phase: Some("Ready".into()),
            observed_generation: Some(4),
            conditions: None,
            endpoint_url: Some("http://web.demo.svc.cluster.local:80".into()),
            image: None,
            environment: None,
            last_applied_spec: None,
        });
        let messages = vec![
            "env STRIPE_KEY → secret \"stripe/api-key\": Secret \"stripe\" not found or missing key \"api-key\"".to_string(),
        ];
        let status = build_env_secret_missing_status(&app, &messages);

        assert_eq!(status.phase.as_deref(), Some(PHASE_ENV_SECRET_MISSING));
        assert_eq!(status.observed_generation, Some(5));
        assert_eq!(
            status.endpoint_url.as_deref(),
            Some("http://web.demo.svc.cluster.local:80")
        );

        let conds = status.conditions.as_ref().expect("conditions present");
        assert_eq!(conds.len(), 1, "exactly one condition");
        let ready = &conds[0];
        assert_eq!(ready.type_, "Ready");
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "EnvSecretMissing");
        assert!(
            ready.message.contains("STRIPE_KEY"),
            "message should carry the var name: {}",
            ready.message
        );
    }

    #[test]
    fn build_env_secret_missing_status_preserves_endpoint_when_status_absent() {
        // First reconcile: app.status is None → endpoint stays None.
        let app = Application::new("web", ApplicationSpec::default());
        let status = build_env_secret_missing_status(
            &app,
            &["env K → secret \"s/k\": Secret \"s\" not found or missing key \"k\"".to_string()],
        );
        assert!(status.endpoint_url.is_none());
        assert_eq!(status.phase.as_deref(), Some(PHASE_ENV_SECRET_MISSING));
    }

    #[test]
    fn build_env_secret_missing_status_preserves_transition_time() {
        // lastTransitionTime must NOT change when Ready=False was already
        // set for the same reason (k8s hot-reconcile-prevention convention).
        let fixed_time = "2026-06-10T12:00:00+00:00".to_string();
        let mut app = Application::new("web", ApplicationSpec::default());
        app.status = Some(ApplicationStatus {
            phase: Some(PHASE_ENV_SECRET_MISSING.into()),
            observed_generation: None,
            conditions: Some(vec![ApplicationCondition {
                type_: "Ready".into(),
                status: "False".into(),
                last_transition_time: fixed_time.clone(),
                reason: "EnvSecretMissing".into(),
                message: "prior".into(),
                observed_generation: None,
            }]),
            endpoint_url: None,
            image: None,
            environment: None,
            last_applied_spec: None,
        });
        let status = build_env_secret_missing_status(
            &app,
            &["env K → secret \"s/k\": Secret \"s\" not found or missing key \"k\"".to_string()],
        );
        let ready = status
            .conditions
            .as_ref()
            .unwrap()
            .iter()
            .find(|c| c.type_ == "Ready")
            .expect("Ready condition");
        assert_eq!(
            ready.last_transition_time, fixed_time,
            "lastTransitionTime must be preserved when Ready=False is unchanged"
        );
    }

    // ---- 1.83b: PublicRouteReady pure helpers ----

    #[test]
    fn hostname_covered_by_zone_apex_and_single_label_wildcard() {
        assert!(hostname_covered_by_zone("demo.dev", "demo.dev"));
        assert!(hostname_covered_by_zone("app.demo.dev", "demo.dev"));
        assert!(!hostname_covered_by_zone("a.b.demo.dev", "demo.dev"));
        assert!(!hostname_covered_by_zone("app.other.dev", "demo.dev"));
        assert!(!hostname_covered_by_zone(".demo.dev", "demo.dev"));
    }

    #[test]
    fn evaluate_public_route_no_matching_zone_is_false() {
        let (status, reason, msg) =
            evaluate_public_route(&["app.unknown.dev".into()], &["demo.dev".into()], None);
        assert_eq!(status, "False");
        assert_eq!(reason, "NoMatchingZone");
        assert!(msg.contains("app.unknown.dev"));
    }

    #[test]
    fn evaluate_public_route_covered_but_no_status_is_pending() {
        let (status, reason, _) =
            evaluate_public_route(&["app.demo.dev".into()], &["demo.dev".into()], None);
        assert_eq!(status, "False");
        assert_eq!(reason, "Pending");
    }

    #[test]
    fn evaluate_public_route_covered_and_accepted_is_true() {
        let route_status = serde_json::json!({
            "parents": [{
                "conditions": [
                    { "type": "Accepted", "status": "True" },
                    { "type": "ResolvedRefs", "status": "True" }
                ]
            }]
        });
        let (status, reason, _) = evaluate_public_route(
            &["app.demo.dev".into()],
            &["demo.dev".into()],
            Some(&route_status),
        );
        assert_eq!(status, "True");
        assert_eq!(reason, "Accepted");
    }

    #[test]
    fn allowed_domains_from_values_reads_gateway_allowed_domains() {
        use operator_core::PlatformStackValues;
        let values: PlatformStackValues = serde_json::from_value(serde_json::json!({
            "tier": 1,
            "gateway": { "allowedDomains": [
                { "domain": "demo.dev", "importedCertRef": "demo-cert" },
                { "domain": "demo2.dev", "importedCertRef": "demo2-cert" }
            ]}
        }))
        .unwrap();
        assert_eq!(
            allowed_domains_from_values(&values),
            vec!["demo.dev".to_string(), "demo2.dev".to_string()]
        );
        let bare: PlatformStackValues =
            serde_json::from_value(serde_json::json!({ "tier": 1 })).unwrap();
        assert!(allowed_domains_from_values(&bare).is_empty());
    }

    // 2.16b Task 9 (R1-H1): app-scope MigrationPlans land in the APP
    // namespace, so the blocking-plan finder must search there — NOT
    // the platform `apprafter-system` namespace.
    #[test]
    fn blocking_plan_searched_in_app_namespace() {
        assert_eq!(blocking_plan_namespace("team-a"), "team-a"); // NOT "apprafter-system"
    }

    // 2.16b Task 10 (R2-H2 / R3-M1): pure state-machine decision fn.
    // All 8 detect × plan-state cells (12 total incl. detect=None
    // buckets) must map exactly to the spec's decision table.
    #[test]
    fn state_machine_cells() {
        use MigrationDecision::*;
        // detect = None
        assert_eq!(decide(false, PlanState::None), Render);
        assert_eq!(decide(false, PlanState::BlockingMatch), DeleteThenRender);
        assert_eq!(decide(false, PlanState::BlockingMismatch), DeleteThenRender);
        assert_eq!(decide(false, PlanState::Failed), DeleteThenRender);
        assert_eq!(decide(false, PlanState::CompletedMatch), DeleteThenRender);
        assert_eq!(decide(false, PlanState::Relic), DeleteThenRender);
        // detect = Some
        assert_eq!(decide(true, PlanState::None), CreatePlan);
        assert_eq!(decide(true, PlanState::BlockingMatch), NoOp);
        assert_eq!(decide(true, PlanState::BlockingMismatch), DeleteThenCreate);
        assert_eq!(decide(true, PlanState::Failed), BlockFailed);
        assert_eq!(decide(true, PlanState::CompletedMatch), ConsumeApply);
        assert_eq!(decide(true, PlanState::Relic), DeleteThenCreate);
    }

    // Helper: a change whose (trigger_type, field) can be tuned to
    // match / not-match a plan's trigger for the `plan_state` bucketer.
    fn change(trigger_type: &str, field: &str) -> DestructiveChange {
        DestructiveChange {
            trigger_type: trigger_type.into(),
            field: field.into(),
            from: None,
            to: None,
            classification: "breaking".into(),
        }
    }

    // Helper: an app-scope plan carrying a specific (type, field)
    // trigger and phase — the `app_plan` helper above hard-codes the
    // trigger to `t`/`f`, so build one directly here.
    fn plan_with_trigger(trigger_type: &str, field: &str, phase: Option<&str>) -> MigrationPlan {
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
                type_: trigger_type.into(),
                field: field.into(),
                from: None,
                to: None,
            },
            risks: None,
            plan: None,
            approvers: None,
            previous_spec_snapshot: None,
        };
        let mut plan = MigrationPlan::new("parser-pg", spec);
        if let Some(p) = phase {
            plan.status = Some(MigrationPlanStatus {
                phase: Some(p.into()),
                ..MigrationPlanStatus::default()
            });
        }
        plan
    }

    #[test]
    fn plan_state_buckets_by_phase_and_trigger_match() {
        let cur = change("selector-change", "needs.pg.selector");
        // No plan → None.
        assert_eq!(plan_state(None, &cur), PlanState::None);
        // Blocking (pending) + trigger matches → BlockingMatch.
        let matching = plan_with_trigger(
            "selector-change",
            "needs.pg.selector",
            Some("pending-approval"),
        );
        assert_eq!(plan_state(Some(&matching), &cur), PlanState::BlockingMatch);
        // Blocking (pending) + trigger differs → BlockingMismatch.
        let mismatch = plan_with_trigger(
            "storage-class-change",
            "needs.pg.storage",
            Some("pending-approval"),
        );
        assert_eq!(
            plan_state(Some(&mismatch), &cur),
            PlanState::BlockingMismatch
        );
        // Phase failed → Failed (regardless of trigger match).
        let failed = plan_with_trigger("selector-change", "needs.pg.selector", Some("failed"));
        assert_eq!(plan_state(Some(&failed), &cur), PlanState::Failed);
        // Completed + trigger matches → CompletedMatch.
        let done = plan_with_trigger("selector-change", "needs.pg.selector", Some("completed"));
        assert_eq!(plan_state(Some(&done), &cur), PlanState::CompletedMatch);
        // Completed + trigger differs → Relic.
        let done_other = plan_with_trigger(
            "storage-class-change",
            "needs.pg.storage",
            Some("completed"),
        );
        assert_eq!(plan_state(Some(&done_other), &cur), PlanState::Relic);
        // Rejected → Relic.
        let rejected = plan_with_trigger("selector-change", "needs.pg.selector", Some("rejected"));
        assert_eq!(plan_state(Some(&rejected), &cur), PlanState::Relic);
    }

    // ---- 2.16b Task 11: reconcile-wiring pure seams ----

    // `plan_state_no_change` — the detect=None bucketer. No plan → None
    // (`decide(false, None) = Render`); any plan → Relic (`decide(false, _)
    // = DeleteThenRender`). The exact non-None variant is irrelevant to
    // `decide`'s `has_change=false` rows, so bucketing every plan as Relic
    // is sound.
    #[test]
    fn plan_state_no_change_buckets_presence_only() {
        assert_eq!(plan_state_no_change(None), PlanState::None);
        let any = plan_with_trigger("selector-change", "needs.pg.selector", Some("completed"));
        assert_eq!(plan_state_no_change(Some(&any)), PlanState::Relic);
        // And it composes with `decide` to the right no-change decisions.
        assert_eq!(
            decide(false, plan_state_no_change(None)),
            MigrationDecision::Render
        );
        assert_eq!(
            decide(false, plan_state_no_change(Some(&any))),
            MigrationDecision::DeleteThenRender
        );
    }

    // `with_stamped_baseline` — sets `last_applied_spec` to the passed spec
    // and preserves every other status field it was handed (mirrors how the
    // happy-path stamp folds into the render's own status write).
    #[test]
    fn with_stamped_baseline_sets_field_and_preserves_rest() {
        let base = ApplicationStatus {
            phase: Some("Ready".into()),
            observed_generation: Some(9),
            conditions: Some(vec![ready_condition("True", "Ok", "ok", &[])]),
            endpoint_url: Some("http://web.demo.svc.cluster.local:80".into()),
            image: Some(StatusImage {
                tag: Some("app:v1".into()),
                resolved: Some("app@sha256:abc".into()),
                resolved_at: Some("2026-07-01T00:00:00Z".into()),
            }),
            environment: Some("prod".into()),
            last_applied_spec: None,
        };
        let spec = ApplicationSpec {
            base: Some(ApplicationBaseSpec {
                image: Some("app:v1".into()),
                ..Default::default()
            }),
            environment: Some("prod".into()),
            ..Default::default()
        };
        let stamped = with_stamped_baseline(base.clone(), &spec);
        // The one new field is set to the RAW spec.
        assert_eq!(stamped.last_applied_spec.as_ref(), Some(&spec));
        assert_eq!(
            stamped
                .last_applied_spec
                .as_ref()
                .unwrap()
                .base
                .as_ref()
                .unwrap()
                .image
                .as_deref(),
            Some("app:v1")
        );
        // Every other field is carried through untouched.
        assert_eq!(stamped.phase, base.phase);
        assert_eq!(stamped.observed_generation, base.observed_generation);
        assert_eq!(stamped.conditions, base.conditions);
        assert_eq!(stamped.endpoint_url, base.endpoint_url);
        assert_eq!(stamped.image, base.image);
        assert_eq!(stamped.environment, base.environment);
    }

    // `plans_to_delete` — the pure filter behind `delete_all_key_plans_except`.
    // Keeps `keep`, drops every OTHER key-matching plan, ignores plans of a
    // different app / env / scope.
    #[test]
    fn plans_to_delete_keeps_keep_and_scopes_by_app_env() {
        // Two plans for (parser, prod): one is the keep, one is stale.
        let keep = {
            let mut p = plan_with_trigger("a", "f", Some("pending-approval"));
            p.metadata.name = Some("parser-prod-migration-2".into());
            p
        };
        let stale = {
            let mut p = plan_with_trigger("b", "g", Some("completed"));
            p.metadata.name = Some("parser-prod-migration-1".into());
            p
        };
        // A plan for the SAME app but a DIFFERENT env — must be ignored.
        let other_env = {
            let mut p = plan_with_trigger("a", "f", Some("pending-approval"));
            p.spec.scope.application.as_mut().unwrap().environment = "dev".into();
            p.metadata.name = Some("parser-dev-migration-1".into());
            p
        };
        // A plan for a DIFFERENT app — must be ignored.
        let other_app = {
            let mut p = plan_with_trigger("a", "f", Some("pending-approval"));
            p.spec.scope.application.as_mut().unwrap().ref_.name = "other".into();
            p.metadata.name = Some("other-prod-migration-1".into());
            p
        };
        // A platform-scope plan — must be ignored (wrong scope).
        let platform = platform_plan("platform-bump");

        let plans = vec![keep.clone(), stale.clone(), other_env, other_app, platform];
        let to_delete = plans_to_delete(&plans, "parser", "prod", Some("parser-prod-migration-2"));
        // Only the stale (parser, prod) plan that is NOT the keep is deleted.
        assert_eq!(to_delete, vec!["parser-prod-migration-1".to_string()]);

        // keep = None → both (parser, prod) plans are deleted; scope filter
        // still excludes the other-app / other-env / platform plans.
        let all = plans_to_delete(&plans, "parser", "prod", None);
        assert_eq!(
            all,
            vec![
                "parser-prod-migration-2".to_string(),
                "parser-prod-migration-1".to_string(),
            ]
        );
    }

    // `effective_baseline` — reconstructs an Application from the stamped
    // spec and unifies it under the baseline's OWN environment (H4/R2-M2).
    // The prod override must win over base when the baseline pins env=prod.
    #[test]
    fn effective_baseline_unifies_under_baseline_own_env() {
        let baseline = ApplicationSpec {
            base: Some(ApplicationBaseSpec {
                image: Some("app:base".into()),
                replicas: Some(1),
                ..Default::default()
            }),
            environments: Some(
                [(
                    "prod".to_string(),
                    ApplicationBaseSpec {
                        replicas: Some(5),
                        ..Default::default()
                    },
                )]
                .into_iter()
                .collect(),
            ),
            environment: Some("prod".into()),
        };
        let eff = effective_baseline(&baseline);
        // base.image survives; prod override replaces replicas.
        assert_eq!(eff.image.as_deref(), Some("app:base"));
        assert_eq!(eff.replicas, Some(5));

        // With NO environment pinned, the base (replicas=1) is the effective.
        let base_only = ApplicationSpec {
            base: baseline.base.clone(),
            environments: baseline.environments.clone(),
            environment: None,
        };
        assert_eq!(effective_baseline(&base_only).replicas, Some(1));
    }

    // `plan_name` — stable, DNS-1123-safe `<app>-<env>-migration-<secs>`;
    // empty env collapses to `<app>-migration-<secs>`.
    #[test]
    fn plan_name_is_dns_safe_and_env_aware() {
        let now = DateTime::parse_from_rfc3339("2026-07-17T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let secs = now.timestamp();
        assert_eq!(
            plan_name("web", "prod", now),
            format!("web-prod-migration-{secs}")
        );
        // Empty env → no double dash, collapses cleanly.
        assert_eq!(plan_name("web", "", now), format!("web-migration-{secs}"));
        // Uppercase / underscores fold to a DNS-1123 name.
        let folded = plan_name("Web_App", "Prod", now);
        assert!(folded
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
        assert!(folded.starts_with("web-app-prod-migration-"));
    }

    // `app_uid_or` — returns the uid when present, a clear MissingUid error
    // otherwise (never emits an owner-less plan).
    #[test]
    fn app_uid_or_returns_uid_or_errors() {
        let mut app = Application::new("web", ApplicationSpec::default());
        app.metadata.uid = Some("uid-abc".into());
        assert_eq!(app_uid_or(&app).unwrap(), "uid-abc");

        let no_uid = Application::new("web", ApplicationSpec::default());
        let err = app_uid_or(&no_uid).unwrap_err();
        assert!(matches!(err, ReconcileError::MissingUid(name) if name == "web"));
    }

    // `build_migration_failed_status` — the BlockFailed arm status: phase
    // stays AwaitingMigrationApproval, Ready=False/MigrationFailed, a
    // MigrationFailed=True condition naming the plan; endpoint + generation
    // preserved; lastAppliedSpec omitted (the failed gate does NOT re-stamp).
    #[test]
    fn build_migration_failed_status_sets_failed_condition() {
        let mut app = Application::new("web", ApplicationSpec::default());
        app.metadata.generation = Some(8);
        app.status = Some(ApplicationStatus {
            phase: Some("Ready".into()),
            observed_generation: Some(7),
            conditions: None,
            endpoint_url: Some("http://web.demo.svc.cluster.local:80".into()),
            image: None,
            environment: None,
            last_applied_spec: Some(ApplicationSpec {
                base: Some(ApplicationBaseSpec {
                    image: Some("app:v1".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        });
        let status = build_migration_failed_status(&app, "web-prod-migration-3");
        assert_eq!(
            status.phase.as_deref(),
            Some(PHASE_AWAITING_MIGRATION_APPROVAL)
        );
        assert_eq!(status.observed_generation, Some(8));
        assert_eq!(
            status.endpoint_url.as_deref(),
            Some("http://web.demo.svc.cluster.local:80")
        );
        // 2.16b (walk-found): a failed gate must NOT re-stamp the baseline to
        // the NEW spec — but under SSA it MUST re-send the EXISTING baseline,
        // because omitting a field the manager owns PRUNES it (an omitted
        // `None` did NOT "survive on the wire" — it wiped the baseline and the
        // gate self-cancelled). So the prior baseline is carried forward here.
        assert_eq!(
            status
                .last_applied_spec
                .as_ref()
                .and_then(|s| s.base.as_ref())
                .and_then(|b| b.image.as_deref()),
            Some("app:v1")
        );

        let conds = status.conditions.as_ref().expect("conditions");
        let ready = conds.iter().find(|c| c.type_ == "Ready").expect("ready");
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "MigrationFailed");
        let failed = conds
            .iter()
            .find(|c| c.type_ == COND_MIGRATION_FAILED)
            .expect("migration failed");
        assert_eq!(failed.status, "True");
        assert!(failed.message.contains("web-prod-migration-3"));
    }
}
