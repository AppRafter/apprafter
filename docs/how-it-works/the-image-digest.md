---
description: "How a moved tag becomes a rollout: what resolves the tag to a digest, when it happens, and what the platform does when it cannot read the registry."
---

# The image digest

How pushing the same tag twice produces a new rollout. The recipe is
[Image iteration](../dev-guide/image-iteration.md); none of this is needed to
use it.

Read it when a push did not roll, or when you are deciding whether to opt out.

The decision is
[ADR 0040](../adr/0040-image-digest-resolution.md). The short form: Kubernetes
does not roll a Deployment whose pod spec has not changed, and a re-pushed tag
does not change the pod spec — the tag string is identical. So something has to
turn the tag into the digest it currently points at, and write *that* into the
spec. That something is the operator, and this page is what it does.

## When the registry cannot be read

Resolution is best-effort: it **never blocks the rollout**, and a
failure never starts one either. If it fails — the registry is
unreachable, the reference is malformed, or a private image has no
covering credential — the operator keeps the Deployment on the digest
it is already running for that same image reference, and records a
status condition `ImageResolved=False` with the reason and the digest
it kept. `apprafter app status <app-name>` reports the phase; the
condition itself reads:

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

The pod template does not change, so nothing rolls: the workload keeps
running the digest it had. What you lose until resolution succeeds
again is the auto-roll on same-tag pushes; a push made while the
registry cannot be read rolls out on the first reconcile after it can.
`apprafter app status` keeps showing the kept digest, and the age next
to it is when the registry last confirmed it, so it grows for as long
as resolution fails. Fixing the cause (for example registering the
missing `SourceCredential`) restores resolution on the next reconcile.

There is no digest to keep in two cases, and then the operator renders
the tag **as written** (the pre-resolution behaviour): a reference that
has never been resolved — most often a private image that never had a
covering credential — and a reference you have just changed in the
spec, because a digest resolved for one reference is never rendered for
another.

The operator knows which reference a running digest belongs to because
the Deployment records it: the `apprafter.io/image-ref` annotation,
written in the same apply as the image. `status.image` holds the digest
too, but it is written at the end of a reconcile, so a reconcile that
fails partway can leave it behind the Deployment. It is used only when
the Deployment has no record for the reference the spec names.

## What opting out actually changes

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

- [Image iteration](../dev-guide/image-iteration.md) — the loop, and how to
  confirm a roll happened.
- [Rolling back a bad deploy](../dev-guide/rollback.md) — returning to the
  previous digest, and the pin that leaves behind.
