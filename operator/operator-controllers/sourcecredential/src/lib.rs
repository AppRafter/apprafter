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
    DestructiveChange, Metrics, MigrationPlan, SourceBackend, SourceCredential,
    SourceCredentialCondition, SourceCredentialSpec, SourceCredentialStatus, SourceGit,
    SourceRegistry, COND_GIT_PRESENT, COND_GIT_VALID, COND_MIGRATION_PENDING,
    COND_REGISTRY_PRESENT, COND_REGISTRY_VALID, PHASE_AWAITING_MIGRATION_APPROVAL,
    REASON_UNVERIFIED,
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
    let deleting = cred.metadata.deletion_timestamp.is_some();
    if deleting {
        // GC FIRST — the finalizer is released only after the derived
        // Secrets are actually gone (a failed sweep returns Err here and
        // the finalizer keeps the object alive for the retry).
        gc_derived_secrets(&ctx.client, &namespace, &name).await?;
    }
    if let Some(list) = finalizer_patch(deleting, &finalizers) {
        // On a live object the patch re-triggers reconcile; derivation
        // proceeds below anyway so the first pass is not wasted.
        set_finalizers(&ctx.client, &namespace, &name, list).await?;
    }
    if deleting {
        info!(%name, %namespace, "SourceCredential derived Secrets garbage-collected");
        return Ok(Action::await_change());
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
    match gate_action(decide(!candidates.is_empty(), state), plan.as_ref()) {
        GateAction::Derive => {
            // No change + no plan → derive normally, stamp the (possibly first)
            // baseline below.
        }
        GateAction::CleanupThenDerive => {
            // No change but a plan lingers (the user re-widened the spec → the
            // destructive delta vanished) → delete the stale plan(s), then
            // derive normally and re-stamp the baseline below.
            info!(
                %name, %namespace,
                "no destructive coverage change but a stale MigrationPlan lingers — cleaning up before derive"
            );
            delete_sc_plans(&ctx.client, &namespace, &name).await?;
        }
        GateAction::ConsumeThenDerive { plan: completed } => {
            // The change's plan completed (operator approved → the
            // MigrationController drove it to `completed`) → consume: derive
            // BOTH halves with the narrowed spec, stamp the new baseline, THEN
            // delete the plan (crash-order derive → stamp → delete).
            consume_plan = completed;
            info!(
                %name, %namespace, plan = ?consume_plan,
                "MigrationPlan completed — consuming: deriving with narrowed coverage"
            );
        }
        GateAction::Pause {
            delete_stale,
            create,
            existing,
            reason,
        } => {
            // Stay paused: derive NOTHING, so the old wider-coverage Secrets
            // stay in place and in-flight apps keep clone / pull access.
            // Resolve the uid BEFORE any write, so a (defensive) missing uid
            // never leaves a deleted plan behind.
            let sc_uid = if create {
                Some(sc_uid_or(&cred)?.to_string())
            } else {
                None
            };
            if delete_stale {
                delete_sc_plans(&ctx.client, &namespace, &name).await?;
            }
            let plan_name = match sc_uid {
                Some(uid) => {
                    let mp = SourceCredentialMigrationStrategy::create_plan_for(
                        &candidates,
                        &sc_plan_name(&name, Utc::now()),
                        &namespace,
                        &name,
                        &uid,
                    );
                    let plan_name = mp.name_any();
                    let trigger = change
                        .as_ref()
                        .map(|c| c.trigger_type.as_str())
                        .unwrap_or("");
                    info!(%name, %namespace, plan = %plan_name, %trigger, "{}", reason);
                    ssa_apply_plan(&ctx.client, &namespace, &mp, &name).await?;
                    plan_name
                }
                None => {
                    info!(%name, %namespace, plan = %existing, "{}", reason);
                    existing
                }
            };
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
        GateAction::PauseFailed { plan: plan_name } => {
            // The change's plan is `failed` → keep gating; surface a
            // `MigrationFailed=True` condition requiring manual delete. Derive
            // nothing (both Secrets stay put).
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
        let material = resolve_material(&ctx.client, &git.backend, &namespace).await?;
        let half = git_half_plan(&name, git, &material, previous);
        let api: Api<Secret> = Api::namespaced(ctx.client.clone(), ARGOCD_NAMESPACE);
        for (secret_name, payload) in &half.secrets {
            api.patch(secret_name, &pp, &Patch::Apply(payload)).await?;
        }
        covered_prefixes.extend(half.covered);
        pending |= half.pending;
        conditions.push(half.condition);
        if let HalfMaterial::Present { username, password } = &material {
            // Live reachability probe (S5): find a representative repo
            // covered by each prefix (from a matching Argo Application) and
            // probe it over git smart-HTTP.
            let normalized: Vec<String> = git
                .repo_prefixes
                .iter()
                .map(|p| normalize_repo_url(p))
                .collect();
            let (verdict, message) =
                validity::probe_git_half(&ctx.client, &normalized, username, password).await;
            let (cond, stamp) =
                validity_outcome(COND_GIT_VALID, verdict, &message, previous, Utc::now());
            if stamp.is_some() {
                last_validated = stamp;
            }
            conditions.push(cond);
        }
    }

    // ---- registry half → canonical dockerconfigjson in this ns ----
    if let Some(registry) = &cred.spec.registry {
        let material = resolve_material(&ctx.client, &registry.backend, &namespace).await?;
        let half = registry_half_plan(&name, &namespace, registry, &material, previous);
        let api: Api<Secret> = Api::namespaced(ctx.client.clone(), &namespace);
        for (secret_name, payload) in &half.secrets {
            api.patch(secret_name, &pp, &Patch::Apply(payload)).await?;
        }
        covered_hosts.extend(half.covered);
        pending |= half.pending;
        conditions.push(half.condition);
        if let HalfMaterial::Present { username, password } = &material {
            // Live reachability probe (S5): find a representative image
            // covered by each host (from a matching AppRafter Application)
            // and probe it via a scoped registry v2 token exchange.
            let (verdict, message) =
                validity::probe_registry_half(&ctx.client, &registry.hosts, username, password)
                    .await;
            let (cond, stamp) =
                validity_outcome(COND_REGISTRY_VALID, verdict, &message, previous, Utc::now());
            if stamp.is_some() {
                last_validated = stamp;
            }
            conditions.push(cond);
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
    let stamped_baseline = stamp_baseline(pending, old_spec.as_ref(), &cred.spec);
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

    let (outcome, requeue) = reconcile_outcome(pending);
    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, &namespace, outcome])
        .inc();
    Ok(Action::requeue(requeue))
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
    // 2.22b (D13): sweep CLUSTER-WIDE by label, not two hard-coded
    // namespaces.
    //
    // The derived material does not all live where this function used to
    // look. The Argo repo-cred and the canonical dockerconfigjson are in
    // `argocd` / the credential's own namespace, but the Application
    // controller projects a pull-secret copy into EVERY workload namespace
    // that pulls through this credential — and those namespaces are not
    // knowable from here without listing them.
    //
    // Deleting the PAT is exactly the moment every derivative must be
    // revoked: leaving a copy behind leaves a working registry credential in
    // a namespace after the operator believed they were all withdrawn. The
    // label is the only thing tying them together, since a cross-namespace
    // ownerReference is impossible (see this module's docstring), so the
    // sweep follows the label wherever it went. `cred_namespace` is no
    // longer read for this reason — the selector is namespace-agnostic.
    let _ = cred_namespace;
    let all: Api<Secret> = Api::all(client.clone());
    let lp = ListParams::default().labels(&derived_secret_selector(name));
    for (ns, secret_name) in secret_delete_targets(all.list(&lp).await?.items) {
        let api: Api<Secret> = Api::namespaced(client.clone(), &ns);
        // ignore "already gone" so re-runs after a partial delete converge
        let _ = api.delete(&secret_name, &DeleteParams::default()).await;
    }
    Ok(())
}

/// Pure: the cluster-wide label selector for every Secret derived from
/// this SourceCredential. Namespace-agnostic ON PURPOSE — the Application
/// controller projects pull-secret copies into workload namespaces this
/// function cannot enumerate, and deleting the PAT must revoke all of
/// them.
fn derived_secret_selector(cred_name: &str) -> String {
    format!("{SOURCE_CREDENTIAL_LABEL}={cred_name}")
}

/// Pure: the `(namespace, name)` coordinates to delete from a labelled
/// Secret list. An entry missing either coordinate cannot be addressed and
/// is skipped rather than aborting the sweep — one unaddressable Secret
/// must not leave the remaining derived credentials live.
fn secret_delete_targets(secrets: Vec<Secret>) -> Vec<(String, String)> {
    secrets
        .into_iter()
        .filter_map(|secret| Some((secret.metadata.namespace?, secret.metadata.name?)))
        .collect()
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

/// Pure: the `metadata.finalizers` list to patch this pass, or `None`
/// when the list is already what it should be (no patch → no self-inflicted
/// reconcile). A live SourceCredential must CARRY our finalizer — without
/// it a delete cascades past this controller and the derived Secrets
/// (cross-namespace, so no ownerReference can reach them) leak as working
/// credentials. A deleting one gets it REMOVED, which the caller does only
/// after the GC sweep succeeded.
fn finalizer_patch(deleting: bool, finalizers: &[String]) -> Option<Vec<String>> {
    let ours = finalizers.iter().any(|f| f == DERIVED_SECRETS_FINALIZER);
    match (deleting, ours) {
        (true, true) => Some(without_finalizer(finalizers)),
        (false, false) => Some(with_finalizer(finalizers)),
        _ => None,
    }
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
    Ok(Some(material_from_data(&secret.data.unwrap_or_default())))
}

/// Pure: decode `(username, password)` out of the unsealed material
/// Secret's `data`. A missing OR EMPTY `username` falls back to
/// [`DEFAULT_GIT_USERNAME`] — GitHub (and friends) accept any non-empty
/// username with a PAT password, and Basic auth with an empty username is
/// rejected outright, so the fallback is what makes a `password`-only
/// material work. A missing `password` decodes to the empty string rather
/// than erroring: the derived Secret is still written and the live probe
/// is what reports the credential as rejected.
fn material_from_data(
    data: &std::collections::BTreeMap<String, k8s_openapi::ByteString>,
) -> (String, String) {
    let username = data
        .get(MATERIAL_USERNAME_KEY)
        .map(|b| String::from_utf8_lossy(&b.0).into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_GIT_USERNAME.to_string());
    let password = data
        .get(MATERIAL_PASSWORD_KEY)
        .map(|b| String::from_utf8_lossy(&b.0).into_owned())
        .unwrap_or_default();
    (username, password)
}

/// The credential material one half resolved to, before anything is
/// derived from it. The three cases are exactly the three `*Present`
/// conditions the half can report.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HalfMaterial {
    /// The backend points at OpenBao — not derivable on Tier 1.
    OpenBao,
    /// The sealed material has not been unsealed into a Secret yet;
    /// `location` is the `namespace/name` we looked for.
    Missing { location: String },
    /// Material read.
    Present { username: String, password: String },
}

/// Resolve one half's backend into its [`HalfMaterial`]: no
/// `sealedSecretRef` means OpenBao (nothing to read), otherwise read the
/// unsealed Secret from the ref's namespace (defaulting to the
/// SourceCredential's own).
async fn resolve_material(
    client: &Client,
    backend: &SourceBackend,
    cred_namespace: &str,
) -> Result<HalfMaterial, ReconcileError> {
    let Some(sealed_ref) = &backend.sealed_secret_ref else {
        return Ok(HalfMaterial::OpenBao);
    };
    let material_ns = sealed_ref
        .namespace
        .clone()
        .unwrap_or_else(|| cred_namespace.to_string());
    match read_material(client, &material_ns, &sealed_ref.name).await? {
        None => Ok(HalfMaterial::Missing {
            location: format!("{material_ns}/{}", sealed_ref.name),
        }),
        Some((username, password)) => Ok(HalfMaterial::Present { username, password }),
    }
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

/// Everything ONE half derives, decided purely from its spec + the
/// resolved material. `reconcile` performs exactly the I/O this names and
/// makes no derivation decision of its own — which is what lets the whole
/// derivation be tested without a cluster.
#[derive(Debug, Clone, PartialEq)]
struct HalfPlan {
    /// The `*Present` condition to report for this half.
    condition: SourceCredentialCondition,
    /// `(secret_name, SSA payload)` to server-side apply, in coverage order.
    secrets: Vec<(String, Value)>,
    /// The coverage entries this half actually derived for (empty unless
    /// the Secrets below were written).
    covered: Vec<String>,
    /// The material is still unsealing — nothing was derived, so the caller
    /// must NOT stamp a new migration baseline and must requeue fast.
    pending: bool,
}

/// Pure: the git half's derivation — one Argo CD `repo-creds` Secret per
/// `repoPrefixes` entry, in spec order. Coverage is only claimed on the
/// `Present` arm: an unsealed-yet material derives nothing and reports
/// `pending`, and an OpenBao backend is not derivable on Tier 1 at all
/// (`Unknown`, never `False` — it is not an error, just out of scope).
fn git_half_plan(
    cred_name: &str,
    git: &SourceGit,
    material: &HalfMaterial,
    previous: &[SourceCredentialCondition],
) -> HalfPlan {
    match material {
        HalfMaterial::OpenBao => HalfPlan {
            condition: condition(
                COND_GIT_PRESENT,
                "Unknown",
                REASON_UNVERIFIED,
                "git backend uses openBaoPath; not derivable on Tier 1",
                previous,
            ),
            secrets: Vec::new(),
            covered: Vec::new(),
            pending: false,
        },
        HalfMaterial::Missing { location } => HalfPlan {
            condition: condition(
                COND_GIT_PRESENT,
                "False",
                "MaterialMissing",
                &format!("unsealed material Secret {location} not found yet"),
                previous,
            ),
            secrets: Vec::new(),
            covered: Vec::new(),
            pending: true,
        },
        HalfMaterial::Present { username, password } => {
            let secrets = git
                .repo_prefixes
                .iter()
                .enumerate()
                .map(|(idx, prefix)| {
                    let secret_name = repo_cred_secret_name(cred_name, idx);
                    let payload = repo_cred_payload(
                        &secret_name,
                        &normalize_repo_url(prefix),
                        username,
                        password,
                        cred_name,
                    );
                    (secret_name, payload)
                })
                .collect();
            HalfPlan {
                condition: condition(
                    COND_GIT_PRESENT,
                    "True",
                    "Derived",
                    "Argo repo-creds Secret(s) derived from sealed material",
                    previous,
                ),
                secrets,
                covered: git.repo_prefixes.clone(),
                pending: false,
            }
        }
    }
}

/// Pure: the registry half's derivation — the single canonical
/// `dockerconfigjson` Secret in the credential's own namespace, covering
/// every `hosts` entry. Same three-arm shape as [`git_half_plan`].
fn registry_half_plan(
    cred_name: &str,
    cred_namespace: &str,
    registry: &SourceRegistry,
    material: &HalfMaterial,
    previous: &[SourceCredentialCondition],
) -> HalfPlan {
    match material {
        HalfMaterial::OpenBao => HalfPlan {
            condition: condition(
                COND_REGISTRY_PRESENT,
                "Unknown",
                REASON_UNVERIFIED,
                "registry backend uses openBaoPath; not derivable on Tier 1",
                previous,
            ),
            secrets: Vec::new(),
            covered: Vec::new(),
            pending: false,
        },
        HalfMaterial::Missing { location } => HalfPlan {
            condition: condition(
                COND_REGISTRY_PRESENT,
                "False",
                "MaterialMissing",
                &format!("unsealed material Secret {location} not found yet"),
                previous,
            ),
            secrets: Vec::new(),
            covered: Vec::new(),
            pending: true,
        },
        HalfMaterial::Present { username, password } => {
            let dockercfg = dockerconfigjson(&registry.hosts, username, password);
            let secret_name = pull_secret_name(cred_name);
            let payload = dockercfg_payload(&secret_name, cred_namespace, &dockercfg, cred_name);
            HalfPlan {
                condition: condition(
                    COND_REGISTRY_PRESENT,
                    "True",
                    "Derived",
                    "dockerconfigjson pull-secret derived from sealed material",
                    previous,
                ),
                secrets: vec![(secret_name, payload)],
                covered: registry.hosts.clone(),
                pending: false,
            }
        }
    }
}

/// Pure: the `*Valid` condition for a probe verdict, plus the
/// `status.lastValidated` stamp it contributes.
///
/// Only a CONCLUDED verdict (`Valid` / `Invalid`) stamps the time.
/// `Unverified` — what a network-less cluster reports for every reconcile,
/// forever — must NOT move the timestamp, otherwise `lastValidated` would
/// advance on passes that validated nothing and the operator could no
/// longer tell when the credential was last actually proven.
fn validity_outcome(
    type_: &str,
    verdict: Validity,
    message: &str,
    previous: &[SourceCredentialCondition],
    now: DateTime<Utc>,
) -> (SourceCredentialCondition, Option<String>) {
    let (status, reason) = verdict.condition_parts();
    let stamp = (verdict != Validity::Unverified).then(|| now.to_rfc3339());
    (condition(type_, status, reason, message, previous), stamp)
}

/// Pure: the migration baseline to stamp after a render pass. On a clean
/// derive the CURRENT spec becomes the baseline; while a half is still
/// `pending` (material unsealing) nothing was derived, so the PRIOR
/// baseline is carried forward — stamping the possibly-narrowed new spec
/// there would move the baseline without the gate ever running, silently
/// dropping a coverage narrowing past the MigrationPlan. Carrying (never
/// `None`) also keeps the field manager's ownership so SSA does not prune
/// the field.
fn stamp_baseline(
    pending: bool,
    old_spec: Option<&SourceCredentialSpec>,
    current: &SourceCredentialSpec,
) -> Option<SourceCredentialSpec> {
    if pending {
        old_spec.cloned()
    } else {
        Some(current.clone())
    }
}

/// Pure: the metric outcome label + requeue delay for a render pass. A
/// half still waiting on its material requeues fast (15s) so rotation /
/// first unseal is picked up promptly; a settled credential polls at 60s.
fn reconcile_outcome(pending: bool) -> (&'static str, Duration) {
    if pending {
        ("pending", Duration::from_secs(15))
    } else {
        ("ok", Duration::from_secs(60))
    }
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

/// What the migration gate wants done this pass. Separating the DECISION
/// (pure, below) from its execution (the cluster writes in `reconcile`)
/// makes every arm — including the three that differ only in whether a
/// plan is created and whether a stale one is superseded first — testable
/// without a cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GateAction {
    /// Derive normally and stamp the baseline.
    Derive,
    /// No destructive change, but a stale plan lingers: GC every SC-scope
    /// plan first, then derive.
    CleanupThenDerive,
    /// The gating plan completed: derive with the narrowed coverage, stamp,
    /// THEN delete the named plan (crash-order derive → stamp → delete).
    ConsumeThenDerive { plan: Option<String> },
    /// Stay paused, deriving nothing. `create` (re)creates the gating plan;
    /// `delete_stale` supersedes a mismatched/relic plan first; `existing`
    /// names the plan already gating when `create` is false.
    Pause {
        delete_stale: bool,
        create: bool,
        existing: String,
        reason: &'static str,
    },
    /// The gating plan is `failed`: stay paused with `MigrationFailed=True`
    /// until an operator deletes it by hand.
    PauseFailed { plan: String },
}

/// Pure: map the migration state machine's [`MigrationDecision`] onto the
/// action this controller takes, resolving the gating plan's name from the
/// live plan. Only `ConsumeApply` and the paused arms need the plan; the
/// render arms ignore it.
fn gate_action(decision: MigrationDecision, plan: Option<&MigrationPlan>) -> GateAction {
    let named = || plan.map(|p| p.name_any()).unwrap_or_default();
    match decision {
        MigrationDecision::Render => GateAction::Derive,
        MigrationDecision::DeleteThenRender => GateAction::CleanupThenDerive,
        MigrationDecision::ConsumeApply => GateAction::ConsumeThenDerive {
            plan: plan.and_then(|p| p.metadata.name.clone()),
        },
        MigrationDecision::CreatePlan => GateAction::Pause {
            delete_stale: false,
            create: true,
            existing: String::new(),
            reason: "destructive coverage change detected — creating gating MigrationPlan, pausing both derivation halves",
        },
        MigrationDecision::DeleteThenCreate => GateAction::Pause {
            delete_stale: true,
            create: true,
            existing: String::new(),
            reason: "superseding stale/relic MigrationPlan with a fresh gating plan — staying paused",
        },
        MigrationDecision::NoOp => GateAction::Pause {
            delete_stale: false,
            create: false,
            existing: named(),
            reason: "coverage change already gated by a matching MigrationPlan — staying paused",
        },
        MigrationDecision::BlockFailed => GateAction::PauseFailed { plan: named() },
    }
}

/// Pure: the label selector narrowing a list to this credential's SC-scope
/// MigrationPlans. Shared by the finder and the deleter so they can never
/// drift apart.
fn sc_plan_selector(sc_name: &str) -> String {
    format!("{SCOPE_LABEL}={SCOPE_SOURCECREDENTIAL},{SOURCE_CREDENTIAL_LABEL}={sc_name}")
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
    let lp = ListParams::default().labels(&sc_plan_selector(sc_name));
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
    let lp = ListParams::default().labels(&sc_plan_selector(sc_name));
    for plan_name in sc_plans_to_delete(api.list(&lp).await?.items, sc_name) {
        match api.delete(&plan_name, &DeleteParams::default()).await {
            Ok(_) => {
                info!(%sc_name, plan = %plan_name, "deleted superseded/consumed MigrationPlan")
            }
            Err(kube::Error::Api(e)) if e.code == 404 => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Pure: the names to delete from a label-selected plan list. Belt-and-braces
/// exact scope match on top of the selector — a stray
/// `apprafter.io/source-credential` label (hand-written, or copied from
/// another credential's plan) must never make this controller delete a
/// DIFFERENT credential's gate. A plan with no `metadata.name` cannot be
/// addressed and is skipped.
fn sc_plans_to_delete(plans: Vec<MigrationPlan>, sc_name: &str) -> Vec<String> {
    plans
        .into_iter()
        .filter(|plan| {
            plan.spec
                .scope
                .sourcecredential
                .as_ref()
                .is_some_and(|s| s.ref_.name == sc_name)
        })
        .filter_map(|plan| plan.metadata.name)
        .collect()
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

    /// `metadata.name` is capped at 63 characters and may not end in `-`,
    /// so a long credential name must be TRUNCATED (and re-trimmed after
    /// truncation) — an over-long name is rejected by the apiserver, which
    /// would leave a destructive coverage change permanently ungatable.
    #[test]
    fn sc_plan_name_truncates_to_a_valid_dns_1123_name() {
        let now = DateTime::parse_from_rfc3339("2026-07-17T00:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let long = "a".repeat(80);
        let name = sc_plan_name(&long, now);
        assert_eq!(name.len(), 63);
        assert!(!name.ends_with('-'));
        // Truncation must cut the SUFFIX, keeping the credential-derived head.
        assert!(name.starts_with(&"a".repeat(63)));
        // A name that truncates onto a separator gets the separator trimmed.
        let trailing = sc_plan_name(&format!("{}-", "b".repeat(62)), now);
        assert!(!trailing.ends_with('-'));
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

    // ---------------- derivation: what each half writes ----------------

    fn present() -> HalfMaterial {
        HalfMaterial::Present {
            username: "git".to_string(),
            password: "ghp_x".to_string(),
        }
    }

    fn git_of(spec: &SourceCredentialSpec) -> SourceGit {
        spec.git.clone().expect("git half")
    }

    fn registry_of(spec: &SourceCredentialSpec) -> SourceRegistry {
        spec.registry.clone().expect("registry half")
    }

    /// The git half derives ONE Argo `repo-creds` Secret PER prefix — Argo
    /// matches a clone against a single `url` per Secret, so collapsing two
    /// prefixes into one Secret would silently drop coverage of the second.
    /// Pins the per-index naming, the normalised URL, and that the claimed
    /// coverage is exactly the spec's prefixes.
    #[test]
    fn git_half_plan_derives_one_repo_cred_secret_per_prefix() {
        let spec = sc_spec(&["github.com/acme/", "https://gitlab.com/acme"], &[]);
        let plan = git_half_plan("acme", &git_of(&spec), &present(), &[]);

        let names: Vec<&str> = plan.secrets.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["srccred-acme-repo-0", "srccred-acme-repo-1"]);
        assert_eq!(
            plan.secrets[0].1["stringData"]["url"],
            "https://github.com/acme"
        );
        assert_eq!(
            plan.secrets[1].1["stringData"]["url"],
            "https://gitlab.com/acme"
        );
        assert_eq!(plan.secrets[0].1["stringData"]["password"], "ghp_x");
        assert_eq!(
            plan.covered,
            vec!["github.com/acme/", "https://gitlab.com/acme"]
        );
        assert!(!plan.pending);
        assert_eq!(plan.condition.type_, COND_GIT_PRESENT);
        assert_eq!(plan.condition.status, "True");
        assert_eq!(plan.condition.reason.as_deref(), Some("Derived"));
    }

    /// Material still unsealing: NOTHING may be derived and NO coverage may
    /// be claimed — `coveredRepoPrefixes` is what the CLI reports as live
    /// coverage, and `pending` is what stops the caller from stamping the
    /// migration baseline for a derive that never happened.
    #[test]
    fn git_half_plan_derives_nothing_while_material_is_missing() {
        let spec = sc_spec(&["github.com/acme/"], &[]);
        let plan = git_half_plan(
            "acme",
            &git_of(&spec),
            &HalfMaterial::Missing {
                location: "apprafter-system/srccred-acme-material".to_string(),
            },
            &[],
        );
        assert!(plan.secrets.is_empty());
        assert!(plan.covered.is_empty());
        assert!(plan.pending);
        assert_eq!(plan.condition.status, "False");
        assert_eq!(plan.condition.reason.as_deref(), Some("MaterialMissing"));
        // The message names WHERE we looked, so the operator can fix the ref.
        assert!(plan
            .condition
            .message
            .as_deref()
            .unwrap()
            .contains("apprafter-system/srccred-acme-material"));
    }

    /// An OpenBao backend is out of scope on Tier 1, not broken: the half
    /// reports `Unknown`/`Unverified` and — critically — is NOT `pending`,
    /// so the reconcile still stamps its migration baseline and the
    /// credential does not requeue at the fast pending interval forever.
    #[test]
    fn git_half_plan_openbao_is_unknown_and_not_pending() {
        let spec = sc_spec(&["github.com/acme/"], &[]);
        let plan = git_half_plan("acme", &git_of(&spec), &HalfMaterial::OpenBao, &[]);
        assert!(plan.secrets.is_empty());
        assert!(plan.covered.is_empty());
        assert!(!plan.pending);
        assert_eq!(plan.condition.status, "Unknown");
        assert_eq!(plan.condition.reason.as_deref(), Some(REASON_UNVERIFIED));
    }

    /// The registry half derives exactly ONE canonical `dockerconfigjson`
    /// covering every host (the Application controller projects copies of
    /// that one Secret), in the credential's OWN namespace — not `argocd`.
    #[test]
    fn registry_half_plan_derives_one_dockerconfigjson_for_every_host() {
        let spec = sc_spec(&[], &["ghcr.io/acme/", "registry.gitlab.com/acme/"]);
        let plan = registry_half_plan(
            "acme",
            "apprafter-system",
            &registry_of(&spec),
            &present(),
            &[],
        );
        assert_eq!(plan.secrets.len(), 1);
        let (name, payload) = &plan.secrets[0];
        assert_eq!(name, "srccred-acme-dockercfg");
        assert_eq!(payload["metadata"]["namespace"], "apprafter-system");
        assert_eq!(payload["type"], "kubernetes.io/dockerconfigjson");
        let body: serde_json::Value =
            serde_json::from_str(payload["stringData"][".dockerconfigjson"].as_str().unwrap())
                .unwrap();
        let auths = body["auths"].as_object().unwrap();
        assert_eq!(auths.len(), 2);
        assert!(auths.contains_key("ghcr.io"));
        assert!(auths.contains_key("registry.gitlab.com"));
        assert_eq!(
            plan.covered,
            vec!["ghcr.io/acme/", "registry.gitlab.com/acme/"]
        );
        assert!(!plan.pending);
        assert_eq!(plan.condition.type_, COND_REGISTRY_PRESENT);
        assert_eq!(plan.condition.status, "True");
    }

    /// Registry half, material still unsealing / OpenBao: same contract as
    /// the git half — no Secret, no coverage claimed, `pending` only for the
    /// unsealing case.
    #[test]
    fn registry_half_plan_derives_nothing_without_material() {
        let spec = sc_spec(&[], &["ghcr.io/acme/"]);
        let missing = registry_half_plan(
            "acme",
            "apprafter-system",
            &registry_of(&spec),
            &HalfMaterial::Missing {
                location: "apprafter-system/srccred-acme-material".to_string(),
            },
            &[],
        );
        assert!(missing.secrets.is_empty());
        assert!(missing.covered.is_empty());
        assert!(missing.pending);
        assert_eq!(missing.condition.status, "False");

        let openbao = registry_half_plan(
            "acme",
            "apprafter-system",
            &registry_of(&spec),
            &HalfMaterial::OpenBao,
            &[],
        );
        assert!(openbao.secrets.is_empty());
        assert!(!openbao.pending);
        assert_eq!(openbao.condition.status, "Unknown");
    }

    // ---------------- probe verdict → condition + lastValidated ----------------

    /// `status.lastValidated` means "when the credential was last actually
    /// PROVEN". Only a concluded verdict may stamp it: `Unverified` is the
    /// steady state of every network-less / restricted-egress cluster, and
    /// stamping it would make `lastValidated` advance every 60s while
    /// nothing was ever validated.
    #[test]
    fn validity_outcome_stamps_only_a_concluded_verdict() {
        let now = DateTime::parse_from_rfc3339("2026-07-17T10:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);

        let (cond, stamp) = validity_outcome(COND_GIT_VALID, Validity::Unverified, "m", &[], now);
        assert_eq!(stamp, None);
        assert_eq!(cond.status, "Unknown");
        assert_eq!(cond.reason.as_deref(), Some(REASON_UNVERIFIED));

        let (cond, stamp) = validity_outcome(COND_GIT_VALID, Validity::Valid, "m", &[], now);
        assert_eq!(stamp.as_deref(), Some("2026-07-17T10:00:00+00:00"));
        assert_eq!(cond.status, "True");

        let (cond, stamp) = validity_outcome(COND_REGISTRY_VALID, Validity::Invalid, "m", &[], now);
        assert!(stamp.is_some());
        assert_eq!(cond.type_, COND_REGISTRY_VALID);
        assert_eq!(cond.status, "False");
        assert_eq!(cond.message.as_deref(), Some("m"));
    }

    // ---------------- render-path tail ----------------

    /// A coverage-narrowing edit made WHILE the material is still unsealing
    /// must not move the migration baseline: nothing was derived, so
    /// stamping the narrowed spec would let the narrowing slip past the
    /// MigrationPlan gate on the next pass (detection compares against the
    /// baseline). Carrying the prior baseline forward also keeps the SSA
    /// field manager's ownership so the field is not pruned.
    #[test]
    fn stamp_baseline_carries_the_prior_spec_while_pending() {
        let wide = sc_spec(&["github.com/acme/", "github.com/acme-labs/"], &[]);
        let narrow = sc_spec(&["github.com/acme/"], &[]);
        assert_eq!(
            stamp_baseline(true, Some(&wide), &narrow).as_ref(),
            Some(&wide)
        );
        assert_eq!(
            stamp_baseline(false, Some(&wide), &narrow).as_ref(),
            Some(&narrow)
        );
        // No prior baseline + still pending → nothing to carry.
        assert_eq!(stamp_baseline(true, None, &narrow), None);
    }

    /// A half waiting on its material must be re-checked fast (the sealed
    /// secret usually materialises within seconds); a settled credential
    /// polls slowly so rotation is picked up without hammering the
    /// apiserver.
    #[test]
    fn reconcile_outcome_requeues_faster_while_pending() {
        assert_eq!(
            reconcile_outcome(true),
            ("pending", Duration::from_secs(15))
        );
        assert_eq!(reconcile_outcome(false), ("ok", Duration::from_secs(60)));
    }

    /// Basic auth with an empty username is rejected by git hosts, so a
    /// material carrying only a PAT must fall back to a non-empty username
    /// — for both the MISSING and the EMPTY-STRING case.
    #[test]
    fn material_from_data_falls_back_to_a_non_empty_username() {
        use k8s_openapi::ByteString;
        let data = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), ByteString(v.as_bytes().to_vec())))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        assert_eq!(
            material_from_data(&data(&[("username", "acme-bot"), ("password", "ghp_x")])),
            ("acme-bot".to_string(), "ghp_x".to_string())
        );
        assert_eq!(
            material_from_data(&data(&[("password", "ghp_x")])),
            (DEFAULT_GIT_USERNAME.to_string(), "ghp_x".to_string())
        );
        assert_eq!(
            material_from_data(&data(&[("username", ""), ("password", "ghp_x")])),
            (DEFAULT_GIT_USERNAME.to_string(), "ghp_x".to_string())
        );
        // A material with no password still decodes — the live probe, not
        // this decoder, is what reports the credential as rejected.
        assert_eq!(
            material_from_data(&data(&[("username", "acme-bot")])),
            ("acme-bot".to_string(), String::new())
        );
    }

    // ---------------- gate action (decision → work) ----------------

    /// Every migration decision maps to exactly one action. The three
    /// PAUSED decisions differ ONLY in whether a plan is created and
    /// whether a stale one is superseded first — getting that pair wrong
    /// either leaves the credential ungated (deriving the narrowed
    /// coverage without approval) or spams a fresh plan every 30s.
    #[test]
    fn gate_action_maps_every_decision() {
        let plan = sc_plan(
            "acme",
            "coverage-removal",
            "spec.git.repoPrefixes",
            Some("pending-approval"),
            None,
        );
        assert_eq!(
            gate_action(MigrationDecision::Render, None),
            GateAction::Derive
        );
        assert_eq!(
            gate_action(MigrationDecision::DeleteThenRender, Some(&plan)),
            GateAction::CleanupThenDerive
        );
        assert_eq!(
            gate_action(MigrationDecision::ConsumeApply, Some(&plan)),
            GateAction::ConsumeThenDerive {
                plan: Some("acme-migration-1".to_string())
            }
        );
        match gate_action(MigrationDecision::CreatePlan, None) {
            GateAction::Pause {
                delete_stale,
                create,
                ..
            } => {
                assert!(create, "CreatePlan must create the gating plan");
                assert!(!delete_stale, "there is no stale plan to supersede");
            }
            other => panic!("CreatePlan must pause: {other:?}"),
        }
        match gate_action(MigrationDecision::DeleteThenCreate, Some(&plan)) {
            GateAction::Pause {
                delete_stale,
                create,
                ..
            } => {
                assert!(delete_stale, "the mismatched plan must be superseded");
                assert!(create, "…and replaced by a fresh gating plan");
            }
            other => panic!("DeleteThenCreate must pause: {other:?}"),
        }
        match gate_action(MigrationDecision::NoOp, Some(&plan)) {
            GateAction::Pause {
                delete_stale,
                create,
                existing,
                ..
            } => {
                assert!(
                    !create,
                    "a matching plan already gates — do not create another"
                );
                assert!(!delete_stale);
                assert_eq!(existing, "acme-migration-1");
            }
            other => panic!("NoOp must pause: {other:?}"),
        }
        assert_eq!(
            gate_action(MigrationDecision::BlockFailed, Some(&plan)),
            GateAction::PauseFailed {
                plan: "acme-migration-1".to_string()
            }
        );
    }

    /// `ConsumeApply` without a live plan (it vanished between the list and
    /// the decision) must consume nothing rather than name a phantom plan
    /// for deletion.
    #[test]
    fn gate_action_consume_without_a_plan_names_nothing() {
        assert_eq!(
            gate_action(MigrationDecision::ConsumeApply, None),
            GateAction::ConsumeThenDerive { plan: None }
        );
    }

    // ---------------- GC selection ----------------

    /// The derived-Secret sweep is CLUSTER-WIDE by label: the Application
    /// controller projects pull-secret copies into workload namespaces this
    /// controller cannot enumerate, and deleting the PAT must revoke every
    /// one of them. Pins the selector to the label alone — adding a
    /// namespace term would leave working credentials behind.
    #[test]
    fn derived_secret_selector_matches_the_label_alone() {
        assert_eq!(
            derived_secret_selector("acme"),
            "apprafter.io/source-credential=acme"
        );
    }

    /// The sweep deletes by `(namespace, name)`; an entry missing either
    /// coordinate cannot be addressed and must be SKIPPED, not abort the
    /// sweep — one malformed object must never leave the remaining derived
    /// credentials live in the cluster.
    #[test]
    fn secret_delete_targets_skips_unaddressable_entries() {
        let secret = |name: Option<&str>, ns: Option<&str>| Secret {
            metadata: kube::core::ObjectMeta {
                name: name.map(str::to_string),
                namespace: ns.map(str::to_string),
                ..Default::default()
            },
            ..Default::default()
        };
        let targets = secret_delete_targets(vec![
            secret(Some("srccred-acme-repo-0"), Some("argocd")),
            secret(None, Some("argocd")),
            secret(Some("srccred-acme-dockercfg"), None),
            secret(Some("srccred-acme-dockercfg"), Some("landing")),
        ]);
        assert_eq!(
            targets,
            vec![
                ("argocd".to_string(), "srccred-acme-repo-0".to_string()),
                ("landing".to_string(), "srccred-acme-dockercfg".to_string()),
            ]
        );
    }

    /// The plan selector pins BOTH the scope discriminator and the owning
    /// credential: dropping either term would let this controller list (and
    /// then delete) another credential's — or another scope's — plans.
    #[test]
    fn sc_plan_selector_pins_scope_and_credential() {
        assert_eq!(
            sc_plan_selector("acme"),
            "apprafter.io/scope=sourcecredential,apprafter.io/source-credential=acme"
        );
    }

    /// Belt-and-braces: the label selector is not trusted on its own. A plan
    /// that carries this credential's label but is scoped to ANOTHER
    /// credential must not be deleted — that would silently un-gate the
    /// other credential's coverage narrowing.
    #[test]
    fn sc_plans_to_delete_ignores_a_plan_scoped_to_another_credential() {
        let mine = sc_plan(
            "acme",
            "coverage-removal",
            "spec.git.repoPrefixes",
            None,
            None,
        );
        let mut theirs = sc_plan(
            "other",
            "coverage-removal",
            "spec.registry.hosts",
            None,
            None,
        );
        theirs.metadata.name = Some("other-migration-1".to_string());
        let mut unnamed = sc_plan(
            "acme",
            "coverage-removal",
            "spec.git.repoPrefixes",
            None,
            None,
        );
        unnamed.metadata.name = None;

        assert_eq!(
            sc_plans_to_delete(vec![theirs, mine, unnamed], "acme"),
            vec!["acme-migration-1".to_string()]
        );
    }

    /// The finalizer is the ONLY thing standing between a deleted
    /// SourceCredential and a working PAT left behind in every namespace it
    /// was projected into (cross-namespace ownerReferences do not exist), so
    /// a live object must always carry it and a deleting one must have it
    /// removed. Both no-op cases must return `None` — patching an unchanged
    /// list would re-trigger reconcile forever.
    #[test]
    fn finalizer_patch_adds_on_live_and_removes_on_delete() {
        let ours = vec![DERIVED_SECRETS_FINALIZER.to_string()];
        let foreign = vec!["other.io/keep".to_string()];
        let both = vec![
            "other.io/keep".to_string(),
            DERIVED_SECRETS_FINALIZER.to_string(),
        ];

        assert_eq!(finalizer_patch(false, &[]), Some(ours.clone()));
        assert_eq!(finalizer_patch(false, &foreign), Some(both.clone()));
        assert_eq!(finalizer_patch(false, &ours), None);

        assert_eq!(finalizer_patch(true, &both), Some(foreign.clone()));
        assert_eq!(finalizer_patch(true, &ours), Some(vec![]));
        // Already released (or never ours) → nothing to patch.
        assert_eq!(finalizer_patch(true, &foreign), None);
    }

    // ---------------- cluster-facing contracts (offline client) ----------------

    /// A `Client` aimed at a closed local port: every request it makes fails
    /// with a connection refusal, so these tests observe what the controller
    /// does when the apiserver cannot be reached — without a cluster.
    ///
    /// The provider install mirrors `install_rustls_crypto_provider` in the
    /// operator/webhook binaries: rustls 0.23 has no auto-default provider,
    /// so building the client's TLS connector panics without it.
    pub(crate) fn unreachable_client() -> Client {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let config = kube::Config::new("http://127.0.0.1:1".parse().unwrap());
        Client::try_from(config).expect("build an offline kube client")
    }

    /// An OpenBao backend has no Secret to read, so resolving it must not
    /// touch the apiserver at all — proven here by resolving it through a
    /// client that cannot reach anything and still getting `OpenBao` rather
    /// than a transport error.
    #[tokio::test]
    async fn resolve_material_openbao_never_reads_a_secret() {
        let backend = SourceBackend {
            sealed_secret_ref: None,
            open_bao_path: Some("secret/apprafter/gh".to_string()),
        };
        let resolved = resolve_material(&unreachable_client(), &backend, "apprafter-system")
            .await
            .expect("openBao resolves without any cluster read");
        assert_eq!(resolved, HalfMaterial::OpenBao);
    }

    /// A backend WITH a `sealedSecretRef` must read the Secret — and when
    /// that read fails, the error must PROPAGATE. Silently treating an
    /// unreachable apiserver as "material missing" would report
    /// `GitPresent=False/MaterialMissing` (a lie) and, worse, mark the half
    /// pending forever.
    #[tokio::test]
    async fn resolve_material_propagates_a_failed_secret_read() {
        let backend = sc_backend();
        let err = resolve_material(&unreachable_client(), &backend, "apprafter-system")
            .await
            .expect_err("an unreachable apiserver must not look like a missing Secret");
        assert!(matches!(err, ReconcileError::Kube(_)));
    }

    /// The GC sweep must FAIL LOUDLY when it cannot list: `reconcile`
    /// releases the finalizer only after this returns `Ok`, so swallowing a
    /// list error would let the SourceCredential finish deleting while its
    /// derived PATs stayed live in every namespace they were projected into.
    #[tokio::test]
    async fn gc_derived_secrets_propagates_a_failed_sweep() {
        let err = gc_derived_secrets(&unreachable_client(), "apprafter-system", "acme")
            .await
            .expect_err("an unlistable cluster must not report a clean sweep");
        assert!(matches!(err, ReconcileError::Kube(_)));
    }

    /// A failed plan LIST must propagate, never degrade to "no plan": the
    /// gate reads `None` as "nothing gating me", so a swallowed error would
    /// let a coverage narrowing derive without the operator's approval.
    #[tokio::test]
    async fn find_sc_plan_propagates_a_list_failure_instead_of_reporting_no_plan() {
        let err = find_sc_plan(&unreachable_client(), "apprafter-system", "acme")
            .await
            .expect_err("an unlistable cluster must not look like an ungated credential");
        assert!(matches!(err, ReconcileError::Kube(_)));
    }

    /// `delete_sc_plans` tolerates a 404 (the plan already cascaded) but a
    /// genuine RBAC / apiserver fault must propagate — a silently-skipped
    /// delete leaves a stale gate that pauses the credential forever.
    #[tokio::test]
    async fn delete_sc_plans_propagates_a_list_failure() {
        let err = delete_sc_plans(&unreachable_client(), "apprafter-system", "acme")
            .await
            .expect_err("a failed list must not report a completed cleanup");
        assert!(matches!(err, ReconcileError::Kube(_)));
    }

    /// Every reconcile error must be visible on BOTH metrics — the
    /// per-kind/namespace outcome counter (for "which credential is
    /// failing") and the error-only counter alerts fire on — and must be
    /// retried rather than dropped.
    // `#[tokio::test]` only because building a kube `Client` needs a
    // runtime; `error_policy` itself is synchronous and does no I/O.
    #[tokio::test]
    async fn error_policy_counts_the_error_on_both_metrics() {
        let ctx = Arc::new(Context {
            client: unreachable_client(),
            metrics: Arc::new(Metrics::new()),
        });
        let mut cred = SourceCredential::new("acme", sc_spec(&["github.com/acme/"], &[]));
        cred.metadata.namespace = Some("apprafter-system".into());
        let err = ReconcileError::MissingUid("acme".into());

        assert_eq!(
            ctx.metrics
                .reconcile_errors
                .with_label_values(&[KIND])
                .get(),
            0.0
        );
        let action = error_policy(Arc::new(cred), &err, ctx.clone());
        assert_eq!(
            ctx.metrics
                .reconcile_errors
                .with_label_values(&[KIND])
                .get(),
            1.0
        );
        assert_eq!(
            ctx.metrics
                .reconcile_total
                .with_label_values(&[KIND, "apprafter-system", "error"])
                .get(),
            1.0
        );
        // The controller must come back to it, not drop the object.
        assert_eq!(
            format!("{action:?}"),
            format!("{:?}", Action::requeue(Duration::from_secs(30)))
        );
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
