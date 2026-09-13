---
description: "Replaying a backup into a running cluster: the three target modes, the ordering the restore is safe under, and the disaster-recovery runbook for an off-site repository."
---

# Restore from a backup

The half of the backup story you read when something has already gone wrong.
[Back up a cluster](backup-restore.md) is the half you read before that, and
the one to set up first.

A restore replays a `backup create` artifact — native data, the AppRafter and
Argo CD custom resources, and the user secrets — into a **running, already
bootstrapped** cluster. Nothing here rebuilds a machine on its own except
`--reprovision`, which provisions one first and then restores into it.

## `apprafter restore` — replay a backup into a running target

```text
apprafter restore <repo> [--target <name>] [--snapshot <id>] \
                  [--data-only] [--passphrase <value>] [--reprovision]
```

`restore` replays a `backup create` artifact into a **running, already
bootstrapped** target cluster. The target defaults to the active target; pass
`--target <name>` to pick another registered target. `--snapshot` selects a
specific snapshot (default `latest`).

`latest` stays inside one cluster's history. When the target has snapshots of
its own in the repository, `latest` is the freshest of **those**. When it does
not — a freshly provisioned cluster, which is the ordinary disaster-recovery
shape — `latest` is the freshest run in the repository, unless the repository
holds more than one cluster's snapshots, in which case `restore` refuses rather
than guess which one you meant. `apprafter backup list --all-clusters` shows
every run with the cluster it came from, and `--snapshot <id>` then says exactly
which to replay. [Which snapshots are
yours](../how-it-works/backup-retention-and-checks.md#which-snapshots-are-yours)
explains the attribution.

A restore replays the whole backup configuration, including the name the source
cluster's snapshots are listed under. When that happens the summary says so and
names `apprafter backup set cluster-name <name>` — the restored cluster's own
snapshots are still attributed to it correctly; only the label is inherited.

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
  [Moving to a bigger machine](moving-to-a-bigger-machine.md).

### What it does, in order

A restore replays config, then gates the applications, then waits for their
claims, then loads the data, then re-seals the secrets, then resumes the
workloads. The order is load-bearing and the reasons are on
[How a restore replays a backup](../how-it-works/how-a-restore-works.md) — read
it before a restore that matters, because two of the steps exist to stop a
workload writing to a database that is not there yet.

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

## Restore from an off-site (S3) repository

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
3. **Choose the mode** (the same three modes as the local-pull restore above — see
   [Target modes](#target-modes)):
   - `--reprovision` — the source cluster is dead: provision **and** bootstrap a
     fresh cluster in the registered target, then replay. Real-Hetzner only.
     (The same mode with `--server-type` also performs a *planned* move onto a
     bigger machine — [Move a cluster onto a bigger
     machine](moving-to-a-bigger-machine.md).)
   - neither flag (restore into a running, already-bootstrapped target; select
     it with `--target <name>`) — the default path.
   - `--data-only` — reload only native data into an already-configured target.
4. Restore **auto-detects the backup format** — monolithic (the default, one
   snapshot per run) vs sequential (a versioned snapshot-set) — by reading the
   manifest version, so you don't specify the format. `latest` resolves to the
   freshest run of **either** format, within one cluster's snapshots as above.
   Both staging formats restore identically from the operator's side; the only
   difference is on the write path.

The restore ordering, the gating and replay-order invariants, and the secret re-sealing behavior
are identical to the local-pull restore documented above — the only difference
is the repository lives in S3 and the credentials come from the operator, not
the target.

## Verify checklist

Two checks are worth running before you trust the off-site backup, mapping to
the design's Verify items:

- **Confirm your provider honors a prefix-scoped delete.** With the
  `enforce: operator` scoped credential, actively **test** that the cluster
  credential can delete an object under `locks/*` but is **refused** deleting an
  object under `data/` (or `snapshots/`). If the deny doesn't hold, your
  provider can't express the append-only guarantee — fall back to
  `enforce: cluster` with provider object-lock, or to the no-delete variant
  with the in-cluster check
  [turned off](backup-maintenance.md#turning-the-in-cluster-check-off).
- **A minimal end-to-end.** `apprafter backup enable` → wait for (or
  trigger) one backup Job → `apprafter backup status` shows a fresh
  `lastSuccess` → `apprafter restore s3:… --reprovision` into a **throwaway**
  cluster (or `--target` a running one) and confirm your data and a sealed
  secret came back.
  This is the only test that proves the whole chain — the passphrase you saved,
  the bucket policy, the runner, and the restore path — actually works together.

## See also

- [Back up a cluster](backup-restore.md) — creating the artifact this page
  replays, and putting it off-site on a schedule.
- [Moving to a bigger machine](moving-to-a-bigger-machine.md) — the planned
  version of a restore-into-a-new-machine, with its own sequence.
- [Recovery and emergency console access](recovery.md) — when the node itself
  is unreachable and there is nothing to restore *into* yet.
