# Backup, export, and restore

AppRafter ships a local-pull backup and restore engine built on
[restic](https://restic.net/). It captures the things a cluster needs to be
rebuilt — native database dumps, volume contents, the AppRafter and Argo CD
custom resources, and the decrypted user secrets — into a single encrypted
repository on the operator's machine, and replays them into a running target
cluster.

There are three commands:

| Command | Captures | Encrypted | Replays |
| --- | --- | --- | --- |
| `apprafter export` | native data only (pg dumps, volume tars, redis snapshots) + `manifest.json` | no | no — a plain folder for inspection / migrate-out |
| `apprafter backup create` | native data **plus** config/app CRs **plus** decrypted user secrets | yes (restic) | via `apprafter restore` |
| `apprafter restore` | — | — | replays a `backup` into a running target cluster |

The full design rationale is in
[ADR 0050](https://github.com/apprafter/apprafter/blob/master/docs/adr/0050-backup-restore.md).
[Velero](https://velero.io/) was evaluated and rejected — it requires an
object-storage bucket as its backup location, which would force a purchase
into the default path; AppRafter's default is a zero-bucket local pull.

## `apprafter export` — Kind 1, native data only

`export` is a read-only convenience: it pulls the live native data to a flat,
self-contained folder. It writes **no** custom resources, **no** secrets, and
applies **no** encryption — use it to inspect a database locally or to migrate
out of the platform, not as a disaster-recovery artifact.

```
apprafter export [--out <dir>] [--namespace <ns> ...] [--select]
```

- Default output directory: `./apprafter-export`.
- Default scope is the **whole cluster** — every namespace that hosts an
  AppRafter `Application`. The scope derives from
  `kubectl get applications.apprafter.io -A`, **not** from `kubectl get ns`,
  so platform and system namespaces are never swept in.
- `--namespace <ns>` is repeatable but is only honoured when `--select` is
  also passed; without `--select`, the scope stays whole-cluster.

The layout:

```
apprafter-export/
  pg/<ns>/<claim>.dump          # pg_dump -Fc (custom format)
  volumes/<ns>/<claim>/data.tar # tar of the volume contents
  redis/<ns>/<claim>/           # documented skeleton
  manifest.json                 # cluster id, platformVersion, namespaces, resources
```

The pg dumps are standard PostgreSQL custom-format archives: open them with
any matching `pg_restore`, e.g. `pg_restore -l pg/demo/shop-pg.dump` to list
the table of contents, or restore into a local database with
`pg_restore --no-owner -d <local-db> pg/demo/shop-pg.dump`. Volume tars are
plain tarballs: `tar -tf volumes/demo/shop-disk/data.tar`.

## `apprafter backup create` — Kind 2, the disaster-recovery artifact

`backup create` runs the same native extraction as `export`, then **also**
serializes the config and app custom resources and the decrypted user
secrets, and wraps everything into an encrypted restic repository.

```
apprafter backup create [--repo <path>] [--passphrase <value>] \
                        [--namespace <ns> ...] [--select]
apprafter backup list   [--repo <path>] [--passphrase <value>]
```

- Default repo path: `<config>/backups/<target>` (under the AppRafter config
  root). Pass `--repo` to override.
- Default scope is the whole cluster; `--namespace`/`--select` narrow it the
  same way as `export`.
- `apprafter backup list` (alias `ls`) prints the snapshots in a repo: their
  short id, timestamp, and tags. The snapshot tag is
  `<cluster-id>-<created-at>` (plus a `-ns-<joined>` marker for a namespace
  subset), so it identifies the **source cluster and the moment**, never a
  single namespace.

### What is captured (and what is not)

The backup distinguishes **user** material from **platform** material — the
load-bearing discrimination that keeps a restore from clobbering the target's
own bootstrap (H1 in the ADR):

- **Config CRs** are captured by kind: the `PlatformStack/default` singleton
  and every `SourceCredential` (cluster-wide). There is no in-cluster
  `Infrastructure` CR — the infrastructure topology is the local manifest, and
  its essentials ride `manifest.platformVersion`.
- **AppRafter `Application` CRs** are captured for the in-scope namespaces.
- **Argo CD `Application` CRs** are captured **only** when they carry the
  `apprafter.io/managed-by=apprafter` label (the apps you registered with
  `apprafter app add`). The platform umbrella and per-component Argo
  Applications lack the label and are never captured — otherwise the restore
  would double-own the target's own bootstrap Applications.
- **`SharedVolume` CRs** are captured for the in-scope namespaces.
- **App user secrets** are captured by a SealedSecret-backed sweep over the
  in-scope namespaces: a Secret is carried only when a SealedSecret of the
  same name exists (a secret you sealed). Derived secrets the operator
  re-creates on restore — connection Secrets, docker pull-secrets — are not
  carried.
- **`SourceCredential` material** lives in `apprafter-system`, **outside** the
  app-namespace set, so the app-ns sweep misses it. The backup instead
  follows each `SourceCredential`'s `spec.git.backend.sealedSecretRef` and
  `spec.registry.backend.sealedSecretRef` and reads the underlying unsealed
  material directly. This is a distinct, cluster-wide capture path.

### The passphrase is mandatory

The repository contains **decrypted** secrets, so it must be encrypted.
`backup create` resolves the passphrase from `--passphrase`, then the
`RESTIC_PASSWORD` environment variable, then (on an interactive terminal) a
masked prompt. An empty or absent passphrase is rejected — the command will
not silently produce an unencrypted-by-empty-key repository.

**The passphrase is your responsibility.** AppRafter does not store it. Lose
it and the backup is unrecoverable. The same goes for any cloud token you
supply once S3 backends ship (below) — AppRafter never persists them.

## `apprafter restore` — replay a backup into a running target

```
apprafter restore <repo> [--target <name>] [--snapshot <id>] \
                  [--data-only] [--passphrase <value>] [--reprovision]
```

`restore` replays a `backup create` artifact into a **running, already
bootstrapped** target cluster. The target defaults to the active target; pass
`--target <name>` to pick another registered target. `--snapshot` selects a
specific snapshot (default `latest`).

### Target modes

- **(a) restore-into-running** (the default, validated path): the target was
  already provisioned and `cluster-bootstrap`-ed at a matching platform
  version. `restore` replays the config CRs, gates the apps, waits for the
  freshly-provisioned claims, loads the data, re-seals the secrets, and
  resumes the workloads.
- **(b) data-only** (`--data-only`): the target is already configured with the
  same apps; `restore` reloads only the native data. It scales the existing
  apps to zero (and disables their Argo auto-sync so the scale-down is not
  reverted), loads the data, and resumes them. No CR or secret replay.
- **clone-to-new** (re-provision a fresh cluster from scratch, then replay) is
  the `--reprovision` flag; it is **deferred** to a later subphase and
  currently exits with a clear error. Today, bootstrap the target first, then
  restore into it. Full cross-version / cross-topology DR (`source is dead,
  rebuild from nothing`) reconciles with the Phase-4 external-S3 DR drill and
  the Phase-8.5 `DisasterRecoveryPlan`.

### Restore ordering and the two safety invariants

The full restore is a fixed sequence:

```
RestoreArtifact -> ApplyPlatformStack -> ApplySourceCredentials ->
ApplyAppsGated -> WaitClaimsBound -> LoadData -> ReSealUserSecrets ->
ResumeWorkloads
```

Two behaviours are load-bearing:

- **H2 — workloads are gated during the load.** The apps are applied with
  `replicas: 0` (and the user Argo Applications have their `syncPolicy.automated`
  stripped) so the operator provisions fresh claims but **no pod runs yet**.
  The data is loaded into the empty, freshly-provisioned backends, and only
  then are the workloads resumed at their original replica count (and Argo
  auto-sync re-enabled). This is what lets a framework-style tracked migration
  see the restored state and **skip** on boot, instead of racing the load.
- **R1 — wait for the claim, not for the volume to bind.** `WaitClaimsBound`
  polls each regenerated `ResourceClaim` until `status.ready == true`, **not**
  until the PVC is `Bound`. A 2.6b disk claim reports ready as soon as its
  `volumeClaimRef` is set; on a `WaitForFirstConsumer` StorageClass the PVC
  only binds when its first consumer pod schedules — and the restore's own
  load helper is that first consumer. Waiting for `Bound` would deadlock.

### How the data is loaded

- **PostgreSQL** is restored over an ephemeral helper pod that pipes the dump
  on **stdin** to `pg_restore --no-owner --clean --if-exists`. The connection
  credentials come from the claim's **fresh** `status.connectionSecretRef`
  (the post-provision Secret), never the credentials embedded in the backup.
  `--no-owner` is assumed because the restored database role is the
  newly-provisioned one, not whatever owned the objects on the source.
- **Volumes** are restored by streaming the tar on stdin to `tar x` in a
  helper pod that mounts the fresh PVC read-write.
- **Redis** restore is a documented skeleton today and is exercised on the
  live walk.

### Secrets are re-sealed for the target

SealedSecrets are bound to the cluster that sealed them — the material only
unseals as `<namespace>/<name>` under the controller key of that cluster. So
`restore` does not copy the source SealedSecrets; it reads the decrypted
material from the backup and **re-seals** it against the target's own
sealed-secrets controller public key, then applies the fresh SealedSecret.
The Kubernetes secret `type` is round-tripped (e.g. `kubernetes.io/tls` stays
a TLS secret, not an `Opaque` one). The two capture paths re-seal into their
respective namespaces: app user secrets into their app namespace,
`SourceCredential` material into `apprafter-system`.

## Assumptions and portability

- **GitOps survives.** The restore replays the Argo CD `Application` CRs, which
  point at your Git repositories — it does **not** carry your application
  source. The assumption is that your Git history is intact; the restore
  re-registers the apps and Argo CD pulls the workloads back from Git.
- **The restic repository is portable.** It is a plain restic repo — read it
  with stock `restic` (`restic -r <repo> snapshots`, `restic -r <repo>
  restore latest --target <dir>`) and the passphrase, with no AppRafter
  involvement. There is no lock-in.
- **Version alignment.** The default restore path targets a cluster
  bootstrapped at the same platform-stack version as the backup. A
  cross-version restore is not blocked, but it warns: a different target
  version may re-render components, so verify after restoring.

## Follow-on work

- **Automated S3 backup (opt-in).** A scheduled push of the same restic
  repository to a remote (S3 / SFTP / rclone — R2, Scaleway, B2, Hetzner) is a
  follow-on. Local pull stays the default; the bucket is never forced.
- **Clone-to-new** (`--reprovision`) and full cross-version DR land in later
  subphases, reconciled with the external-S3 DR drill and the
  `DisasterRecoveryPlan` resource.

## See also

- The end-to-end harness: `e2e/backup-restore-walk.sh` exercises
  export → backup → restore-into-fresh → data-only on local clusters.
- [ADR 0050](https://github.com/apprafter/apprafter/blob/master/docs/adr/0050-backup-restore.md)
  for the full design and the rejected alternatives.
