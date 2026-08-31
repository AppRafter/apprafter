# ADR 0059: rolling back a moving tag pins the application to a digest

## Status

`Accepted`

Date: 2026-08-31.

## Context

ADR 0040 made a moving tag deployable: the operator resolves `base.image` to a
registry digest on every reconcile and re-rolls the workload when the tag moves.
A developer can ship by pushing `:latest` and never touching the manifest, which
is the headline of the platform's deployment story.

The recovery path for that story does not exist.

`apprafter app rollback` patches `spec.source.targetRevision` on the Argo CD
Application — it rolls back the **Git revision**. After a same-tag push the Git
revision has not changed, so there is nothing for it to roll back to and the
command achieves nothing. `git revert` fails for the same reason. Nothing in the
tree retains the digest that was previously resolved, so even a rollback that
wanted to act on the image has no target.

The failure lands at the worst possible moment. A developer who has just shipped
a broken build runs the command named `rollback`, sees it report success, and is
no better off. The one remedy that does work — `kubectl rollout undo` — is
undone by the operator's next pass, so it is a workaround with an expiry
measured in seconds. The durable answer the guide currently offers is to switch
the manifest to a hand-pinned digest with `imagePolicy.resolve: off`: abandoning
the auto-deploy loop in order to recover from it.

Two properties of the problem shape everything below.

**A rollback against a moving tag is a mode change, not a value change.** Set the
workload back to the older digest and the next reconcile re-resolves `:latest`,
finds the bad build again, and rolls forward — within one 60-second requeue.
Rolling back therefore *necessarily* takes the application off the moving-tag
train. That is not a side effect to be tolerated; it is the operation.

**A pin cannot live in Git.** The manifest is the user's, and it says `:latest`.
A pin written into it would be a lie about intent, and would have to be un-said
by a second commit. But a pin held outside Git is invisible *to* Git: a reader of
the repository sees `:latest` and cannot know the cluster is deliberately holding
an older digest. The status surface is therefore not decoration here — it is the
only place the truth exists.

## Decision

We will make `apprafter app rollback` pin the application to a resolved image
digest, retain the previous digest to roll back *to*, and ship an un-pin verb in
the same change.

### The pin lives in an annotation on the AppRafter `Application` CR

`metadata.annotations["apprafter.io/image-pin"]` carries the full reference
(`<repo>@sha256:<64 hex>`), alongside `apprafter.io/image-pinned-at`.

The CLI writes it with server-side apply under a **dedicated field manager**,
`apprafter-cli-pin`, whose body contains nothing but `apiVersion`, `kind`,
`metadata.name`, `metadata.namespace` and the two annotation keys. Un-pinning
re-applies the same body with the keys omitted, and SSA prunes them.

Two rules are load-bearing and both are scars:

- **That manager writes nothing else, ever.** Because its ownership set is
  exactly two keys, a later re-apply that omits them prunes exactly two keys. If
  it ever also owned a sibling field, an un-pin would silently delete that too —
  which is the 2.10 egress defect (an SSA write pruned `source`/`values`, fixed
  by giving the write its own `apprafter-cli-egress` manager) reappearing at a
  new address.
- **Before writing, refuse if Argo owns the key.** The CLI walks
  `metadata.managedFields` for an Argo-shaped manager owning
  `f:metadata.f:annotations.f:apprafter.io/image-pin`, on the model of
  `egress_field_appears_git_managed`. Git wins the next sync, so a pin the
  manifest also declares would be reverted, and reporting success would be a lie.

We verified on a live apiserver (Kubernetes 1.35, this repository's generated
CRD) that per-key SSA ownership inside a custom resource's
`metadata.annotations` is real, that an Argo-shaped forced re-apply leaves a
foreign key untouched — including when the desired manifest carries an empty
`annotations` map, and when Argo owns a different key in the same map — and that
a same-manager omit-apply prunes it. This was measured rather than reasoned,
because the mechanism is per-key ownership on a CR whose `ObjectMeta` schema is
injected by apiextensions, and that granularity was a fair thing to doubt.

A pin-only partial apply **cannot create** the CR (`spec: Required value`), so
both verbs refuse explicitly on an application that has not synced yet rather
than half-succeeding.

*Rejected.* A per-app pin map on the `PlatformStack` singleton: it puts
application-scoped data on the platform's object, needs its own garbage
collection (an app deleted while pinned leaves a stale entry — defect D4's class
at a new address), and touches a spec whose SSA is already known-hazardous. The
annotation needs none of that and dies with the CR. The Argo CD `Application`
object: uncontested today, but that inverts under Phase 3's config-repo GitOps.
The `status` subresource: status is derived state and a pin is user intent.

### The operator honours the pin without traversing a path that can fail open

One new match arm, first, in the image-resolution block of the Application
controller. The pinned arm never calls `resolve_digest` and never consults the
resolve throttle.

The property that makes it an arm rather than an override is that **it can never
yield `None`**. The existing failure branch returns `None`, and the renderer then
falls back to the verbatim `spec.image` — the moving tag. A "resolve, then
override with the pin" implementation would therefore silently un-pin the
application during a registry outage and re-expose the bad build, which is the
exact failure the feature exists to prevent.

The annotation is hand-writable, so the pin is validated before it is honoured:
the reference must parse and must be a `sha256:` digest, and its repository must
match the manifest image's repository — otherwise a pin outliving an image-path
change would pull from a different repository altogether, which the migration
classifier grades `security-boundary`. Both sides are normalised through
`parse_image_ref` before comparison, because `nginx` and
`index.docker.io/library/nginx` are the same repository and a string compare
would reject every short-name image.

A rejected pin falls through to normal resolution and is reported loudly. It
never freezes the reconcile — the ADR 0048 anchor-403 rule — and it does not mark
the application as pinned, because claiming a pin that is not in effect is
precisely the assertion that passes in the degenerate case.

### `status.image` retains the previous digest, and stops being pruned

`StatusImage` gains `previous` (a record of `{resolved, tag, resolvedAt}`, not a
bare digest — after a manifest tag change a bare digest cannot say which tag it
belonged to) and `pinned`.

**Shift rule, on the resolution arm only:** shift `previous ← prior.resolved`
when the new resolution differs from the prior one, and carry `previous` forward
verbatim otherwise. The three non-shift cases each have a specific harm. A
resolve *failure* writes `resolved: None`, so shifting unconditionally would let
a second consecutive registry failure destroy the rollback target. A same-digest
re-resolve after the throttle expires would set `previous = current`, making
rollback a no-op onto the build being escaped. The pin arm must not rewrite
history at all.

A manifest tag change (`v1`→`v2`) **does** shift, under the same single rule. The
consequence is real and is accepted: a rollback can then hold a digest whose tag
the manifest no longer names. It is a true statement about what was running, and
`app status` says which tag the held digest came from rather than leaving the
reader to assume it was the current one.

**Retention forced a prune fix that is a defect in its own right.** Status is
applied whole-object under one forced field manager, so a payload that omits a
field prunes it. Five pause/failure status builders hard-code `image: None`, so
entering a MigrationPlan gate, a claim-pending pause, `EnvSecretMissing` or an
invalid effective spec **deletes `status.image` outright** today. Benign while
nothing reads it; fatal here, because it destroys the only rollback target
exactly when an application is in trouble. The same prune happens on the healthy
path under `imagePolicy.resolve: off`. All are fixed by carrying the existing
value forward.

While pinned the status records the pin as the resolved image and leaves
`resolvedAt` unset, which re-arms the resolve throttle so an un-pin resumes
tag-following on the next reconcile rather than up to a minute later.

**No CRD change.** `status` carries `x-kubernetes-preserve-unknown-fields`, and
the CUE schema deliberately does not declare `image`. Declaring it as a drive-by
would make the apiserver prune the nested fields — which `application.cue`
already records happening to `recommendedResources`.

### `ImageResolved` becomes tri-state, because absence is already spoken for

| state | status | reason |
| --- | --- | --- |
| resolved | `True` | `Resolved` |
| pinned | `True` | `Pinned` |
| pin rejected | `False` | `PinRejected` |
| resolve failed | `False` | `ResolveFailed` |

`True/Pinned` because the rendered reference *is* a digest, which is what the
condition asserts; `False` would read as a fault to anything gated on health.
`False/PinRejected` because the user's stated intent is not in effect and that
must be loud. "Absent" is not available as a state — it already means
`resolve: off`.

### CLI

`--to sha256:<hex>` is an image digest; anything else is a Git revision. A value
carrying a colon that is not a well-formed digest is rejected rather than passed
through, because Git refnames forbid `:` and falling through would only defer the
failure to Argo.

**Bare `apprafter app rollback <name>` prefers the retained digest** when one
exists, and falls back to the previous Git revision otherwise. For a
tag-following application the Git path provably does not roll the workload back —
that is this defect — so preferring it would be preferring the known-wrong
answer. The confirmation prompt names the mode chosen and the flag that forces
the other.

**`apprafter app unpin <name>` is mandatory, not polish.** Without it `rollback`
is a one-way door out of the platform's headline feature, and the only way back
is hand-editing the manifest — the same trap, entered from the other side. The
name transposes the existing `platform freeze` / `unfreeze` pair one noun over.

### Visibility

`app status` prints a yellow line stating that the application is held at a
digest and is no longer following its tag, naming the un-pin command.
`platform status` lists every pinned application, so an operator sees them in one
place. In Argo CD, a new branch in the existing `apprafter.io_Application` health
script returns **`Suspended`** — a pinned application is deliberately held, not
broken, and `Degraded` would be wrong on the merits as well as tripping
health-gated automation.

Two limits on the Argo surface, both established by reading gitops-engine at the
version this platform ships rather than by reading its documentation:

- `Suspended` is the *second-healthiest* code in the health ordering, so it
  overrides `Healthy` and nothing else. The pin shows on the tile only when every
  other managed resource is healthy. The pinned branch must therefore be
  evaluated **first** and keyed on the pin marker alone — nested under a phase
  check it would read `Progressing` exactly while a rollback rolls pods. And in a
  repository rendering several applications into one Argo Application, one
  pinned CR beside one reconciling CR yields a `Progressing` tile: the pin is
  invisible in precisely the multi-app case. Stated as a limit rather than
  discovered later.
- A permanently-`Suspended` managed resource **hangs a sync operation** in
  `Running` when the task set spans more than one wave or phase. The shipped
  user-application shape — one CR, no waves, no hooks, no
  `managedNamespaceMetadata` — is single-wave and single-phase, so the operation
  completes. That is an **invariant this platform must keep**, and the walk
  asserts the operation reaches `Succeeded`, not merely that the tile is
  `Suspended`.

## Consequences

A developer who ships a bad build through a moving tag can recover with one
command, and can return to auto-deploy with a second. The state in between is
stated in words wherever it is visible at all.

Rolling back stops the application receiving new builds. That is inherent — it is
what "roll back a moving tag" means — but it makes the un-pin verb and the
visibility surface load-bearing rather than decorative, which is why both ship in
the same change rather than as follow-ons.

Five status builders stop destroying `status.image`. That is a defect fixed in
passing, and the retention feature is what made it visible.

The pin is invisible to a reader of the Git repository, permanently. No design
avoids this while keeping the manifest honest about intent; we pay for it with
three status surfaces instead.

The Argo tile signal is real but conditional, and the conditions are recorded
above rather than left to be discovered by someone wondering why their pinned
application looks `Progressing`.

## Alternatives considered

**Retain the digest but do not pin.** Fails within one reconcile: the operator
re-resolves the tag and rolls the bad build forward again. This is the naive fix,
and it passes a walk that checks the workload once — which is why the walk
asserts the digest is still held after two or more reconciles.

**Pin by rewriting the user's manifest to `imagePolicy.resolve: off` plus a
hand-pinned digest.** This is what the documentation recommends today. It
requires a commit to recover and a second commit to return, puts a temporary
operational state into permanent history, and cannot be done at all by someone
without write access to the repository.

**Express the pin through the existing resolve throttle.** The throttle re-arms
on a tag change and on a stale timestamp, so a throttle-based pin leaks after one
minute. It is a cache, not a mode.

**`app rollback --undo` instead of a separate un-pin verb.** No command in this
CLI reverses itself through a flag, and it reads as "undo the rollback" rather
than "resume following the tag".

**Return `Degraded` in the Argo health script.** A pinned application is held,
not broken. Beyond being wrong on the merits it would trip alerting and
health-gated automation, which ADR 0048 already chose `Suspended` to avoid.
