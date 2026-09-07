---
description: "Undoing a bad deploy: what `apprafter app rollback` reverts, the pin it leaves behind, and how to release it."
---

# Rolling back a bad deploy

The build that is running is wrong and you want the previous one back.
This is developer-flow step 10, and it is a different reading moment from
[Image iteration](image-iteration.md), which is about what the platform does
with your tag **before** you write the manifest. The two used to share a page
and nothing connected them but chronology.

`apprafter app rollback` undoes a bad **manifest** change. It reads
Argo CD's sync history for your application, points the application at
an earlier Git revision, and lets auto-sync do the rest — the same path
a normal deploy takes, so the rollback lands within a reconcile cycle.

Look at the history first. `apprafter app status <app-name>` prints the
revision the application currently tracks and its last three syncs;
these are the lines that matter here:

```text
  revision:      main
  sync state:    Synced
  health:        Healthy

Recent revisions (last 3):
  #  7 9f2c1ab    2026-08-19T18:04:11Z
  #  6 3e81f40    2026-08-19T11:52:07Z
  #  5 c07a2d9    2026-08-18T09:20:44Z
```

With no flag, the rollback goes to the sync **before** the most recent
one — `3e81f40` above. You are shown both revisions and asked to
confirm:

```sh
apprafter app rollback <app-name>
```

Or name the revision yourself — a commit SHA, a tag or a branch — and
skip the prompt. `--yes` is required in a non-interactive shell, where
there is nobody to answer it:

```sh
apprafter app rollback <app-name> --to 3e81f40 --yes
```

If the application is deployed to more than one environment, `--env`
chooses which deployment rolls back. A single deployment resolves from
the name alone, so the flag is only needed to disambiguate:

```sh
apprafter app rollback <app-name> --env staging --yes
```

Two refusals you may meet, both before anything is changed:

- **fewer than two entries in the sync history.** There is no
  "previous" yet, so pass `--to` with the revision you want.
- **the revision you asked for is the one already tracked.** The
  rollback would be a no-op, and the CLI says so rather than issuing an
  empty change.

### Rolling back an image, and rolling back Git

`--to` decides which of the two you get, by the shape of the value:

- `--to sha256:<64 hex>` is an **image digest**. The application is held
  at that image.
- anything else is a **Git revision** — a commit SHA, tag or branch.

With no `--to`, you get the image rollback when the platform has a
previous digest to offer, and the Git rollback otherwise. That default
is deliberate: if your regression arrived as a new build under an
unchanged tag, the older commit still names that same tag, so a Git
rollback would resolve to the same bad image and change nothing.

### An image rollback holds the application

Rolling back an image is not just a value change. If the workload were
simply set back to the older digest, the next reconcile would resolve
your tag again, find the same bad build, and roll forward — within a
minute. So the rollback **pins**: the application stops following its
tag until you release it.

```sh
apprafter app rollback <app-name> --yes
```

`apprafter app status` says so, in yellow:

```text
  image:         ghcr.io/acme/web:latest -> @sha256:9f2c…
  pinned:        held at ghcr.io/acme/web@sha256:41ab… — NOT following ghcr.io/acme/web:latest
                 resume with `apprafter app unpin web`
```

In Argo CD the application's tile reads **Suspended** with the same
message — held, not broken.

**A pinned application receives no new builds.** Pushing a fix does
nothing until you release it:

```sh
apprafter app unpin <app-name> --yes
```

Within a reconcile the application follows its tag again and picks up
whatever that now points at.

One property is worth knowing because nothing else can tell you: the
pin lives in the cluster, not in your repository. Your manifest still
says `:latest`, which is the truth about your intent — but it means a
reader of the repository cannot see that the cluster is deliberately
holding an older build. `apprafter app status`, `apprafter platform
status` and the Argo CD tile are the only places that fact exists.

### A rollback pins you off the branch

`apprafter app add` normally leaves an application tracking a
**branch** — the one you were on, or `main` when it had no way to tell
(`--branch` overrides it). Rolling back replaces that with the single
revision you rolled back to, which is the point: pushes to the branch
stop deploying, so the bad revision cannot arrive again while you fix
it. `apprafter app status` shows the change, with the `revision:` line
reading a commit instead of a branch name.

When the fix is merged, resume tracking with the same command — `--to`
takes the branch name:

```sh
apprafter app rollback <app-name> --to main --yes
```

## Escape hatch

One case the auto-deploy loop deliberately does not cover: you re-pushed
the same tag to the same digest — nothing moved — and you want fresh pods
anyway. There is nothing for the platform to act on, so roll the
`Deployment` by hand. It carries your manifest's `metadata.name` and
lives in your manifest's `metadata.namespace`, so pass `-n` exactly as
above:

<!-- docs: check=none reason=external-tool since=v0.2.51 — a manual re-pull the auto-deploy loop deliberately does not cover: a tag that has not moved has nothing for the platform to act on -->
```sh
kubectl -n <namespace> rollout restart deployment/<app-name>
```

`kubectl rollout undo` is **not** the way to revert a bad build. The
operator owns the `Deployment` and re-resolves on its next pass, so an
undo lasts until the next reconcile and no longer. Use
`apprafter app rollback`, which holds.

## See also

- [The image digest](../how-it-works/the-image-digest.md) — why a moved tag
  rolls at all, and what the pin actually holds the application at.
- [Image iteration](image-iteration.md) — what a re-pushed tag does, and the
  hand-pinned reference that opts out of it.
- [Deploying more than one environment](environments.md) — `--env` picks which
  deployment a rollback acts on when an application has more than one.
