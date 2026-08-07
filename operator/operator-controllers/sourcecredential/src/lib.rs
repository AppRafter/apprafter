// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs Controller for the v1alpha1 `SourceCredential` CRD (1.79c /
//! ADR 0039).
//!
//! A `SourceCredential` is a config-only reference object. This
//! controller is the single owner of every derived materialisation:
//!
//!   - **git half:** for each `repoPrefixes` entry, a prefix-matched
//!     Argo CD `repo-creds` Secret in the `argocd` namespace, so Argo
//!     CD can clone the private repo.
//!   - **registry half:** a canonical `dockerconfigjson` Secret in the
//!     SourceCredential's namespace; the Application controller projects
//!     a per-workload copy and attaches it to `imagePullSecrets`
//!     (Seam A).
//!
//! The credential material is read from the Secret the sealed-secrets
//! controller unsealed (`spec.git.backend.sealedSecretRef.name`,
//! defaulting to the SourceCredential's own namespace). The material
//! Secret carries two keys: `username` and `password` (the PAT). The
//! controller never holds the sealed blob — only the controller's
//! private key unseals, and that already happened by the time this
//! reconcile reads the materialised Secret.
//!
//! Validity is probed live (S5): for each covered prefix the controller
//! finds a representative object actually in use — a git repo from a
//! matching Argo CD `Application`, an image from a matching AppRafter
//! `Application` — and probes it (git smart-HTTP / a scoped registry v2
//! token exchange). The verdict lands in `GitValid` / `RegistryValid`
//! (`True` Reachable / `False` AuthRejected / `Unknown` Unverified) with
//! `status.lastValidated`. When no application references a covered
//! prefix yet, or egress is blocked, the half stays `Unverified` — the
//! state the `present` coverage gate accepts by design (ADR 0039
//! §Validation and status). See `validity.rs`.
//!
//! Derived Secrets are garbage-collected on delete via a finalizer
//! (`apprafter.io/derived-secrets-cleanup`): cross-namespace
//! ownerReferences are disallowed by Kubernetes, so the controller GCs
//! the Argo `repo-creds` (in `argocd`) and the canonical
//! `dockerconfigjson` (selected by the `apprafter.io/source-credential`
//! label) itself before releasing the finalizer. Per-workload
//! pull-secret copies are owned by the Application controller (Seam A)
//! and cascade with their app. (Reclaiming a single derived Secret when
//! only one `repoPrefix` is *removed* — coverage narrowing, distinct
//! from full deletion — is the destructive change the MigrationPlan gate
//! classifies; see `operator-controllers-migration`.)

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use k8s_openapi::api::core::v1::Secret;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Client, ResourceExt};
use serde_json::{json, Value};
use thiserror::Error;
use tracing::{info, warn};

// 2.16b-sc Task 7: the scope-agnostic migration state machine (hoisted in
// Task 1/5) + the SourceCredential-scope classifier / plan builder (Task 6).
use operator_controllers_migration::SourceCredentialMigrationStrategy;
use operator_core::migration_state::{decide, plan_state, plan_state_no_change, MigrationDecision};
use operator_core::{
    DestructiveChange, Metrics, MigrationPlan, SourceCredential, SourceCredentialCondition,
    SourceCredentialSpec, SourceCredentialStatus, COND_GIT_PRESENT, COND_GIT_VALID,
    COND_MIGRATION_PENDING, COND_REGISTRY_PRESENT, COND_REGISTRY_VALID,
    PHASE_AWAITING_MIGRATION_APPROVAL, REASON_UNVERIFIED,
};

mod validity;
use validity::Validity;

const KIND: &str = "SourceCredential";

/// SSA field manager for everything this controller owns (status +
/// derived Secrets). Distinct from `apprafter-operator` so the
/// Application controller and this one never fight over fields.
pub const FIELD_MANAGER: &str = "apprafter-sourcecredential";

/// Namespace Argo CD reads `repo-creds` Secrets from.
const ARGOCD_NAMESPACE: &str = "argocd";

/// Finalizer that blocks SourceCredential deletion until the derived
/// Secrets (cross-namespace, so no ownerReference cascade is possible)
/// are garbage-collected.
const DERIVED_SECRETS_FINALIZER: &str = "apprafter.io/derived-secrets-cleanup";

/// Label every derived Secret carries, keyed to the owning
/// SourceCredential — the GC selector and the human "who made this".
const SOURCE_CREDENTIAL_LABEL: &str = "apprafter.io/source-credential";

/// 2.16b-sc Task 7: label `create_plan_for` stamps on the gating
/// MigrationPlan, marking a SourceCredential-scope plan. Paired with
/// [`SOURCE_CREDENTIAL_LABEL`] it selects the (≤1) plan for a given
/// credential from its namespace — the SC-scope analogue of the app
/// controller's `find_any_key_plan`.
const SCOPE_LABEL: &str = "apprafter.io/scope";
const SCOPE_SOURCECREDENTIAL: &str = "sourcecredential";

/// Keys in the unsealed material Secret.
const MATERIAL_USERNAME_KEY: &str = "username";
const MATERIAL_PASSWORD_KEY: &str = "password";

/// Username used when the material Secret omits `username` — GitHub
/// accepts any non-empty username with a PAT password.
const DEFAULT_GIT_USERNAME: &str = "git";

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("kube-rs error: {0}")]
    Kube(#[from] kube::Error),

    #[error("serde_json error: {0}")]
    Serde(#[from] serde_json::Error),

    /// 2.16b-sc Task 7: a SourceCredential without a `metadata.uid` reached
    /// the MigrationPlan-creation path. The uid is required for the plan's
    /// controller `ownerReference` (so the plan cascades on SourceCredential
    /// delete); the apiserver always assigns one, so this is defensive —
    /// surface it rather than emit an owner-less plan. Mirrors the app-scope
    /// `ReconcileError::MissingUid`.
    #[error("SourceCredential {0} has no metadata.uid; cannot own a MigrationPlan")]
    MissingUid(String),
}

/// Per-controller reconcile context.
pub struct Context {
    pub client: Client,
    pub metrics: Arc<Metrics>,
}

/// Spawn the SourceCredential Controller. Watches
/// `apprafter.io/v1alpha1` `SourceCredential` resources cluster-wide.
pub async fn run(client: Client, metrics: Arc<Metrics>) -> Result<(), ReconcileError> {
    let creds: Api<SourceCredential> = Api::all(client.clone());
    // 2.16b-sc Task 7: watch the SC-scope MigrationPlan children so a plan
    // reaching `completed` (a same-ns child with a controlling ownerRef → the
    // SourceCredential, set by `create_plan_for`) re-fires the owning
    // SourceCredential reconcile IMMEDIATELY (instant consume → ConsumeApply →
    // derive with the narrowed coverage), instead of waiting for the 60s
    // requeue. Mirrors the Application controller's `.owns(plans)`.
    let plans: Api<MigrationPlan> = Api::all(client.clone());
    let context = Arc::new(Context { client, metrics });

    Controller::new(creds, watcher::Config::default())
        .owns(plans, watcher::Config::default())
        .run(reconcile, error_policy, context)
        .for_each(|res| async move {
            match res {
                Ok((obj_ref, _action)) => info!(?obj_ref, "sourcecredential step ok"),
                Err(err) => warn!(%err, "sourcecredential step error"),
            }
        })
        .await;
    Ok(())
}

/// Reconcile: derive the git half's Argo `repo-creds` Secret(s) and the
/// registry half's canonical `dockerconfigjson` from the unsealed
/// material, and report per-half status. Requeues every 60s so material
/// rotation is picked up (15s while material is still unsealing).
pub async fn reconcile(
    cred: Arc<SourceCredential>,
    ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    let name = cred.name_any();
    let namespace = cred.namespace().unwrap_or_default();
    let _timer = ctx
        .metrics
        .reconcile_duration
        .with_label_values(&[KIND])
        .start_timer();

    info!(%name, %namespace, "reconciling SourceCredential");

    // Finalizer-gated cleanup: derived Secrets live in other namespaces
    // (argocd repo-creds, apprafter-system dockerconfigjson), so a
    // cross-namespace ownerReference cascade is impossible (k8s forbids
    // it). A finalizer lets this controller GC them on delete instead.
    let finalizers = cred.metadata.finalizers.clone().unwrap_or_default();
    if cred.metadata.deletion_timestamp.is_some() {
        gc_derived_secrets(&ctx.client, &namespace, &name).await?;
        if finalizers.iter().any(|f| f == DERIVED_SECRETS_FINALIZER) {
            set_finalizers(
                &ctx.client,
                &namespace,
                &name,
                without_finalizer(&finalizers),
            )
            .await?;
        }
        info!(%name, %namespace, "SourceCredential derived Secrets garbage-collected");
        return Ok(Action::await_change());
    }
    if !finalizers.iter().any(|f| f == DERIVED_SECRETS_FINALIZER) {
        set_finalizers(&ctx.client, &namespace, &name, with_finalizer(&finalizers)).await?;
        // The patch re-triggers reconcile; derivation proceeds below
        // anyway so the first pass is not wasted.
    }

    // ---- 2.16b-sc Task 7: coverage-narrowing migration pause-gate ----
    // Sits BEFORE both derivation halves (git repo-creds + registry
    // pull-secret), mirroring the app-scope gate. A destructive coverage
    // change (a removed `repoPrefix`/`host`, a dropped half) pauses BOTH
    // halves — the old, wider-coverage derived Secrets are LEFT IN PLACE so
    // in-flight apps keep git-clone / image-pull access — until the operator
    // approves the gating MigrationPlan. On approve/consume the reconcile
    // falls through and derives BOTH halves with the narrowed spec + stamps
    // the new baseline. On re-widen (the user un-does the narrowing) the stale
    // plan is GC'd and derivation proceeds. The machine is a pure function of
    // "did we detect a destructive change vs the stamped baseline?" × the live
    // plan's `PlanState` (see `decide`).
    //
    // A missing baseline (first derive, or a pre-2.16b-sc credential) does NOT
    // gate — detection is skipped and the first baseline is stamped after a
    // successful derive below.
    let old_spec: Option<SourceCredentialSpec> = cred
        .status
        .as_ref()
        .and_then(|s| s.last_applied_spec.clone());
    // `detect_destructive` returns a SINGLE `Option<DestructiveChange>` for the
    // SC scope (not a Vec); wrap it into a 0-or-1-element slice so it feeds the
    // same `create_plan_for(&candidates, ..)` / `plan_state(.., &candidates)`
    // API the app scope uses (both hash / roll up over a slice).
    let change: Option<DestructiveChange> =
        SourceCredentialMigrationStrategy::detect_destructive(old_spec.as_ref(), &cred.spec);
    let candidates: Vec<DestructiveChange> = change.clone().into_iter().collect();

    let plan = find_sc_plan(&ctx.client, &namespace, &name).await?;
    let state = match candidates.first() {
        Some(c) => plan_state(plan.as_ref(), c, &candidates),
        None => plan_state_no_change(plan.as_ref()),
    };

    // On the render arms (`Render` / `ConsumeApply` / `DeleteThenRender`) fall
    // through to the derivation below and stamp the baseline after; the paused
    // arms write a paused status and return HERE (so no derivation SSA runs —
    // the old Secrets stay put). `consume_plan` names the plan to delete AFTER
    // the render+stamp (crash-ordering: derive → stamp → delete) — set only by
    // `ConsumeApply`.
    let mut consume_plan: Option<String> = None;
    match decide(!candidates.is_empty(), state) {
        MigrationDecision::Render => {
            // No change + no plan → derive normally, stamp the (possibly first)
            // baseline below.
        }
        MigrationDecision::CreatePlan => {
            let change = change
                .clone()
                .expect("CreatePlan implies a detected change");
            let sc_uid = sc_uid_or(&cred)?;
            let mp = SourceCredentialMigrationStrategy::create_plan_for(
                &candidates,
                &sc_plan_name(&name, Utc::now()),
                &namespace,
                &name,
                sc_uid,
            );
            let plan_name = mp.name_any();
            info!(
                %name, %namespace, plan = %plan_name, trigger = %change.trigger_type,
                "destructive coverage change detected — creating gating MigrationPlan, pausing both derivation halves"
            );
            ssa_apply_plan(&ctx.client, &namespace, &mp, &name).await?;
            let status = build_paused_status(&cred, &namespace, &plan_name);
            patch_status(&ctx.client, &namespace, &name, &status).await?;
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, &namespace, "paused"])
                .inc();
            // Return BEFORE either derivation half → the wider-coverage Secrets
            // stay in place.
            return Ok(Action::requeue(Duration::from_secs(30)));
        }
        MigrationDecision::NoOp => {
            // Change + a matching blocking plan already gates → stay paused,
            // derive nothing.
            let plan_name = plan.as_ref().map(|p| p.name_any()).unwrap_or_default();
            info!(
                %name, %namespace, plan = %plan_name,
                "coverage change already gated by a matching MigrationPlan — staying paused"
            );
            let status = build_paused_status(&cred, &namespace, &plan_name);
            patch_status(&ctx.client, &namespace, &name, &status).await?;
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, &namespace, "paused"])
                .inc();
            return Ok(Action::requeue(Duration::from_secs(30)));
        }
        MigrationDecision::DeleteThenCreate => {
            // A lingering plan gates a DIFFERENT change (stale gate / relic /
            // approval-hash mismatch) → delete every SC plan, then create the
            // right one and stay paused.
            let change = change
                .clone()
                .expect("DeleteThenCreate implies a detected change");
            let sc_uid = sc_uid_or(&cred)?;
            delete_sc_plans(&ctx.client, &namespace, &name).await?;
            let mp = SourceCredentialMigrationStrategy::create_plan_for(
                &candidates,
                &sc_plan_name(&name, Utc::now()),
                &namespace,
                &name,
                sc_uid,
            );
            let plan_name = mp.name_any();
            info!(
                %name, %namespace, plan = %plan_name, trigger = %change.trigger_type,
                "superseding stale/relic MigrationPlan with a fresh gating plan — staying paused"
            );
            ssa_apply_plan(&ctx.client, &namespace, &mp, &name).await?;
            let status = build_paused_status(&cred, &namespace, &plan_name);
            patch_status(&ctx.client, &namespace, &name, &status).await?;
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, &namespace, "paused"])
                .inc();
            return Ok(Action::requeue(Duration::from_secs(30)));
        }
        MigrationDecision::ConsumeApply => {
            // The change's plan completed (operator approved → the
            // MigrationController drove it to `completed`) → consume: derive
            // BOTH halves with the narrowed spec, stamp the new baseline, THEN
            // delete the plan (crash-order derive → stamp → delete).
            consume_plan = plan.as_ref().and_then(|p| p.metadata.name.clone());
            info!(
                %name, %namespace, plan = ?consume_plan,
                "MigrationPlan completed — consuming: deriving with narrowed coverage"
            );
        }
        MigrationDecision::DeleteThenRender => {
            // No change but a plan lingers (the user re-widened the spec → the
            // destructive delta vanished) → delete the stale plan(s), then
            // derive normally and re-stamp the baseline below.
            info!(
                %name, %namespace,
                "no destructive coverage change but a stale MigrationPlan lingers — cleaning up before derive"
            );
            delete_sc_plans(&ctx.client, &namespace, &name).await?;
        }
        MigrationDecision::BlockFailed => {
            // The change's plan is `failed` → keep gating; surface a
            // `MigrationFailed=True` condition requiring manual delete. Derive
            // nothing (both Secrets stay put).
            let plan_name = plan.as_ref().map(|p| p.name_any()).unwrap_or_default();
            warn!(
                %name, %namespace, plan = %plan_name,
                "gating MigrationPlan is in phase=failed — staying paused, manual delete required"
            );
            let status = build_migration_failed_status(&cred, &plan_name);
            patch_status(&ctx.client, &namespace, &name, &status).await?;
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, &namespace, "paused"])
                .inc();
            return Ok(Action::requeue(Duration::from_secs(30)));
        }
    }

    let previous = cred
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[]);

    let mut conditions: Vec<SourceCredentialCondition> = Vec::new();
    let mut covered_prefixes: Vec<String> = Vec::new();
    let mut covered_hosts: Vec<String> = Vec::new();
    let mut last_validated: Option<String> = None;
    let mut pending = false;
    let pp = PatchParams::apply(FIELD_MANAGER).force();

    // ---- git half → Argo repo-creds Secret(s) in `argocd` ----
    if let Some(git) = &cred.spec.git {
        match &git.backend.sealed_secret_ref {
            None => conditions.push(condition(
                COND_GIT_PRESENT,
                "Unknown",
                REASON_UNVERIFIED,
                "git backend uses openBaoPath; not derivable on Tier 1",
                previous,
            )),
            Some(sealed_ref) => {
                let material_ns = sealed_ref.namespace.clone().unwrap_or(namespace.clone());
                match read_material(&ctx.client, &material_ns, &sealed_ref.name).await? {
                    None => {
                        conditions.push(condition(
                            COND_GIT_PRESENT,
                            "False",
                            "MaterialMissing",
                            &format!(
                                "unsealed material Secret {material_ns}/{} not found yet",
                                sealed_ref.name
                            ),
                            previous,
                        ));
                        pending = true;
                    }
                    Some((username, password)) => {
                        let api: Api<Secret> =
                            Api::namespaced(ctx.client.clone(), ARGOCD_NAMESPACE);
                        for (idx, prefix) in git.repo_prefixes.iter().enumerate() {
                            let url = normalize_repo_url(prefix);
                            let secret_name = repo_cred_secret_name(&name, idx);
                            let payload =
                                repo_cred_payload(&secret_name, &url, &username, &password, &name);
                            api.patch(&secret_name, &pp, &Patch::Apply(&payload))
                                .await?;
                            covered_prefixes.push(prefix.clone());
                        }
                        conditions.push(condition(
                            COND_GIT_PRESENT,
                            "True",
                            "Derived",
                            "Argo repo-creds Secret(s) derived from sealed material",
                            previous,
                        ));
                        // Live reachability probe (S5): find a representative
                        // repo covered by each prefix (from a matching Argo
                        // Application) and probe it over git smart-HTTP.
                        let normalized: Vec<String> = git
                            .repo_prefixes
                            .iter()
                            .map(|p| normalize_repo_url(p))
                            .collect();
                        let (verdict, message) = validity::probe_git_half(
                            &ctx.client,
                            &normalized,
                            &username,
                            &password,
                        )
                        .await;
                        if verdict != Validity::Unverified {
                            last_validated = Some(Utc::now().to_rfc3339());
                        }
                        let (status, reason) = verdict.condition_parts();
                        conditions.push(condition(
                            COND_GIT_VALID,
                            status,
                            reason,
                            &message,
                            previous,
                        ));
                    }
                }
            }
        }
    }

    // ---- registry half → canonical dockerconfigjson in this ns ----
    if let Some(registry) = &cred.spec.registry {
        match &registry.backend.sealed_secret_ref {
            None => conditions.push(condition(
                COND_REGISTRY_PRESENT,
                "Unknown",
                REASON_UNVERIFIED,
                "registry backend uses openBaoPath; not derivable on Tier 1",
                previous,
            )),
            Some(sealed_ref) => {
                let material_ns = sealed_ref.namespace.clone().unwrap_or(namespace.clone());
                match read_material(&ctx.client, &material_ns, &sealed_ref.name).await? {
                    None => {
                        conditions.push(condition(
                            COND_REGISTRY_PRESENT,
                            "False",
                            "MaterialMissing",
                            &format!(
                                "unsealed material Secret {material_ns}/{} not found yet",
                                sealed_ref.name
                            ),
                            previous,
                        ));
                        pending = true;
                    }
                    Some((username, password)) => {
                        let dockercfg = dockerconfigjson(&registry.hosts, &username, &password);
                        let secret_name = pull_secret_name(&name);
                        let payload =
                            dockercfg_payload(&secret_name, &namespace, &dockercfg, &name);
                        let api: Api<Secret> = Api::namespaced(ctx.client.clone(), &namespace);
                        api.patch(&secret_name, &pp, &Patch::Apply(&payload))
                            .await?;
                        covered_hosts.extend(registry.hosts.iter().cloned());
                        conditions.push(condition(
                            COND_REGISTRY_PRESENT,
                            "True",
                            "Derived",
                            "dockerconfigjson pull-secret derived from sealed material",
                            previous,
                        ));
                        // Live reachability probe (S5): find a representative
                        // image covered by each host (from a matching
                        // AppRafter Application) and probe it via a scoped
                        // registry v2 token exchange.
                        let (verdict, message) = validity::probe_registry_half(
                            &ctx.client,
                            &registry.hosts,
                            &username,
                            &password,
                        )
                        .await;
                        if verdict != Validity::Unverified {
                            last_validated = Some(Utc::now().to_rfc3339());
                        }
                        let (status, reason) = verdict.condition_parts();
                        conditions.push(condition(
                            COND_REGISTRY_VALID,
                            status,
                            reason,
                            &message,
                            previous,
                        ));
                    }
                }
            }
        }
    }

    // 2.16b-sc Task 7: stamp the migration baseline. Only reached on a render
    // arm (`Render` / `ConsumeApply` / `DeleteThenRender`) — every paused arm
    // returned early above. Stamp `lastAppliedSpec = spec` (the current spec)
    // ONLY when BOTH applicable halves derived successfully (`!pending`); when a
    // half is still pending (material unsealing) we did NOT actually derive, so
    // CARRY the prior baseline forward instead of stamping the (possibly
    // narrowed) new spec — otherwise a coverage-narrowing edit made while
    // material is still unsealing would move the baseline without the gate ever
    // running its full derivation. Carrying (never `None`) also avoids the
    // SSA-prune self-cancel: the field manager keeps ownership of the field.
    let stamped_baseline = if pending {
        old_spec.clone()
    } else {
        Some(cred.spec.clone())
    };
    patch_status(
        &ctx.client,
        &namespace,
        &name,
        &build_status(
            conditions,
            covered_prefixes,
            covered_hosts,
            last_validated,
            stamped_baseline,
        ),
    )
    .await?;

    // 2.16b-sc Task 7: `ConsumeApply` — delete the completed plan AFTER the
    // derive + baseline stamp landed (crash-order derive → stamp → delete). A
    // crash between the stamp and this delete re-enters as detect=None ×
    // completed-plan → `DeleteThenRender`, which cleans the relic up; so the
    // delete is safe to be the last step. Best-effort (404-tolerant). Skipped
    // when a half is still `pending` — the plan stays until the derive fully
    // lands, so a crashed/partial derive re-consumes on the next pass.
    if !pending {
        if let Some(plan_to_delete) = &consume_plan {
            delete_sc_plans(&ctx.client, &namespace, &name).await?;
            info!(%name, %namespace, plan = %plan_to_delete, "consumed MigrationPlan deleted after derive");
        }
    }

    let outcome = if pending { "pending" } else { "ok" };
    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, &namespace, outcome])
        .inc();
    Ok(Action::requeue(Duration::from_secs(if pending {
        15
    } else {
        60
    })))
}

pub fn error_policy(
    cred: Arc<SourceCredential>,
    err: &ReconcileError,
    ctx: Arc<Context>,
) -> Action {
    let name = cred.name_any();
    let namespace = cred.namespace().unwrap_or_default();
    warn!(%name, %namespace, %err, "sourcecredential reconcile error");
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

/// Delete every derived Secret owned by this SourceCredential across
/// the namespaces it writes to (Argo `repo-creds` in `argocd`, the
/// canonical `dockerconfigjson` in the credential's own namespace),
/// selected by the `apprafter.io/source-credential` label. Per-workload
/// pull-secret copies are owned by the Application controller (Seam A)
/// and cascade with their app, so they are not GC'd here. Idempotent:
/// a missing Secret is not an error.
async fn gc_derived_secrets(
    client: &Client,
    cred_namespace: &str,
    name: &str,
) -> Result<(), ReconcileError> {
    let selector = format!("{SOURCE_CREDENTIAL_LABEL}={name}");
    let mut namespaces = vec![ARGOCD_NAMESPACE];
    if cred_namespace != ARGOCD_NAMESPACE {
        namespaces.push(cred_namespace);
    }
    for ns in namespaces {
        let api: Api<Secret> = Api::namespaced(client.clone(), ns);
        let lp = ListParams::default().labels(&selector);
        for secret in api.list(&lp).await? {
            if let Some(secret_name) = secret.metadata.name {
                // ignore "already gone" so re-runs after a partial
                // delete still converge
                let _ = api.delete(&secret_name, &DeleteParams::default()).await;
            }
        }
    }
    Ok(())
}

/// Merge-patch the SourceCredential's `metadata.finalizers` to `list`.
async fn set_finalizers(
    client: &Client,
    namespace: &str,
    name: &str,
    list: Vec<String>,
) -> Result<(), ReconcileError> {
    let api: Api<SourceCredential> = Api::namespaced(client.clone(), namespace);
    let patch = json!({ "metadata": { "finalizers": list } });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

/// `current` with our finalizer appended (idempotent — caller checks
/// absence first, but kept pure for testing).
fn with_finalizer(current: &[String]) -> Vec<String> {
    let mut out = current.to_vec();
    if !out.iter().any(|f| f == DERIVED_SECRETS_FINALIZER) {
        out.push(DERIVED_SECRETS_FINALIZER.to_string());
    }
    out
}

/// `current` without our finalizer.
fn without_finalizer(current: &[String]) -> Vec<String> {
    current
        .iter()
        .filter(|f| *f != DERIVED_SECRETS_FINALIZER)
        .cloned()
        .collect()
}

/// Read `(username, password)` from the unsealed material Secret.
/// Returns `None` if the Secret does not exist yet.
async fn read_material(
    client: &Client,
    namespace: &str,
    name: &str,
) -> Result<Option<(String, String)>, ReconcileError> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let Some(secret) = api.get_opt(name).await? else {
        return Ok(None);
    };
    let data = secret.data.unwrap_or_default();
    let username = data
        .get(MATERIAL_USERNAME_KEY)
        .map(|b| String::from_utf8_lossy(&b.0).into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_GIT_USERNAME.to_string());
    let password = data
        .get(MATERIAL_PASSWORD_KEY)
        .map(|b| String::from_utf8_lossy(&b.0).into_owned())
        .unwrap_or_default();
    Ok(Some((username, password)))
}

async fn patch_status(
    client: &Client,
    namespace: &str,
    name: &str,
    status: &SourceCredentialStatus,
) -> Result<(), ReconcileError> {
    let api: Api<SourceCredential> = Api::namespaced(client.clone(), namespace);
    let pp = PatchParams::apply(FIELD_MANAGER).force();
    let payload = json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "SourceCredential",
        "metadata": { "name": name },
        "status": status,
    });
    api.patch_status(name, &pp, &Patch::Apply(&payload)).await?;
    Ok(())
}

// ---------------- pure helpers (unit-tested without kube) ----------------

/// Normalise a repo prefix into an Argo-CD `repo-creds` URL: ensure a
/// scheme (`https://` by default) and strip the trailing slash so the
/// prefix match against `Application.spec.source.repoURL` is clean.
fn normalize_repo_url(prefix: &str) -> String {
    let with_scheme = if prefix.contains("://") {
        prefix.to_string()
    } else {
        format!("https://{prefix}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

/// Deterministic name for the derived `repo-creds` Secret of one prefix.
fn repo_cred_secret_name(cred_name: &str, idx: usize) -> String {
    format!("srccred-{cred_name}-repo-{idx}")
}

/// SSA payload for a derived Argo CD `repo-creds` Secret.
fn repo_cred_payload(
    secret_name: &str,
    url: &str,
    username: &str,
    password: &str,
    cred_name: &str,
) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": secret_name,
            "namespace": ARGOCD_NAMESPACE,
            "labels": {
                "argocd.argoproj.io/secret-type": "repo-creds",
                "apprafter.io/managed-by": "apprafter",
                "apprafter.io/source-credential": cred_name,
            }
        },
        "type": "Opaque",
        "stringData": {
            "url": url,
            "username": username,
            "password": password,
        }
    })
}

/// Registry hostname (the `dockerconfigjson` auths key) from a host
/// prefix: the first path segment. `"ghcr.io/myorg/"` → `"ghcr.io"`.
fn registry_hostname(host_prefix: &str) -> String {
    host_prefix
        .split('/')
        .next()
        .unwrap_or(host_prefix)
        .to_string()
}

/// Deterministic name for the canonical derived pull-secret (lives in
/// the SourceCredential's namespace). The Application controller reads
/// it by this name to project per-workload copies (Seam A).
pub fn pull_secret_name(cred_name: &str) -> String {
    format!("srccred-{cred_name}-dockercfg")
}

/// Build a `.dockerconfigjson` body: one `auths` entry per distinct
/// registry hostname covered by `hosts`, all using the same material.
fn dockerconfigjson(hosts: &[String], username: &str, password: &str) -> String {
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    let mut auths = serde_json::Map::new();
    for host in hosts {
        let hostname = registry_hostname(host);
        auths
            .entry(hostname)
            .or_insert_with(|| json!({ "username": username, "password": password, "auth": auth }));
    }
    serde_json::to_string(&json!({ "auths": auths })).expect("serialise dockerconfigjson")
}

/// SSA payload for a `kubernetes.io/dockerconfigjson` Secret.
fn dockercfg_payload(
    secret_name: &str,
    namespace: &str,
    dockercfg: &str,
    cred_name: &str,
) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": secret_name,
            "namespace": namespace,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
                "apprafter.io/source-credential": cred_name,
            }
        },
        "type": "kubernetes.io/dockerconfigjson",
        "stringData": { ".dockerconfigjson": dockercfg }
    })
}

fn build_status(
    conditions: Vec<SourceCredentialCondition>,
    covered_prefixes: Vec<String>,
    covered_hosts: Vec<String>,
    last_validated: Option<String>,
    last_applied_spec: Option<SourceCredentialSpec>,
) -> SourceCredentialStatus {
    SourceCredentialStatus {
        conditions: Some(conditions),
        covered_repo_prefixes: if covered_prefixes.is_empty() {
            None
        } else {
            Some(covered_prefixes)
        },
        covered_hosts: if covered_hosts.is_empty() {
            None
        } else {
            Some(covered_hosts)
        },
        last_validated,
        // 2.16b-sc Task 7: the migration baseline. `patch_status` is a
        // server-side apply under a SINGLE field manager (`FIELD_MANAGER`,
        // `.force()`), so OMITTING a field the manager previously owned makes
        // the apiserver PRUNE it — NOT "leave it untouched". The pause arms
        // stamp `lastAppliedSpec` via `build_paused_status`; if this render-path
        // writer emitted `None`, the next reconcile would read no baseline, skip
        // destructive detection, and the gate would self-cancel (the same
        // SSA-prune bug the app scope hit). So the caller threads the baseline
        // through: `Some(cred.spec)` on a successful derive (stamp), or the
        // carried-forward existing baseline when a half is still pending.
        last_applied_spec,
        // 2.16b-sc Task 7: reaching the render path means the credential is NOT
        // paused → clear the `AwaitingMigrationApproval` phase (a prior pause
        // may have set it; omitting it prunes it, which is what we want here).
        phase: None,
    }
}

// ---------------- 2.16b-sc Task 7: migration pause-gate helpers ----------------

/// The SourceCredential's `metadata.uid`, required for the gating
/// MigrationPlan's controller ownerRef (cascade on SC delete). The apiserver
/// always assigns one — a missing uid is a defensive error, never surfaced in
/// practice. Mirrors the app-scope `app_uid_or`.
fn sc_uid_or(cred: &SourceCredential) -> Result<&str, ReconcileError> {
    cred.metadata
        .uid
        .as_deref()
        .ok_or_else(|| ReconcileError::MissingUid(cred.name_any()))
}

/// Deterministic, DNS-1123-safe MigrationPlan name for a SourceCredential.
/// SourceCredentials have no environment (unlike Applications), so the name is
/// `<sc-name>-migration-<unix-secs>`. The timestamp gives each superseding plan
/// a fresh name so a delete-then-create never collides with the object it just
/// deleted (which may still be terminating). Folded per-char to a valid
/// `metadata.name`. Mirrors the app-scope `plan_name` (minus the env segment).
fn sc_plan_name(sc_name: &str, now: DateTime<Utc>) -> String {
    let raw = format!("{sc_name}-migration-{}", now.timestamp());
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

/// Pure: pick the (≤1) SC-scope MigrationPlan for `sc_name` from a list. Matches
/// on the `sourcecredential` scope discriminator + the plan's
/// `scope.sourcecredential.ref.name`, ignoring plans for other credentials (or
/// other scopes). Mirrors the app-scope `pick_any_key_plan`; unit-testable
/// without a client. Returns plans of ANY phase (blocking / completed / relic)
/// so the state machine can bucket them for `ConsumeApply` / cleanup.
fn pick_sc_plan(plans: Vec<MigrationPlan>, sc_name: &str) -> Option<MigrationPlan> {
    plans.into_iter().find(|plan| {
        plan.spec.scope.type_ == SCOPE_SOURCECREDENTIAL
            && plan
                .spec
                .scope
                .sourcecredential
                .as_ref()
                .is_some_and(|s| s.ref_.name == sc_name)
    })
}

/// Find the (≤1) SC-scope MigrationPlan for this credential in its own
/// namespace (where `create_plan_for` lands it). Best-effort list narrowed by
/// the `apprafter.io/scope=sourcecredential` + `apprafter.io/source-credential`
/// labels the plan carries, then `pick_sc_plan` applies the exact scope match.
/// A list error propagates (the reconcile retries).
async fn find_sc_plan(
    client: &Client,
    sc_namespace: &str,
    sc_name: &str,
) -> Result<Option<MigrationPlan>, ReconcileError> {
    let api: Api<MigrationPlan> = Api::namespaced(client.clone(), sc_namespace);
    let selector =
        format!("{SCOPE_LABEL}={SCOPE_SOURCECREDENTIAL},{SOURCE_CREDENTIAL_LABEL}={sc_name}");
    let lp = ListParams::default().labels(&selector);
    let list = api.list(&lp).await?;
    Ok(pick_sc_plan(list.items, sc_name))
}

/// SSA-apply a freshly-built SC-scope MigrationPlan into the credential's own
/// namespace under field manager [`FIELD_MANAGER`] (`apprafter-sourcecredential`
/// — the SC controller owns the plan, matching the SSA split). The plan already
/// carries its `metadata.name`/`namespace` + controller-ownerRef (from
/// `create_plan_for`); this serializes it, injects the `apiVersion`/`kind` SSA
/// requires, and applies. `_sc_name` documents the owning credential.
async fn ssa_apply_plan(
    client: &Client,
    sc_namespace: &str,
    plan: &MigrationPlan,
    _sc_name: &str,
) -> Result<(), ReconcileError> {
    let plan_name = plan.metadata.name.clone().unwrap_or_default();
    let mut payload = serde_json::to_value(plan)?;
    if let Value::Object(map) = &mut payload {
        map.insert(
            "apiVersion".to_string(),
            Value::String("apprafter.io/v1alpha1".to_string()),
        );
        map.insert(
            "kind".to_string(),
            Value::String("MigrationPlan".to_string()),
        );
    }
    let api: Api<MigrationPlan> = Api::namespaced(client.clone(), sc_namespace);
    let pp = PatchParams::apply(FIELD_MANAGER).force();
    api.patch(&plan_name, &pp, &Patch::Apply(&payload)).await?;
    Ok(())
}

/// Delete every SC-scope MigrationPlan for this credential in its namespace
/// (the supersede / consume / cleanup delete — the SC scope enforces "≤1 live
/// plan per credential"). Best-effort: a 404 is tolerated (the plan already
/// cascaded / a concurrent reconcile removed it); a non-404 delete error
/// propagates so a genuine RBAC / apiserver fault surfaces rather than silently
/// leaving a stale gate.
async fn delete_sc_plans(
    client: &Client,
    sc_namespace: &str,
    sc_name: &str,
) -> Result<(), ReconcileError> {
    let api: Api<MigrationPlan> = Api::namespaced(client.clone(), sc_namespace);
    let selector =
        format!("{SCOPE_LABEL}={SCOPE_SOURCECREDENTIAL},{SOURCE_CREDENTIAL_LABEL}={sc_name}");
    let lp = ListParams::default().labels(&selector);
    for plan in api.list(&lp).await?.items {
        // Belt-and-braces exact scope match — a stray label shouldn't delete a
        // different credential's plan.
        if plan
            .spec
            .scope
            .sourcecredential
            .as_ref()
            .is_none_or(|s| s.ref_.name != sc_name)
        {
            continue;
        }
        let Some(plan_name) = plan.metadata.name.as_deref() else {
            continue;
        };
        match api.delete(plan_name, &DeleteParams::default()).await {
            Ok(_) => {
                info!(%sc_name, plan = %plan_name, "deleted superseded/consumed MigrationPlan")
            }
            Err(kube::Error::Api(e)) if e.code == 404 => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Build the paused status for the `CreatePlan` / `NoOp` / `DeleteThenCreate`
/// arms: `phase = AwaitingMigrationApproval` + a `Ready=False/MigrationPending`
/// condition and a `MigrationPending=True` condition naming the gating plan.
/// PRESERVES the covered lists, `lastValidated`, and the migration BASELINE
/// (`lastAppliedSpec`) from the prior status — the pause is skip-derive, the
/// old wider-coverage Secrets stay in place, so their coverage lists and the
/// baseline they derived from remain accurate. Carrying the baseline forward is
/// LOAD-BEARING under SSA: omitting it would prune it and self-cancel the gate
/// (the walk-found app-scope bug), so the pause would evaporate in one requeue.
fn build_paused_status(
    cred: &SourceCredential,
    plan_ns: &str,
    plan_name: &str,
) -> SourceCredentialStatus {
    let previous = cred
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[]);
    let ready = condition(
        "Ready",
        "False",
        "MigrationPending",
        &format!("paused awaiting approval of MigrationPlan {plan_ns}/{plan_name}"),
        previous,
    );
    let pending = condition(
        COND_MIGRATION_PENDING,
        "True",
        "MigrationPending",
        &format!("coverage-narrowing gated by MigrationPlan {plan_ns}/{plan_name}"),
        previous,
    );
    carry_forward_status(
        cred,
        vec![ready, pending],
        Some(PHASE_AWAITING_MIGRATION_APPROVAL),
    )
}

/// Build the paused status for the `BlockFailed` arm — the gating plan is in
/// phase `failed` and needs manual resolution. Mirrors [`build_paused_status`]
/// but emits `Ready=False/MigrationFailed` + a `MigrationFailed=True` condition
/// so consumers distinguish "awaiting approval" from "failed, manual delete
/// required". Preserves the covered lists + baseline the same way.
fn build_migration_failed_status(
    cred: &SourceCredential,
    plan_name: &str,
) -> SourceCredentialStatus {
    let previous = cred
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[]);
    let ready = condition(
        "Ready",
        "False",
        "MigrationFailed",
        &format!("paused: MigrationPlan {plan_name} failed — manual delete required"),
        previous,
    );
    let failed = condition(
        "MigrationFailed",
        "True",
        "MigrationPlanFailed",
        &format!("MigrationPlan {plan_name} failed — manual delete required to unblock derivation"),
        previous,
    );
    carry_forward_status(
        cred,
        vec![ready, failed],
        Some(PHASE_AWAITING_MIGRATION_APPROVAL),
    )
}

/// Shared paused-status builder: carry the prior `coveredRepoPrefixes` /
/// `coveredHosts` / `lastValidated` / `lastAppliedSpec` forward UNCHANGED (the
/// pause left the derived Secrets in place, so none of those recompute) while
/// swapping in the paused `conditions` + `phase`. Carrying every un-recomputed
/// field is the SSA-prune guard: `patch_status` writes under a single forced
/// field manager, so any field this payload OMITS is pruned — including the
/// migration baseline, whose loss self-cancels the gate.
fn carry_forward_status(
    cred: &SourceCredential,
    conditions: Vec<SourceCredentialCondition>,
    phase: Option<&str>,
) -> SourceCredentialStatus {
    let prior = cred.status.as_ref();
    SourceCredentialStatus {
        conditions: Some(conditions),
        covered_repo_prefixes: prior.and_then(|s| s.covered_repo_prefixes.clone()),
        covered_hosts: prior.and_then(|s| s.covered_hosts.clone()),
        last_validated: prior.and_then(|s| s.last_validated.clone()),
        last_applied_spec: prior.and_then(|s| s.last_applied_spec.clone()),
        phase: phase.map(str::to_string),
    }
}

/// Build a condition, preserving `lastTransitionTime` when the
/// `(type, status)` pair is unchanged — the same hot-loop guard the
/// Application controller uses (identical status ⇒ no-op SSA ⇒ no
/// self-triggered re-reconcile).
fn condition(
    type_: &str,
    status: &str,
    reason: &str,
    message: &str,
    previous: &[SourceCredentialCondition],
) -> SourceCredentialCondition {
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == type_ && c.status == status)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    SourceCredentialCondition {
        type_: type_.to_string(),
        status: status.to_string(),
        last_transition_time,
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_repo_url_adds_https_and_strips_trailing_slash() {
        assert_eq!(
            normalize_repo_url("github.com/acme/"),
            "https://github.com/acme"
        );
        assert_eq!(
            normalize_repo_url("github.com/acme"),
            "https://github.com/acme"
        );
    }

    #[test]
    fn normalize_repo_url_keeps_existing_scheme() {
        assert_eq!(
            normalize_repo_url("https://github.com/acme/"),
            "https://github.com/acme"
        );
        assert_eq!(
            normalize_repo_url("ssh://git@github.com/acme"),
            "ssh://git@github.com/acme"
        );
    }

    #[test]
    fn repo_cred_secret_name_is_deterministic_per_prefix() {
        assert_eq!(repo_cred_secret_name("acme", 0), "srccred-acme-repo-0");
        assert_eq!(repo_cred_secret_name("acme", 1), "srccred-acme-repo-1");
    }

    #[test]
    fn repo_cred_payload_is_an_argo_repo_creds_secret() {
        let p = repo_cred_payload(
            "srccred-acme-repo-0",
            "https://github.com/acme",
            "git",
            "ghp_x",
            "acme",
        );
        assert_eq!(p["kind"], "Secret");
        assert_eq!(p["metadata"]["namespace"], "argocd");
        assert_eq!(
            p["metadata"]["labels"]["argocd.argoproj.io/secret-type"],
            "repo-creds"
        );
        assert_eq!(
            p["metadata"]["labels"]["apprafter.io/source-credential"],
            "acme"
        );
        assert_eq!(p["type"], "Opaque");
        assert_eq!(p["stringData"]["url"], "https://github.com/acme");
        assert_eq!(p["stringData"]["username"], "git");
        assert_eq!(p["stringData"]["password"], "ghp_x");
    }

    #[test]
    fn condition_reuses_timestamp_when_status_unchanged() {
        let prev = vec![SourceCredentialCondition {
            type_: COND_GIT_PRESENT.to_string(),
            status: "True".to_string(),
            last_transition_time: "2026-01-01T00:00:00+00:00".to_string(),
            reason: Some("Derived".to_string()),
            message: Some("x".to_string()),
        }];
        let c = condition(COND_GIT_PRESENT, "True", "Derived", "y", &prev);
        assert_eq!(c.last_transition_time, "2026-01-01T00:00:00+00:00");
    }

    #[test]
    fn condition_bumps_timestamp_when_status_changes() {
        let prev = vec![SourceCredentialCondition {
            type_: COND_GIT_PRESENT.to_string(),
            status: "False".to_string(),
            last_transition_time: "2026-01-01T00:00:00+00:00".to_string(),
            reason: Some("MaterialMissing".to_string()),
            message: Some("x".to_string()),
        }];
        let c = condition(COND_GIT_PRESENT, "True", "Derived", "y", &prev);
        assert_ne!(c.last_transition_time, "2026-01-01T00:00:00+00:00");
    }

    #[test]
    fn build_status_omits_empty_covered_lists() {
        let s = build_status(vec![], vec![], vec![], None, None);
        assert!(s.covered_repo_prefixes.is_none());
        assert!(s.covered_hosts.is_none());
        assert!(s.last_validated.is_none());
        assert!(s.last_applied_spec.is_none());
        assert!(s.phase.is_none());
        let s = build_status(
            vec![],
            vec!["github.com/acme/".to_string()],
            vec!["ghcr.io/acme/".to_string()],
            Some("2026-05-31T00:00:00+00:00".to_string()),
            None,
        );
        assert_eq!(s.covered_repo_prefixes.unwrap(), vec!["github.com/acme/"]);
        assert_eq!(s.covered_hosts.unwrap(), vec!["ghcr.io/acme/"]);
        assert!(s.last_validated.is_some());
    }

    // 2.16b-sc Task 7: the render-path baseline stamp threads the current spec
    // into `build_status` as `lastAppliedSpec` and clears the paused phase.
    #[test]
    fn build_status_stamps_baseline_and_clears_phase() {
        let spec = SourceCredentialSpec {
            git: None,
            registry: None,
        };
        let s = build_status(vec![], vec![], vec![], None, Some(spec.clone()));
        assert_eq!(s.last_applied_spec.as_ref(), Some(&spec));
        // Reaching the render path clears any prior AwaitingMigrationApproval.
        assert!(s.phase.is_none());
    }

    #[test]
    fn finalizer_add_is_idempotent_and_preserves_others() {
        let none: Vec<String> = vec![];
        assert_eq!(
            with_finalizer(&none),
            vec![DERIVED_SECRETS_FINALIZER.to_string()]
        );
        // already present → unchanged (no duplicate)
        let present = vec![DERIVED_SECRETS_FINALIZER.to_string()];
        assert_eq!(with_finalizer(&present), present);
        // foreign finalizers are preserved
        let mixed = vec!["other.io/keep".to_string()];
        let added = with_finalizer(&mixed);
        assert_eq!(
            added,
            vec![
                "other.io/keep".to_string(),
                DERIVED_SECRETS_FINALIZER.to_string()
            ]
        );
    }

    #[test]
    fn finalizer_remove_drops_only_ours() {
        let list = vec![
            "other.io/keep".to_string(),
            DERIVED_SECRETS_FINALIZER.to_string(),
        ];
        assert_eq!(without_finalizer(&list), vec!["other.io/keep".to_string()]);
        let only_ours = vec![DERIVED_SECRETS_FINALIZER.to_string()];
        assert!(without_finalizer(&only_ours).is_empty());
    }

    #[test]
    fn registry_hostname_takes_the_first_path_segment() {
        assert_eq!(registry_hostname("ghcr.io/myorg/"), "ghcr.io");
        assert_eq!(registry_hostname("ghcr.io/myorg"), "ghcr.io");
        assert_eq!(registry_hostname("ghcr.io"), "ghcr.io");
        assert_eq!(
            registry_hostname("registry.gitlab.com/grp/proj"),
            "registry.gitlab.com"
        );
    }

    #[test]
    fn pull_secret_name_is_deterministic() {
        assert_eq!(pull_secret_name("acme"), "srccred-acme-dockercfg");
    }

    #[test]
    fn dockerconfigjson_dedups_hosts_and_encodes_auth() {
        let body = dockerconfigjson(
            &["ghcr.io/org1/".to_string(), "ghcr.io/org2/".to_string()],
            "git",
            "ghp_x",
        );
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        // Both prefixes collapse to the single ghcr.io host.
        assert_eq!(v["auths"].as_object().unwrap().len(), 1);
        assert_eq!(v["auths"]["ghcr.io"]["username"], "git");
        assert_eq!(v["auths"]["ghcr.io"]["password"], "ghp_x");
        let expected_auth = base64::engine::general_purpose::STANDARD.encode("git:ghp_x");
        assert_eq!(v["auths"]["ghcr.io"]["auth"], expected_auth);
    }

    #[test]
    fn dockercfg_payload_is_a_dockerconfigjson_secret() {
        let p = dockercfg_payload(
            "srccred-acme-dockercfg",
            "apprafter-system",
            "{\"auths\":{}}",
            "acme",
        );
        assert_eq!(p["type"], "kubernetes.io/dockerconfigjson");
        assert_eq!(p["metadata"]["namespace"], "apprafter-system");
        assert_eq!(
            p["metadata"]["labels"]["apprafter.io/source-credential"],
            "acme"
        );
        assert_eq!(p["stringData"][".dockerconfigjson"], "{\"auths\":{}}");
    }

    // ---------------- 2.16b-sc Task 7: migration pause-gate tests ----------------

    use operator_core::migration::change_hash;
    use operator_core::{
        MigrationPlanScope, MigrationPlanSpec, MigrationPlanStatus, MigrationSourceCredentialRef,
        MigrationSourceCredentialScope, MigrationTrigger, SealedSecretRef, SourceBackend,
        SourceGit, SourceRegistry,
    };

    fn sc_backend() -> SourceBackend {
        SourceBackend {
            sealed_secret_ref: Some(SealedSecretRef {
                name: "srccred-acme-material".to_string(),
                namespace: None,
            }),
            open_bao_path: None,
        }
    }

    /// A SourceCredentialSpec with the given repo prefixes + registry hosts
    /// (empty slice → that half is absent).
    fn sc_spec(prefixes: &[&str], hosts: &[&str]) -> SourceCredentialSpec {
        SourceCredentialSpec {
            git: if prefixes.is_empty() {
                None
            } else {
                Some(SourceGit {
                    backend: sc_backend(),
                    repo_prefixes: prefixes.iter().map(|s| s.to_string()).collect(),
                })
            },
            registry: if hosts.is_empty() {
                None
            } else {
                Some(SourceRegistry {
                    backend: sc_backend(),
                    hosts: hosts.iter().map(|s| s.to_string()).collect(),
                })
            },
        }
    }

    /// An SC-scope MigrationPlan for `sc_name` with the given trigger tuple,
    /// phase, and optional approval hash — mirrors `create_plan_for`'s scope +
    /// label shape enough for the finder/state-machine tests.
    fn sc_plan(
        sc_name: &str,
        trigger_type: &str,
        field: &str,
        phase: Option<&str>,
        approved_hash: Option<String>,
    ) -> MigrationPlan {
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: "sourcecredential".into(),
                application: None,
                platform: None,
                sourcecredential: Some(MigrationSourceCredentialScope {
                    ref_: MigrationSourceCredentialRef {
                        name: sc_name.into(),
                        namespace: "apprafter-system".into(),
                    },
                }),
            },
            trigger: MigrationTrigger {
                type_: trigger_type.into(),
                field: field.into(),
                from: None,
                to: None,
                approved_spec_hash: approved_hash,
            },
            risks: None,
            changes: None,
            plan: None,
            approvers: None,
            previous_spec_snapshot: None,
        };
        let mut mp = MigrationPlan::new(&format!("{sc_name}-migration-1"), spec);
        mp.metadata.namespace = Some("apprafter-system".into());
        if let Some(p) = phase {
            mp.status = Some(MigrationPlanStatus {
                phase: Some(p.into()),
                ..MigrationPlanStatus::default()
            });
        }
        mp
    }

    #[test]
    fn sc_plan_name_is_deterministic_dns_safe_and_env_free() {
        let now = DateTime::parse_from_rfc3339("2026-07-17T00:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            sc_plan_name("gh-creds", now),
            "gh-creds-migration-1784246400"
        );
        // Same input → same name (deterministic given the timestamp).
        assert_eq!(sc_plan_name("gh-creds", now), sc_plan_name("gh-creds", now));
        // Non-DNS chars fold to '-', trailing '-' trimmed.
        let folded = sc_plan_name("Team_A.Creds", now);
        assert!(folded
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
        assert!(!folded.ends_with('-'));
        assert!(folded.starts_with("team-a-creds-migration-"));
    }

    #[test]
    fn pick_sc_plan_selects_this_creds_plan_and_ignores_others() {
        let mine = sc_plan(
            "gh-creds",
            "coverage-removal",
            "spec.git.repoPrefixes",
            None,
            None,
        );
        let other_cred = sc_plan(
            "dh-creds",
            "coverage-removal",
            "spec.registry.hosts",
            None,
            None,
        );
        // An application-scope plan that (impossibly) shares the label must be
        // rejected by the scope-type guard.
        let mut app_scope = sc_plan(
            "gh-creds",
            "coverage-removal",
            "spec.git.repoPrefixes",
            None,
            None,
        );
        app_scope.spec.scope.type_ = "application".into();
        app_scope.spec.scope.sourcecredential = None;

        let plans = vec![other_cred.clone(), app_scope, mine.clone()];
        let picked = pick_sc_plan(plans, "gh-creds").expect("gh-creds plan found");
        assert_eq!(
            picked.spec.scope.sourcecredential.unwrap().ref_.name,
            "gh-creds"
        );
        // No plan for a credential with none.
        assert!(pick_sc_plan(vec![other_cred], "gh-creds").is_none());
        assert!(pick_sc_plan(vec![], "gh-creds").is_none());
    }

    #[test]
    fn sc_uid_or_returns_uid_or_errors() {
        let mut cred = SourceCredential::new("acme", sc_spec(&["github.com/acme/"], &[]));
        cred.metadata.uid = Some("uid-1".into());
        assert_eq!(sc_uid_or(&cred).unwrap(), "uid-1");
        cred.metadata.uid = None;
        assert!(matches!(
            sc_uid_or(&cred),
            Err(ReconcileError::MissingUid(_))
        ));
    }

    // The gate DECISION over a mocked (old, new, plan), reusing the hoisted
    // `detect_destructive` → `plan_state`/`plan_state_no_change` → `decide`
    // exactly as the reconcile does. This exercises the SC-specific wiring
    // without a cluster.
    fn gate_decision(
        old: Option<&SourceCredentialSpec>,
        new: &SourceCredentialSpec,
        plan: Option<&MigrationPlan>,
    ) -> MigrationDecision {
        let change = SourceCredentialMigrationStrategy::detect_destructive(old, new);
        let candidates: Vec<DestructiveChange> = change.into_iter().collect();
        let state = match candidates.first() {
            Some(c) => plan_state(plan, c, &candidates),
            None => plan_state_no_change(plan),
        };
        decide(!candidates.is_empty(), state)
    }

    #[test]
    fn gate_first_derive_no_baseline_renders_without_gating() {
        // old = None (first reconcile / pre-2.16b-sc credential): no detection,
        // no plan → Render (derive + stamp the first baseline).
        let new = sc_spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        assert_eq!(gate_decision(None, &new, None), MigrationDecision::Render);
    }

    #[test]
    fn gate_coverage_removal_with_no_plan_creates_plan() {
        // A repoPrefix removed vs the baseline + no plan yet → CreatePlan (pause
        // BOTH halves).
        let old = sc_spec(
            &["github.com/acme/", "github.com/acme-labs/"],
            &["ghcr.io/acme/"],
        );
        let new = sc_spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        assert_eq!(
            gate_decision(Some(&old), &new, None),
            MigrationDecision::CreatePlan
        );
    }

    #[test]
    fn gate_coverage_removal_with_matching_blocking_plan_is_noop() {
        // Same removal, a matching pending plan already gates → NoOp (stay
        // paused, derive nothing).
        let old = sc_spec(
            &["github.com/acme/", "github.com/acme-labs/"],
            &["ghcr.io/acme/"],
        );
        let new = sc_spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        let plan = sc_plan(
            "acme",
            "coverage-removal",
            "spec.git.repoPrefixes",
            Some("pending-approval"),
            None,
        );
        assert_eq!(
            gate_decision(Some(&old), &new, Some(&plan)),
            MigrationDecision::NoOp
        );
    }

    #[test]
    fn gate_coverage_removal_with_completed_matching_plan_consumes() {
        // The operator approved → the MigrationController drove the plan to
        // `completed`. Its stamped approval hash covers the current candidate
        // set → ConsumeApply (derive with the narrowed coverage + stamp).
        let old = sc_spec(
            &["github.com/acme/", "github.com/acme-labs/"],
            &["ghcr.io/acme/"],
        );
        let new = sc_spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        let change =
            SourceCredentialMigrationStrategy::detect_destructive(Some(&old), &new).unwrap();
        let candidate_set = std::slice::from_ref(&change);
        let plan = sc_plan(
            "acme",
            &change.trigger_type,
            &change.field,
            Some("completed"),
            Some(change_hash(candidate_set)),
        );
        assert_eq!(
            gate_decision(Some(&old), &new, Some(&plan)),
            MigrationDecision::ConsumeApply
        );
    }

    #[test]
    fn gate_completed_plan_with_wrong_hash_re_gates_not_consumes() {
        // S-4: a completed, tuple-matching plan whose stamped hash is for a
        // DIFFERENT change must NOT transfer → DeleteThenCreate (re-gate), never
        // ConsumeApply.
        let old = sc_spec(
            &["github.com/acme/", "github.com/acme-labs/"],
            &["ghcr.io/acme/"],
        );
        let new = sc_spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        let change =
            SourceCredentialMigrationStrategy::detect_destructive(Some(&old), &new).unwrap();
        // A hash over a different candidate (removing a host instead) — same
        // trigger family but different content.
        let other = DestructiveChange {
            trigger_type: change.trigger_type.clone(),
            field: change.field.clone(),
            from: Some(json!({ "removedRepoPrefixes": ["github.com/x/"] })),
            to: None,
            classification: "breaking".into(),
        };
        let plan = sc_plan(
            "acme",
            &change.trigger_type,
            &change.field,
            Some("completed"),
            Some(change_hash(std::slice::from_ref(&other))),
        );
        assert_eq!(
            gate_decision(Some(&old), &new, Some(&plan)),
            MigrationDecision::DeleteThenCreate
        );
    }

    #[test]
    fn gate_widen_back_with_stale_plan_deletes_then_renders() {
        // The user re-widened: the baseline is the NARROW spec (what was
        // derived), and the new spec re-adds the prefix → no destructive delta
        // (widening is not destructive). A stale plan from the earlier narrowing
        // lingers → DeleteThenRender (GC the plan, derive, re-stamp).
        let baseline = sc_spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        let widened = sc_spec(
            &["github.com/acme/", "github.com/acme-labs/"],
            &["ghcr.io/acme/"],
        );
        // sanity: widening is not destructive
        assert!(
            SourceCredentialMigrationStrategy::detect_destructive(Some(&baseline), &widened)
                .is_none()
        );
        let stale = sc_plan(
            "acme",
            "coverage-removal",
            "spec.git.repoPrefixes",
            Some("completed"),
            None,
        );
        assert_eq!(
            gate_decision(Some(&baseline), &widened, Some(&stale)),
            MigrationDecision::DeleteThenRender
        );
        // …and with no lingering plan, a widen just renders.
        assert_eq!(
            gate_decision(Some(&baseline), &widened, None),
            MigrationDecision::Render
        );
    }

    #[test]
    fn gate_coverage_removal_with_failed_plan_blocks() {
        // The gating plan is in phase=failed → BlockFailed (stay paused, manual
        // delete required).
        let old = sc_spec(
            &["github.com/acme/", "github.com/acme-labs/"],
            &["ghcr.io/acme/"],
        );
        let new = sc_spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        let plan = sc_plan(
            "acme",
            "coverage-removal",
            "spec.git.repoPrefixes",
            Some("failed"),
            None,
        );
        assert_eq!(
            gate_decision(Some(&old), &new, Some(&plan)),
            MigrationDecision::BlockFailed
        );
    }

    #[test]
    fn build_paused_status_gates_and_preserves_baseline_and_covered_lists() {
        // A prior status carrying a baseline + covered lists must survive the
        // pause write (SSA-prune guard): the pause skips derivation, so nothing
        // recomputes and everything is carried forward.
        let baseline = sc_spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        let mut cred = SourceCredential::new("acme", baseline.clone());
        cred.metadata.namespace = Some("apprafter-system".into());
        cred.status = Some(SourceCredentialStatus {
            covered_repo_prefixes: Some(vec!["github.com/acme/".into()]),
            covered_hosts: Some(vec!["ghcr.io/acme/".into()]),
            last_validated: Some("2026-07-01T00:00:00+00:00".into()),
            last_applied_spec: Some(baseline.clone()),
            ..SourceCredentialStatus::default()
        });
        let s = build_paused_status(&cred, "apprafter-system", "acme-migration-1");
        // Phase flipped, conditions present.
        assert_eq!(s.phase.as_deref(), Some(PHASE_AWAITING_MIGRATION_APPROVAL));
        let conds = s.conditions.unwrap();
        assert!(conds
            .iter()
            .any(|c| c.type_ == "Ready" && c.status == "False"));
        assert!(conds
            .iter()
            .any(|c| c.type_ == COND_MIGRATION_PENDING && c.status == "True"));
        // Baseline + covered lists carried forward UNCHANGED (not pruned).
        assert_eq!(s.last_applied_spec.as_ref(), Some(&baseline));
        assert_eq!(s.covered_repo_prefixes.unwrap(), vec!["github.com/acme/"]);
        assert_eq!(s.covered_hosts.unwrap(), vec!["ghcr.io/acme/"]);
        assert!(s.last_validated.is_some());
    }

    #[test]
    fn build_migration_failed_status_surfaces_failed_condition_and_carries_baseline() {
        let baseline = sc_spec(&["github.com/acme/"], &[]);
        let mut cred = SourceCredential::new("acme", baseline.clone());
        cred.status = Some(SourceCredentialStatus {
            last_applied_spec: Some(baseline.clone()),
            ..SourceCredentialStatus::default()
        });
        let s = build_migration_failed_status(&cred, "acme-migration-1");
        assert_eq!(s.phase.as_deref(), Some(PHASE_AWAITING_MIGRATION_APPROVAL));
        let conds = s.conditions.unwrap();
        assert!(conds
            .iter()
            .any(|c| c.type_ == "MigrationFailed" && c.status == "True"));
        assert!(conds
            .iter()
            .any(|c| c.type_ == "Ready" && c.status == "False"));
        // The failed gate does NOT re-stamp — the credential is still on the
        // prior baseline; carry it forward (SSA-prune guard).
        assert_eq!(s.last_applied_spec.as_ref(), Some(&baseline));
    }
}
