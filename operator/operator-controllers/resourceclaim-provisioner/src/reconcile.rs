// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Reconcile loop for the `ResourceClaim` provisioner (Phase 2.4c).
//!
//! The I/O orchestration is thin; the decision points are pure functions
//! (`should_provision`, the `Backend` dispatch, `ready_condition`,
//! `connection_secret_object`) unit-tested without a cluster. The actual
//! lazy-Cluster / role / database / Secret apply path is exercised by the
//! gated real-cluster smoke (`tests/cluster_smoke_test.rs`) and the 2.4g
//! manual walk.
//!
//! ## SSA field-manager split (CRITICAL)
//!
//! The status patch this controller sends contains ONLY `status.ready`,
//! `status.connectionSecretRef`, and the `Ready` condition, under field
//! manager [`crate::FIELD_MANAGER`]. It NEVER includes `status.provider`
//! or a `Scheduled` condition — those are owned by the scheduler, and
//! patching them would make the two controllers fight.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use chrono::Utc;
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, Patch, PatchParams};
use kube::core::GroupVersionKind;
use kube::runtime::controller::Action;
use kube::{Client, ResourceExt};
use rand::distributions::Alphanumeric;
use rand::Rng;
use serde_json::{json, Value};
use tracing::{info, warn};

use operator_core::{ResourceClaim, ResourceClaimCondition, RetainedClaim, ServiceProvider};

use crate::cnpg;
use crate::dragonfly;
use crate::grace;
use crate::{Context, ReconcileError, FIELD_MANAGER, KIND, PROVISIONER_FINALIZER};

/// Condition type this controller owns. The scheduler owns `Scheduled`;
/// this controller owns ONLY `Ready`.
const COND_READY: &str = "Ready";

/// Namespace the finalizer snapshots a deleted claim's `RetainedClaim`
/// into. A platform namespace that outlives tenant namespaces, so the
/// 7-day-grace GC always fires even if the app's own namespace is torn
/// down (Phase 2.4f, decision 1). The lineage is preserved in
/// `spec.claimRef`.
const RETAINED_CLAIM_NAMESPACE: &str = "apprafter-system";

/// CNPG cluster + namespace the finalizer snapshot falls back to when
/// the matched provider config is missing them. Mirrors the
/// provisioning defaults so a snapshot never stalls on a config miss.
const DEFAULT_CNPG_CLUSTER: &str = "platform-postgres";
const DEFAULT_CNPG_NAMESPACE: &str = "cnpg-system";

/// Condition type the scheduler writes — read-only here.
const COND_SCHEDULED: &str = "Scheduled";

/// Length of the generated role password (alphanumeric).
const PASSWORD_LEN: usize = 32;

/// How many times to retry the read-modify-write of the shared Cluster's
/// unkeyed `spec.managed.roles` list when the GET→replace races another
/// claim's provisioner pass (HTTP 409 Conflict).
const ROLE_RMW_RETRIES: usize = 5;

// ---------------------------------------------------------------------------
// Backend dispatch
// ---------------------------------------------------------------------------

/// The provisioning backends this controller knows how to drive.
/// `cloudnative-pg` (2.4) and `dragonfly` (2.6) are wired; 2.5 adds
/// jetstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cloudnativepg,
    Dragonfly,
}

impl Backend {
    /// Map a `ServiceProvider.spec.backend` string to a known backend.
    /// Unknown backends return `None` (a future controller may handle
    /// them; this one requeues).
    pub fn from_spec_backend(backend: &str) -> Option<Self> {
        match backend {
            "cloudnative-pg" => Some(Backend::Cloudnativepg),
            "dragonfly" => Some(Backend::Dragonfly),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Public reconcile + error_policy
// ---------------------------------------------------------------------------

/// Reconcile a single `ResourceClaim`:
///
/// 1. On delete (`deletion_timestamp` set) → snapshot the claim into an
///    immutable `RetainedClaim` in `apprafter-system` (retainUntil =
///    deletion + 7d) BEFORE dropping the provisioner finalizer, then
///    await change. The 7-day-grace GC controller drops the role/DB/
///    Secret later. The connection Secret cascades via its ownerRef.
/// 2. Ensure the provisioner finalizer is present so deletes are seen.
/// 3. If the scheduler hasn't marked the claim `Scheduled=True` with a
///    provider yet (or it's already `ready`) → requeue 60s.
/// 4. Read the matched `ServiceProvider` and dispatch on `spec.backend`.
/// 5. For `cloudnative-pg`: lazy-apply the shared Cluster, generate a
///    role + password, RMW the Cluster's managed roles, apply the
///    Database + the connection Secret, and write
///    `status.ready` / `status.connectionSecretRef` / `Ready=True`.
pub async fn reconcile(
    claim: Arc<ResourceClaim>,
    ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    let ns = claim.namespace().unwrap_or_default();
    let name = claim.name_any();
    let _timer = ctx
        .metrics
        .reconcile_duration
        .with_label_values(&[KIND])
        .start_timer();

    // 1. Deletion → snapshot a RetainedClaim, THEN un-finalize (2.4f).
    let finalizers = claim.metadata.finalizers.clone().unwrap_or_default();
    if claim.metadata.deletion_timestamp.is_some() {
        if finalizers.iter().any(|f| f == PROVISIONER_FINALIZER) {
            // Crash-safe order: snapshot FIRST (an idempotent SSA-apply
            // of a deterministic-named object — a crash before
            // un-finalizing simply re-applies the byte-identical
            // RetainedClaim on the next reconcile), THEN drop the
            // finalizer. The snapshot is the GC's only handle on the
            // retained role/DB/Secret, so it MUST exist before the
            // finalizer (and thus the only delete observation) is gone.
            snapshot_retained_claim(&ctx, &claim, &ns, &name).await?;
            info!(
                %name, %ns,
                "ResourceClaim deleted — snapshotted RetainedClaim for 2.4f GC; releasing finalizer"
            );
            set_finalizers(&ctx.client, &ns, &name, without_finalizer(&finalizers)).await?;
        }
        return Ok(Action::await_change());
    }

    // 2. Ensure the finalizer is present so deletes are observed.
    if !finalizers.iter().any(|f| f == PROVISIONER_FINALIZER) {
        set_finalizers(&ctx.client, &ns, &name, with_finalizer(&finalizers)).await?;
        // The patch re-triggers reconcile; provisioning proceeds below.
    }

    // 3. Gate on the scheduler's verdict.
    let status_json = claim
        .status
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?
        .unwrap_or_else(|| json!({}));
    if !should_provision(&status_json) {
        info!(%name, %ns, "not yet Scheduled (or already ready) — waiting for scheduler");
        return Ok(Action::requeue(Duration::from_secs(60)));
    }

    let provider_name = status_json
        .pointer("/provider")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // 4. Read the matched ServiceProvider. Providers are seeded into
    //    `apprafter-system`; list cluster-wide and match by name so we
    //    do not assume the namespace.
    let provider = match find_provider(&ctx.client, &provider_name).await? {
        Some(p) => p,
        None => {
            warn!(%name, %ns, provider = %provider_name, "matched ServiceProvider not found — requeue");
            return Ok(Action::requeue(Duration::from_secs(60)));
        }
    };

    match Backend::from_spec_backend(&provider.spec.backend) {
        Some(Backend::Cloudnativepg) => {
            provision_cloudnativepg(&ctx, &claim, &ns, &name, &provider).await
        }
        Some(Backend::Dragonfly) => provision_dragonfly(&ctx, &claim, &ns, &name, &provider).await,
        None => {
            warn!(
                %name, %ns, backend = %provider.spec.backend,
                "unknown provider backend — no provisioner wired; requeue (a future backend may handle it)"
            );
            Ok(Action::requeue(Duration::from_secs(300)))
        }
    }
}

/// Error policy: increment error metrics and requeue after 30 seconds.
pub fn error_policy(claim: Arc<ResourceClaim>, err: &ReconcileError, ctx: Arc<Context>) -> Action {
    let name = claim.name_any();
    let namespace = claim.namespace().unwrap_or_default();
    warn!(%name, %namespace, %err, "resourceclaim provisioner reconcile error");
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

// ---------------------------------------------------------------------------
// CloudNativePG provisioning (I/O orchestration)
// ---------------------------------------------------------------------------

/// Provision a `cloudnative-pg` claim into the shared CNPG Cluster.
async fn provision_cloudnativepg(
    ctx: &Arc<Context>,
    claim: &Arc<ResourceClaim>,
    ns: &str,
    name: &str,
    provider: &ServiceProvider,
) -> Result<Action, ReconcileError> {
    let cfg = provider.spec.config.clone().unwrap_or_else(|| json!({}));
    let cluster = cfg
        .pointer("/cluster")
        .and_then(Value::as_str)
        .unwrap_or("platform-postgres")
        .to_string();
    let cnpg_ns = cfg
        .pointer("/namespace")
        .and_then(Value::as_str)
        .unwrap_or("cnpg-system")
        .to_string();
    let instances = cfg
        .pointer("/instances")
        .and_then(Value::as_i64)
        .unwrap_or(1);
    let storage = cfg
        .pointer("/storage")
        .and_then(Value::as_str)
        .unwrap_or("10Gi")
        .to_string();

    info!(%name, %ns, %cluster, %cnpg_ns, "provisioning cloudnative-pg claim");

    // 1. Lazily SSA-apply the shared Cluster (sole-owned). First claim
    //    creates `platform-postgres`; later claims no-op the apply.
    let cluster_api: Api<DynamicObject> =
        Api::namespaced_with(ctx.client.clone(), &cnpg_ns, &cluster_ar());
    let cluster_body = cnpg::cluster_object(&cluster, &cnpg_ns, instances, &storage);
    cluster_api
        .patch(&cluster, &apply_params(), &Patch::Apply(&cluster_body))
        .await?;

    // 2. Derive Postgres identifiers (role/db — `_` is valid inside
    //    Postgres) AND a DNS-1123 Kubernetes object name (`-` — for the
    //    Database CR + password Secret `metadata.name`, which the
    //    apiserver validates and would reject with `_`) + a fresh password.
    let role = cnpg::pg_identifier(ns, name);
    let db = role.clone();
    let object_name = cnpg::k8s_name(ns, name);
    let password = generate_password();
    let pw_secret_name = format!("{object_name}-pw");

    // 3. Apply the basic-auth password Secret in the CNPG namespace.
    let secret_api: Api<DynamicObject> =
        Api::namespaced_with(ctx.client.clone(), &cnpg_ns, &secret_ar());
    let pw_secret = cnpg::basic_auth_secret(&pw_secret_name, &cnpg_ns, &role, &password);
    secret_api
        .patch(&pw_secret_name, &apply_params(), &Patch::Apply(&pw_secret))
        .await?;

    // 4. RMW the Cluster's unkeyed `spec.managed.roles` (retry on 409).
    upsert_managed_role(ctx, &cnpg_ns, &cluster, &role, &pw_secret_name).await?;

    // 5. Apply the Database (DynamicObject). CNPG retries until the
    //    owner role materialises.
    let db_api: Api<DynamicObject> =
        Api::namespaced_with(ctx.client.clone(), &cnpg_ns, &database_ar());
    let db_body = cnpg::database_object(&object_name, &cnpg_ns, &cluster, &db, &role, "present");
    db_api
        .patch(&object_name, &apply_params(), &Patch::Apply(&db_body))
        .await?;

    // 6. Apply the connection Secret in the claim's namespace, owned by
    //    the claim so it cascades on delete.
    let conn_secret_name = connection_secret_name(name);
    let dsn = cnpg::dsn(&role, &password, &db, &cluster, &cnpg_ns);
    let owner_uid = claim.metadata.uid.clone().unwrap_or_default();
    let conn_secret = connection_secret_object(&conn_secret_name, ns, &dsn, &owner_uid, name);
    let conn_api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), ns, &secret_ar());
    conn_api
        .patch(
            &conn_secret_name,
            &apply_params(),
            &Patch::Apply(&conn_secret),
        )
        .await?;

    // 7. Write status — ONLY ready / connectionSecretRef / Ready
    //    condition, under our own field manager.
    let prior: Vec<ResourceClaimCondition> = claim
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let cond = ready_condition(
        "True",
        "Provisioned",
        &format!("provisioned into {cluster} ({cnpg_ns})"),
        &prior,
    );
    patch_status(&ctx.client, ns, name, &conn_secret_name, cond).await?;

    ctx.metrics
        .claim_provisioned_total
        .with_label_values(&["cloudnative-pg", ns])
        .inc();
    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, ns, "ok"])
        .inc();
    info!(%name, %ns, %role, %db, "ResourceClaim provisioned");

    // Recovery (2.4f Fix A): the claim is (re)provisioned, so any
    // RetainedClaim from a prior deletion is stale — cancel it so its
    // grace-GC can never drop this now-live claim's role/DB. The snapshot
    // name is deterministic (`cnpg::k8s_name(ns, name)` == `object_name`),
    // so this targets exactly the matching RetainedClaim. 404-tolerant.
    let rc_api: Api<RetainedClaim> = Api::namespaced(ctx.client.clone(), RETAINED_CLAIM_NAMESPACE);
    if let Err(e) = rc_api.delete(&object_name, &DeleteParams::default()).await {
        if !matches!(&e, kube::Error::Api(ae) if ae.code == 404) {
            warn!(%object_name, error=%e, "could not cancel stale RetainedClaim on re-provision");
        }
    }

    Ok(Action::requeue(Duration::from_secs(300)))
}

// ---------------------------------------------------------------------------
// Dragonfly provisioning (I/O orchestration)
// ---------------------------------------------------------------------------

/// Index of the single shared pool instance per persistence class. The
/// 2.6 baseline runs one ephemeral + one persistent instance and grows
/// the pool horizontally (a new index) only when the current instance's
/// 1024-DB ceiling is hit (ADR 0042 §3). Spreading claims across multiple
/// instances is future work; today the allocator always targets index 0
/// and surfaces a "pool full → grow" error when it fills.
const POOL_INSTANCE_INDEX: u32 = 0;

/// Provision a `dragonfly` claim into a lazily-created shared Dragonfly
/// pool instance.
///
/// Task 3 scope: resolve the persistence class from the claim, lazily
/// SSA-apply the per-instance admin Secret + the shared `Dragonfly` CR,
/// allocate the claim a numbered logical DB off the live-claim source of
/// truth, and persist `status.{instance,dbnum}` — but leave the claim
/// NOT-ready (no `$N` ACL user, no connection Secret yet). Task 4 reads
/// that allocation back, drives the imperative ACL user + connection
/// Secret, and flips `Ready=True`. The split keeps each task independently
/// testable; a claim parked here simply requeues until Task 4's path runs.
async fn provision_dragonfly(
    ctx: &Arc<Context>,
    claim: &Arc<ResourceClaim>,
    ns: &str,
    name: &str,
    provider: &ServiceProvider,
) -> Result<Action, ReconcileError> {
    let cfg = provider.spec.config.clone().unwrap_or_else(|| json!({}));
    let df_ns = cfg
        .pointer("/namespace")
        .and_then(Value::as_str)
        .unwrap_or("dragonfly-system")
        .to_string();
    let dbnum_max = cfg
        .pointer("/dbnum")
        .and_then(Value::as_u64)
        .unwrap_or(1024) as u16;
    let num_shards = cfg
        .pointer("/numShards")
        .and_then(Value::as_u64)
        .unwrap_or(1) as u16;

    // Persistence class is carried on the claim (the Application controller
    // copies `needs.<type>.persistent` onto the generated claim spec, 2.4d).
    let persistent = claim.spec.persistent.unwrap_or(false);
    let instance = dragonfly::pool_instance_name(persistent, POOL_INSTANCE_INDEX);

    info!(%name, %ns, %instance, %df_ns, persistent, "provisioning dragonfly claim");

    // 1. Lazily SSA-apply the per-instance admin Secret then the shared
    //    `Dragonfly` CR. First claim of a class creates both; later claims
    //    no-op the apply. `generate_password()` returns a fresh random
    //    value on every call, so an unconditional `Patch::Apply` would
    //    clobber any existing `stringData.password` (SSA overwrites whatever
    //    the field manager sends). Hence read-or-create: only apply the
    //    admin Secret when it is absent, so an established password survives
    //    re-reconciles.
    let admin_secret_name = dragonfly::admin_secret_name(&instance);
    let secret_api: Api<DynamicObject> =
        Api::namespaced_with(ctx.client.clone(), &df_ns, &secret_ar());
    if secret_api.get_opt(&admin_secret_name).await?.is_none() {
        let admin_pw = generate_password();
        let admin_secret = dragonfly::admin_secret_object(&admin_secret_name, &df_ns, &admin_pw);
        secret_api
            .patch(
                &admin_secret_name,
                &apply_params(),
                &Patch::Apply(&admin_secret),
            )
            .await?;
        info!(%instance, %df_ns, "created Dragonfly admin password Secret");
    }

    let df_api: Api<DynamicObject> =
        Api::namespaced_with(ctx.client.clone(), &df_ns, &dragonfly_cluster_ar());
    let df_body = dragonfly::dragonfly_object(&instance, &df_ns, dbnum_max, num_shards, persistent);
    df_api
        .patch(&instance, &apply_params(), &Patch::Apply(&df_body))
        .await?;

    // 2. Allocate a numbered logical DB off the LIVE claim source of truth.
    //    If this claim already holds an allocation on this instance (a
    //    re-reconcile after the status landed but before Task 4 finished),
    //    keep it — `used_dbnums_on_instance` excludes nothing, so we look
    //    up our own first to stay idempotent.
    let existing_alloc =
        claim
            .status
            .as_ref()
            .and_then(|s| match (s.instance.as_deref(), s.dbnum) {
                (Some(i), Some(n)) if i == instance => Some(n),
                _ => None,
            });
    let dbnum = match existing_alloc {
        Some(n) => n,
        None => {
            let claims: Vec<ResourceClaim> = Api::<ResourceClaim>::all(ctx.client.clone())
                .list(&Default::default())
                .await?
                .items;
            let used = dragonfly::used_dbnums_on_instance(&claims, &instance);
            match dragonfly::allocate_dbnum(&used, dbnum_max) {
                Some(n) => n,
                None => {
                    warn!(
                        %name, %ns, %instance, dbnum_max,
                        "dragonfly pool instance full — grow the pool (ADR 0042 §3); requeue"
                    );
                    return Ok(Action::requeue(Duration::from_secs(60)));
                }
            }
        }
    };

    // 3. Persist the allocation (instance + dbnum) so a crash between here
    //    and the ACL apply re-reads it back idempotently (the
    //    `existing_alloc` short-circuit above), and a later reconcile / the
    //    re-pin loop (2.6-5) reads it. Under our own field manager (the SSA
    //    split stays intact: instance/dbnum are provisioner-owned, never
    //    scheduler fields). This patch does NOT set `ready` — step 6 does,
    //    only after the ACL user + connection Secret exist.
    patch_allocation(&ctx.client, ns, name, &instance, dbnum).await?;

    // 4. Drive the per-claim `$N` ACL user imperatively (it is runtime
    //    state, not declarable on the CR). Read the instance admin
    //    password, FLUSHDB the target DB FIRST (recycle-safety: a reused
    //    dbnum must start empty — ADR 0042 §3), then ACL SETUSER the
    //    `$N`-pinned, keyspace-isolated user with a fresh password.
    let addr = dragonfly::instance_addr(&instance, &df_ns);
    let admin_pw = read_admin_password(ctx, &df_ns, &admin_secret_name).await?;
    let user = dragonfly::acl_user(ns, name);
    let claim_pw = generate_password();

    ctx.redis
        .flushdb(&addr, &admin_pw, dbnum)
        .await
        .map_err(|e| ReconcileError::Provisioning(format!("dragonfly FLUSHDB: {e}")))?;
    let setuser_args = dragonfly::acl_setuser_args(&user, &claim_pw, dbnum);
    ctx.redis
        .acl_setuser(&addr, &admin_pw, &setuser_args)
        .await
        .map_err(|e| ReconcileError::Provisioning(format!("dragonfly ACL SETUSER: {e}")))?;

    // 5. Apply the connection Secret in the claim's namespace, owner-ref'd
    //    to the claim so it cascades on delete. Two keys: the `$N`-pinned
    //    DSN + the pub/sub channel prefix the `&{user}:*` ACL enforces.
    let conn_secret_name = connection_secret_name(name);
    let dsn = dragonfly::redis_dsn(&user, &claim_pw, &instance, &df_ns, dbnum);
    let prefix = dragonfly::channel_prefix(&user);
    let owner_uid = claim.metadata.uid.clone().unwrap_or_default();
    let conn_secret =
        redis_connection_secret_object(&conn_secret_name, ns, &dsn, &prefix, &owner_uid, name);
    let conn_api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), ns, &secret_ar());
    conn_api
        .patch(
            &conn_secret_name,
            &apply_params(),
            &Patch::Apply(&conn_secret),
        )
        .await?;

    // 6. Write status — ready / connectionSecretRef / Ready condition,
    //    under our own field manager.
    let prior: Vec<ResourceClaimCondition> = claim
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let cond = ready_condition(
        "True",
        "Provisioned",
        &format!("provisioned into {instance} ({df_ns}) DB {dbnum}"),
        &prior,
    );
    patch_status(&ctx.client, ns, name, &conn_secret_name, cond).await?;

    ctx.metrics
        .claim_provisioned_total
        .with_label_values(&["dragonfly", ns])
        .inc();
    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, ns, "ok"])
        .inc();
    info!(%name, %ns, %instance, dbnum, %user, "dragonfly claim provisioned");

    // Recovery (2.4f Fix A, mirrored for dragonfly): the claim is now
    // (re)provisioned, so any RetainedClaim from a prior deletion is stale
    // — cancel it so its grace-GC can never FLUSHDB/DELUSER this now-live
    // claim's DB/user. Deterministic name (`cnpg::k8s_name(ns, name)` ==
    // `object_name`), 404-tolerant.
    let object_name = cnpg::k8s_name(ns, name);
    let rc_api: Api<RetainedClaim> = Api::namespaced(ctx.client.clone(), RETAINED_CLAIM_NAMESPACE);
    if let Err(e) = rc_api.delete(&object_name, &DeleteParams::default()).await {
        if !matches!(&e, kube::Error::Api(ae) if ae.code == 404) {
            warn!(%object_name, error=%e, "could not cancel stale RetainedClaim on dragonfly re-provision");
        }
    }

    // Re-poll on the standard cadence; the ACL user is re-asserted by the
    // 2.6-5 reconcile loop after an instance restart.
    Ok(Action::requeue(Duration::from_secs(300)))
}

/// Read the `password` key from a pool instance's admin Secret. The
/// Secret is created with `stringData`, so on read the value comes back
/// base64-encoded under `data.password`.
async fn read_admin_password(
    ctx: &Arc<Context>,
    df_ns: &str,
    admin_secret_name: &str,
) -> Result<String, ReconcileError> {
    let secret_api: Api<DynamicObject> =
        Api::namespaced_with(ctx.client.clone(), df_ns, &secret_ar());
    let secret = secret_api.get(admin_secret_name).await?;
    let raw = secret
        .data
        .pointer("/data/password")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ReconcileError::Provisioning(format!(
                "Dragonfly admin Secret {admin_secret_name} missing data.password"
            ))
        })?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|e| {
            ReconcileError::Provisioning(format!(
                "Dragonfly admin Secret {admin_secret_name} password not base64: {e}"
            ))
        })?;
    String::from_utf8(decoded).map_err(|e| {
        ReconcileError::Provisioning(format!(
            "Dragonfly admin Secret {admin_secret_name} password not UTF-8: {e}"
        ))
    })
}

/// SSA-patch ONLY the dragonfly allocation fields (`status.instance` /
/// `status.dbnum`) under the provisioner field manager. Never touches
/// `ready` / `connectionSecretRef` (Task 4) or the scheduler's
/// `provider` / `Scheduled` (the SSA split).
async fn patch_allocation(
    client: &Client,
    ns: &str,
    name: &str,
    instance: &str,
    dbnum: u16,
) -> Result<(), ReconcileError> {
    let api: Api<ResourceClaim> = Api::namespaced(client.clone(), ns);
    let body = json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "ResourceClaim",
        "metadata": { "name": name },
        "status": {
            "instance": instance,
            "dbnum": dbnum,
        },
    });
    api.patch_status(name, &apply_params(), &Patch::Apply(&body))
        .await?;
    Ok(())
}

/// Read-modify-write the shared Cluster's `spec.managed.roles`. The list
/// is unkeyed under SSA, so we GET the whole Cluster, `merge_role`, and
/// replace it — retrying on a 409 conflict from a racing provisioner.
///
/// Why `replace` (PUT) and not `Patch::Apply` here, when step 1's lazy
/// Cluster create IS an SSA apply: an SSA apply of an unkeyed list takes
/// ownership of the WHOLE list, so applying our one role would strip every
/// foreign entry (CNPG-seeded roles, other claims' roles). `merge_role`
/// preserves them and the PUT writes the full object back. The interaction
/// with step 1 is safe: that apply body deliberately omits `spec.managed`
/// (see `cnpg::cluster_object`), so this manager never owns `roles` via its
/// Apply entry — it owns them via this PUT's Update entry, and a later
/// apply that omits `roles` does not strip a field held by an Update entry.
/// (First live validation is the 2.4g manual walk: confirm a second claim's
/// role survives the first claim's next reconcile.)
async fn upsert_managed_role(
    ctx: &Arc<Context>,
    cnpg_ns: &str,
    cluster: &str,
    role: &str,
    pw_secret_name: &str,
) -> Result<(), ReconcileError> {
    let api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), cnpg_ns, &cluster_ar());
    let entry = cnpg::managed_role_entry(role, pw_secret_name);

    for attempt in 0..ROLE_RMW_RETRIES {
        let current = api.get(cluster).await?;
        let mut current_json = serde_json::to_value(&current)?;

        let existing: Vec<Value> = current_json
            .pointer("/spec/managed/roles")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let merged = cnpg::merge_role(existing, entry.clone());

        // Splice the merged list back in, preserving the resourceVersion
        // so the replace is optimistic-concurrency guarded.
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
        managed.insert("roles".to_string(), Value::Array(merged));

        let replaced: DynamicObject = serde_json::from_value(current_json)?;
        match api.replace(cluster, &Default::default(), &replaced).await {
            Ok(_) => return Ok(()),
            Err(kube::Error::Api(err)) if err.code == 409 => {
                warn!(
                    %cluster, attempt,
                    "managed.roles RMW conflict (409) — retrying with fresh resourceVersion"
                );
                continue;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(ReconcileError::Provisioning(format!(
        "managed.roles RMW for {cluster} exhausted {ROLE_RMW_RETRIES} retries on 409 conflict"
    )))
}

/// Snapshot a deleting `ResourceClaim` into an immutable `RetainedClaim`
/// in `apprafter-system` (Phase 2.4f). Idempotent SSA-apply: the
/// snapshot name is deterministic and the body is byte-stable, so a
/// crash before the finalizer is dropped re-applies the same object.
///
/// The finalizer ONLY creates the RetainedClaim (its own object) — it
/// never writes `ResourceClaim` status here (SSA split preserved). A
/// missing provider / config never stalls the finalizer: the CNPG
/// cluster + namespace fall back to the provisioning defaults with a
/// `warn!`, and an absent deletion timestamp falls back to `Utc::now()`.
async fn snapshot_retained_claim(
    ctx: &Arc<Context>,
    claim: &Arc<ResourceClaim>,
    ns: &str,
    name: &str,
) -> Result<(), ReconcileError> {
    // Re-derive the same identifiers the provisioner used (deterministic
    // from the claim's (namespace, name)).
    let role = cnpg::pg_identifier(ns, name);
    let db = role.clone();
    let object_name = cnpg::k8s_name(ns, name);
    let pw_secret_name = format!("{object_name}-pw");

    // Provider lineage + CNPG target. status.provider may be absent if
    // the claim never got scheduled — snapshot anyway with empty
    // provider/backend and the default CNPG target (the GC tolerates a
    // missing role/DB/Secret 404).
    let provider_name = claim
        .status
        .as_ref()
        .and_then(|s| s.provider.clone())
        .unwrap_or_default();
    let (backend, cluster, cnpg_ns) = match find_provider(&ctx.client, &provider_name).await? {
        Some(p) => {
            let cfg = p.spec.config.clone().unwrap_or_else(|| json!({}));
            let cluster = cfg
                .pointer("/cluster")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_CNPG_CLUSTER)
                .to_string();
            let cnpg_ns = cfg
                .pointer("/namespace")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_CNPG_NAMESPACE)
                .to_string();
            (p.spec.backend.clone(), cluster, cnpg_ns)
        }
        None => {
            warn!(
                %name, %ns, provider = %provider_name,
                "matched ServiceProvider not found on delete — snapshotting RetainedClaim with default CNPG target"
            );
            (
                String::new(),
                DEFAULT_CNPG_CLUSTER.to_string(),
                DEFAULT_CNPG_NAMESPACE.to_string(),
            )
        }
    };

    // retainUntil = deletionTimestamp + 7-day grace. The injected clock
    // is the deletion instant; `Utc::now()` is the fallback only if the
    // apiserver somehow omitted the timestamp (it never does on a
    // delete, but the finalizer must never stall).
    let deletion = claim
        .metadata
        .deletion_timestamp
        .as_ref()
        .map(|t| t.0)
        .unwrap_or_else(Utc::now);
    let retain_until = grace::compute_retain_until(deletion, grace::GRACE_PERIOD).to_rfc3339();

    // Backend-dispatch the snapshot shape. A dragonfly claim carries its
    // allocation (`status.instance` / `status.dbnum`) — snapshot that +
    // the deterministic ACL user + connection-Secret ref so the GC
    // (2.6-7) can FLUSHDB + DELUSER. Everything else stays the CNPG shape.
    let payload = match Backend::from_spec_backend(&backend) {
        Some(Backend::Dragonfly) => {
            let (instance, dbnum) = claim
                .status
                .as_ref()
                .and_then(|s| Some((s.instance.clone()?, s.dbnum?)))
                .unwrap_or_default();
            if instance.is_empty() {
                // A dragonfly claim deleted before it was ever allocated:
                // no DB/ACL exists, so there is nothing for the GC to drop.
                // Snapshot it anyway (the GC tolerates the missing
                // allocation) so the 7-day RetainedClaim lifecycle is
                // uniform — the dragonfly GC path 404-tolerates everything.
                warn!(
                    %name, %ns,
                    "dragonfly claim deleted before allocation — snapshotting without instance/dbnum"
                );
            }
            let acl_user = dragonfly::acl_user(ns, name);
            let conn_secret_name = connection_secret_name(name);
            retained_claim_dragonfly_object(
                &object_name,
                name,
                ns,
                &provider_name,
                &instance,
                dbnum,
                &acl_user,
                &conn_secret_name,
                ns,
                &retain_until,
            )
        }
        // CNPG (or an unknown/empty backend — legacy snapshots default to
        // the CNPG shape, matching the GC's `gc_backend("")` default).
        _ => retained_claim_object(
            &object_name,
            name,
            ns,
            &provider_name,
            &backend,
            &cluster,
            &cnpg_ns,
            &role,
            &db,
            &object_name,
            &pw_secret_name,
            &retain_until,
        ),
    };

    let api: Api<RetainedClaim> = Api::namespaced(ctx.client.clone(), RETAINED_CLAIM_NAMESPACE);
    api.patch(&object_name, &apply_params(), &Patch::Apply(&payload))
        .await?;

    info!(
        %name, %ns, snapshot = %object_name, %retain_until,
        "RetainedClaim snapshot applied"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Dynamic ApiResources for the externally-installed CNPG CRDs + Secrets
// ---------------------------------------------------------------------------

pub(crate) fn cluster_ar() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk(
        "postgresql.cnpg.io",
        "v1",
        "Cluster",
    ))
}

pub(crate) fn database_ar() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk(
        "postgresql.cnpg.io",
        "v1",
        "Database",
    ))
}

pub(crate) fn secret_ar() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk("", "v1", "Secret"))
}

/// ApiResource for the externally-installed dragonfly-operator
/// `Dragonfly` CRD (plan.md 2.6-1 component; group `dragonflydb.io`).
pub(crate) fn dragonfly_cluster_ar() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk(
        "dragonflydb.io",
        "v1alpha1",
        "Dragonfly",
    ))
}

pub(crate) fn apply_params() -> PatchParams {
    PatchParams::apply(FIELD_MANAGER).force()
}

/// Retry budget for the GC's `spec.managed.roles` read-modify-write
/// (mirrors the provisioner's [`ROLE_RMW_RETRIES`]).
pub(crate) const GC_ROLE_RMW_RETRIES: usize = ROLE_RMW_RETRIES;

// ---------------------------------------------------------------------------
// Status + finalizer I/O
// ---------------------------------------------------------------------------

/// SSA-patch the claim status with ONLY `ready` / `connectionSecretRef` /
/// the `Ready` condition. Never touches `provider` or `Scheduled`.
async fn patch_status(
    client: &Client,
    ns: &str,
    name: &str,
    conn_secret_name: &str,
    cond: ResourceClaimCondition,
) -> Result<(), ReconcileError> {
    let api: Api<ResourceClaim> = Api::namespaced(client.clone(), ns);
    let body = json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "ResourceClaim",
        "metadata": { "name": name },
        "status": {
            "ready": true,
            "connectionSecretRef": conn_secret_name,
            "conditions": [cond],
        },
    });
    api.patch_status(name, &apply_params(), &Patch::Apply(&body))
        .await?;
    Ok(())
}

/// Merge-patch the claim's `metadata.finalizers` to `list`.
async fn set_finalizers(
    client: &Client,
    ns: &str,
    name: &str,
    list: Vec<String>,
) -> Result<(), ReconcileError> {
    let api: Api<ResourceClaim> = Api::namespaced(client.clone(), ns);
    let patch = json!({ "metadata": { "finalizers": list } });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Pure decision helpers (unit-tested without a cluster)
// ---------------------------------------------------------------------------

/// True iff the claim is ready to be provisioned: the scheduler set
/// `Scheduled=True`, `status.provider` is a non-empty string, and the
/// claim is not already `ready` (don't re-provision a done claim).
pub fn should_provision(status: &Value) -> bool {
    if status.pointer("/ready").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    let provider_set = status
        .pointer("/provider")
        .and_then(Value::as_str)
        .map(|p| !p.is_empty())
        .unwrap_or(false);
    if !provider_set {
        return false;
    }
    status
        .pointer("/conditions")
        .and_then(Value::as_array)
        .map(|conds| {
            conds.iter().any(|c| {
                c.get("type").and_then(Value::as_str) == Some(COND_SCHEDULED)
                    && c.get("status").and_then(Value::as_str) == Some("True")
            })
        })
        .unwrap_or(false)
}

/// Build the `Ready` condition, preserving `lastTransitionTime` when the
/// `(type, status)` pair is unchanged — the same hot-loop guard the
/// scheduler uses. Always emits the `Ready` type.
pub fn ready_condition(
    status: &str,
    reason: &str,
    message: &str,
    previous: &[ResourceClaimCondition],
) -> ResourceClaimCondition {
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == COND_READY && c.status == status)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    ResourceClaimCondition {
        type_: COND_READY.to_string(),
        status: status.to_string(),
        last_transition_time,
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
    }
}

/// Deterministic name for a claim's connection Secret (in the claim's
/// own namespace).
pub fn connection_secret_name(claim_name: &str) -> String {
    format!("{claim_name}-conn")
}

/// Build the connection Secret apply body: an `Opaque` Secret carrying
/// `DATABASE_URL`, with an `ownerReference` back to the `ResourceClaim`
/// so it cascades on claim delete (no finalizer needed for it).
pub fn connection_secret_object(
    name: &str,
    ns: &str,
    dsn: &str,
    owner_uid: &str,
    owner_name: &str,
) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
            },
            "ownerReferences": [{
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "ResourceClaim",
                "name": owner_name,
                "uid": owner_uid,
                "controller": true,
                "blockOwnerDeletion": true,
            }],
        },
        "type": "Opaque",
        "stringData": {
            "DATABASE_URL": dsn,
        },
    })
}

/// Build the dragonfly connection Secret apply body: an `Opaque` Secret
/// carrying BOTH `REDIS_URL` (the `$N`-pinned DSN) and
/// `REDIS_CHANNEL_PREFIX` (the pub/sub prefix the app must apply),
/// owner-ref'd to the `ResourceClaim` so it cascades on claim delete.
/// The renderer (2.6-6) injects both keys; KEEP THE KEYS in sync with
/// `operator-rendering`'s `NEEDS_ENV_BINDINGS` and the webhook guard.
pub fn redis_connection_secret_object(
    name: &str,
    ns: &str,
    dsn: &str,
    channel_prefix: &str,
    owner_uid: &str,
    owner_name: &str,
) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
            },
            "ownerReferences": [{
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "ResourceClaim",
                "name": owner_name,
                "uid": owner_uid,
                "controller": true,
                "blockOwnerDeletion": true,
            }],
        },
        "type": "Opaque",
        "stringData": {
            "REDIS_URL": dsn,
            "REDIS_CHANNEL_PREFIX": channel_prefix,
        },
    })
}

/// Build the flat `apprafter.io/v1alpha1` `RetainedClaim` SSA-apply body
/// the finalizer snapshots into `apprafter-system` before un-finalizing
/// a deleted claim (Phase 2.4f).
///
/// `snapshot_name` is `cnpg::k8s_name(claim_ns, claim_name)` — a
/// deterministic, DNS-1123-safe name that encodes the claim origin, so
/// every claim maps to a unique RetainedClaim in the single
/// `apprafter-system` namespace and a crash-then-retry re-applies the
/// byte-identical object (idempotent SSA). The original
/// `(claim_name, claim_ns)` lineage is preserved in `spec.claimRef`.
#[allow(clippy::too_many_arguments)]
pub fn retained_claim_object(
    snapshot_name: &str,
    claim_name: &str,
    claim_ns: &str,
    provider: &str,
    backend: &str,
    cnpg_cluster: &str,
    cnpg_namespace: &str,
    role: &str,
    database: &str,
    database_object_name: &str,
    password_secret_name: &str,
    retain_until: &str,
) -> Value {
    json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "RetainedClaim",
        "metadata": {
            "name": snapshot_name,
            "namespace": RETAINED_CLAIM_NAMESPACE,
        },
        "spec": {
            "claimRef": {
                "name": claim_name,
                "namespace": claim_ns,
            },
            "provider": provider,
            "backend": backend,
            "cnpgCluster": cnpg_cluster,
            "cnpgNamespace": cnpg_namespace,
            "role": role,
            "database": database,
            "databaseObjectName": database_object_name,
            "passwordSecretName": password_secret_name,
            "retainUntil": retain_until,
        },
    })
}

/// Build the flat `RetainedClaim` SSA-apply body for a DRAGONFLY claim
/// (2.6-4). Carries the allocation (`instance` / `dbnum`), the `$N` ACL
/// username, and the connection-Secret ref/namespace — everything the GC
/// (2.6-7) needs to `FLUSHDB` + `ACL DELUSER` + drop the Secret once
/// `retainUntil` passes. No CNPG fields are set (the GC backend-dispatches
/// on `spec.backend`). Same deterministic `snapshot_name` /
/// `apprafter-system` placement / `claimRef` lineage as the CNPG path.
#[allow(clippy::too_many_arguments)]
pub fn retained_claim_dragonfly_object(
    snapshot_name: &str,
    claim_name: &str,
    claim_ns: &str,
    provider: &str,
    instance: &str,
    dbnum: u16,
    acl_user: &str,
    connection_secret_ref: &str,
    connection_secret_namespace: &str,
    retain_until: &str,
) -> Value {
    json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "RetainedClaim",
        "metadata": {
            "name": snapshot_name,
            "namespace": RETAINED_CLAIM_NAMESPACE,
        },
        "spec": {
            "claimRef": {
                "name": claim_name,
                "namespace": claim_ns,
            },
            "provider": provider,
            "backend": "dragonfly",
            "instance": instance,
            "dbnum": dbnum,
            "aclUser": acl_user,
            "connectionSecretRef": connection_secret_ref,
            "connectionSecretNamespace": connection_secret_namespace,
            "retainUntil": retain_until,
        },
    })
}

/// `current` with the provisioner finalizer appended (idempotent).
fn with_finalizer(current: &[String]) -> Vec<String> {
    let mut out = current.to_vec();
    if !out.iter().any(|f| f == PROVISIONER_FINALIZER) {
        out.push(PROVISIONER_FINALIZER.to_string());
    }
    out
}

/// `current` without the provisioner finalizer.
fn without_finalizer(current: &[String]) -> Vec<String> {
    current
        .iter()
        .filter(|f| *f != PROVISIONER_FINALIZER)
        .cloned()
        .collect()
}

/// Generate a random alphanumeric password for a managed role.
fn generate_password() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(PASSWORD_LEN)
        .map(char::from)
        .collect()
}

/// Find the matched `ServiceProvider` by name across all namespaces.
/// Providers are seeded into `apprafter-system`, but listing cluster-wide
/// keeps the controller agnostic to where they live.
async fn find_provider(
    client: &Client,
    provider_name: &str,
) -> Result<Option<ServiceProvider>, ReconcileError> {
    let providers: Vec<ServiceProvider> = Api::<ServiceProvider>::all(client.clone())
        .list(&Default::default())
        .await?
        .items;
    Ok(providers
        .into_iter()
        .find(|p| p.name_any() == provider_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prev_cond(type_: &str, status: &str, ts: &str) -> ResourceClaimCondition {
        ResourceClaimCondition {
            type_: type_.to_string(),
            status: status.to_string(),
            last_transition_time: ts.to_string(),
            reason: Some("Reason".to_string()),
            message: Some("msg".to_string()),
        }
    }

    // --- should_provision() ---

    #[test]
    fn should_provision_true_when_scheduled_provider_set_not_ready() {
        let status = json!({
            "provider": "pg-integrated",
            "conditions": [{ "type": "Scheduled", "status": "True" }],
        });
        assert!(should_provision(&status));
    }

    #[test]
    fn should_provision_false_when_not_scheduled() {
        let status = json!({
            "provider": "pg-integrated",
            "conditions": [{ "type": "Scheduled", "status": "False" }],
        });
        assert!(!should_provision(&status));
    }

    #[test]
    fn should_provision_false_when_no_scheduled_condition() {
        let status = json!({ "provider": "pg-integrated", "conditions": [] });
        assert!(!should_provision(&status));
    }

    #[test]
    fn should_provision_false_when_provider_empty_or_absent() {
        let absent = json!({ "conditions": [{ "type": "Scheduled", "status": "True" }] });
        assert!(!should_provision(&absent));
        let empty = json!({
            "provider": "",
            "conditions": [{ "type": "Scheduled", "status": "True" }],
        });
        assert!(!should_provision(&empty));
    }

    #[test]
    fn should_provision_false_when_already_ready() {
        let status = json!({
            "provider": "pg-integrated",
            "ready": true,
            "conditions": [{ "type": "Scheduled", "status": "True" }],
        });
        assert!(!should_provision(&status));
    }

    // --- Backend dispatch ---

    #[test]
    fn backend_maps_cloudnative_pg() {
        assert_eq!(
            Backend::from_spec_backend("cloudnative-pg"),
            Some(Backend::Cloudnativepg)
        );
    }

    #[test]
    fn backend_maps_dragonfly() {
        assert_eq!(
            Backend::from_spec_backend("dragonfly"),
            Some(Backend::Dragonfly)
        );
    }

    #[test]
    fn backend_unknown_is_none() {
        assert_eq!(Backend::from_spec_backend("redis"), None); // type, not backend
        assert_eq!(Backend::from_spec_backend(""), None);
    }

    // --- ready_condition() (reuses condition() shape) ---

    #[test]
    fn ready_condition_reuses_timestamp_when_status_unchanged() {
        let ts = "2026-01-01T00:00:00+00:00";
        let prev = vec![prev_cond(COND_READY, "True", ts)];
        let c = ready_condition("True", "Provisioned", "ok", &prev);
        assert_eq!(c.last_transition_time, ts);
        assert_eq!(c.type_, COND_READY);
    }

    #[test]
    fn ready_condition_bumps_timestamp_when_status_changes() {
        let ts = "2026-01-01T00:00:00+00:00";
        let prev = vec![prev_cond(COND_READY, "False", ts)];
        let c = ready_condition("True", "Provisioned", "ok", &prev);
        assert_ne!(c.last_transition_time, ts);
    }

    // --- connection_secret_object() ---

    #[test]
    fn connection_secret_carries_dsn_and_owner_ref_cascade() {
        let s = connection_secret_object(
            "demo-web-pg-conn",
            "demo",
            "postgresql://r:p@platform-postgres-rw.cnpg-system.svc:5432/db",
            "uid-123",
            "demo-web-pg",
        );
        assert_eq!(s["apiVersion"], "v1");
        assert_eq!(s["kind"], "Secret");
        assert_eq!(s["metadata"]["name"], "demo-web-pg-conn");
        assert_eq!(s["metadata"]["namespace"], "demo");
        assert_eq!(s["type"], "Opaque");
        assert_eq!(
            s["stringData"]["DATABASE_URL"],
            "postgresql://r:p@platform-postgres-rw.cnpg-system.svc:5432/db"
        );
        // ownerReference → ResourceClaim cascade.
        let owner = &s["metadata"]["ownerReferences"][0];
        assert_eq!(owner["apiVersion"], "apprafter.io/v1alpha1");
        assert_eq!(owner["kind"], "ResourceClaim");
        assert_eq!(owner["name"], "demo-web-pg");
        assert_eq!(owner["uid"], "uid-123");
        assert_eq!(owner["controller"], true);
        assert_eq!(owner["blockOwnerDeletion"], true);
    }

    #[test]
    fn connection_secret_name_is_deterministic() {
        assert_eq!(connection_secret_name("demo-web-pg"), "demo-web-pg-conn");
    }

    // --- retained_claim_object() (2.4f finalizer snapshot) ---

    #[test]
    fn retained_claim_object_is_a_flat_apprafter_snapshot_in_apprafter_system() {
        let snapshot_name = cnpg::k8s_name("demo", "demo-web-pg");
        let rc = retained_claim_object(
            &snapshot_name,
            "demo-web-pg",
            "demo",
            "pg-integrated",
            "cloudnative-pg",
            "platform-postgres",
            "cnpg-system",
            "claim_demo_demo_web_pg",
            "claim_demo_demo_web_pg",
            &snapshot_name,
            &format!("{snapshot_name}-pw"),
            "2026-06-10T00:00:00+00:00",
        );
        assert_eq!(rc["apiVersion"], "apprafter.io/v1alpha1");
        assert_eq!(rc["kind"], "RetainedClaim");
        // Deterministic name = k8s_name(claim_ns, claim_name); always in
        // apprafter-system, never the claim's own namespace.
        assert_eq!(rc["metadata"]["name"], snapshot_name);
        assert_eq!(rc["metadata"]["namespace"], "apprafter-system");
        // Lineage preserved in spec.claimRef.
        assert_eq!(rc["spec"]["claimRef"]["name"], "demo-web-pg");
        assert_eq!(rc["spec"]["claimRef"]["namespace"], "demo");
        // Every flat spec field carries through.
        assert_eq!(rc["spec"]["provider"], "pg-integrated");
        assert_eq!(rc["spec"]["backend"], "cloudnative-pg");
        assert_eq!(rc["spec"]["cnpgCluster"], "platform-postgres");
        assert_eq!(rc["spec"]["cnpgNamespace"], "cnpg-system");
        assert_eq!(rc["spec"]["role"], "claim_demo_demo_web_pg");
        assert_eq!(rc["spec"]["database"], "claim_demo_demo_web_pg");
        assert_eq!(rc["spec"]["databaseObjectName"], snapshot_name);
        assert_eq!(
            rc["spec"]["passwordSecretName"],
            format!("{snapshot_name}-pw")
        );
        assert_eq!(rc["spec"]["retainUntil"], "2026-06-10T00:00:00+00:00");
    }

    #[test]
    fn reprovision_cancels_the_matching_retained_claim_by_deterministic_name() {
        // 2.4f Fix A: on (re)provision the tail deletes the RetainedClaim
        // named `cnpg::k8s_name(ns, name)` — the SAME deterministic name
        // the finalizer snapshots under (`object_name` in the provision
        // body) and the same name encoded in the snapshot's metadata.name.
        // If these ever diverged, the cancel would 404 forever and the
        // recovery time-bomb would survive. Pin the equality.
        let ns = "demo";
        let name = "demo-web-pg";
        let object_name = cnpg::k8s_name(ns, name);
        // The cancel target (object_name) == the snapshot's metadata.name.
        let snapshot = retained_claim_object(
            &object_name,
            name,
            ns,
            "p",
            "b",
            "c",
            "n",
            "r",
            "d",
            &object_name,
            "pw",
            "2026-06-10T00:00:00+00:00",
        );
        assert_eq!(snapshot["metadata"]["name"], object_name);
        assert_eq!(object_name, "claim-demo-demo-web-pg");
    }

    // --- redis_connection_secret_object() (2.6-4) ---

    #[test]
    fn redis_connection_secret_carries_both_keys_and_owner_ref_cascade() {
        let s = redis_connection_secret_object(
            "web-redis-conn",
            "demo",
            "redis://claim_demo_web_redis:p@platform-redis-ephemeral-000.dragonfly-system.svc:6379/7",
            "claim_demo_web_redis:",
            "uid-123",
            "web-redis",
        );
        assert_eq!(s["apiVersion"], "v1");
        assert_eq!(s["kind"], "Secret");
        assert_eq!(s["metadata"]["name"], "web-redis-conn");
        assert_eq!(s["metadata"]["namespace"], "demo");
        assert_eq!(s["type"], "Opaque");
        // Both env keys land in the connection Secret.
        assert_eq!(
            s["stringData"]["REDIS_URL"],
            "redis://claim_demo_web_redis:p@platform-redis-ephemeral-000.dragonfly-system.svc:6379/7"
        );
        assert_eq!(
            s["stringData"]["REDIS_CHANNEL_PREFIX"],
            "claim_demo_web_redis:"
        );
        // ownerReference → ResourceClaim cascade (same as the pg path).
        let owner = &s["metadata"]["ownerReferences"][0];
        assert_eq!(owner["apiVersion"], "apprafter.io/v1alpha1");
        assert_eq!(owner["kind"], "ResourceClaim");
        assert_eq!(owner["name"], "web-redis");
        assert_eq!(owner["uid"], "uid-123");
        assert_eq!(owner["controller"], true);
        assert_eq!(owner["blockOwnerDeletion"], true);
    }

    // --- retained_claim_dragonfly_object() (2.6-4 finalizer snapshot) ---

    #[test]
    fn retained_claim_dragonfly_object_carries_allocation_and_no_cnpg_fields() {
        let snapshot_name = cnpg::k8s_name("demo", "web-redis");
        let rc = retained_claim_dragonfly_object(
            &snapshot_name,
            "web-redis",
            "demo",
            "redis-integrated",
            "platform-redis-ephemeral-000",
            7,
            "claim_demo_web-redis_redis",
            "web-redis-conn",
            "demo",
            "2026-06-12T00:00:00+00:00",
        );
        assert_eq!(rc["apiVersion"], "apprafter.io/v1alpha1");
        assert_eq!(rc["kind"], "RetainedClaim");
        assert_eq!(rc["metadata"]["name"], snapshot_name);
        assert_eq!(rc["metadata"]["namespace"], "apprafter-system");
        assert_eq!(rc["spec"]["claimRef"]["name"], "web-redis");
        assert_eq!(rc["spec"]["claimRef"]["namespace"], "demo");
        assert_eq!(rc["spec"]["provider"], "redis-integrated");
        assert_eq!(rc["spec"]["backend"], "dragonfly");
        assert_eq!(rc["spec"]["instance"], "platform-redis-ephemeral-000");
        assert_eq!(rc["spec"]["dbnum"], 7);
        assert_eq!(rc["spec"]["aclUser"], "claim_demo_web-redis_redis");
        assert_eq!(rc["spec"]["connectionSecretRef"], "web-redis-conn");
        assert_eq!(rc["spec"]["connectionSecretNamespace"], "demo");
        assert_eq!(rc["spec"]["retainUntil"], "2026-06-12T00:00:00+00:00");
        // No CNPG fields leak onto a dragonfly snapshot.
        assert!(rc["spec"].get("cnpgCluster").is_none());
        assert!(rc["spec"].get("role").is_none());
        assert!(rc["spec"].get("databaseObjectName").is_none());
    }

    #[test]
    fn retained_claim_object_name_encodes_origin_and_is_dns1123() {
        // The snapshot name must be the DNS-1123-safe k8s_name (NOT the
        // underscore pg_identifier — the apiserver rejects `_` in a
        // metadata.name).
        let snapshot_name = cnpg::k8s_name("my.ns", "my/claim");
        let rc = retained_claim_object(
            &snapshot_name,
            "my/claim",
            "my.ns",
            "p",
            "b",
            "c",
            "n",
            "r",
            "d",
            &snapshot_name,
            "pw",
            "2026-06-10T00:00:00+00:00",
        );
        let name = rc["metadata"]["name"].as_str().unwrap();
        assert!(
            !name.contains('_'),
            "snapshot name must be DNS-1123: {name}"
        );
        assert!(name.starts_with("claim-"));
    }
}
