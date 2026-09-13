---
description: "What a prune actually deletes, what the weekly integrity Job runs and where its result appears, and the three ceilings on the credential the cluster holds."
---

# How retention and the integrity check work

What `apprafter backup prune` deletes, what the weekly check Job does, and what
the cluster's copy of the backup credential can reach. The recipe is [Backup
retention, integrity and
credentials](../operator-guide/backup-maintenance.md); running any of those
commands needs none of this.

Read it when a prune removed more than the keep numbers led you to expect, when
the weekly check went red and `apprafter backup status` will not say why, or
when you are deciding how much delete power to hand the cluster's S3 key.

The decisions behind all three — a restic repository written by the platform's
own runner rather than a third-party backup operator, retention computed in code
we own, and a two-tier credential model — are
[ADR 0050](../adr/0050-backup-restore.md).

## What retention counts

The unit is a **run**, not a snapshot.

Every snapshot a scheduled run writes carries the same restic tag,
`<cluster-uid>-<timestamp>`, where the cluster uid is the cluster's own
`kube-system` namespace UID and the timestamp is the run's start in RFC 3339. In
the default `monolithic` staging mode a run is one snapshot, taken over a staging
tree that holds the dumps, the serialized CRs, the captured secrets and
`manifest.json` together. In `sequential` mode a run is *several* snapshots
sharing that one tag: one per claim, and then a final **commit snapshot** —
written last — that carries the CRs, the secrets and `manifest.json`.

Because the uid leads the tag, two clusters writing to one repository have
disjoint tag namespaces — see [Which snapshots are
yours](#which-snapshots-are-yours) below, which is the property the whole of
retention rests on.

A local run narrowed to a subset of namespaces appends them to the tag, so its
snapshots do not group with a whole-cluster run's — which is the intended
behaviour, since they do not restore together either.

That last snapshot is the run's **representative**. It is what retention
actually applies its keep numbers to, and a run that has one is a run that
completed. Identification is by staging path: a snapshot alone in its tag group
is a monolithic run and is its own representative; in a multi-snapshot group the
representative is the one staged under `commit`.

Two consequences follow, and they are the two that surprise people:

- **A run is kept or dropped whole.** When a representative is not kept, every
  snapshot sharing its tag is forgotten with it. A sequential run can therefore
  never rotate apart into a surviving manifest with missing claim data, or into
  claim snapshots no manifest refers to.
- **An interrupted run is swept regardless of policy.** A tag group with *no*
  representative — a sequential run that died before it wrote its commit point —
  is an orphan, and all of its snapshots are forgotten on the next prune no
  matter how generous the keep numbers are. This is deliberate: a run with no
  manifest is one a restore ignores anyway.

## Why the keep numbers are not restic's keep flags

`--keep-daily` and its siblings are never passed to restic. The platform reads
`restic snapshots --json`, derives the tag, time and representative flag for
each snapshot, computes the set of ids to remove, and then calls
`restic forget <ids…> --prune` with that explicit set. If the set comes out
empty, the `forget` call is skipped entirely.

The keep computation runs three passes over the representatives, each walking
them newest-first:

- **keep-daily** keeps the newest representative in each distinct calendar day,
  for up to N distinct days;
- **keep-weekly** does the same per distinct **ISO** week — so a run on
  31 December and one on 1 January can land in the same bucket, which is the
  point of using ISO weeks rather than day arithmetic;
- **keep-monthly** does the same per distinct calendar month.

A representative kept by *any* of the three is kept — the buckets are a union,
not a sequence. The defaults are **7 / 4 / 6**. Note that the buckets count
*distinct periods*, not runs: two runs on the same day consume one day of the
daily budget, and the older of the two is kept only if some other bucket rescues
it.

The runner pins a stable restic host rather than letting the ephemeral pod name
become one: `spec.backup.clusterName` when the cluster has been named, and the
fixed `apprafter-backup` when it has not. It changes nothing about retention —
grouping is by tag alone — but it is what the CLUSTER column of a listing shows.
A local `apprafter backup create` pulled to your own machine passes no host and
uses the machine's own.

## Which snapshots are yours

One restic repository can hold more than one cluster's snapshots, and doing that
on purpose is a supported shape: [moving to a bigger
machine](../operator-guide/moving-to-a-bigger-machine.md) has the old and the new
cluster alive at the same time, both writing to the same bucket.

Attribution is by the **`kube-system` namespace UID**, which leads every run tag.
It is the de-facto standard cluster identifier: it exists on every cluster, it
needs no state the platform has to generate and keep, and a restored copy of a
cluster is a different Kubernetes cluster with a different UID — so a clone
cannot inherit it. The nightly runner reads it through its own ServiceAccount
(`get` on the single `kube-system` object; nothing else in the runner's RBAC
touches namespaces), and a run whose identity cannot be read fails rather than
writing an unattributable snapshot.

`spec.backup.clusterName` is the other half, and it is a **label, not an
identity**. It is what the listing shows and what makes a repository readable
when a UUID alone would not be. Because it lives in `spec.backup`, a restore
replays it: a restored cluster inherits the source's name and its snapshots are
listed under it. That is cosmetic — attribution still follows the UID, which the
clone has its own of — and the restore summary says so and points at
`apprafter backup set cluster-name <name>`.

Three places narrow a repository-wide listing to this cluster before it decides
anything:

- **`apprafter backup list`** shows this cluster's snapshots and reports how many
  it withheld; `--all-clusters` shows the rest.
- **`restore` without `--snapshot`** resolves `latest` inside this cluster's
  snapshots. When the target has none of its own and the repository holds more
  than one cluster, it refuses rather than guessing; when the repository holds
  exactly one cluster it resolves normally, which is the ordinary
  disaster-recovery case. An explicit `--snapshot <id>` is always honoured.
- **prune** plans only over this cluster's runs, so a clone can never delete the
  source's history.

### Snapshots older than cluster identity

Snapshots written before the tag carried a UID have no identity in them. They
are treated as **this cluster's**: they take part in `latest` selection and in
prune, so a repository does not accumulate history nothing can ever reclaim. In
a repository two clusters already shared, that means the other cluster's old
snapshots are attributed to whoever asks first. `apprafter backup list` marks
those rows `(legacy)` so the assumption is visible rather than silent.

### Why a bucket lifecycle rule is not retention

A restic repository is content-addressed: many snapshots' data lives in shared
pack objects, and the newest snapshot routinely references packs written long
ago. A bucket lifecycle rule deletes by *object age*, so it will delete packs
that are still referenced and leave the repository unrestorable. Only the
reference-graph walk that `forget --prune` performs can tell a genuinely
unreferenced pack from an old but live one.
[ADR 0050](../adr/0050-backup-restore.md) records this as a hard constraint
rather than a preference.

## Who runs the prune

`spec.backup.retention.enforce` decides, and it reaches the runner as the
environment variable `APPRAFTER_BACKUP_ENFORCE`. Only the exact value `cluster`
turns in-Job pruning on.

Under the default `operator`, the nightly Job takes the backup and stops. The
repository grows until you run `apprafter backup prune` yourself. Under
`cluster`, the prune runs inside the Job immediately after the backup, with the
keep counts arriving as `APPRAFTER_BACKUP_KEEP_DAILY` / `_WEEKLY` / `_MONTHLY`
— threaded through by the chart only when they are configured, and defaulting to
7/4/6 in the runner otherwise. Unlike the status ConfigMap write and the failure
webhook, which are both best-effort, **a prune failure fails the run**: a
repository whose retention is silently not being enforced is a real fault.

The operator-side `apprafter backup prune` resolves its policy as CLI flags →
`spec.backup.retention` → 7/4/6, and its repository as `--repo` →
`spec.backup.bucket`. On success it stamps `apprafter.io/last-prune` on the
`PlatformStack` with the current time; that annotation is exactly what
`apprafter backup status` prints as `Last prune`.

`apprafter backup prune` always needs a reachable cluster, and no combination of
flags takes that away. It used to go fully offline when `--repo` and all three
`--keep-*` were supplied; it cannot any more, because it also has to read the
cluster's identity to know whose snapshots it may forget, and no flag can stand
in for that. `apprafter backup check` and `apprafter backup unlock` are
unaffected — they read the CR only for the repository URL, so `--repo` alone
makes either of them work with no cluster at all, which matters because verifying
a repository before restoring from it tends to happen when the cluster is gone.

## What the weekly check runs

The chart emits a second CronJob, `apprafter-backup-check`, in
`apprafter-system`. It runs on `checkSchedule` (default `0 6 * * 0`, Sundays at
06:00) in `spec.backup.timeZone` when one is set, with
`concurrencyPolicy: Forbid`, keeping three succeeded and three failed Jobs.
An **empty** `checkSchedule` omits the CronJob from the render entirely — it is
not created and suspended, it is not created.

It uses the runner image but not the runner binary, which has no check-only
mode. What it executes is two restic commands in a shell:

```text
restic -r "$APPRAFTER_BACKUP_REPO" unlock
restic -r "$APPRAFTER_BACKUP_REPO" check --read-data-subset=10%
```

`restic unlock` is invoked without `--remove-all`, so it removes stale locks
only and never a live one held by a concurrent run.

The check itself has three depths, and the platform default is the middle one:

| `spec.backup` | What runs | What it reads |
| --- | --- | --- |
| `checkReadDataSubset: "10%"` (default) | `check --read-data-subset=10%` | structure, plus a random tenth of the packs |
| `checkReadData: true` | `check --read-data` | structure, plus every pack |
| both empty / false | `check` | structure and metadata only |

The reasoning for the default is worth stating, because before 0.2.67 it was
"structure only". A structural check verifies that every reference resolves and
every index agrees — without reading a single byte of the data it is
certifying. Bit-rot in a pack file is therefore invisible to it, permanently. A
full read every week finds that, and bills a repository-sized egress each time
for a fault that is rare.

Ten percent of the packs, chosen at random each week, finds a rotted pack in
five weeks on average, covers the whole repository in ten, and costs a tenth of
the bandwidth. `checkReadData: true` still means "all of it, every week" and
wins when both are set; `apprafter backup set check-depth structure` returns to
the metadata-only check.

The nightly backup Job opens the same way — an `unlock` first, whose failure is
logged and does not fail the run — so a lock left behind by a crashed run does
not wedge the next night's backup.

## Where the check result shows up

`apprafter backup status` lists the Jobs in `apprafter-system` whose names begin
with `apprafter-backup`, splits them into backup Jobs and check Jobs, and prints
the most recent of each as `Succeeded`, `Running`, `Failed` or `Unknown`. A red
weekly check is therefore visible on the `Last check Job:` line.

What that line cannot give you is the reason, and this is the part worth
knowing: **the check Job never writes the runner's status ConfigMap.** Only the
backup runner writes `apprafter-backup-status`, using server-side apply under
the field manager `apprafter-backup` and merging its fields so that
`lastSuccess` and `lastFailure` both survive across alternating runs (a
successful run additionally clears `lastError`, so a stale message never sits
beside a fresh success). Everything under `Runner status:` — including
`lastError` — is about a *backup*, never about a check. For a failed check, read
the Job's pod log, or re-run `apprafter backup check` yourself with full
credentials.

The status ConfigMap is deliberately not chart-owned, so Argo CD does not
reconcile the runner's self-report away.

## What the in-cluster credential can and cannot do

There are three ceilings on the backup Jobs, and they are enforced in different
places.

**The S3 ceiling.** Chart values never carry secret material — only
`credentialRef.name`, naming a Secret in `apprafter-system`. Both CronJobs pull
that Secret through explicit per-key `secretKeyRef` entries and map the neutral
names the platform stores to the ones restic reads: `RESTIC_PASSWORD` passes
through, `S3_ACCESS_KEY_ID` becomes `AWS_ACCESS_KEY_ID`,
`S3_SECRET_ACCESS_KEY` becomes `AWS_SECRET_ACCESS_KEY`, and `S3_REGION` becomes
`AWS_DEFAULT_REGION` and is marked optional, because many S3-compatible stores
do not need one. Everything beyond that is the bucket policy's job. Both Jobs open with
`restic unlock`, and restic takes and releases its own lock while it works,
which is what the scoped policy's delete under `locks/` is for.

One thing the S3 ceiling does not bound: the pod holds `RESTIC_PASSWORD`, and
that passphrase is what the repository's encryption rests on. A scoped
credential stops the cluster *erasing* history; it does not stop it *reading*
history.

**The Kubernetes ceiling.** The Jobs run as the ServiceAccount
`apprafter-backup`, whose ClusterRole grants `get` and `list` on the AppRafter
CRs, Argo CD Applications, CNPG Clusters, SealedSecrets and core Secrets;
`create`, `get`, `patch` and `delete` on pods; `create` and `get` on
`pods/exec`; and on ConfigMaps an unrestricted `create` plus `get`/`update`/
`patch` narrowed by `resourceNames` to the single `apprafter-backup-status`
object. That is a deliberately powerful role — a backup legitimately reads
everything — with one line drawn explicitly: it holds **no write verb on
`platformstacks`**, only `get`/`list`. A compromised backup pod cannot move the
platform's upgrade target. The cluster-wide `pods/exec` is unavoidable rather
than chosen: Kubernetes RBAC cannot scope exec to the runner's own helper pods.

**The network ceiling.** A CiliumNetworkPolicy, `apprafter-backup-egress`,
selects pods labelled `apprafter.io/backup-runner: "true"` — which both
CronJobs' pods carry — and permits DNS to kube-dns, the `kube-apiserver` entity,
and the `world` entity on TCP 443 and nothing else. The S3 endpoint is a
template value and so is not known at render time, which is why the last rule is
`world:443` rather than a name. The policy is emitted only when `cilium.io/v2`
is served, so a cluster without Cilium still converges.

By contrast, the operator-side verbs hold their credentials only for the length
of a subprocess: `apprafter backup prune` / `check` / `unlock` read them from
`--credential-file` (a dotenv file) or the environment — `S3_ACCESS_KEY_ID`,
`S3_SECRET_ACCESS_KEY` and `RESTIC_PASSWORD` required, `S3_REGION` optional,
`AWS_*` spellings accepted as aliases with the canonical name winning a
conflict — and set them on the restic child process only. They are never placed
on argv, and never on the CLI's own process environment.

## See also

- [Backup retention, integrity and
  credentials](../operator-guide/backup-maintenance.md) — the commands, and the
  bucket policies that express the scoped model.
- [Back up a cluster](../operator-guide/backup-restore.md) — enabling the
  schedule these settings tune.
- [How a restore replays a backup](how-a-restore-works.md) — what the runs this
  page keeps are eventually for.
- [ADR 0050](../adr/0050-backup-restore.md) — the restic engine, the run-aware
  retention, and the two-tier credential model.
