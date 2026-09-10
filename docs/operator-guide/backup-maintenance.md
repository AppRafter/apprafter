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
In the default `enforce: operator` mode you run it yourself, outside the
cluster, with full credentials:

```text
apprafter backup prune [--repo s3:…] \
                       [--credential-file <dotenv>] \
                       [--keep-daily N] [--keep-weekly N] [--keep-monthly N]
```

`--repo` defaults to `PlatformStack.spec.backup.bucket`; the keep-* flags
override the configured `spec.backup.retention` (else the 7/4/6 defaults).
Credentials resolve from `--credential-file`, then the environment, then the
credential Secret the cluster already holds (`spec.backup.credentialRef`) — so
against a configured cluster this command needs no credential flags at all. On success
`prune` stamps `apprafter.io/last-prune` on `PlatformStack`, which
`backup status` then shows. Run it on your own cadence (e.g. monthly) — restic
dedup makes growth sub-linear, so retention is a rare, deliberate operation, not
a per-run one.

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
`0 6 * * 0`; [what that Job actually
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

The other settable keys are `at`, `check`, `timezone`, `keep-daily`,
`keep-weekly`, `keep-monthly`, `enforce`, `staging-mode` and
`failure-webhook`. The bucket and its credential are deliberately not among
them: pointing an existing schedule at a different repository is a new
repository, with its own init and its own first backup, so it goes through
`enable`.

#### Turning the in-cluster check off

Pass `--check off`:

```sh
apprafter backup enable --bucket s3:… --credential apprafter-backup-s3 \
                        --check off --i-have-saved-credentials
```

The weekly CronJob is then not created at all, and `apprafter backup status`
reports `check: off` so nobody has to infer it from an absence.

Or set `PlatformStack.spec.backup.checkSchedule` to `""` in your infra repo, if
the backup block is git-managed — that is the same thing, expressed durably.

Do **not** reach for `kubectl patch cronjob apprafter-backup-check
--patch '{"spec":{"suspend":true}}'`. The CronJob is chart-owned and the
platform components sync with `selfHeal: true`, so Argo CD reverts the
suspend on its next reconcile.

Whichever you choose, run `apprafter backup check` operator-side on your own
cadence — parking the in-cluster check means nothing verifies the repository
until you do.

> **Where check failures surface.** `apprafter backup status` reports the most
> recent check Job on its `Last check Job:` line, so a red check is visible
> there. What it cannot tell you is *why*: the check Job never writes the
> runner's status ConfigMap, so the `lastError` you see is always a *backup*
> error, never a check error. For the reason, read the failed Job's pod log — or
> re-run `apprafter backup check` yourself with full credentials
> ([why](../how-it-works/backup-retention-and-checks.md#where-the-check-result-shows-up)).

```text
apprafter backup unlock [--repo s3:…] [--credential-file <dotenv>]
```

`unlock` removes **only stale** locks (`restic unlock`) — a live lock held by a
running backup is never touched. Reach for it when a Job was killed mid-run
(OOM, a node reboot) and left a lock behind that blocks the next operation. The
in-cluster CronJobs already unlock stale locks as their first step, so you
mostly need `unlock` for operator-side `prune`/`check` against a repo whose last
in-cluster run died unexpectedly.

## The scoped-credentials ladder — `enforce: operator` vs `cluster`

`--enforce` controls **who runs retention** and therefore **how much delete
power the cluster credential needs.** What that credential reaches once it is in
the cluster — the S3 keys it maps to, and the Kubernetes verbs the runner's
ServiceAccount holds — is [on the mechanism
page](../how-it-works/backup-retention-and-checks.md#what-the-in-cluster-credential-can-and-cannot-do).

**`enforce: operator` (the default).** The in-cluster Secret should carry S3
rights scoped to **Put / Get / List on the repository prefix, plus Delete only
on `locks/*`.** The scheduled backup Job then does `restic backup` only — it
**cannot** delete `data/`, `index/`, or `snapshots/` objects, so a cluster
compromise (ransomware) cannot erase the backup history. Retention runs
**outside** the cluster: you run `apprafter backup prune` with your **full**
credentials ([Retention and prune](#retention-and-prune), above). A minimal
bucket/IAM policy shape:

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
> drop its own lock — [turn the in-cluster check off](#turning-the-in-cluster-check-off)
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
