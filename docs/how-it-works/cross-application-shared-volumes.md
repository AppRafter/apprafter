---
description: "Why a shared volume is an explicit opt-in, why every referencing application must share a namespace on Tier 1, and what changes when the storage class does."
---

# Cross-application shared volumes

Why sharing a volume works the way it does. The recipe is
[Shared volumes](../operator-guide/shared-volumes.md); none of this is needed
to create one.

Read it when the admission webhook rejects a `ref` you expected to work, or
when you are deciding whether a workload can share storage at all.

The decision is [ADR 0049](../adr/0049-cross-app-sharedvolume.md); the
single-application case it contrasts with is
[ADR 0043](../adr/0043-needs-disk-named-claims.md).

## Sharing is opt-in, and the namespace is the boundary

SharedVolumes rely on an **explicit opt-in** by the cluster operator: you
create the SharedVolume, and you list which Applications reference it. There
is no automatic sharing — an Application gains access only when you add the
`ref` field pointing to the volume's name.

All Applications that reference a SharedVolume must belong to the **same
namespace**. On Tier-1 this is equivalent to "same team / same trust group".
The platform does not provide additional access-control isolation within a
namespace beyond what Kubernetes RBAC already enforces.

## Why the namespace rule exists on Tier 1

On Tier-1 (single-node, no NFS) the backing PVC has `accessModes:
[ReadWriteOnce]`. Because all pods land on the same node, multiple pods
mounting an RWO volume works correctly at the OS level (node-local
concurrent access). The invariant is:

> A SharedVolume and all Applications referencing it **must be in the same
> namespace**.

Cross-namespace sharing requires a `ReadWriteMany` capable storage class
(NFS, Rook-Ceph, …) available on Tier-2. The admission webhook rejects
cross-namespace `ref` values on T1 with a descriptive error and a hint to
upgrade to Tier-2 when needed.

## What changes when the tier does

The `shared-disk` backend is matched by a seeded `shared-local`
ServiceProvider (Tier-1, `storageClass: local-path`). On Tier-2 the
provider is swapped to `shared-nfs` (`storageClass: nfs-client`, `RWX`),
which enables cross-namespace and cross-node sharing. The `ref` field in
Application manifests is forward-compatible — no manifest changes are
needed when the cluster tier upgrades from `shared-local` to `shared-nfs`.

## See also

- [Shared volumes](../operator-guide/shared-volumes.md) — creating one,
  referencing it, and reading its capacity signal.
- [Declared disk dependencies](needs-disk.md) — the single-application case,
  and why its PVC has no owner.
