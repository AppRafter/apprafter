---
description: "Keeping a backup repository trustworthy: how long snapshots are kept, the integrity check and its locks, and how narrow the in-cluster credentials can be."
---

# Backup retention, integrity and credentials

The three things that decide whether a repository is still worth anything a
year from now. None of them is a first-read: [Back up a
cluster](backup-restore.md) gets you a working backup with defaults that are
already sensible, and this page is what you come back to when you want to
change one of them, or when a check has told you something.

What each of these settings does once it is in the cluster — what a prune
deletes, what the weekly Job runs, what the credential reaches — is [How
retention and the integrity check
work](../how-it-works/backup-retention-and-checks.md).

## Retention and prune

Retention keeps whole backup **runs**, not individual snapshots, by keep-daily /
keep-weekly / keep-monthly counts (defaults **7 / 4 / 6**) — [what a run is, and
how the three counts
combine](../how-it-works/backup-retention-and-checks.md#what-retention-counts).

By default (`enforce: check`) the weekly check Job prunes, after a check that
passed, as far as the cluster's S3 key may delete. With a key that may delete,
that is all there is to it. With the scoped key [recommended
below](#who-prunes-and-what-the-clusters-key-may-delete), the prune is refused
at its first delete and **nothing is deleted**; `apprafter status` and
`apprafter backup status` then say `Retention: NOT ENFORCED — PruneNotPermitted`,
with the repository's size and its growth since the week before. That is the
signal to prune from outside the cluster, with full credentials:

```text
apprafter backup prune [--repo s3:…] \
                       [--credential-file <dotenv>] \
                       [--keep-daily N] [--keep-weekly N] [--keep-monthly N] \
                       [--timezone <zone>] [--cluster-uid <uid>]
```

`--repo` defaults to `PlatformStack.spec.backup.bucket`; the keep-* flags
override the configured `spec.backup.retention` (else the 7/4/6 defaults), and
`--timezone` the zone their days, weeks and months are counted in, which is
`spec.backup.timeZone`, the zone the backup schedules run in (UTC when none is
set).
Credentials resolve from `--credential-file`, then the environment, then the
credential Secret the cluster already holds (`spec.backup.credentialRef`). With
a key that may delete in that Secret, the command needs no credential flags at
all; with the scoped one, it stops at the first refused delete, deletes
nothing, and exits non-zero saying so — pass `--credential-file` with the full
credentials. On success `prune` stamps `apprafter.io/last-prune` on
`PlatformStack`, which `backup status` shows and the retention verdict names —
but only when it pruned that cluster's own history: its configured repository,
as its own identity. A prune of another repository, or of another
`--cluster-uid`, says it stamps nothing.
Run it on your own cadence (e.g. monthly) — restic dedup makes growth
sub-linear, so retention is a rare, deliberate operation, not a per-run one;
the size and growth `backup status` prints are what to judge the cadence by.

`prune` needs an **identity**. It forgets by explicit snapshot id, one
repository can hold more than one cluster's snapshots, and something has to say
which are yours — so it plans over your cluster's runs only, and a prune here
can never delete another cluster's history
([how](../how-it-works/backup-retention-and-checks.md#which-snapshots-are-yours)).
Normally the identity is the cluster's own, read from your kubeconfig, and you
never think about it.

#### Pruning a repository whose cluster is gone

When the cluster no longer exists and only the repository is left, name the
identity yourself:

```sh
apprafter backup prune --repo s3:<endpoint>/<bucket>/<prefix> \
                       --credential-file ./operator-s3.env \
                       --keep-daily 7 --keep-weekly 4 --keep-monthly 6 \
                       --timezone Europe/Berlin \
                       --cluster-uid <kube-system-uid>
```

With `--repo`, all three `--keep-*`, `--timezone` and the operator's
credentials, that runs with no cluster at all. `--timezone` is the zone the
destroyed cluster's backup schedules ran in, or `UTC` if it named none; without
it the command refuses rather than assume one. A prune counted in another zone
keeps different runs than the cluster's own prune did
([why the zone matters](../how-it-works/backup-retention-and-checks.md#why-the-keep-numbers-are-not-restics-keep-flags)).
`--cluster-uid` is the destroyed cluster's `kube-system`
namespace UID, and passing it is a claim about **whose** history may be deleted
— `apprafter backup list --repo <repo> --all-clusters` prints the identities a
repository holds, under the listing. The claim is checked before anything is
forgotten: a UID that has never written to this repository is refused, naming
the ones that have. Nothing is stamped on `PlatformStack` on this path, because
there is no `PlatformStack` left to stamp.

> **Do not use an S3 bucket lifecycle rule for this.** A "delete objects older
> than N days" rule deletes by *object age*, and a restic repository routinely
> references physically old pack objects from its newest snapshot — the rule
> will delete still-referenced packs and leave the repository unrestorable.
> Retention must go through `apprafter backup prune`
> ([why](../how-it-works/backup-retention-and-checks.md#why-a-bucket-lifecycle-rule-is-not-retention)).

## Integrity checks and locks

```text
apprafter backup check [--repo s3:…] [--credential-file <dotenv>] [--read-data]
```

`check` runs `restic check` against the repository — the same verification the
in-cluster **`apprafter-backup-check` CronJob** runs weekly (default
`0 6 * * 0`) before, under `enforce: check`, it prunes ([what that Job actually
runs](../how-it-works/backup-retention-and-checks.md#what-the-weekly-check-runs)).
By default it verifies structure only; `--read-data` re-downloads and re-hashes
**every** pack for a deep verify (slower, bandwidth-heavy). Like `prune`, it
reads repository and credentials from the cluster when you do not name them,
so `apprafter backup check` on its own is a complete command. Run the
operator-side `check` when your provider can't express the scoped-delete policy
and you have [turned the in-cluster check off](#turning-the-in-cluster-check-off), or any
time you want a manual verification with full credentials.

#### Changing what the weekly check verifies

```sh
apprafter backup set check-depth 10%          # the default: a random tenth each week
apprafter backup set check-depth full         # every pack, every week
apprafter backup set check-depth structure    # metadata only, reads no data
```

`backup set` changes **one** field and leaves the rest alone. `backup enable`
composes the whole `spec.backup` block from its flags and the platform
defaults, so re-running it to adjust one setting resets the others — including
this one. Use `set` for changes, `enable` for configuring.

The other settable keys are `enabled`, `at`, `check`, `cluster-name`,
`timezone`, `keep-daily`, `keep-weekly`, `keep-monthly`, `enforce`,
`staging-mode`, `failure-webhook`, `deadline` and `check-deadline`. The bucket
and its credential are deliberately not among them: pointing an existing
schedule at a different repository is a new repository, with its own init and
its own first backup, so it goes through `enable`.

#### Switching a configured schedule on and off

```sh
apprafter backup disable          # config retained, nothing runs
apprafter backup set enabled true # back on, unchanged
```

Two things leave a cluster holding a complete, correct, switched-off backup
configuration: `backup disable`, and a `restore` that was told not to inherit
the source's schedule — with `--discard-backup-schedule`, or by answering no
when it asked
([why](restore.md#the-backup-schedule-comes-with-the-restore-and-you-are-asked-about-it)).
`backup set enabled true` is the way back in both cases. Re-running `backup
enable` is not: it composes the whole block from its flags, so it would reset
the schedule, timezone, retention and staging mode you were trying to keep.

#### Renaming the cluster in its repository

```sh
apprafter backup set cluster-name eu-prod
```

`cluster-name` is the name this cluster's snapshots are listed under — it
becomes the restic host on every snapshot and the CLUSTER column of
`apprafter backup list`. It defaults to the target name at `backup enable`.

Renaming is safe at any time and affects nothing but the label: snapshots are
attributed by the cluster's own identity, not by this name
([which snapshots are
yours](../how-it-works/backup-retention-and-checks.md#which-snapshots-are-yours)).
Snapshots already in the repository keep the name they were written under.

A cluster restored from a backup inherits the source's name, because the whole
backup configuration is replayed — the restore summary tells you when that has
happened, and this is the command it points at.

#### Turning the in-cluster check off

Pass `--check off`:

```sh
apprafter backup enable --bucket s3:… --credential apprafter-backup-s3 \
                        --check off --i-have-saved-credentials
```

The weekly CronJob is then not created at all, and `apprafter backup status`
reports `check: off` so nobody has to infer it from an absence. Under the
default `enforce: check` that also turns the in-cluster prune off, and the
retention verdict says `CheckOff`.

Or set `PlatformStack.spec.backup.checkSchedule` to `""` in your infra repo, if
the backup block is git-managed — that is the same thing, expressed durably.

Do **not** reach for `kubectl patch cronjob apprafter-backup-check
--patch '{"spec":{"suspend":true}}'`. The CronJob is chart-owned and the
platform components sync with `selfHeal: true`, so Argo CD reverts the
suspend on its next reconcile.

Whichever you choose, run `apprafter backup check` operator-side on your own
cadence — parking the in-cluster check means nothing verifies the repository
until you do.

> **Where check failures surface.** A check that does not pass turns
> `apprafter status` red (`RepositoryCheckFailed`), and `apprafter backup status`
> shows it under `last check` with the first lines of restic's output; the
> failed Job's pod log has all of it, and `apprafter backup check` runs the same
> check with full credentials. A check that does not pass never prunes
> ([where the result is
> recorded](../how-it-works/backup-retention-and-checks.md#where-the-check-result-shows-up)).

```text
apprafter backup unlock [--repo s3:…] [--credential-file <dotenv>]
```

`unlock` removes **only stale** locks (`restic unlock`) — a live lock held by a
running backup is never touched. Reach for it when a Job was killed mid-run
(OOM, a node reboot) and left a lock behind that blocks the next operation. The
in-cluster CronJobs already unlock stale locks as their first step, so you
mostly need `unlock` for operator-side `prune`/`check` against a repo whose last
in-cluster run died unexpectedly.

#### A run that stopped at its deadline

A backup or check Job runs for six hours at most. Past that, Kubernetes stops
it and fails the Job with reason `DeadlineExceeded`; `backup status` shows it
as `Failed: DeadlineExceeded`, and the next scheduled run goes ahead as normal
([how long a run may take](../how-it-works/backup-retention-and-checks.md#how-long-a-run-may-take)).
A backup stopped this way records it like any other failure: `lastError` in
`backup status` reads `run exceeded its deadline of 6h …`, and the failure
webhook fires. That record is the runner's own, so a Job whose pod never
started records nothing but its `Failed: DeadlineExceeded` line: [the backup
runner's pod cannot be scheduled](backup-restore.md#runner-unschedulable).
If your backups legitimately take longer — typically the first
one of a large data set, and the same limit applies to each claim's dump
within it — raise the limit:

```sh
apprafter backup set deadline 12h         # the backup Job; 6h by default
apprafter backup set check-deadline 12h   # the weekly check Job; 6h by default
```

Both take whole hours, minutes or seconds (`12h`, `90m`, `43200s`), ten
minutes at least. Keep each shorter than the interval between two runs of its
schedule: if you run backups more often than every six hours, lower the limit
below that interval rather than raising it. The two also bound each other,
because the check takes the repository's exclusive lock and a backup and a
check that overlap fail one another: keep the backup's limit below the time
from a backup to the next check, and the check's below the time from the check
to the next backup. Under the defaults that is three hours (03:00 to Sunday
06:00) and twenty-one; if Sunday backups run past three hours, move the check
later with `apprafter backup set check 12:00`
([the two schedules bound each other](../how-it-works/backup-retention-and-checks.md#how-long-a-run-may-take)).
If the backup block is git-managed,
set `activeDeadlineSeconds` / `checkActiveDeadlineSeconds` under
`spec.backup` in your infra repo instead.

The backup limit reaches `apprafter backup create`, `apprafter export` and
`apprafter restore` too, though nothing stops those commands as a whole: each
dump or load they run in a helper pod gets the limit, and never less than six
hours, so lowering it for a frequent schedule does not cut a restore short. One
that needs longer fails saying its helper pod's keep-alive ran out; raise the
limit with `apprafter backup set deadline` and run the command again.

A backup that failed with `pg dump of <namespace>/<claim> gave up` met one of
the two limits on a dump's start, both a lock held by another session. `gave
up: another session held a lock` is a table lock held for five minutes,
typically a migration, and the message names the tables. `gave up: pg_dump
wrote nothing for 10 minutes` is usually a lock on a view, a materialized view
or a sequence, typically a `REFRESH MATERIALIZED VIEW` or a migration's
transaction left open; in a database with thousands of tables it can also be
table locks. Run `apprafter backup run` again once that session has finished.

## Who prunes, and what the cluster's key may delete {#who-prunes-and-what-the-clusters-key-may-delete}

`--enforce` (and `apprafter backup set enforce`) controls **who runs
retention**, and so **how much delete power the cluster credential needs.**
What that credential reaches once it is in the cluster — the S3 keys it maps
to, and the Kubernetes verbs the runner's ServiceAccount holds — is [on the
mechanism
page](../how-it-works/backup-retention-and-checks.md#what-the-in-cluster-credential-can-and-cannot-do).

```sh
apprafter backup set enforce check      # the default: the weekly check Job prunes, if the key may
apprafter backup set enforce cluster    # the backup Job prunes after every backup
apprafter backup set enforce operator   # nothing in the cluster prunes; you run backup prune
```

**Scoped key (recommended), with the default `enforce: check`.** The
in-cluster Secret carries S3 rights scoped to **Put / Get / List on the
repository prefix, plus Delete only on `locks/*`.** The backup Job does
`restic backup` only; the weekly check Job checks, and its prune is refused at
the first delete — it **cannot** delete `data/`, `index/`, or `snapshots/`
objects, so a cluster compromise (ransomware) cannot erase the backup history.
Nothing is deleted, and `apprafter status` says retention is not enforced, with
the repository's growth: run `apprafter backup prune` with your **full**
credentials ([Retention and prune](#retention-and-prune), above). A minimal
bucket/IAM policy shape:

```jsonc
// scoped cluster credential (append-only-ish)
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

**A key that may delete, with `enforce: check`.** The weekly check Job prunes
after every check that passes, and nothing else is needed. A check that fails
never prunes. A compromised cluster can now delete the history; use it only
with compensating provider controls (object versioning / object lock).

**`enforce: cluster`.** The in-cluster Secret carries **full** credentials, and
the backup Job prunes after each backup: the repository never holds more than
the keep policy between two weekly checks, at the cost of a prune and an
exclusive lock every night. With a scoped key every backup fails at the prune
(the snapshot is still taken), so this mode needs the key that may delete, with
the same trade-off and the same compensating controls.

**`enforce: operator`.** Nothing in the cluster prunes, whatever the key may
do. Retention is `apprafter backup prune` on your cadence, and `apprafter
status` says `not enforced in the cluster, by choice`, with the repository's
size.

> **Upgrading from before platform-stack 0.2.80.** The default was `operator`
> until then. A cluster whose `spec.backup.retention.enforce` is set keeps what
> it set; one that never set it runs `check` from 0.2.80 on, and **if its key
> may delete, its first weekly check after the upgrade removes every snapshot
> beyond the keep policy** (7 daily / 4 weekly / 6 monthly unless configured).
> To keep the old behaviour, run `apprafter backup set enforce operator` before
> upgrading. A cluster on the scoped key deletes nothing either way.

> **Statement-level granularity is provider-dependent.** "Delete only on
> `locks/*`" needs **statement-level** scoping (different actions on different
> prefixes), which AWS-style policies express but some key-prefix-only providers
> do not. If your provider cannot express it and you decline to hand the cluster
> full delete, issue a credential with **no** Delete at all: `backup` still
> works (restic uses non-exclusive locks), but the in-cluster `check` cannot
> drop its own lock — [turn the in-cluster check off](#turning-the-in-cluster-check-off)
> and run `apprafter backup check` and `apprafter backup prune` operator-side
> instead. Verify your provider's
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
>
> The same boundary runs the other way, and it is the reason to be deliberate
> about who gets the passphrase: the snapshots contain the repository's **own**
> S3 credentials, because the sweep captures `apprafter-system` like every other
> namespace. Whoever you hand the passphrase to for a restore can read them —
> see [the passphrase also protects the repository's own
> credentials](backup-restore.md#the-passphrase-also-protects-the-repositorys-own-credentials).

## Reading the repository without AppRafter

If you need to inspect a repository with no AppRafter at all, stock `restic`
still works — it is a plain restic repository (see [Assumptions and
portability](restore.md#assumptions-and-portability)):

```sh
restic -r s3:<endpoint>/<bucket>/<prefix> snapshots
restic -r s3:<endpoint>/<bucket>/<prefix> check
```

## See also

- [How retention and the integrity check work](../how-it-works/backup-retention-and-checks.md) —
  what a prune deletes, what the weekly Job runs, and the two ceilings on the
  cluster's credential.
- [Back up a cluster](backup-restore.md) — the two paths these settings apply to.
- [Restore from a backup](restore.md) — what the repository is for.
- [Secrets](secrets.md) — sealing the credential Secret this page narrows.
