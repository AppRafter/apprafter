---
description: "How a re-pushed mutable tag becomes a rollout with no manifest edit, how to confirm it happened, how to opt out, and how to roll a bad deploy back."
---

# Image iteration: push and it deploys

When your CI re-pushes a container image under the same mutable tag
(for example `ghcr.io/acme/web:latest` built from your protected
branch), AppRafter rolls the running workload to the new build for
you. You do not edit the manifest, and you do not bump a tag — the
push is the deploy.

This page covers the loop, how to confirm it worked, how to opt out
when you want a hand-pinned reference, how to roll a bad deploy back
with `apprafter app rollback`, and the `kubectl` escape hatches for
forcing or reverting a roll.

The design rationale lives in
[ADR 0040](../adr/0040-image-digest-resolution.md).

## The push → auto-deploy loop

`Application.spec.base.image` is a string you write — in practice a
mutable tag your CI produces. By default the operator resolves that
tag to its **current registry digest** on every reconcile and renders
the child `Deployment` pinned to `repo@sha256:<digest>` rather than to
the bare tag.

1. Your CI builds and pushes a new image under the same tag.
2. On its next reconcile (roughly once per minute — the operator
   re-resolves on the same ~60-second requeue it already runs), the
   operator reads the tag's current digest from the registry.
3. The new digest differs from the running one, so the rendered pod
   template changes, and Kubernetes performs an ordinary rolling
   update.

So a fresh push lands on the cluster within about a minute, with no
manifest change and no manual step. The human-readable tag stays in
your manifest and in Git; only the rendered `Deployment` carries the
digest.

This is the default on **every hardware tier** — no flag turns it on.

## Confirming the running image

`apprafter app status` surfaces the digest the operator resolved, so
you can answer "what is actually running" without reading the pod
spec:

```sh
apprafter app status <app-name>
```

The image line reads as the written tag, an arrow, and the resolved
digest, with the age of the resolution:

```text
AppRafter phase: Ready
  image:         ghcr.io/acme/web:latest -> @sha256:9f2c… (resolved 41s ago)
```

Git shows the tag; status shows what the cluster is running. If the
two ever drift, the `resolved` digest and its age are where you look.

The line is omitted when there is nothing to report yet — before the
first resolution, or when resolution is turned off (see below).

## Private images

Resolving the digest is a registry read, so a **private** image needs
credentials. AppRafter reuses the same `SourceCredential` you already
register for pulling the image: if a `SourceCredential` covers the
image's registry host, its credential authenticates the digest
lookup; a public image resolves anonymously. See
[ADR 0039](../adr/0039-source-credential.md) for how credentials
cover a registry.

If a private image has **no** covering `SourceCredential`, resolution
cannot read the registry. It then fails gracefully (next section)
rather than blocking your rollout.

## Graceful fallback

Resolution is best-effort and **never blocks the rollout**. If it
fails — the registry is unreachable, the reference is malformed, or a
private image has no covering credential — the operator renders the
**verbatim tag** (the pre-resolution behaviour) and records a status
condition `ImageResolved=False` with the reason:

```sh
kubectl get application <app-name> \
  -o jsonpath='{.status.conditions[?(@.type=="ImageResolved")]}'
```

The workload still runs on the tag; you lose the digest pin and the
auto-roll on same-tag pushes until resolution succeeds again. Fixing
the cause (for example registering the missing `SourceCredential`)
restores resolution on the next reconcile.

## Opting out: a hand-pinned reference

Set `spec.base.imagePolicy.resolve: off` to disable resolution for an
application. The operator then renders the image reference **exactly
as written** and performs **no** registry poll:

```cue
spec: {
    base: {
        image: "ghcr.io/acme/web@sha256:9f2c…"
        imagePolicy: {resolve: "off"}
        // …
    }
}
```

Use this when you manage your own reference — most commonly a
hand-pinned digest for an environment that requires immutable,
reviewed image changes. Opting out does **not** force you to write a
digest; it only turns resolution off. With `resolve: off` there is no
`status.image` and no `ImageResolved` condition, so `app status` shows
no image line.

The default (the field absent, or `resolve: digest`) is digest
resolution as described above.

## Rolling back a bad deploy

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

### It rolls back Git, not the image

The rollback moves the revision your manifest is rendered from. If the
regression arrived as a **new build pushed under an unchanged tag**,
the older commit still names that same tag, resolution still finds the
same current digest, and you get the same bad image back.

- The manifest changed (a new tag, an env value, replica count) →
  `apprafter app rollback`.
- The image behind an unchanged tag changed → the escape hatches below.

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

## Escape hatches

Two standard `kubectl` commands cover the cases the auto-deploy loop
does not:

- **Force a re-pull without changing the image.** If you re-pushed
  the same tag and want the workload to roll immediately rather than
  wait for the next reconcile (or you simply want fresh pods):

  ```sh
  kubectl rollout restart deployment/<app-name>
  ```

- **Revert to the previous build.** Neither `git revert` of a same-tag
  push nor `apprafter app rollback` is a true image rollback, because
  the tag in Git is unchanged either way. To roll the `Deployment` back
  to its previous pod template:

  ```sh
  kubectl rollout undo deployment/<app-name>
  ```

  The operator owns the `Deployment` and re-resolves on its next
  reconcile, so an undo holds only until the next resolution. To pin a
  specific build durably, switch the manifest to a hand-pinned digest
  with `imagePolicy.resolve: off`.
