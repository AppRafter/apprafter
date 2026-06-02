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

use chrono::Utc;
use kube::api::{Api, ApiResource, DynamicObject, Patch, PatchParams};
use kube::core::GroupVersionKind;
use kube::runtime::controller::Action;
use kube::{Client, ResourceExt};
use rand::distributions::Alphanumeric;
use rand::Rng;
use serde_json::{json, Value};
use tracing::{info, warn};

use operator_core::{ResourceClaim, ResourceClaimCondition, ServiceProvider};

use crate::cnpg;
use crate::{Context, ReconcileError, FIELD_MANAGER, KIND, PROVISIONER_FINALIZER};

/// Condition type this controller owns. The scheduler owns `Scheduled`;
/// this controller owns ONLY `Ready`.
const COND_READY: &str = "Ready";

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

/// The provisioning backends this controller knows how to drive. Only
/// `cloudnative-pg` is wired today; 2.5/2.6 add jetstream / redis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cloudnativepg,
}

impl Backend {
    /// Map a `ServiceProvider.spec.backend` string to a known backend.
    /// Unknown backends return `None` (a future controller may handle
    /// them; this one requeues).
    pub fn from_spec_backend(backend: &str) -> Option<Self> {
        match backend {
            "cloudnative-pg" => Some(Backend::Cloudnativepg),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Public reconcile + error_policy
// ---------------------------------------------------------------------------

/// Reconcile a single `ResourceClaim`:
///
/// 1. On delete (`deletion_timestamp` set) → finalizer SKELETON: log
///    "role/DB retained pending 2.4f GC", drop the provisioner finalizer,
///    await change. The connection Secret cascades via its ownerRef.
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

    // 1. Deletion → finalizer skeleton.
    let finalizers = claim.metadata.finalizers.clone().unwrap_or_default();
    if claim.metadata.deletion_timestamp.is_some() {
        if finalizers.iter().any(|f| f == PROVISIONER_FINALIZER) {
            info!(
                %name, %ns,
                "ResourceClaim deleted — role/DB retained pending 2.4f GC; releasing finalizer"
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
    let db_body = cnpg::database_object(&object_name, &cnpg_ns, &cluster, &db, &role);
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

    Ok(Action::requeue(Duration::from_secs(300)))
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

// ---------------------------------------------------------------------------
// Dynamic ApiResources for the externally-installed CNPG CRDs + Secrets
// ---------------------------------------------------------------------------

fn cluster_ar() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk(
        "postgresql.cnpg.io",
        "v1",
        "Cluster",
    ))
}

fn database_ar() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk(
        "postgresql.cnpg.io",
        "v1",
        "Database",
    ))
}

fn secret_ar() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk("", "v1", "Secret"))
}

fn apply_params() -> PatchParams {
    PatchParams::apply(FIELD_MANAGER).force()
}

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
    fn backend_unknown_is_none() {
        assert_eq!(Backend::from_spec_backend("dragonfly"), None);
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
}
