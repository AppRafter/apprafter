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
design rationale, and the order a restore replays this in, are
[How a restore replays a backup](../how-it-works/how-a-restore-works.md).

## Back up to your own machine

```sh
apprafter backup create
apprafter backup list
```

That is the whole of it. The repository lands at `<config>/backups/<target>`
under the AppRafter config root; `--repo <path>` puts it elsewhere.
`backup list` (alias `ls`) prints each snapshot's short id, timestamp, cluster
and tag — the tag is `<cluster-uid>-<created-at>`, so it identifies the source
cluster and the moment, never a single namespace.

## What is in a backup

```sh
apprafter backup show                 # the latest snapshot
apprafter backup show <snapshot-id>
```

`show` answers the question a snapshot id and a timestamp cannot: what the
run actually captured. It prints the source cluster, the platform-stack
version it ran, the snapshot's size, the namespaces in scope, and a count of
the captured resources by kind — with `ResourceClaim` broken down by backend,
because "3 claims" does not tell you which databases are in there.

The numbers come from the `manifest.json` the backup itself carries, so they
describe the snapshot rather than the cluster it was taken from. That is the
distinction that matters when you are deciding whether a backup from three
weeks ago still covers what you have now.

```sh
apprafter backup list --details
```

adds size and per-snapshot counts to the listing, so two runs can be compared
down the columns and the row where a count moves is the run where something
entered or left the cluster. It costs three restic calls per snapshot, which
is why it is not the default. A snapshot whose manifest cannot be read still
gets a row, with dashes rather than zeroes — "unknown" and "none" are
different answers.

**`backup list` follows the cluster.** Once off-site backup is enabled, a bare
`backup list` shows what the *schedule* stored, because those are the cluster's
backups; `--local` shows this machine's repository instead, and `--repo` names
one directly. The heading above the table always says which repository you are
looking at. With no cluster reachable — the disaster-recovery case — it falls
back to the local repository rather than failing.

**It also shows only *this* cluster's snapshots.** A repository can hold more
than one cluster's, and the listing says so at the bottom when it withheld any:

```sh
apprafter backup list --all-clusters
```

shows every snapshot with the cluster each belongs to, and names the cluster
identities the repository holds under the table. That is how you find the id of
another cluster's run when you genuinely want it — `restore --snapshot <id>`
takes it from there — and how you read off the UID that `backup prune
--cluster-uid` needs when a cluster is gone. Rows marked `(legacy)` were written
before snapshots carried a cluster identity and are being treated as this
cluster's by assumption; with no cluster reachable, nothing can be narrowed and
everything is listed. [Which snapshots are
yours](../how-it-works/backup-retention-and-checks.md#which-snapshots-are-yours)
is the mechanism.

`apprafter backup show` with no snapshot named follows the same rule: it shows
**this** cluster's latest run, not the repository's. It is read-only, but it is
what you read before deciding what to restore, so it has to be looking at the
snapshot a restore would replay.

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
                        [--cluster-name <name>] \
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

`--cluster-name` is the name this cluster's snapshots are listed under, and it
defaults to the target name, so most setups never pass it. It matters when two
clusters share one repository: the listing is read by that name. Change it later
with `apprafter backup set cluster-name <name>` — it is a label, and snapshots
are attributed by the cluster's own identity rather than by it.

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

Values are taken exactly as written, so surrounding quotes or a trailing space
become part of the secret — a store that rejects a key you know is good is
usually reporting one of those.

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

### When the preflight rejects the repository

Step 3 is the one that fails in the field. The error names the obstacle and
what to do about it; two of the shapes are worth knowing in advance, because
the second one causes the first.

**"repository already contains keys."** The location holds restic key files
but no repository config. `restic init` writes the master key before it writes
the config, so an init that dies in between leaves exactly this — and restic
refuses to init over existing keys, so the location is now wedged and every
later `enable` fails the same way. Two ways out: delete the `keys/` prefix
under the repository path and re-run, or point `--prefix` at a fresh path
inside the same bucket and leave the old objects where they are.

One caveat. If the location also holds `data/` and `snapshots/`, this is not
an interrupted init but a repository that lost its config. Those snapshots
cannot be read without it, and deleting the keys will not bring them back.

**"Access Denied" on a write.** The store accepted the credentials and refused
the write, which is a permission problem rather than a wrong key or a wrong
passphrase. restic needs read, write **and** delete across the whole
repository prefix: it creates `config`, `keys/`, `data/`, `index/` and
`snapshots/`, and removes objects again when a backup is pruned. Check what
the access key is granted and whether a bucket policy narrows it.

The error also says whether that run left a key file behind, and it is not
guessing: restic names the object it was saving, and `config` is written after
the master key. A failure on `config` means the key is already stored and has
to be cleared before retrying; a failure on the key itself means the location
is untouched.

When the refusal names `config` specifically, the store may be taking writes
under a path while refusing them at the root of the bucket. `--prefix <path>`
puts the whole repository one level down and gets past it. That is a fine
permanent arrangement rather than a workaround — a prefix per cluster is a
common way to share one bucket.

A prefix per cluster is no longer *required* for correctness, though: two
clusters writing to one restic repository are told apart by their own
identities, so neither can restore or prune the other's snapshots ([which
snapshots are
yours](../how-it-works/backup-retention-and-checks.md#which-snapshots-are-yours)).
Separate prefixes still give you separate repositories, which means separate
deduplication, separate locks and a separate `restic check` — reasons to keep
doing it, none of them about safety.

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

### The first backup runs immediately

`enable` does not leave you waiting until 03:00 to find out whether any of this
works. Once the platform chart has deployed the schedule, the CLI runs it once
and reports the outcome, so the command that configures backup is also the
command that proves it: the cluster's own credentials, the runner's RBAC, and
egress from the cluster to your bucket are all exercised for real.

Waiting for the chart takes a few minutes — Argo CD reconciles on its own
cycle. If it has not landed in that window, `enable` says so and stops: backup
is enabled either way, and `apprafter backup run` takes the first one whenever
you like. `--no-initial-backup` skips this entirely.

## Back up right now

```sh
apprafter backup run
```

Runs the cluster's scheduled backup immediately, without waiting for its
window — before an upgrade, before a risky migration, or to see a snapshot
appear after enabling. It instantiates the platform's backup CronJob as a
one-off Job, so it is the *same* backup the schedule takes, with the same
image and the same in-cluster credentials; nothing S3-related is needed on
your machine.

The command waits and reports the result. `--no-wait` returns as soon as the
Job is created, and `--timeout <minutes>` bounds the wait — neither cancels
anything, because the Job belongs to the cluster once it exists. Ctrl-C is
equally safe.

A suspended schedule (`backup disable`) does not block a manual run: taking one
last backup after turning the schedule off is a normal thing to want.

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
`clickhouse`, `s3` and `notifications` have no capture path in this release,
so a cluster using one of them is **not**
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
- **App user secrets** are captured by a SealedSecret-backed sweep across
  **every** namespace, not only the ones in scope: a Secret is carried when a
  SealedSecret of the same name exists (a secret you sealed). Derived secrets
  the operator re-creates on restore — connection Secrets, docker
  pull-secrets — are not carried.

  The sweep follows the SealedSecrets rather than the applications on
  purpose. Credentials are routinely sealed *before* the deployment that will
  use them, into a namespace that holds no `Application` yet; scoping the
  sweep to application namespaces dropped exactly those, and a substrate
  migration through backup/restore would have asked the operator to seal them
  again by hand. `apprafter backup show` prints a `secrets from:` line
  whenever the secret namespaces are wider than the application ones, and
  `restore` creates those namespaces before replaying into them.

  One consequence worth stating: the backup credential itself
  (`apprafter-backup-s3`) is a sealed secret, so it travels in the
  repository too. The repository is encrypted, and this changes nothing about
  who can read it — but it does mean a restore hands the new cluster the
  credentials of the repository it came from.
- **`SourceCredential` material** lives in `apprafter-system`, **outside** the
  app-namespace set, so the app-ns sweep misses it. The backup instead
  follows each `SourceCredential`'s `spec.git.backend.sealedSecretRef` and
  `spec.registry.backend.sealedSecretRef` and reads the underlying unsealed
  material directly. This is a distinct, cluster-wide capture path.
- **Imported TLS certificates** — the Secrets `apprafter target cert import`
  put in `apprafter-system`, carrying the `apprafter.io/cert-mode: imported`
  label. They are captured whole, labels and annotations included, by a third
  path of their own: an imported certificate has no SealedSecret behind it, so
  the sweep above cannot see it, and no controller can re-issue it — the
  material only exists because an operator supplied it.

  This is the other half of a connected domain. The zones themselves live in
  `PlatformStack.spec.values.gateway.allowedDomains`, so they come back with
  the config CRs; the platform Gateway is rendered from those zones with
  `tls.certificateRefs` naming the certificate Secret. A backup that carried
  the zones but not the certificate restored a Gateway pointing at a Secret
  that was not there — the domains would resolve and the site would not serve.
  `apprafter backup create` names the certificates it captured in its summary,
  `apprafter backup show` lists them as `ImportedCert`, and `apprafter target
  domain list` marks a reference with no Secret behind it as `MISSING`.

  Certificates issued by cert-manager are **not** captured: they are re-issued
  on the restored cluster, and replaying an old one would be worse than
  letting it renew.
- **The Cloudflare origin-firewall toggle** rides the `PlatformStack` like the
  zones do, in `spec.firewall.cloudflareOrigin`. The firewall itself is a cloud
  object the CLI reconciles from your machine, so the cluster is not what
  enforces it — but the cluster is what a backup can read, which is why the
  intent is recorded there. `apprafter target firewall cloudflare-origin
  enable` writes both: your target's config, which `apprafter apply` builds the
  node's firewall from, and the CR, which every backup captures. That includes
  the scheduled in-cluster runner, which has no target store to read and so
  could never have recorded it otherwise.

  `apprafter restore --reprovision` reads the field back off the restored CR
  and turns the firewall on for the target it provisioned. A snapshot of a
  cluster that never recorded an answer — anything taken before this field
  existed — is treated as *unknown*, not as "the source had it off": the
  restore says nothing rather than guessing. See
  [Move to a bigger machine](moving-to-a-bigger-machine.md).

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
