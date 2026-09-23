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
RestoreArtifact -> ApplyImportedCerts -> ApplyPlatformStack ->
ApplySourceCredentials -> ApplyAppsGated -> WaitClaimsBound -> LoadData ->
ReSealUserSecrets -> ResumeWorkloads
```

Five behaviours are load-bearing:

- **The certificate lands before the domains that name it.** The registered
  zones live in the `PlatformStack`, and the platform Gateway is rendered from
  them with `tls.certificateRefs` pointing at the imported certificate's Secret
  in `apprafter-system`. `ApplyImportedCerts` therefore runs *before*
  `ApplyPlatformStack`: replaying the zones first would publish a Gateway whose
  certificate reference resolves to nothing, if only for the seconds between
  two steps. The certificates are applied as the plain `kubernetes.io/tls`
  Secrets they were captured as, labels included — unlike the material under
  `secrets/`, they are not re-sealed, because nothing sealed them at the source
  and the import labels are what `apprafter target domain add` and the next
  backup's capture both key on. A snapshot that carries zones but no
  certificate — anything taken before certificates were captured — is reported
  by name in the summary rather than left to surface as a TLS error.
- **Workloads are gated during the load.** The apps are applied with
  `replicas: 0` (and the user Argo Applications have their `syncPolicy.automated`
  stripped) so the operator provisions fresh claims but **no pod runs yet**.
  The data is loaded into the empty, freshly-provisioned backends, and only
  then are the workloads resumed at their original replica count (and Argo
  auto-sync re-enabled). This is what lets a framework-style tracked migration
  see the restored state and **skip** on boot, instead of racing the load.
- **The count to come back to outlives the process.** A full restore reads each
  application's replica count from the backup artifact, so a second attempt
  reads the same number as the first. A `--data-only` restore has no artifact to
  read it from — the applications are already in the cluster and it replays no
  custom resources — so it reads the count off the live object, and the live
  object is exactly what it is about to overwrite with a zero. A run that dies
  between the two would leave the next run reading its own zero as the app's
  size. The count is therefore written **to the application**, as the
  `apprafter.io/pre-restore-replicas` annotation, in the same merge-patch that
  scales it down; a later run prefers the annotation over the live field, and
  the resume clears it. It is also the only record an operator who abandons a
  restore has, which is why it is on the object rather than in the process.
- **Wait for the claim, not for the volume to bind.** `WaitClaimsBound`
  polls each regenerated `ResourceClaim` until `status.ready == true`, **not**
  until the PVC is `Bound`. A disk claim reports ready as soon as its
  `volumeClaimRef` is set; on a `WaitForFirstConsumer` StorageClass the PVC
  only binds when its first consumer pod schedules — and the restore's own
  load helper is that first consumer. Waiting for `Bound` would deadlock.
- **The replayed backup schedule is a question, asked before it is applied.**
  `ApplyPlatformStack` replays the captured `PlatformStack`, and `spec.backup`
  is part of it — the bucket, the credential reference, the schedule, the
  timezone, the retention counts and `enforce`. The operator projects that block
  straight into the platform chart's values, so a restored cluster that inherits
  it begins backing up to the **source's** repository on its own. That is
  correct in two of the three restores — [disaster
  recovery](../operator-guide/restore.md), and [moving to a bigger
  machine](../operator-guide/moving-to-a-bigger-machine.md) once the old cluster
  is retired — and wrong for a second cluster that should leave the old
  destination alone. Nothing in the artifact distinguishes them, so when the
  replayed block is enabled the restore asks, naming the repository and what two
  live writers would mean. `--keep-backup-schedule` and
  `--discard-backup-schedule` answer it up front; a run with no terminal and no
  flag stops rather than pick a side. A discarded schedule is replayed exactly
  as captured with `spec.backup.enabled: false`, so `apprafter backup set
  enabled true` turns it on unchanged. The summary states the answer either way.
  This applies to every mode that replays the CR — `--reprovision` only prepends
  a provisioning step — and not to `--data-only`, which replays no CRs at all
  and so is never asked.

  The tempting shortcut is to detect the dangerous case instead of asking: look
  in the repository for snapshots newer than the one being restored, and treat
  them as proof the source kept writing. It fails in the unsafe direction on the
  most ordinary flow there is — take a backup now, restore it into the new
  machine immediately. No newer snapshot exists, and the source is very much
  alive.
- **The origin firewall is reconciled from the CR, one step late by
  construction.** The Cloudflare origin firewall is a cloud object, not a
  Kubernetes one: it is reconciled against the provider API from the target on
  the operator's machine. What travels in the snapshot is the *intent*, in
  `PlatformStack.spec.firewall.cloudflareOrigin` — recorded there rather than
  in the backup manifest so that the scheduled in-cluster runner, which has no
  target store to read, captures it like any other field. Immediately after
  `ApplyPlatformStack`, a `--reprovision` restore reads the field back off the
  CR it just applied and, if it says `true`, writes the toggle onto the
  destination target and reconciles that target's live firewall.

  The order means the node exists from step one and the intent is only readable
  at step four, so the new node's `80`/`443` are open for the length of the
  restore. That is inherent rather than an oversight: the snapshot is behind a
  kubeconfig that does not exist until the cluster does. The carry is
  one-directional — a recorded `true` restricts ports, a recorded `false`
  changes nothing — so inheriting can never leave a cluster more exposed than
  doing nothing. An absent field is *unknown*, not "off", and produces no claim
  either way. A failure to apply the firewall never fails the restore; the
  summary reports it and says the ports are still open.

## How the data is loaded

- **PostgreSQL** is restored over an ephemeral helper pod that pipes the dump
  on **stdin** to `pg_restore --no-owner --clean --if-exists`. The connection
  credentials come from the claim's **fresh** `status.connectionSecretRef`
  (the post-provision Secret), never the credentials embedded in the backup;
  the helper's container reads the password from that Secret by reference, so
  the Pod object carries none.
  `--no-owner` is assumed because the restored database role is the
  newly-provisioned one, not whatever owned the objects on the source. The
  helper connects with `client_connection_check_interval` set to ten seconds,
  as a backup's does. `--clean` starts with `DROP TABLE`, which waits behind
  any open transaction that has read the table, and while it waits every later
  query on that table queues behind it. Without the setting, a restore stopped
  at that point would leave its `DROP` waiting on the server after the helper
  pod was gone; with it, the server ends that session within seconds.
- **Volumes** are restored by streaming the tar on stdin to `tar x` in a
  helper pod that mounts the fresh PVC read-write.
- **Redis** (persistent claims) is restored by live-loading the captured
  Dragonfly snapshot into the running instance with `DFLY LOAD`: the tar is
  unpacked into the instance's snapshot directory and the latest snapshot is
  loaded on the data port. The load authenticates with the instance's admin
  password as the Dragonfly container already holds it, in the
  `DFLY_requirepass` variable its operator sets from the instance's `-admin`
  Secret, so the password is on no command line and the restore never reads
  it; a container without it stops the load before the snapshot directory is
  touched. Nothing is scaled or restarted, so the claim provisioner never
  re-provisions (and FLUSHes) the DB mid-restore. Ephemeral
  (`persistent: false`) claims carry no snapshot and come back empty — see the
  note at the top of this page.
- **JetStream** streams are restored over the NATS wire from a helper pod in
  the namespace the message server runs in, authenticated as the namespace's
  **manager** user, whose name and password the helper's container reads by
  reference from `nats-mgr-<ns>` beside the server — a claim's own user is
  denied the snapshot API by design, and the manager identity is the one the design
  reserves for this. The stream is **deleted and replayed**, because a restore
  refuses a stream that already exists, and the controller that creates
  declared streams will have recreated this one empty from its declaration
  moments earlier. Losing that race is retried rather than reported: the
  snapshot carries the messages and the consumers with their pending state,
  while the stream's *configuration* keeps coming from the declaration in Git.
  On a `--data-only` run the same replacement happens to a live stream, so
  anything it holds that the snapshot does not is discarded.

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
- [How retention and the integrity check work](backup-retention-and-checks.md) —
  whether the run you are about to replay still exists, and whether it verifies.
