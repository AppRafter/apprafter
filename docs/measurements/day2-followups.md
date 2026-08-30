# Day-2 follow-ups — product defects the 2.19j walk documented rather than fixed

Found while correcting the day-2 operations pages (2.19j). Each one is a
**product change**, not a documentation change: the pages now describe
the behaviour as it actually is, and these entries record what would
have to move for the description to go back to the intended one.

Internal working data — this tree is excluded from the site
(`exclude_docs` in `mkdocs.yml`) and from the documentation gate
(`docs/measurements/` is in `EXCLUDED`, `cli/docsgen/src/scan.rs:96`).

## VPA in-place right-sizing has never run: wrong feature-gate name

**Opened:** 2026-08-20 (2.19j, correcting
`docs/dev-guide/resources-and-autoscaling.md`).
**Status:** RESOLVED in platform-stack `0.2.56`, and **live-re-validated
on real Hetzner against `0.2.57` on 2026-08-28** — the gate the chart
ships is `InPlace`, matching the `updateMode` the operator renders
(`platform-stack/cue/component_vpa.cue`). Option 1 below is what was
taken. The gate rename itself shipped in `0.2.55`, which is **yanked**:
starting the controllers exposed a second unpinned upstream default
underneath this one — the recommender's own 250Mi minimum
recommendation — which made every application recommend an identical
250Mi and could not be clamped by `minAllowed: 32Mi`. `0.2.56` pins that
floor to the 32Mi seed. See ADR 0054's two amendments.

The re-validation is part of the resolution rather than a formality: for
a week this was *fixed in code and unverified live*, which is the state
the entry itself was written in. `e2e/vpa-walk.sh` on real Hetzner
confirmed all three controllers Available, the recommendation landing on
the pinned 32Mi floor, the mirror into `Application.status`, and — the
observation the walk had never made — the managed pod's
`requests.memory` **resized in place**, uid stable, no recreation.
**Severity (while open):** the whole applying half of ADR 0054 was
inert, silently.

### What is wrong

`platform-stack/cue/component_vpa.cue` passes

```text
--feature-gates=InPlaceOrRecreate=true
```

to the VPA **admission controller** (line 58) and the **updater**
(line 87). The chart is pinned at `vertical-pod-autoscaler` 0.11.0,
whose `appVersion` is `1.7.1` (confirmed with `helm show chart`), and
that version does not have a gate by that name. Reproduced locally
against the released images, no cluster involved:

```console
$ podman run --rm registry.k8s.io/autoscaling/vpa-updater:1.7.1 \
    --feature-gates=InPlaceOrRecreate=true
invalid argument "InPlaceOrRecreate=true" for "--feature-gates" flag: unrecognized feature gate: InPlaceOrRecreate

$ podman run --rm registry.k8s.io/autoscaling/vpa-admission-controller:1.7.1 \
    --feature-gates=InPlaceOrRecreate=true
invalid argument "InPlaceOrRecreate=true" for "--feature-gates" flag: unrecognized feature gate: InPlaceOrRecreate
```

The gate 1.7.1 *does* recognise is **`InPlace`** — from the same
binary's own `--help`:

```text
--feature-gates mapStringBool   A set of key=value pairs … Options are:
                                AllAlpha=true|false (ALPHA - default=false)
                                AllBeta=true|false (BETA - default=false)
                                CPUStartupBoost=true|false (ALPHA - default=false)
                                InPlace=true|false (ALPHA - default=false)
```

So both Deployments crash-loop on startup, and have since the component
shipped — the chart version is pinned, so there was never a window in
which it worked. Confirmed on the live cluster by the 2.19j walk: both
pods `CrashLoopBackOff`, nine days.

### Why it is silent

- The **recommender** carries no such flag, so it runs: recommendations
  are produced and mirrored onto `Application.status.recommendedResources`.
  Every read path (`apprafter app status`'s `VPA reco:` line, the VPA
  object's `status.recommendation`) keeps working.
- The mutating webhook is registered `failurePolicy: Ignore`
  (deliberately, and correctly — it stops a down admission pod from
  deadlocking pod creation), so nothing fails when it is absent.
- `Application.status.recommendedResources.notApplied` is set **only**
  for node-capacity infeasibility
  (`operator-controllers/application/src/lib.rs:1372`), never for "no
  updater". So the status reads exactly as it would if everything were
  working.
- The installed-check the docs previously offered —
  `kubectl get crd verticalpodautoscalers.autoscaling.k8s.io` — passes
  in this state. It answers "was the component rendered", not "is the
  autoscaler working".

Net effect: recommendations are computed and reported; **no pod request
is ever changed**. Applications keep the rendered `32Mi` forever.

### The decision needed — taken, option 1

Kept as written because the resize-wave warning below is the accurate
one and was nearly lost: it was recorded here on 2026-08-20 and did not
reach the release notes until a review caught the omission. It now sits
in the `0.2.56` `notes` block of `compatibility.cue`, which is what
`apprafter platform status` surfaces to an upgrading operator.

Renaming the gate to `InPlace` is a one-word change — and it flips live
behaviour from "nothing happens" to "the updater starts resizing running
pods". On a cluster that has been running for months at `32Mi` with
recommendations accumulating, that is a fleet-wide resize on the first
reconcile after the upgrade, not a no-op. It needs an owner's decision
and a maintenance window, which is why 2.19j documented it instead.

Options, in the order they should be considered:

1. **Rename to `InPlace` and ship it**, with the resize wave called out
   in the platform-stack changelog entry and the compatibility
   classification set accordingly (this is at least `requires-restart`
   for anything the updater cannot resize in place).
2. **Drop the gate and pin `updateMode: Off`-equivalent behaviour**
   explicitly, i.e. make "observe and report only" the documented,
   intended posture rather than an accident. Then
   `apprafter platform autoscale set` needs to say what it really does.
3. **Leave as-is.** Only acceptable while this file exists and the
   dev-guide page keeps saying so.

Whichever is chosen: **re-read the gate name on every chart bump.** The
component's own header comment already says so and it still drifted —
the name changed upstream and nothing here noticed, because a
crash-looping controller in a namespace nobody watches produces no
signal that reaches an operator.

### The guard, and why the walk had been passing

**Added 2026-08-28** — `e2e/vpa-walk.sh` now asserts all three VPA
controllers are Available and none is in `CrashLoopBackOff`, plus a soft
observation that an apply actually lands. This section previously asked
for that guard; it exists.

What is worth carrying is the reason it was missing. The walk was green
throughout the crash-loop — on platform-stack `0.2.49`, with the
updater and admission controller down — because it asserted only the
**recommender** path, which the bad gate never touched, and because a
dead updater passes a no-thrash check trivially. The tell was
`kubectl -n vpa get pods`, which the walk never read.

So the component was invisible to every gate we had *and* to the walk
written to exercise it. A walk that observes only the half of a
mechanism that still works is not a weaker check than none; it is worse,
because it reports green. That is the shape to look for elsewhere, not
the specific assertion.

## `apprafter backup enable --cron` / `--check-cron` accept a value the apiserver rejects

**Opened:** 2026-08-20 (2.19j, correcting
`docs/operator-guide/backup-restore.md`).
**Status:** open. **The documentation half landed and the CLI half did
not**, which is worth stating because that split is how an entry gets
forgotten: `backup-restore.md` no longer offers `--check-cron off`, says
plainly that there is no off switch today, and uses a never-firing
schedule instead — so the symptom stopped being visible while the defect
stayed. Re-verified 2026-08-30: both flags are still
`#[arg(long, value_name = "cron")]` with no `value_parser`
(`cli/platform-cli/src/cli.rs:1560`, `:1588`).

### What is wrong

Both flags are `Option<String>` with no validation
(`cli/platform-cli/src/cli.rs`, `check_cron` / `cron`). The value is
threaded into `spec.backup.schedule` / `spec.backup.checkSchedule` —
declared `string` in `schemas/v1alpha1/platformstack.cue:87` with no
pattern — and rendered verbatim into the CronJob's `schedule:` field
(`platform-stack/cue/render_tool.cue:538`).

A non-cron value therefore travels all the way to the apiserver before
anything objects, and when it does object it takes the platform-stack
sync down with it. Verified against a real apiserver (kind):

```console
$ kubectl apply -f apprafter-backup-check.yaml     # schedule: "off"
The CronJob "apprafter-backup-check" is invalid: spec.schedule: Invalid value: "off": expected exactly 5 fields, found 1: [off]
```

This is not hypothetical wording: `--check-cron off` was offered three
times in `backup-restore.md` as the way to disable the in-cluster check.
It is not implemented anywhere — no `"off"` literal exists in
`commands/backup.rs` — so every operator who followed that instruction
broke their platform sync. The page now documents a never-firing
schedule (`0 6 31 2 *`, verified accepted) instead.

### The fix

A clap `value_parser` on both flags that rejects anything that is not
five whitespace-separated fields, alongside the existing client-side
enum checks for `--enforce` and `--staging-mode`
(`commands/backup.rs:1732-1745`) — cron is the odd one out among the
validated options. Rejecting at the CLI keeps a typo from reaching the
CR at all.

Optionally, and separately: decide whether "disable the in-cluster
check" deserves a first-class spelling. If it does, it is a
`checkSchedule: ""`-means-omit-the-CronJob branch in the chart plus a
`--check-cron off` that maps onto it — **not** a string that reaches
`schedule:`.

## Two diagnostics whose `help:` text describes a layout that moved

**Opened:** 2026-08-20 (2.19j, correcting
`docs/operator-guide/troubleshooting.md`).
**Status:** open — text-only, but it is the text an operator reads at
the exact moment they are stuck.

Both live in `cli/cli-core/src/error.rs`. Left alone here because
changing a shipped diagnostic's help is a CLI release, and because the
troubleshooting page now states the correct answer beside the stale one.

1. **`apprafter::state::corrupt`** (error.rs:152-155) says "The local
   `.apprafter/state.json` file … delete `.apprafter/`". State moved to
   `<config-root>/state/<target>/.apprafter/state.json` in **v0.1.154**
   (`cli/cli-state/src/state.rs:10`, `commands/state_paths.rs`); the
   per-cwd file is a legacy artefact that is migrated once. An operator
   following the help deletes a directory that is not the problem, and
   the error keeps firing. The summary line does print the real path, so
   the fix is to make the help point at it rather than at a fixed
   location.

2. **`apprafter::provider::server_type_not_selected`** (error.rs:142)
   says "set `nodes[0].kind` in your Infrastructure manifest". There is
   no `kind` field on a node: the manifest schema declares
   `spec.nodes[*].type` (`schemas/v1alpha1/infrastructure.cue:29`) and
   `kind` is only the Rust field name, renamed because `type` is a
   keyword (`cli/cli-core/src/manifest.rs:106-113`). Every other surface
   gets this right — `apprafter restore --server-type` and
   `bootstrap-all --server-type` both say `spec.nodes[0].type` — so this
   one string is the outlier, and it names a field that a manifest
   author cannot set.

## Removing a `needs.*` entry orphans its ResourceClaim forever

**Opened:** 2026-08-30 (2.20c, correcting `docs/operator-guide/postgres.md`,
`redis.md`, `persistent-disk.md` and their `how-it-works/` siblings).
**Status:** OPEN.
**Severity:** high. Data and credentials the operator believes are on a
seven-day clock stay live indefinitely, and the shared backend they sit on can
never scale down.

### What is wrong

Four documents say the claim is garbage-collected. No code deletes it.

- ADR 0051:86-87 — "on the next reconcile the controller applies the new spec
  (**including any `needs` GC**)".
- ADR 0051:107-108 — "removal of any `needs.*` entry — the `ResourceClaim` and
  its data are garbage-collected."
- `spec.md:555` (public) — the same sentence.
- `docs/operator-guide/migration-plans.md:62` (public) — "the backing claim and
  its data are garbage-collected".
- `plan.md:3599` — the removal of a `needs.*` entry is described as triggering
  the 2.4f ResourceClaim GC, with the gate mandatory for that reason.

What the code does: the Application controller's needs block
(`operator-controllers/application/src/lib.rs:504-544`) is **generate-only**.
`has_needs` gates the whole block on the NEW spec, `generate_resource_claims`
builds payloads for the declared entries, and the loop SSA-applies exactly
those names. There is no LIST of live claims, no diff against a desired set,
and no delete. Removing the last need makes `has_needs` false and the block is
skipped entirely — the claim is not even read.

Approving the MigrationPlan changes nothing:
`ApplicationMigrationStrategy::execute_step`
(`operator-controllers/migration/src/strategy.rs:70-80`) unconditionally
returns `Succeeded` for every step, so approval marches the plan to `completed`
and does zero cluster work.

**The RBAC confirms it was never intended to be possible.**
`charts/apprafter-operator/templates/rbac.yaml:202-212` grants
`get, list, watch, create, patch, update` on `resourceclaims`. There is no
`delete` verb — no code path could delete a claim today even if one existed.
(Same class as the two RBAC-versus-code mismatches ADR 0048 and the 0.2.31 GC
fix already cost us.)

### What it costs

1. **The retention path never starts.** No delete means no `deletion_timestamp`,
   so the provisioner finalizer
   (`resourceclaim-provisioner/src/reconcile.rs:144-159`) never fires, no
   `RetainedClaim` snapshot is written, the seven-day clock never starts, and
   `gc.rs` never has anything to reclaim. The database, volume or ACL user
   survives indefinitely — and for redis it is *actively re-asserted* every 300s
   by the ACL resync loop.
2. **The shared backend can never scale down.** The reaper LISTs every live
   `ResourceClaim` as its veto set (`reaper.rs:541`) and vetoes on any match
   (`reaper.rs:389`, `Decision::Veto(VetoReason::Live)`). An orphan is
   indistinguishable from a live tenant, so the pool instance or shared CNPG
   cluster is pinned up forever by an application that no longer declares it.
3. **Visible but unflagged.** `apprafter app status` does list the claim (the
   orphan keeps its `ownerReferences`, and `list_resource_claims_for_app`
   filters on exactly that), but nothing marks it as undeclared — no condition,
   no phase, no metric. The operator has to diff the printed claim table against
   their own manifest to notice.

### Why every gate passed

No test asserts the current behaviour, and no walk exercises the path. All four
needs walks (`e2e/needs-pg-walk.sh`, `needs-redis-walk.sh`, `needs-disk-walk.sh`,
`app-migration-walk.sh`) delete the whole Application to test retention; none
removes a single `needs` entry. `app-migration-walk.sh` touches `needs_pg` once,
and only for the `selector` tripwire.

### The fix

The same reconcile already prunes two other declared-then-undeclared children —
`prune_http_route` (`lib.rs:758`, when the app stops being public) and
`prune_vpa` (`lib.rs:784`). Claims are the one child with no prune arm; the fix
is a third one in the same shape.

Ordering matters and is the whole risk: the delete must happen only after the
plan is approved and the new spec applied, so an unapproved edit cannot destroy
anything, and the provisioner finalizer must still run so the snapshot is
written and the seven-day window opens as documented.

Ships with: the missing `delete` verb in `rbac.yaml`, a unit test on the
desired-set diff, and a walk step that removes one `needs` entry from a
two-need app and asserts a `RetainedClaim` appears while the other claim is
untouched. Without that last one the class stays invisible exactly as it is now.

## A Dragonfly restart drops every claim's ACL user

**Opened:** 2026-08-30 (2.20c, correcting `docs/operator-guide/redis.md` and
`docs/how-it-works/needs-redis.md`).
**Status:** OPEN.
**Severity:** high. A single pod restart is a cluster-wide Redis
authentication outage for every non-persistent claim, for up to five minutes.

### What is wrong

Per-claim ACL users are created imperatively and only imperatively.
`dragonfly_object` (`resourceclaim-provisioner/src/dragonfly.rs:215-295`) emits
`replicas`, `args`, `resources`, `authentication.passwordFromSecret` and — when
persistent — a `snapshot` block. It sets **no `aclFromSecret` and no
`--aclfile`**, so the users exist only in the Dragonfly process's memory.
Upstream loads users from a file only when that flag is set
(`AclFamily::Init`), and the snapshot path never touches the ACL registry.

The `default` admin user survives, because the dragonfly-operator injects it as
`DFLY_requirepass` from `passwordFromSecret`. So after a restart the operator
can still log in while every tenant cannot — which is why the failure reads as
a credential problem rather than a restart.

Recovery is a blind fixed-period sweep, not a reaction:
`acl_reconcile::run` (`acl_reconcile.rs:210-221`) is
`loop { resync_all(...); sleep(RESYNC_INTERVAL) }` with `RESYNC_INTERVAL = 300s`
(`:57`). There is no watch and no readiness edge. The provisioner cannot help
either — `should_provision` (`reconcile.rs:1418-1421`) returns false once
`status.ready` is true, so a provisioned claim is never re-touched.

**ADR 0042 §4 ratified more than shipped** (`docs/adr/0042-…:79-81`): "It
watches the pool instances' pod readiness / `status` generation. On a (re)start
transition it enumerates every live `ResourceClaim` bound to that instance and
re-applies each `ACL SETUSER`" — **and** a periodic resync as a backstop. Only
the periodic half was built, and `plan.md:3170` ticks 2.6-5 on the stronger
wording.

### What it costs

The ACL table is process-global, so a restart drops every user at once.
`POOL_INSTANCE_INDEX` is hardcoded to `0` (`reconcile.rs:421`), so a cluster
runs exactly one ephemeral and one persistent instance — an ephemeral restart
is therefore a **cluster-wide** authentication outage across unrelated tenants
and namespaces. The window is uniform in (0, 300s], mean ~150s.

For a `persistent: true` claim the keyspace is restored from the snapshot but
the ACL table is not part of it, so the tenant is locked out of its own intact
data. That is the sharpest form: nothing was lost, and it is unreachable
anyway.

### The fix

`aclFromSecret` is a CRD field the platform already pins and has never used.

1. A pure builder beside `acl_setuser_args` (`dragonfly.rs:165`) emitting the
   same rule vector in ACL-file form, byte-compared against it in a test so the
   live grant and the persisted grant cannot drift.
2. A per-instance `Secret` `<instance>-acl` holding one line per live claim
   **plus a `default` line consistent with the admin password** — mandatory:
   upstream synthesises `default` only when the file omits it, so a file that
   carries one fully overrides the `requirepass`-derived admin, and a file that
   omits one risks locking the provisioner out of its own instance.
3. `spec.aclFromSecret` on the CR; the dragonfly-operator mounts it and adds
   the flag itself.
4. Rewrite the Secret at the three points that already mutate ACL state —
   `provision_dragonfly`, `gc_drop_dragonfly`, and the resync loop, which
   becomes the reconciler of the file (`claims_to_repin` already computes
   exactly the required set).
5. **Keep the loop.** The mounted Secret is read-only, so `ACL SAVE` is
   impossible, and Dragonfly does not re-read the file on Secret update without
   an explicit `ACL LOAD` — the loop remains the path for a claim created
   seconds before a restart. The failure mode collapses from "every tenant,
   every restart, up to five minutes" to "one just-created claim, rarely".

Upstream supports the rules the platform needs in file form:
`LoadToRegistryFromFile` parses each line with `ParseAclSetUser`, the same
parser `ACL SETUSER` uses, so `$N`, `~*` and `&user:*` all work by
construction. A live confirm on the pinned image is a confirmation, not an
investigation.

Ships with: a walk step that restarts the pool instance and asserts a tenant
can still authenticate immediately afterwards. No current walk restarts an
instance, which is why this was never caught.

## Rotating a secret does not take effect until something else restarts the pods

**Opened:** 2026-08-30 (2.20c, correcting `docs/dev-guide/secrets.md`).
**Status:** OPEN.
**Severity:** high, and security-relevant. The operation a developer performs
to revoke a leaked credential does not revoke it.

### What is wrong

`apprafter secret seal <name>` re-seals the value and the controller unseals it
into the `Secret`. Nothing else happens. The rendered Deployment still points at
the same `Secret` and the same key, so **no field of the workload changed** and
no rollout is triggered — running pods keep the value they started with, for as
long as they keep running.

`docs/dev-guide/secrets.md` documented the consequence honestly and then handed
the reader `kubectl -n <ns> rollout restart deployment -l
apprafter.io/application=<app>` as the remedy. That is a foreign command
standing in for platform behaviour, on a developer page, for the one operation
where being wrong is a security incident: a developer who rotates a leaked key
and sees the command succeed has not rotated anything on the pods still serving
traffic.

Nothing in the operator hashes or otherwise tracks the resolved contents of the
Secrets an application references — `grep -rn "env.secret.*hash\|secrets_hash"
operator/` returns nothing.

### What it costs

The window is unbounded. A pod that is not restarted for other reasons serves
the old credential indefinitely, and no status, condition or metric says so —
the application is `Ready` throughout, because from the platform's point of view
nothing changed. The failure is silent in both directions: the developer
believes the rotation landed, and the platform believes there is nothing to do.

### The fix

Stamp a hash of the resolved env-Secret contents onto the rendered pod template
as an annotation — `apprafter.io/env-secrets-hash`. Re-sealing changes the
hash, the template changes, and the Deployment rolls on the operator's next
reconcile. This is the standard Kubernetes idiom for exactly this problem, and
it keeps the behaviour declarative rather than adding an imperative restart
verb — which matters, because the owner's position is that an `app restart`
command is normally a symptom rather than a feature.

Two things to get right. The hash must cover the *resolved values*, not the
Secret's `resourceVersion`, or an unrelated write to the Secret rolls every
application referencing it. And the roll must respect the same destructive-change
gating everything else does, so a rotation cannot bypass a MigrationPlan.

Ships with a walk step that seals a new value and asserts the running pod picks
it up without any manual action. No current walk rotates a secret.

**Release chain:** this is operator behaviour, so it carries the full
operator → `appVersion` → platform-stack → compatibility chain. It was
deliberately kept out of the 2.20 documentation track for that reason.

## The CLI cannot answer the question its own error asks

**Opened:** 2026-08-30 (2.20c, correcting `docs/dev-guide/secrets.md`).
**Status:** OPEN.
**Severity:** medium. Every diagnosis path for the most common secrets mistake
leaves the CLI.

### What is wrong

When an env reference does not resolve, the operator reports
`EnvSecretMissing` and the condition message is deliberately ambiguous:

```text
env STRIPE_KEY -> secret "checkout-secrets/stripe-api-key": Secret
"checkout-secrets" not found or missing key "stripe-api-key"
```

"not found **or** missing key" names two causes and distinguishes neither. The
two follow-up questions are *where is the secret* and *what keys does it have* —
and `apprafter secret` ships only `seal` and `remove`. Neither question has a
first-class answer, so the guide reached for `kubectl get sealedsecrets
--all-namespaces` and a `go-template` over `.data`, three times across the page.

This is not the platform declining to own something. It is the platform raising
a question and then having no way to answer it.

**The demand was already recorded once.** ADR 0057's 2.19h amendment predicted
that the sealed-secrets guide would be unable to say what is sealed and in which
namespace without such a listing, and the 2.19j walk observed exactly that,
noting the guide answers both through `kubectl` — "honest, but a `kubectl`
answer on a page whose whole subject is a first-class CLI task". This is the
second observation of the same gap.

### Why it bites so often

`secret seal` and `secret remove` both default `--namespace` to
`apprafter-system`, which is right for platform credentials and wrong for every
application secret. Sealing into the wrong namespace is therefore the
single most likely way to arrive at `EnvSecretMissing` — and sealed secrets are
namespace-bound, so the recovery is to re-seal rather than move. The one mistake
the default invites is the one the CLI cannot help diagnose.

### The fix

A listing subcommand under `apprafter secret`, with an all-namespaces flag,
printing name, namespace and **key names** — never values — is enough to answer both questions and to collapse the
whole "telling a wrong namespace from a wrong key" section into one command.

Worth pairing with a smaller change that removes the need for it: have
`app status` name the resolved namespace it looked in when it reports
`EnvSecretMissing`, so the ambiguous half of the message becomes concrete
without a second command at all.

## A capacity warning nobody can receive

**Opened:** 2026-08-30 (2.20c, correcting `docs/operator-guide/shared-volumes.md`).
**Status:** OPEN.
**Severity:** medium. The signal exists, is correct, and reaches nobody.

### What is wrong

The operator stamps a `CapacityWarning` condition on the `SharedVolume` CR and
emits an edge-triggered Warning Event when a volume's node is nearly full. Both
are the platform telling an operator to act before a disk fills.

No CLI surface reads either. `apprafter volume status` prints the sampled
`Used`/`Free` bytes and nothing about the warning; `apprafter app status` says
nothing about SharedVolumes at all. The guide names this itself, and then hands
the reader two `kubectl` reads — one for the condition, one for
`describe … | Events` — as the only way to receive a warning the platform went
to the trouble of raising.

This is the same class as the `EnvSecretMissing` diagnosis gap recorded above:
the platform opens a loop it cannot close. It is worse in one respect — that
one is reached only after something has already gone wrong, while this one is
the *early* warning, so a signal that reaches nobody costs the lead time it
exists to buy.

### The fix

`apprafter volume status` is already the command an operator runs to look at a
volume, and already prints the sample the condition is derived from. Printing
the condition beside it — the status, the reason, and the timestamp — closes
the loop with no new noun and no new command to learn.

Worth doing at the same time: `apprafter app status` says nothing about
SharedVolumes even for an application that mounts one, so an application-side
reader has no path to the warning at all.

## There is nothing to roll a moving tag back to

**Opened:** 2026-08-30 (2.20c, correcting `docs/dev-guide/image-iteration.md`).
**Status:** OPEN.
**Severity:** medium. The command a developer reaches for after shipping a bad
build does nothing in the case they are most likely to be in.

### What is wrong

The platform resolves a moving tag to a digest and re-rolls the workload when
that tag moves (ADR 0040) — which is the feature, and it means a developer can
ship by pushing `:latest` and never touch the manifest.

`apprafter app rollback` patches `spec.source.targetRevision` on the Argo CD
Application: it rolls back the **Git revision**. After a same-tag push the Git
revision has not changed, so there is nothing for it to roll back to and the
command achieves nothing. `git revert` has the same problem for the same reason.

Nothing retains the digest that was previously resolved — a grep for a
prior-resolution field across `operator-core` and the Application controller
finds none — so even a rollback that wanted to act on the image has no target.

The guide documents this accurately and hands the reader `kubectl rollout undo`,
noting that the operator owns the Deployment and re-resolves on its next pass,
so the undo holds only until then. That is true, which makes it a workaround
with an expiry measured in seconds.

### What it costs

The failure lands at the worst moment. A developer who has just shipped a
broken build runs the command named `rollback`, sees it succeed, and is no
better off — and the one remedy that does work is temporary by construction.
The durable answer the guide gives is to switch the manifest to a hand-pinned
digest with `imagePolicy.resolve: off`, which means abandoning the auto-deploy
loop to recover from it.

### The fix

Retain the previously-resolved digest — one field on `Application.status`
alongside the resolution the operator already performs — and let
`app rollback` fall back to it when the Git revision is unchanged. That makes
the command mean what its name says in the case the platform's own headline
feature creates.

Ships with a walk step that pushes a second image to the same tag and asserts
the rollback returns the workload to the first.

