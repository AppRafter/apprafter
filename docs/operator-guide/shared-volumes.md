---
description: "When several applications in one namespace should share a directory, how to declare and manage that, and when to reach for an owned disk instead."
---

# Shared volumes — cross-app persistent storage

A `SharedVolume` is a named, platform-managed PVC that multiple Applications
in the same namespace can mount simultaneously. It lets teams share a
directory across micro-services — a common pattern when migrating from a
process manager like pm2, where sibling processes wrote to the same folder —
while preserving AppRafter's single-operator-managed lifecycle.

The full design decisions and trade-offs are in
[ADR 0049](https://github.com/apprafter/apprafter/blob/master/docs/adr/0049-cross-app-sharedvolume.md).

## When to use a SharedVolume

Use a `SharedVolume` when two or more Applications in the **same namespace**
need to read from or write to the same directory. Examples:

- A web worker and a background job share an upload staging area.
- Two processes both append to the same log directory for a legacy
  monolith being split into services.

Do **not** use a SharedVolume when:

- Only one Application ever mounts the volume — use a plain `needs.disk`
  (owned disk, ADR 0043) instead.
- Applications live in different namespaces — cross-namespace sharing
  requires Tier-2 with an NFS-backed `shared-nfs` provider and is not
  available on Tier-1.
- You need per-replica volumes — `SharedVolume` is single-PVC `RWO`; on a
  single-node cluster all pods land on the same node, so multiple pods
  mounting the same `RWO` volume works correctly. Multi-node replicated
  storage (`RWX`) is a Tier-2 / NFS follow-up.

## The `needs.disk.ref` reference grammar

An Application declares a *reference* disk need instead of an owned one by
supplying `ref` (the SharedVolume name) in place of `size`:

```cue
spec: base: needs: disk: {
    ref:       "shared-uploads"   // name of the SharedVolume in this namespace
    mountPath: "/data/uploads"
    readOnly:  false              // optional; default false
}
```

Key rules enforced by the admission webhook:

| Rule | Why |
|------|-----|
| `ref` and `size` are mutually exclusive | `ref` binds an existing volume; `size` provisions a new one. |
| `ref` and `class` are mutually exclusive | The storage class is a property of the `SharedVolume`, not the reference. |
| `ref` must be in the **same namespace** on T1 | Cross-namespace sharing requires T2 (NFS) and is not available here. The webhook rejects a namespaced `ref` (e.g. `other-ns/shared-uploads`) with a hint pointing to T2. |
| A referenced disk does **not** count toward the `replicas > 1` block | Owned disks block multiple replicas (`RWO` cannot be held by two pods on two nodes). A `ref` disk does not, because the SharedVolume's PVC is not this Application's to manage. |

SharedVolume **existence** is checked by the controller, not the webhook. The
webhook is stateless and only validates shape. A `ref` disk makes the
Application controller emit an ordinary `shared-disk` `ResourceClaim`, so the
Application is held by the same `AwaitingResourceClaim` gate every other
dependency uses. What names the *reason* is the claim: while the referenced
`SharedVolume` is absent — or present but not yet `status.ready` — the
provisioner publishes `Ready=False` on that claim with reason
`AwaitingSharedVolume` and requeues every 30s. So the diagnostic lives one
level down:

```sh
kubectl -n apps get resourceclaim.apprafter.io -o \
  jsonpath='{range .items[*]}{.metadata.name}{" "}{.spec.type}{" "}{.status.conditions[?(@.type=="Ready")].reason}{"\n"}{end}'
# -> <claim> shared-disk AwaitingSharedVolume
#    message: "SharedVolume shared-uploads not ready"
```

The reference claim carries the label `apprafter.io/shared-volume=<ref>`,
which is also how the volume's own `refCount` is computed — so
`kubectl -n apps get resourceclaim.apprafter.io -l
apprafter.io/shared-volume=shared-uploads` lists exactly the claims that
`REFS` counts.

## Trust-group model

SharedVolumes rely on an **explicit opt-in** by the cluster operator: you
create the SharedVolume, and you list which Applications reference it. There
is no automatic sharing — an Application gains access only when you add the
`ref` field pointing to the volume's name.

All Applications that reference a SharedVolume must belong to the **same
namespace**. On Tier-1 this is equivalent to "same team / same trust group".
The platform does not provide additional access-control isolation within a
namespace beyond what Kubernetes RBAC already enforces.

## Managing SharedVolumes with the CLI

> **`--namespace` is not optional in practice.** `volume create`,
> `volume status` and `volume rm` default it to **`apprafter-system`**,
> not to the namespace your apps live in — omit it and you create the
> volume somewhere your Applications cannot reference it, or get
> `SharedVolume '<name>' not found in apprafter-system` looking for one
> you did create. Only `volume list` treats the flag as genuinely
> optional: omitted, it lists cluster-wide.

### Create

```sh
apprafter volume create <name> --size <size> [--namespace <ns>]
```

`<size>` is a Kubernetes storage quantity (`1Gi`, `500Mi`, …). The operator
provisions a backing `ReadWriteOnce` PVC via the seeded `shared-local`
ServiceProvider (Tier-1: `local-path` storage class) and marks the
SharedVolume `Ready=True` once the PVC exists.

### List

```sh
apprafter volume list [--namespace <ns>]
```

Prints a table with one row per SharedVolume:

| Column | Contents |
|--------|----------|
| NAME | SharedVolume name |
| SIZE | Requested size from `spec.size` |
| READY | `true` once the backing PVC exists |
| REFS | Number of ResourceClaims currently bound to this volume |
| USED/FREE | Used and **free** bytes from the last kubelet sample — `usedBytes` / `capacityBytes − usedBytes`, not used-over-capacity. An em-dash (`—`) until the first sample lands |

### Status

```sh
apprafter volume status <name> [--namespace <ns>]
```

Single-resource detail view — six fixed lines, no conditions:

```text
SharedVolume:  apps/shared-uploads
  Size:        2Gi
  Ready:       true
  PVC ref:     sv-apps-shared-uploads
  Ref count:   2
  Used/Free:   41943040/2105540608 bytes
```

`Size` is `spec.size` echoed back; the rest is status the operator wrote.
`Ready` is `status.ready`, `PVC ref` is `status.pvcRef`, `Ref count` is
`status.refCount`, and `Used/Free` is derived from
`status.capacity.{usedBytes,capacityBytes}` — it prints an em-dash (`—`)
until the first kubelet sample lands. The `CapacityWarning` condition
described below is **not** among these lines; read it with `kubectl`
(see [Capacity signal](#capacity-signal)).

### Remove

```sh
apprafter volume rm <name> [--namespace <ns>]
```

Deletes the SharedVolume CR. The `apprafter volume rm` CLI reads
`status.refCount` before issuing the delete and **refuses while the count
is above zero** — remove all `needs.disk.ref` entries from your Application
manifests first, let the reconcile run (refCount drops), then retry `rm`.

Note: this guard is in the CLI, not a hard admission webhook. A raw
`kubectl delete sharedvolume <name>` bypasses it. The backing PVC is still
protected by Kubernetes' built-in PVC-protection controller — the PVC
remains `Terminating` while any pod has it mounted, so data is not pulled
from running Applications even if the CR is deleted directly.

On deletion the operator runs the `apprafter.io/sharedvolume-pvc-cleanup`
finalizer: the backing PVC is deleted (404-tolerant), then the finalizer is
released.

## Capacity signal

The SharedVolume controller polls the kubelet Summary API
(`/api/v1/nodes/{node}/proxy/stats/summary`) once per reconcile cycle
(every 5 minutes by default, with a 30-second TTL cache shared across all
reconciles in the window). It extracts:

- **Node-free fraction** — `availableBytes / capacityBytes` of the node's
  root filesystem. When this falls below **15%**, the controller:
  1. Sets `CapacityWarning=True` on the SharedVolume with reason
     `NodeNearlyFull`.
  2. Emits a `Warning` Kubernetes Event (`CapacityWarning` reason) on the
     SharedVolume object — **edge-triggered**: the event fires only on the
     transition from OK to warning, not on every reconcile while the node
     stays nearly full.
- **PVC used/capacity bytes** — from the pod volume stats for the backing
  PVC name, surfaced as `status.capacity.{usedBytes,capacityBytes}`.

When the node frees up above 15%, `CapacityWarning` flips to
`CapacityWarning=False` (reason `SufficientCapacity`).

Capacity sampling is **best-effort**: any failure (RBAC denial, kubelet
unreachable, parse error, no node) is logged at debug level and the
reconcile continues with `capacity` absent for that cycle. A CapacityWarning
is only ever stamped when a fresh sample is available.

### Reading the capacity signal

**No `apprafter` command prints `CapacityWarning` today.** The operator
stamps the condition on the CR and emits the Event; the CLI does not read
either. `apprafter volume status` shows the sampled bytes (`Used/Free`)
and nothing about the warning, and `apprafter app status` says nothing
about SharedVolumes at all. Until a CLI surface exists, read the
condition and the Event directly:

```sh
kubectl -n apps get sharedvolume shared-uploads -o \
  jsonpath='{.status.conditions[?(@.type=="CapacityWarning")].status}{" "}{.status.conditions[?(@.type=="CapacityWarning")].reason}{"\n"}'
# -> True NodeNearlyFull      (or: False SufficientCapacity)

kubectl -n apps describe sharedvolume shared-uploads
# the Events section carries the edge-triggered Warning, reason
# `CapacityWarning`
```

`Used/Free` from `apprafter volume status` is the same sample the
condition is computed from, so an em-dash there means no sample landed
this cycle and the condition will not have been re-stamped either.

## Tier-1 single-namespace invariant

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

## Provider line and tier upgrades

The `shared-disk` backend is matched by a seeded `shared-local`
ServiceProvider (Tier-1, `storageClass: local-path`). On Tier-2 the
provider is swapped to `shared-nfs` (`storageClass: nfs-client`, `RWX`),
which enables cross-namespace and cross-node sharing. The `ref` field in
Application manifests is forward-compatible — no manifest changes are
needed when the cluster tier upgrades from `shared-local` to `shared-nfs`.

## Example end-to-end

```sh
# 1. Create a 2 GiB SharedVolume in the "apps" namespace.
apprafter volume create shared-uploads --size 2Gi --namespace apps

# 2. Check it is ready.
apprafter volume status shared-uploads --namespace apps
# → SharedVolume:  apps/shared-uploads
#     Size:        2Gi
#     Ready:       true
#     PVC ref:     sv-apps-shared-uploads
#     Ref count:   0
#     Used/Free:   —

# 3. Deploy two Applications that reference the volume.
#    Both Application.cue files include:
#    needs: disk: { ref: "shared-uploads", mountPath: "/uploads" }
#    `app add` takes the REPO URL, not the app name — `--name` is what
#    names the Argo CD Application (it defaults to the repo basename).
apprafter app add https://github.com/your-org/writer.git \
  --name writer --namespace apps
apprafter app add https://github.com/your-org/reader.git \
  --name reader --namespace apps

# 4. Confirm both apps are healthy and the volume shows Ref count 2.
#    `app status` takes no --namespace: the namespace was fixed by
#    `app add` above and is read back off the app's Argo CD Application.
apprafter app status writer
apprafter app status reader
apprafter volume status shared-uploads --namespace apps
# → …  Ref count:   2

# 5. Remove apps, then the volume.
apprafter app remove writer --yes
apprafter app remove reader --yes
# wait for Ref count to drop to 0 …
apprafter volume rm shared-uploads --namespace apps
```

> The two verbs are spelled differently on purpose. `apprafter app remove`
> accepts `apprafter app rm` as an alias; `volume` has only `rm`, and spelling it `remove`
> there is an unrecognised subcommand (clap suggests `rm`). Both refuse
> to act without `--yes` in a non-interactive shell. Note the explicit
> `--namespace apps` on every `volume` line above — without it they act
> on `apprafter-system`.
