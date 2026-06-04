// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! RetainedClaim garbage-collection Controller (Phase 2.4f) — the 7th
//! controller in the operator binary.
//!
//! Watches `apprafter.io/v1alpha1` `RetainedClaim` resources
//! cluster-wide (they all live in `apprafter-system`, but the watch is
//! `Api::all` so the namespace is immaterial). For each snapshot it
//! parses `spec.retainUntil` against an INJECTED `now` (`Utc::now()` in
//! production) and:
//!
//!   - if `retainUntil` hasn't passed → requeue for the remaining grace
//!     (floored to 60s so a near-deadline snapshot is re-checked
//!     promptly);
//!   - if the original `ResourceClaim` is back (a recovery re-claim with
//!     no `deletion_timestamp`) → the snapshot is stale: delete it and
//!     STOP, never drop a live claim's role/DB (Phase 2.4f Fix A
//!     live-guard; the provisioner's cancel-on-reprovision is primary,
//!     this is the belt-and-suspenders GC side);
//!   - once `retainUntil` passes (and no live claim) → drop the per-claim
//!     Postgres role + database in a PHASED, CNPG-confirmed sequence
//!     (Phase 2.4f Fix B2). CNPG drops a managed role ONLY via an
//!     `ensure: absent` entry (pruning merely un-manages it — the
//!     role-leak bug) and CANNOT drop a role that still owns a database,
//!     so:
//!       * Phase 1 — declare the `Database` CR `spec.ensure: absent`
//!         (CNPG drops the DB; deleting the CR would NOT, the Postgres
//!         reclaim default is `retain`) THEN UPSERT the managed-role entry
//!         to `ensure: absent` (kept, not pruned);
//!       * Phase 2 — GET the Cluster `status.managedRolesStatus` and
//!         requeue (`ROLE_DROP_REQUEUE`) until the role lands in
//!         `byStatus.reconciled` (drop confirmed);
//!       * Phase 3 — ONLY THEN prune the `ensure: absent` entry (so the
//!         shared Cluster spec accumulates no tombstones), delete the
//!         password Secret, and delete the RetainedClaim snapshot.
//!
//! Every drop step is idempotent + 404-tolerant: a half-finished sweep
//! (operator crash mid-drop) re-runs cleanly — the phase is recomputed
//! from live state — and a missing role / DB / Secret / cluster (already
//! gone, or never provisioned) is swallowed. A malformed `retainUntil`
//! logs + requeues — never panics, never silently skips.
//!
//! ## SSA / read split (CRITICAL)
//!
//! The GC reads `RetainedClaim.spec` ONLY. It never writes a
//! `ResourceClaim` (that object is long gone) and never writes
//! `RetainedClaim` status (there is no status subresource). The
//! RetainedClaim is immutable from creation; the GC's only write to it
//! is the terminal `delete`.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use kube::api::{Api, DeleteParams, DynamicObject, Patch};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Client, ResourceExt};
use serde_json::{json, Value};
use tracing::{info, warn};

use operator_core::{Metrics, ResourceClaim, RetainedClaim};

use crate::cnpg;
use crate::grace;
use crate::reconcile::{apply_params, cluster_ar, database_ar, secret_ar, GC_ROLE_RMW_RETRIES};
use crate::{Context, ReconcileError};

/// Kind label for this controller's metrics.
pub(crate) const KIND: &str = "RetainedClaim";

/// Requeue interval after a completed sweep (or a still-pending check
/// with no shorter deadline). Cheap re-list cadence.
const REQUEUE_AFTER: Duration = Duration::from_secs(300);

/// Floor on the remaining-grace requeue so a snapshot whose deadline is
/// seconds away is re-checked promptly without a tight busy-loop.
const MIN_REQUEUE: Duration = Duration::from_secs(60);

/// Requeue between Phase 2 polls while waiting for CNPG to drop the
/// `ensure: absent` role (2.4f Fix B2). CNPG drops the DB then the role
/// over a few reconcile passes; we poll the Cluster `status` until the
/// role lands in `byStatus.reconciled`, then prune. Short so the drop
/// finalizes promptly without a tight busy-loop.
const ROLE_DROP_REQUEUE: Duration = Duration::from_secs(15);

/// Spawn the RetainedClaim GC Controller (7th controller).
pub async fn run(client: Client, metrics: Arc<Metrics>) -> Result<(), ReconcileError> {
    let retained: Api<RetainedClaim> = Api::all(client.clone());
    let ctx = Arc::new(Context { client, metrics });
    info!("RetainedClaimGC starting");
    Controller::new(retained, watcher::Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj_ref, _)) => info!(retained = %obj_ref.name, "gc reconciled"),
                Err(e) => warn!(error = %e, "gc reconcile failed"),
            }
        })
        .await;
    info!("RetainedClaimGC stream ended");
    Ok(())
}

/// Reconcile a single `RetainedClaim`: requeue until `retainUntil`,
/// then drop the role + DB + password Secret + the snapshot.
pub async fn reconcile(
    rc: Arc<RetainedClaim>,
    ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    let now = Utc::now();
    let rc_name = rc.name_any();
    let rc_ns = rc.namespace().unwrap_or_default();

    // Parse retainUntil. A malformed value is a corrupt snapshot — log
    // and requeue rather than panic (the snapshot is operator-written
    // and immutable, so this should never happen, but the GC must be
    // crash-safe regardless).
    let retain_until = match grace::parse_retain_until(&rc.spec.retain_until) {
        Ok(t) => t,
        Err(err) => {
            warn!(
                retained = %rc_name, retain_until = %rc.spec.retain_until, %err,
                "RetainedClaim has a malformed retainUntil — requeueing, not dropping"
            );
            return Ok(Action::requeue(REQUEUE_AFTER));
        }
    };

    // Not yet expired → requeue for the remaining grace (floored).
    if !grace::should_gc(retain_until, now) {
        let remaining = grace::remaining_grace(retain_until, now).max(MIN_REQUEUE);
        info!(
            retained = %rc_name, retain_until = %rc.spec.retain_until,
            requeue_secs = remaining.as_secs(),
            "RetainedClaim grace not yet elapsed — requeueing"
        );
        return Ok(Action::requeue(remaining));
    }

    info!(
        retained = %rc_name, retain_until = %rc.spec.retain_until,
        role = %rc.spec.role, database = %rc.spec.database_object_name,
        "RetainedClaim grace elapsed — dropping role/DB/Secret"
    );

    // 2.4f Fix A live-guard: if the original ResourceClaim is back (a
    // recovery re-claim), this RetainedClaim is stale — never drop a live
    // claim's role/DB. Delete the snapshot and stop (the provisioner's
    // cancel is primary; this is the belt-and-suspenders GC side).
    let claim_api: Api<ResourceClaim> =
        Api::namespaced(ctx.client.clone(), &rc.spec.claim_ref.namespace);
    if let Ok(Some(c)) = claim_api.get_opt(&rc.spec.claim_ref.name).await {
        if claim_is_live(&c) {
            info!(
                retained = %rc_name, claim = %rc.spec.claim_ref.name,
                "live ResourceClaim present (recovery) — deleting stale RetainedClaim, skipping drop"
            );
            delete_retained_claim(&ctx.client, &rc_ns, &rc_name).await?;
            ctx.metrics
                .claim_gc_total
                .with_label_values(&["skipped-live", &rc_ns])
                .inc();
            return Ok(Action::await_change());
        }
    }

    // Phased role drop (2.4f Fix B2). CNPG drops a managed role ONLY via
    // an `ensure: absent` entry (pruning the entry merely un-manages it —
    // the role-leak bug), and it CANNOT drop a role that still owns a
    // database (it records `cannotReconcile: owner of database …`). So the
    // drop is ordered DB-then-role and gated on CNPG confirming the drop
    // before we prune. Each phase is idempotent + 404-tolerant; the GC
    // requeues between phases (crash-safe — a re-entry recomputes the
    // phase from live state).

    // Phase 1 — declare absent: the Database (`ensure: absent`, existing)
    // THEN the role (`ensure: absent`, upsert — kept, NOT pruned). Order
    // matters: CNPG drops the DB first, which unblocks the role drop.
    remove_database(&ctx, &rc).await?;
    if !set_role_absent(&ctx, &rc).await? {
        // The shared Cluster is already gone → no role/DB to drop. Skip
        // straight to finalize (Phase 3 steps are all 404-tolerant).
        return finalize_drop(&ctx, &rc, &rc_ns, &rc_name).await;
    }

    // Phase 2 — confirm the role is dropped. GET the Cluster status; until
    // the role appears in `byStatus.reconciled` (drop confirmed), requeue
    // and come back. A 404 here means the Cluster vanished mid-drop →
    // nothing left to prune, proceed to finalize.
    let cluster_status = cluster_managed_roles_status(&ctx, &rc).await?;
    match next_drop_decision(cluster_status.as_ref(), &rc.spec.role) {
        DropDecision::WaitForDrop => {
            info!(
                retained = %rc_name, role = %rc.spec.role,
                "role drop pending (DB/role not yet reconciled by CNPG) — requeueing"
            );
            return Ok(Action::requeue(ROLE_DROP_REQUEUE));
        }
        DropDecision::Finalize => {
            info!(
                retained = %rc_name, role = %rc.spec.role,
                "role drop confirmed (or Cluster gone) — finalizing"
            );
        }
    }

    // Phase 3 — finalize: NOW that CNPG has dropped the role, prune the
    // `ensure: absent` entry (so the shared Cluster spec accumulates no
    // absent tombstones), delete the password Secret, delete the snapshot.
    finalize_drop(&ctx, &rc, &rc_ns, &rc_name).await
}

/// Phase 3 of the 2.4f drop: the role is confirmed dropped (or the
/// Cluster is gone). Prune the managed-role entry, delete the password
/// Secret, delete the RetainedClaim snapshot, and record success. Each
/// step is idempotent + 404-tolerant.
async fn finalize_drop(
    ctx: &Arc<Context>,
    rc: &RetainedClaim,
    rc_ns: &str,
    rc_name: &str,
) -> Result<Action, ReconcileError> {
    remove_managed_role(ctx, rc).await?;
    delete_password_secret(ctx, rc).await?;
    delete_retained_claim(&ctx.client, rc_ns, rc_name).await?;

    ctx.metrics
        .claim_gc_total
        .with_label_values(&["success", rc_ns])
        .inc();
    info!(retained = %rc_name, "RetainedClaim GC complete");

    Ok(Action::requeue(REQUEUE_AFTER))
}

/// Error policy: increment the GC error counter + requeue after 30s
/// (mirror the scheduler/provisioner cadence).
pub fn error_policy(rc: Arc<RetainedClaim>, err: &ReconcileError, ctx: Arc<Context>) -> Action {
    let name = rc.name_any();
    let namespace = rc.namespace().unwrap_or_default();
    warn!(%name, %namespace, %err, "RetainedClaim GC reconcile error");
    ctx.metrics
        .claim_gc_total
        .with_label_values(&["error", &namespace])
        .inc();
    ctx.metrics
        .reconcile_errors
        .with_label_values(&[KIND])
        .inc();
    Action::requeue(Duration::from_secs(30))
}

/// Read-modify-write the shared Cluster's unkeyed `spec.managed.roles`,
/// applying `transform` to the current list and PUT-replacing (409-retry;
/// mirror of the provisioner's `upsert_managed_role`). Returns `Ok(true)`
/// when the replace lands, `Ok(false)` when the Cluster is gone (404 on
/// the GET or mid-RMW) — the caller treats a missing Cluster as "nothing
/// to do" (Phase-1 role-absent) or "skip to finalize" (the reconcile).
///
/// Both 2.4f GC role mutations share this loop: Phase 1 upserts an
/// `ensure: absent` entry (`cnpg::merge_role` + `managed_role_entry_absent`,
/// keeping the entry so CNPG drops the role), Phase 3 prunes it
/// (`cnpg::remove_role`, after CNPG confirms the drop).
async fn rmw_managed_roles<F>(
    ctx: &Arc<Context>,
    rc: &RetainedClaim,
    transform: F,
) -> Result<bool, ReconcileError>
where
    F: Fn(Vec<Value>) -> Vec<Value>,
{
    let cluster = &rc.spec.cnpg_cluster;
    let cnpg_ns = &rc.spec.cnpg_namespace;
    let api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), cnpg_ns, &cluster_ar());

    for attempt in 0..GC_ROLE_RMW_RETRIES {
        let current = match api.get(cluster).await {
            Ok(c) => c,
            Err(kube::Error::Api(e)) if e.code == 404 => {
                info!(%cluster, %cnpg_ns, "shared Cluster already gone — role RMW no-op");
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        };
        let mut current_json = serde_json::to_value(&current)?;

        let existing: Vec<Value> = current_json
            .pointer("/spec/managed/roles")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let next = transform(existing);

        let spec = current_json
            .get_mut("spec")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| {
                ReconcileError::Provisioning(format!("Cluster {cluster} has no spec object"))
            })?;
        let managed = spec
            .entry("managed")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| {
                ReconcileError::Provisioning(format!(
                    "Cluster {cluster} spec.managed not an object"
                ))
            })?;
        managed.insert("roles".to_string(), Value::Array(next));

        let replaced: DynamicObject = serde_json::from_value(current_json)?;
        match api.replace(cluster, &Default::default(), &replaced).await {
            Ok(_) => return Ok(true),
            Err(kube::Error::Api(e)) if e.code == 409 => {
                warn!(
                    %cluster, attempt,
                    "managed.roles GC RMW conflict (409) — retrying with fresh resourceVersion"
                );
                continue;
            }
            Err(kube::Error::Api(e)) if e.code == 404 => {
                info!(%cluster, "shared Cluster deleted mid-RMW — role RMW no-op");
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(ReconcileError::Provisioning(format!(
        "managed.roles GC RMW for {cluster} exhausted {GC_ROLE_RMW_RETRIES} retries on 409 conflict"
    )))
}

/// Phase 1 (2.4f Fix B2): UPSERT the per-claim role to `ensure: absent`
/// in the shared Cluster's `spec.managed.roles` — CNPG then drops the
/// role (after the DB is dropped). The entry is KEPT, not pruned: pruning
/// only un-manages the role (the role-leak bug). Returns `false` when the
/// Cluster is gone (caller skips straight to finalize). 409-retried,
/// 404-tolerant via [`rmw_managed_roles`].
async fn set_role_absent(ctx: &Arc<Context>, rc: &RetainedClaim) -> Result<bool, ReconcileError> {
    let role = rc.spec.role.clone();
    let landed = rmw_managed_roles(ctx, rc, |existing| {
        cnpg::merge_role(existing, cnpg::managed_role_entry_absent(&role))
    })
    .await?;
    if landed {
        info!(role = %rc.spec.role, "role declared ensure:absent (CNPG drops it after the DB)");
    }
    Ok(landed)
}

/// Phase 3 (2.4f Fix B2): PRUNE the per-claim role entry from the shared
/// Cluster's `spec.managed.roles` — called ONLY after CNPG has confirmed
/// the drop (the role is in `byStatus.reconciled`), so the shared spec
/// accumulates no `ensure: absent` tombstones. 409-retried, 404-tolerant.
async fn remove_managed_role(ctx: &Arc<Context>, rc: &RetainedClaim) -> Result<(), ReconcileError> {
    let role = rc.spec.role.clone();
    rmw_managed_roles(ctx, rc, |existing| cnpg::remove_role(existing, &role)).await?;
    Ok(())
}

/// Phase 2 (2.4f Fix B2): GET the shared Cluster and return its
/// `status.managedRolesStatus` object (`None` if the Cluster is gone).
/// The reconcile feeds it to [`role_is_dropped`] to decide whether CNPG
/// has finished dropping the `ensure: absent` role before pruning.
async fn cluster_managed_roles_status(
    ctx: &Arc<Context>,
    rc: &RetainedClaim,
) -> Result<Option<Value>, ReconcileError> {
    let cluster = &rc.spec.cnpg_cluster;
    let cnpg_ns = &rc.spec.cnpg_namespace;
    let api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), cnpg_ns, &cluster_ar());
    match api.get(cluster).await {
        Ok(c) => {
            let json = serde_json::to_value(&c)?;
            Ok(Some(
                json.pointer("/status/managedRolesStatus")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            ))
        }
        Err(kube::Error::Api(e)) if e.code == 404 => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Set the `Database` CR to `spec.ensure: absent` via an SSA-patch under
/// the provisioner's field manager (CNPG then drops the DB). We do NOT
/// delete the CR: the Postgres reclaim default is `retain`, so deleting
/// the CR would leave the database in place; `ensure: absent` is the
/// correct drop, and the ~1 KB tombstone self-heals if the app comes
/// back (decision 2). Swallows a 404 if the Database is already gone.
async fn remove_database(ctx: &Arc<Context>, rc: &RetainedClaim) -> Result<(), ReconcileError> {
    let db_object = &rc.spec.database_object_name;
    let cnpg_ns = &rc.spec.cnpg_namespace;
    let api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), cnpg_ns, &database_ar());
    // Re-send the FULL Database body with `ensure: absent`. A partial SSA
    // apply (spec.ensure only) under this same field manager would STRIP the
    // cluster/name/owner this manager previously owned, leaving an incomplete
    // spec the apiserver rejects / CNPG cannot drop. Reconstruct from the
    // snapshot so ownership is preserved and the drop actually happens.
    let body = cnpg::database_object(
        db_object,
        cnpg_ns,
        &rc.spec.cnpg_cluster,
        &rc.spec.database,
        &rc.spec.role,
        "absent",
    );
    match api
        .patch(db_object, &apply_params(), &Patch::Apply(&body))
        .await
    {
        Ok(_) => {
            info!(database = %db_object, %cnpg_ns, "Database set to ensure:absent (CNPG drops the DB)");
            Ok(())
        }
        Err(kube::Error::Api(e)) if e.code == 404 => {
            info!(database = %db_object, "Database CR already gone — ensure:absent no-op");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Delete the per-claim basic-auth password Secret in the CNPG
/// namespace (no ownerRef → no cascade, so the GC must delete it).
/// Swallows a 404. The connection Secret in the claim's own namespace
/// already cascaded on the original claim delete — NOT GC's concern.
async fn delete_password_secret(
    ctx: &Arc<Context>,
    rc: &RetainedClaim,
) -> Result<(), ReconcileError> {
    let secret = &rc.spec.password_secret_name;
    let cnpg_ns = &rc.spec.cnpg_namespace;
    let api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), cnpg_ns, &secret_ar());
    match api.delete(secret, &DeleteParams::default()).await {
        Ok(_) => {
            info!(secret = %secret, %cnpg_ns, "password Secret deleted");
            Ok(())
        }
        Err(kube::Error::Api(e)) if e.code == 404 => {
            info!(secret = %secret, "password Secret already gone — delete no-op");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Delete the RetainedClaim snapshot — the terminal GC step. Swallows a
/// 404 (a racing duplicate reconcile may have deleted it first).
async fn delete_retained_claim(
    client: &Client,
    ns: &str,
    name: &str,
) -> Result<(), ReconcileError> {
    let api: Api<RetainedClaim> = Api::namespaced(client.clone(), ns);
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => {
            info!(retained = %name, %ns, "RetainedClaim deleted");
            Ok(())
        }
        Err(kube::Error::Api(e)) if e.code == 404 => {
            info!(retained = %name, "RetainedClaim already gone — delete no-op");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------------------
// Pure decision helpers (unit-tested without a cluster)
// ---------------------------------------------------------------------------

/// True iff the fetched `ResourceClaim` is LIVE — present with no
/// `deletion_timestamp` (Phase 2.4f Fix A live-guard).
///
/// When the GC finds the original claim back at this name with no
/// pending deletion, the user re-claimed (recovery) within the grace
/// window: the snapshot's role/DB now back the LIVE claim, so the GC
/// must NOT drop them — it deletes the stale RetainedClaim instead. A
/// claim that is itself mid-deletion (`deletion_timestamp` set) is NOT
/// live: it will produce its own fresh RetainedClaim, so this older one
/// may still be GC'd.
pub fn claim_is_live(claim: &ResourceClaim) -> bool {
    claim.metadata.deletion_timestamp.is_none()
}

/// True iff `role` is reported DROPPED by CNPG — i.e. it appears in
/// `managed_roles_status.byStatus.reconciled` (Phase 2.4f Fix B2).
///
/// `managed_roles_status` is the `Cluster` `status.managedRolesStatus`
/// object. CNPG's declarative-role reconciler reports each managed role
/// under `byStatus.<status>` where `<status>` is one of `reconciled` /
/// `pending-reconciliation` / `not-managed` / `reserved` (arrays of role
/// names), plus a `cannotReconcile` map keyed by role name with error
/// strings. For an `ensure: absent` entry, "reconciled" means the
/// database state matches spec — i.e. the role is DROPPED.
///
/// The GC declares the role absent, then requeues until this returns
/// `true`, THEN prunes the entry. Pruning before `reconciled` would
/// merely un-manage a still-present role (the role-leak bug B). A role
/// stuck under `pending-reconciliation` (DB not yet dropped) or
/// `cannotReconcile` (e.g. still owns a database) returns `false`, so the
/// GC keeps waiting rather than leaking.
pub fn role_is_dropped(managed_roles_status: &Value, role: &str) -> bool {
    managed_roles_status
        .pointer("/byStatus/reconciled")
        .and_then(Value::as_array)
        .map(|roles| roles.iter().any(|r| r.as_str() == Some(role)))
        .unwrap_or(false)
}

/// Phase-2 verdict of the 2.4f Fix B2 drop state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropDecision {
    /// CNPG confirms the role is dropped (in `byStatus.reconciled`) OR the
    /// shared Cluster is gone — proceed to Phase 3 (prune + finalize).
    Finalize,
    /// CNPG has not yet dropped the role — requeue and re-check.
    WaitForDrop,
}

/// Pure Phase-2 decision: given the Cluster's `managedRolesStatus`
/// (`None` if the Cluster is gone) and the role name, decide whether to
/// finalize the drop or keep waiting.
///
/// - Cluster gone (`None`) → [`DropDecision::Finalize`]: nothing left to
///   prune, the role/DB went with the Cluster.
/// - role in `byStatus.reconciled` → [`DropDecision::Finalize`]: CNPG
///   confirmed the drop; now safe to prune the `ensure: absent` entry.
/// - otherwise → [`DropDecision::WaitForDrop`]: still pending (DB not yet
///   dropped, role still owns the DB, or status not yet populated). NEVER
///   prune here — pruning before `reconciled` un-manages a live role (the
///   role-leak bug B).
pub fn next_drop_decision(managed_roles_status: Option<&Value>, role: &str) -> DropDecision {
    match managed_roles_status {
        None => DropDecision::Finalize,
        Some(status) if role_is_dropped(status, role) => DropDecision::Finalize,
        Some(_) => DropDecision::WaitForDrop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
    use operator_core::ResourceClaimSpec;
    use serde_json::json;

    // --- claim_is_live() (2.4f Fix A live-guard) ---

    #[test]
    fn claim_is_live_true_when_no_deletion_timestamp() {
        // A claim back at the same name with no pending deletion is a
        // recovery re-claim — the snapshot is stale and must NOT be GC'd.
        let claim = ResourceClaim::new("demo-web-pg", ResourceClaimSpec::default());
        assert!(claim_is_live(&claim));
    }

    #[test]
    fn claim_is_live_false_when_deletion_timestamp_set() {
        // A claim that is itself mid-deletion will produce its OWN fresh
        // RetainedClaim, so this older snapshot is not protected by it.
        let mut claim = ResourceClaim::new("demo-web-pg", ResourceClaimSpec::default());
        claim.metadata.deletion_timestamp = Some(Time(Utc::now()));
        assert!(!claim_is_live(&claim));
    }

    // --- role_is_dropped() (2.4f Fix B2 drop-confirmation) ---

    #[test]
    fn role_is_dropped_true_when_role_in_reconciled() {
        let status = json!({
            "byStatus": {
                "reconciled": ["claim_demo_web", "other-role"],
                "pending-reconciliation": [],
            },
        });
        assert!(role_is_dropped(&status, "claim_demo_web"));
    }

    #[test]
    fn role_is_dropped_false_when_pending_reconciliation() {
        // The DB hasn't been dropped yet — the role is still mid-flight.
        let status = json!({
            "byStatus": {
                "reconciled": [],
                "pending-reconciliation": ["claim_demo_web"],
            },
        });
        assert!(!role_is_dropped(&status, "claim_demo_web"));
    }

    #[test]
    fn role_is_dropped_false_when_cannot_reconcile_owns_database() {
        // CNPG cannot drop a role that still owns a database — it lands in
        // `cannotReconcile` (a map keyed by role), NOT `reconciled`.
        let status = json!({
            "byStatus": { "reconciled": [] },
            "cannotReconcile": {
                "claim_demo_web": ["could not perform DELETE on role claim_demo_web: owner of database claim_demo_web"],
            },
        });
        assert!(!role_is_dropped(&status, "claim_demo_web"));
    }

    #[test]
    fn role_is_dropped_false_when_absent_from_all_buckets() {
        let status = json!({
            "byStatus": {
                "reconciled": ["unrelated"],
                "reserved": ["postgres", "streaming_replica"],
            },
        });
        assert!(!role_is_dropped(&status, "claim_demo_web"));
    }

    #[test]
    fn role_is_dropped_false_when_status_missing_or_empty() {
        // No managedRolesStatus yet (CNPG hasn't reconciled the spec) →
        // never report a drop, so the GC keeps waiting.
        assert!(!role_is_dropped(&json!({}), "claim_demo_web"));
        assert!(!role_is_dropped(&json!(null), "claim_demo_web"));
        assert!(!role_is_dropped(
            &json!({ "byStatus": {} }),
            "claim_demo_web"
        ));
    }

    // --- next_drop_decision() (2.4f Fix B2 Phase-2 verdict) ---

    #[test]
    fn next_drop_decision_finalize_when_cluster_gone() {
        // Cluster vanished mid-drop → nothing left to prune.
        assert_eq!(
            next_drop_decision(None, "claim_demo_web"),
            DropDecision::Finalize
        );
    }

    #[test]
    fn next_drop_decision_finalize_when_role_reconciled() {
        // CNPG confirmed the drop → safe to prune the absent entry.
        let status = json!({ "byStatus": { "reconciled": ["claim_demo_web"] } });
        assert_eq!(
            next_drop_decision(Some(&status), "claim_demo_web"),
            DropDecision::Finalize
        );
    }

    #[test]
    fn next_drop_decision_wait_when_pending_or_cannot_reconcile() {
        // Still pending (DB not yet dropped) → keep waiting, NEVER prune.
        let pending = json!({ "byStatus": { "pending-reconciliation": ["claim_demo_web"] } });
        assert_eq!(
            next_drop_decision(Some(&pending), "claim_demo_web"),
            DropDecision::WaitForDrop
        );
        // Owns a database → cannotReconcile → keep waiting (the DB-absent
        // declaration unblocks it on a later pass).
        let cannot = json!({
            "byStatus": { "reconciled": [] },
            "cannotReconcile": { "claim_demo_web": ["owner of database claim_demo_web"] },
        });
        assert_eq!(
            next_drop_decision(Some(&cannot), "claim_demo_web"),
            DropDecision::WaitForDrop
        );
        // Status not yet populated → wait, never prematurely prune.
        assert_eq!(
            next_drop_decision(Some(&json!({})), "claim_demo_web"),
            DropDecision::WaitForDrop
        );
    }

    // --- set_role_absent transform correctness (the RMW closure) ---

    #[test]
    fn set_role_absent_transform_upserts_present_entry_to_absent() {
        // The closure `set_role_absent` feeds `rmw_managed_roles` is
        // `merge_role(existing, managed_role_entry_absent(role))`. Confirm
        // it REPLACES the present entry in-place with an ensure:absent one
        // (CNPG drops the role) and preserves foreign entries — NOT a
        // prune (which would only un-manage it, the role-leak bug).
        let existing = vec![
            cnpg::managed_role_entry("claim_demo_web", "claim-demo-web-pw"),
            json!({ "name": "keep-me", "login": false }),
        ];
        let out = cnpg::merge_role(existing, cnpg::managed_role_entry_absent("claim_demo_web"));
        assert_eq!(
            out.len(),
            2,
            "foreign entry preserved, target replaced not duplicated"
        );
        let target = out
            .iter()
            .find(|r| r["name"] == "claim_demo_web")
            .expect("target role still present (declared absent, not pruned)");
        assert_eq!(target["ensure"], "absent");
        assert!(out.iter().any(|r| r["name"] == "keep-me"));
    }
}
