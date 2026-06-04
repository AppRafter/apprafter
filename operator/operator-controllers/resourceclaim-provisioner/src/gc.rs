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
//!   - once `retainUntil` passes → drop the per-claim Postgres role
//!     (RMW the shared Cluster's `spec.managed.roles` via
//!     `cnpg::remove_role` + 409-retry), set the `Database` CR to
//!     `spec.ensure: absent` (CNPG drops the DB — the Postgres reclaim
//!     default is `retain`, so deleting the CR would NOT drop the DB;
//!     `ensure: absent` is the correct drop), delete the password
//!     Secret, and finally delete the RetainedClaim itself.
//!
//! Every drop step is idempotent + 404-tolerant: a half-finished sweep
//! (operator crash mid-drop) re-runs cleanly, and a missing role / DB /
//! Secret / cluster (already gone, or never provisioned) is swallowed.
//! A malformed `retainUntil` logs + requeues — never panics, never
//! silently skips.
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

use operator_core::{Metrics, RetainedClaim};

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

    // Each step idempotent + 404-tolerant, in order.
    remove_managed_role(&ctx, &rc).await?;
    remove_database(&ctx, &rc).await?;
    delete_password_secret(&ctx, &rc).await?;
    delete_retained_claim(&ctx.client, &rc_ns, &rc_name).await?;

    ctx.metrics
        .claim_gc_total
        .with_label_values(&["success", &rc_ns])
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

/// Drop the per-claim role from the shared Cluster's unkeyed
/// `spec.managed.roles` (RMW + 409-retry; mirror of the provisioner's
/// `upsert_managed_role`, but using `cnpg::remove_role`). Swallows a 404
/// if the Cluster is already gone.
async fn remove_managed_role(ctx: &Arc<Context>, rc: &RetainedClaim) -> Result<(), ReconcileError> {
    let cluster = &rc.spec.cnpg_cluster;
    let cnpg_ns = &rc.spec.cnpg_namespace;
    let role = &rc.spec.role;
    let api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), cnpg_ns, &cluster_ar());

    for attempt in 0..GC_ROLE_RMW_RETRIES {
        let current = match api.get(cluster).await {
            Ok(c) => c,
            Err(kube::Error::Api(e)) if e.code == 404 => {
                info!(%cluster, %cnpg_ns, "shared Cluster already gone — role drop no-op");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        let mut current_json = serde_json::to_value(&current)?;

        let existing: Vec<Value> = current_json
            .pointer("/spec/managed/roles")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let pruned = cnpg::remove_role(existing, role);

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
        managed.insert("roles".to_string(), Value::Array(pruned));

        let replaced: DynamicObject = serde_json::from_value(current_json)?;
        match api.replace(cluster, &Default::default(), &replaced).await {
            Ok(_) => return Ok(()),
            Err(kube::Error::Api(e)) if e.code == 409 => {
                warn!(
                    %cluster, attempt,
                    "managed.roles GC RMW conflict (409) — retrying with fresh resourceVersion"
                );
                continue;
            }
            Err(kube::Error::Api(e)) if e.code == 404 => {
                info!(%cluster, "shared Cluster deleted mid-RMW — role drop no-op");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(ReconcileError::Provisioning(format!(
        "managed.roles GC RMW for {cluster} exhausted {GC_ROLE_RMW_RETRIES} retries on 409 conflict"
    )))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
