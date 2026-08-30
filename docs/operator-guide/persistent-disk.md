---
description: "Give an application a persistent disk by declaring it in the manifest: how to declare it, what the single-writer constraint means for you, and what happens to the data when you remove it."
---

# Persistent disk from a declared dependency

An application declares that it needs a disk, and the platform provisions a
volume and mounts it into the container at the path you asked for. Data written
there survives pod restarts and rollouts.

Deleting the application does **not** delete the volume. Nothing does, for
seven days — and re-declaring the dependency in that window reattaches the same
volume with the data still in it.

## Declare the dependency

```sh
apprafter app scaffold --name web --namespace demo --needs disk
```

That writes a `needs` block into `apprafter/Application.cue` with launch
defaults to edit before committing:

```cue
spec: base: {
    // ... image / replicas: 1 / expose ...
    needs: {
        disk: { size: "1Gi", mountPath: "/data" }
    }
}
```

`size` is a Kubernetes quantity — `"1Gi"`, not a t-shirt size. The volume
appears at `mountPath` inside the container; write ordinary files there.

Check it before you push:

```sh
apprafter app validate
```

Use that rather than a bare `cue vet ./apprafter/...`: the scaffold does not
vendor the schema next to your manifest, so `cue` refuses the import outright.
`app validate` lays the bundled schema into a temporary workspace first.

### Keep `replicas: 1`

A disk provisioned this way admits exactly one writer, and this is enforced
rather than advised: the manifest is **rejected** if such a `needs.disk` appears
with `replicas > 1`. The platform also switches your rollout to `Recreate`, so
an update stops the old pod before starting the new one — a brief gap instead of
an overlap, because two pods cannot hold the same volume at once.

If several applications must read one volume, that is a different primitive:
see [Shared volumes](shared-volumes.md), which is not subject to either
constraint.

Plan for that gap. An application that must never be down cannot use a `local`
disk for the thing that must never be down.

### More than one disk

`needs.disk` also takes an array of named entries, each with its own size and
path:

```cue
needs: {
    disk: [
        { name: "uploads", size: "10Gi", mountPath: "/data/uploads" },
        { name: "cache",   size: "1Gi",  mountPath: "/data/cache" },
    ]
}
```

Each gets its own volume. An entry's identity is its `name`, or — if you omit
one — the last segment of its `mountPath`; either way it must be unique within
the app, as must the paths. Add `readOnly: true` to an entry to mount it
read-only.

## Register the application

```sh
apprafter app add https://github.com/your-org/web.git \
  --name web --namespace demo --project apps
```

If the repository is private, register the credential first:

```sh
apprafter repo creds add web-creds \
  --url-prefix https://github.com/your-org \
  --type pat --token "$YOUR_PAT"
apprafter repo creds list
```

## Watch it come up

```sh
apprafter app status web
```

The volume is created immediately, but it is only **bound to storage when your
pod schedules onto it**. A claim reported as ready while the volume still shows
as pending is the normal intermediate state, not a fault — it resolves as soon
as the pod starts.

??? note "Verify independently with kubectl"

    ```sh
    kubectl -n demo get resourceclaim.apprafter.io web-disk -o \
      jsonpath='ready={.status.ready} vcr={.status.volumeClaimRef}{"\n"}'
    # -> ready=true vcr=claim-demo-web-disk

    kubectl -n demo get pvc claim-demo-web-disk -o \
      jsonpath='phase={.status.phase} size={.spec.resources.requests.storage}{"\n"}'
    # -> phase=Bound size=1Gi   (Pending until the pod schedules)

    kubectl -n demo get deployment web -o jsonpath='{.spec.strategy.type}{"\n"}'
    # -> Recreate
    ```

## Remove the dependency, and get it back

**Dropping the `needs.disk` block from the manifest does not take effect on
push.** Removing a declared dependency is classified as a destructive change,
so the platform holds it: the application pauses at
`AwaitingMigrationApproval` and nothing is torn down until you approve it.

```sh
apprafter migration list
apprafter migration approve <plan-name>
```

The approval gate is covered on [Migration plans](migration-plans.md).

**To retire the application, delete it:**

```sh
apprafter app remove web
```

**The volume survives either way.** It is deliberately not owned by anything
that would cascade, so retiring the application does not delete it. It is kept
for seven days and then deleted automatically.

**Re-declaring the dependency inside that window brings the same volume back**,
with the data still in it: register the application again and the pending
deletion is cancelled, the original volume reattached. This is the intended way
to undo an accidental removal, and `e2e/needs-disk-walk.sh` exercises it end to
end.

There is no supported way to shorten the seven days, and the retention record
is deliberately not hand-editable — it is the work order the cleanup executes,
so a hand-written substitute is a way to delete the wrong volume.

`--keep-data` does something different, despite the name: it strips the cascade
first, so **only** the Argo CD object is deleted and the workload, its
`Application` CR and its volume all keep running. Use it to hand an application
off or re-register it elsewhere, not to retire one.

??? note "Verify independently with kubectl"

    After `app remove` — the snapshot exists and the volume is untouched:

    ```sh
    kubectl -n apprafter-system get retainedclaim claim-demo-web-disk -o \
      jsonpath='vcr={.spec.volumeClaimRef} until={.spec.retainUntil}{"\n"}'
    # -> vcr=claim-demo-web-disk until=<RFC3339, ~7 days out>

    kubectl -n demo get pvc claim-demo-web-disk -o jsonpath='{.status.phase}{"\n"}'
    # -> Bound   (still there)
    ```

    After re-registering, the claim points at the same volume and the snapshot
    is gone — the deletion was cancelled:

    ```sh
    kubectl -n demo get resourceclaim.apprafter.io web-disk \
      -o jsonpath='{.status.volumeClaimRef}{"\n"}'                 # -> claim-demo-web-disk
    kubectl -n apprafter-system get retainedclaim claim-demo-web-disk  # -> NotFound
    ```

## How it works

[Declared disk dependencies](../how-it-works/needs-disk.md) covers why the
volume has no owner and how that single decision is the whole retention model,
what the single-writer constraint forces on the rollout, how names are derived,
and the deletion after the grace window.

## Troubleshooting

| Symptom | Likely cause | Fix |
| ------- | ------------ | --- |
| The application stays at `AwaitingResourceClaim` and the claim never gets a provider | the need matches no provider, or there is no default StorageClass | Confirm `disk-local` carries `tier=integrated`: `kubectl get serviceprovider disk-local -n apprafter-system -o yaml`. Confirm a default StorageClass exists: `kubectl get storageclass`. |
| The volume stays `Pending` | normal until the pod schedules — storage binds on first use | Check the pod: `kubectl -n demo get pod -l app.kubernetes.io/name=web`. If the pod is `Pending` too, its events say why. |
| The manifest is rejected on sync | `needs.disk` with `replicas > 1`, a duplicate disk name or mount path, or a `size` that is not a Kubernetes quantity | Keep `replicas: 1`; give each disk a unique name and path; write `size` as `"1Gi"`. |
| Data gone after deleting and recreating the app | the grace window elapsed, or the bare claim was deleted rather than the application | Remove the **application**, not the claim: deleting the claim while the manifest still declares the need makes the controller regenerate it, which cancels the snapshot. |
| The volume is still there long after the grace window | the source claim is still live — the cleanup skips a volume whose claim exists | Confirm the claim is gone: `kubectl -n demo get resourceclaim.apprafter.io web-disk`, then check the operator log: `kubectl -n apprafter-system logs deploy/apprafter-operator`. |

## Prerequisites

- A Tier-1 cluster provisioned with `apprafter bootstrap-all` (see the
  [Quickstart](quickstart.md)), operator **≥ v0.2.21** — the release that ships
  the disk provisioner, the `disk-local` provider seed, the renderer and
  webhook rules, and the disk cleanup path.
- For the verification blocks above only, a kubeconfig:

  ```sh
  apprafter kubeconfig --refresh > /tmp/kc && export KUBECONFIG=/tmp/kc
  ```

## Cleanup

```sh
apprafter app remove web   # the volume enters its seven-day window
apprafter destroy --yes    # every apprafter=true resource in the token's project
```
