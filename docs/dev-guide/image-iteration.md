---
description: "How a re-pushed mutable tag becomes a rollout with no manifest edit, how to confirm it happened, how to opt out, and how to roll a bad deploy back."
---

# Image iteration: push and it deploys

When your CI re-pushes a container image under the same mutable tag
(for example `ghcr.io/acme/web:latest` built from your protected
branch), AppRafter rolls the running workload to the new build for
you. You do not edit the manifest, and you do not bump a tag — the
push is the deploy.

This page covers the loop, how to confirm it worked, and how to opt out
when you want a hand-pinned reference. Undoing a roll is
[Rolling back a bad deploy](rollback.md) — a different question, asked at a
different moment.

The design rationale lives in
[The image digest](../how-it-works/the-image-digest.md).

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
credentials. There is nothing extra to set up: the same credential you
register for *pulling* the image authenticates the digest lookup, and a
public image resolves anonymously. Register one with
[Private repos & registries](private-repos-and-registries.md) — that page
also covers which token scopes a registry pull needs, which differ from
the ones a Git clone needs.

If a private image has **no** covering `SourceCredential`, resolution
cannot read the registry. It then fails gracefully (next section)
rather than blocking your rollout.

## When it cannot read the registry

A private image with no covering credential, or a registry that is down, means
the digest cannot be resolved. The rollout is **not** blocked: the platform
falls back to the tag as written and says so in the application's status. What
that costs you, and the hand-pinned reference that opts out of resolution
entirely, are on
[The image digest](../how-it-works/the-image-digest.md).

## See also

- [Rolling back a bad deploy](rollback.md) — `apprafter app rollback`, the pin
  it leaves behind, and the `kubectl` escape hatches.
