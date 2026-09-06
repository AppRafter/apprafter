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
