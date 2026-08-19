---
description: "Watching a declared persistent-disk dependency all the way through, including the single-writer constraints it puts on the workload."
---

# needs.disk manual walk — persistent disk from a declared dependency

This guide walks a Tier-1 operator through the full `needs.disk` chain: an
Application that declares `spec.base.needs.disk` gets a persistent block
volume provisioned on demand, mounted into its container at the requested
path, and — when the dependency is removed — the volume retained for a
7-day grace period and then physically deleted.

The backend is an ordinary Kubernetes **PersistentVolumeClaim** (`disk`
type, `ReadWriteOnce`). Each claim provisions one **unowned** PVC on the
disk class advertised by the seeded `disk-local` ServiceProvider — on
Tier-1 that is the in-cluster `local-path` provisioner (one node, one
replica). Because the PVC is single-writer, a `needs.disk` workload is
pinned to `replicas: 1` with a `Recreate` rollout. The full design — the
launch-minimal `class: local` slice, the unowned-PVC retention model, and
the deferred replicated/shared classes — is in
[ADR 0043](https://github.com/apprafter/apprafter/blob/master/docs/adr/0043-needs-disk-named-claims.md).

## The chain in one paragraph

You declare `needs: { disk: { size: "1Gi", mountPath: "/data" } }` on an
Application. The operator generates a `ResourceClaim` (`type: disk`) — the
gate's load-bearing action: it emits a claim per need instead of rendering
the Deployment immediately. The scheduler matches the claim to the seeded
`disk-local` ServiceProvider and marks it `Scheduled=True`. The provisioner
SSA-applies a standalone PVC (`ReadWriteOnce`, the provider's storage
class, the requested size) with **no ownerReference**, records its name in
`status.volumeClaimRef`, and marks the claim `ready=true` in a single pass
— it does NOT wait for the PVC to bind (`local-path` binds
`WaitForFirstConsumer`, i.e. once the pod schedules). The Application then
resumes to `Ready`, and the rendered Deployment mounts that PVC at
`mountPath` and pivots to `strategy: Recreate` (a single writer cannot
overlap during a rollout). When you delete the Application (or remove the
dependency), a finalizer snapshots an immutable `RetainedClaim` (deletion +
7 days, `backend: disk`) carrying the `volumeClaimRef` + namespace — but
the **unowned PVC survives** the app delete and the grace window, so the
data is intact. Re-declaring the dependency reattaches the *same* PVC
(cancel-on-reprovision) with the data in place. Once `retainUntil` passes,
the GC controller deletes the PVC and removes the snapshot.

## How an app uses the disk

- **Mount path:** the volume appears at `needs.disk.mountPath` inside the
  container. Read and write ordinary files there; the data persists across
  pod restarts and rollouts (it lives in the PVC, not the pod).
- **Single writer:** a `local` disk is `ReadWriteOnce` — exactly one pod
  may mount it. The admission webhook therefore rejects `needs.disk`
  together with `replicas > 1`; the renderer also sets `strategy: Recreate`
  so a rolling update never runs two pods against the one volume.
- **Read-only mounts:** set `readOnly: true` on the need to mount the PVC
  read-only (the container cannot write — useful for a sidecar that only
  consumes a volume another need writes).
- **Multiple disks:** `needs.disk` accepts an array of named entries — each
  `{ name: "<n>", size: …, mountPath: … }` provisions its own PVC
  (`<app>-disk-<n>`) at its own path. `name` (or, unnamed, the last
  `mountPath` segment) must be unique within the app.

## Prerequisites

- A real Tier-1 cluster provisioned with `apprafter bootstrap-all` (see
  [`quickstart.md`](quickstart.md)), operator **≥ v0.2.21** (the release
  that ships the `Backend::Disk` provisioner, the `disk-local`
  ServiceProvider seed, the disk renderer/webhook, and the disk GC path).
- `kubectl` bound to the cluster:

  ```sh
  export KUBECONFIG="$(apprafter kubeconfig --refresh)"
  ```

- Pre-flight: the seeded provider, a default StorageClass, and the
  admission webhook are all present:

  ```sh
  kubectl get serviceprovider disk-local -n apprafter-system \
    -o jsonpath='{.metadata.labels.tier} {.spec.backend}{"\n"}'   # -> integrated disk
  kubectl get storageclass                                        # -> local-path (default) on T1
  kubectl -n apprafter-system rollout status \
    deploy admission-webhook                                      # -> available
  ```

## Step 0 — run the k3d e2e first (cheap gate)

Before spending a real Tier-1 cluster, run the automated walk on a local
k3d/kind cluster. It exercises the identical chain (provision → mount →
data durability → delete + snapshot → reattach → force-GC) plus a
`needs.pg`-array multi-claim assertion, and is the pre-manual-walk gate:

```sh
just e2e-disk        # green in ~3-5 min (kind+podman, LOCAL_OPERATOR build)
```

If that is red, fix it before continuing — the manual walk only adds value
once the automated chain is green.

## Platform-CLI coverage

This walk exercises every shipped `platform-cli` subcommand. Raw `kubectl`
is a sanity-only supplement that confirms the machine state behind each CLI
surface.

| Stage | Command |
| ----- | ------- |
| Identity + target | `apprafter target list`, `apprafter target add …`, `apprafter whoami` |
| Self-diagnostic | `apprafter doctor` |
| Provision | `apprafter bootstrap-all` (then re-run `apprafter cluster-bootstrap` once to prove idempotency) |
| Cluster access | `apprafter kubeconfig --refresh`, `apprafter argocd-password` |
| Cluster + platform health | `apprafter status`, `apprafter platform status` |
| Author the manifest | `apprafter app scaffold --needs disk` |
| Register the app | `apprafter app add` (public repo); `apprafter repo creds add/list/show` for the private-repo variant |
| Inspect the app | `apprafter app status`, `apprafter app logs` |
| Portal | `apprafter open argocd` |
| Cleanup | `apprafter app remove --keep-data`, `apprafter destroy --yes` |

### Author the manifest with `apprafter app scaffold`

```sh
apprafter app scaffold --name web --namespace demo --needs disk
```

> The repeatable `--needs disk` flag emits the `spec.base.needs` block for
> you; an unknown type is a clear error. The generated
> `apprafter/Application.cue` carries launch defaults you edit before
> committing:
>
> ```cue
> spec: base: {
>     // ... image / replicas:1 / expose ...
>     needs: {
>         disk: { size: "1Gi", mountPath: "/data" }
>     }
> }
> ```
>
> `cue vet ./apprafter/...` validates it locally before you push. Keep
> `replicas: 1` — a `ReadWriteOnce` disk admits a single writer.

### Register the app with `apprafter app add`

Public repo:

```sh
apprafter app add https://github.com/your-org/web.git \
  --name web --namespace demo --project apps
```

Private repo — register the git credential first:

```sh
apprafter repo creds add web-creds \
  --url-prefix https://github.com/your-org \
  --type pat --token "$YOUR_PAT"
apprafter repo creds list           # confirm it is registered
apprafter repo creds show web-creds
```

## Happy chain — steps 1 to 9

The numbered steps below assume the Application CR is in the cluster (via
Argo CD sync after `app add`, or applied directly for a quick check). The
`kubectl` lines are the machine-readable proof behind each CLI surface; the
constants (`demo` namespace, `web` app, `web-disk` claim, PVC
`claim-demo-web-disk`, volume `disk-data`) are derived deterministically
from the claim's `(namespace, name)` and the `/data` mount path.

> **Naming — the `web` you query is the apprafter.io `metadata.name`, not
> the `apprafter app add` name.** The steps address
> `application.apprafter.io web` and `resourceclaim.apprafter.io web-disk`:
> the kinds are group-qualified on purpose, because bare `application` also
> matches Argo CD's `argoproj.io` Application. If `kubectl get
> application.apprafter.io <name>` returns "not found", you likely used the
> `app add` name; use the `Application.cue` `metadata.name` instead.

**1. The Application carries the dependency.**

```sh
kubectl -n demo get application.apprafter.io web \
  -o jsonpath='size={.spec.base.needs.disk.size} path={.spec.base.needs.disk.mountPath}{"\n"}'
# -> size=1Gi path=/data
```

**2. The operator generated a ResourceClaim.**

```sh
kubectl -n demo get resourceclaim.apprafter.io web-disk \
  -o jsonpath='type={.spec.type} tier={.spec.selector.tier} size={.spec.size}{"\n"}'
# -> type=disk tier=integrated size=1Gi
kubectl -n demo get resourceclaim.apprafter.io web-disk \
  -o jsonpath='{.metadata.ownerReferences[0].kind}{"\n"}'   # -> Application
```

**3. The scheduler matched the disk-local provider.**

```sh
kubectl -n demo get resourceclaim.apprafter.io web-disk \
  -o jsonpath='{.status.provider}{"\n"}'                    # -> disk-local
kubectl -n demo get resourceclaim.apprafter.io web-disk -o \
  jsonpath='{.status.conditions[?(@.type=="Scheduled")].status}{"\n"}'   # -> True
```

**4. The claim is provisioned — an UNOWNED RWO PVC.** The provision is
single-pass: `ready` flips once the PVC object EXISTS (not bound —
`local-path` binds `WaitForFirstConsumer`, so binding waits for the pod).

```sh
kubectl -n demo get resourceclaim.apprafter.io web-disk -o \
  jsonpath='ready={.status.ready} vcr={.status.volumeClaimRef}{"\n"}'
# -> ready=true vcr=claim-demo-web-disk
kubectl -n demo get pvc claim-demo-web-disk -o \
  jsonpath='class={.spec.storageClassName} modes={.spec.accessModes[0]} size={.spec.resources.requests.storage}{"\n"}'
# -> class=local-path modes=ReadWriteOnce size=1Gi
```

**CRITICAL (retention): the PVC is UNOWNED.** No `ownerReferences`, so an
app delete does NOT cascade-delete it — only the 7-day GC drops it.

```sh
kubectl -n demo get pvc claim-demo-web-disk \
  -o jsonpath='{.metadata.ownerReferences}{"\n"}'           # -> (empty)
```

**SSA-split guard.** The provisioner's status write (ready / volumeClaimRef
/ Ready) must NOT clobber the scheduler's verdict — `Scheduled` is still
`True` after provisioning:

```sh
kubectl -n demo get resourceclaim.apprafter.io web-disk -o \
  jsonpath='{.status.conditions[?(@.type=="Scheduled")].status}{"\n"}'   # -> True
```

**5. The Application resumed.**

```sh
kubectl -n demo get application.apprafter.io web -o jsonpath='{.status.phase}{"\n"}'
# -> Ready
```

**6. The PVC is mounted and the rollout is `Recreate`.**

```sh
kubectl -n demo get deployment web -o \
  jsonpath='{.spec.template.spec.containers[0].volumeMounts[?(@.name=="disk-data")].mountPath}{"\n"}'
# -> /data
kubectl -n demo get deployment web -o \
  jsonpath='{.spec.template.spec.volumes[?(@.name=="disk-data")].persistentVolumeClaim.claimName}{"\n"}'
# -> claim-demo-web-disk
kubectl -n demo get deployment web -o jsonpath='{.spec.strategy.type}{"\n"}'
# -> Recreate
```

The PVC binds once the pod schedules onto it:

```sh
kubectl -n demo get pvc claim-demo-web-disk -o jsonpath='{.status.phase}{"\n"}'   # -> Bound
```

**7. Data durability — write a file, restart the pod, the file survives.**

```sh
POD=$(kubectl -n demo get pod -l app.kubernetes.io/name=web -o jsonpath='{.items[0].metadata.name}')
kubectl -n demo exec "$POD" -- sh -c 'echo hello-disk > /data/probe'
kubectl -n demo delete pod "$POD" --wait=true       # Recreate brings a fresh pod
kubectl -n demo wait --for=condition=Available deployment/web --timeout=120s
POD2=$(kubectl -n demo get pod -l app.kubernetes.io/name=web -o jsonpath='{.items[0].metadata.name}')
kubectl -n demo exec "$POD2" -- cat /data/probe     # -> hello-disk (survived the restart)
```

**8. Inspect the app via the CLI.**

```sh
apprafter app status web
apprafter app logs web --tail 50
```

> `apprafter app status web` surfaces, by default, the AppRafter phase, the
> workload Pods and Services with live status, and the ResourceClaim
> provisioning state (provider / ready / Scheduled / volumeClaimRef); add
> `--resources` for the full Argo CD resource tree.

**9. Delete the dependency — the data is retained.** Removing the
`needs.disk` block (and re-syncing) deletes the claim and snapshots a
`RetainedClaim`; for a direct check, delete the **Application** (not the
bare claim — the claim is owned by the app, so deleting it alone while the
app still declares `needs.disk` makes the controller regenerate it):

```sh
kubectl -n demo delete application.apprafter.io web --wait=true
kubectl -n apprafter-system get retainedclaim claim-demo-web-disk -o \
  jsonpath='backend={.spec.backend} vcr={.spec.volumeClaimRef} vcn={.spec.volumeClaimNamespace} until={.spec.retainUntil}{"\n"}'
# -> backend=disk vcr=claim-demo-web-disk vcn=demo until=<RFC3339, ~7 days out>
```

The **unowned PVC survives** the app delete (GC has not fired —
`retainUntil` is days away):

```sh
kubectl -n demo get pvc claim-demo-web-disk -o jsonpath='{.status.phase}{"\n"}'   # -> Bound (still present)
```

**Reattach — re-declaring the dependency reuses the same PVC.** Re-apply
the Application: the provisioner reattaches the *same* PVC (idempotent SSA),
cancels the RetainedClaim, and the file from step 7 is still there.

```sh
kubectl apply -f apprafter/Application.cue   # or re-sync via Argo CD
kubectl -n demo get resourceclaim.apprafter.io web-disk \
  -o jsonpath='{.status.volumeClaimRef}{"\n"}'             # -> claim-demo-web-disk (same PVC)
kubectl -n apprafter-system get retainedclaim claim-demo-web-disk   # -> NotFound (cancelled)
POD3=$(kubectl -n demo get pod -l app.kubernetes.io/name=web -o jsonpath='{.items[0].metadata.name}')
kubectl -n demo exec "$POD3" -- cat /data/probe           # -> hello-disk (data reattached)
```

## MANDATORY: force the GC (the PVC is physically deleted)

The `RetainedClaim` is immutable (a CEL `self == oldSelf` rule), so an
in-place `kubectl patch` of `retainUntil` is **rejected**. Delete the app
to snapshot a fresh `RetainedClaim`, then delete and re-create it with a
past `retainUntil` — your walk kubeconfig is `system:masters`, which the
operator-only webhook permits to CREATE:

```sh
kubectl -n demo delete application.apprafter.io web --wait=true
kubectl -n apprafter-system delete retainedclaim claim-demo-web-disk

kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: RetainedClaim
metadata:
  name: claim-demo-web-disk
  namespace: apprafter-system
spec:
  claimRef:
    name: web-disk
    namespace: demo
  provider: disk-local
  backend: disk
  volumeClaimRef: claim-demo-web-disk
  volumeClaimNamespace: demo
  retainUntil: "2000-01-01T00:00:00Z"
YAML
```

The GC fires immediately. It deletes the unowned PVC, then removes the
snapshot. Confirm the PVC is **physically gone** — not merely that the
snapshot was removed:

```sh
kubectl -n demo get pvc claim-demo-web-disk            # -> NotFound (the PV is reclaimed)
kubectl -n apprafter-system get retainedclaim claim-demo-web-disk   # -> NotFound
```

**If the PVC is still present after the snapshot is gone, the GC did NOT
delete it — STOP, this is a closure-blocking bug.**

## DoD checklist

The walk must exercise both shipped surfaces. Check every box.

**Surface 1 — Argo CD UI (`apprafter open argocd`):**

- [ ] The bootstrap / platform Application is **Synced** + **Healthy**.
- [ ] The `disk-local` ServiceProvider resource shows green.

**Surface 2 — kubectl assertions:**

- [ ] ResourceClaim `web-disk` reaches `status.ready=true` with a
      `status.volumeClaimRef` (steps 2, 3, 4).
- [ ] The PVC is `ReadWriteOnce`, the right storage class + size, and
      **UNOWNED** (no ownerReferences) (step 4).
- [ ] Application `web` transitions `AwaitingResourceClaim → Ready` and the
      Deployment mounts the PVC at `/data` with `strategy: Recreate`
      (steps 5, 6).
- [ ] **Data written under the mount survives a pod restart** (step 7).
- [ ] Deleting the Application writes a `RetainedClaim` (`backend: disk`)
      and the **unowned PVC survives** the grace floor; re-declaring the
      need reattaches the same PVC with the data intact (step 9).
- [ ] **Forcing the GC deletes the PVC — it is physically gone — and the
      snapshot is removed.**

## Troubleshooting

| Symptom | Likely cause | Fix |
| ------- | ------------ | --- |
| Claim stuck `Scheduled` absent / Application stuck `AwaitingResourceClaim` | `needs.disk` does not match any provider, or no default StorageClass | Confirm `disk-local` carries `tier=integrated`: `kubectl get serviceprovider disk-local -n apprafter-system -o yaml`; confirm a default StorageClass exists: `kubectl get storageclass`. |
| PVC stuck `Pending` | `local-path` binds `WaitForFirstConsumer` — this is normal until the pod schedules | Check the pod schedules: `kubectl -n demo get pod -l app.kubernetes.io/name=web`. If the pod is `Pending` too, inspect its events. |
| `kubectl apply` of the Application rejected | admission webhook: `needs.disk` with `replicas > 1`, a duplicate disk name/mountPath, or a `size` that is not a k8s quantity | A `local` disk is single-writer — keep `replicas: 1`; give each disk a unique `name`/`mountPath`; write `size` as a quantity (`"1Gi"`, not a t-shirt size). |
| Data lost after deleting + recreating the app | you deleted the bare ResourceClaim (which can race the regenerated claim) instead of the Application, or the grace window already elapsed | Delete the **Application** to snapshot a RetainedClaim; the PVC survives until `retainUntil`. Re-declaring the need reattaches it. |
| PVC not deleted after `retainUntil` passes | GC controller error, or the source ResourceClaim is still live (the GC live-guard skips the delete while the claim exists) | Confirm the claim is gone: `kubectl -n demo get resourceclaim.apprafter.io web-disk`; check the operator logs: `kubectl -n apprafter-system logs deploy/apprafter-operator`. |

## Cleanup

```sh
apprafter app remove web --keep-data    # leave any retained PVC in place
apprafter destroy --yes                  # tear down the Tier-1 cluster
```
