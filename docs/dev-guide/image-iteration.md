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
credentials. There is nothing extra to set up: the same credential you
register for *pulling* the image authenticates the digest lookup, and a
public image resolves anonymously. Register one with
[Private repos & registries](private-repos-and-registries.md) — that page
also covers which token scopes a registry pull needs, which differ from
the ones a Git clone needs.

If a private image has **no** covering `SourceCredential`, resolution
cannot read the registry. It then fails gracefully (next section)
rather than blocking your rollout.

## Graceful fallback

Resolution is best-effort and **never blocks the rollout**. If it
fails — the registry is unreachable, the reference is malformed, or a
private image has no covering credential — the operator renders the
**verbatim tag** (the pre-resolution behaviour) and records a status
condition `ImageResolved=False` with the reason. `apprafter app status
<app-name>` reports the phase; the condition itself reads:

??? note "Reading the condition with kubectl"

    ```sh
    kubectl -n <namespace> get application.apprafter.io <app-name> \
      -o jsonpath='{.status.conditions[?(@.type=="ImageResolved")]}'
    ```

!!! warning "Spell out `.apprafter.io`, and pass the namespace"
    AppRafter's `applications.apprafter.io` CRD and Argo CD's
    `applications.argoproj.io` share the plural `applications`, so a
    bare `kubectl get application` is ambiguous — it can hand you the
    Argo CD object, which has no `ImageResolved` condition, and you
    read an empty result as "resolution is fine". The workload CR also
    lives in your application's own namespace (the one
    `apprafter app add --namespace` set, `apprafter` by default), never
    in `default`, so `-n` is required too.

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

## See also

- [Rolling back a bad deploy](rollback.md) — `apprafter app rollback`, the pin
  it leaves behind, and the `kubectl` escape hatches.
