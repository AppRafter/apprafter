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
[Cross-application shared volumes](../how-it-works/cross-application-shared-volumes.md).

## When to use a SharedVolume

Use a `SharedVolume` when two or more Applications in the **same namespace**
need to read from or write to the same directory. Examples:

- A web worker and a background job share an upload staging area.
- Two processes both append to the same log directory for a legacy
  monolith being split into services.

Do **not** use a SharedVolume when:

- Only one Application ever mounts the volume — use a plain `needs.disk`
  (an owned disk) instead.
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
`AwaitingSharedVolume` and requeues every 30s.

??? note "Reading that reason with kubectl"

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

## Who may mount it

Sharing is an explicit opt-in: you create the SharedVolume, and an Application
gains access only when you add a `ref` pointing at its name. Nothing is shared
automatically.

All Applications referencing one must be **in the same namespace** — the
admission webhook rejects a cross-namespace `ref` with a hint. Why that rule
exists, and what changes when the cluster's storage class does, is
[Cross-application shared volumes](../how-it-works/cross-application-shared-volumes.md).

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
| USED/FREE | Used and **free** bytes from the last kubelet sample — `usedBytes` / `capacityBytes − usedBytes`, not used-over-capacity. An em-dash (`—`) until a sample lands. Unlike `volume status`, this column does not say when the figures are the host disk's rather than the volume's |

### Status

```sh
apprafter volume status <name> [--namespace <ns>]
```

Single-resource detail view:

```text
SharedVolume:  apps/shared-uploads
  Size:        2Gi
  Ready:       true
  PVC ref:     sv-apps-shared-uploads
  Ref count:   2
  Host disk:   12058030080/56163426304 bytes used/free (this volume shares it)
  Used/Free:   — (local-path has no per-volume quota to report)
```

`Size` is `spec.size` echoed back; the rest is status the operator wrote.
`Ready` is `status.ready`, `PVC ref` is `status.pvcRef` and `Ref count` is
`status.refCount`. The last two lines both come from
`status.capacity.{usedBytes,capacityBytes}`: on Tier 1 the `local-path`
backend gives the kubelet no per-volume quota to report against, so what got
measured is the filesystem the volume sits on — the output names it as the
host disk rather than passing the node's size off as the volume's. A backend
that does enforce a quota prints `Used/Free:   41943040/2105540608 bytes` and
no host-disk line, and a volume nothing has measured yet prints
`Used/Free:   —` on its own.

A further `Capacity:` line appears while the volume is nearly full — see
[When a volume is running out of room](#when-a-volume-is-running-out-of-room).

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

## When a volume is running out of room

`apprafter volume status` prints an extra line once the volume passes 85%
full:

```text
  Capacity:    volume 91.2% full (> 85% threshold) — writes will fail when it reaches capacity
```

The line is absent while the volume has room — and also absent when the
operator could not measure the volume this cycle, so a missing warning is not
a promise of space. Check that the same output carries a `Used/Free` or
`Host disk:` figure rather than an em-dash before you read the silence as good
news.

There is nothing to configure and nothing to acknowledge. Free space and the
line clears on the next reconcile, up to five minutes later. On Tier 1, where
the figures are the host disk's, freeing space means freeing it **on the node**
— every local-path volume, the databases and the image store share that
filesystem, and the node reports its own version of this as a banner on every
`apprafter` command that reaches beyond your machine.

What gets sampled, why a Tier-1 volume's figures can be the node's, and where
the node's own disk is reported are in [Cross-application shared
volumes](../how-it-works/cross-application-shared-volumes.md#what-capacitywarning-measures).


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
