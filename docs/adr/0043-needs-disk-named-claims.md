# ADR 0043: needs.disk → persistent block storage, on a named multi-claim `needs` format

## Status

Accepted (2026-06-06). ADR-first for Phase 2.6b; supersedes the `plan.md` 2.6b "ADR TBD" marker.

## Context

Phase 2.6b ships `Application.spec.base.needs.disk` — a developer declares persistent block storage and the platform gives them a volume that survives pod restarts and app delete+redeploy. It reuses the 2.1–2.4f machinery (Application → child `ResourceClaim`, the 2.3 scheduler matching a `ServiceProvider`, the `resourceclaim-provisioner` provisioning per-claim resources, `operator-rendering`, the 2.4f RetainedClaim + 7-day-grace GC).

Disk forces two genuinely new decisions.

**1. The `needs` format must carry multiplicity + names.** Today `needs?: [#PlatformServiceType]: #ServiceNeed` is a map keyed by type — at most one claim per type, so an app cannot declare two Postgres databases, and there is no way to address a specific claim. Disk makes the gap obvious (multiple disks at different mount paths is the common case), but the real change is cross-cutting: a `(type, name)` identity must thread through claim generation, the connection-Secret/env injection (2.4e/2.6-6), and the 2.12 reference engine (`claim.<type>.<name>`). Designing disk's array format in isolation would create an inconsistency with how pg/redis and 2.12 will work, so the format is generalized for **all** types here.

**2. Disk does not fit the claim's "connection Secret → env" output.** pg/redis claims produce a DSN Secret injected as an env var. A disk produces a PVC that must be **wired into the pod spec** (a `volumeMount` + a `volume`), which only the renderer can do. The naive alternative — render the PVC as an owned object of the Application — is unacceptable: an ownerRef makes the PVC **cascade-delete with the app**, destroying user data instantly. Renderer-only `volumeClaimTemplates` have the opposite failure: Kubernetes never garbage-collects them, so they leak forever (or, with `whenDeleted: Delete`, vanish immediately with no grace). Neither gives the "retain for a grace window, then reclaim" lifecycle that data demands — which is exactly what the 2.4f RetainedClaim machinery already provides for the pg role/DB.

Two more forces shape the launch slice:

- **Tier portability of storage** is a `StorageClass` mapping (T1 `local-path`, T2 `hcloud-volumes`/`longhorn`/`rook-nfs`, …). That is precisely what the `ServiceProvider` + scheduler abstraction expresses — a per-tier seeded provider carrying the class — so the renderer needs no hardcoded tier→class table.
- **StatefulSet is only needed for per-replica multiplicity.** A single-replica stateful app (the launch target — SQLite and friends) is served by a standalone RWO PVC on a `replicas: 1` Deployment. `volumeClaimTemplates` (and thus the Deployment→StatefulSet pivot, which would also drag in the 2.4h image-digest resolver that patches the child *Deployment* out-of-band) are required only for `replicas > 1` per-replica disks — out of launch scope.

The launch tier (Hosted Services, mostly T1 single-node) cannot run the broad `plan.md` 2.6b sketch (replicated/shared classes, CSI snapshots, auto-expand) — all need T2+ CSI infrastructure. Those are deferred.

## Decision

### 1. Generalize `needs` to a named multi-claim format

Each need type accepts a **scalar or an array**; the value gains an optional `name`. `(type, name)` is the claim identity. The CUE `needs` becomes an explicit closed struct so `disk` can carry its own value type (`#DiskClaim`) alongside the service types (`#ServiceNeed`):

```
needs?: { pg?: #ServiceNeed | [...#ServiceNeed]; …; disk?: #DiskClaim | [...#DiskClaim] }
```

- The scalar/unnamed form is the **default** claim and stays valid (zero migration): claim `<app>-<type>`, env `DATABASE_URL`/`REDIS_URL`.
- A named entry yields claim `<app>-<type>-<name>` and a disambiguated env `<VAR>_<NAME>` (UPPER_SNAKE). Names are unique within a type; at most one unnamed default per type.
- The format is implemented end-to-end for all types in 2.6b (pg/redis multi-claim provisioning + disambiguated injection), validated on pg/redis arrays before disk.

### 2. `needs.disk` is a ResourceClaim, not a renderer-only volume

Disk runs the standard pipeline (generate → schedule → provision → resume → snapshot → GC), diverging only in the output channel: `Backend::Disk` provisions a PVC, and the renderer mounts it (instead of injecting env). This buys retention (decision 5), tier StorageClass (decision 4), and `(type, name)` uniformity — at the cost of a thin "provision = create a PVC" arm and a new render input.

### 3. Launch = standalone RWO PVC on a `replicas: 1` Deployment; no StatefulSet

`Backend::Disk` SSA-applies a standalone PVC (`accessModes: [ReadWriteOnce]`, `storageClassName` from the provider, size from the claim) in the app namespace, deterministically named, **with no ownerRef** (so it outlives the app). The renderer adds the `volumeMount` + `volume{persistentVolumeClaim}` and, whenever a disk is present, sets the Deployment `strategy: Recreate` (an RWO PVC cannot be held by two pods during a rolling update). `ready` means the PVC exists, not Bound (`local-path` is `WaitForFirstConsumer`; the pod binds it). The webhook rejects `needs.disk` + `replicas > 1` — per-replica multi-replica (StatefulSet + `volumeClaimTemplates`) is deferred.

### 4. Tier StorageClass via a seeded `disk-local` ServiceProvider

`platform-stack` seeds a `disk-local` `ServiceProvider` (`backend: disk`) per tier with `config.storageClass` (T1 → `local-path`). The scheduler matches the disk claim to it; the provisioner reads `config.storageClass`. The tier→class table lives in provider config, not the renderer. No new platform-stack component at launch (`local-path` ships on T1).

### 5. Retention + reattach via RetainedClaim

On disk-claim delete, the provisioner finalizer snapshots a RetainedClaim (backend `disk`, carrying the PVC ref), retains 7 days, then `gc_backend("disk")` deletes the PVC (idempotent / 404-tolerant). Cancel-on-re-provision (2.4f Fix A) applies unchanged: re-deploying the app cancels the RetainedClaim, and `provision_disk` **reattaches** to the existing PVC (the dbnum-reattach analog, ADR 0042 §8), so disk data survives delete+redeploy within the grace window. The GC live-guard never drops a PVC whose claim is live again.

## Consequences

- One uniform `needs` model: every dependency — pg, redis, disk — is a `(type, name)` ResourceClaim with the same generate/schedule/provision/snapshot/GC lifecycle, `app status` view, and (future) 2.12 addressing.
- Disk data has a real lifecycle: retained on delete, reclaimed after grace, preserved across redeploy — no instant loss, no leak.
- No StatefulSet at launch → no 2.4h-resolver interaction, no headless-Service requirement; the Deployment renderer gains only a volume block + a `Recreate` strategy switch.
- `Backend::Disk` "provisions" a PVC rather than a backend service — a thin, slightly unusual arm, but it keeps disk inside the one pipeline.
- pg/redis become multi-claim-capable; the shipped single-claim `DATABASE_URL`/`REDIS_URL` is unchanged for the unnamed default.

## Alternatives considered

- **Renderer-only PVC / `volumeClaimTemplates`.** Rejected as the primary model: an owned PVC cascades (data loss), an unowned rendered PVC leaks (no GC). Loses retention, the tier-class abstraction, and `(type, name)` uniformity. (StatefulSet `volumeClaimTemplates` remains the future path for per-replica `replicas > 1`.)
- **Name-keyed map `needs.pg: {primary: {...}}`.** Ambiguous against the scalar form (`{selector: …}` vs `{primary: …}`) and awkward to keep backward-compatible; the explicit-`name`-field union (scalar|array) is clearer.
- **Always-named (no implicit default).** Uniform but breaks every shipped single-pg app (`DATABASE_URL` disappears) → migration. Rejected for backward-compat.
- **Always StatefulSet when disk present.** Simpler than a conditional pivot but drags in the 2.4h-resolver/headless-Service work and StatefulSet update semantics for the common single-replica case that a Deployment + RWO PVC handles cleanly.

## Risks

- **CRD scalar|array `oneOf` union.** OpenAPI v3 structural schemas are finicky (the 2.4h `additionalProperties` regression passed every gate but the live apiserver). Mitigation: mandatory `just crd-validate` on the schema sub-subphase.
- **PVC leak on a GC bug.** A mis-scoped or skipped disk GC leaks storage silently. Mitigation: the e2e walk asserts the force-GC path deletes the PVC; the GC is idempotent + scoped to the snapshot's PVC ref.
- **`spec.size` overload.** `ResourceClaim.spec.size` is an enum-ish hint for pg (`small`) and a quantity for disk (`10Gi`); the webhook validates per-type (quantity for disk).
- **`WaitForFirstConsumer` confusion.** A disk claim reports ready before its PVC is Bound; operators inspecting a Pending PVC pre-pod should not read it as a failure.

## Owner

Platform / operator. Implementation: Phase 2.6b (`plan.md` 2.6b-1 … 2.6b-6).
