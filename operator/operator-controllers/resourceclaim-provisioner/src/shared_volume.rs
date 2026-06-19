// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `SharedVolume` reconcile loop (Phase 2.6c).
//!
//! A `SharedVolume` is backed by an unowned `ReadWriteOnce`
//! `PersistentVolumeClaim` SSA-applied by the SharedVolume reconciler
//! (T6). The PVC-builder helpers + the pure status/refCount helpers are
//! pure (`-> serde_json::Value` / `-> i64`), so they are unit-testable
//! without a cluster; the async [`reconcile_shared_volume`] orchestrates
//! the I/O (provider selection → SSA-apply PVC → refCount → status) and
//! is validated on the T13 walk.
//!
//! ## SSA field-manager split (CRITICAL)
//!
//! Like the ResourceClaim provisioner, this controller writes ONLY the
//! `SharedVolume` `.status` (`ready` / `pvcRef` / `refCount` / `capacity`
//! / the `Ready` condition) under [`crate::FIELD_MANAGER`], and never
//! touches `.spec`. SSA REPLACES a manager's owned field-set on each
//! apply, so the terminal status body carries ALL status fields this
//! controller owns.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, Patch, PatchParams};
use kube::core::GroupVersionKind;
use kube::runtime::controller::Action;
use kube::{Client, ResourceExt};
use serde_json::{json, Value};
use tracing::{info, warn};

use operator_core::matching::{select_provider, Candidate};
use operator_core::{ServiceProvider, SharedVolume, SharedVolumeCondition};

use crate::{Context, ReconcileError, FIELD_MANAGER};

/// The `ServiceProvider.spec.type` a `SharedVolume` binds to. There is no
/// scheduler for `SharedVolume` CRs (they carry no selector), so the
/// reconcile selects the provider itself with an EMPTY selector — any
/// `shared-disk` provider qualifies (the seeded `shared-local`).
const SHARED_DISK_TYPE: &str = "shared-disk";

/// Kind string for log/metric labels.
const KIND: &str = "SharedVolume";

/// Condition type this controller owns on a `SharedVolume`.
const COND_READY: &str = "Ready";

/// Finalizer that reaps the backing PVC on `SharedVolume` delete. The PVC
/// is unowned (no `ownerReferences`), so without this finalizer the PVC
/// would leak when the CR is deleted.
const SV_PVC_FINALIZER: &str = "apprafter.io/sharedvolume-pvc-cleanup";

/// StorageClass the SharedVolume backend falls back to when the matched
/// `shared-disk` ServiceProvider config omits `/storageClass`.
const DEFAULT_STORAGE_CLASS: &str = "local-path";

/// Deterministic unowned-PVC name for a SharedVolume.
///
/// The `sv-` prefix avoids any collision with owned-disk PVC names
/// (which are named `claim-<ns>-<app>-disk-<claim>`).
pub fn sv_pvc_name(ns: &str, name: &str) -> String {
    format!("sv-{ns}-{name}")
}

/// Pure SSA-apply body for the unowned backing PVC.
///
/// `accessModes: [ReadWriteOnce]` — on a single node N pods all
/// schedule onto the same node so RWO allows concurrent mounts.
///
/// **NO `ownerReferences`** — the PVC lifecycle is owned by the
/// `SharedVolume` CR (reaped by `volume rm` via finalizer, not by
/// an app delete). A second label `apprafter.io/shared-volume=<name>`
/// makes the PVC inventory-queryable by name without a full label scan.
pub fn sv_pvc_object(
    pvc_name: &str,
    ns: &str,
    size: &str,
    storage_class: &str,
    sv_name: &str,
) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {
            "name": pvc_name,
            "namespace": ns,
            "labels": {
                "apprafter.io/managed-by": "apprafter",
                "apprafter.io/shared-volume": sv_name,
            },
        },
        "spec": {
            "accessModes": ["ReadWriteOnce"],
            "storageClassName": storage_class,
            "resources": {
                "requests": {
                    "storage": size,
                },
            },
        },
    })
}

// ---------------------------------------------------------------------------
// Pure refCount + status helpers (unit-tested without a cluster)
// ---------------------------------------------------------------------------

/// Count reference-ResourceClaims (raw JSON) bound to this SharedVolume
/// name via the `apprafter.io/shared-volume=<name>` label.
///
/// The RFC6901 escape `~1` encodes the `/` in the label key so the JSON
/// pointer resolves the single label entry rather than walking a path.
pub fn ref_count_for(sv_name: &str, claims: &[Value]) -> i64 {
    claims
        .iter()
        .filter(|c| {
            c.pointer("/metadata/labels/apprafter.io~1shared-volume")
                .and_then(Value::as_str)
                == Some(sv_name)
        })
        .count() as i64
}

/// Pure SSA status body for a SharedVolume (field manager = provisioner).
///
/// Always carries `ready` + `refCount`; `pvcRef` and `capacity` are
/// optional. The caller appends the `Ready` condition before sending (see
/// [`sv_status_apply_body_with_condition`]); this bare form keeps the
/// field-set the unit tests assert on minimal.
pub fn sv_status_apply_body(
    sv_name: &str,
    ready: bool,
    pvc_ref: Option<&str>,
    ref_count: i64,
    capacity: Option<(i64, i64)>, // (usedBytes, capacityBytes)
) -> Value {
    let mut status = json!({ "ready": ready, "refCount": ref_count });
    if let Some(p) = pvc_ref {
        status["pvcRef"] = json!(p);
    }
    if let Some((used, cap)) = capacity {
        status["capacity"] = json!({ "usedBytes": used, "capacityBytes": cap });
    }
    json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "SharedVolume",
        "metadata": { "name": sv_name },
        "status": status
    })
}

/// As [`sv_status_apply_body`] but also stamps the `conditions` array with
/// the supplied `Ready` condition — the terminal body the reconcile sends
/// (SSA REPLACES the manager's field-set, so every owned status field,
/// including the condition, must ride this one apply).
pub fn sv_status_apply_body_with_condition(
    sv_name: &str,
    ready: bool,
    pvc_ref: Option<&str>,
    ref_count: i64,
    capacity: Option<(i64, i64)>,
    cond: SharedVolumeCondition,
) -> Value {
    let mut body = sv_status_apply_body(sv_name, ready, pvc_ref, ref_count, capacity);
    body["status"]["conditions"] = json!([cond]);
    body
}

/// Build the `Ready` condition, preserving `lastTransitionTime` when the
/// `(type, status)` pair is unchanged — the same hot-loop guard the
/// ResourceClaim provisioner uses.
pub fn ready_condition(
    status: &str,
    reason: &str,
    message: &str,
    previous: &[SharedVolumeCondition],
) -> SharedVolumeCondition {
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == COND_READY && c.status == status)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    SharedVolumeCondition {
        type_: COND_READY.to_string(),
        status: status.to_string(),
        last_transition_time,
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Dynamic ApiResources + apply params
// ---------------------------------------------------------------------------

/// ApiResource for the core `PersistentVolumeClaim` (group "", v1).
fn pvc_ar() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk("", "v1", "PersistentVolumeClaim"))
}

/// Force SSA apply under the provisioner field manager.
fn apply_params() -> PatchParams {
    PatchParams::apply(FIELD_MANAGER).force()
}

// ---------------------------------------------------------------------------
// Async reconcile + error_policy
// ---------------------------------------------------------------------------

/// Reconcile a single `SharedVolume`:
///
/// 1. On delete (`deletion_timestamp` set) → delete the backing PVC
///    (404-tolerant) then drop the finalizer; await change.
/// 2. Ensure the PVC-cleanup finalizer is present so deletes are seen.
/// 3. Select a `shared-disk` ServiceProvider (EMPTY selector — any
///    qualifies). None → status `ready=false` + `Ready=False/NoProvider`,
///    requeue 60s. Read `config.storageClass` (default `local-path`).
/// 4. SSA-apply the unowned RWO backing PVC (`sv_pvc_object`).
/// 5. Count reference-ResourceClaims in the namespace (`ref_count_for`).
/// 6. SSA-write the terminal status — `ready=true` / `pvcRef` / `refCount`
///    / `Ready=True`, under [`crate::FIELD_MANAGER`] (never `.spec`).
pub async fn reconcile_shared_volume(
    sv: Arc<SharedVolume>,
    ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    let ns = sv.namespace().unwrap_or_default();
    let name = sv.name_any();
    let _timer = ctx
        .metrics
        .reconcile_duration
        .with_label_values(&[KIND])
        .start_timer();
    let pvc_name = sv_pvc_name(&ns, &name);

    // 1. Deletion → delete the backing PVC, then un-finalize.
    let finalizers = sv.metadata.finalizers.clone().unwrap_or_default();
    if sv.metadata.deletion_timestamp.is_some() {
        if finalizers.iter().any(|f| f == SV_PVC_FINALIZER) {
            let pvc_api: Api<DynamicObject> =
                Api::namespaced_with(ctx.client.clone(), &ns, &pvc_ar());
            if let Err(e) = pvc_api.delete(&pvc_name, &DeleteParams::default()).await {
                if !matches!(&e, kube::Error::Api(ae) if ae.code == 404) {
                    return Err(e.into());
                }
            }
            info!(%name, %ns, %pvc_name, "SharedVolume deleted — dropped backing PVC; releasing finalizer");
            set_finalizers(&ctx.client, &ns, &name, without_finalizer(&finalizers)).await?;
        }
        return Ok(Action::await_change());
    }

    // 2. Ensure the finalizer is present so deletes are observed.
    if !finalizers.iter().any(|f| f == SV_PVC_FINALIZER) {
        set_finalizers(&ctx.client, &ns, &name, with_finalizer(&finalizers)).await?;
        // The patch re-triggers reconcile; provisioning proceeds below.
    }

    // 3. Select a `shared-disk` provider (EMPTY selector — SharedVolume
    //    has no selector field; any shared-disk provider qualifies).
    let providers: Vec<ServiceProvider> = Api::<ServiceProvider>::all(ctx.client.clone())
        .list(&Default::default())
        .await?
        .items;
    let candidates: Vec<Candidate> = providers.iter().map(Candidate::from_provider).collect();
    let provider_name = match select_provider(
        SHARED_DISK_TYPE,
        &std::collections::BTreeMap::new(),
        &candidates,
    ) {
        Some(n) => n,
        None => {
            warn!(%name, %ns, "no shared-disk ServiceProvider — SharedVolume not ready; requeue");
            let prior = sv
                .status
                .as_ref()
                .and_then(|s| s.conditions.clone())
                .unwrap_or_default();
            let cond = ready_condition(
                "False",
                "NoProvider",
                "no shared-disk ServiceProvider available",
                &prior,
            );
            let ref_count = current_ref_count(&ctx.client, &ns, &name).await?;
            patch_shared_volume_status(&ctx.client, &ns, &name, false, None, ref_count, None, cond)
                .await?;
            return Ok(Action::requeue(Duration::from_secs(60)));
        }
    };
    let provider = providers
        .into_iter()
        .find(|p| p.name_any() == provider_name);
    let storage_class = provider
        .as_ref()
        .and_then(|p| p.spec.config.as_ref())
        .and_then(|cfg| cfg.pointer("/storageClass"))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_STORAGE_CLASS)
        .to_string();

    info!(%name, %ns, %pvc_name, provider = %provider_name, %storage_class, "provisioning SharedVolume backing PVC");

    // 4. SSA-apply the unowned RWO backing PVC. Idempotent: a re-apply of
    //    a deterministic-named PVC is a no-op (reattach); never recreated.
    let pvc_api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), &ns, &pvc_ar());
    let pvc_body = sv_pvc_object(&pvc_name, &ns, &sv.spec.size, &storage_class, &name);
    pvc_api
        .patch(&pvc_name, &apply_params(), &Patch::Apply(&pvc_body))
        .await?;

    // 5. Count the reference-ResourceClaims bound to this volume.
    let ref_count = current_ref_count(&ctx.client, &ns, &name).await?;

    // 6. capacity sample is wired in Task 11 (kubelet `df` over the PVC).
    let capacity: Option<(i64, i64)> = None; // T11: capacity sample

    // 7. Write the terminal status — ready / pvcRef / refCount / Ready
    //    condition, under our own field manager (never `.spec`).
    let prior = sv
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let cond = ready_condition(
        "True",
        "Provisioned",
        &format!("provisioned PVC {pvc_name} (class {storage_class})"),
        &prior,
    );
    patch_shared_volume_status(
        &ctx.client,
        &ns,
        &name,
        true,
        Some(&pvc_name),
        ref_count,
        capacity,
        cond,
    )
    .await?;

    // SharedVolume provisioning is deliberately counted under the shared
    // `claim_provisioned_total` metric with the synthetic `shared-disk`
    // backend label (SharedVolume has no ResourceClaim of its own).
    ctx.metrics
        .claim_provisioned_total
        .with_label_values(&["shared-disk", &ns])
        .inc();
    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, &ns, "ok"])
        .inc();
    info!(%name, %ns, %pvc_name, ref_count, "SharedVolume provisioned");

    Ok(Action::requeue(Duration::from_secs(300)))
}

/// Error policy for the SharedVolume controller: increment error metrics
/// and requeue after 30 seconds (mirrors the ResourceClaim provisioner).
pub fn error_policy_sv(sv: Arc<SharedVolume>, err: &ReconcileError, ctx: Arc<Context>) -> Action {
    let name = sv.name_any();
    let namespace = sv.namespace().unwrap_or_default();
    warn!(%name, %namespace, %err, "SharedVolume reconcile error");
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
// Status + finalizer I/O
// ---------------------------------------------------------------------------

/// List the namespace's ResourceClaims (raw JSON) and count those bound to
/// this SharedVolume via the `apprafter.io/shared-volume` label.
async fn current_ref_count(client: &Client, ns: &str, name: &str) -> Result<i64, ReconcileError> {
    let rc_ar = ApiResource::from_gvk(&GroupVersionKind::gvk(
        "apprafter.io",
        "v1alpha1",
        "ResourceClaim",
    ));
    let claims: Vec<Value> = Api::<DynamicObject>::namespaced_with(client.clone(), ns, &rc_ar)
        .list(&Default::default())
        .await?
        .items
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()?;
    Ok(ref_count_for(name, &claims))
}

/// SSA-patch the `SharedVolume` `.status` with `ready` / `pvcRef` /
/// `refCount` / `capacity` / the `Ready` condition, under the provisioner
/// field manager. Never touches `.spec`. The terminal body carries the
/// full owned field-set (SSA REPLACES the manager's set on each apply).
#[allow(clippy::too_many_arguments)]
async fn patch_shared_volume_status(
    client: &Client,
    ns: &str,
    name: &str,
    ready: bool,
    pvc_ref: Option<&str>,
    ref_count: i64,
    capacity: Option<(i64, i64)>,
    cond: SharedVolumeCondition,
) -> Result<(), ReconcileError> {
    let api: Api<SharedVolume> = Api::namespaced(client.clone(), ns);
    let body = sv_status_apply_body_with_condition(name, ready, pvc_ref, ref_count, capacity, cond);
    api.patch_status(name, &apply_params(), &Patch::Apply(&body))
        .await?;
    Ok(())
}

/// Merge-patch the SharedVolume's `metadata.finalizers` to `list`.
async fn set_finalizers(
    client: &Client,
    ns: &str,
    name: &str,
    list: Vec<String>,
) -> Result<(), ReconcileError> {
    let api: Api<SharedVolume> = Api::namespaced(client.clone(), ns);
    let patch = json!({ "metadata": { "finalizers": list } });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

/// `current` with the PVC-cleanup finalizer appended (idempotent).
fn with_finalizer(current: &[String]) -> Vec<String> {
    let mut out = current.to_vec();
    if !out.iter().any(|f| f == SV_PVC_FINALIZER) {
        out.push(SV_PVC_FINALIZER.to_string());
    }
    out
}

/// `current` without the PVC-cleanup finalizer.
fn without_finalizer(current: &[String]) -> Vec<String> {
    current
        .iter()
        .filter(|f| *f != SV_PVC_FINALIZER)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sv_pvc_object_is_rwo_unowned_labelled() {
        let p = sv_pvc_object("sv-demo-shared", "demo", "5Gi", "local-path", "shared");
        assert_eq!(p["spec"]["accessModes"], json!(["ReadWriteOnce"]));
        assert_eq!(p["spec"]["storageClassName"], "local-path");
        assert_eq!(p["spec"]["resources"]["requests"]["storage"], "5Gi");
        assert_eq!(
            p["metadata"]["labels"]["apprafter.io/shared-volume"],
            "shared"
        );
        assert!(p["metadata"].get("ownerReferences").is_none());
    }

    #[test]
    fn sv_pvc_name_is_deterministic_and_prefixed() {
        assert_eq!(sv_pvc_name("demo", "shared"), "sv-demo-shared");
    }

    #[test]
    fn ref_count_counts_claims_labelled_with_this_volume() {
        let claims = vec![
            json!({"metadata":{"labels":{"apprafter.io/shared-volume":"shared"}}}),
            json!({"metadata":{"labels":{"apprafter.io/shared-volume":"shared"}}}),
            json!({"metadata":{"labels":{"apprafter.io/shared-volume":"other"}}}),
            json!({"metadata":{"labels":{}}}),
        ];
        assert_eq!(ref_count_for("shared", &claims), 2);
    }

    #[test]
    fn sv_status_body_sets_ready_pvcref_refcount() {
        let body = sv_status_apply_body("shared", true, Some("sv-demo-shared"), 2, None);
        assert_eq!(body["status"]["ready"], true);
        assert_eq!(body["status"]["pvcRef"], "sv-demo-shared");
        assert_eq!(body["status"]["refCount"], 2);
    }

    #[test]
    fn sv_status_body_omits_pvcref_and_capacity_when_none() {
        let body = sv_status_apply_body("shared", false, None, 0, None);
        assert_eq!(body["status"]["ready"], false);
        assert_eq!(body["status"]["refCount"], 0);
        assert!(body["status"].get("pvcRef").is_none());
        assert!(body["status"].get("capacity").is_none());
        assert_eq!(body["kind"], "SharedVolume");
    }

    #[test]
    fn sv_status_body_carries_capacity_when_set() {
        let body = sv_status_apply_body("shared", true, Some("sv-demo-shared"), 1, Some((10, 100)));
        assert_eq!(body["status"]["capacity"]["usedBytes"], 10);
        assert_eq!(body["status"]["capacity"]["capacityBytes"], 100);
    }

    #[test]
    fn terminal_status_body_stamps_ready_condition() {
        let cond = ready_condition("True", "Provisioned", "ok", &[]);
        let body = sv_status_apply_body_with_condition(
            "shared",
            true,
            Some("sv-demo-shared"),
            2,
            None,
            cond,
        );
        assert_eq!(body["status"]["ready"], true);
        assert_eq!(body["status"]["pvcRef"], "sv-demo-shared");
        assert_eq!(body["status"]["refCount"], 2);
        assert_eq!(body["status"]["conditions"][0]["type"], "Ready");
        assert_eq!(body["status"]["conditions"][0]["status"], "True");
    }

    #[test]
    fn ready_condition_reuses_timestamp_when_status_unchanged() {
        let ts = "2026-01-01T00:00:00+00:00";
        let prev = vec![SharedVolumeCondition {
            type_: COND_READY.to_string(),
            status: "True".to_string(),
            last_transition_time: ts.to_string(),
            reason: Some("Provisioned".to_string()),
            message: Some("ok".to_string()),
        }];
        let c = ready_condition("True", "Provisioned", "ok", &prev);
        assert_eq!(c.last_transition_time, ts);
        assert_eq!(c.type_, COND_READY);
    }
}
