---
description: "What the export and backup commands each capture, where it lands, and how a backup is replayed into a running cluster."
---

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
| `apprafter export` | native data only — pg dumps, volume tars, and persistent-redis snapshots + `manifest.json` | no | no — a plain folder for inspection / migrate-out |
| `apprafter backup create` | the same native data **plus** config/app CRs **plus** decrypted user secrets | yes (restic) | via `apprafter restore` |
| `apprafter restore` | — | — | replays a `backup` into a running target cluster |

!!! note "Redis: persistent claims are captured and restored; ephemeral caches are not"

    A `persistent: true` `needs.redis` claim **is** backed up and restored.
    `export` and `backup create` capture it as a whole-instance Dragonfly
    snapshot — a `SAVE` on the instance, then a tar of its snapshot directory
    written to `redis/<ns>/<claim>/dump.tar`
    (`cli/backup-core/src/extract.rs`). `restore` reloads it by live-loading
    that snapshot into the **running** instance with `DFLY LOAD` — nothing is
    scaled or restarted, so the claim provisioner never re-provisions (and
    FLUSHes) the DB mid-restore
    (`cli/platform-cli/src/commands/restore.rs`).

    A `persistent: false` claim is **not** captured — it is a cache by
    declaration, with no durable PVC to snapshot, and a restore re-provisions it
    empty. The persistence gate is `cli/backup-core/src/extract.rs:101`.

    **Whole-instance semantics.** A Dragonfly *instance* fronts a pool of claims
    that share it, each on its own logical DB. The snapshot and `DFLY LOAD` act
    on the **whole instance**, so backup dedupes by instance and a restore
    brings back every persistent claim on that instance together — restoring one
    app's redis rolls its pool-siblings back to the same point in time.

The full design rationale is in
[ADR 0050](../adr/0050-backup-restore.md).
[Velero](https://velero.io/) was evaluated and rejected — it requires an
object-storage bucket as its backup location, which would force a purchase
into the default path; AppRafter's default is a zero-bucket local pull.

## `apprafter export` — Kind 1, native data only

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

## `apprafter backup create` — Kind 2, the disaster-recovery artifact

`backup create` runs the same native extraction as `export`, then **also**
serializes the config and app custom resources and the decrypted user
secrets, and wraps everything into an encrypted restic repository.

```text
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
own bootstrap:

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

```text
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
  bootstrapped platform differs. This is the flow a full disaster-recovery
  drill exercises — restore a new cluster from a backup in under an hour. A
  `DisasterRecoveryPlan` object that automates the drill is not implemented
  yet; today the drill is this command, run deliberately.

  Recovery is not its only use. The same mode, given `--server-type`, is how a
  **healthy** cluster is moved onto a bigger machine — a planned operation with
  a different shape and a different checklist. See
  [Move a cluster onto a bigger machine](#substrate-upgrade).

### Restore ordering and the two safety invariants

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

### How the data is loaded

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

## Move a cluster onto a bigger machine {#substrate-upgrade}

The node under a healthy cluster has become too small — it is out of
allocatable memory and the scheduler has started refusing pods — and you want
the same cluster, with the same data, on a bigger machine. There is no
in-place resize, so the move is a rebuild: take a backup, release the machine,
provision a bigger one, and replay the backup into it.

It runs the same `--reprovision` command as disaster recovery, and it is not
the same operation. In a disaster the source cluster is already gone, the
backup is whatever the schedule last managed to take, and you are recovering
from a position you did not choose. Here the source is healthy and in your
hands: you take the backup yourself and know it is current, you drain what
will not survive, and you pick the hour. If your cluster is already dead, read
[Target modes](#target-modes) (c) instead — the sequence below assumes a
cluster that still works.

**What this section does not cover.** The machine-choice side of the move
lives on [Choosing the
machine](choosing-the-machine.md#changing-the-machine): why `apprafter target
machine` refuses on a running cluster, how wide `apprafter destroy` really is
(it empties a provider **project**, not a cluster), the variant that stands
the new cluster up in a second Hetzner project and keeps the old one serving
until you are satisfied, and the cluster-name collision that turns a rebuild
into a silent no-op. Read that page once before your first upgrade. What
follows is the backup side: the sequence for each storage backend, what to
verify when it is over, and what it costs you.

### Before you start

- **Persistent redis comes across; ephemeral caches do not.** A restore
  live-loads each `persistent: true` `needs.redis` claim's Dragonfly snapshot
  back into place — see the note at the top of this page. A `persistent: false`
  claim is re-provisioned empty (it is a cache by declaration). A planned
  upgrade is the good case for the latter: drain a queue or accept an empty
  cache before you take the backup. In a disaster you get no such chance.
- **A bigger node does not raise per-application limits.** The default memory
  limit is 512Mi on every machine size and does not grow with the node — see
  [Resources and
  autoscaling](../dev-guide/resources-and-autoscaling.md#when-512mi-is-the-problem).
  If one application is the thing hitting a ceiling, a bigger machine may not
  be the fix you need.
- **Do not resize in the provider console instead.** It looks cheaper and it
  leaves the cluster's recorded machine type disagreeing with the live one,
  which `apprafter apply` then warns about on every run until you reconcile it
  with `apprafter import --force`. The route below keeps the record and the
  machine in step.

### It is one cluster, not two

`apprafter destroy` clears the recorded cluster from local state; it does not
touch the target. The name, the token, the region and the SSH key all survive,
which is exactly what lets `restore --reprovision --target <same>` provision
back into the same target and the same Hetzner project. You register no second
target, issue no second token, and cut nothing over: at the end there is one
cluster in one project, on a bigger machine.

That is what separates this from the second-project route on [Choosing the
machine](choosing-the-machine.md#changing-the-machine), which deliberately
runs two clusters at once — safer, and more expensive for as long as both are
up.

### The sequence — host-local repository

```sh
# 1 — back up, into a repository that is not on the cluster
RESTIC_PASSWORD=<passphrase> apprafter backup create --repo /backups/prod-repo

# 2 — confirm the snapshot is really there, before anything is destroyed
RESTIC_PASSWORD=<passphrase> apprafter backup list --repo /backups/prod-repo

# 3 — release the machine (read the destroy-scope warning on the machine page)
apprafter destroy --yes --target prod

# 4 — record the new machine on the target, in the one window that allows it
#     (see "the target keeps reporting the old machine" below)
apprafter target machine --target prod --server-type cx33

# 5 — provision the bigger machine and replay the backup into it
RESTIC_PASSWORD=<passphrase> apprafter restore /backups/prod-repo \
    --reprovision --server-type cx33 --target prod
```

### The sequence — off-site (S3) repository

When the cluster already pushes an encrypted repository off-site on a schedule
([Off-site scheduled backup](#off-site-scheduled-backup-s3)), the artifact you
restore from is already there. Step 1 becomes a check that it is current and
intact rather than a fresh backup.

```sh
# 1 — confirm the off-site repository is current and sound.
#     Do this BEFORE the destroy — see the second defect below for why.
apprafter backup status
apprafter backup check --repo s3:<endpoint>/<bucket>/<prefix> \
    --credential-file ./operator-s3.env

# 2 — release the machine
apprafter destroy --yes --target prod

# 3 — record the new machine on the target
apprafter target machine --target prod --server-type cx33

# 4 — provision the bigger machine and replay from off-site
apprafter restore s3:<endpoint>/<bucket>/<prefix> \
    --reprovision --server-type cx33 --target prod \
    --credential-file ./operator-s3.env
```

The credentials in `--credential-file` are the **operator's**, read from your
own machine: between steps 2 and 4 there is no cluster left to read a
credential from. That is the [two-tier credential
model](#off-site-scheduled-backup-s3) doing the exact job it exists for, and a
substrate upgrade is a cheap way to find out whether yours actually works —
if the operator-side credentials cannot reach the repository while the cluster
is down, neither could a real recovery.

Scheduled backup survives the move. `PlatformStack.spec.backup` is part of the
captured configuration, so the rebuilt cluster comes back with the same
bucket, schedule and retention, and `apprafter backup status` reports it
enabled again without you re-running `apprafter backup enable`.

### What to verify afterwards

`restore` reports success once it has replayed the artifact. That is not the
same as the upgrade having worked. Six checks, each earning its place:

1. **The server type, read from the provider.** Take it from the Hetzner Cloud
   Console or the provider API — not from `apprafter target show` and not from
   local state. Local records say what AppRafter asked for; only the provider
   says what it got, and the two can disagree (they do; see below).
2. **The server id changed.** A new id is what proves a genuinely new machine
   rather than a local record rewritten around the old one. If the id is the
   same, no rebuild happened — the most likely cause is the cluster-name
   collision described on [Choosing the
   machine](choosing-the-machine.md#changing-the-machine), where provisioning
   finds the existing machine, reconciles it, and never uses the
   `--server-type` you passed.
3. **Node allocatable memory grew.** This is the number the scheduler budgets
   against, and it is the reason you did any of this — a bigger SKU whose
   allocatable did not move has bought you nothing.

    ```sh
    kubectl get node -o jsonpath='{.items[0].status.allocatable.memory}'
    ```

    Across the two validation runs it went from 1963 MiB to 5895 MiB on a
    `cx23` → `cx33` move.

4. **The workload reached `Ready` on its own.** Not "the pods exist" — the
   application's own readiness, arrived at without you intervening. The
   restore resumes workloads only after the data is loaded, so an application
   that reaches `Ready` did so against restored state.
5. **A sealed secret decrypts to its original value.** SealedSecrets are bound
   to the cluster that sealed them, so every one of them was re-sealed against
   the new cluster's key during the replay ([Secrets are re-sealed for the
   target](#secrets-are-re-sealed-for-the-target)). Read one back and compare
   it to what you put in. The failure this catches is an application sitting
   at `Ready=False` with `EnvSecretMissing`.
6. **The data, compared properly.** A marker row is not evidence. A single row
   you planted and read back proves only that *a* restore happened; it would
   still read back if every other table had been truncated. Use the stronger
   check the walk uses:

    - **per-table row counts**, compared table for table, so "rows went
      missing" is a distinguishable failure from "the same rows, different
      bytes"; and
    - **a content digest** over every user table, hashed with an explicit,
      deterministic `ORDER BY` — otherwise the digest depends on the order
      rows happen to come back in and tells you nothing.

    Compute both on the source before the backup and again on the upgraded
    cluster. Compute each **twice on each side**: if the two computations on
    one side disagree, that database is still being written to and any
    comparison across the upgrade is noise rather than a result — a failure you
    want to be able to tell apart from real data loss. Both validation runs
    did exactly this against a real CMS database — 73 tables and 701 rows in
    the first — and the digest came back byte-identical each time.

### The downtime

The cluster is destroyed and rebuilt. This is not a live migration and there
is no overlap: from `apprafter destroy` until the workloads are back up on the
new machine, the cluster does not exist and nothing it served is reachable.

Two full runs of the automated walk on real Hetzner took **29m35s** and
**21m9s** wall-clock. Read that as an order of magnitude, not a budget — it
covers the whole walk, including standing the original cluster up and seeding
it, which you are not doing. Your outage is the `destroy` → workloads-`Ready`
span inside it.

What dominates that span is the rebuild: provisioning the machine and running
the full bootstrap — Cilium, Argo CD, then the platform stack syncing its
components — not the data load, which was small at this size and scales with
your data rather than with anything AppRafter controls. So plan for tens of
minutes rather than seconds, and measure your own before you commit to a
maintenance window. The walk's phase banners carry an elapsed clock, which
makes the Phase 5 → Phase 7 span the number to read off a run of your own.

### Two defects to plan around

Both were found by the validation runs. They are current behaviour with a
workaround, not planned work.

#### `apprafter target show` keeps reporting the old machine

After `restore --reprovision --server-type <big>`, the live machine and the
recorded state are both the new SKU — but the target's saved *preference* is
untouched, because the backfill that writes it only fills the slot when it is
empty and yours is not. So `apprafter target show <name>` prints the old SKU
indefinitely.

It is harmless immediately: recorded state outranks the target preference in
the resolution chain (flag → manifest → recorded state → target preference →
`APPRAFTER_SERVER_TYPE`), so the stale value is shadowed for as long as that
state exists. It stops being harmless the moment the state is cleared — which
is precisely what `apprafter destroy` does. A **second** upgrade run without an
explicit `--server-type` would resolve the stale preference and rebuild the
machine you were trying to leave.

The correction has to happen in the window between `destroy` and `restore` —
which is why it is a step of its own in both sequences above. `apprafter
target machine` refuses on a target that already runs a provisioned cluster,
so once the restore has finished there is no supported command left to fix it.
If you have already completed an upgrade without that step, either pass
`--server-type` explicitly on every future rebuild, or set the preference in
the destroy-to-restore window of the next one.

#### `backup check`, `prune` and `unlock` fail once the cluster is gone

All three resolve a kubeconfig as their first statement (`run_backup_check`,
`run_backup_prune` and `run_backup_unlock` in
`cli/platform-cli/src/commands/backup.rs`), and `apprafter destroy` clears the
provider section of the state file. So between the destroy and the restore all
three exit with:

```text
state has no hetzner_cloud section; run `apprafter apply` first
```

None of them needs the cluster or the `PlatformStack` when `--repo` and
`--credential-file` are both given — the repository URL and the operator's
credentials are the entire input. The failure therefore lands exactly where it
hurts most: verifying an off-site backup *before* restoring from it, with no
cluster in the world to ask.

Two ways around it:

- **Check before you destroy.** Step 1 of the S3 sequence above does this
  deliberately, and it is the better habit regardless — a repository you
  verify while the cluster is still up is one you can still fall back to.
- **Use stock restic**, which needs no AppRafter and no cluster at all (the
  repository is a plain restic repo — see [Assumptions and
  portability](#assumptions-and-portability)):

    ```sh
    restic -r s3:<endpoint>/<bucket>/<prefix> snapshots
    restic -r s3:<endpoint>/<bucket>/<prefix> check
    ```

### The executable version

`e2e/substrate-upgrade-hetzner.sh` is this procedure written as a script, and
it is the specification the section above describes: it provisions a `cx23`,
deploys a real application with a `needs.pg` claim and a `secret:` reference,
fingerprints the database, upgrades to a `cx33` through the sequence above,
and asserts every item in *What to verify afterwards*. One switch
(`APPRAFTER_SUBSTRATE_BACKEND=local|s3`) selects the storage backend, so both
sequences are the same script. Its Phase 7 is where the verification list came
from.

Unlike the local-cluster harnesses cited elsewhere on this site, it runs
against **real Hetzner** and spends real money — two short-lived machines, on
the order of a couple of euro cents — and it calls `apprafter destroy`, which
empties the whole project its token belongs to. Point it at a project you are
willing to lose.

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
[ADR 0050](../adr/0050-backup-restore.md).

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

```text
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
> drop its own lock — [park the in-cluster check](#parking-the-in-cluster-check)
> and run `apprafter backup check` operator-side instead. Verify your provider's
> behavior — the Verify checklist below has a step for confirming it.

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
> - **A further reduction, also verified:** you may additionally deny the cluster key
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
>     { "Sid": "ClusterDenyReadData", "Effect": "Deny",           // optional
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

```text
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

```text
apprafter backup check [--repo s3:…] [--credential-file <dotenv>] [--read-data]
```

`check` runs `restic check` against the repository — the same verification that
the in-cluster **`apprafter-backup-check` CronJob** runs weekly (default
`0 6 * * 0`). By default it verifies structure only; `--read-data` re-downloads
and re-hashes **every** pack for a deep verify (slower, bandwidth-heavy). Run the
operator-side `check` when your provider can't express the scoped-delete policy
and you have [parked the in-cluster check](#parking-the-in-cluster-check), or any
time you want a manual verification with full credentials.

#### Parking the in-cluster check

There is **no off switch** for the weekly check today. `--check-cron` takes a
five-field cron and nothing else: whatever you pass is written verbatim to
`PlatformStack.spec.backup.checkSchedule` (`spec.backup.checkSchedule` is an
unconstrained string in the CRD) and rendered verbatim into the CronJob's
`schedule:` field
(`platform-stack/cue/render_tool.cue:538`). A word like `off` is not handled
anywhere in the CLI — it reaches the apiserver and is rejected:

```console
$ kubectl apply -f apprafter-backup-check.yaml
The CronJob "apprafter-backup-check" is invalid: spec.schedule: Invalid value: "off": expected exactly 5 fields, found 1: [off]
```

which fails the platform-stack sync and leaves the whole backup component
`OutOfSync` — the opposite of what you wanted.

Two things that do work:

- **Give it a schedule that never comes.** The 31st of February is a valid
  five-field cron and no date ever matches it, so the CronJob exists, is
  Synced, and never fires:

    ```sh
    apprafter backup enable --bucket s3:… --credential apprafter-backup-s3 \
                            --check-cron "0 6 31 2 *" --i-have-saved-credentials
    ```

    (verified against a Kubernetes apiserver: `schedule: "0 6 31 2 *"` is
    accepted and stored as written.)

- **Or edit `PlatformStack.spec.backup.checkSchedule` in your infra repo** to
  the same value, if the backup block is git-managed.

Do **not** reach for `kubectl patch cronjob apprafter-backup-check
--patch '{"spec":{"suspend":true}}'`. The CronJob is chart-owned and the
platform components sync with `selfHeal: true`
(`platform-stack/cue/platform.cue:112`), so Argo CD reverts the suspend on
its next reconcile.

Whichever you choose, run `apprafter backup check` operator-side on your own
cadence — parking the in-cluster check means nothing verifies the repository
until you do.

> **Where check failures surface.** The weekly in-cluster check runs `restic`
> directly (the runner binary has no check-only mode), so a failed check shows
> up as a **`Failed` Job** in `apprafter-system` (kept per
> `failedJobsHistoryLimit`) — it does **not** write the `apprafter-backup-status`
> ConfigMap (only the backup runner does). So `apprafter backup status` reflects
> the last *backup* outcome; for the last *check* outcome, look at the
> `apprafter-backup-check` Job history (`kubectl get jobs -n apprafter-system`)
> or run `apprafter backup check` yourself.

```text
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

```text
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
     (The same mode with `--server-type` also performs a *planned* move onto a
     bigger machine — [Move a cluster onto a bigger
     machine](#substrate-upgrade).)
   - neither flag (restore into a running, already-bootstrapped target; select
     it with `--target <name>`) — the default path.
   - `--data-only` — reload only native data into an already-configured target.
4. Restore **auto-detects the backup format** — monolithic (the default, one
   snapshot per run) vs sequential (a versioned snapshot-set) — by reading the
   manifest version, so you don't specify the format. `latest` resolves to the
   freshest run of **either** format. Both staging formats restore identically
   from the operator's side; the only difference is on the write path.

The restore ordering, the gating and replay-order invariants, and the secret re-sealing behavior
are identical to the local-pull restore documented above — the only difference
is the repository lives in S3 and the credentials come from the operator, not
the target.

### Verify checklist (operator-runnable)

Two checks are worth running before you trust the off-site backup, mapping to
the design's Verify items:

- **Confirm your provider honors a prefix-scoped delete.** With the
  `enforce: operator` scoped credential, actively **test** that the cluster
  credential can delete an object under `locks/*` but is **refused** deleting an
  object under `data/` (or `snapshots/`). If the deny doesn't hold, your
  provider can't express the append-only guarantee — fall back to
  `enforce: cluster` with provider object-lock, or to the no-delete variant
  with the in-cluster check
  [parked](#parking-the-in-cluster-check).
- **A minimal end-to-end.** `apprafter backup enable` → wait for (or
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
- `e2e/substrate-upgrade-hetzner.sh` — the planned-upgrade walk on real
  Hetzner: `cx23` → `cx33` with the workload and every byte of its Postgres
  intact, on either storage backend. See [Move a cluster onto a bigger
  machine](#substrate-upgrade).
- [Choosing the machine](choosing-the-machine.md#changing-the-machine) — why a
  machine change is a rebuild, how far `apprafter destroy` reaches, and the
  two-project alternative that keeps the old cluster serving.
- [ADR 0050](../adr/0050-backup-restore.md)
  for the full design and the rejected alternatives.
