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

use crate::acl_reconcile::{read_secret_key, redis_namespace};
use crate::cnpg;
use crate::dragonfly;
use crate::grace;
use crate::nats;
use crate::nats_accounts;
use crate::nats_client;
use crate::reconcile::{
    apply_params, cluster_ar, database_ar, find_provider, jetstream_account_ar,
    jetstream_consumer_ar, jetstream_stream_ar, pvc_ar, reconcile_accounts_secret, secret_ar,
    Backend, DEFAULT_NATS_SYSTEM_NAMESPACE, GC_ROLE_RMW_RETRIES, NATS_CLIENT_PORT,
    NATS_STATEFULSET_NAME,
};
use crate::{Context, ReconcileError};

/// Kind label for this controller's metrics.
pub(crate) const KIND: &str = "RetainedClaim";

/// Requeue interval after a completed sweep (or a still-pending check
/// with no shorter deadline). Cheap re-list cadence.
const REQUEUE_AFTER: Duration = Duration::from_secs(300);

/// Floor on the remaining-grace requeue so a snapshot whose deadline is
/// seconds away is re-checked promptly without a tight busy-loop.
const MIN_REQUEUE: Duration = Duration::from_secs(60);

/// CEILING on that requeue, and it is a crash guard rather than a tuning knob.
///
/// The remaining grace is computed from `retainUntil`, which is DATA on an
/// object. kube-runtime schedules a requeue through a tokio `DelayQueue`,
/// which PANICS on a deadline it cannot represent — `invalid deadline;
/// err=Invalid` — and that panic takes the whole operator down, not just this
/// reconcile. A `RetainedClaim` whose `retainUntil` is years out therefore
/// crash-looped every controller in the process: the 2.22 battery produced
/// exactly that with a fixture dated 2031 (a 4.3-year requeue).
///
/// Production snapshots are always deletion + 7 days, so nothing legitimate
/// reaches this ceiling. That is precisely why it is worth having: the values
/// that get here are hand-edited objects, clock skew, and mistakes — the cases
/// where the operator most needs to stay up. Re-checking a far-future deadline
/// once an hour costs nothing.
const MAX_REQUEUE: Duration = Duration::from_secs(3600);

/// Clamp a computed requeue delay into the representable window (D23).
///
/// EXTRACTED SO THE TESTS CAN CALL IT. The two clamp tests used to write
/// `raw.max(MIN_REQUEUE).min(MAX_REQUEUE)` in their own bodies and assert on
/// the result — which asserts that the TEST's arithmetic works, and stays
/// green if the clamp is deleted from the reconcile. That is the same
/// cannot-fail shape this file's D23 entry exists to prevent, committed while
/// fixing D23.
///
/// The ceiling is not cosmetic: `tokio_util::time::DelayQueue` PANICS on a
/// deadline it cannot represent, and a panic on a worker takes the whole
/// operator process down. One `RetainedClaim` carrying an absurd
/// `retainUntil` is enough, and the data comes from an object a user can
/// write. The floor keeps a near-deadline claim from spinning the reconcile.
fn clamp_requeue(raw: Duration) -> Duration {
    raw.max(MIN_REQUEUE).min(MAX_REQUEUE)
}

/// Requeue between Phase 2 polls while waiting for CNPG to drop the
/// `ensure: absent` role (2.4f Fix B2). CNPG drops the DB then the role
/// over a few reconcile passes; we poll the Cluster `status` until the
/// role lands in `byStatus.reconciled`, then prune. Short so the drop
/// finalizes promptly without a tight busy-loop.
const ROLE_DROP_REQUEUE: Duration = Duration::from_secs(15);

/// Spawn the RetainedClaim GC Controller (7th controller).
pub async fn run(
    client: Client,
    metrics: Arc<Metrics>,
    acl_dirty: Arc<tokio::sync::Notify>,
) -> Result<(), ReconcileError> {
    let retained: Api<RetainedClaim> = Api::all(client.clone());
    let ctx = Arc::new(Context::with_acl_dirty(client, metrics, acl_dirty));
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
        let remaining = clamp_requeue(grace::remaining_grace(retain_until, now));
        info!(
            retained = %rc_name, retain_until = %rc.spec.retain_until,
            requeue_secs = remaining.as_secs(),
            "RetainedClaim grace not yet elapsed — requeueing"
        );
        return Ok(Action::requeue(remaining));
    }

    info!(
        retained = %rc_name, retain_until = %rc.spec.retain_until,
        role = rc.spec.role.as_deref().unwrap_or_default(),
        database = rc.spec.database_object_name.as_deref().unwrap_or_default(),
        "RetainedClaim grace elapsed — dropping role/DB/Secret"
    );

    // 2.4f Fix A live-guard: if the original ResourceClaim is back (a
    // recovery re-claim), this RetainedClaim is stale — never drop a live
    // claim's role/DB. Delete the snapshot and stop (the provisioner's
    // cancel is primary; this is the belt-and-suspenders GC side).
    // The live-guard MUST fail CLOSED. `get_opt` errors on any non-404
    // (apiserver 503 / throttle / network blip); the `?` propagates that
    // to error_policy → requeue → re-check on a healthy apiserver, rather
    // than silently treating it as "no live claim" and FALLING THROUGH to
    // the destructive drop (which would drop a LIVE re-attached claim's DB).
    // We drop ONLY on a confirmed `Ok(None)` (claim genuinely gone) or a
    // claim that is itself mid-deletion.
    let claim_api: Api<ResourceClaim> =
        Api::namespaced(ctx.client.clone(), &rc.spec.claim_ref.namespace);
    match claim_api.get_opt(&rc.spec.claim_ref.name).await? {
        Some(c) if claim_is_live(&c) => {
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
        // Ok(None) = claim genuinely gone (safe to drop); Some(mid-deletion)
        // = it will produce its own fresh RetainedClaim, so this stale one
        // is GC-able.
        _ => {}
    }

    // Backend dispatch (2.6-7). The live-guard above is backend-agnostic
    // (it protects ANY recovery re-claim from a destructive drop). Past it,
    // the reclaim mechanics diverge: dragonfly is `FLUSHDB` + `ACL DELUSER`
    // + connection-Secret delete + snapshot delete; CNPG is the 2.4f phased
    // role/DB drop below. A legacy / empty backend defaults to CNPG.
    match gc_backend(&rc.spec.backend) {
        GcBackend::Dragonfly => return gc_drop_dragonfly(&ctx, &rc, &rc_ns, &rc_name).await,
        GcBackend::Disk => return gc_drop_disk(&ctx, &rc, &rc_ns, &rc_name).await,
        GcBackend::Nats => return gc_drop_nats(&ctx, &rc, &rc_ns, &rc_name).await,
        GcBackend::Cnpg => { /* fall through to the phased CNPG drop */ }
    }

    // Phased role drop (2.4f Fix B2). CNPG drops a managed role ONLY via
    // an `ensure: absent` entry (pruning the entry merely un-manages it —
    // the role-leak bug), and it CANNOT drop a role that still owns a
    // database (it records `cannotReconcile: owner of database …`). So the
    // drop is ordered DB-then-role and gated on CNPG confirming the drop
    // before we prune. Each phase is idempotent + 404-tolerant; the GC
    // requeues between phases (crash-safe — a re-entry recomputes the
    // phase from live state).

    // Phase 1a — Database `ensure: absent` (idempotent; ordered FIRST so the
    // owner-role can ever be dropped: CNPG cannot drop a role that owns a DB).
    remove_database(&ctx, &rc).await?;

    // GET the shared Cluster ONCE (spec + status, consistent snapshot).
    // 404 → Cluster gone → role/DB went with it → finalize. A transient
    // error PROPAGATES (fail-closed via `?`) → error_policy requeues.
    let cluster = match get_cluster(&ctx, &rc).await? {
        None => return finalize_drop(&ctx, &rc, &rc_ns, &rc_name).await,
        Some(c) => c,
    };

    let role = rc.spec.role.clone().unwrap_or_default();
    if role_entry_ensure(&cluster, &role) != Some("absent") {
        // PASS 1 — the entry is still `ensure: present` (or missing). Declare
        // it absent, then WAIT one CNPG reconcile cycle before trusting status:
        // `byStatus.reconciled` is stale-true right after the PUT (it still
        // reflects the prior present-and-matching state), and pruning on that
        // stale read is the role-leak bug. Re-entry next pass sees the entry
        // already-absent and proceeds to confirm.
        if !set_role_absent(&ctx, &rc).await? {
            // Cluster vanished mid-RMW → nothing to drop.
            return finalize_drop(&ctx, &rc, &rc_ns, &rc_name).await;
        }
        info!(
            retained = %rc_name, %role,
            "role declared ensure:absent — waiting one CNPG reconcile before confirming the drop"
        );
        return Ok(Action::requeue(ROLE_DROP_REQUEUE));
    }

    // PASS 2+ — the entry is ALREADY `ensure: absent` (declared on a prior
    // pass; CNPG has reconciled it), so `byStatus` now reflects the absent
    // spec and is trustworthy.
    let status = cluster
        .pointer("/status/managedRolesStatus")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if role_is_dropped(&status, &role) {
        info!(retained = %rc_name, %role, "CNPG confirms the role is dropped — finalizing");
        finalize_drop(&ctx, &rc, &rc_ns, &rc_name).await
    } else {
        // Still dropping, or STUCK (e.g. cannotReconcile: owns a DB that has
        // live connections). Observability so a permanent wedge is visible
        // (finding #4 — no silent forever-loop).
        if let Some(reason) = role_cannot_reconcile_reason(&status, &role) {
            warn!(
                retained = %rc_name, %role, %reason,
                "role drop BLOCKED (CNPG cannotReconcile) — retrying; needs attention if persistent"
            );
            ctx.metrics
                .claim_gc_total
                .with_label_values(&["blocked", &rc_ns])
                .inc();
        } else {
            info!(retained = %rc_name, %role, "role drop pending CNPG reconcile — requeueing");
            ctx.metrics
                .claim_gc_total
                .with_label_values(&["waiting", &rc_ns])
                .inc();
        }
        Ok(Action::requeue(ROLE_DROP_REQUEUE))
    }
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

/// Dragonfly reclaim (2.6-7, ADR 0042 §8). The live-guard has already run
/// (shared with the CNPG path), so we are past the grace window with the
/// claim confirmed gone. Reclaim the per-claim allocation imperatively over
/// the Redis admin seam, then delete the connection Secret + the snapshot.
///
/// Order: `FLUSHDB` the numbered DB (wipe the claim's data so a future
/// dbnum reuse starts empty — belt-and-suspenders, the provisioner also
/// flushes on allocate), then `ACL DELUSER` the `$N`-pinned user (revoke
/// access). The freed `dbnum` returns to the pool implicitly — the next
/// allocation scan reads only LIVE claims' `status.dbnum`, and this claim
/// is gone, so its number is free again with no explicit release.
///
/// Idempotent + failure-tolerant by design: a snapshot with no allocation
/// (empty `instance` — a claim deleted before the provisioner wrote
/// `status.instance`/`dbnum`; the snapshot still carries the deterministic
/// `aclUser` + connection-Secret refs, but `instance` is the field that
/// gates reclaim, and there is no DB to reach) skips the Redis ops entirely;
/// a Redis op against a
/// torn-down instance (the whole pool instance was deleted, e.g. a tier
/// teardown) is logged and tolerated rather than wedging the GC forever on
/// an unreachable host — there is nothing left to leak in that case. The
/// connection Secret usually already cascaded on the original claim delete
/// (it is owner-ref'd to the claim); the delete here is belt-and-suspenders
/// for the recovery path and is 404-tolerant.
async fn gc_drop_dragonfly(
    ctx: &Arc<Context>,
    rc: &RetainedClaim,
    rc_ns: &str,
    rc_name: &str,
) -> Result<Action, ReconcileError> {
    // Reclaim the DB + revoke the user only when there IS an allocation.
    // A claim deleted before it was ever provisioned snapshots with an empty
    // `instance` (dbnum 0) — the deterministic `aclUser` is still set, but no
    // DB/user was ever created on an instance, so there is nothing to reach.
    // Skip straight to the Secret + snapshot cleanup.
    if let Some(target) = dragonfly_reclaim_target(rc) {
        let DragonflyReclaim {
            instance,
            dbnum,
            acl_user,
        } = target;
        // The snapshot carries the instance NAME but not its namespace;
        // resolve it the SAME way the provisioner + ACL loop do (the seeded
        // `redis-integrated` ServiceProvider config, default `dragonfly-system`).
        let df_ns = redis_namespace(ctx).await?;
        let addr = dragonfly::instance_addr(&instance, &df_ns);
        let admin_secret = dragonfly::admin_secret_name(&instance);

        // 2.6 Fix #2b defensive live-guard: if a DIFFERENT live claim has
        // recycled this snapshot's (instance, dbnum) before the grace
        // elapsed, FLUSHDB would wipe THAT tenant's data (cross-tenant loss).
        // List live ResourceClaims and decide per `dragonfly_flushdb_is_safe`.
        // The allocator now RESERVES retained dbnums so a recycle should never
        // happen, but this guard makes the destructive flush fail-safe. When
        // unsafe we SKIP the FLUSHDB but STILL ACL DELUSER (per-claim
        // usernames differ, so dropping the dead user is always safe) + the
        // Secret/snapshot delete below.
        let live_claims: Vec<ResourceClaim> = Api::<ResourceClaim>::all(ctx.client.clone())
            .list(&Default::default())
            .await?
            .items;

        // Reading the admin password is itself tolerated-on-failure: if the
        // instance (and its admin Secret) is already gone, there is nothing
        // left to reclaim on it, so we log and proceed to the local cleanup
        // rather than failing the whole GC closed on an unreachable host.
        match read_secret_key(ctx, &df_ns, &admin_secret, "password").await {
            Ok(admin_pw) => {
                if let Some(dbnum) = dbnum {
                    if dragonfly_flushdb_is_safe(
                        &live_claims,
                        &instance,
                        dbnum,
                        &rc.spec.claim_ref.name,
                        &rc.spec.claim_ref.namespace,
                    ) {
                        if let Err(err) = ctx.redis.flushdb(&addr, &admin_pw, dbnum).await {
                            warn!(
                                retained = %rc_name, %instance, dbnum, %err,
                                "dragonfly FLUSHDB failed during GC — tolerating (instance may be gone)"
                            );
                        }
                    } else {
                        warn!(
                            retained = %rc_name, %instance, dbnum,
                            "dragonfly GC live-guard: a different live claim holds this \
                             (instance, dbnum) — SKIPPING FLUSHDB to avoid cross-tenant data loss; \
                             still revoking the dead ACL user"
                        );
                    }
                }
                if let Err(err) = ctx.redis.acl_deluser(&addr, &admin_pw, &acl_user).await {
                    warn!(
                        retained = %rc_name, %instance, user = %acl_user, %err,
                        "dragonfly ACL DELUSER failed during GC — tolerating (instance may be gone)"
                    );
                } else {
                    // ADR 0042 §10: close the revocation window. The runtime
                    // user is gone, but until the loop re-derives the file it
                    // still carries this tenant's line — and a restart inside
                    // that window would RE-GRANT a credential the platform has
                    // already revoked. Only on success: a failed DELUSER means
                    // the user may still exist, and dropping its line while it
                    // does would make the file disagree with the instance.
                    ctx.acl_dirty.notify_one();
                }
            }
            Err(err) => {
                warn!(
                    retained = %rc_name, %instance, %err,
                    "dragonfly admin Secret unreadable during GC (instance likely torn down) — \
                     skipping FLUSHDB/DELUSER, proceeding to Secret + snapshot cleanup"
                );
            }
        }
    } else {
        info!(
            retained = %rc_name,
            "dragonfly snapshot has no instance (claim deleted pre-provision) — \
             no DB to reach on an instance; nothing to reclaim"
        );
    }

    // Delete the connection Secret in the claim's origin namespace
    // (belt-and-suspenders — it usually cascaded on the original claim
    // delete via its ownerRef; 404-tolerant for the case it did not).
    delete_connection_secret(ctx, rc).await?;

    // Terminal step: drop the snapshot.
    delete_retained_claim(&ctx.client, rc_ns, rc_name).await?;

    ctx.metrics
        .claim_gc_total
        .with_label_values(&["success", rc_ns])
        .inc();
    info!(retained = %rc_name, "dragonfly RetainedClaim GC complete");

    Ok(Action::requeue(REQUEUE_AFTER))
}

/// Disk reclaim (2.6b-5, ADR 0043). The live-guard has already run (shared
/// with the CNPG/dragonfly paths), so we are past the grace window with the
/// source disk claim confirmed gone — a re-deploy within grace would have
/// cancelled this snapshot (the provisioner's cancel-on-reprovision) and
/// reattached to the SAME PVC via idempotent SSA, so reaching here means the
/// app is genuinely gone and the retained data may be reclaimed.
///
/// DELETE the unowned RWO PVC named `spec.volumeClaimRef` in
/// `spec.volumeClaimNamespace` (the PVC has NO ownerRef — the provisioner
/// created it standalone precisely so an app delete does not cascade it — so
/// the GC is the ONLY thing that drops it), then delete the snapshot. Both
/// deletes are idempotent + 404-tolerant: a half-finished sweep (operator
/// crash between the two deletes) re-runs cleanly, and a snapshot whose PVC
/// was already removed (or never created — a pre-provision delete) is a
/// no-op. A snapshot missing `volumeClaimRef`/`volumeClaimNamespace` skips
/// straight to the snapshot delete.
async fn gc_drop_disk(
    ctx: &Arc<Context>,
    rc: &RetainedClaim,
    rc_ns: &str,
    rc_name: &str,
) -> Result<Action, ReconcileError> {
    if let (Some(pvc), Some(ns)) = (
        rc.spec.volume_claim_ref.as_deref(),
        rc.spec.volume_claim_namespace.as_deref(),
    ) {
        let api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), ns, &pvc_ar());
        match api.delete(pvc, &DeleteParams::default()).await {
            Ok(_) => info!(%pvc, %ns, retained = %rc_name, "disk PVC deleted"),
            Err(kube::Error::Api(e)) if e.code == 404 => {
                info!(%pvc, retained = %rc_name, "disk PVC already gone — delete no-op")
            }
            Err(e) => return Err(e.into()),
        }
    } else {
        info!(
            retained = %rc_name,
            "disk snapshot has no volumeClaimRef (claim deleted pre-provision) — \
             no PVC to delete; dropping the snapshot"
        );
    }

    // Terminal step: drop the snapshot.
    delete_retained_claim(&ctx.client, rc_ns, rc_name).await?;

    ctx.metrics
        .claim_gc_total
        .with_label_values(&["success", rc_ns])
        .inc();
    info!(retained = %rc_name, "disk RetainedClaim GC complete");

    Ok(Action::requeue(REQUEUE_AFTER))
}

/// What the NATS GC may delete for a departing application, and what it
/// must only REPORT (2.5e, ADR 0061 §8). Pure — no client, no clock — so
/// the rule is unit-pinned rather than only observable on a live cluster.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NatsSweep {
    /// NATS-side stream names to delete.
    pub sweep: Vec<String>,
    /// NATS-side stream names the rule CANNOT attribute — reported,
    /// never deleted.
    pub unattributed: Vec<String>,
}

/// Whether every one of `subjects` lies under `<app>.`.
///
/// **Prefix, with the separating dot, and nothing cleverer.** `feeder.`
/// does not prefix `feederbot.orders`, so two applications whose names
/// share a prefix cannot sweep each other. A subject equal to the bare
/// app name, or a bare `>`, is NOT under the prefix.
///
/// An EMPTY subject list is `false`, not vacuously true: a stream with no
/// subjects (a mirror, or one sourced from others) cannot be attributed by
/// subject at all, and the whole rule is "keys on subjects, never on
/// names". Treating it as "wholly under" would delete exactly the streams
/// the rule has no evidence about.
fn subjects_wholly_under(subjects: &[String], app_prefix: &str) -> bool {
    !subjects.is_empty() && subjects.iter().all(|s| s.starts_with(app_prefix))
}

/// The sweep plan for one departing application's account (ADR 0061 §8).
///
/// - `declared` — the departing claim's OWN declared streams (NATS-side
///   names). Deleted unconditionally: the platform created them for this
///   application and no other application can have been granted them.
/// - `protected` — every stream DECLARED by any other live claim in the
///   same account. Never touched and never reported: it is attributed,
///   just not to us. ("excluding declared streams, whatever their owner")
/// - everything else is DYNAMIC, and is judged by SUBJECTS ONLY: wholly
///   under `<app>.` → delete; some-but-not-all under it → report as
///   unattributed and leave; none under it → leave silently, it is not
///   ours in any sense.
///
/// **This is a heuristic in BOTH directions and stays one.** ADR 0061 §8
/// says so plainly: a capture stream over a neighbour's subjects is
/// missed, and a capture stream over the DEPARTING application's prefix is
/// swept with it, reassigning ownership. Do not "tighten" this into a
/// name-based rule — dynamic stream names are unconstrained, which is the
/// entire reason the rule keys on subjects.
///
/// An empty `app` disables the dynamic sweep entirely (returns only the
/// declared streams): the prefix would be a bare `"."`, which matches
/// nothing sane, and the failure mode of guessing here is deleting another
/// tenant's data.
pub fn nats_sweep_plan(
    streams: &[nats_client::StreamSummary],
    app: &str,
    declared: &[String],
    protected: &[String],
) -> NatsSweep {
    let mut out = NatsSweep::default();
    let app_prefix = format!("{app}.");
    for s in streams {
        if declared.iter().any(|d| d == &s.name) {
            out.sweep.push(s.name.clone());
            continue;
        }
        if protected.iter().any(|p| p == &s.name) {
            continue;
        }
        if app.is_empty() {
            continue;
        }
        if subjects_wholly_under(&s.subjects, &app_prefix) {
            out.sweep.push(s.name.clone());
        } else if s.subjects.iter().any(|x| x.starts_with(&app_prefix)) || s.subjects.is_empty() {
            out.unattributed.push(s.name.clone());
        }
    }
    out.sweep.sort();
    out.sweep.dedup();
    out.unattributed.sort();
    out.unattributed.dedup();
    out
}

/// [`nats_sweep_plan`], plus the one case that overrides it entirely:
/// **the namespace's LAST claim takes the whole account's contents.**
///
/// ADR 0061 §8 — "when the namespace's last claim goes, the account goes
/// with its store." Removing the account from the accounts file (which
/// `gc_drop_nats` does a step later) makes every stream still in it
/// permanently unreachable, but NOT free: the files stay on the shared
/// JetStream PVC that every other namespace's `max_file` promise is
/// carved out of, with nothing left that could ever name them again. That
/// is a silent, unattributable capacity leak — the exact failure class
/// ADR 0061 §6's global budget exists to prevent on the allocation side.
///
/// Safe precisely because it is conditioned on `namespace_still_claimed`
/// being FALSE. Every attribution rule in [`nats_sweep_plan`] exists to
/// protect a NEIGHBOUR, and "no live jetstream claim in this namespace"
/// means there is no neighbour: no other application shares the account,
/// so nothing in it can belong to anyone else. `unattributed` comes back
/// empty rather than merely ignored — there is no one left to report it
/// to.
fn nats_sweep_for(
    streams: &[nats_client::StreamSummary],
    app: &str,
    declared: &[String],
    protected: &[String],
    namespace_still_claimed: bool,
) -> NatsSweep {
    if namespace_still_claimed {
        return nats_sweep_plan(streams, app, declared, protected);
    }
    let mut sweep: Vec<String> = streams.iter().map(|s| s.name.clone()).collect();
    sweep.sort();
    sweep.dedup();
    NatsSweep {
        sweep,
        unattributed: Vec::new(),
    }
}

/// NATS/jetstream reclaim (2.5e, ADR 0061 §8) — the arm whose absence
/// routed every jetstream snapshot into the CloudNativePG drop (see
/// [`gc_backend`]'s own doc and `reconcile::retained_claim_nats_object`'s).
///
/// Order matters and is not interchangeable:
///
/// 1. Delete this claim's NACK `Stream`/`Consumer` CRs. Idempotent and
///    404-tolerant — `reconcile::delete_nats_declared_objects` already
///    did this at claim-delete time, so normally every delete here is a
///    no-op; it runs anyway because a crash between the two is exactly
///    what a re-entrant GC is for.
/// 2. Sweep, as `mgr_<ns>`, while the account STILL EXISTS in the
///    accounts file. Step 4 removes it, so nothing NATS-side can be
///    reached after that point.
/// 3. Delete the connection Secret (belt-and-suspenders; it usually
///    cascaded on the original claim's ownerRef).
/// 4. Re-derive the whole accounts file from the live claim set. The
///    departing user's line disappears because the claim is gone, and —
///    when it was the namespace's LAST claim — so does the whole account
///    block. That is "drop the user, re-derive the file" and "when the
///    namespace's last claim goes, the account goes" in one write; there
///    is no separate account-deletion step because the file IS the
///    account.
///
/// Every NATS-side step is tolerated-on-failure (warn + proceed), the
/// same posture `gc_drop_dragonfly` takes: an unreachable or torn-down
/// server must not wedge the snapshot in the GC forever.
async fn gc_drop_nats(
    ctx: &Arc<Context>,
    rc: &RetainedClaim,
    rc_ns: &str,
    rc_name: &str,
) -> Result<Action, ReconcileError> {
    let claim_ns = rc.spec.claim_ref.namespace.clone();
    let nats_ns = rc
        .spec
        .nats_namespace
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_NATS_SYSTEM_NAMESPACE.to_string());
    let app = rc.spec.nats_app.clone().unwrap_or_default();
    let declared_streams = rc.spec.nats_declared_streams.clone().unwrap_or_default();
    let declared_consumers = rc.spec.nats_declared_consumers.clone().unwrap_or_default();

    // --- 1. the claim's own NACK CRs -----------------------------------
    if !app.is_empty() {
        let stream_api: Api<DynamicObject> =
            Api::namespaced_with(ctx.client.clone(), &nats_ns, &jetstream_stream_ar());
        for declared in &declared_streams {
            delete_nack_object(
                &stream_api,
                &format!("{claim_ns}-{app}-{declared}"),
                rc_name,
            )
            .await;
        }
        let consumer_api: Api<DynamicObject> =
            Api::namespaced_with(ctx.client.clone(), &nats_ns, &jetstream_consumer_ar());
        for declared in &declared_consumers {
            delete_nack_object(
                &consumer_api,
                &format!("{claim_ns}-{app}-{declared}"),
                rc_name,
            )
            .await;
        }
    }

    // --- 2. the stream sweep, as mgr_<ns> ------------------------------
    //
    // Every live jetstream claim, cluster-wide: used twice — to build the
    // `protected` set (streams DECLARED by this namespace's other claims,
    // which the sweep must never touch) and, below, to decide whether the
    // namespace still has any claim at all.
    let live_claims: Vec<ResourceClaim> = Api::<ResourceClaim>::all(ctx.client.clone())
        .list(&Default::default())
        .await?
        .items;
    let namespace_still_claimed = live_claims.iter().any(|c| {
        c.spec.type_ == "jetstream" && c.metadata.namespace.as_deref() == Some(claim_ns.as_str())
    });

    let mgr_user = nats_accounts::mgr_user(&claim_ns);
    let mgr_secret = nats::mgr_secret_name(&claim_ns);
    match read_secret_key(ctx, &nats_ns, &mgr_secret, "password").await {
        Err(err) => {
            // The management credential is gone (the namespace was torn
            // down, or the account never rendered) — there is nothing to
            // authenticate as, so there is nothing reachable to reclaim.
            warn!(
                retained = %rc_name, %nats_ns, secret = %mgr_secret, %err,
                "nats mgr Secret unreadable during GC — skipping the stream sweep, \
                 proceeding to Secret + accounts-file cleanup"
            );
        }
        Ok(mgr_pass) => {
            let url = format!("nats://{NATS_STATEFULSET_NAME}.{nats_ns}.svc:{NATS_CLIENT_PORT}");
            match ctx.nats.list_streams(&url, &mgr_user, &mgr_pass).await {
                Err(err) => warn!(
                    retained = %rc_name, %url, user = %mgr_user, %err,
                    "nats STREAM.LIST failed during GC — tolerating (server may be gone); \
                     no streams swept"
                ),
                Ok(streams) => {
                    // This claim's own declared streams, composed to their
                    // NATS-side names — the same derivation the accounts
                    // file and the Stream CR use, never a second copy.
                    let declared_nats: Vec<String> = declared_streams
                        .iter()
                        .map(|d| nats_accounts::nats_stream_name(&app, d))
                        .collect();
                    let protected = protected_stream_names(&live_claims, &claim_ns);
                    let plan = nats_sweep_for(
                        &streams,
                        &app,
                        &declared_nats,
                        &protected,
                        namespace_still_claimed,
                    );

                    for name in &plan.unattributed {
                        warn!(
                            retained = %rc_name, stream = %name, %app,
                            "nats GC: stream has subjects both inside and outside the \
                             departing application's prefix — reported as unattributed and \
                             LEFT IN PLACE (ADR 0061 §8: the rule keys on subjects, and is a \
                             heuristic in both directions)"
                        );
                    }
                    for name in &plan.sweep {
                        if let Err(err) = ctx
                            .nats
                            .delete_stream(&url, &mgr_user, &mgr_pass, name)
                            .await
                        {
                            warn!(
                                retained = %rc_name, stream = %name, %err,
                                "nats STREAM.DELETE failed during GC — tolerating"
                            );
                        } else {
                            info!(retained = %rc_name, stream = %name, "nats GC: stream deleted");
                        }
                    }
                }
            }
        }
    }

    // --- 3. the connection Secret --------------------------------------
    delete_connection_secret(ctx, rc).await?;

    // --- 4. re-derive the accounts file --------------------------------
    //
    // Drops the departed user's line, and — when this was the namespace's
    // last claim — the whole account block with it. Skipped (loudly) when
    // the matched provider is gone: the render needs that provider's own
    // `sizeBytes` map and `ceilingBytes` to compute EVERY OTHER
    // namespace's quota, and re-rendering the shared file with fabricated
    // defaults would silently rewrite quotas for tenants that have nothing
    // to do with this GC.
    match find_provider(&ctx.client, &rc.spec.provider).await? {
        None => warn!(
            retained = %rc_name, provider = %rc.spec.provider,
            "nats GC: matched ServiceProvider is gone — skipping the accounts-file \
             re-derive rather than re-rendering every other namespace's quota from defaults"
        ),
        Some(p) => {
            let cfg = p.spec.config.clone().unwrap_or_else(|| json!({}));
            let size_bytes = nats::size_bytes_map(&cfg);
            let ceiling_bytes = cfg
                .pointer("/ceilingBytes")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX);
            reconcile_accounts_secret(ctx, &nats_ns, ceiling_bytes, &size_bytes).await?;
        }
    }

    // The namespace's Account CR is NACK's connection configuration for an
    // account that no longer exists in the file — delete it, or NACK keeps
    // retrying a login that can never succeed. The `mgr_<ns>` password
    // Secret is deliberately KEPT (see `nats::mgr_secret_name`: the
    // management identity is not claim-scoped, and a future claim in the
    // same namespace re-renders the account with the same credential).
    if !namespace_still_claimed {
        let api: Api<DynamicObject> =
            Api::namespaced_with(ctx.client.clone(), &nats_ns, &jetstream_account_ar());
        delete_nack_object(&api, &nats::account_k8s_name(&claim_ns), rc_name).await;
    }

    // Terminal step: drop the snapshot.
    delete_retained_claim(&ctx.client, rc_ns, rc_name).await?;

    ctx.metrics
        .claim_gc_total
        .with_label_values(&["success", rc_ns])
        .inc();
    info!(retained = %rc_name, "nats RetainedClaim GC complete");

    Ok(Action::requeue(REQUEUE_AFTER))
}

/// Every NATS-side stream name DECLARED by a live jetstream claim in
/// `namespace` — the sweep's `protected` set. Pure over an
/// already-fetched claim list so the GC makes exactly one list call.
///
/// Reads `spec.jetstream.streams[].name` plus the claim's declaring
/// application (its `apprafter.io/managed-by` owner) through
/// `nats::declaring_app`, so the composed name matches what the accounts
/// file and the `Stream` CR use.
fn protected_stream_names(live_claims: &[ResourceClaim], namespace: &str) -> Vec<String> {
    let mut out = Vec::new();
    for c in live_claims {
        if c.metadata.namespace.as_deref() != Some(namespace) {
            continue;
        }
        let (Some(js), Some(app)) = (c.spec.jetstream.as_ref(), nats::declaring_app(c)) else {
            continue;
        };
        for s in &js.streams {
            out.push(nats_accounts::nats_stream_name(&app, &s.name));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 404-tolerant delete of one NACK CR. Never fails the GC: these objects
/// are normally already gone (the provisioner deletes them at claim-delete
/// time), and a NACK CRD that was never installed makes the whole Api call
/// fail with `NoKindMatch` rather than 404 — neither is a reason to wedge
/// a snapshot in the GC forever.
async fn delete_nack_object(api: &Api<DynamicObject>, object_name: &str, retained: &str) {
    match api.delete(object_name, &DeleteParams::default()).await {
        Ok(_) => info!(%retained, object = %object_name, "nats GC: NACK object deleted"),
        Err(kube::Error::Api(e)) if e.code == 404 => {}
        Err(e) => warn!(
            %retained, object = %object_name, error = %e,
            "nats GC: could not delete NACK object — tolerating"
        ),
    }
}

/// Delete the claim's connection Secret in its origin namespace. Shared by
/// the dragonfly and NATS drops (both snapshot `connectionSecretRef` +
/// `connectionSecretNamespace`); the CNPG path has its own password-Secret
/// step instead.
///
/// Swallows a 404 (it normally cascaded already via its ownerRef on the
/// original claim delete; this is the recovery-path belt-and-suspenders).
/// A snapshot missing the ref/namespace (a pre-provision delete) is a no-op.
async fn delete_connection_secret(
    ctx: &Arc<Context>,
    rc: &RetainedClaim,
) -> Result<(), ReconcileError> {
    let (Some(secret), Some(ns)) = (
        rc.spec.connection_secret_ref.as_deref(),
        rc.spec.connection_secret_namespace.as_deref(),
    ) else {
        return Ok(());
    };
    let api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), ns, &secret_ar());
    match api.delete(secret, &DeleteParams::default()).await {
        Ok(_) => {
            info!(secret = %secret, %ns, "claim connection Secret deleted");
            Ok(())
        }
        Err(kube::Error::Api(e)) if e.code == 404 => {
            info!(secret = %secret, "claim connection Secret already gone — delete no-op");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
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
    let cluster = rc.spec.cnpg_cluster.clone().unwrap_or_default();
    let cnpg_ns = rc.spec.cnpg_namespace.clone().unwrap_or_default();
    let cluster = cluster.as_str();
    let cnpg_ns = cnpg_ns.as_str();
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

/// The pure managed-roles transform [`set_role_absent`] applies: UPSERT
/// the role to `ensure: absent` (KEEP the entry). Named + pure so a
/// regression to pruning (the role-leak bug) is unit-catchable.
pub fn role_absent_upsert(existing: Vec<Value>, role: &str) -> Vec<Value> {
    cnpg::merge_role(existing, cnpg::managed_role_entry_absent(role))
}

/// Phase 1 (2.4f Fix B2): UPSERT the per-claim role to `ensure: absent`
/// in the shared Cluster's `spec.managed.roles` — CNPG then drops the
/// role (after the DB is dropped). The entry is KEPT, not pruned: pruning
/// only un-manages the role (the role-leak bug). Returns `false` when the
/// Cluster is gone (caller skips straight to finalize). 409-retried,
/// 404-tolerant via [`rmw_managed_roles`].
async fn set_role_absent(ctx: &Arc<Context>, rc: &RetainedClaim) -> Result<bool, ReconcileError> {
    let role = rc.spec.role.clone().unwrap_or_default();
    let landed = rmw_managed_roles(ctx, rc, |existing| role_absent_upsert(existing, &role)).await?;
    if landed {
        info!(%role, "role declared ensure:absent (CNPG drops it after the DB)");
    }
    Ok(landed)
}

/// Phase 3 (2.4f Fix B2): PRUNE the per-claim role entry from the shared
/// Cluster's `spec.managed.roles` — called ONLY after CNPG has confirmed
/// the drop (the role is in `byStatus.reconciled`), so the shared spec
/// accumulates no `ensure: absent` tombstones. 409-retried, 404-tolerant.
async fn remove_managed_role(ctx: &Arc<Context>, rc: &RetainedClaim) -> Result<(), ReconcileError> {
    let role = rc.spec.role.clone().unwrap_or_default();
    rmw_managed_roles(ctx, rc, |existing| cnpg::remove_role(existing, &role)).await?;
    Ok(())
}

/// Phase 2 (2.4f Fix B2): GET the shared CNPG Cluster as JSON (spec +
/// status, one consistent snapshot). `None` if the Cluster is gone (404).
/// A transient (non-404) error PROPAGATES — the GC must fail closed
/// (error_policy requeues), never silently treat a flaky GET as
/// "Cluster gone" and finalize a half-done drop.
async fn get_cluster(
    ctx: &Arc<Context>,
    rc: &RetainedClaim,
) -> Result<Option<Value>, ReconcileError> {
    let cluster = rc.spec.cnpg_cluster.clone().unwrap_or_default();
    let cnpg_ns = rc.spec.cnpg_namespace.clone().unwrap_or_default();
    let cluster = cluster.as_str();
    let cnpg_ns = cnpg_ns.as_str();
    let api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), cnpg_ns, &cluster_ar());
    match api.get(cluster).await {
        Ok(c) => Ok(Some(serde_json::to_value(&c)?)),
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
    let db_object = rc.spec.database_object_name.clone().unwrap_or_default();
    let cnpg_ns = rc.spec.cnpg_namespace.clone().unwrap_or_default();
    let db_object = db_object.as_str();
    let cnpg_ns = cnpg_ns.as_str();
    let api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), cnpg_ns, &database_ar());
    // Re-send the FULL Database body with `ensure: absent`. A partial SSA
    // apply (spec.ensure only) under this same field manager would STRIP the
    // cluster/name/owner this manager previously owned, leaving an incomplete
    // spec the apiserver rejects / CNPG cannot drop. Reconstruct from the
    // snapshot so ownership is preserved and the drop actually happens.
    let body = cnpg::database_object(
        db_object,
        cnpg_ns,
        rc.spec.cnpg_cluster.as_deref().unwrap_or_default(),
        rc.spec.database.as_deref().unwrap_or_default(),
        rc.spec.role.as_deref().unwrap_or_default(),
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
    let secret = rc.spec.password_secret_name.clone().unwrap_or_default();
    let cnpg_ns = rc.spec.cnpg_namespace.clone().unwrap_or_default();
    let secret = secret.as_str();
    let cnpg_ns = cnpg_ns.as_str();
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

/// Which backend's reclaim path a `RetainedClaim` routes to (2.6-7). The
/// snapshot's `spec.backend` mirrors the matched provider's `spec.backend`
/// (`cloudnative-pg` / `dragonfly`). Pure so the dispatch is unit-pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcBackend {
    /// The 2.4f phased CNPG drop (DB `ensure: absent` → role drop → prune).
    Cnpg,
    /// The 2.6 dragonfly drop (`FLUSHDB` + `ACL DELUSER` + Secret delete).
    Dragonfly,
    /// The 2.6b disk drop (delete the unowned RWO PVC + the snapshot).
    Disk,
    /// The 2.5e NATS/jetstream drop (ADR 0061 §8: sweep the app's streams
    /// as `mgr_<ns>`, delete its NACK CRs, drop the user, re-derive the
    /// accounts file, and take the account down with the last claim).
    Nats,
}

/// Route a snapshot's `spec.backend` to its reclaim path.
///
/// **Routed through [`Backend::from_spec_backend`] and matched
/// EXHAUSTIVELY, deliberately.** This function used to be
/// `match backend { "dragonfly" => …, "disk" => …, _ => Cnpg }`, and that
/// catch-all is how a jetstream snapshot (`backend: "nats"`) silently got
/// the CloudNativePG reclaim: `ensure: absent` written to a CNPG
/// `Database` named after the jetstream claim, a CNPG role RMW, both
/// aimed at `nats-system`, and nothing NATS-side reclaimed at all
/// (observed in the 2.5e walk). Nothing in the old shape distinguished
/// "deliberately defaults to CNPG" from "nobody added an arm". Going
/// through the `Backend` enum makes a NEW backend variant a COMPILE
/// error here rather than a silent CNPG routing — earlier than a test
/// could catch it, and impossible to miss.
///
/// The two cases that genuinely DO default to CNPG stay explicit:
/// `SharedDisk` (a reference claim that owns no backing and never
/// snapshots at all, so this is unreachable rather than meaningful) and
/// `None` — an unknown or empty `spec.backend`, which covers both legacy
/// pre-2.6 snapshots predating the multi-backend split and the
/// documented "type, not backend" case (`gc_backend("redis")`).
pub fn gc_backend(backend: &str) -> GcBackend {
    match Backend::from_spec_backend(backend) {
        Some(Backend::Dragonfly) => GcBackend::Dragonfly,
        Some(Backend::Disk) => GcBackend::Disk,
        Some(Backend::Nats) => GcBackend::Nats,
        Some(Backend::Cloudnativepg) | Some(Backend::SharedDisk) | None => GcBackend::Cnpg,
    }
}

/// The per-claim Dragonfly allocation a snapshot points at, when the claim
/// was actually provisioned. `None` for a snapshot with no instance/user
/// (a claim deleted before it ever reached an instance), which the GC
/// treats as "nothing to reclaim on a Dragonfly host".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragonflyReclaim {
    /// The shared pool instance the claim's DB lived on.
    pub instance: String,
    /// The numbered logical DB to `FLUSHDB` (`None` only if the snapshot is
    /// malformed — has an instance + user but no dbnum; the user is still
    /// `DELUSER`'d, the flush is skipped).
    pub dbnum: Option<u16>,
    /// The `$N`-pinned ACL username to `DELUSER`.
    pub acl_user: String,
}

/// Pure: what the dragonfly GC must reclaim on an instance for this
/// snapshot, or `None` when there is no instance to reach (the claim was
/// deleted before the provisioner wrote `status.instance`, so the snapshot's
/// `instance` is empty — no DB/user was ever created on a host). The snapshot
/// writer always sets the deterministic `aclUser` + connection-Secret refs,
/// so `instance` is the gating field; the `acl_user` guard below is
/// belt-and-suspenders for a malformed snapshot. Pinned so the "pre-provision
/// delete → skip the Redis ops" tolerance is a unit-catchable regression
/// guard (mirrors the snapshot writer's pre-allocation branch).
pub fn dragonfly_reclaim_target(rc: &RetainedClaim) -> Option<DragonflyReclaim> {
    let instance = rc.spec.instance.clone().filter(|s| !s.is_empty())?;
    let acl_user = rc.spec.acl_user.clone().filter(|s| !s.is_empty())?;
    Some(DragonflyReclaim {
        instance,
        dbnum: rc.spec.dbnum,
        acl_user,
    })
}

/// Defensive live-guard for the dragonfly grace-GC `FLUSHDB` (2.6 Fix #2b,
/// ADR 0042 §8). Returns `false` (SKIP the destructive flush) iff some
/// OTHER live `ResourceClaim` currently holds the SAME `(instance, dbnum)`
/// the snapshot points at — i.e. the freed dbnum was recycled to a new
/// tenant before this snapshot's grace elapsed. Flushing then would wipe
/// the new tenant's data (cross-tenant loss).
///
/// "Other" means `(name, namespace) != (snap_claim_name, snap_claim_ns)`:
/// the snapshot's OWN origin claim being back is a recovery the
/// reconcile-level live-guard already handles, and its data IS the
/// snapshot's data, so that case is `true` (safe). Any DIFFERENT claim on
/// the same `(instance, dbnum)` is the recycle hazard → `false`.
///
/// This is belt-and-suspenders: the allocator now RESERVES retained dbnums
/// ([`dragonfly::used_dbnums`]), so a recycle should not happen — but if a
/// snapshot were ever mis-created or the reservation regressed, this guard
/// prevents the data-loss flush. The caller still runs `ACL DELUSER` + the
/// Secret/snapshot delete when this returns `false` (per-claim usernames
/// differ, so dropping the dead user is always safe).
pub fn dragonfly_flushdb_is_safe(
    live: &[ResourceClaim],
    snap_instance: &str,
    snap_dbnum: u16,
    snap_claim_name: &str,
    snap_claim_ns: &str,
) -> bool {
    !live.iter().any(|c| {
        let st = match c.status.as_ref() {
            Some(st) => st,
            None => return false,
        };
        st.instance.as_deref() == Some(snap_instance)
            && st.dbnum == Some(snap_dbnum)
            && (c.name_any().as_str() != snap_claim_name
                || c.namespace().as_deref() != Some(snap_claim_ns))
    })
}

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

/// The `ensure` value of the named role's entry in the Cluster's
/// `spec.managed.roles` (`None` if the entry is absent or carries no
/// `ensure`). Drives the 2.4f Fix B2 "already-declared-absent" gate: only
/// once OUR entry reads `ensure: absent` (a PRIOR pass set it, so CNPG has
/// had >=1 requeue [`ROLE_DROP_REQUEUE`] to reconcile it) do we trust
/// `byStatus` — avoiding the stale-status premature-prune that re-leaks
/// the role. Right after the `ensure: absent` PUT, `byStatus.reconciled`
/// still reflects the prior `present`-and-matching state; pruning on that
/// stale read is the role-leak bug, relocated.
pub fn role_entry_ensure<'a>(cluster: &'a Value, role: &str) -> Option<&'a str> {
    cluster
        .pointer("/spec/managed/roles")
        .and_then(Value::as_array)?
        .iter()
        .find(|r| r.get("name").and_then(Value::as_str) == Some(role))
        .and_then(|r| r.get("ensure").and_then(Value::as_str))
}

/// The CNPG `cannotReconcile` error reason(s) for a role (joined with
/// `; `), if any — used to surface a WEDGED drop (e.g. the role still
/// owns a database with live connections) instead of looping silently
/// forever (finding #4). `None` when the role has no `cannotReconcile`
/// entry, or its entry is an empty array.
pub fn role_cannot_reconcile_reason(managed_roles_status: &Value, role: &str) -> Option<String> {
    let arr = managed_roles_status
        .pointer(&format!("/cannotReconcile/{role}"))
        .and_then(Value::as_array)?;
    let joined = arr
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("; ");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

#[cfg(test)]
mod tests {

    // ---- D23: a data-derived requeue must never reach the scheduler raw ----

    #[test]
    fn a_far_future_retain_until_is_clamped_rather_than_scheduled() {
        // kube-runtime schedules through a tokio DelayQueue, which PANICS on a
        // deadline it cannot represent — and that panic takes the whole
        // operator down, not just this reconcile. A `RetainedClaim` dated 2031
        // produced a 4.3-year requeue and crash-looped every controller in the
        // process. Reproduced by the 2.22 battery with exactly that fixture.
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let far = "2031-01-01T00:00:00Z";
        let raw = crate::grace::remaining_grace(
            chrono::DateTime::parse_from_rfc3339(far)
                .unwrap()
                .with_timezone(&chrono::Utc),
            now,
        );
        assert!(
            raw > MAX_REQUEUE,
            "fixture must exceed the ceiling or it proves nothing"
        );
        assert_eq!(
            clamp_requeue(raw),
            MAX_REQUEUE,
            "an unrepresentable deadline must be capped by PRODUCTION code, not by the test"
        );
    }

    #[test]
    fn a_near_deadline_still_gets_its_floor() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let soon = chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:05+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            clamp_requeue(crate::grace::remaining_grace(soon, now)),
            MIN_REQUEUE,
            "the floor must still apply"
        );
    }
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

    // --- role_entry_ensure() (2.4f Fix B2 already-declared-absent gate) ---

    /// Build a Cluster JSON whose `spec.managed.roles` carries the given
    /// `(name, ensure)` entries.
    fn cluster_with_roles(roles: &[(&str, &str)]) -> Value {
        let entries: Vec<Value> = roles
            .iter()
            .map(|(name, ensure)| json!({ "name": name, "ensure": ensure }))
            .collect();
        json!({ "spec": { "managed": { "roles": entries } } })
    }

    #[test]
    fn role_entry_ensure_present_when_entry_present() {
        let cluster = cluster_with_roles(&[("claim_demo_web", "present"), ("other", "present")]);
        assert_eq!(
            role_entry_ensure(&cluster, "claim_demo_web"),
            Some("present")
        );
    }

    #[test]
    fn role_entry_ensure_absent_when_entry_absent() {
        let cluster = cluster_with_roles(&[("claim_demo_web", "absent")]);
        assert_eq!(
            role_entry_ensure(&cluster, "claim_demo_web"),
            Some("absent")
        );
    }

    #[test]
    fn role_entry_ensure_none_when_entry_missing() {
        // Our role has no entry at all → the gate treats it as "not yet
        // declared absent" → declare + wait.
        let cluster = cluster_with_roles(&[("someone-else", "present")]);
        assert_eq!(role_entry_ensure(&cluster, "claim_demo_web"), None);
    }

    #[test]
    fn role_entry_ensure_none_when_entry_has_no_ensure() {
        // An entry without an `ensure` field (CNPG defaults present) reads
        // as None → the gate forces the declare+wait branch, never trusting
        // status prematurely.
        let cluster = json!({
            "spec": { "managed": { "roles": [{ "name": "claim_demo_web", "login": true }] } }
        });
        assert_eq!(role_entry_ensure(&cluster, "claim_demo_web"), None);
    }

    #[test]
    fn role_entry_ensure_none_when_no_roles_array() {
        assert_eq!(role_entry_ensure(&json!({}), "claim_demo_web"), None);
        assert_eq!(
            role_entry_ensure(&json!({ "spec": { "managed": {} } }), "claim_demo_web"),
            None
        );
    }

    // --- the stale-`reconciled` premature-prune temporal regression ---

    #[test]
    fn entry_ensure_gate_holds_prune_while_reconciled_is_stale() {
        // THE critical guard for 2.4f Fix B2 finding #2. Right after the
        // `ensure: absent` PUT, CNPG has NOT reconciled the new spec yet, so
        // `status.managedRolesStatus.byStatus.reconciled` STILL lists the
        // role (left over from its prior `ensure: present` state). If the GC
        // trusted that read it would prune on pass 1 and the Postgres role
        // would NEVER be dropped — the role-leak bug B, relocated.
        //
        // reconcile is async, so we assert at the decision seam: the
        // entry-ensure gate must read `present` (forcing the declare+wait
        // branch) EVEN THOUGH `role_is_dropped` reads true off the stale
        // status. The gate, not the status, holds the prune.
        let cluster = json!({
            "spec": { "managed": { "roles": [
                { "name": "claim_demo_web", "ensure": "present" }
            ] } },
            "status": { "managedRolesStatus": {
                "byStatus": { "reconciled": ["claim_demo_web"] }
            } },
        });
        let status = cluster
            .pointer("/status/managedRolesStatus")
            .cloned()
            .unwrap();

        // Stale `reconciled` would say "dropped" — the trap.
        assert!(
            role_is_dropped(&status, "claim_demo_web"),
            "fixture: status is stale-true (the trap the gate must resist)"
        );
        // But the spec entry is still `present`, so the gate forces the
        // declare+wait branch — the prune is held until a LATER pass sees
        // the entry already-absent (CNPG has had >=1 requeue to reconcile).
        assert_eq!(
            role_entry_ensure(&cluster, "claim_demo_web"),
            Some("present"),
            "the gate must hold the prune until the entry reads ensure:absent"
        );
    }

    // --- role_absent_upsert() (pinned set_role_absent transform) ---

    #[test]
    fn role_absent_upsert_replaces_present_with_absent() {
        // Pins the exact transform `set_role_absent` applies. A regression
        // to `remove_role` (prune) — the role-leak bug — is unit-catchable
        // here: the target must SURVIVE as `ensure: absent`, not vanish.
        let existing = vec![
            cnpg::managed_role_entry("r", "r-pw"),
            json!({ "name": "keep-me", "login": false }),
        ];
        let out = role_absent_upsert(existing, "r");
        assert_eq!(
            out.len(),
            2,
            "foreign entry preserved, target replaced not duplicated"
        );
        let target = out
            .iter()
            .find(|e| e["name"] == "r")
            .expect("target role still present (declared absent, not pruned)");
        assert_eq!(target["ensure"], "absent");
        assert!(
            out.iter().any(|e| e["name"] == "keep-me"),
            "foreign role preserved"
        );
    }

    // --- role_cannot_reconcile_reason() (wedged-drop observability) ---

    #[test]
    fn role_cannot_reconcile_reason_present_joins_messages() {
        let status = json!({
            "cannotReconcile": {
                "claim_demo_web": [
                    "could not perform DELETE on role: owner of database claim_demo_web",
                    "second reason",
                ],
            },
        });
        assert_eq!(
            role_cannot_reconcile_reason(&status, "claim_demo_web").as_deref(),
            Some(
                "could not perform DELETE on role: owner of database claim_demo_web; second reason"
            )
        );
    }

    #[test]
    fn role_cannot_reconcile_reason_none_when_role_absent() {
        let status = json!({
            "cannotReconcile": { "some-other-role": ["nope"] },
        });
        assert_eq!(
            role_cannot_reconcile_reason(&status, "claim_demo_web"),
            None
        );
        // No cannotReconcile map at all.
        assert_eq!(
            role_cannot_reconcile_reason(&json!({}), "claim_demo_web"),
            None
        );
    }

    #[test]
    fn role_cannot_reconcile_reason_none_when_empty_array() {
        // An empty reason array is not a wedge — treat as None.
        let status = json!({ "cannotReconcile": { "claim_demo_web": [] } });
        assert_eq!(
            role_cannot_reconcile_reason(&status, "claim_demo_web"),
            None
        );
    }

    // --- gc_backend() (2.6-7 backend dispatch) ---

    #[test]
    fn gc_dispatch_selects_backend() {
        assert_eq!(gc_backend("dragonfly"), GcBackend::Dragonfly);
        assert_eq!(gc_backend("cloudnative-pg"), GcBackend::Cnpg);
        // 2.6b: a disk snapshot routes to the disk drop (delete the PVC),
        // NOT the CNPG phased role/DB drop. Without this arm a disk snapshot
        // would fall through to CNPG and the PVC would leak forever.
        assert_eq!(gc_backend("disk"), GcBackend::Disk);
        // Legacy / empty snapshots predate the multi-backend split and
        // carry the CNPG shape — default to the CNPG drop so they are never
        // mis-routed to the (no-op-on-CNPG-fields) dragonfly path.
        assert_eq!(gc_backend(""), GcBackend::Cnpg);
        assert_eq!(gc_backend("redis"), GcBackend::Cnpg); // type, not backend
                                                          // 2.5e (walk-found): a jetstream snapshot routes to the NATS drop.
                                                          // Before this arm existed it hit the `_ => Cnpg` catch-all and got
                                                          // the CloudNativePG reclaim — `ensure: absent` written to a CNPG
                                                          // `Database` named after the jetstream claim, and a CNPG role RMW,
                                                          // both aimed at `nats-system`. Nothing NATS-side was reclaimed.
        assert_eq!(gc_backend("nats"), GcBackend::Nats);
    }

    #[test]
    fn gc_dispatch_is_exhaustive_over_every_known_backend() {
        // The structural half of the fix, and the reason `gc_backend` now
        // routes through `Backend::from_spec_backend` instead of matching
        // raw strings with a `_` arm: a NEW `Backend` variant is a COMPILE
        // error in `gc_backend`, not a silent CNPG routing. This test
        // pins the OTHER direction — that every variant that exists today
        // is genuinely reachable from a `spec.backend` string, so the
        // exhaustive match is over real inputs rather than over an enum
        // nothing produces.
        for (spec_backend, expected) in [
            ("cloudnative-pg", GcBackend::Cnpg),
            ("dragonfly", GcBackend::Dragonfly),
            ("disk", GcBackend::Disk),
            ("nats", GcBackend::Nats),
            // `shared-disk` reference claims never snapshot at all
            // (`snapshot_retained_claim` returns early), so this routing
            // is unreachable rather than meaningful — pinned so a future
            // reader does not mistake it for a decision.
            ("shared-disk", GcBackend::Cnpg),
        ] {
            assert_eq!(
                gc_backend(spec_backend),
                expected,
                "{spec_backend} routed wrongly"
            );
        }
    }

    // --- nats_sweep_plan (2.5e, ADR 0061 §8) --------------------------

    fn stream(name: &str, subjects: &[&str]) -> nats_client::StreamSummary {
        nats_client::StreamSummary {
            name: name.to_string(),
            subjects: subjects.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn the_sweep_deletes_the_departing_apps_own_declared_streams() {
        let streams = vec![stream("feeder_blocks", &["feeder.blocks.>"])];
        let plan = nats_sweep_plan(&streams, "feeder", &["feeder_blocks".into()], &[]);
        assert_eq!(plan.sweep, vec!["feeder_blocks".to_string()]);
        assert!(plan.unattributed.is_empty());
    }

    #[test]
    fn the_sweep_deletes_a_dynamic_stream_wholly_under_the_apps_prefix() {
        let streams = vec![stream("whatever-name", &["feeder.a", "feeder.b.>"])];
        let plan = nats_sweep_plan(&streams, "feeder", &[], &[]);
        assert_eq!(
            plan.sweep,
            vec!["whatever-name".to_string()],
            "the rule keys on SUBJECTS — the name is unconstrained and \
             carries no ownership information at all"
        );
    }

    #[test]
    fn the_sweep_leaves_a_mixed_subject_stream_and_reports_it() {
        // ADR 0061 §8: "leaving mixed-subject streams reported as
        // unattributed". This is the half that must NOT be deleted — a
        // stream capturing a neighbour's subjects alongside ours is not
        // ours to remove.
        let streams = vec![stream("capture", &["feeder.a", "indexer.b"])];
        let plan = nats_sweep_plan(&streams, "feeder", &[], &[]);
        assert!(plan.sweep.is_empty(), "{plan:?}");
        assert_eq!(plan.unattributed, vec!["capture".to_string()]);
    }

    #[test]
    fn the_sweep_ignores_a_stream_with_no_subject_of_ours_at_all() {
        // Not ours in any sense — leave it and do NOT report it, or every
        // GC in a busy namespace logs every neighbour's stream.
        let streams = vec![stream("neighbour", &["indexer.a", "indexer.b"])];
        let plan = nats_sweep_plan(&streams, "feeder", &[], &[]);
        assert_eq!(plan, NatsSweep::default());
    }

    #[test]
    fn the_sweep_never_touches_a_neighbours_declared_stream() {
        // "excluding declared streams, whatever their owner" — and the
        // case that makes it load-bearing: a neighbour DECLARED a stream
        // whose subjects happen to sit wholly under the departing app's
        // prefix (one-sided fan-in, which ADR 0061 explicitly supports).
        // The subject rule alone would sweep it; `protected` is what
        // stops that.
        let streams = vec![stream("indexer_fanin", &["feeder.blocks.>"])];
        let plan = nats_sweep_plan(&streams, "feeder", &[], &["indexer_fanin".into()]);
        assert_eq!(plan, NatsSweep::default());
    }

    #[test]
    fn the_sweep_does_not_confuse_apps_whose_names_share_a_prefix() {
        // `feeder.` does not prefix `feederbot.orders`. Without the
        // separating dot this test is the only thing between a
        // `feeder` deletion and `feederbot`'s data.
        let streams = vec![stream("bot", &["feederbot.orders.>"])];
        let plan = nats_sweep_plan(&streams, "feeder", &[], &[]);
        assert_eq!(plan, NatsSweep::default());
    }

    #[test]
    fn a_subjectless_stream_is_reported_never_swept() {
        // A mirror or a sourced stream has no subjects of its own. The
        // rule keys on subjects, so it has no evidence — and "no
        // evidence" must not collapse to "wholly under our prefix" the
        // way a naive `.all()` over an empty slice would.
        let streams = vec![stream("mirror", &[])];
        let plan = nats_sweep_plan(&streams, "feeder", &[], &[]);
        assert!(plan.sweep.is_empty(), "{plan:?}");
        assert_eq!(plan.unattributed, vec!["mirror".to_string()]);
    }

    #[test]
    fn an_empty_app_disables_the_dynamic_sweep_entirely() {
        // A claim deleted after its declaring Application ownerReference
        // was already gone snapshots with an empty `natsApp`. The prefix
        // would then be a bare "." — matching nothing useful, but the
        // failure mode of guessing is deleting another tenant's data, so
        // the sweep is skipped and only explicitly-declared streams go.
        let streams = vec![
            stream("someone", &[".odd.subject"]),
            stream("declared", &["x.y"]),
        ];
        let plan = nats_sweep_plan(&streams, "", &["declared".into()], &[]);
        assert_eq!(plan.sweep, vec!["declared".to_string()]);
        assert!(plan.unattributed.is_empty(), "{plan:?}");
    }

    #[test]
    fn the_last_claim_in_a_namespace_takes_the_whole_account_with_it() {
        // ADR 0061 §8: "the account goes with its store." Once the
        // account is removed from the accounts file, anything left in it
        // is unreachable forever but still occupies the shared JetStream
        // PVC — a silent capacity leak with no owner left to attribute it
        // to. So the last claim sweeps EVERYTHING, including streams the
        // per-application rules would have left alone: a mixed-subject
        // stream, and one with no subject of the departing app's at all.
        let streams = vec![
            stream("capture", &["feeder.a", "indexer.b"]),
            stream("orphan", &["indexer.only"]),
            stream("feeder_blocks", &["feeder.blocks.>"]),
        ];
        let plan = nats_sweep_for(&streams, "feeder", &["feeder_blocks".into()], &[], false);
        assert_eq!(
            plan.sweep,
            vec![
                "capture".to_string(),
                "feeder_blocks".to_string(),
                "orphan".to_string()
            ]
        );
        assert!(
            plan.unattributed.is_empty(),
            "with no live claim left in the namespace there is no one to \
             report an unattributed stream TO: {plan:?}"
        );
    }

    #[test]
    fn a_namespace_that_still_has_a_claim_keeps_the_per_application_rules() {
        // The other half, and the one that makes the override safe: while
        // ANY live jetstream claim remains in the namespace, a neighbour
        // may own what is left, so the attribution rules apply unchanged.
        // Same fixture as above — every assertion here is the opposite of
        // the one above, which is the point.
        let streams = vec![
            stream("capture", &["feeder.a", "indexer.b"]),
            stream("orphan", &["indexer.only"]),
            stream("feeder_blocks", &["feeder.blocks.>"]),
        ];
        let plan = nats_sweep_for(&streams, "feeder", &["feeder_blocks".into()], &[], true);
        assert_eq!(plan.sweep, vec!["feeder_blocks".to_string()]);
        assert_eq!(plan.unattributed, vec!["capture".to_string()]);
    }

    #[test]
    fn protected_stream_names_composes_only_this_namespaces_declarations() {
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
        use operator_core::{JetStreamStream, ResourceClaimJetStream, ResourceClaimSpec};
        let mk = |ns: &str, claim: &str, app: &str, stream: &str| ResourceClaim {
            metadata: ObjectMeta {
                name: Some(claim.to_string()),
                namespace: Some(ns.to_string()),
                owner_references: Some(vec![OwnerReference {
                    api_version: "apprafter.io/v1alpha1".into(),
                    kind: "Application".into(),
                    name: app.into(),
                    uid: "u".into(),
                    controller: Some(true),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            spec: ResourceClaimSpec {
                type_: "jetstream".into(),
                jetstream: Some(ResourceClaimJetStream {
                    streams: vec![JetStreamStream {
                        name: stream.into(),
                        subjects: vec![format!("{app}.x")],
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
            status: None,
        };
        let claims = vec![
            mk("demo", "feeder-jetstream", "feeder", "blocks"),
            mk("demo", "indexer-jetstream", "indexer", "fanin"),
            mk("other", "solo-jetstream", "solo", "elsewhere"),
        ];
        assert_eq!(
            protected_stream_names(&claims, "demo"),
            vec!["feeder_blocks".to_string(), "indexer_fanin".to_string()],
            "another NAMESPACE's declarations are a different account \
             entirely and must not leak into this one's protected set"
        );
    }

    // --- disk GC live-guard (2.6b-5 / 2.4f Fix A) ---

    #[test]
    fn claim_is_live_guards_a_reprovisioned_disk_claim() {
        // 2.6b-5: the disk GC drop (delete the PVC) MUST NOT fire when the
        // source disk claim is back (a re-deploy within grace reattaches to
        // the SAME PVC via idempotent SSA — disk data survives). The
        // reconcile-level live-guard (`claim_is_live`) runs BEFORE the
        // backend dispatch, so it covers disk identically to CNPG/dragonfly:
        // a live disk claim → the snapshot is stale → delete the snapshot,
        // never delete the PVC. Pin that a live disk claim reads live (skip
        // the drop) and a mid-deletion one does not (its own fresh snapshot
        // will supersede). The live PVC-delete I/O itself is e2e-covered.
        let live_disk = ResourceClaim::new("web-disk-data", ResourceClaimSpec::default());
        assert!(
            claim_is_live(&live_disk),
            "a re-provisioned disk claim is live — the GC must skip the PVC delete"
        );

        let mut deleting_disk = ResourceClaim::new("web-disk-data", ResourceClaimSpec::default());
        deleting_disk.metadata.deletion_timestamp = Some(Time(Utc::now()));
        assert!(
            !claim_is_live(&deleting_disk),
            "a mid-deletion disk claim is not live — it produces its own fresh snapshot"
        );
    }

    // --- dragonfly_reclaim_target() (2.6-7 pre-provision-delete tolerance) ---

    /// Build a dragonfly-shaped `RetainedClaim` with the given allocation.
    fn dragonfly_snapshot(
        instance: Option<&str>,
        dbnum: Option<u16>,
        acl_user: Option<&str>,
    ) -> RetainedClaim {
        RetainedClaim::new(
            "claim-demo-web-redis",
            operator_core::RetainedClaimSpec {
                claim_ref: operator_core::retainedclaim::ClaimRef {
                    name: "web-redis".into(),
                    namespace: "demo".into(),
                },
                provider: "redis-integrated".into(),
                backend: "dragonfly".into(),
                instance: instance.map(str::to_owned),
                dbnum,
                acl_user: acl_user.map(str::to_owned),
                connection_secret_ref: Some("web-redis-conn".into()),
                connection_secret_namespace: Some("demo".into()),
                retain_until: "2026-06-12T00:00:00+00:00".into(),
                ..Default::default()
            },
        )
    }

    #[test]
    fn dragonfly_reclaim_target_present_for_a_provisioned_claim() {
        let rc = dragonfly_snapshot(
            Some("platform-redis-ephemeral-000"),
            Some(7),
            Some("claim_demo_web-redis_redis"),
        );
        assert_eq!(
            dragonfly_reclaim_target(&rc),
            Some(DragonflyReclaim {
                instance: "platform-redis-ephemeral-000".into(),
                dbnum: Some(7),
                acl_user: "claim_demo_web-redis_redis".into(),
            })
        );
    }

    #[test]
    fn dragonfly_reclaim_target_none_when_no_allocation() {
        // A claim deleted BEFORE it was ever provisioned: the snapshot
        // carries no instance/user, so there is nothing on a Dragonfly host
        // to reclaim — the GC must skip the Redis ops (and not blow up
        // trying to reach an instance that never existed). The Secret +
        // snapshot cleanup still runs.
        assert_eq!(
            dragonfly_reclaim_target(&dragonfly_snapshot(None, None, None)),
            None
        );
        // Partial snapshots (instance but no user, or vice versa) are also
        // treated as "nothing to reclaim" — both are required to act.
        assert_eq!(
            dragonfly_reclaim_target(&dragonfly_snapshot(
                Some("platform-redis-ephemeral-000"),
                Some(7),
                None
            )),
            None
        );
        assert_eq!(
            dragonfly_reclaim_target(&dragonfly_snapshot(None, Some(7), Some("u"))),
            None
        );
        // An empty-string instance/user is equivalent to absent.
        assert_eq!(
            dragonfly_reclaim_target(&dragonfly_snapshot(Some(""), Some(7), Some("u"))),
            None
        );
    }

    // --- dragonfly_flushdb_is_safe() (2.6 Fix #2b GC live-guard) ---

    fn live_redis_claim(name: &str, ns: &str, instance: &str, dbnum: u16) -> ResourceClaim {
        let mut c = ResourceClaim::new(name, ResourceClaimSpec::default());
        c.metadata.namespace = Some(ns.to_owned());
        c.status = Some(operator_core::ResourceClaimStatus {
            instance: Some(instance.to_owned()),
            dbnum: Some(dbnum),
            ..Default::default()
        });
        c
    }

    #[test]
    fn flushdb_safe_when_no_live_claim_on_dbnum() {
        // No live claim holds this (instance, dbnum) — the snapshot owns it,
        // so FLUSHDB is safe.
        let live = vec![live_redis_claim(
            "other",
            "demo",
            "platform-redis-ephemeral-000",
            3,
        )];
        assert!(dragonfly_flushdb_is_safe(
            &live,
            "platform-redis-ephemeral-000",
            7,
            "web-redis",
            "demo",
        ));
    }

    #[test]
    fn flushdb_safe_when_owner_is_the_snapshots_own_claim() {
        // The only live claim on (instance, dbnum) is the snapshot's OWN
        // origin claim (same name+ns) — a recovery the live-guard already
        // handles; the flush decision itself treats it as safe (it is the
        // snapshot's own data). The reconcile-level live-guard is what skips
        // the destructive drop in that case.
        let live = vec![live_redis_claim(
            "web-redis",
            "demo",
            "platform-redis-ephemeral-000",
            7,
        )];
        assert!(dragonfly_flushdb_is_safe(
            &live,
            "platform-redis-ephemeral-000",
            7,
            "web-redis",
            "demo",
        ));
    }

    #[test]
    fn flushdb_not_safe_when_different_claim_recycled_the_dbnum() {
        // A DIFFERENT live tenant now holds the same (instance, dbnum) — the
        // freed number was recycled. FLUSHDB here would wipe the new tenant's
        // data (cross-tenant data loss). The guard must return false.
        let live = vec![live_redis_claim(
            "new-tenant",
            "other-ns",
            "platform-redis-ephemeral-000",
            7,
        )];
        assert!(!dragonfly_flushdb_is_safe(
            &live,
            "platform-redis-ephemeral-000",
            7,
            "web-redis",
            "demo",
        ));
    }

    #[test]
    fn flushdb_safe_when_a_live_claim_has_no_status() {
        // A live claim with no `status` (None — newly created, not yet
        // provisioned) carries no (instance, dbnum), so it can NOT be the
        // recycler of this snapshot's DB. The guard's per-claim closure
        // returns `false` for it (not-a-conflict), so the FLUSHDB stays safe.
        // Guards the `None => return false` branch in `dragonfly_flushdb_is_safe`.
        let mut c = ResourceClaim::new("new-claim", ResourceClaimSpec::default());
        c.metadata.namespace = Some("demo".to_owned());
        c.status = None;
        let live = vec![c];
        assert!(dragonfly_flushdb_is_safe(
            &live,
            "platform-redis-ephemeral-000",
            7,
            "web-redis",
            "demo",
        ));
    }

    #[test]
    fn flushdb_safe_when_recycler_is_on_a_different_instance() {
        // Same dbnum but a DIFFERENT instance is a different DB — not a
        // conflict.
        let live = vec![live_redis_claim(
            "new-tenant",
            "other-ns",
            "platform-redis-ephemeral-001",
            7,
        )];
        assert!(dragonfly_flushdb_is_safe(
            &live,
            "platform-redis-ephemeral-000",
            7,
            "web-redis",
            "demo",
        ));
    }
}
