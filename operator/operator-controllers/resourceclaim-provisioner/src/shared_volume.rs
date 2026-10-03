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
use k8s_openapi::api::core::v1::{Node, ObjectReference};
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, Patch, PatchParams};
use kube::core::GroupVersionKind;
use kube::runtime::controller::Action;
use kube::runtime::events::{Event as KubeEvent, EventType, Reporter};
use kube::runtime::reflector::{ObjectRef, Store};
use kube::{Client, Resource, ResourceExt};
use operator_core::events::ObjectRecorder;
use serde_json::{json, Value};
use tracing::{info, warn};

use operator_core::matching::{select_provider, Candidate};
use operator_core::{
    ResourceClaim, ServiceProvider, SharedVolume, SharedVolumeCondition, SharedVolumeStatus,
    COND_CAPACITY_WARNING,
};

use crate::{Context, ReconcileError, FIELD_MANAGER};
use operator_core::capacity::{
    is_volume_warning, pvc_usage, should_emit_event, DEFAULT_VOLUME_FULL_THRESHOLD, SCOPE_HOST,
    SCOPE_VOLUME,
};

/// The `ServiceProvider.spec.type` a `SharedVolume` binds to. There is no
/// scheduler for `SharedVolume` CRs (they carry no selector), so the
/// reconcile selects the provider itself with an EMPTY selector — any
/// `shared-disk` provider qualifies (the seeded `shared-local`).
const SHARED_DISK_TYPE: &str = "shared-disk";

/// Kind string for log/metric labels.
const KIND: &str = "SharedVolume";

/// Condition type this controller owns on a `SharedVolume`.
const COND_READY: &str = "Ready";

/// `Reporter.controller` for `SharedVolume` capacity Warning Events.
const EVENT_REPORTER_CONTROLLER: &str = "apprafter-resourceclaim-provisioner";

/// Finalizer that reaps the backing PVC on `SharedVolume` delete. The PVC
/// is unowned (no `ownerReferences`), so without this finalizer the PVC
/// would leak when the CR is deleted.
const SV_PVC_FINALIZER: &str = "apprafter.io/sharedvolume-pvc-cleanup";

/// How long the decorative capacity sample — the Node LIST and the kubelet
/// Summary through the node proxy — may take before this pass stops waiting
/// for it (WI-400).
///
/// It runs BEFORE the terminal status write, so without a bound of its own a
/// wedged kubelet holds that write hostage: the reconcile deadline would cut
/// the whole pass and `refCount` — what `volume rm` reads — would stop moving
/// for every SharedVolume, this controller running one reconcile at a time.
/// Under [`operator_core::capacity::FETCH_TIMEOUT`] (15s, the kubelet call
/// alone) so it also covers the Node LIST and fires first; far under the
/// SharedVolume reconcile deadline (60s). On expiry the figure on the object
/// is carried forward ([`carried_capacity`]), never dropped.
pub const CAPACITY_SAMPLE_BUDGET: Duration = Duration::from_secs(10);

/// How long the edge-triggered `CapacityWarning` Event publish may take
/// (WI-400). It runs before the terminal status write (step 8 of
/// [`reconcile_shared_volume`]), so it must not be able to hold that write.
const CAPACITY_EVENT_BOUND: Duration = Duration::from_secs(5);

/// StorageClass the SharedVolume backend falls back to when the matched
/// `shared-disk` ServiceProvider config omits `/storageClass`.
const DEFAULT_STORAGE_CLASS: &str = "local-path";

/// Label the app-controller stamps on a `shared-disk` reference-claim (and
/// this controller stamps on the backing PVC) carrying the `SharedVolume`
/// name it binds to. The watch mapper ([`shared_volume_ref_for_claim`]) and
/// the refCount query (`apprafter.io/shared-volume=<name>`) both key off it.
const SHARED_VOLUME_LABEL: &str = "apprafter.io/shared-volume";

/// Deterministic unowned-PVC name for a SharedVolume.
///
/// The `sv-` prefix avoids any collision with owned-disk PVC names
/// (which are named `claim-<ns>-<app>-disk-<claim>`).
pub fn sv_pvc_name(ns: &str, name: &str) -> String {
    format!("sv-{ns}-{name}")
}

/// Map a `shared-disk` reference-`ResourceClaim` to the `SharedVolume` it
/// references, for the SharedVolume controller's `.watches()` fan-out.
///
/// `refCount` is computed by LISTING reference-claims labelled
/// `apprafter.io/shared-volume=<name>`, so it only refreshes when the
/// SharedVolume itself reconciles. Without this mapper a claim CREATE/DELETE
/// would not re-trigger the parent SharedVolume, leaving `refCount` stale
/// until the 300s requeue — a correctness bug, not just latency, because the
/// `volume rm` delete-guard reads `refCount` to decide whether the volume is
/// still in use (a stale `0` would wrongly allow deletion). This is the
/// standard kube-rs "reconcile the parent when a child changes" pattern.
///
/// Returns `None` for a claim with no `apprafter.io/shared-volume` label (not
/// a reference-claim) or no namespace (SharedVolume is namespaced).
pub fn shared_volume_ref_for_claim(claim: &ResourceClaim) -> Option<ObjectRef<SharedVolume>> {
    let ns = claim.metadata.namespace.as_deref()?;
    let name = claim.metadata.labels.as_ref()?.get(SHARED_VOLUME_LABEL)?;
    Some(ObjectRef::<SharedVolume>::new(name).within(ns))
}

/// The watch-mapper fan-out for a claim, FILTERED against the SharedVolume
/// reflector `store`: only emit the referenced `SharedVolume` if it actually
/// exists in the store (i.e. the runtime can reconcile it).
///
/// An ORPHAN reference-claim — labelled `apprafter.io/shared-volume=<gone>`
/// for a SharedVolume that was deleted or never existed — would otherwise map
/// to a ref the Controller runtime cannot resolve (`tried to reconcile object
/// SharedVolume/<gone> that was not found in local store`), erroring + requeue
/// every 30s forever (walk-found Bug C). Dropping the unresolvable ref kills
/// the storm. A SharedVolume that DOES exist self-reconciles on create + the
/// 300s requeue, so the rare not-yet-in-store window is at worst a brief
/// refCount-refresh delay (the safety net), never a missed reconcile.
pub fn shared_volume_refs_in_store(
    claim: &ResourceClaim,
    store: &Store<SharedVolume>,
) -> Option<ObjectRef<SharedVolume>> {
    shared_volume_ref_for_claim(claim).filter(|r| store.get(r).is_some())
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
                SHARED_VOLUME_LABEL: sv_name,
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
/// A sampled volume figure and WHICH THING it measured (D29).
///
/// Grouped rather than passed as two adjacent parameters: they are one fact,
/// and clippy's argument-count limit is the honest signal that a widening
/// parameter list wanted a type. Splitting them also invites the shape where a
/// caller passes the bytes and forgets the scope, which is precisely the
/// unlabelled figure this field exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SvCapacity {
    pub used: i64,
    pub cap: i64,
    /// `None` only when the node figure was unreadable — treated as unknown by
    /// every reader, never as `host`.
    pub scope: Option<&'static str>,
}

pub fn sv_status_apply_body(
    sv_name: &str,
    ready: bool,
    pvc_ref: Option<&str>,
    ref_count: i64,
    capacity: Option<SvCapacity>,
) -> Value {
    let mut status = json!({ "ready": ready, "refCount": ref_count });
    if let Some(p) = pvc_ref {
        status["pvcRef"] = json!(p);
    }
    if let Some(SvCapacity { used, cap, scope }) = capacity {
        let mut c = json!({ "usedBytes": used, "capacityBytes": cap });
        if let Some(scope) = scope {
            c["scope"] = json!(scope);
        }
        status["capacity"] = c;
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
    capacity: Option<SvCapacity>,
    cond: SharedVolumeCondition,
) -> Value {
    let mut body = sv_status_apply_body(sv_name, ready, pvc_ref, ref_count, capacity);
    body["status"]["conditions"] = json!([cond]);
    body
}

/// As [`sv_status_apply_body`] but stamps BOTH the `Ready` and the
/// `CapacityWarning` conditions in the single terminal `conditions` array.
///
/// SSA REPLACES the manager's owned field-set on each apply, so when this
/// controller owns two conditions they MUST both ride this one body — a
/// body carrying only `Ready` would prune the `CapacityWarning` it set on a
/// prior apply (and vice-versa). The `CapacityWarning` condition is
/// optional: a sample-less cycle (`None`) drops back to the single-`Ready`
/// shape so capacity simply goes absent rather than stamping a stale value.
pub fn sv_status_apply_body_with_conditions(
    sv_name: &str,
    ready: bool,
    pvc_ref: Option<&str>,
    ref_count: i64,
    capacity: Option<SvCapacity>,
    ready_cond: SharedVolumeCondition,
    capacity_cond: Option<SharedVolumeCondition>,
) -> Value {
    let mut body = sv_status_apply_body(sv_name, ready, pvc_ref, ref_count, capacity);
    body["status"]["conditions"] = match capacity_cond {
        Some(cap) => json!([ready_cond, cap]),
        None => json!([ready_cond]),
    };
    body
}

/// Build the `CapacityWarning` condition, preserving `lastTransitionTime`
/// when the `(type, status)` pair is unchanged (same hot-loop guard as
/// [`ready_condition`]).
pub fn capacity_warning_condition(
    status: &str,
    reason: &str,
    message: &str,
    previous: &[SharedVolumeCondition],
) -> SharedVolumeCondition {
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == COND_CAPACITY_WARNING && c.status == status)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    SharedVolumeCondition {
        type_: COND_CAPACITY_WARNING.to_string(),
        status: status.to_string(),
        last_transition_time,
        reason: Some(reason.to_string()),
        message: Some(message.to_string()),
    }
}

/// The capacity figure the object already reports, for a pass whose sample
/// did not answer within [`CAPACITY_SAMPLE_BUDGET`].
///
/// A sample that never answered says nothing about the volume, and dropping
/// the figure is not a neutral "unknown" here: the terminal status apply is
/// the whole field-set of [`crate::FIELD_MANAGER`], so an omitted
/// `status.capacity` is PRUNED — and the `CapacityWarning` with it, which also
/// re-arms the edge trigger, so a kubelet that keeps landing near the budget
/// would raise a fresh Warning Event every time it recovered. Carried forward,
/// the figure reads as it did before the pass, and the condition beside it
/// ([`carried_capacity_condition`]) says it was not re-measured.
///
/// (A sample that ANSWERED with nothing usable still drops the figure, as
/// before — that is a finding, not a missing answer.)
pub fn carried_capacity(status: Option<&SharedVolumeStatus>) -> Option<SvCapacity> {
    let capacity = status?.capacity.as_ref()?;
    Some(SvCapacity {
        used: capacity.used_bytes?,
        cap: capacity.capacity_bytes?,
        scope: match capacity.scope.as_deref() {
            Some(SCOPE_HOST) => Some(SCOPE_HOST),
            Some(SCOPE_VOLUME) => Some(SCOPE_VOLUME),
            _ => None,
        },
    })
}

/// What a carried `CapacityWarning` message ends with: the figure beside it
/// was not measured on this pass.
fn not_remeasured_note() -> String {
    format!(
        " — not re-measured: the kubelet did not answer within {}s",
        CAPACITY_SAMPLE_BUDGET.as_secs()
    )
}

/// The `CapacityWarning` condition the object already carries, for the same
/// pass as [`carried_capacity`] and for the same reason, with its message
/// saying that the figure was not re-measured.
///
/// Status, reason and `lastTransitionTime` stay as they were. Changing any of
/// them would describe the kubelet rather than the volume, and a warning that
/// stopped being `True` would re-arm the edge trigger. Only the message
/// changes, so the age of the figure shows on the object (and in `apprafter
/// volume status`, which prints the message while the warning is up) rather
/// than only in a log line. Carried again on the next miss, the message does
/// not grow: the note is replaced, never appended twice.
pub fn carried_capacity_condition(
    previous: &[SharedVolumeCondition],
) -> Option<SharedVolumeCondition> {
    let mut carried = previous
        .iter()
        .find(|c| c.type_ == COND_CAPACITY_WARNING)
        .cloned()?;
    let note = not_remeasured_note();
    let measured = carried.message.as_deref().unwrap_or_default();
    let measured = measured.strip_suffix(note.as_str()).unwrap_or(measured);
    carried.message = Some(format!("{measured}{note}"));
    Some(carried)
}

/// Whether the `previous` conditions show `CapacityWarning=True` (the prior
/// state used to edge-trigger the Warning Event).
pub fn was_capacity_warning(previous: &[SharedVolumeCondition]) -> bool {
    previous
        .iter()
        .any(|c| c.type_ == COND_CAPACITY_WARNING && c.status == "True")
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

/// How long one SharedVolume pass may run before it is abandoned (WI-400,
/// GOTCHA-51).
///
/// Short on purpose. This controller runs at `concurrency(1)`, so while one
/// pass is in flight kube-runtime holds every SharedVolume's triggers, and
/// `status.refCount`, the figure the `volume rm` guard reads, freezes for
/// all of them. A healthy pass is about six apiserver round trips and
/// sub-second. The worst first pass: three writes (the finalizer add, the
/// PVC apply and the status write) at a slow-but-healthy 10s each, the
/// admission webhook included; the provider and claim LISTs fast; the
/// capacity sample at `CAPACITY_SAMPLE_BUDGET` (10s) and the Event at
/// `CAPACITY_EVENT_BOUND` (5s). That comes to about 45s, and a steady-state
/// pass skips the finalizer write. 60s also ends an accepted-and-never-
/// answered request about 5x sooner than the client's 295s read timeout.
pub const RECONCILE_DEADLINE: Duration = Duration::from_secs(60);

/// [`reconcile_shared_volume`] under [`RECONCILE_DEADLINE`] — what
/// [`crate::run`] hands kube-runtime, never the unbounded reconcile.
///
/// A timed-out pass writes nothing to the SharedVolume's status — under
/// [`crate::FIELD_MANAGER`] a partial body would prune `refCount`, `pvcRef`,
/// `capacity` and both conditions. It reaches [`error_policy_sv`], which
/// warns, counts `apprafter_reconcile_timeouts_total{kind="SharedVolume"}`
/// and backs off, and it leaves a `ReconcileTimedOut` Warning Event on the
/// volume.
pub async fn reconcile_shared_volume_with_deadline(
    sv: Arc<SharedVolume>,
    ctx: Arc<Context>,
) -> Result<Action, ReconcileError> {
    let outcome = operator_core::deadline::within(
        RECONCILE_DEADLINE,
        reconcile_shared_volume(sv.clone(), ctx.clone()),
    )
    .await;
    if let Err(ReconcileError::TimedOut(timed_out)) = &outcome {
        crate::deadline_event::publish(&ctx.client, sv.object_ref(&()), KIND, *timed_out).await;
    }
    outcome
}

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

    let prior = sv
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();

    // 6. Sample capacity via the kubelet Summary API (BEST-EFFORT — any
    //    failure leaves `capacity = None` + no CapacityWarning, NEVER fails
    //    the reconcile). On single-node T1 the local-path PVC lives on the
    //    one node, so we sample the first node's kubelet for both the
    //    node-free fraction (warning trigger) and the PVC's own used/cap.
    //
    //    BOUNDED by `CAPACITY_SAMPLE_BUDGET` (WI-400): a sample that does not
    //    answer in time carries the object's own figure forward instead.
    let sample = tokio::time::timeout(CAPACITY_SAMPLE_BUDGET, async {
        match first_node_name(&ctx.client).await {
            Some(node) => ctx.capacity.summary_for_node(&ctx.client, &node).await,
            None => None,
        }
    })
    .await;
    let (sv_capacity, capacity_cond, now_warning) = match sample {
        Ok(summary) => {
            let capacity: Option<(i64, i64)> =
                summary.as_ref().and_then(|s| pvc_usage(s, &pvc_name));
            // D29: the same figure carries the same ambiguity here as on a
            // disk claim — a local-path PV makes the kubelet report the
            // backing filesystem, so these can be the node's numbers wearing
            // the volume's name. Decided from THIS summary, so the two
            // readings cannot be a poll apart.
            let sv_capacity: Option<SvCapacity> = capacity.map(|(used, cap)| SvCapacity {
                used,
                cap,
                scope: summary.as_ref().map(|s| {
                    operator_core::capacity::capacity_scope(
                        cap,
                        operator_core::capacity::node_fs_capacity(s),
                    )
                }),
            });

            // 2.22d (D8): `CapacityWarning` on a SharedVolume now means THE
            // VOLUME.
            //
            // It used to be derived from the NODE's free fraction, so a
            // condition named for a volume reported something else entirely —
            // and the volume's own usage, which the sampler above has always
            // collected, was written to `status.capacity` and never
            // thresholded. A volume at 99% of its own request on a healthy
            // node said nothing.
            //
            // The node signal has not been dropped; it moved to where it
            // belongs, as `NodeDiskPressure` on the PlatformStack singleton,
            // which every cluster has whether or not it has a SharedVolume.
            let now_warning = capacity
                .and_then(|(used, cap)| is_volume_warning(used, cap, DEFAULT_VOLUME_FULL_THRESHOLD))
                .unwrap_or(false);

            // Stamped only when the volume's own usage was sampled this
            // cycle; a sample-less cycle leaves the condition absent rather
            // than carrying a stale value forward.
            let capacity_cond = capacity.map(|(used, cap)| {
                let pct_used = if cap > 0 {
                    used as f64 / cap as f64 * 100.0
                } else {
                    0.0
                };
                if now_warning {
                    capacity_warning_condition(
                        "True",
                        "VolumeNearlyFull",
                        &format!(
                            "volume {pct_used:.1}% full (> {:.0}% threshold) — writes will fail \
                             when it reaches capacity",
                            DEFAULT_VOLUME_FULL_THRESHOLD * 100.0
                        ),
                        &prior,
                    )
                } else {
                    capacity_warning_condition(
                        "False",
                        "SufficientCapacity",
                        &format!("volume {pct_used:.1}% full"),
                        &prior,
                    )
                }
            });
            (sv_capacity, capacity_cond, now_warning)
        }
        Err(_elapsed) => {
            warn!(
                %name, %ns,
                budget_secs = CAPACITY_SAMPLE_BUDGET.as_secs(),
                "capacity: the kubelet sample did not answer in time — carrying the previous \
                 figure forward"
            );
            (
                carried_capacity(sv.status.as_ref()),
                carried_capacity_condition(&prior),
                was_capacity_warning(&prior),
            )
        }
    };

    // 7. Build the terminal conditions: `Ready`, plus the `CapacityWarning`
    //    step 6 sampled or carried. Step 9 writes them.
    let ready_cond = ready_condition(
        "True",
        "Provisioned",
        &format!("provisioned PVC {pvc_name} (class {storage_class})"),
        &prior,
    );

    // 8. Edge-triggered Warning Event on an OK→warning transition only
    //    (anti-spam). Best-effort: a publish failure is logged, not fatal.
    //
    //    Sent BEFORE the status write below, and bounded (WI-400). The edge
    //    is read from the condition the object carries, and the write below
    //    is what records the crossing — so with the Event after the write, a
    //    pass cut between the two (the reconcile deadline, or a crash) lost
    //    the Event for good: the next pass reads `was_warning = true`. In
    //    this order a cut before the write repeats the Event next pass
    //    instead; a duplicate Warning is the cheaper failure. A carried
    //    sample (step 6) takes `now_warning` from the same condition as
    //    `was_warning`, so it never sends one.
    let was_warning = was_capacity_warning(&prior);
    if should_emit_event(was_warning, now_warning) {
        let pct_used = sv_capacity
            .map(|c| {
                if c.cap > 0 {
                    c.used as f64 / c.cap as f64 * 100.0
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0);
        let recorder = build_recorder(&ctx.client, &sv);
        let ev = KubeEvent {
            type_: EventType::Warning,
            reason: "CapacityWarning".into(),
            note: Some(format!(
                "SharedVolume {name}: volume {pct_used:.1}% full (> {:.0}% threshold) — writes will fail when it reaches capacity",
                DEFAULT_VOLUME_FULL_THRESHOLD * 100.0
            )),
            action: "Provision".into(),
            secondary: None,
        };
        match tokio::time::timeout(CAPACITY_EVENT_BOUND, recorder.publish(ev)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                warn!(%name, %ns, error = %e, "failed to publish CapacityWarning event (continuing)")
            }
            Err(_) => warn!(
                %name, %ns, bound_secs = CAPACITY_EVENT_BOUND.as_secs(),
                "CapacityWarning event publish did not answer within its bound (continuing)"
            ),
        }
    }

    // 9. Write the terminal status — ready / pvcRef / refCount / capacity /
    //    BOTH the `Ready` and (when sampled or carried) the `CapacityWarning`
    //    conditions, under our own field manager (never `.spec`). SSA
    //    REPLACES the manager's field-set, so both conditions ride one body.
    patch_shared_volume_status_with_conditions(
        &ctx.client,
        &ns,
        &name,
        true,
        Some(&pvc_name),
        ref_count,
        sv_capacity,
        ready_cond,
        capacity_cond,
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
/// and requeue after 30 seconds (mirrors the ResourceClaim provisioner) — or,
/// for a pass abandoned at [`RECONCILE_DEADLINE`], count the timeout and back
/// off by one deadline, so a volume that stalls on every pass cannot hold the
/// controller's only slot most of the time. A watch event still runs it at
/// once.
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
    if let ReconcileError::TimedOut(timed_out) = err {
        ctx.metrics
            .reconcile_timeouts
            .with_label_values(&[KIND])
            .inc();
        return Action::requeue(timed_out.after);
    }
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
    capacity: Option<SvCapacity>,
    cond: SharedVolumeCondition,
) -> Result<(), ReconcileError> {
    let api: Api<SharedVolume> = Api::namespaced(client.clone(), ns);
    let body = sv_status_apply_body_with_condition(name, ready, pvc_ref, ref_count, capacity, cond);
    api.patch_status(name, &apply_params(), &Patch::Apply(&body))
        .await?;
    Ok(())
}

/// SSA-patch the `SharedVolume` `.status` carrying BOTH the `Ready` and
/// (when present) the `CapacityWarning` conditions in the single terminal
/// body. SSA REPLACES the manager's owned field-set, so both conditions
/// must ride this one apply (see [`sv_status_apply_body_with_conditions`]).
#[allow(clippy::too_many_arguments)]
async fn patch_shared_volume_status_with_conditions(
    client: &Client,
    ns: &str,
    name: &str,
    ready: bool,
    pvc_ref: Option<&str>,
    ref_count: i64,
    capacity: Option<SvCapacity>,
    ready_cond: SharedVolumeCondition,
    capacity_cond: Option<SharedVolumeCondition>,
) -> Result<(), ReconcileError> {
    let api: Api<SharedVolume> = Api::namespaced(client.clone(), ns);
    let body = sv_status_apply_body_with_conditions(
        name,
        ready,
        pvc_ref,
        ref_count,
        capacity,
        ready_cond,
        capacity_cond,
    );
    api.patch_status(name, &apply_params(), &Patch::Apply(&body))
        .await?;
    Ok(())
}

/// Best-effort name of the first node in the cluster (single-node T1 hosts
/// the local-path PVCs). Returns `None` on any list failure — capacity
/// sampling is decorative and must never fail the reconcile.
async fn first_node_name(client: &Client) -> Option<String> {
    match Api::<Node>::all(client.clone())
        .list(&Default::default())
        .await
    {
        Ok(nodes) => nodes.items.first().map(|n| n.name_any()),
        Err(e) => {
            warn!(error = %e, "capacity: failed to list nodes (continuing without capacity)");
            None
        }
    }
}

/// Build a `Recorder` that publishes Events against the given
/// `SharedVolume`. Per-reconcile construction keeps the reconcile pure;
/// `Recorder::new` is cheap.
fn build_recorder(client: &Client, sv: &SharedVolume) -> ObjectRecorder {
    let reporter = Reporter {
        controller: EVENT_REPORTER_CONTROLLER.into(),
        instance: std::env::var("POD_NAME").ok(),
    };
    let reference: ObjectReference = sv.object_ref(&());
    ObjectRecorder::new(client.clone(), reporter, reference)
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

    /// Build a `ResourceClaim` with the given namespace + optional
    /// `apprafter.io/shared-volume` label for the watch-mapper tests.
    fn claim_with(ns: Option<&str>, sv_label: Option<&str>) -> ResourceClaim {
        use operator_core::ResourceClaimSpec;
        let mut claim = ResourceClaim::new("ref-claim", ResourceClaimSpec::default());
        claim.metadata.namespace = ns.map(str::to_string);
        if let Some(v) = sv_label {
            claim.metadata.labels = Some(
                [(SHARED_VOLUME_LABEL.to_string(), v.to_string())]
                    .into_iter()
                    .collect(),
            );
        }
        claim
    }

    #[test]
    fn watch_mapper_targets_the_referenced_shared_volume() {
        let claim = claim_with(Some("demo"), Some("shared"));
        let want = ObjectRef::<SharedVolume>::new("shared").within("demo");
        assert_eq!(shared_volume_ref_for_claim(&claim), Some(want));
    }

    #[test]
    fn watch_mapper_is_none_without_the_label() {
        // A claim that is not a shared-disk reference-claim must not fan a
        // reconcile to any SharedVolume.
        let claim = claim_with(Some("demo"), None);
        assert_eq!(shared_volume_ref_for_claim(&claim), None);
    }

    #[test]
    fn watch_mapper_is_none_without_a_namespace() {
        // SharedVolume is namespaced; a namespace-less claim can't target one.
        let claim = claim_with(None, Some("shared"));
        assert_eq!(shared_volume_ref_for_claim(&claim), None);
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

    /// Build a `Store<SharedVolume>` seeded with the given SharedVolumes
    /// (name, ns), mirroring what the reflector holds at runtime.
    fn store_with(svs: &[(&str, &str)]) -> Store<SharedVolume> {
        use kube::runtime::reflector::store;
        use kube::runtime::watcher;
        use operator_core::SharedVolumeSpec;
        let (reader, mut writer) = store::<SharedVolume>();
        for (name, ns) in svs {
            let mut sv = SharedVolume::new(name, SharedVolumeSpec::default());
            sv.metadata.namespace = Some((*ns).to_string());
            writer.apply_watcher_event(&watcher::Event::Apply(sv));
        }
        reader
    }

    #[test]
    fn store_filtered_mapper_emits_ref_when_shared_volume_exists() {
        // The referenced SharedVolume IS in the store → fan the reconcile out
        // so refCount refreshes promptly (the correctness path the mapper
        // protects).
        let store = store_with(&[("shared", "demo")]);
        let claim = claim_with(Some("demo"), Some("shared"));
        let want = ObjectRef::<SharedVolume>::new("shared").within("demo");
        assert_eq!(shared_volume_refs_in_store(&claim, &store), Some(want));
    }

    #[test]
    fn store_filtered_mapper_drops_orphan_ref_to_missing_shared_volume() {
        // Walk-found Bug C: an ORPHAN reference-claim labelled
        // `apprafter.io/shared-volume=does-not-exist` maps to a SharedVolume
        // the runtime cannot find in its store, which would error + requeue
        // every 30s forever. The store filter drops the unresolvable ref so
        // NO fan-out (and no storm) is produced.
        let store = store_with(&[("shared", "demo")]);
        let orphan = claim_with(Some("demo"), Some("does-not-exist"));
        assert_eq!(shared_volume_refs_in_store(&orphan, &store), None);
    }

    #[test]
    fn store_filtered_mapper_drops_ref_in_a_different_namespace() {
        // SharedVolume is namespaced — a same-named SharedVolume in ANOTHER
        // namespace must not satisfy the filter (the ObjectRef carries the ns).
        let store = store_with(&[("shared", "other-ns")]);
        let claim = claim_with(Some("demo"), Some("shared"));
        assert_eq!(shared_volume_refs_in_store(&claim, &store), None);
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
        let body = sv_status_apply_body(
            "shared",
            true,
            Some("sv-demo-shared"),
            1,
            Some(SvCapacity {
                used: 10,
                cap: 100,
                scope: Some("volume"),
            }),
        );
        assert_eq!(body["status"]["capacity"]["usedBytes"], 10);
        assert_eq!(body["status"]["capacity"]["capacityBytes"], 100);
        assert_eq!(body["status"]["capacity"]["scope"], "volume");
    }

    #[test]
    fn sv_status_body_omits_scope_when_the_node_figure_was_unreadable() {
        // D29: absence is UNKNOWN. Writing `host` on a missing node figure
        // would relabel a correct volume number on any cluster whose kubelet
        // stopped reporting node.fs, and readers fall back to the pre-D29
        // rendering only because the key is genuinely absent.
        let body = sv_status_apply_body(
            "shared",
            true,
            Some("sv-demo-shared"),
            1,
            Some(SvCapacity {
                used: 10,
                cap: 100,
                scope: None,
            }),
        );
        assert_eq!(body["status"]["capacity"]["usedBytes"], 10);
        assert!(body["status"]["capacity"].get("scope").is_none());
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

    #[test]
    fn dual_condition_body_carries_both_when_capacity_present() {
        let ready = ready_condition("True", "Provisioned", "ok", &[]);
        let cap = capacity_warning_condition("True", "NodeNearlyFull", "10% free", &[]);
        let body = sv_status_apply_body_with_conditions(
            "shared",
            true,
            Some("sv-demo-shared"),
            1,
            Some(SvCapacity {
                used: 90,
                cap: 100,
                scope: Some("volume"),
            }),
            ready,
            Some(cap),
        );
        let conds = body["status"]["conditions"].as_array().unwrap();
        assert_eq!(conds.len(), 2);
        assert_eq!(conds[0]["type"], "Ready");
        assert_eq!(conds[1]["type"], "CapacityWarning");
        assert_eq!(conds[1]["status"], "True");
        assert_eq!(body["status"]["capacity"]["usedBytes"], 90);
    }

    #[test]
    fn dual_condition_body_carries_only_ready_when_no_capacity_sample() {
        let ready = ready_condition("True", "Provisioned", "ok", &[]);
        let body = sv_status_apply_body_with_conditions(
            "shared",
            true,
            Some("sv-demo-shared"),
            1,
            None,
            ready,
            None,
        );
        let conds = body["status"]["conditions"].as_array().unwrap();
        assert_eq!(conds.len(), 1);
        assert_eq!(conds[0]["type"], "Ready");
        assert!(body["status"].get("capacity").is_none());
    }

    #[test]
    fn capacity_warning_condition_reuses_timestamp_when_unchanged() {
        let ts = "2026-01-01T00:00:00+00:00";
        let prev = vec![SharedVolumeCondition {
            type_: COND_CAPACITY_WARNING.to_string(),
            status: "True".to_string(),
            last_transition_time: ts.to_string(),
            reason: Some("NodeNearlyFull".to_string()),
            message: Some("low".to_string()),
        }];
        let c = capacity_warning_condition("True", "NodeNearlyFull", "low", &prev);
        assert_eq!(c.last_transition_time, ts);
        assert_eq!(c.type_, COND_CAPACITY_WARNING);
    }

    #[test]
    fn was_capacity_warning_reads_prior_true_only() {
        let warned = vec![SharedVolumeCondition {
            type_: COND_CAPACITY_WARNING.to_string(),
            status: "True".to_string(),
            last_transition_time: "t".to_string(),
            reason: None,
            message: None,
        }];
        assert!(was_capacity_warning(&warned));
        let cleared = vec![SharedVolumeCondition {
            type_: COND_CAPACITY_WARNING.to_string(),
            status: "False".to_string(),
            last_transition_time: "t".to_string(),
            reason: None,
            message: None,
        }];
        assert!(!was_capacity_warning(&cleared));
        assert!(!was_capacity_warning(&[]));
    }
}

/// The capacity sample's bound (WI-400), driven through the real reconcile
/// against a scripted apiserver whose kubelet proxy can be told to never
/// answer.
#[cfg(test)]
mod capacity_budget_tests {
    use super::*;

    use operator_core::{Metrics, SharedVolumeCapacity, SharedVolumeSpec};

    use crate::route_apiserver::{apiserver, calls_to, route, Reply, Route};

    const STATUS_PATH: &str =
        "/apis/apprafter.io/v1alpha1/namespaces/apps/sharedvolumes/data/status";
    const SUMMARY_PATH: &str = "/api/v1/nodes/n1/proxy/stats/summary";
    const T0: &str = "2026-09-01T00:00:00+00:00";

    fn cond(type_: &str, status: &str, reason: &str) -> SharedVolumeCondition {
        SharedVolumeCondition {
            type_: type_.into(),
            status: status.into(),
            last_transition_time: T0.into(),
            reason: Some(reason.into()),
            message: Some(format!("{reason} as of the last sample")),
        }
    }

    /// `apps/data`, finalizer on, last sampled 95% full with the warning up.
    fn nearly_full_volume() -> Arc<SharedVolume> {
        let mut sv = SharedVolume::new(
            "data",
            SharedVolumeSpec {
                size: "1Gi".into(),
                class: None,
            },
        );
        sv.metadata.namespace = Some("apps".into());
        sv.metadata.finalizers = Some(vec![SV_PVC_FINALIZER.into()]);
        sv.status = Some(SharedVolumeStatus {
            ready: Some(true),
            pvc_ref: Some("sv-apps-data".into()),
            ref_count: Some(0),
            capacity: Some(SharedVolumeCapacity {
                used_bytes: Some(950),
                capacity_bytes: Some(1000),
                scope: Some("volume".into()),
            }),
            conditions: Some(vec![
                cond("Ready", "True", "Provisioned"),
                cond(COND_CAPACITY_WARNING, "True", "VolumeNearlyFull"),
            ]),
        });
        Arc::new(sv)
    }

    /// Every request the reconcile makes, with the kubelet Summary answering
    /// `summary`.
    fn routes(summary: Reply) -> Vec<Route> {
        vec![
            route(
                "GET",
                "/apis/apprafter.io/v1alpha1/serviceproviders",
                Reply::Json(
                    200,
                    json!({
                        "apiVersion": "apprafter.io/v1alpha1", "kind": "ServiceProviderList",
                        "metadata": { "resourceVersion": "1" },
                        "items": [{
                            "apiVersion": "apprafter.io/v1alpha1", "kind": "ServiceProvider",
                            "metadata": { "name": "shared-local", "namespace": "apprafter-system" },
                            "spec": { "type": "shared-disk", "backend": "local-path" },
                        }],
                    }),
                ),
            ),
            route(
                "PATCH",
                "/api/v1/namespaces/apps/persistentvolumeclaims/sv-apps-data",
                Reply::Json(
                    200,
                    json!({
                        "apiVersion": "v1", "kind": "PersistentVolumeClaim",
                        "metadata": { "name": "sv-apps-data", "namespace": "apps" },
                    }),
                ),
            ),
            route(
                "GET",
                "/apis/apprafter.io/v1alpha1/namespaces/apps/resourceclaims",
                Reply::Json(
                    200,
                    json!({
                        "apiVersion": "apprafter.io/v1alpha1", "kind": "ResourceClaimList",
                        "metadata": { "resourceVersion": "1" }, "items": [],
                    }),
                ),
            ),
            route(
                "GET",
                "/api/v1/nodes",
                Reply::Json(
                    200,
                    json!({
                        "apiVersion": "v1", "kind": "NodeList",
                        "metadata": { "resourceVersion": "1" },
                        "items": [{ "apiVersion": "v1", "kind": "Node", "metadata": { "name": "n1" } }],
                    }),
                ),
            ),
            route("GET", SUMMARY_PATH, summary),
            route(
                "PATCH",
                STATUS_PATH,
                Reply::Json(
                    200,
                    json!({
                        "apiVersion": "apprafter.io/v1alpha1", "kind": "SharedVolume",
                        "metadata": { "name": "data", "namespace": "apps" },
                        "spec": { "size": "1Gi" },
                    }),
                ),
            ),
        ]
    }

    #[tokio::test(start_paused = true)]
    async fn a_kubelet_that_never_answers_carries_the_previous_figure_forward() {
        let (client, log) = apiserver(routes(Reply::Never));
        let ctx = Arc::new(Context::new(client, Arc::new(Metrics::new())));
        let started = tokio::time::Instant::now();
        // Bounded from outside as well (the SharedVolume reconcile deadline),
        // so a missing budget fails the test instead of hanging it.
        let action = tokio::time::timeout(
            Duration::from_secs(60),
            reconcile_shared_volume(nearly_full_volume(), ctx),
        )
        .await
        .expect("the capacity sample must not hold the reconcile")
        .expect("a missing sample never fails the reconcile");

        assert_eq!(action, Action::requeue(Duration::from_secs(300)));
        assert_eq!(started.elapsed(), CAPACITY_SAMPLE_BUDGET);
        assert_eq!(calls_to(&log, "GET", SUMMARY_PATH).len(), 1);

        let writes = calls_to(&log, "PATCH", STATUS_PATH);
        assert_eq!(writes.len(), 1, "{writes:?}");
        let status = &writes[0].body["status"];
        // Under SSA an omitted field is PRUNED: the figure and the warning
        // must ride the write exactly as the object had them.
        assert_eq!(
            status["capacity"],
            json!({ "usedBytes": 950, "capacityBytes": 1000, "scope": "volume" })
        );
        let warning = status["conditions"]
            .as_array()
            .expect("conditions")
            .iter()
            .find(|c| c["type"] == COND_CAPACITY_WARNING)
            .expect("the CapacityWarning rides the write");
        assert_eq!(warning["status"], "True");
        assert_eq!(warning["reason"], "VolumeNearlyFull");
        assert_eq!(warning["lastTransitionTime"], T0);
        // …and it says the figure is the last one that answered.
        assert_eq!(
            warning["message"],
            "VolumeNearlyFull as of the last sample — not re-measured: the kubelet did not \
             answer within 10s"
        );
        // …so the edge trigger is not re-armed: no fresh Warning Event.
        assert!(
            !log.lock()
                .expect("log")
                .iter()
                .any(|c| c.path.ends_with("/events")),
            "no Event may be published for a carried warning"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_kubelet_that_answers_still_sets_the_fresh_figure() {
        let summary = json!({
            "node": { "nodeName": "n1", "fs": { "capacityBytes": 100_000, "availableBytes": 50_000 } },
            "pods": [{ "volume": [{
                "name": "data", "pvcRef": { "name": "sv-apps-data", "namespace": "apps" },
                "usedBytes": 100, "capacityBytes": 1000,
            }] }],
        });
        let (client, log) = apiserver(routes(Reply::Json(200, summary)));
        let ctx = Arc::new(Context::new(client, Arc::new(Metrics::new())));
        let started = tokio::time::Instant::now();
        reconcile_shared_volume(nearly_full_volume(), ctx)
            .await
            .expect("reconcile");

        assert_eq!(
            started.elapsed(),
            Duration::ZERO,
            "nothing waited on the budget"
        );
        let writes = calls_to(&log, "PATCH", STATUS_PATH);
        let status = &writes[0].body["status"];
        assert_eq!(
            status["capacity"],
            json!({ "usedBytes": 100, "capacityBytes": 1000, "scope": "volume" })
        );
        let warning = status["conditions"]
            .as_array()
            .expect("conditions")
            .iter()
            .find(|c| c["type"] == COND_CAPACITY_WARNING)
            .expect("a sampled volume carries the condition");
        assert_eq!(warning["status"], "False");
        assert_eq!(warning["reason"], "SufficientCapacity");
    }

    #[test]
    fn nothing_is_carried_that_the_object_did_not_have() {
        // A carried figure is the object's own, never a fabricated one.
        assert_eq!(carried_capacity(None), None);
        assert_eq!(carried_capacity(Some(&SharedVolumeStatus::default())), None);
        assert_eq!(carried_capacity_condition(&[]), None);
    }

    #[test]
    fn a_carried_figure_keeps_its_scope() {
        let status = SharedVolumeStatus {
            capacity: Some(SharedVolumeCapacity {
                used_bytes: Some(1),
                capacity_bytes: Some(2),
                scope: Some("host".into()),
            }),
            ..Default::default()
        };
        assert_eq!(
            carried_capacity(Some(&status)),
            Some(SvCapacity {
                used: 1,
                cap: 2,
                scope: Some(SCOPE_HOST),
            })
        );
    }

    #[test]
    fn a_carried_warning_says_once_that_it_was_not_remeasured() {
        let first = carried_capacity_condition(&[cond(
            COND_CAPACITY_WARNING,
            "False",
            "SufficientCapacity",
        )])
        .expect("carried");
        assert_eq!(
            first.message.as_deref(),
            Some(
                "SufficientCapacity as of the last sample — not re-measured: the kubelet did \
                 not answer within 10s"
            )
        );
        assert_eq!(first.status, "False");
        assert_eq!(first.reason.as_deref(), Some("SufficientCapacity"));
        assert_eq!(first.last_transition_time, T0);
        // A second miss in a row carries the same message, not a longer one.
        let again = carried_capacity_condition(std::slice::from_ref(&first)).expect("carried");
        assert_eq!(again, first);
    }
}

/// WI-400: the order and the bound of the SharedVolume reconcile's
/// edge-triggered Event, and its deadline, driven through the scripted
/// apiserver (`crate::route_apiserver`) on a paused clock.
#[cfg(test)]
mod deadline_tests {
    use super::*;
    use crate::route_apiserver::{apiserver, calls_to, route, Reply, Route};
    use operator_core::{Metrics, SharedVolumeSpec};

    /// Longer than any bound under test, so a bound that stops cutting fails
    /// the test instead of hanging it.
    const OUTER_GUARD: Duration = Duration::from_secs(600);

    const PROVIDERS: &str = "/apis/apprafter.io/v1alpha1/serviceproviders";
    const PVC: &str = "/api/v1/namespaces/apps/persistentvolumeclaims/sv-apps-data";
    const CLAIMS: &str = "/apis/apprafter.io/v1alpha1/namespaces/apps/resourceclaims";
    const NODES: &str = "/api/v1/nodes";
    const SUMMARY: &str = "/api/v1/nodes/n1/proxy/stats/summary";
    const EVENTS: &str = "/apis/events.k8s.io/v1/namespaces/apps/events";
    const STATUS: &str = "/apis/apprafter.io/v1alpha1/namespaces/apps/sharedvolumes/data/status";

    fn context(client: Client) -> Arc<Context> {
        Arc::new(Context::new(client, Arc::new(Metrics::new())))
    }

    fn ok(body: Value) -> Reply {
        Reply::Json(200, body)
    }

    /// SharedVolume `apps/data`, finalizer already in place, never sampled.
    fn volume() -> Arc<SharedVolume> {
        let mut sv = SharedVolume::new(
            "data",
            SharedVolumeSpec {
                size: "1Gi".into(),
                class: None,
            },
        );
        sv.metadata.namespace = Some("apps".into());
        sv.metadata.uid = Some("u-sv".into());
        sv.metadata.finalizers = Some(vec![SV_PVC_FINALIZER.into()]);
        Arc::new(sv)
    }

    /// The bare object an apiserver hands back from a SharedVolume write.
    fn volume_object() -> Value {
        json!({
            "apiVersion": "apprafter.io/v1alpha1", "kind": "SharedVolume",
            "metadata": { "name": "data", "namespace": "apps" },
            "spec": { "size": "1Gi" },
        })
    }

    /// What an apiserver hands back from an Event create.
    fn created_event() -> Reply {
        Reply::Json(
            201,
            json!({
                "apiVersion": "events.k8s.io/v1", "kind": "Event",
                "metadata": { "name": "data.1", "namespace": "apps" },
            }),
        )
    }

    fn pvc_applied() -> Reply {
        ok(json!({
            "apiVersion": "v1", "kind": "PersistentVolumeClaim",
            "metadata": { "name": "sv-apps-data", "namespace": "apps" },
        }))
    }

    /// A kubelet Summary reporting the backing PVC at 95% of its capacity.
    fn nearly_full_summary() -> Reply {
        ok(json!({
            "node": { "nodeName": "n1", "fs": { "capacityBytes": 100_000, "availableBytes": 50_000 } },
            "pods": [{ "volume": [{
                "name": "data", "pvcRef": { "name": "sv-apps-data", "namespace": "apps" },
                "usedBytes": 950, "capacityBytes": 1000,
            }] }],
        }))
    }

    /// Every request a provisioning pass makes; the PVC apply, the Event
    /// publish and the status write answer as given.
    fn provisioning(pvc: Reply, event: Reply, status: Reply) -> Vec<Route> {
        vec![
            route(
                "GET",
                PROVIDERS,
                ok(json!({
                    "apiVersion": "apprafter.io/v1alpha1", "kind": "ServiceProviderList",
                    "metadata": {},
                    "items": [{
                        "apiVersion": "apprafter.io/v1alpha1", "kind": "ServiceProvider",
                        "metadata": { "name": "shared-local", "namespace": "apprafter-system" },
                        "spec": { "type": "shared-disk", "backend": "shared-disk" },
                    }],
                })),
            ),
            route("PATCH", PVC, pvc),
            route(
                "GET",
                CLAIMS,
                ok(json!({
                    "apiVersion": "apprafter.io/v1alpha1", "kind": "ResourceClaimList",
                    "metadata": {}, "items": [],
                })),
            ),
            route(
                "GET",
                NODES,
                ok(json!({
                    "apiVersion": "v1", "kind": "NodeList", "metadata": {},
                    "items": [{ "apiVersion": "v1", "kind": "Node", "metadata": { "name": "n1" } }],
                })),
            ),
            route("GET", SUMMARY, nearly_full_summary()),
            route("POST", EVENTS, event),
            route("PATCH", STATUS, status),
        ]
    }

    /// The edge-triggered Event goes out BEFORE the status write that
    /// records the crossing. Here that write never answers, so this is the
    /// pass a deadline would cut, and the Event has still been sent. In the
    /// old order (write, then Event) it never was, and the next pass, reading
    /// `CapacityWarning=True` back, never sent it either.
    #[tokio::test(start_paused = true)]
    async fn the_capacity_warning_event_is_sent_before_the_status_write() {
        let (client, log) = apiserver(provisioning(pvc_applied(), created_event(), Reply::Never));

        let cut = tokio::time::timeout(
            Duration::from_secs(60),
            reconcile_shared_volume(volume(), context(client)),
        )
        .await;
        assert!(
            cut.is_err(),
            "the status write never answers, so the pass is cut"
        );

        let log = log.lock().expect("log").clone();
        let event_at = log
            .iter()
            .position(|c| c.method == "POST" && c.path == EVENTS)
            .expect("the CapacityWarning Event was sent before the cut");
        assert_eq!(log[event_at].body["reason"], json!("CapacityWarning"));
        let status_at = log
            .iter()
            .position(|c| c.method == "PATCH" && c.path == STATUS)
            .expect("the status write was attempted");
        assert!(
            event_at < status_at,
            "Event first, then the write: {log:#?}"
        );
    }

    /// The Event cannot hold the status write either: a publish that never
    /// answers is abandoned at its bound, and the write follows, carrying the
    /// warning.
    #[tokio::test(start_paused = true)]
    async fn a_hung_event_publish_does_not_hold_the_status_write() {
        let (client, log) = apiserver(provisioning(
            pvc_applied(),
            Reply::Never,
            ok(volume_object()),
        ));

        let started = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(
            OUTER_GUARD,
            reconcile_shared_volume(volume(), context(client)),
        )
        .await
        .expect("a hung Event publish must not hold the pass");

        assert_eq!(
            outcome.expect("provisioned"),
            Action::requeue(Duration::from_secs(300))
        );
        assert_eq!(started.elapsed(), CAPACITY_EVENT_BOUND);
        let writes = calls_to(&log, "PATCH", STATUS);
        assert_eq!(writes.len(), 1, "{writes:?}");
        let warning = writes[0].body["status"]["conditions"]
            .as_array()
            .expect("conditions")
            .iter()
            .find(|c| c["type"] == COND_CAPACITY_WARNING)
            .cloned()
            .expect("the CapacityWarning rides the write");
        assert_eq!(warning["status"], json!("True"));
        assert_eq!(warning["reason"], json!("VolumeNearlyFull"));
    }

    /// A pass that never returns is abandoned at the deadline; every request
    /// hangs here, the Event publish included.
    #[tokio::test(start_paused = true)]
    async fn a_volume_pass_that_never_returns_is_abandoned_at_the_deadline() {
        let ctx = context(operator_core::testing::stalled_client());

        let started = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(
            RECONCILE_DEADLINE * 2,
            reconcile_shared_volume_with_deadline(volume(), ctx),
        )
        .await
        .expect("the deadline must end the pass");

        match outcome {
            Err(ReconcileError::TimedOut(t)) => assert_eq!(t.after, RECONCILE_DEADLINE),
            other => panic!("expected TimedOut, got {other:?}"),
        }
        assert_eq!(
            started.elapsed(),
            RECONCILE_DEADLINE + crate::deadline_event::PUBLISH_BOUND
        );
    }

    /// The timeout path writes nothing to the volume's status: a body without
    /// `refCount` would prune the figure `volume rm` reads. The pass is cut on
    /// its PVC apply; the only request after the cut is the Event.
    #[tokio::test(start_paused = true)]
    async fn a_timed_out_volume_pass_writes_no_status() {
        let (client, log) = apiserver(provisioning(
            Reply::Never,
            created_event(),
            ok(volume_object()),
        ));

        let outcome = tokio::time::timeout(
            RECONCILE_DEADLINE * 2,
            reconcile_shared_volume_with_deadline(volume(), context(client)),
        )
        .await
        .expect("the deadline must end the pass");
        assert!(
            matches!(outcome, Err(ReconcileError::TimedOut(_))),
            "{outcome:?}"
        );

        let log = log.lock().expect("log").clone();
        assert!(
            !log.iter().any(|c| c.path.ends_with("/status")),
            "a timed-out pass must not touch status: {log:#?}"
        );
        let methods: Vec<&str> = log.iter().map(|c| c.method.as_str()).collect();
        assert_eq!(methods, vec!["GET", "PATCH", "POST"], "{log:#?}");
        assert_eq!(log[2].body["reason"], json!("ReconcileTimedOut"));
        assert_eq!(log[2].body["regarding"]["kind"], json!("SharedVolume"));
    }

    #[tokio::test]
    async fn error_policy_sv_counts_a_timeout_and_backs_off_one_deadline() {
        let ctx = context(operator_core::testing::stalled_client());
        let timed_out = ReconcileError::TimedOut(operator_core::deadline::ReconcileTimedOut {
            after: RECONCILE_DEADLINE,
        });

        assert_eq!(
            error_policy_sv(volume(), &timed_out, ctx.clone()),
            Action::requeue(RECONCILE_DEADLINE)
        );
        let timeouts = || {
            ctx.metrics
                .reconcile_timeouts
                .with_label_values(&[KIND])
                .get()
        };
        assert_eq!(timeouts(), 1.0);
        assert_eq!(
            error_policy_sv(
                volume(),
                &ReconcileError::Provisioning("x".into()),
                ctx.clone()
            ),
            Action::requeue(Duration::from_secs(30))
        );
        assert_eq!(timeouts(), 1.0, "only a timeout counts as one");
    }
}
