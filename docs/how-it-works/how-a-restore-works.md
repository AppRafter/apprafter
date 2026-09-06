---
description: "The order a restore replays a backup in, the two invariants that order protects, how the data is loaded, and why secrets are re-sealed rather than copied."
---

# How a restore replays a backup

Why a restore does what it does, in the order it does it. The recipes are
[Back up a cluster](../operator-guide/backup-restore.md) and
[Restore from a backup](../operator-guide/restore.md); neither needs any of
this.

Read it when a restore produced a cluster you did not expect, or when you are
judging whether a backup taken under one platform version can be replayed under
another.

The decision behind the engine — a local pull on restic rather than an
object-storage-first design — is
[ADR 0050](../adr/0050-backup-restore.md).

## The order, and the two invariants it protects

The full restore is a fixed sequence:

```text
RestoreArtifact -> ApplyPlatformStack -> ApplySourceCredentials ->
ApplyAppsGated -> WaitClaimsBound -> LoadData -> ReSealUserSecrets ->
ResumeWorkloads
```

Two behaviours are load-bearing:

- **Workloads are gated during the load.** The apps are applied with
  `replicas: 0` (and the user Argo Applications have their `syncPolicy.automated`
  stripped) so the operator provisions fresh claims but **no pod runs yet**.
  The data is loaded into the empty, freshly-provisioned backends, and only
  then are the workloads resumed at their original replica count (and Argo
  auto-sync re-enabled). This is what lets a framework-style tracked migration
  see the restored state and **skip** on boot, instead of racing the load.
- **Wait for the claim, not for the volume to bind.** `WaitClaimsBound`
  polls each regenerated `ResourceClaim` until `status.ready == true`, **not**
  until the PVC is `Bound`. A disk claim reports ready as soon as its
  `volumeClaimRef` is set; on a `WaitForFirstConsumer` StorageClass the PVC
  only binds when its first consumer pod schedules — and the restore's own
  load helper is that first consumer. Waiting for `Bound` would deadlock.

## How the data is loaded

- **PostgreSQL** is restored over an ephemeral helper pod that pipes the dump
  on **stdin** to `pg_restore --no-owner --clean --if-exists`. The connection
  credentials come from the claim's **fresh** `status.connectionSecretRef`
  (the post-provision Secret), never the credentials embedded in the backup.
  `--no-owner` is assumed because the restored database role is the
  newly-provisioned one, not whatever owned the objects on the source.
- **Volumes** are restored by streaming the tar on stdin to `tar x` in a
  helper pod that mounts the fresh PVC read-write.
- **Redis** (persistent claims) is restored by live-loading the captured
  Dragonfly snapshot into the running instance with `DFLY LOAD`: the tar is
  unpacked into the instance's snapshot directory and the latest snapshot is
  loaded on the data port (admin password read from the instance's `-admin`
  Secret). Nothing is scaled or restarted, so the claim provisioner never
  re-provisions (and FLUSHes) the DB mid-restore. Ephemeral
  (`persistent: false`) claims carry no snapshot and come back empty — see the
  note at the top of this page.

## Secrets are re-sealed, not copied

SealedSecrets are bound to the cluster that sealed them — the material only
unseals as `<namespace>/<name>` under the controller key of that cluster. So
`restore` does not copy the source SealedSecrets; it reads the decrypted
material from the backup and **re-seals** it against the target's own
sealed-secrets controller public key, then applies the fresh SealedSecret.
The Kubernetes secret `type` is round-tripped (e.g. `kubernetes.io/tls` stays
a TLS secret, not an `Opaque` one). The two capture paths re-seal into their
respective namespaces: app user secrets into their app namespace,
`SourceCredential` material into `apprafter-system`.

## See also

- [Restore from a backup](../operator-guide/restore.md) — the three target
  modes and the disaster-recovery runbook.
- [Back up a cluster](../operator-guide/backup-restore.md) — what the artefact
  this replays actually contains.
