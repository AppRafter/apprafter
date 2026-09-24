---
description: "What a prune actually deletes, who runs it and what happens when the cluster's key may not delete, what the weekly integrity Job runs and where its result appears, how long a backup or check run may take, and the three ceilings on the credential the cluster holds."
---

# How retention and the integrity check work

What a prune deletes and who runs it, what the weekly check Job does, and what
the cluster's copy of the backup credential can reach. The recipe is [Backup
retention, integrity and
credentials](../operator-guide/backup-maintenance.md); running any of those
commands needs none of this.

Read it when a prune removed more than the keep numbers led you to expect, when
`apprafter status` says retention is not enforced, when the weekly check went
red, when a backup Job was stopped by its deadline, when `apprafter status` says
backups are failing, or when you are deciding how much delete power to hand the
cluster's S3 key.

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
each snapshot, computes the set of ids to remove, and then removes exactly that
set, in the three steps [below](#forget-look-then-prune). If the set comes out
empty, nothing is run at all: no `forget`, no `prune`.

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

### Forget, look, then prune {#forget-look-then-prune}

Every prune — the weekly check Job's, the backup Job's under `enforce: cluster`,
and `apprafter backup prune` — removes the set in the same three steps:

1. `restic forget` of **one** snapshot of the set, then `restic snapshots` again.
   If that snapshot is still listed, nothing else is tried: the prune ends as
   *not permitted* when restic says the store refused the delete (S3's
   `AccessDenied`), and as a failure otherwise.
2. `restic forget` of the rest of the set, then `restic snapshots` again. A
   snapshot of the set still listed is a failure, and no prune runs.
3. `restic prune`, which removes the data no listed snapshot refers to any more.

It is not restic's own `forget <ids…> --prune`, for a reason measured with
restic 0.18.1: `forget` exits 0 when the store refused its deletes — it prints
`unable to remove snapshot/<id> from the repository` and goes on — and with
`--prune` it then prunes as if those snapshots were gone, counting the data only
they use as unused. Under a key that may not delete, that prune writes a new
index before it fails. Under a key that may delete packs but not snapshots, it
would delete data a snapshot still in the repository needs. A bare `forget` of
one snapshot under the same refusal changes nothing in the bucket: a restic
lock is written and removed, and every other object is left exactly as it was.
`restic prune` run on its own is safe whatever `forget` did before it, because
it counts as used everything the listed snapshots refer to.

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

Four places narrow a repository-wide listing to this cluster before it decides
anything:

- **`apprafter backup list`** shows this cluster's snapshots and reports how many
  it withheld; `--all-clusters` shows the rest, and names the cluster identities
  the repository holds.
- **`restore` without `--snapshot`** resolves `latest` inside this cluster's
  snapshots. When the target has none of its own and the repository holds more
  than one cluster, it refuses rather than guessing; when the repository holds
  exactly one cluster it resolves normally, which is the ordinary
  disaster-recovery case. An explicit `--snapshot <id>` is always honoured.
- **`apprafter backup show`** with no snapshot named resolves `latest` through
  that same rule, deliberately the same code. `show` is read-only, but it is
  what you read before choosing what to restore, so it must be looking at the
  same snapshot the restore would.
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

## Who runs the prune {#who-runs-the-prune}

`spec.backup.retention.enforce` decides, and it reaches both CronJobs as the
environment variable `APPRAFTER_BACKUP_ENFORCE`. It has three values:

| `enforce` | Who prunes | When |
| --- | --- | --- |
| `check` (the default) | the weekly check Job | after a check that passed, as far as the cluster's key may delete |
| `cluster` | the backup Job | after every backup |
| `operator` | nothing in the cluster; you, with `apprafter backup prune` | when you run it |

The keep counts reach both Jobs as `APPRAFTER_BACKUP_KEEP_DAILY` / `_WEEKLY` /
`_MONTHLY` — threaded through by the chart only when they are configured, and
defaulting to 7/4/6 in the runner otherwise.

**`check`.** The weekly check Job runs `restic check` and, only when it passed,
the prune; [what it runs](#what-the-weekly-check-runs) is below. A check that
does not pass never prunes: a repository whose integrity is in doubt is the last
one to delete from. How far the prune gets is up to the key the cluster holds.
A key that may delete prunes. The scoped key recommended for the cluster (Put,
Get and List on the repository, Delete only under `locks/`) may not: the store
refuses the first delete, the prune stops there, and **nothing is deleted** — the
bucket is left exactly as it was. The runner records the prune as
`not-permitted`, the check still counts as passed, and `apprafter status` and
`apprafter backup status` say that retention is not enforced, with the
repository's size and how much it grew since the week before
([whether retention is enforced](#whether-retention-is-enforced)). That is the
scoped key doing its job — a compromised cluster cannot erase history, so it
cannot prune it either — and the pruning then belongs where the full
credentials are: `apprafter backup prune`, run from your machine.

**`cluster`.** The prune runs inside the backup Job immediately after the
backup. Unlike the status ConfigMap write and the failure webhook, which are
both best-effort, **a prune failure fails the run**, and so does a key that may
not delete: this mode promises a prune after every backup. The backup itself is
taken first, and the failure names its snapshot.

**`operator`.** Nothing in the cluster deletes a snapshot. The repository grows
until you run `apprafter backup prune`, and `apprafter status` says so, with
the repository's size.

A `PlatformStack` that sets `enforce` keeps it. One that never set it runs the
platform's default, which was `operator` before platform-stack 0.2.80 and is
`check` from it on — so a cluster that never chose, and whose key may delete,
starts removing the snapshots beyond its keep policy at its first weekly check
after the upgrade. `apprafter backup set enforce operator` before the upgrade
keeps the old behaviour.

The operator-side `apprafter backup prune` resolves its policy as CLI flags →
`spec.backup.retention` → 7/4/6, and its repository as `--repo` →
`spec.backup.bucket`. On success it stamps `apprafter.io/last-prune` on the
`PlatformStack` with the current time; that annotation is what
`apprafter backup status` prints as `Last prune`, and what the retention
condition names as the last prune from outside the cluster. Its credentials
resolve from `--credential-file`, then the environment, then the cluster's own
Secret; with that last, scoped, one it ends the same way the Job's prune does —
nothing deleted — and says to pass `--credential-file` with the full
credentials.

`apprafter backup prune` needs an **identity**, and that is the one input it
will not infer. With a live cluster it reads the `kube-system` UID off the
kubeconfig. It used to go fully offline when `--repo` and all three `--keep-*`
were supplied and nothing else, which planned across every snapshot in the
bucket; that form is gone. The offline form that replaced it says whose history
it means: `--cluster-uid <uid>`, with `--repo` and the three `--keep-*`, runs
with no cluster at all. That is the real offline case — the cluster is gone, the
repository remains, and its snapshots should be reclaimable.

The claim is checked against the repository before anything is forgotten. A UID
that has never written there is refused, naming the identities that have, rather
than matching nothing and quietly forgetting the pre-identity snapshots instead.
A repository that holds *only* pre-identity snapshots is allowed and says so —
there is no identity in it to contradict. Nothing is stamped on `PlatformStack`
on that path, because there is no CR to stamp.

`apprafter backup check` and `apprafter backup unlock` never needed an identity —
they delete nothing. They read the CR only for the repository URL, so `--repo`
alone makes either of them work with no cluster at all, which matters because
verifying a repository before restoring from it tends to happen when the cluster
is gone.

## What the weekly check runs {#what-the-weekly-check-runs}

The chart emits a second CronJob, `apprafter-backup-check`, in
`apprafter-system`. It runs on `checkSchedule` (default `0 6 * * 0`, Sundays at
06:00) in `spec.backup.timeZone` when one is set, with
`concurrencyPolicy: Forbid`, keeping three succeeded and three failed Jobs.
An **empty** `checkSchedule` omits the CronJob from the render entirely — it is
not created and suspended, it is not created. Under `enforce: check` that also
means nothing in the cluster prunes, and the retention condition says so.

It runs the same runner binary as the backup, as `apprafter-backup check`, with
the same ServiceAccount, the same resources and the same restic settings. One
run is, in order:

1. `restic unlock`, whose failure is logged and does not fail the run;
2. `restic check` at the depth below;
3. under `enforce: check`, and only when the check passed, the prune
   ([forget, look, then prune](#forget-look-then-prune)), scoped to this
   cluster's own snapshots by its `kube-system` UID — the read it needs is the
   one the backup makes;
4. `restic stats --mode raw-data`: the repository's stored size, snapshots and
   blobs, whatever the mode. The blob count is what the memory of every restic
   command that loads the index grows with.

Each step is written to the runner's status ConfigMap as soon as it ends
([where the result shows up](#where-the-check-result-shows-up)). The Job fails
only when the check did not pass. A prune the key may not run, or one that
failed, is recorded and reported as retention, not as a failed check: the
repository's integrity was confirmed, and the Job says so.

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

## Where the check result shows up {#where-the-check-result-shows-up}

The check Job writes the same `apprafter-backup-status` ConfigMap the backup
runner writes, using server-side apply under the field manager
`apprafter-backup` and merging its fields with what is there, so a check's
record and a backup's never overwrite each other:

| Key | What it holds |
| --- | --- |
| `lastCheck`, `lastCheckResult`, `lastCheckError` | when the last check ended, `passed` or `failed`, and restic's own output for a failure (empty after a pass) |
| `lastPrune`, `lastPruneResult`, `lastPruneDetail`, `lastPruneBy` | when the last prune in the cluster ended; `pruned`, `nothing-to-prune`, `not-permitted` or `failed`; what was forgotten, or why not; and whether the check Job (`check`) or a backup Job (`backup`) ran it |
| `repoStatsAt`, `repoBytes`, `repoSnapshots`, `repoBlobs`, `repoStatsRepo` | the repository's figures from the last check, and the repository they are of |
| `repoPrev…` | the same figures from the check before, while the repository is the same one |

`apprafter backup status` prints them as a `Repository` block — the last check
(with the first lines of restic's output when it failed), the last prune, the
size and the growth since the check before — followed by the operator's verdict
on retention. The most recent check Job is still on its `Last check Job:` line,
as `Succeeded`, `Failed`, or, for a Job that has not finished, what it is doing.

A check that did not pass also turns the `BackupHealthy` condition `False` with
reason `RepositoryCheckFailed`, quoting what the runner recorded
([when a backup cannot run](#when-a-backup-cannot-run)). A red check therefore
shows in `apprafter status`, in `apprafter backup status`, and in the check
pod's log, which has all of restic's output.

The status ConfigMap is deliberately not chart-owned, so Argo CD does not
reconcile the runner's self-report away.

## How long a run may take

Both CronJobs are `concurrencyPolicy: Forbid`: while one run is still active,
no scheduled run starts. The slots that fall meanwhile are neither queued nor
all lost: neither CronJob sets `startingDeadlineSeconds`, so when the active
run ends, the CronJob controller starts the most recent slot it missed at once,
and every earlier one is lost. A run that never ended would therefore stop
every later one without anything failing — the Job would sit at `Running`
indefinitely. So every run has an outer limit, and the steps inside
it that wait on something outside the runner have their own.

**The Job deadline.** Each Job template carries `activeDeadlineSeconds`, from
`spec.backup.activeDeadlineSeconds` for the backup and
`spec.backup.checkActiveDeadlineSeconds` for the check. Both default to six
hours and must be at least ten minutes. When the deadline passes, Kubernetes
fails the Job with reason `DeadlineExceeded` and stops its pod: it sends the
pod SIGTERM, and SIGKILL once the pod's grace period has passed. The backup
runner uses that grace period, 90 seconds, to record the run like any other
failure — `lastFailure`, a `lastError` that reads `run exceeded its deadline
of 6h …`, and the failure webhook — and to stop the work it was doing before
it exits. It deletes the helper pod it was working in, and it passes the
signal on to restic if a `restic backup` or the prune was running, because
Kubernetes signals only the runner. restic then removes its repository lock
and exits; it is given 15 seconds to do so. Without that, restic would be
killed with the runner and leave its lock behind for 30 minutes, until restic
counts it as stale. When the prune was running, that lock is exclusive, and a
check or a manual run in those 30 minutes would fail on it. Each of those
steps has its own bound, and together they fit inside the 90 seconds. The
check Job's runner does the same in its own 90 seconds: it passes the signal on
to restic and records the step it was stopped in — the check as not passed
(`lastCheckError` names `spec.backup.checkActiveDeadlineSeconds` and `apprafter
backup set check-deadline`), or the prune after it as failed, with the check's
pass left as it was.
`apprafter backup status` shows such a Job as `Failed: DeadlineExceeded`, and
scheduling resumes: the next slot runs on time or, if one fell while the
stopped run was active, the latest such slot starts at once. `apprafter backup
run` copies the same Job template, so a manual run has the same limit.

The deadline also sets how long each helper pod lives. The pod a single
`pg_dump`, `tar` or `pg_restore` runs in keeps itself alive for the deadline,
but never less than six hours, and every command still running in it ends
when that time is up. In a scheduled run the Job's deadline always comes
first, so the whole run and each claim's extraction within it are bounded by
the deadline, and raising it gives a single large dump more time too.
`apprafter backup create`, `apprafter export` and `apprafter restore` have no
Job and no deadline — the person running them stops them — so their helper
pods' time is the only limit on one dump or load. The six-hour floor is there
for them: a deadline lowered to suit a frequent schedule does not cut a
restore short. A command ended this way fails with a message that says its
helper pod's keep-alive ran out and names `apprafter backup set deadline` as
the way to allow longer, rather than with a bare exit code 137.

`apprafter backup create`, `apprafter export` and `apprafter restore` delete
the helper pods they created when they are interrupted with Ctrl-C or SIGTERM,
as the runner deletes its own when its Job stops it. A command creates a helper
pod that is not there — the create fails rather than touch one another run has
made in the meantime — and applies over one of the same spec that is. The
interrupt deletes only a pod the apiserver named in its answer to the command's
own create, with that pod's uid as the delete's precondition: a pod of the same
name that another run is using is left alone. So is one whose create the same
Ctrl-C cut off before the answer came, because a pod of that name found
afterwards may be another run's; the command prints the `kubectl delete pod`
line that removes it. It undoes nothing else, and it takes at most fifteen
seconds; a second Ctrl-C exits at once, without it. From the signal on, the
command starts nothing new either: no `kubectl`, no `restic`, and no further
restore step. A SIGTERM sent to the command's process alone — as `timeout`,
systemd or a cancelled CI job send it — does not stop the `kubectl` or `restic`
it is waiting on, so that one runs to its end, but nothing after it does: a
restore stopped before it scaled the applications down does not scale them down
afterwards. `apprafter restore --reprovision` handles the signal this way only
once its new cluster exists; while it is still provisioning, Ctrl-C ends it at
once. A helper pod that a command or run was killed before it could delete —
`kill -9`, a lost node, that second Ctrl-C — keeps running until its keep-alive
ends and then stays behind as `Completed` until something deletes it. The next
command or run that needs a helper pod of that name, the same step for the same
claim, deletes it, waits until it is gone, and creates its own. It does the
same with a leftover it cannot update in place because an older version built
it with a different spec. A pod still running with the same spec is used as it
is. Two runs that need the same helper pod at the same time —
`apprafter backup create` while the scheduled backup is dumping the same claim
— do not both finish. With the same spec they share the pod, and the first to
finish deletes it under the other. With different specs, as a CLI and a backup
runner of different versions build them while an upgrade is under way, the
second run replaces the first one's pod and both fail.

Pick the value against the schedule it applies to:

- **Shorter than the interval between two runs**, so a stuck run is stopped
  before the next one is due. Six hours leaves the nightly default's next run
  eighteen hours clear.
- **Longer than the slowest run you expect to succeed**, because the deadline
  stops a slow run exactly as it stops a stuck one. The first backup of a large
  data set is the one to size it for; the largest single claim in it is dumped
  within the same limit.

With a schedule more frequent than the deadline — hourly, under the default —
a stuck run still costs the runs that fall while it is active: stuck from 00:00
and stopped at 06:00, it loses the 01:00 to 05:00 runs, and the 06:00 run
starts only once the stuck Job has failed. Set the deadline below the interval
for such a schedule. A run that simply takes longer than the interval has
always delayed the next one — the slot it overlaps starts as soon as it ends;
that is `Forbid`, not the deadline.

**The two schedules bound each other.** `restic check`, and the `forget` and
`prune` after it, hold the repository's exclusive lock, and neither Job waits
for a lock (no `--retry-lock`): a check that starts while a backup is running
fails on the backup's lock, and a backup that starts while a check or its prune
is running fails on theirs. The check Job's time is the check's and, under
`enforce: check`, the prune's together. For the
same reason `apprafter backup run` starts nothing while a backup or check Job
has not finished (`apprafter::backup::job_active`). So on top of
its own interval, each deadline has a second ceiling:

- the backup's deadline stays below the time from a backup's start to the next
  check's start;
- the check's deadline stays below the time from a check's start to the next
  backup's start.

Under the default schedules — the backup at 03:00 every day, the check at
06:00 on Sundays — those gaps are three hours and twenty-one. The six-hour
backup default is longer than the first: a Sunday backup still running at
06:00, slow or stuck, makes that week's check fail on its lock (the check Job
retries for about ten minutes, then fails), and the next week's check runs as
usual. If Sunday backups take longer than three hours, move the check later —
`apprafter backup set check 12:00` — rather than living with a failed check.
The check's six hours end by Sunday noon, well clear of Monday's backup; when
you raise `checkActiveDeadlineSeconds` for `checkReadData: true` on a
repository that takes long to download, keep it under the twenty-one hours, or
a slow full-read check fails Monday's backup instead.

`apprafter backup set deadline 12h` and `apprafter backup set check-deadline
12h` write the two fields; the CLI never writes them otherwise, so a cluster
that has not set them runs on the chart's six hours.

**Inside a run.** The steps that wait on something outside the runner are
bounded too, far below the Job deadline. The first four fail the run with a
reason of its own, recorded in `lastError` and sent to the failure webhook:

| What the run waits on | Bound | What you see |
| --- | --- | --- |
| `pg_dump` taking its table locks | 5 minutes (`--lock-wait-timeout=300s`) | `pg dump of <namespace>/<claim> gave up: another session held a lock …`, followed by `pg_dump`'s own `LOCK TABLE` statement naming the tables |
| `pg_dump` reading the schema, before the first byte of the dump | 10 minutes | `pg dump of <namespace>/<claim> gave up: pg_dump wrote nothing for 10 minutes …`, naming the locks it covers |
| a helper pod becoming Ready | 5 minutes | `pod … did not reach Ready within 300s` |
| a helper pod whose container cannot start because a Secret it reads a credential from, or the key in it, is missing | 15 seconds | `helper pod <namespace>/<pod> cannot start its container: secret "…" not found (CreateContainerConfigError) …`, in the kubelet's own words |
| the failure webhook answering | 30 seconds | nothing: the notification is best-effort, and the run's outcome is already recorded |

Both `pg_dump` bounds cover the start of a dump, before it has read a row, and
both are about a lock held by another session. `pg_dump` first takes a shared
lock on every table it dumps; a session holding a conflicting lock — a
migration's `ALTER TABLE`, `VACUUM FULL`, `CLUSTER`, or a `LOCK TABLE` in a
transaction left open — makes it wait, and `--lock-wait-timeout` ends that
wait after five minutes. It covers plain and partitioned tables only. After
it, `pg_dump` reads the rest of the schema with no timeout of its own, and
those reads wait on a lock held on a view, a materialized view or a sequence:
a `REFRESH MATERIALIZED VIEW` still running or left in an open transaction, a
migration that ran `CREATE OR REPLACE VIEW` or `ALTER SEQUENCE` in a
transaction that has not ended. A custom-format dump writes nothing at all
until that schema read is done, so the runner gives the dump ten minutes to
write its first byte: five for the table locks, and five for a schema read
that takes seconds even at ten thousand tables. The five minutes for table
locks are per `LOCK TABLE` statement, and `pg_dump` starts a new statement
every 100 KB or so of table names. A database with thousands of tables needs
several, and table locks held on them one after another can then add up past
the ten minutes; the message for that bound says so. Once the dump is writing,
only the run's deadline limits it, so copying a large table is never cut
short. Both bounds apply to `apprafter backup create` and `apprafter export`
too. A dump that gave up, or that a run's deadline stopped, does not stay
behind on the database. Deleting its helper pod kills `pg_dump`, but a server
session waiting on a lock does not notice that its client has gone, and would
keep its shared table locks and a connection until that lock was released. So
the helper connects with `client_connection_check_interval` set to ten
seconds, and the session ends within seconds of the kill. A dump that gave up leaves no restorable snapshot behind: a `monolithic`
run fails before restic writes anything, and a `sequential` run never writes
the commit snapshot, so the claim snapshots it already wrote are ignored by
restore and removed by the next prune.

## When a backup cannot run {#when-a-backup-cannot-run}

Everything above is recorded by the runner, and a runner records only what
happens while it runs. Some failures come before it starts, or end it without
warning: a pod that no node has room for, a runner killed at its memory limit,
a pod the kubelet evicts, a Job stopped by its deadline before its pod was ever
placed. In each of those the runner's record is not updated and the failure
webhook does not fire, so `lastSuccess` goes on showing the last run that got
through.

So the operator also watches the backup from outside the runner. It reads the
`apprafter-backup` and `apprafter-backup-check` CronJobs, their Jobs and the
runner pods in `apprafter-system`, and keeps its verdict in the `BackupHealthy`
condition of `PlatformStack/default`. `apprafter status` and `apprafter
platform status` print it as a `Backups:` line, with the time the failure began
and what to run next. The condition moves when those objects change, not on
the operator's six-hour upstream check.

| Status | Reason | What happened |
| --- | --- | --- |
| `True` | `Succeeded` | The most recent finished backup succeeded, the most recent finished check passed (or none has run yet), and no unfinished run is in trouble. |
| `False` | `RunnerUnschedulable` | The scheduler has found no node for the runner's pod for more than ten minutes, most often for lack of memory. The message quotes the scheduler and says what the pod asks for. A Job the schedule started holds the schedule while it waits: see [the troubleshooting entry](../operator-guide/backup-restore.md#runner-unschedulable). |
| `False` | `RunnerNotStarted` | The pod has not started for ten minutes for another reason: its image cannot be pulled, a Secret it reads is missing, or the scheduler has not yet tried to place it. Or the Job has had no pod at all for ten minutes, because a quota, a LimitRange or an admission webhook refused it; the Job's `FailedCreate` events say which. |
| `False` | `RunnerOOMKilled` | An attempt was killed at the runner's memory limit. The Job retries, but the same data meets the same limit, so this is reported at once rather than after the last attempt. |
| `False` | `RunnerEvicted` | The kubelet evicted an attempt: memory pressure on the node, or the staging directory grown past its size limit. The message quotes the kubelet. |
| `False` | `RunnerFailed` | An attempt of a backup ran and failed: the runner exited non-zero, and the message quotes the error it recorded. Reported at once, while the Job retries, because the runner has already recorded the failure and posted its webhook; a retry that succeeds returns the condition to `True`. |
| `False` | `DeadlineExceeded` | The Job was stopped by its deadline before any attempt failed on its own. The message says what the runner recorded, or that it recorded nothing, and, for a pod that was never placed, what the scheduler said before the deadline. A Job whose attempts had already failed keeps the reason they had (`RunnerFailed`, `RunnerOOMKilled`, `RunnerEvicted` or `RepositoryCheckFailed`), and its message says the deadline ended it. |
| `False` | `BackoffLimitExceeded` | Every attempt of a backup failed. The message says how the last one ended and quotes the runner's `lastError` when it wrote one. |
| `False` | `RepositoryCheckFailed` | An attempt of the weekly check ran and failed: `restic check` did not pass, because it found the repository damaged or could not read it. Reported at once, while the Job retries. The message quotes what the runner recorded, `apprafter backup status` shows it under `last check`, the check pod's log has all of it, and `apprafter backup check` runs the same check from your machine. |
| `False` | `Failed` | The Job failed for another reason, which the message quotes. |
| `False` | `ScheduleSuspended` | The CronJob is suspended, so no scheduled backup starts. |
| `Unknown` | `NoRunYet` | No backup has finished yet. |
| `Unknown` | `ScheduleNotDeployed` | `spec.backup.enabled` is true, but the CronJob does not exist: the platform chart has not deployed it. |
| `Unknown` | `StateUnreadable` | The operator could not read the CronJobs, Jobs or pods. |

The condition is absent while backup is disabled. It is never `True` on no
evidence: a cluster whose objects could not be read, or whose first backup has
not finished, is `Unknown`.

Four rules keep it from raising false alarms, and from going quiet:

- A pod that is waiting to be placed, or is placed with its container not yet
  started, counts only after ten minutes. Room is often on its way: a database
  instance being deleted can take three minutes to stop, the kubelet keeps a
  memory-pressure taint for five minutes after the pressure ends, and a node
  that restarts is not ready for a few minutes. A pod the scheduler is making
  room for by preemption does not count at all. The ten minutes are for the
  pod the Job is waiting on, never for an attempt that has already failed: a
  runner killed at its memory limit or evicted counts at once, and stays the
  verdict while the Job's next pod waits for room or starts.
- An attempt that runs and fails turns the condition `False` at once. The
  runner has already recorded the error and posted its failure webhook, and
  the Job's own ending can be far off: seven attempts by default, and an hour
  each for a check that reads every pack. A retry that succeeds completes the
  Job and returns the condition to `True`. A pod stopped from outside, by a
  node drain or a deletion, is not an attempt that failed.
- The newest finished run decides, whether the schedule started it or
  `apprafter backup run` did, so a later successful run returns the condition
  to `True`. An unfinished run in trouble counts before any finished one,
  because a scheduled run that cannot start holds back every later one. A
  failed check turns the condition `False` even while backups succeed. When
  both fail, the backup's failure is the reason, and the message ends by naming
  the check's (`Also failing: repository check Job …`).
- While the condition stays `False`, its transition time stays where the
  failure began, even when the cause changes: a pod that could not be placed,
  and then the deadline that stopped its Job.

Argo CD does not show this verdict on the platform's tile, on purpose.
`PlatformStack/default` is created by `apprafter cluster-bootstrap`, not by an
Argo CD Application, so no Argo CD health check ever evaluates it. Moving the
verdict onto a resource the platform Application does manage would cost more
than it shows. That Application syncs in waves, and in every wave but the
last, Argo CD waits for each resource to become healthy before it applies the
next wave: a `Degraded` resource fails the sync, and a `Progressing` or
`Suspended` one holds it. The backup CronJob is not in the last wave, so a
failing backup would stop every sync of the platform, the upgrade that might
carry the fix included. A resource in the last wave would not stop a sync, but
it would turn the whole platform Application `Degraded`, and the `Ready`
condition of `PlatformStack/default` follows that health: a backup problem
would read as a platform that is not ready. So the verdict stays on the
`PlatformStack`, where `apprafter status` reads it.

## Whether retention is enforced {#whether-retention-is-enforced}

Whether the backups run is one question; whether anything prunes the
repository is another, and it gets a condition of its own, `BackupRetention`,
beside `BackupHealthy` on `PlatformStack/default`. Folded into
`BackupHealthy`, the recommended scoped key — which can never prune — would be
a permanent failure, and a real one would hide behind it. `apprafter status`
prints it under the `Backups:` line as `Retention:`, and
`apprafter backup status` after its `Repository` block.

It is built from the runner's record (the table
[above](#where-the-check-result-shows-up)): the last check, the prune recorded
after it, and the repository's figures, which every message that has them
carries with the growth since the check before.

| Status | Reason | What happened |
| --- | --- | --- |
| `True` | `Pruned` | The prune after the latest check (or backup, under `enforce: cluster`) forgot the snapshots past the keep policy and removed the data only they used. |
| `True` | `NothingToPrune` | The prune ran, and every run is inside the keep policy. |
| `False` | `PruneNotPermitted` | The cluster's S3 key may not delete: the prune stopped at the first refused delete and nothing was deleted. Prune from outside the cluster with the full credentials. |
| `False` | `PruneFailed` | The prune failed; the message quotes why. |
| `False` | `CheckFailed` | The latest check did not pass, and a check that does not pass never prunes. `BackupHealthy` says why. |
| `False` | `CheckOff` | `enforce: check` with the weekly check turned off: nothing in the cluster prunes. |
| `False` | `EnforcedOutsideCluster` | `enforce: operator`: nothing in the cluster prunes, by choice. The message names the last `apprafter backup prune` against this cluster. |
| `Unknown` | `NoCheckYet` | No check has recorded a result yet. |
| `Unknown` | `NoPruneYet` | The latest check passed and no prune after it is recorded yet, or, under `enforce: cluster`, no backup has recorded one. |
| `Unknown` | `RecordUnreadable` | The operator could not read the runner's record. |

The condition is absent while backup is disabled. None of its `False` reasons
touches `BackupHealthy`: a scoped key that cannot prune is not a failing backup.
While it stays `False`, its transition time stays where retention stopped being
enforced, whatever the reason becomes.

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
which is what the scoped policy's delete under `locks/` is for. Under that
policy the check Job's prune asks the store for one more delete — of one
snapshot — is refused, and stops; nothing else is deleted or written.

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

**Credentials in helper pods.** A helper pod never carries a password in its
own spec. The PostgreSQL helper reads `PGPASSWORD` from the `pass` key of the
claim's connection Secret, and the JetStream helper reads `NATS_USER` and
`NATS_PASSWORD` from the `user` and `password` keys of the namespace's
`nats-mgr-<namespace>` Secret, each through a `secretKeyRef`: the kubelet
resolves them when it starts the container, and the Pod object carries only the
Secret's name and key. So `get pods` — a right commonly granted more widely
than `get secrets` — shows no credential, while the pod runs or after. Each of
those Secrets already lives in the namespace its helper runs in — a PostgreSQL
helper runs in its claim's namespace, a JetStream helper in the one NATS runs
in — so a run creates no Secret of its own, and the ClusterRole above needs no
write verb on Secrets. The same holds for the helpers of
`apprafter backup create`, `apprafter export` and `apprafter restore`. The step
that extracts the data never reads the password — the helper's container
resolves it — and a restore reads a claim's connection Secret only to check
that it has every key it needs. The password does pass through a backup in one
other place: the secret capture lists every Secret in each namespace that holds
a SealedSecret, and a Secret list returns each Secret's data, before it keeps
only the Secrets that have a SealedSecret behind them. A claim's connection
Secret in such a namespace is therefore read there and then dropped; it is
never written to the snapshot. A Secret or key that is missing leaves the
container unable to start; the run stops on that after fifteen seconds with the
kubelet's own words (the table above), rather than waiting five minutes for a
pod that cannot become Ready.

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
