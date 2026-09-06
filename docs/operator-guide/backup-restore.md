---
description: "Backing a cluster up: a copy on your own machine, a scheduled copy off-site in S3, what each captures, and the retention and integrity work that keeps a repository trustworthy."
---

# Back up a cluster

Two questions, and the whole page is ordered by them: *how do I get a copy on
my own machine*, and *how do I get one off-site, automatically*. Everything
after those two sections is detail you come back for.

Replaying a backup is [Restore from a backup](restore.md). Moving a healthy
cluster onto different hardware is [Moving to a bigger
machine](moving-to-a-bigger-machine.md) — it uses a backup, but it is a planned
operation with its own sequence.

AppRafter's engine is [restic](https://restic.net/), pulled from the operator's
machine. [Velero](https://velero.io/) was evaluated and rejected: it requires
an object-storage bucket as its backup location, which forces a purchase into
the default path, and AppRafter's default is a zero-bucket local pull. The
design rationale is [ADR 0050](../adr/0050-backup-restore.md).

## Back up to your own machine

```sh
apprafter backup create
apprafter backup list
```

That is the whole of it. The repository lands at `<config>/backups/<target>`
under the AppRafter config root; `--repo <path>` puts it elsewhere.
`backup list` (alias `ls`) prints each snapshot's short id, timestamp and tag —
the tag is `<cluster-id>-<created-at>`, so it identifies the source cluster and
the moment, never a single namespace.

```text
apprafter backup create [--repo <path>] [--passphrase <value>] \
                        [--namespace <ns> ...] [--select] \
                        [--staging-mode monolithic|sequential]
```

`--namespace` / `--select` narrow the scope the same way `export` does.
`--staging-mode` matters on a large cluster and nowhere else: the default
`monolithic` stages every namespace's native data at once and takes one
snapshot, while `sequential` stages and snapshots one namespace at a time,
bounding peak staging disk.

### The passphrase is mandatory

The repository contains **decrypted** secrets, so it must be encrypted.
`backup create` resolves the passphrase from `--passphrase`, then
`RESTIC_PASSWORD`, then a masked prompt on an interactive terminal. An empty or
absent passphrase is rejected — the command will not silently produce a
repository encrypted with an empty key.

**The passphrase is yours to keep.** AppRafter does not store it. Lose it and
the backup is unrecoverable. The same goes for the S3 credentials below.

## Back up off-site, on a schedule

One command seals the credentials into the cluster and turns the scheduled
backup on. There is no separate `apprafter secret seal` step:

```sh
apprafter backup enable \
    --bucket <bucket-name> \
    --endpoint <s3-host> \
    --prefix <path/inside/bucket> \
    --credential-file ./s3.env \
    --i-have-saved-credentials
```

`--bucket` takes a **bare name** and the CLI builds the
`s3:https://<endpoint>/<bucket>/<prefix>` repository URL from it. Pass a full
restic URL in `--bucket` instead — and omit `--endpoint`/`--prefix` — when the
backend is not S3 (`b2:`, a local path, …).

The full surface, all optional beyond the four above:

```text
apprafter backup enable --bucket <name> --endpoint <host> [--prefix <path>] \
                        --credential-file <dotenv> [--credential <secret-name>] \
                        [--at 03:00] [--timezone Europe/Berlin] \
                        [--staging-mode monolithic|sequential] \
                        [--enforce operator|cluster] \
                        [--keep-daily N] [--keep-weekly N] [--keep-monthly N] \
                        [--check off|06:00] [--failure-webhook <url>] \
                        --i-have-saved-credentials
```

Exactly one credential input is required: `--credential-file <dotenv>` for a
fresh setup (parsed, probed, then auto-sealed as a `SealedSecret` in
`apprafter-system`, named by `--credential`, default `apprafter-backup-s3`), or
`--credential <name>` when the Secret already exists.

### The credential file

Create a plain `KEY=VALUE` dotenv file. The canonical key names are
S3-vendor-neutral; the `AWS_*` forms are accepted as aliases:

```dotenv
# Canonical form (preferred)
S3_ACCESS_KEY_ID=your-access-key
S3_SECRET_ACCESS_KEY=your-secret-key
RESTIC_PASSWORD=your-restic-passphrase
S3_REGION=eu-central-1       # optional — many S3-compatible stores don't need it

# AWS_* aliases are also accepted — they are restic's own env names, and
# work against any S3-compatible store:
# AWS_ACCESS_KEY_ID     → same as S3_ACCESS_KEY_ID
# AWS_SECRET_ACCESS_KEY → same as S3_SECRET_ACCESS_KEY
# AWS_DEFAULT_REGION    → same as S3_REGION
```

Required keys: `S3_ACCESS_KEY_ID` (or alias), `S3_SECRET_ACCESS_KEY` (or
alias), `RESTIC_PASSWORD`. `S3_REGION` is optional. When both the canonical and
alias form of a key appear, the canonical form wins. The CLI normalises aliases
to canonical names before sealing — the in-cluster Secret always holds the `S3_*`
keys regardless of what the dotenv file used.

The credential Secret is a **platform** secret and must live in
`apprafter-system` — `--credential-file` seals it there for you. If you seal by
hand, leave `apprafter secret seal` at its default namespace, and read
[Secrets](secrets.md) on what re-sealing replaces.

### The preflight, in order

`enable` does not blindly patch the CR. It runs a fail-closed preflight, and
stops at the first failure:

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

### Defaults, and the time of day

Defaults when a flag is omitted: `--at` `03:00` (nightly), `--check` three
hours later on Sunday, `--staging-mode` `monolithic`, `--enforce` `operator`,
retention `--keep-daily 7 --keep-weekly 4 --keep-monthly 6`.

**`--timezone` defaults to the machine you run the command on.** A time of day
without a zone is not a time: `03:00` means three in the morning where you are,
and the CLI records which zone that was so the schedule keeps meaning it. If it
cannot determine your zone it says so and asks — it will not quietly pick UTC.
Pass `--timezone UTC` if that is what you want.

`apprafter backup status` prints the schedule back as a time in the same zone,
so what you read is what you set.

One night a year, in a zone that observes summer time, an `--at` inside the
skipped hour means that night's backup does not run and the next one does —
choosing a time outside 01:00–03:00 avoids it in most European and North
American zones.

## Check on it, and turn it off

```sh
apprafter backup disable
```

`disable` sets `spec.backup.enabled=false` and **keeps** every other configured
field, so a later `enable` re-uses the same bucket, credential, and retention.

```sh
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

## What a backup captures

### The data behind your `needs`

This is what an operator is usually asking, so it comes first. One entry per
declared dependency:

| `needs` type | What lands in the repository | Not captured |
| --- | --- | --- |
| `pg` | the **database itself** — a `pg_dump` in custom format, one file per claim, taken through a helper pod against the claim's own credentials | a claim with no connection Secret yet, i.e. one that has never finished provisioning |
| `disk`, and a `SharedVolume` reference | the **volume contents**, as a tar of the bound `PersistentVolumeClaim` | a claim whose volume is not bound yet |
| `redis` with `persistent: true` | the **whole Dragonfly instance** the claim shares — a `SAVE`, then a tar of its snapshot directory. Backup dedupes by instance, so every persistent claim on it travels together | — |
| `redis` with `persistent: false` | nothing | a cache by declaration, with no durable volume to snapshot. A restore re-provisions it empty |

**Every other `needs` type is skipped, silently and by design.** `jetstream`,
`clickhouse`, `s3` and `notifications` have no capture path in this release
(`cli/backup-core/src/extract.rs`), so a cluster using one of them is **not**
fully covered by a backup. Nothing warns you at backup time; this table is the
warning.

### The objects that describe your cluster

The backup distinguishes **user** material from **platform** material — the
discrimination that keeps a restore from clobbering the target's own bootstrap:

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

How a restore reloads each of these is on
[Restore from a backup](restore.md).

## Inspect the data without making a backup

`export` is a read-only convenience: it pulls the live native data to a flat,
self-contained folder. It writes **no** custom resources, **no** secrets, and
applies **no** encryption — use it to inspect a database locally or to migrate
out of the platform, not as a disaster-recovery artifact.

```text
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

```text
apprafter-export/
  pg/<ns>/<claim>.dump          # pg_dump -Fc (custom format)
  volumes/<ns>/<claim>/data.tar # tar of the volume contents
  redis/<ns>/<claim>/dump.tar   # tar of a persistent Dragonfly snapshot
  manifest.json                 # cluster id, platformVersion, namespaces, resources
```

The `redis/` directory holds one `dump.tar` per **persistent** redis claim —
a whole-instance Dragonfly snapshot. Ephemeral (`persistent: false`) claims
carry no durable data and are skipped, so they never appear here.

The pg dumps are standard PostgreSQL custom-format archives: open them with
any matching `pg_restore`, e.g. `pg_restore -l pg/demo/shop-pg.dump` to list
the table of contents, or restore into a local database with
`pg_restore --no-owner -d <local-db> pg/demo/shop-pg.dump`. Volume tars are
plain tarballs: `tar -tf volumes/demo/shop-disk/data.tar`.

## See also

- [Restore from a backup](restore.md) — replaying what this page creates.
- [Backup retention, integrity and credentials](backup-maintenance.md) — how
  long snapshots live, the integrity check, and narrowing the in-cluster
  credentials.
- [Moving to a bigger machine](moving-to-a-bigger-machine.md) — the planned
  rebuild that a backup makes possible.
- [Secrets](secrets.md) — sealing, and what re-sealing replaces.
- `e2e/backup-restore-hetzner.sh` and `e2e/backup-s3-hetzner.sh` — this page as
  executable specifications, run on real hardware.
