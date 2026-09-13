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
                  [--data-only] [--passphrase <value>] [--reprovision] \
                  [--keep-backup-schedule | --discard-backup-schedule]
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

### The backup schedule comes with the restore, and you are asked about it

A restore replays the whole backup configuration — bucket, credential
reference, schedule, timezone, retention counts and `enforce`, because all of it
lives in the one `PlatformStack` the restore replays. Inherited unchanged, the
restored cluster starts backing up to the **source's** repository on the
source's schedule.

Whether that is what you want depends on something the snapshot does not
record. There are three restores and inheriting is right in two of them:

| Situation | Inherit? |
| --- | --- |
| A second cluster that should not touch the old destination | No |
| [Moving to a bigger machine](moving-to-a-bigger-machine.md) — the old cluster is retired once the new one proves out | Yes |
| Disaster recovery — the cluster died and this one replaces it | Yes |

Nothing in the artifact distinguishes them, so when the replayed block carries
an **enabled** schedule, `restore` asks:

```text
This backup carries the source cluster's backup schedule, and it is ENABLED.
  repository: s3:https://nbg1.your-objectstorage.com/prod-backups
  schedule:   0 3 * * * (Europe/Berlin)
...
Inherit the source's backup schedule? [y/N]
```

Answering **no** restores the block exactly as captured but switched off.
Turning it on later changes that one field and nothing else:

```sh
apprafter backup set enabled true
```

Not `apprafter backup enable` — that composes the whole `spec.backup` block
from its flags, so it would reset the schedule, timezone, retention and staging
mode the restore just carried across.

Two flags answer the question up front, and either one skips the prompt:

| Flag | Effect |
| --- | --- |
| `--keep-backup-schedule` | Inherit it enabled: this cluster becomes the repository's writer |
| `--discard-backup-schedule` | Replay the block as captured but switched off |

**A non-interactive restore must pass one of them.** With no terminal to ask
on, `restore` stops and names both rather than pick a side: a scripted restore
that silently inherited would redirect a still-running cluster's backups, and a
scripted restore that silently discarded would leave a disaster-recovery
replacement unbacked. Either way, the summary states what happened.

`--data-only` is unaffected — it replays no custom resources, so nothing is
asked and the target's own backup configuration is not touched at all.

Inheriting while the source cluster is still running and still backing up puts
two live clusters on one repository. They are told apart by identity, but they
share its retention, and a prune run by either can remove the other's
snapshots.

The inherited block includes the name the source cluster's snapshots are listed
under. When that happens the summary says so and names
`apprafter backup set cluster-name <name>` — the restored cluster's own
snapshots are still attributed to it correctly; only the label is inherited.

### Your domains, your certificate, and the origin firewall

The zones you registered with `apprafter target domain add` are part of the
`PlatformStack`, so they come back with it — and the platform Gateway is
rendered from them, with `tls.certificateRefs` naming the certificate Secret
those zones were imported against.

The certificate itself comes back too. It is captured as a whole Secret and
re-applied **before** the `PlatformStack`, so the Gateway is never rendered
against a reference that is not there yet. The summary says how many were
restored.

A backup taken before certificates were captured brings the zones back without
the certificate. That is the one case where a restored cluster resolves but
does not serve, so the restore names it explicitly, and so does `apprafter
target domain list` (a `MISSING` marker on the Cert column). The fix is the
import alone:

```sh
apprafter target cert import <name> --cert <file> --key <file>
```

Do **not** re-run `apprafter target domain add` afterwards. The zone is already
registered — it came back in the snapshot — so the command refuses with
`Domain already registered`. The import is the whole repair.

The **Cloudflare origin firewall** comes back too. The firewall is a cloud
object, reconciled from the target on your machine, but the *intent* is
recorded in the `PlatformStack` (`spec.firewall.cloudflareOrigin`) — so every
backup carries it, including the scheduled in-cluster one. `restore
--reprovision` reads it off the restored CR and turns the firewall on for the
target it provisioned; otherwise a rebuild onto a new target would come up with
`80`/`443` open to the internet while the source had them restricted. It is
only ever turned **on** by a restore, never off. On the modes that provision
nothing, the restored CR still carries the intent but no firewall is touched,
and the summary says so. A snapshot of a cluster that never recorded an answer
is *unknown*, not "off", and the restore stays quiet rather than guessing.

There is a window here, and it is not papered over. The node is provisioned at
the start of the restore and the intent only becomes readable once the
`PlatformStack` has been replayed a few steps later, so the new node's
`80`/`443` are open for the length of the restore. That is inherent: the
snapshot lives behind a kubeconfig that does not exist until the cluster does.
If it matters for your cutover, keep DNS pointed at the old cluster until the
restore finishes and the summary confirms the firewall — which is what
[Move to a bigger machine](moving-to-a-bigger-machine.md) has you do anyway.

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

### If a restore stops partway

Both modes hold your workloads **down** for the middle of the run — a restore
must not let a pod write to a database it is still loading — and bring them
back in the last step. A restore that fails, times out, loses its connection or
is interrupted with `Ctrl-C` therefore leaves a cluster whose applications are
scaled to zero with Argo CD auto-sync switched off. That is expected, and it is
recoverable.

The restore says so on the way out, naming every application it left down and
every Argo CD Application whose auto-sync it disabled:

```text
✗ The restore stopped before it finished, and it did not undo what it had already done.
  1 application(s) were scaled to 0 replicas for the load and are still down:
    - demo/web → 3 replica(s)
  Argo CD auto-sync is switched OFF on 1 Application(s), so GitOps will not put
  any of this back on its own:
    - argocd/web-prod
  Re-running the SAME command is the way to continue: its last step is the one
  that puts the replica counts and auto-sync back.
```

**Re-running the same command is the remedy.** Its final step is the one that
restores the replica counts and re-enables auto-sync, so a second run that
reaches the end leaves the cluster correct.

**There is no resume.** The restore has no checkpoints and no `--continue`: a
re-run replays every step from the first, re-fetching the snapshot and
re-loading the data. Budget the same time as the first attempt.

A `--data-only` restore records each application's replica count **on the
application itself**, in the `apprafter.io/pre-restore-replicas` annotation,
in the same write that scales it to zero. That is what makes a second run
safe: it reads the recorded count rather than the zero the first run wrote, so
the app comes back at its real size instead of staying down. The annotation is
removed when the restore finishes. A full restore needs no such record — it
reads the counts from the backup artifact every time.

One caveat if you scale a suspended application up by hand and then re-run a
`--data-only` restore: the recorded count wins over the count you set, because
the recorded one is what the app had before any of this started. Remove the
annotation first if you meant the new number to stick.

??? note "Bringing the workloads back without finishing the restore"

    The interrupted run prints these commands with your own names and
    counts filled in; they are repeated here for when the output has
    scrolled away. Reach for them only if you have decided **not** to
    complete the restore — they bring the workloads up on whatever data
    is in the cluster right now, and if the run died during the load,
    that is a partly-loaded backend.

    The recorded count is readable on the application:

    ```sh
    kubectl -n demo get applications.apprafter.io web \
      -o jsonpath='{.metadata.annotations.apprafter\.io/pre-restore-replicas}'
    ```

    Putting it back is one patch, which also clears the record — leaving
    it in place would let it win over the live count on the *next*
    restore:

    ```sh
    kubectl -n demo patch applications.apprafter.io web --type=merge \
      -p '{"metadata":{"annotations":{"apprafter.io/pre-restore-replicas":null}},"spec":{"base":{"replicas":3}}}'
    ```

    Auto-sync is a separate object and has to be re-enabled on each Argo
    CD Application the run named:

    ```sh
    kubectl -n argocd patch applications.argoproj.io web-prod --type=merge \
      -p '{"spec":{"syncPolicy":{"automated":{"prune":true,"selfHeal":true}}}}'
    ```

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
