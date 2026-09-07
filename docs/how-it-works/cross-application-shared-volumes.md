---
description: "Why a shared volume is an explicit opt-in, why every referencing application must share a namespace on Tier 1, what changes when the storage class does, and what the capacity warning actually measures."
---

# Cross-application shared volumes

Why sharing a volume works the way it does. The recipe is
[Shared volumes](../operator-guide/shared-volumes.md); none of this is needed
to create one.

Read it when the admission webhook rejects a `ref` you expected to work, when
you are deciding whether a workload can share storage at all, or when a
capacity line reads as a figure you did not expect — an em-dash where bytes
should be, or a warning naming the node's disk rather than your volume.

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

## What `CapacityWarning` measures

`CapacityWarning` on a SharedVolume is about **that volume's own usage**. Once
per reconcile the controller asks the kubelet for its statistics summary —
through the apiserver's node-proxy subresource,
`GET /api/v1/nodes/{node}/proxy/stats/summary`, which is why the operator holds
`get` on `nodes/proxy` — finds the pod-volume entry whose `pvcRef.name` is the
backing PVC, and compares `usedBytes` against `capacityBytes`.

Past **85%** the controller writes `CapacityWarning=True` with reason
`VolumeNearlyFull` and a message naming the figure: *"volume 91.2% full (> 85%
threshold) — writes will fail when it reaches capacity"*. Below it the same
condition is written `False` with reason `SufficientCapacity`. The comparison
is strict, so a volume sitting exactly on 85% does not warn — a threshold that
fires at its own value makes the number in the message look wrong to whoever
reads it. The 85% is a constant in the operator: there is no per-volume or
cluster-wide field that changes it. Both this condition and `Ready` ride a
single server-side apply, because one field manager owns both and an apply
carrying only one would prune the other.

A transition into the warning state also publishes a `Warning` Event with
reason `CapacityWarning` against the SharedVolume; a reconcile that finds the
volume already warning publishes nothing, and recovering publishes nothing at
all — so `kubectl describe` shows the moment the volume filled rather than one
line per reconcile. The comparison is against the condition the object
currently carries, so a cycle that could not sample — which drops the
condition, as below — makes the next successful cycle look like a fresh
crossing and publish again.

Sampling runs on every reconcile: the 300-second requeue, plus any change to a
reference-claim, which fans a reconcile back to the parent volume. A
30-second cache in front of the kubelet means several volumes reconciling
inside the same window cost one fetch between them rather than one each.

## Why the figures can be the host disk's

On Tier 1 the backing PVC is served by `local-path`, which hands out a
directory on the node's root filesystem. A SharedVolume carrying no `scope` key at all was written by an
operator predating the field; read that as unknown and fall back to the plain
`Used/Free` line. A directory has no quota to report
against, so the kubelet answers a request for that volume's statistics with the
**backing filesystem's**: a volume created at `2Gi` reports the node's whole
disk, and its used bytes are the node's used bytes.

The controller detects this by comparing the volume's `capacityBytes` with the
node root filesystem's, read from the same summary document so the two readings
cannot be a poll apart. An exact match is recorded as
`status.capacity.scope: host`; everything else — including a summary whose node
figures could not be read at all — is recorded as `volume`, on the reasoning that
a backend enforcing a real quota reports that quota and would not coincide with
the node's byte count by accident. When the node's own figure cannot be read
the field is left absent, and absent is treated as unknown rather than as
`host`.

The scope changes what you are shown, not what is measured.
`apprafter volume status` prints the pair on a `Host disk:` line and leaves
`Used/Free` an em-dash with the reason when the scope is `host`;
`apprafter volume list` has no such column and prints the raw pair under
`USED/FREE` either way. The threshold is applied to the same figures. So on a
local-path volume, `CapacityWarning=True` is telling you that the filesystem
your volume lives on is over 85% full — accurate about what is going to happen
to your writes, and attributed to the volume because the volume is the object
carrying the condition.

## Why an em-dash is a reading, not a fault

Capacity is decorative, and every path that produces it answers "absent"
rather than failing: no node listable, the kubelet unreachable, RBAC denied, a
non-JSON body, a summary carrying no entry for this PVC. A failed kubelet fetch — RBAC
denial, an unreachable kubelet, a non-JSON body — is logged at debug and
swallowed. A node list that fails is logged at warning; a cluster with no nodes,
and a summary carrying no entry for this PVC, are silent. All five produce the
same outcome: no sample, and a reconcile that carries on, so a volume provisions
and goes `Ready` whether or not it can be measured.

A cycle with no sample writes no `status.capacity` and no `CapacityWarning` at
all. It does not carry the previous values forward — the status apply replaces
everything this controller owns, so omitting the sample removes it. The
condition is therefore never older than the last successful sample, and the
absence of a warning is never evidence of space.

That is also why an em-dash is the usual reading on a cluster whose kubelet
publishes no per-volume metrics at all. `e2e/shared-volume-walk.sh` treats its
whole capacity phase as a soft assertion for that reason, and hard-asserts
instead that the volume stayed `Ready` without a sample.

## Where the node's own disk is reported

A volume warning tells you about one volume. The filesystem underneath it
carries every other local-path volume, the databases' data directories,
snapshots, the container image store and the logs — and that is reported on its
own: `NodeDiskPressure` on the `PlatformStack` singleton, `True` with reason
`NodeFilesystemNearlyFull` once the node's root filesystem drops below **15%**
free, `False` with `SufficientSpace` above it. It exists on every cluster,
including one with no SharedVolume at all, and every `apprafter` command prints
a banner while it is `True`.

The same sampler stamps `status.capacity`, with the same `scope` field, on an
owned disk's `ResourceClaim` — that is what `apprafter app status` shows in its
dependency table. There is no `CapacityWarning` condition there; the threshold
and the Event are SharedVolume behaviour.

## See also

- [Shared volumes](../operator-guide/shared-volumes.md) — creating one,
  referencing it, and what to do when it fills up.
- [Preparing a node](../operator-guide/node-prep.md) — the node filesystem
  every local-path volume shares, and the reservations that keep it usable.
- [Declared disk dependencies](needs-disk.md) — the single-application case,
  and why its PVC has no owner.
