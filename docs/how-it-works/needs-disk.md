---
description: "How a declared needs.disk dependency becomes a mounted volume: the unowned PVC, the single-writer constraints it forces, reattachment, and deletion after the grace window."
---

# Declared disk dependencies

The recipe is [Persistent disk](../operator-guide/persistent-disk.md). This
page is what happens behind it. None of it is needed to use `needs.disk`.

The backend is an ordinary Kubernetes **PersistentVolumeClaim**,
`ReadWriteOnce`, on the storage class the seeded `disk-local`
`ServiceProvider` advertises — on Tier 1 the in-cluster `local-path`
provisioner. The design, including the launch-minimal `class: local` slice and
the deferred replicated and shared classes, is in [ADR
0043](../adr/0043-needs-disk-named-claims.md).

## The chain

1. The operator generates a `ResourceClaim` of `type: disk` instead of
   rendering the Deployment immediately — that deferral is the whole mechanism.
2. The scheduler matches it to `disk-local` and marks it `Scheduled=True`.
3. The provisioner server-side-applies a standalone PVC with the requested size
   and the provider's storage class, records its name in
   `status.volumeClaimRef`, and marks the claim `ready=true` in a single pass.
   It does **not** wait for the PVC to bind: `local-path` binds
   `WaitForFirstConsumer`, so binding happens when a pod schedules onto it.
   A claim that is `ready` with a PVC still `Pending` is the normal
   intermediate state, not a fault.
4. The Application resumes, and the rendered Deployment mounts the PVC at
   `mountPath` and pivots to `strategy: Recreate`.

## The PVC has no owner, and that is the retention model

The provisioner deliberately applies the PVC with **no `ownerReference`**.
Every other object in a claim's blast radius cascades; this one does not.

That single decision is what makes the guide's promises true. Deleting the
application does not delete the volume. Removing the dependency does not delete
the volume. The only thing that deletes it is the GC, after `retainUntil`
passes — and re-declaring the dependency before then cancels the pending
deletion and reattaches the same PVC, data intact, because the provisioner's
apply is idempotent on the same name.

Names are derived from the claim's `(namespace, name)`: an app `web` in
namespace `demo` gets `ResourceClaim` `web-disk` and PVC `claim-demo-web-disk`.
A named entry in a multi-disk array is identified by its `name`, or — when
omitted — by the last segment of its `mountPath`, and that identity is what the
claim and PVC names are built from.

**Delete the Application, not the claim, when you want the retention path.**
The claim is owned by the Application, so deleting the claim alone while the
manifest still declares `needs.disk` makes the controller regenerate it — and
the regeneration cancels the snapshot it just wrote. That is correct behaviour
(it is the reattach path doing its job) but it looks like a retention failure
if you were not expecting it.

## Single writer, and what it forces

`ReadWriteOnce` means exactly one pod may mount the volume. This section is
about a disk an application *owns* — one it declares with a `size` and a
`mountPath`. A volume several applications read is a different primitive
(`SharedVolume`, reached through `needs.disk.ref`) and is not subject to either
constraint below.

For an owned disk, two consequences are enforced rather than advised:

- the admission webhook **rejects** `needs.disk` together with `replicas > 1`;
- the renderer sets `strategy: Recreate`, so a rolling update never overlaps
  two pods against one volume. A `RollingUpdate` would deadlock: the new pod
  cannot mount until the old one releases, and the old one does not terminate
  until the new one is ready.

`readOnly: true` on the need mounts it read-only, so the container cannot write
to it.

## Watching it happen

```sh
# the claim is provisioned, and which PVC it made
kubectl -n demo get resourceclaim.apprafter.io web-disk -o \
  jsonpath='ready={.status.ready} vcr={.status.volumeClaimRef}{"\n"}'
# -> ready=true vcr=claim-demo-web-disk

# the PVC's shape, and that it is unowned
kubectl -n demo get pvc claim-demo-web-disk -o \
  jsonpath='class={.spec.storageClassName} modes={.spec.accessModes[0]} size={.spec.resources.requests.storage}{"\n"}'
kubectl -n demo get pvc claim-demo-web-disk -o jsonpath='{.metadata.ownerReferences}{"\n"}'
# -> (empty — nothing cascades to it)

# the mount and the rollout strategy the renderer chose
kubectl -n demo get deployment web -o jsonpath='{.spec.strategy.type}{"\n"}'   # -> Recreate
```

## The grace window, and the deletion

**Deleting the Application** deletes its claim — the `ResourceClaim` carries a
controlling `ownerReference` back to it — and the finalizer writes an immutable
`RetainedClaim` snapshot with `retainUntil` at deletion + 7 days, carrying the
volume claim's name and namespace. The PVC survives untouched. Once
`retainUntil` passes, the GC deletes the PVC and removes the snapshot.

Editing the manifest is a different path. Dropping a `needs.<type>` key is
classified as a destructive `data-migration` change, so it is gated behind a
MigrationPlan and the Application pauses at `AwaitingMigrationApproval`. Even
after approval nothing deletes the claim: the render path applies claims for
*declared* needs and skips the block when there are none. The retention path
runs on Application deletion, not on a manifest edit.

The snapshot is immutable by a CEL `self == oldSelf` rule, and the admission
webhook restricts CREATE to the operator's ServiceAccount, with a deliberate
cluster-admin break-glass (`system:masters`, `kubeadm:cluster-admins`): it is
written by the provisioner's finalizer, and hand-writing one is unsupported
rather than impossible. There is no supported way to
shorten the window, and the guide does not teach one — the snapshot is the
work order the GC executes, so a hand-written substitute is a way to delete the
wrong volume.

`e2e/needs-disk-walk.sh` — `just e2e-disk` — exercises the whole chain on a
local cluster: provision, mount, data durability across a pod restart, delete
with the volume surviving, reattach with the data still there, and the deletion
after a forced grace expiry. That is where those properties are proved.

## For contributors

The durability and reattach properties are the ones most likely to break
silently, because both look fine from the outside until the moment data is
gone. If you touch the disk provisioner or the GC path, `just e2e-disk` is the
gate: it writes a file, restarts the pod, deletes the app, reattaches, and
reads the file back.

The retention model is also the one most likely to be broken by a well-meaning
change: adding an `ownerReference` to the PVC would make every one of the
guide's promises false at once, and nothing in the ordinary reconcile path
would notice.
