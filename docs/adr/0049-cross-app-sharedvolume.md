# ADR 0049: cross-app SharedVolume

## Status

`Accepted` (2026-06-19).

ADR-first for Phase 2.6c (`plan.md` §2.6c). Ships as a coordinated
operator + platform-stack + CLI release (new CRD — additive).

## Context

Phase 2.6b (ADR 0043) ships `needs.disk` as an owned-PVC claim: each
Application gets its own PVC, sized at declaration time, retained for 7 days
after the app is removed. This covers the single-app stateful case well
(SQLite, local queues, single-process caches).

Two gaps remain:

**Gap 1 — multiple Applications sharing one directory.** A common migration
pattern from process managers (pm2, systemd with a shared path) involves
sibling processes writing to the same folder. With owned disks each app gets
its own PVC and cannot see the other's writes. A shared-volume primitive is
the missing piece.

**Gap 2 — node-level capacity visibility.** A local-path PVC on a single
node can fill the node's root filesystem without any visible signal. The
`local-path` provisioner does not expose quota or usage metrics; the only
source of truth is the kubelet's Summary API. Without a capacity signal,
operators discover a full disk only when writes start failing silently.

These two gaps are addressed together in 2.6c because the capacity signal
is most naturally surfaced on the `SharedVolume` object (which is the
explicit, named, long-lived resource), and owned-disk capacity (surfaced on
the Application or ResourceClaim) is deferred (see Deferred below).

## Decision

### 1. A new `SharedVolume` CRD (namespaced, `apprafter.io/v1alpha1`)

`SharedVolume` is an explicit, platform-managed resource with its own
lifecycle, independent of any Application:

```
spec:
  size:  "2Gi"         # Kubernetes storage quantity
  class: "local"       # optional; default "local" (T1); "nfs" on T2
status:
  ready:     true
  pvcRef:    "sv-apps-shared-uploads"
  refCount:  2
  capacity:
    usedBytes:     …
    capacityBytes: …
  conditions:
    - type: Ready
    - type: CapacityWarning
```

The CLI surface is `apprafter volume create/rm/list/status`. A delete is
refused while `status.refCount > 0` (still referenced).

### 2. The `needs.disk.ref` reference concept in the claim grammar

ADR 0043 introduced the `(type, name)` claim identity as a cross-cutting
concept — the named multi-claim format generalizes for all need types.
This ADR extends the claim grammar with a second cross-cutting concept:
the **reference** vs **owned** discrimination.

A disk need carries either `size` (owned: generate a new ResourceClaim →
provision a new PVC) or `ref` (reference: bind an existing SharedVolume →
mount its backing PVC directly). The two fields are mutually exclusive,
enforced by the admission webhook.

```cue
needs: disk: { ref: "shared-uploads", mountPath: "/uploads" }
```

The `ref` field names a `SharedVolume` in the Application's own namespace.
A reference-disk need does not generate a `ResourceClaim` in the standard
pipeline; the operator renderer resolves the SharedVolume's `status.pvcRef`
and adds the `volumeMount` + `volume{persistentVolumeClaim}` block directly.

This reference concept is **cross-cutting**: future need types (shared
queues, shared caches) could adopt a `ref` field under the same semantics
without revisiting the discriminator model.

### 3. `shared-disk` backend — a separate scheduler type from `disk`

SharedVolume uses backend type `shared-disk` (not `disk`). This keeps the
scheduler from cross-matching: an owned-disk ServiceProvider (`backend:
disk`) never satisfies a shared-volume lookup, and the `shared-local`
provider (`backend: shared-disk`) never satisfies a `type: disk`
ResourceClaim. The separation is load-bearing for T2 where both `disk-local`
and `shared-nfs` coexist.

### 4. `shared-local` → `shared-nfs` provider line (Tier-1 / Tier-2)

On Tier-1 the platform seeds a `shared-local` ServiceProvider
(`storageClass: local-path`, `accessModes: [ReadWriteOnce]`). On Tier-2 the
provider is upgraded to `shared-nfs` (`storageClass: nfs-client`,
`accessModes: [ReadWriteMany]`), enabling cross-node and cross-namespace
sharing. The `ref` field in Application manifests is stable across this
upgrade — no manifest changes are required.

### 5. Single-namespace T1 invariant; cross-namespace → T2

On Tier-1 the backing PVC is `ReadWriteOnce`. Because all pods land on the
same single node, multiple pods mounting the same RWO volume works at the
OS level — but only if they are on the same node. The T1 invariant is:

> A `SharedVolume` and all Applications referencing it must be in the
> **same namespace**.

The admission webhook enforces this: a `ref` value that names a
SharedVolume in a different namespace (e.g. `other-ns/shared-uploads`) is
rejected with a message pointing to Tier-2 / NFS. This is not a policy
choice but a hard limit of `RWO` access semantics.

Cross-namespace sharing (including dev-namespace ↔ prod-namespace) is
deferred to Tier-2 + NFS. The `ref` field is forward-compatible with that
future.

### 6. Capacity-signal via kubelet Summary API

The SharedVolume controller polls
`GET /api/v1/nodes/{node}/proxy/stats/summary` (the apiserver node-proxy
path) per reconcile cycle, cached for 30 seconds per node (shared across
all reconciles in the window). From the Summary document it extracts:

- **Node-free fraction** (`node.fs.availableBytes / node.fs.capacityBytes`).
  When below **15%** (the `DEFAULT_NODE_FREE_THRESHOLD`), the controller
  sets `CapacityWarning=True` (`NodeNearlyFull`) on the SharedVolume and
  emits a `Warning` Kubernetes Event (`CapacityWarning` reason).
- **PVC used/capacity bytes** — per-pod volume stats for the backing PVC
  name — surfaced as `status.capacity.{usedBytes,capacityBytes}`.

The event is **edge-triggered**: it fires only on the OK → warning
transition to avoid flooding the event log while the node stays nearly full.
On recovery (node-free rises above 15%) the condition flips to
`CapacityWarning=False` (`SufficientCapacity`).

Capacity sampling is **best-effort**: any failure (RBAC denial, kubelet
unreachable, parse error, cluster has no nodes) is logged at debug level and
the reconcile continues without a capacity value for that cycle. The
`CapacityWarning` condition is only stamped when a fresh sample is available.

### 7. Two explicit decisions on webhook vs controller responsibility

**(a) SharedVolume EXISTENCE is checked controller-side, not in the webhook.**

The admission webhook for `Application` CREATE/UPDATE is **stateless**: it
validates shape only — owned-or-ref discrimination, reject namespaced `ref`,
replicas-relaxation for ref disks (a `ref` disk does not trigger the
`replicas > 1` block that owned disks do, because the referenced PVC is not
this Application's to manage). It does **not** check whether the named
`SharedVolume` actually exists in the cluster.

Existence is the controller's job: when the Application's referenced
SharedVolume is absent, the Application controller sets an
`AwaitingSharedVolume` condition and pauses until the SharedVolume appears.
This is the same separation-of-concerns pattern as `AwaitingResourceClaim`
in the owned-disk path — the webhook rejects known-bad shape, the
controller handles runtime dependencies.

**(b) Capacity signal surfaces on SharedVolume in 2.6c; owned-disk capacity
is a deferred follow-up.**

The node-level capacity signal is attached to `SharedVolume` because the
SharedVolume is the explicit, long-lived, operator-managed resource that
naturally owns this status. Owned-disk (`type: disk`) ResourceClaims do not
yet have a capacity signal:

- `ResourceClaimStatus` has no `capacity` field today — adding one requires
  a CRD change (new status sub-field, touching the crdgen drift gate from
  ADR 0047).
- The node-free signal is node-level (not PVC-level), so surfacing it on an
  Application or ResourceClaim would require a similar polling loop.
- A follow-up should add an owned-disk / Application-level capacity signal
  for clusters that use owned disks but have no SharedVolume. That is
  tracked as a 2.6c deferred item and does not block launch.

## Consequences

Positive:

- Teams can share a directory across micro-services with no manual PVC
  management or storage class knowledge.
- The capacity signal surfaces node-filesystem pressure before writes start
  failing, via both a condition and a Warning Event.
- `ref` is forward-compatible with a Tier-2 `shared-nfs` upgrade: no
  Application manifest changes are needed when the provider is swapped.
- The owned-disk path (ADR 0043) is unaffected — `needs.disk: {size: …}`
  continues to work exactly as before.

Negative / neutral:

- A new explicit lifecycle step: the cluster operator must `volume create`
  before any Application can reference the volume. There is no implicit
  SharedVolume creation from a `ref` field (deliberate — shared resources
  need an explicit owner).
- The `shared-disk` / `disk` scheduler-type split adds a name to know. It
  is load-bearing for T2 coexistence and self-documenting in provider config.
- Owned-disk capacity signal is deferred. Clusters with owned disks but no
  SharedVolumes will not see a `CapacityWarning` until the follow-up lands.
- The `nodes/proxy` RBAC verb (capacity fetch) is granted cluster-wide;
  in a multi-tenant cluster this allows the operator to proxy any node. On
  T1 (single-node, single-tenant) this is the correct scope.

## Alternatives considered

- **Bare PVC reference (no SharedVolume CRD).** Let `ref` name a raw PVC.
  Rejected: loses the explicit lifecycle (delete-guarded by refCount),
  the status surface (capacity, pvcRef, conditions), and the provider
  portability line (`shared-local` → `shared-nfs`).
- **Implicit SharedVolume creation from the first `ref`.** The first
  Application declaring `ref: "shared-uploads"` would create the
  SharedVolume. Rejected: who owns the lifecycle? Size comes from where?
  Explicit creation keeps the resource model unambiguous.
- **Webhook checks SharedVolume existence.** Perform a cluster API call from
  the webhook to verify the named SharedVolume exists at admission time.
  Rejected: webhooks must be fast and stateless; an API call adds latency,
  can fail with transient cluster errors, and makes the webhook dependent
  on cluster state (a failure policy of `Fail` would block all Application
  updates when the API is slow). Controller-side `AwaitingSharedVolume` is
  the established pattern.
- **PVC with ownerRef on the Application.** Let the renderer create an owned
  PVC per Application that mounts the SharedVolume. Rejected: ownerRefs
  cause cascade-delete (data loss on app delete); cross-app ownership is
  undefined; defeats the purpose of a shared, independently-managed volume.

## Risks

- **RWO concurrent-mount correctness.** On a single node all pods land on
  the same kubelet, so RWO concurrent mounts work (node-local access). On
  a multi-node cluster this assumption breaks silently — pod scheduling to
  a second node causes a volume attachment conflict. *Mitigation:* T1 is
  a single-node constraint by design; the T2 upgrade swaps the provider to
  `shared-nfs` (RWX). The T1 single-namespace invariant (enforced by the
  webhook) is the near-term guard.
- **Capacity false-negative (kubelet unreachable).** If the kubelet proxy is
  unavailable (RBAC change, kubelet restart), no CapacityWarning fires even
  when the node is full. *Mitigation:* best-effort is the stated safety
  property; the Warning Event and condition are supplementary signals, not
  the primary disk-full defense. The `local-path` provisioner's own eviction
  pressure (Kubernetes node-pressure eviction) is the primary guard.
- **RefCount race.** A concurrent app-add and volume-rm could slip past the
  refCount guard if the new ResourceClaim is not yet labeled. *Mitigation:*
  the delete is refused while `status.refCount > 0`; the controller updates
  refCount on each reconcile. A brief window exists; a follow-up can add a
  finalizer on the ResourceClaim itself pointing to the SharedVolume.

## Owner

Andrey Ryahovskiy.

## Re-evaluation

- At Phase T2 (NFS substrate): verify `shared-nfs` provider, RWX access
  modes, and cross-namespace ref lift.
- When owned-disk / Application-level capacity signal is taken up (the
  deferred 2.6c item): add `ResourceClaimStatus.capacity`, wire the same
  kubelet-polling loop, and extend `apprafter app status` to surface it.
- Revisit `ref` existence check being controller-only if the pattern causes
  confusing UX (e.g. an Application appears deployed but pods never start
  because the SharedVolume was never created). A webhook advisory-mode check
  (Warn, not Fail) is a possible follow-up.

## References

- ADR 0043 — `needs.disk` named multi-claim format (the `(type, name)`
  identity and the owned-PVC retention model this ADR builds on).
- plan.md §2.6c — the decomposition and acceptance criteria for the
  SharedVolume and capacity-signal deliverables.
- `operator/operator-controllers/resourceclaim-provisioner/src/shared_volume.rs`
  — SharedVolume reconciler (T6 provisioning, T8 lifecycle).
- `operator/operator-controllers/resourceclaim-provisioner/src/capacity.rs`
  — kubelet Summary API polling, TTL cache, and pure capacity parsers (T11).
- `operator/admission-webhook/src/validator.rs` — `validate_sharedvolume`
  (shape-only: quantity size, allowed class values) and
  `validate_application` ref-arm (owned-or-ref discrimination, cross-ns
  reject, replicas relaxation) (T10).
- `docs/operator-guide/shared-volumes.md` — operator-facing guide.
