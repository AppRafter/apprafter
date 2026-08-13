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
- **(c) clone-to-new** (`--reprovision`): the "source cluster is dead, rebuild
  from nothing" path. `restore --reprovision --target <name>` provisions **and**
  bootstraps a fresh cluster in the target (the same provisioning
  `apprafter up` runs — the server type, region, and cloud token come from the
  target's **local** configuration, so the target must still be registered via
  `apprafter target add`), then replays the backup exactly as mode (a). Use it
  when you have only the backup artifact and a registered target, e.g.:

  ```console
  # the source cluster is gone; the encrypted restic repo survived off-cluster
  $ RESTIC_PASSWORD=… apprafter restore /backups/prod-repo --reprovision --target prod
  ```

  It is **real-Hetzner only** (kind has no cloud provider). `--reprovision` and
  `--data-only` are mutually exclusive (one rebuilds the whole cluster, the
  other reloads data into a running one) and the combination is rejected up
  front. Platform-version alignment rides the backup's captured `PlatformStack`
  (applied during the replay) plus a cross-version warning if the freshly
  bootstrapped platform differs. This is the flow the Phase-4 external-S3 DR
  drill ("restore a new cluster from backup in < 1 hour") and the Phase-8.5
  `DisasterRecoveryPlan` build on.

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

## Off-site scheduled backup (S3)

The commands above are an **operator-machine local pull** — you run them by
hand, and the encrypted repository lands on your laptop. On top of that same
engine, AppRafter can run the backup **on a schedule, inside the cluster**,
pushing the encrypted restic repository to a user-configured external object
store (S3-compatible — AWS S3, Cloudflare R2, Backblaze B2, Hetzner Object
Storage, Scaleway, …). The cluster then keeps an off-site backup with no
operator laptop in the loop.

It is **opt-in and GitOps-native**: no bucket is ever forced (local pull stays
the default), and enabling it is a declarative patch of
`PlatformStack.spec.backup`, not an imperative `kubectl apply` of a CronJob. A
default-off platform-stack chart component renders two CronJobs (a daily backup
and a weekly integrity check), a scoped ServiceAccount, and a
CiliumNetworkPolicy from that typed spec. There is **no** operator controller
and **no** first-class health condition — failure surfaces as a non-zero Job
exit plus a status ConfigMap plus an optional webhook (below).

The design and the rejected alternatives (K8up / helper-runs-restic) are in
[ADR 0050](https://github.com/apprafter/apprafter/blob/master/docs/adr/0050-backup-restore.md).

### The two-tier credential model — read this first

The single most important idea: **the operator owns the full credentials; the
cluster gets a reduced copy.**

- **Operator credentials (authoritative, mandatory).** The restic passphrase
  (`RESTIC_PASSWORD`) plus full S3 keys live **with you, outside the cluster**
  (a password manager). They are required for the `backup enable` preflight,
  for operator-side retention (`backup prune`) and integrity checks
  (`backup check`), and — critically — for **restore after the cluster is
  dead**. If these live only in the cluster, then when the cluster dies the
  SealedSecret and the controller key die with it, and the off-site repository
  becomes unreadable exactly when you need it. `backup enable` refuses to
  proceed without them (see the DR confirmation below).
- **In-cluster credentials (reduced).** One Kubernetes Secret in
  `apprafter-system`, holding the same keys but **scoped** S3 rights (the
  default `operator` enforce mode narrows delete to `locks/*` — the
  scoped-credentials ladder below). On Tier 1 this is a SealedSecret you seal;
  on Tier 2+ it is OpenBao via the same `credentialRef`.

The in-cluster backup CronJobs read credentials from a Kubernetes Secret in
`apprafter-system` via explicit `secretKeyRef` entries. The Secret holds
**neutral canonical keys** (`S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`,
`RESTIC_PASSWORD`, optionally `S3_REGION`); the chart template maps them to the
`AWS_*` names that restic expects. This means the credential format is
**S3-vendor-neutral** — the key names do not imply any Amazon product.

### Enabling

`backup enable` is a **single command** — it seals the credential Secret into
the cluster and enables the scheduled backup in one step, with no separate
`apprafter secret seal` required:

```
apprafter backup enable --bucket s3:<endpoint>/<bucket>/<prefix> \
                        --credential-file <dotenv> \
                        [--credential apprafter-backup-s3] \
                        [--cron "0 3 * * *"] \
                        [--staging-mode monolithic|sequential] \
                        [--enforce operator|cluster] \
                        [--keep-daily N] [--keep-weekly N] [--keep-monthly N] \
                        [--check-cron "0 6 * * 0"] \
                        [--failure-webhook <url>] \
                        --i-have-saved-credentials
```

Exactly **one** of the following credential input forms is required:

- **`--credential-file <dotenv>`** (fresh setup — the common path). The CLI
  parses the file, probes the repository, then **auto-seals** the credentials as
  a `SealedSecret` in `apprafter-system`. The sealed Secret's name is set by
  `--credential` (default: `apprafter-backup-s3`). No separate
  `apprafter secret seal` step is needed.

- **`--credential <name>`** (Secret already exists). The CLI reads the named
  Secret from the cluster to obtain the credentials for the repository probe.
  No sealing step — the Secret must already exist.

#### Credential file format

Create a plain `KEY=VALUE` dotenv file. The canonical key names are
S3-vendor-neutral; the `AWS_*` forms are accepted as aliases:

```dotenv
# Canonical form (preferred)
S3_ACCESS_KEY_ID=your-access-key
S3_SECRET_ACCESS_KEY=your-secret-key
RESTIC_PASSWORD=your-restic-passphrase
S3_REGION=eu-central-1       # optional — many S3-compatible stores don't need it

# AWS_* aliases are also accepted (any S3-compatible store; these are restic's
# own env names and do not imply Amazon-specific services):
# AWS_ACCESS_KEY_ID     → same as S3_ACCESS_KEY_ID
# AWS_SECRET_ACCESS_KEY → same as S3_SECRET_ACCESS_KEY
# AWS_DEFAULT_REGION    → same as S3_REGION
```

Required keys: `S3_ACCESS_KEY_ID` (or alias), `S3_SECRET_ACCESS_KEY` (or
alias), `RESTIC_PASSWORD`. `S3_REGION` is optional. When both the canonical and
alias form of a key appear, the canonical form wins. The CLI normalises aliases
to canonical names before sealing — the in-cluster Secret always holds the `S3_*`
keys regardless of what the dotenv file used.

> **Namespace matters.** The backup credential Secret is a **platform** secret
> and must live in `apprafter-system`. `backup enable --credential-file` seals
> it there automatically. If you seal manually (the `--credential <name>` path),
> the default `--namespace apprafter-system` in `apprafter secret seal` is
> correct — leave it at the default (a SealedSecret only unseals as
> `<namespace>/<name>`, so sealing into an app namespace means the runner never
> finds it).

> **Re-sealing a Secret replaces its keys — it does not merge.** If you run
> `apprafter secret seal` on a name that already exists, all keys in the sealed
> Secret are replaced by the new ones. Pass all keys in a single command. On an
> interactive terminal the CLI prompts for confirmation; in non-interactive
> shells pass `--yes` to skip the prompt (without it the command errors instead
> of silently overwriting).

`enable` does **not** blindly patch the CR. It runs a **fail-closed preflight**
in order:

1. **Credential source resolved** — from `--credential-file` (parsed, normalised
   to canonical `S3_*` keys) or from the live cluster Secret named by
   `--credential`. Missing or empty required keys produce an error that names
   the specific missing key(s) and explains both input paths.
2. **restic version** — the operator's system `restic` must be **≥ 0.14** (repo
   format v2). Not on `PATH` is an error; a confidently-lower version is an
   error.
3. **Repo reachability** — `restic cat config` against `--bucket` (an existing
   repo) or, if that fails on an empty bucket, `restic init`. This validates the
   endpoint, credentials, and passphrase **now**, and it means a typo in
   `--bucket` can't silently create a second empty repo. The runner **never**
   auto-inits at run time — an unreadable repo is an honest failure that points
   back at `backup enable`.
4. **Auto-seal** (only when `--credential-file` is given) — the CLI seals the
   canonical `S3_*` credential map into `apprafter-system` as a `SealedSecret`.
5. **DR confirmation** — `--i-have-saved-credentials` (or an interactive
   confirm; non-interactive without the flag is an error). This makes the
   operator-owns-the-keys rule *material*: you physically cannot enable without
   asserting that the passphrase and S3 credentials are saved outside the
   cluster.

Only after all of these pass does `enable` **merge-patch**
`PlatformStack.spec.backup`.

> **GitOps advisory.** If `PlatformStack.spec.backup` is git-managed via Argo CD,
> the next sync will overwrite an imperative patch — set the backup block in
> your **infra repo** for a durable change. The CLI prints this reminder after
> a successful `enable`.

Defaults when a flag is omitted: `--cron` `"0 3 * * *"` (daily 03:00),
`--check-cron` `"0 6 * * 0"` (weekly Sunday 06:00, staggered clear of the
backup), `--staging-mode` `monolithic`, `--enforce` `operator`, retention
`--keep-daily 7 --keep-weekly 4 --keep-monthly 6`.

### Disable and status

```
apprafter backup disable
```

`disable` sets `spec.backup.enabled=false` and **keeps** every other configured
field, so a later `enable` re-uses the same bucket, credential, and retention.

```
apprafter backup status
```

`status` reads three sources and reconciles them into one view:

- the resolved `PlatformStack.spec.backup` (bucket, schedule, enforce mode,
  retention, …);
- the last backup and check **Job** outcomes;
- the non-chart-owned **`apprafter-backup-status` ConfigMap** in
  `apprafter-system` — the runner create-or-updates it on every run with
  `lastSuccess`, `lastFailure`, `lastError` (short), and `lastRunFormat`. This
  is why it is a ConfigMap and not just Job history: the `failedJobsHistoryLimit`
  can rotate the last *successful* Job out of view, but "when did the last
  successful backup run" — the core backup question — stays reliably
  answerable;
- the `apprafter.io/last-prune` annotation stamped on `PlatformStack` by the
  operator-side `backup prune`.

### The scoped-credentials ladder — `enforce: operator` vs `cluster`

`--enforce` controls **who runs retention** and therefore **how much delete
power the cluster credential needs.**

**`enforce: operator` (the default).** The in-cluster Secret should carry S3
rights scoped to **Put / Get / List on the repository prefix, plus Delete only
on `locks/*`.** The scheduled backup Job then does `restic backup` only — it
**cannot** delete `data/`, `index/`, or `snapshots/` objects, so a cluster
compromise (ransomware) cannot erase the backup history. Retention runs
**outside** the cluster: you run `apprafter backup prune` with your **full**
credentials (see Retention below). A minimal bucket/IAM policy shape:

```jsonc
// enforce: operator — cluster credential (scoped, append-only-ish)
{
  "Statement": [
    {                                    // write + read the repo
      "Effect": "Allow",
      "Action": ["s3:PutObject", "s3:GetObject", "s3:ListBucket"],
      "Resource": ["arn:aws:s3:::my-bucket", "arn:aws:s3:::my-bucket/my-prefix/*"]
    },
    {                                    // delete ONLY restic lock files
      "Effect": "Allow",
      "Action": ["s3:DeleteObject"],
      "Resource": ["arn:aws:s3:::my-bucket/my-prefix/locks/*"]
    }
  ]
}
```

**`enforce: cluster`.** The in-cluster Secret carries **full** credentials, and
the backup Job runs `restic forget --prune` in-cluster after each backup. No
operator-side prune is needed, but a compromised cluster can now delete the
whole history — the trade-off is documented and opt-in. Use it only with
compensating provider controls (object versioning / object lock).

> **Statement-level granularity is provider-dependent.** "Delete only on
> `locks/*`" needs **statement-level** scoping (different actions on different
> prefixes), which AWS-style policies express but some key-prefix-only providers
> do not. If your provider cannot express it and you decline to hand the cluster
> full delete, issue a credential with **no** Delete at all: `backup` still
> works (restic uses non-exclusive locks), but the in-cluster `check` cannot
> drop its own lock — set `--check-cron off` to disable the in-cluster check and
> run `apprafter backup check` operator-side instead. Verify your provider's
> behavior (this is spec item **V2** in the Verify checklist below).

> **Hetzner Object Storage (the flagship provider) — branch (a), verified.**
> Hetzner OS supports statement-level bucket policies with per-key `Principal`
> and per-prefix `Resource`/`NotResource` scoping, so the scoped model IS fully
> expressible **and enforced** (empirically confirmed 2026-07-17: under a policy
> denying `s3:DeleteObject` outside `locks/*`, a `data/` delete is refused while
> a `locks/` delete succeeds). Two differences from the illustrative AWS shape:
>
> - Hetzner OS keys default to **full access to every bucket in the project**,
>   so you narrow a key with explicit **`Deny`** statements (an Allow-only policy
>   does *not* reduce the default). Reference a key as the principal via
>   `arn:aws:iam:::user/p<project_id>:<access_key>`.
> - **V7 reduction (also verified):** you may additionally deny the cluster key
>   `s3:GetObject` on `data/*` — a `restic backup` still succeeds (it reads
>   `index/` + `snapshots/`, never the `data/` packs), so a compromised cluster
>   can neither *erase* nor *read* the historical backup data, only the small
>   metadata. `check` / `restore` / `prune` run operator-side with full creds.
>
> Create a **second, cluster-only** access key (the operator's key stays
> full-access and is not named in the policy), then apply this bucket policy:
>
> ```jsonc
> {
>   "Version": "2012-10-17",
>   "Statement": [
>     { "Sid": "ClusterDenyDeleteOutsideLocks", "Effect": "Deny",
>       "Principal": { "AWS": "arn:aws:iam:::user/p<project_id>:<cluster_key>" },
>       "Action": "s3:DeleteObject",
>       "NotResource": "arn:aws:s3:::my-bucket/my-prefix/locks/*" },
>     { "Sid": "ClusterDenyReadData", "Effect": "Deny",           // V7 (optional)
>       "Principal": { "AWS": "arn:aws:iam:::user/p<project_id>:<cluster_key>" },
>       "Action": "s3:GetObject",
>       "Resource": "arn:aws:s3:::my-bucket/my-prefix/data/*" }
>   ]
> }
> ```

> **Confidentiality caveat (read this).** The scoped / append-only credential
> protects the **integrity and availability** of the backup history — an
> attacker with the cluster's credential cannot *erase* your snapshots. It does
> **not** protect confidentiality. The cluster credential still has `GetObject`
> and holds `RESTIC_PASSWORD`, so anyone who compromises the cluster can **read
> every snapshot in the history** — including secrets that were rotated long ago
> and are no longer live in the cluster. restic encrypts the repository with the
> passphrase, so confidentiality rests entirely on that passphrase; that is why
> the passphrase must be saved **outside** the cluster and, in the spirit of the
> `operator` model, kept off the cluster wherever the workflow allows. Do not
> read "survives a compromise" as "the history is secret."

### Retention and prune

Retention uses **restic's own** snapshot retention — a host- and format-aware
`forget --prune` keyed by keep-daily / keep-weekly / keep-monthly (built-in
defaults **7 / 4 / 6**). In the default `enforce: operator` mode you run it
yourself, outside the cluster, with full credentials:

```
apprafter backup prune [--repo s3:…] \
                       [--credential-file <dotenv>] \
                       [--keep-daily N] [--keep-weekly N] [--keep-monthly N]
```

`--repo` defaults to `PlatformStack.spec.backup.bucket`; the keep-* flags
override the configured `spec.backup.retention` (else the 7/4/6 defaults).
Credentials resolve from `--credential-file` then the environment. On success
`prune` stamps `apprafter.io/last-prune` on `PlatformStack`, which
`backup status` then shows. Run it on your own cadence (e.g. monthly) — restic
dedup makes growth sub-linear, so retention is a rare, deliberate operation, not
a per-run one.

> **Why not an S3 bucket lifecycle rule?** It is tempting to set a bucket-level
> "delete objects older than N days" lifecycle rule and skip `prune` entirely.
> **Do not** — it will corrupt the repository. restic is content-addressed and
> packs *many* snapshots' data into shared `data/` pack files; a fresh snapshot
> routinely references pack objects that are physically old. A lifecycle rule
> deletes objects by *object age*, so it will delete still-referenced packs and
> leave the repo unrestorable. Retention **must** go through
> `restic forget --prune` (i.e. `apprafter backup prune`), which walks the
> reference graph and only removes truly unreferenced packs.

### Integrity checks and locks

```
apprafter backup check [--repo s3:…] [--credential-file <dotenv>] [--read-data]
```

`check` runs `restic check` against the repository — the same verification that
the in-cluster **`apprafter-backup-check` CronJob** runs weekly (default
`0 6 * * 0`). By default it verifies structure only; `--read-data` re-downloads
and re-hashes **every** pack for a deep verify (slower, bandwidth-heavy). Run the
operator-side `check` when your provider can't express the scoped-delete policy
and you disabled the in-cluster check (`--check-cron off`), or any time you want
a manual verification with full credentials.

> **Where check failures surface.** The weekly in-cluster check runs `restic`
> directly (the runner binary has no check-only mode), so a failed check shows
> up as a **`Failed` Job** in `apprafter-system` (kept per
> `failedJobsHistoryLimit`) — it does **not** write the `apprafter-backup-status`
> ConfigMap (only the backup runner does). So `apprafter backup status` reflects
> the last *backup* outcome; for the last *check* outcome, look at the
> `apprafter-backup-check` Job history (`kubectl get jobs -n apprafter-system`)
> or run `apprafter backup check` yourself.

```
apprafter backup unlock [--repo s3:…] [--credential-file <dotenv>]
```

`unlock` removes **only stale** locks (`restic unlock`) — a live lock held by a
running backup is never touched. Reach for it when a Job was killed mid-run
(OOM, a node reboot) and left a lock behind that blocks the next operation. The
in-cluster CronJobs already unlock stale locks as their first step, so you
mostly need `unlock` for operator-side `prune`/`check` against a repo whose last
in-cluster run died unexpectedly.

### Restore from S3 (disaster-recovery runbook)

Restore reads the repository over S3 using the **operator's** credentials —
**never** from the cluster (in a real DR the source cluster is gone):

```
apprafter restore s3:<endpoint>/<bucket>/<prefix> \
                  --credential-file <dotenv> \
                  [--reprovision | --data-only] [--target <name>] \
                  [--snapshot <id>]
```

`--credential-file` (or `S3_ACCESS_KEY_ID` / `S3_SECRET_ACCESS_KEY` /
`RESTIC_PASSWORD` in the environment — `AWS_*` aliases are also accepted) is
**required** for an `s3:` repo; the operator's full credentials are read locally.
The DR steps:

1. **Obtain the passphrase + S3 credentials you saved out-of-band** — from your
   password manager, the artifacts the `--i-have-saved-credentials` gate made
   you save. Confirm your operator machine has `restic ≥ 0.14`.
2. **Put them in a dotenv file** (`RESTIC_PASSWORD`, `S3_ACCESS_KEY_ID`,
   `S3_SECRET_ACCESS_KEY`, optional `S3_REGION`) and pass it as
   `--credential-file`, or export the matching env vars (`AWS_ACCESS_KEY_ID` /
   `AWS_SECRET_ACCESS_KEY` / `AWS_DEFAULT_REGION` are accepted as aliases).
3. **Choose the mode** (the same modes as the local-pull restore above — see
   [Target modes](#target-modes)):
   - `--reprovision` — the source cluster is dead: provision **and** bootstrap a
     fresh cluster in the registered target, then replay. Real-Hetzner only.
   - neither flag (restore into a running, already-bootstrapped target; select
     it with `--target <name>`) — the default path.
   - `--data-only` — reload only native data into an already-configured target.
4. Restore **auto-detects the backup format** — monolithic (the default, one
   snapshot per run) vs sequential (a versioned snapshot-set) — by reading the
   manifest version, so you don't specify the format. `latest` resolves to the
   freshest run of **either** format. Both staging formats restore identically
   from the operator's side; the only difference is on the write path.

The restore ordering, the H2/R1 invariants, and the secret re-sealing behavior
are identical to the local-pull restore documented above — the only difference
is the repository lives in S3 and the credentials come from the operator, not
the target.

### Verify checklist (operator-runnable)

Two checks are worth running before you trust the off-site backup, mapping to
the design's Verify items:

- **V2 — confirm your provider honors a prefix-scoped delete.** With the
  `enforce: operator` scoped credential, actively **test** that the cluster
  credential can delete an object under `locks/*` but is **refused** deleting an
  object under `data/` (or `snapshots/`). If the deny doesn't hold, your
  provider can't express the append-only guarantee — fall back to
  `enforce: cluster` with provider object-lock, or to the no-delete +
  `--check-cron off` variant.
- **V7 — a minimal end-to-end.** `apprafter backup enable` → wait for (or
  trigger) one backup Job → `apprafter backup status` shows a fresh
  `lastSuccess` → `apprafter restore s3:… --reprovision` into a **throwaway**
  cluster (or `--target` a running one) and confirm your data and a sealed
  secret came back.
  This is the only test that proves the whole chain — the passphrase you saved,
  the bucket policy, the runner, and the restore path — actually works together.

## Follow-on work

- **Clone-to-new** (`--reprovision`) ships and is real-Hetzner-validated (see
  Target modes (c)); full cross-version DR reconciles with the external-S3 DR
  drill and the `DisasterRecoveryPlan` resource.

## See also

- The end-to-end harness: `e2e/backup-restore-walk.sh` exercises
  export → backup → restore-into-fresh → data-only on local clusters.
- [ADR 0050](https://github.com/apprafter/apprafter/blob/master/docs/adr/0050-backup-restore.md)
  for the full design and the rejected alternatives.
