# Day-2 follow-ups — product defects the 2.19j walk documented rather than fixed

Found while correcting the day-2 operations pages (2.19j). Each one is a
**product change**, not a documentation change: the pages now describe
the behaviour as it actually is, and these entries record what would
have to move for the description to go back to the intended one.

Internal working data — this tree is excluded from the site
(`exclude_docs` in `mkdocs.yml`) and from the documentation gate
(`docs/measurements/` is in `EXCLUDED`, `cli/docsgen/src/scan.rs:96`).

Entries are numbered `D<n>` so they can be referred to without quoting a
title. Numbers are permanent: a resolved entry keeps its number and its
text rather than being deleted, because what was wrong and what it cost
is the part worth having later.

| | Entry | Status |
| --- | --- | --- |
| **D1** | VPA in-place right-sizing has never run: wrong feature-gate name | RESOLVED |
| **D2** | `--cron` / `--check-cron` are the wrong surface, not an unvalidated one | open (docs half landed, CLI half did not) |
| **D3** | Two diagnostics whose `help:` text describes a layout that moved | RESOLVED |
| **D4** | Removing a `needs.*` entry orphans its ResourceClaim forever | open — high |
| **D5** | A Dragonfly restart drops every claim's ACL user | open — high |
| **D6** | Rotating a secret does not take effect until something else restarts the pods | open — high, security |
| **D7** | The CLI cannot answer the question its own error asks | open |
| **D8** | A capacity warning nobody can receive | open |
| **D9** | There is nothing to roll a moving tag back to | open |
| **D10** | The applying half of right-sizing has never been observed to apply anything | open — high |
| **D11** | 584 failures share one catch-all, and the cheap checks run last | open — high |
| **D12** | Removing `expose` leaves the Service behind | open — medium |
| **D13** | A registry credential copy that nothing ever reclaims | open — high, security |

## D1. VPA in-place right-sizing has never run: wrong feature-gate name

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
the pinned 32Mi floor, and the mirror into `Application.status`.

**It did not confirm that anything was applied to a pod**, though this
entry and `plan-history.md` both said so. The apply-observation's test is
`[ "$REQ" = "$RECO" ]`, and the seed request and the recommender's pinned
floor are both 32Mi — so a pod the updater never touched passes it
identically. The walk's own comment concedes the possibility ("seed may
already match"). That is **D10**, and it is the same shape as the defect
this entry records.
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
- `Application.status.recommendedResources.notApplied` is **never set**.
  The only production call site passes `infeasible: false` outright
  (`operator-controllers/application/src/lib.rs:870`, with the comment
  above it deferring the probe to "a follow-up"), and the field is
  populated only when that flag is true (`:1382`) — so the whole
  downstream path is built and dead: the CRD field, the operator type,
  and the CLI's ` · not applied — {reason}` rendering. An earlier version
  of this bullet said it was set for node-capacity infeasibility; that
  was wrong. Tracked as **D10**.

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

## D2. `--cron` / `--check-cron` are the wrong surface, not an unvalidated one

**Opened:** 2026-08-20 (2.19j, correcting
`docs/operator-guide/backup-restore.md`).
**Reframed:** 2026-08-30 — the original entry proposed adding a
`value_parser`. That treats the symptom. The flags should not take a cron
expression at all.
**Status:** OPEN. **The documentation half landed and the CLI half did
not**, which is worth stating because that split is how an entry gets
forgotten: `backup-restore.md` no longer offers `--check-cron off`, says
plainly that there is no off switch today, and uses a never-firing
schedule instead — so the symptom stopped being visible while the defect
stayed. Re-verified 2026-08-30: both flags are still
`#[arg(long, value_name = "cron")]` with no `value_parser`
(`cli/platform-cli/src/cli.rs:1560`, `:1588`).

### What they do

Nothing but pass a string through. The value is threaded into
`spec.backup.schedule` / `spec.backup.checkSchedule` — declared bare
`string` in `schemas/v1alpha1/platformstack.cue:76,87` with no pattern —
and written **verbatim** into the CronJob's `schedule:` field
(`platform-stack/cue/render_tool.cue:453`, `:538`). Defaults are
`0 3 * * *` nightly and `0 6 * * 0` weekly for the integrity check.

### Why a cron expression is the wrong thing to ask for

**The expressiveness is unwanted.** An operator wants to say *when* — a
time of day, perhaps a day of week — and possibly *how often*. Steps,
ranges, minute-granularity: none of it means anything for a nightly
backup.

**And it is already being misused.** There is no off switch for the
weekly check, so the page now teaches `--check-cron "0 6 31 2 *"` — the
31st of February, a date that never arrives. That is not a schedule; it
is a hack standing in for a missing `--check off`, and the documentation
teaches it as the supported answer.

**The timezone is unset and unnamed.** `timeZone` appears nowhere — not
in the chart, not in the schema, not in the CLI — and no page says
"UTC". A CronJob without it runs in the kube-controller-manager's
timezone. An operator writing `0 3 * * *` means three in the morning
*their* time, and has no way to learn what the three means. That is a
correctness trap, not an ergonomic one, and no amount of syntax
validation catches it.

**A typo takes the platform down, not the command.** The two crons are
the only options in `backup enable` with no client-side check — unlike
`--enforce` and `--staging-mode` (`commands/backup.rs:1732-1745`) — so a
bad value travels to the apiserver and fails the platform-stack sync:

<!-- docs: check=none reason=third-party-output since=v0.2.51 — the apiserver's own rejection, quoted -->
```console
$ kubectl apply -f apprafter-backup-check.yaml     # schedule: "off"
The CronJob "apprafter-backup-check" is invalid: spec.schedule: Invalid value: "off": expected exactly 5 fields, found 1: [off]
```

`--check-cron off` was offered three times on the backup page before
2.19j, and is implemented nowhere — no `"off"` literal exists in
`commands/backup.rs` — so every operator who followed that instruction
broke their platform sync.

### The fix

Replace the surface rather than validate it:

- **`--at 03:00`** for the time of day, defaulting to **the operator's
  own timezone** — read from the machine running the command — with
  `--timezone` to override it. A time without a zone is not a time, and
  the zone the operator means is the one they are in; UTC is a
  reasonable thing to ask for and a poor thing to assume. The CLI
  composes the cron and sets `spec.timeZone` on the CronJob, so neither
  is the operator's problem, and `backup status` prints the schedule
  back in the same zone it was given.
- **Frequency stays a product decision.** Daily for the backup, weekly
  for the check. If a second value is ever needed it is
  `--every day|week`, not a cron field.
- **`--check off` as a real branch** — a `checkSchedule: ""`-means-omit
  path in the chart — so disabling the check stops requiring a date that
  never comes.

Keeping a raw-cron escape hatch is defensible later; it must not be the
only surface, and it must not be the one the guide teaches.

## D3. Two diagnostics whose `help:` text describes a layout that moved

**Opened:** 2026-08-20 (2.19j, correcting
`docs/operator-guide/troubleshooting.md`).
**Status:** RESOLVED 2026-08-30. Both help texts corrected in
`cli/cli-core/src/error.rs`, each with a test pinning the fix and its
negative.

The entry deferred these because "changing a shipped diagnostic's help
is a CLI release". That was true when written and stopped being true
once 2.20a put two CLI fixes on the same release — the cost argument
had quietly expired, and nothing re-examined it. Worth noting as a
pattern: a deferral justified by a cost should name what makes the cost
go away, or it outlives its reason.

**A test was holding the second one in place.**
`server_type_not_selected_has_stable_code_and_actionable_help` asserted
the help contains `nodes[0].kind` — the wrong string, the Rust field
name. So the defect was not merely unnoticed; it was defended on every
run. That assertion now checks the durable part of the hint and the
specific field name is pinned, positively and negatively, by a test of
its own. **A test that pins the wrong string is worse than no test.**

1. **`apprafter::state::corrupt`** (error.rs:152-155) says "The local
   `.apprafter/state.json` file … delete `.apprafter/`". State moved to
   `<config-root>/state/<target>/.apprafter/state.json` in **v0.1.154**
   (`cli/cli-state/src/state.rs:10`, `commands/state_paths.rs`); the
   per-cwd file is a legacy artefact that is migrated once. An operator
   following the help deletes a directory that is not the problem, and
   the error keeps firing. The summary line does print the real path, so
   the fix is to make the help point at it rather than at a fixed
   location.

   **Fixed better than proposed.** The entry suggested pointing the help
   at the real path. Since 2.20a there is a repair that needs no path at
   all: `apprafter import --force` moves the unreadable file aside and
   rebuilds from live resources, so the help now names that and says the
   file is preserved rather than deleted.

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

   Now reads `spec.nodes[0].type`.

## D4. Removing a `needs.*` entry orphans its ResourceClaim forever

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

### The root cause: an ownership relation modelled as a lifetime binding

*Added 2026-08-30, after the project owner named the principle: whoever creates
a resource owns it, and owning it means being able to change **and delete** it.*

Two different things were treated as one.

An `ownerReference` is a **lifetime binding** — "this child dies with its
parent" — and it is executed by the apiserver's garbage collector, not by us.
**Ownership** is "the parent decides whether this child exists at all", and only
a controller can execute that. Every generated claim carries
`controller: true, blockOwnerDeletion: true` (`lib.rs:2645-2653`), and that was
taken to have settled the question. It answers what happens when the parent
dies. It never answers what happens when the parent no longer wants the child.

This is why the gap looked closed. The one deletion scenario anyone ever
exercised — deleting the whole Application — is performed **entirely by the
apiserver**, and performed correctly: it stamps a `deletionTimestamp`, which
fires the provisioner finalizer, which writes the `RetainedClaim`, which starts
the seven-day clock, which the reaper eventually collects. That whole chain is
sound. Exactly one link is missing, the first one: **the operator never
deletes.** A finalizer should be an optimisation layered on top of ownership;
here it is the only path, and it is driven by somebody else's code.

The consequence is that removal is not a property of the reconcile loop at all.
It is handled per-child, one child at a time. The Application controller can
create **eight** kinds; here is where each stands:

| child of `Application` | created when | removed when no longer wanted |
| --- | --- | --- |
| Deployment | always | n/a — always desired |
| Service | `expose` is set | **no** — D12 |
| CiliumNetworkPolicy | Cilium present | **no** (`lib.rs:736-740`, no `else`) |
| HTTPRoute | `expose.network: public` | yes — `prune_http_route` (1.83b) |
| VerticalPodAutoscaler | managed env | yes — `prune_vpa` (2.16e) |
| ResourceClaim | each `needs.*` entry | **no** — this defect |
| MigrationPlan | a gated change | yes — LIST-driven, `lib.rs:1974-1995` |
| pull-secret `Secret` copy | private image | **no**, and no ownerRef — D13 |

**Neither prune arm was reactive.** This was the investigation's most
uncomfortable finding, and it moves the diagnosis: this is not a class nobody
thought of.

- `prune_http_route` shipped in the **same commit** as the apply it balances
  (3abd6d7, 1.83b, 2026-06-13 — `git log -S"apply_http_route"` returns that one
  commit), pre-specified in the design doc written that morning.
- `prune_vpa` was pre-specified even harder: filed as hazard **H7** by external
  design review round R2, written into a locked decision, and supplied verbatim
  as a fenced Rust block in the plan's Task 4 — the shipped function is
  character-for-character the planned one.

And 2.16e's spec states the rule **in the abstract**, quoting the doc-comment it
wanted copied: *"on the exact precedent of `prune_http_route` … same 404-is-OK
shape + doc-comment '**child must disappear when the app stops qualifying**'"*.

So the general rule was written down, twice, five months apart, by two different
reviews — and applied each time to exactly the one child in front of the author.
That is the process failure, and it is not inattention. It is that **nothing in
the repository holds the rule**: it lived in two design documents and one
doc-comment, none of which any later change is obliged to read.

Two mechanical facts about why the existing arms could not have spread on their
own. Both prune arms are single `delete(name)` calls against a **singleton at a
name derived from the app's name**; claims are a **set of variable size with
data-dependent names** (`<app>-<type>[-<name>]`), so the shape does not reach
them. But that is only an argument about reuse, not an excuse: the Service is a
singleton at a fixed name with `delete` already in its RBAC, and it has no arm
either (D12). The discriminator is not difficulty.

### What the controller does and does not see

An earlier version of this entry said the orphan is *unobservable*. That is
wrong, and the truth is worse.

The controller process **does** watch every ResourceClaim in the cluster:
`lib.rs:145` builds `Api::<ResourceClaim>::all(...)` and `:162` passes it to
`.owns(claims, watcher::Config::default())`. kube-runtime maps an owned object
back to its owner through `metadata.ownerReferences`, and the orphan still
carries the controlling ownerRef the operator stamped on it, pointing at a
still-live Application. **The orphan's own events therefore re-enqueue its owning
Application.** The operator wakes up *because of* the orphan.

What is discarded is the observation. `Controller::run` hands `reconcile` only
the `Arc<Application>`, and every live-side read in the reconcile body is
**name-addressed off the projected desired state** — `claim_api.get(claim_name)`
(`:517`), `api.get_opt(&name)` in `apply_deployment` (`:1003`), the VPA read at
`:865`. The correct statement is: *the reconcile body never reads a child it did
not just render*. Note what the variable is called at `:513-517` — `current`,
built by GETting exactly the names `generate_resource_claims` just produced. It
is the desired set wearing the name of the live set.

### The fix

Not "a third prune arm in the same shape" — that phrasing was in an earlier
version of this entry and it will not work. `has_needs` (`lib.rs:504-505`) gates
the entire needs block on the NEW spec, so removing the **last** need skips the
block wholesale and any `else` arm inside it never runs. The prune must sit
outside that gate.

The generic form is what the ownership principle asks for: LIST the children this
Application owns and delete those absent from the desired set. Generated claims
carry no labels (only name, namespace, `ownerReferences`), so the predicate is
the owner UID — the same one `list_resource_claims_for_app` already uses, and the
same one the `.owns()` watch is already routing on. Four LIST-then-delete sites
exist in the operator to copy from, of which `platform-stack/src/reconcile.rs:685-696`
is the only one that computes a real keep-set at runtime; `delete_all_key_plans_except`
(`application/src/lib.rs:1974-1995`, pure filter at `:2304-2324`) is the closest
in-crate model.

Ships with: the missing `delete` verb on `resourceclaims` in `rbac.yaml`; the
ordering constraint below; and the walk step.

Ordering matters and is the whole risk: the delete must happen only after the
plan is approved and the new spec applied, so an unapproved edit cannot destroy
anything, and the provisioner finalizer must still run so the snapshot is
written and the seven-day window opens as documented.

Ships with: the missing `delete` verb in `rbac.yaml`, a unit test on the
desired-set diff, and a walk step that removes one `needs` entry from a
two-need app and asserts a `RetainedClaim` appears while the other claim is
untouched. Without that last one the class stays invisible exactly as it is now.

## D5. A Dragonfly restart drops every claim's ACL user

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

### Verified 2026-08-30 — ADR 0042's revisit condition is met

ADR 0042 rejected this design twice, at `:25` ("conflicting signals on whether
that file can carry key/channel patterns") and at `:152` as a named alternative:
*"Dragonfly's ACL-file support for key/channel patterns is ambiguous … Revisit
if file-based ACLs are officially confirmed."* An audit against upstream source
at the pinned versions resolves the ambiguity. **So this is ADR-first work: the
0042 amendment comes before the code.**

**The parser is the same one.** At dragonfly `v1.37.0`,
`AclFamily::LoadToRegistryFromFile` (`acl_family.cc:289`) parses each line with
`ParseAclSetUser` (`:318`) — one definition (`:1056-1156`), exactly two call
sites, the other being `AclFamily::SetUser` (`:142`), which backs runtime
`ACL SETUSER`. Both file entry points route through it: startup `Load()`
(`:355-357`) and the runtime `ACL LOAD` command (`:361-368`). There is no
separate, weaker file parser. `$N`, `~*`, `&user:*`, `resetkeys` and
`resetchannels` therefore parse identically in both paths.

**Three qualifications that "identical by construction" glosses over.**

1. The file path adds a layer with no runtime counterpart:
   `MaterializeFileContents` (`:701-722`) splits on `\n` then on a **single
   space**, and requires every non-empty line to begin with the literal token
   `USER` and carry at least four tokens. So the file grammar is
   `USER <name> <rules…>`, and double spaces or a tab are a malformed line, not
   whitespace. The builder must emit exactly single-space-joined lines, and the
   byte-comparison test in step 1 below has to compare against *that* form, not
   against the `SETUSER` argv.
2. File mode passes `hashed=true`, which only **adds** the `#<sha256hex>`
   password form; our `>plaintext` form is unaffected (`:822`, the `>` branch is
   unconditional). Safe direction. Noted only because if we ever pre-hash, an
   invalid hex string is not an error — `user.cc:92-102` logs and inserts no
   password, silently yielding a user with no credential.
3. Runtime additionally passes the existing user's `has_all_keys`; the file path
   takes the default. Affects only the "pattern after `*`" guard
   (`:1072-1078`), which our vector does not trip.

**`aclFromSecret` exists and is usable, with one trap.** At dragonfly-operator
`v1.5.0` (`api/v1alpha1/dragonfly_types.go:56-59`) it is a
`corev1.SecretKeySelector` — Secret name plus a **caller-chosen key**. The
operator (`internal/resources/resources.go:191-213`) turns it into a Secret
volume `dragonfly-acl`, projects the chosen key to the filename
`dragonfly.acl`, mounts it at `/var/lib/dragonfly`, and appends
`--aclfile=/var/lib/dragonfly/dragonfly.acl` itself.

The trap: **`optional` is accepted by the CRD schema and silently ignored** —
`resources.go:195-204` never sets `SecretVolumeSource.Optional`, so it stays
nil, which means *required*. A CR whose `aclFromSecret` names a Secret that does
not exist yet gives a pod that cannot start. **The Secret must be written before
the CR gains the field**, and `provision_dragonfly`'s current ordering must be
checked against that.

Also worth pinning down separately: the Dragonfly **server** image is not
pinned by this platform at all. `dragonfly_object` emits no `spec.image`, the
chart's `dragonflyImage` default is empty and platform-stack does not override
it, so the tag resolves to the operator's compiled-in
`internal/resources/version.go` default, today `v1.37.0`. Every ACL fact above
is a fact about a tag we do not control.

### The concurrency risk, corrected

An earlier draft of this entry treated "derive the whole file from a fresh LIST"
as sufficient. It is not, and the reason is specific.

The provisioner reconcile (`main.rs:247`) and the ACL resync loop
(`main.rs:287`) are **separate tokio tasks**. The 2.6 dbnum allocator race —
commit `7890d9e`, which caused a real isolation breach — was fixed by
**serialisation, not atomicity**: a single added line,
`.with_config(ControllerConfig::default().concurrency(1))`. That serialises the
controller against itself. It does **not** serialise the controller against the
resync loop, so these two writers do race.

And the dominant window is not a stale snapshot. `claims_to_repin`
(`acl_reconcile.rs:94-125`) requires `status.ready == true`, while the ACL user
is created at `reconcile.rs:655-657` — **before** the terminal ready apply at
`:704`. So a concurrent whole-file derivation drops the in-flight claim's line
*even if it LISTs strictly after the provisioner wrote the file*. A
resourceVersion precondition does not close that; the filter does not see the
claim yet.

The damage is also delayed and therefore hard to catch: a dropped line breaks
nothing at the time, because the user is alive in memory. It breaks at the next
restart, arbitrarily later, for reasons that look unrelated.

Nor can per-key SSA ownership save us — `Secret.data` is a granular map under
SSA, but `aclFromSecret` selects **one key** projected to one file, so the whole
file is a single key with a single owner.

The design that survives this: **one writer.** The resync loop owns the file;
`provision_dragonfly` and `gc_drop_dragonfly` keep doing their imperative
`ACL SETUSER` / `DELUSER` and do not touch the Secret. Durability then lags
liveness by at most one tick, which is the correct tradeoff — the file is the
restart-survival mechanism, not the grant mechanism. `ACL LOAD` being available
is a bonus the loop can use to converge a running instance to the file.

### The e2e story, corrected

An earlier version of this entry said: *"Ships with a walk step that restarts
the pool instance … No current walk restarts an instance, which is why this was
never caught."* **The second sentence is false**, and the truth is more
interesting.

`e2e/needs-redis-walk.sh` restarts a Dragonfly pool instance **twice**, by
deterministic StatefulSet pod name — `:1203` kills the ephemeral instance in
Phase 9 and `:1307` the persistent one in Phase 10 (added 2026-06-05 in
`cf398e3`). Phase 9 then asserts a tenant re-authenticates after the restart.

It asserts it with a **420-second deadline** (`:1218-1231` — one
`RESYNC_INTERVAL` plus slack), under a comment at `:1193-1194` that ratifies the
outage in so many words:

> the transient failure window between Ready and re-pin is racy, so we do not
> assert it; eventual recovery is the acceptance criterion

So the defect is not uncovered by the walk. **It is codified as passing by it.**
The walk saw the outage, decided it was acceptable, and wrote that decision into
the assertion — which is the third instance in this file of the same shape, after
D1's `[ "$REQ" = "$RECO" ]` and D3's test pinning the stale help text.

The delta is therefore one word, not one step: Phase 9's assertion tightens from
*recovers within 420s* to *never lost authentication at all* — poll `PING` as
the tenant across the restart and require no `NOAUTH`/`WRONGPASS` window beyond
the pod's own unreadiness. Phase 10 gets the same for the persistent instance,
where the sharpest form lives: the keyspace comes back from the snapshot and the
ACL table does not, so the tenant is locked out of data that is intact.

Ships with: the ADR 0042 amendment; the tightened Phase 9/10 assertions; a test
that the file line and the `SETUSER` argv are the same grant; and an assertion
that the provisioner can still reach the instance as `default` after the file is
in place — the lockout in step 2 above is the one failure here that has no
recovery short of deleting the CR.

## D6. Rotating a secret does not take effect until something else restarts the pods

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

## D7. The CLI cannot answer the question its own error asks

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

## D8. A capacity warning nobody can receive

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

## D9. There is nothing to roll a moving tag back to

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

## D10. The applying half of right-sizing has never been observed to apply anything

**Opened:** 2026-08-30 (re-reading **D1** against the tree).
**Status:** OPEN.
**Severity:** high. `InPlace` behaves correctly by construction and by
upstream source; what is missing is any evidence it has ever acted, and any
signal when it cannot.

### What is right, first

The mode choice is sound and was verified outside the repository, so this
entry is not "VPA is broken again":

- **`InPlace` is a valid `updateMode`** on the shipped chart —
  `helm show crds vertical-pod-autoscaler --version 0.11.0` gives
  `["Off","Initial","Recreate","InPlaceOrRecreate","InPlace","Auto"]` on the
  **v1** block. (The **v1beta2** block of the same CRD carries only
  `[Off, Initial, Recreate, Auto]`, so the `autoscaling.k8s.io/v1` string at
  `operator-rendering/src/lib.rs:774` is load-bearing and nothing in-tree
  asserts it.)
- **`InPlace` never evicts**, by explicit upstream construction at tag
  `vertical-pod-autoscaler-1.7.1`, in three places in `pkg/updater`: the mode
  branch never populates `podsForEviction`; on an actuation error it logs,
  records a failure and `continue`s, where `InPlaceOrRecreate` falls back to
  eviction; and the restriction layer's deferred/in-progress timeout →
  `InPlaceEvict` path is gated to `InPlaceOrRecreate`.

So the property the mode was chosen for — a request change without a restart
— holds. The rest of this entry is about not being able to tell whether it
ever happens.

### What is wrong

**The apply-observation is degenerate.** `e2e/vpa-walk.sh:213-219` tests
`[ "$REQ" = "$RECO" ]`. The rendered seed request is 32Mi
(`operator-rendering/src/lib.rs:333`) and the recommender's memory floor is
pinned to 32Mi (`platform-stack/cue/component_vpa.cue:166`), and the recorded
0.2.57 run observed exactly 32Mi on both sides. **A pod the updater never
touched satisfies that equality.** The walk concedes it inline ("seed may
already match") and the check is soft, but the result was written up as
"resized in place" in both `plan-history.md` and D1 above.

**It also reads the wrong field.** `spec.containers[].resources.requests` is
what the updater *patches* — the desired value. What the kubelet actually
actuated is `status.containerStatuses[].resources`, and the in-flight and
blocked states are the `PodResizePending` / `PodResizeInProgress` pod
conditions. The walk reads none of the three, so even a moved `spec` would
prove the updater patched, not that the resize landed.

**The failure mode of the mode we chose is the one nothing reports.** Because
`InPlace` defers instead of evicting, an infeasible upward resize is silent
and indefinite. ADR 0054 anticipated exactly this and specified a
`recommendation not applied — node capacity` signal on `Application.status`.
The signal is dead: the only production call site hardcodes
`infeasible: false` (`application/src/lib.rs:870`), the field is populated
only when that is true (`:1382`), and the CRD field, the operator type and
the CLI rendering are all built and unreachable.

**And the Kubernetes side is unpinned and unprobed.** In-place pod resize is
itself a Kubernetes feature. ADR 0054:13 says it is GA "on the **pinned** k8s
v1.35" — nothing pins k8s: `build_k3s_user_data` installs stable-channel k3s
with no `INSTALL_K3S_VERSION`, which `quickstart.md` states outright. It is
benign today because the gate is on by default at the versions the stable
channel serves, but that is an accident of upstream defaults rather than
something the platform arranges or would notice changing. The contrast is
sharp: the swap path *does* preflight the kubelet version
(`node_prep.rs`, `k8s_ge_134`); the VPA prerequisite does not.

### The fix

1. **Make the walk able to fail.** Seed the app above the floor — an explicit
   large request, or a workload that allocates — so the recommendation and the
   seed genuinely differ, then assert on `status.containerStatuses[].resources`
   rather than `spec`, and hard-fail rather than log. Without a differing
   pair the assertion cannot distinguish success from no-op, which is what it
   currently does not.
2. **Emit the deferred signal.** Wire the `notApplied` probe the ADR
   specified and the whole downstream path already implements — read the
   VPA's own in-place condition rather than inferring node capacity, since
   upstream now reports it.
3. **Preflight the Kubernetes prerequisite** the way the swap path already
   preflights its own: a version and feature check at bootstrap, and a walk
   assertion, so an unpinned upstream moving under us is a finding rather
   than a silence.

### The shape worth carrying

D1 was a component that never ran while every gate reported green, because
the walk asserted only the half that still worked. The guard added for it
asserts the controllers are *up*. This entry is the next layer of the same
thing: the controllers are up, and the assertion that they *do* anything
passes whether they do or not.

A walk that cannot fail is not evidence. Both times the tell was the same —
an assertion whose success condition is satisfied by the null case.

## D11. 584 failures share one catch-all, and the cheap checks run last

**Opened:** 2026-08-30, from a transcript the project owner hit:

```text
$ apprafter backup list
> Backup passphrase: ********
Error: apprafter::cli::other
  × spawn restic: No such file or directory (os error 2)
```

**Status:** OPEN.
**Severity:** high. Two defects meet in that transcript, and neither is the
one the error text suggests.

### Part 1 — the taxonomy has been overtaken by the code

`cli/cli-core/src/error.rs` defines 14 variants: 13 typed plus
`Other(String)`. Every one carries a stable `code(apprafter::*)` and
multi-line `help(...)` — **diagnostic coverage is 100%**. The gap is not
variants without help; it is failures without a variant.

Measured across the operator-facing crates (excluding `docsgen`, `tests/`
and `#[cfg(test)]` blocks):

| | |
| --- | --- |
| `CliError::Other` construction sites | **584** |
| production source files | 112 |
| files that construct the catch-all | 57 |
| files that construct a typed variant | ~8 |
| backup + restore subsystem | **181 catch-alls, zero typed variants** |

The two variants that exist for exactly the right purpose —
`ProviderApiUnreachable` and `ProviderTokenRejected` — are wired into **two
call sites each**, both on the `target add` ping. Every other provider
transport failure (DNS, TLS, refused, proxy, timeout) is one of 17
identical `transport error talking to {endpoint}` catch-alls.

There is no typed variant for: a missing external binary other than `cue`;
any `kubectl` / `helm` / `restic` subprocess failure; cluster reachability,
RBAC, or a missing CRD; prompt cancellation or a non-TTY shell; a missing
Kubernetes object; or anything at all in backup and restore.

**The clearest single symptom:** `docs/operator-guide/quickstart.md` documents
the catch-all string as expected output — *"Without `kubectl`, `apprafter`
fails with `× spawn kubectl: No such file or directory (os error 2)`"*. When a
documented UX is a catch-all error, the taxonomy has stopped describing the
product.

### Part 2 — the cheap check runs after the expensive step

The reported transcript is not only an untyped error. **The operator typed a
secret into a command that could not have worked**, because the passphrase
prompt runs before anything checks that `restic` exists. Ordered by what the
inversion costs:

1. **`restore --reprovision`** — the sharpest, because the instinct was
   right and stopped one rung too high. Credentials *are* gated first,
   deliberately, with a comment at `restore.rs:126-133`: "a bad passphrase
   must not leave a freshly re-provisioned cluster half-restored." It gates
   the passphrase and not the binary. The `Reprovision` step then runs a full
   billable Hetzner provision plus bootstrap, and the first `restic` spawn
   happens in the step after it — so a missing binary costs a paid, running
   cluster before anyone notices.
2. **`bootstrap-all`** — phase 1/3 creates billable resources; phase 3/3 is
   the first code to need `helm`, and performs no binary preflight. The
   provider layer already learned this exact lesson one level down
   (`provider.rs:301-334` validates the SKU against the live catalogue before
   any create, because "a retired type here used to fail mid-apply at step 4,
   leaking SSH-key + network + firewall state").
3. **`repo creds add`** — the wizard collects a friendly name, URL prefix,
   auth type, username and finally a **production PAT**, then discovers a
   duplicate name, an absent kubectl, an unreachable cluster or a missing
   sealed-secrets controller. `rotate`, in the same file, resolves the cluster
   first: the correct pattern is a sibling function away.
4. **`backup create`** — prompt, then kubeconfig, then kubectl, then restic.
   An operator with no cluster types a secret and is then told there is no
   cluster.
5. **`backup list`** — the reported case.
6. **`backup prune` / `check` / `unlock`** — all three round-trip to the
   cluster before spawning restic, and `check`/`unlock` are explicitly built
   to work with `--repo` and no cluster at all, because verifying a repository
   happens when the cluster is gone. These are the outage commands; the
   missing-binary check matters more there, not less.
7. **`doctor`** — inverted the other way. With no active target it returns a
   catch-all at `doctor.rs:183` and never reaches `build_env_checks()`, so a
   first-run user — the audience its own module docstring names — is told
   nothing about kubectl, helm, ssh or DNS. The environment half depends on no
   target and should always print.

### Part 3 — `doctor` does not cover what the CLI needs

| binary | spawn sites | fatal | doctor checks |
| --- | --- | --- | --- |
| `kubectl` | 36 | yes | yes — **WARN only** |
| `restic` | 8 | yes | **no** |
| `git` | 7 (3 fatal) | partly | **no** |
| `helm` | — | yes | yes — WARN only |

**A missing `kubectl` exits 0.** `check_tool` returns a WARN, `has_failures()`
counts only FAIL, so `doctor` prints *"Ready to go; review warnings if they
apply to your use case"* — while the quickstart calls kubectl and helm "not
optional". A unit test pins the WARN behaviour; nothing asserts a FAIL. The
inline justification is a developer-workflow argument applied to an
operator-facing tool.

`restic` is worse than unchecked: it is absent from the quickstart's
prerequisites entirely, while `kubectl` and `helm` are named. A good preflight
exists — `preflight_restic_version` at `backup.rs:2142`, which even has a
`NotFound` branch — and is called from exactly one command, `backup enable`.

### The fix

Three moves, in this order, because each makes the next cheaper:

1. **`preflight_tool(...)` at the top of every command that spawns one**,
   before any prompt, any cluster round-trip and any billable step. This is
   the whole of the reported bug and it is mechanical: the check exists, it is
   simply not called. Ship it with a test that asserts, per command, that no
   prompt or provider call precedes it.
2. **`ExternalToolNotFound { tool, needed_by, min_version }`**, code
   `apprafter::env::tool_not_found`, with install lines per platform and the
   sentence that matters after a prompt: *nothing was sent anywhere and no
   passphrase was used*. Then promote the two recurring subprocess families —
   a `Kubectl` variant with a stderr classifier for the three shapes that
   actually recur (unreachable / forbidden / CRD missing), and a `Restic`
   variant that separates a wrong passphrase from a broken repository, since
   today both are one raw-stderr catch-all.
3. **Make `doctor` load-bearing**: FAIL rather than WARN for a binary the CLI
   cannot work without, add `restic` and `git`, run the environment half
   unconditionally, and derive the checked list from the binaries the CLI
   actually spawns so a new dependency cannot be added without appearing
   there.

584 is not a number to drive to zero. The catch-all is the right home for a
genuinely one-off failure; what it is not is the right home for the six
commands on the disaster-recovery path.

## D12. Removing `expose` leaves the Service behind

**Opened:** 2026-08-30, found by applying the ownership principle recorded in
D4's root-cause section to the other children of `Application` rather than to
claims alone.

**Status:** OPEN.
**Severity:** medium — no data or cost is stranded, but a developer who removes
`expose` has said "stop serving this" and the cluster keeps a live Service with
its ClusterIP and its pod selector intact.

### What is wrong

`operator-controllers/application/src/lib.rs:726`:

```rust
if let Some(service) = &rendered.service {
    apply_service(&ctx.client, &namespace, service, &pp).await?;
}
// no else
```

The renderer emits a Service if and only if `expose` is set
(`operator-rendering/src/lib.rs:184` — `.map(|expose| render_service(...))`), so
the Service is an optional child exactly like the HTTPRoute and the VPA. Those
two have an `else` arm that deletes the stale object. This one has nothing, and
`git log -S"prune_service" -- operator/` is empty: no such function has ever
existed.

### Why this one is the clearest evidence for D4's root cause

An earlier version of this section said the Service "was left behind" because
the fix was scoped to the incident. The archaeology found something sharper, in
two documents that contradict each other.

**The 1.83b design spec asserted the Service already prunes.** From
`docs/superpowers/specs/2026-06-13-1-83b-app-public-ingress-design.md:67`:

> When `network != "public"` (or unset) the controller does **not** apply an
> HTTPRoute and **prunes** any stale one (the app flipped public → internal) —
> **same prune discipline the Service already follows**.

It does not follow it. It never has.

**The implementer discovered that, wrote it down, and shipped anyway.** The
doc-comment on `prune_http_route` (`lib.rs:1171-1176`) is that discovery:

> Best-effort delete of a stale `HTTPRoute` named `name` (the app flipped
> `public → internal`, or removed `expose`). 404-tolerant … 1.83b — **the
> Service has no analogous prune**, but the design requires the route disappears
> when the app stops being public.

So the comment is a tombstone. Somebody checked the spec's premise, found it
false, recorded the finding in the one place nobody re-reads, and closed the
subphase. The Service already has `delete` in its RBAC
(`rbac.yaml:118-126`) — nothing blocked the six-line fix except that it was not
the thing being shipped that day.

This is D4's diagnosis in miniature, and it is the strongest argument in this
file for making the rule mechanical rather than documentary. The rule was in a
spec. The violation was in a comment. Both were true, both were visible, and
seventy-eight days later the Service is still there.

The CiliumNetworkPolicy at `lib.rs:737` has the same missing `else`. It is far
lower risk — the controller always threads `needs_targets`, so
`rendered.network_policy` is `Some` on every controller-driven render, and the
`None` arm belongs to the bare `render_application` entry point used by tests.
It is listed here so the audit is complete, not because it is known to bite.

### The fix

Ships with D4, because it is the same fix: either a third and fourth prune arm,
or the generic owned-children diff that makes prune arms unnecessary. Add a unit
test that renders with `expose`, then without, and asserts the Service is
deleted; and a walk step that removes `expose` from an exposed app and asserts
the Service is gone while the Deployment survives.

One caution for whoever implements it: deleting the Service is correct but is
also the first time this operator will remove a networking object a user might
still have DNS or callers pointed at. The delete belongs behind the same
destructive-change gate as a `needs` removal if `expose` removal is classified
destructive; check `is_destructive` before assuming it is not.

## D13. A registry credential copy that nothing ever reclaims

**Opened:** 2026-08-30, from the same ownership audit that produced D12 — this
is the third and worst instance of the class.

**Status:** OPEN.
**Severity:** high, security. A registry credential written into an application
namespace survives the removal of the private image, the deletion of the
Application, **and** the deletion of the `SourceCredential` it came from. Only
deleting the namespace reclaims it.

### What is wrong

`apply_pull_secret_copy` (`operator-controllers/application/src/lib.rs:1583-1604`)
projects the SourceCredential's derived `dockerconfigjson` into the app's
namespace so the kubelet can pull:

```rust
"metadata": {
    "name": name,
    "namespace": namespace,
    "labels": { "apprafter.io/managed-by": "apprafter" }
},
"type": "kubernetes.io/dockerconfigjson",
```

There is **no `ownerReferences` key at all**. Every other child this controller
writes carries a controlling ownerRef; this one carries a label and nothing
else. So unlike D4 and D12 — where the apiserver cascade at least covers
Application deletion — this Secret is not reclaimed even then.

Nor does the SourceCredential side reclaim it. `gc_derived_secrets`
(`sourcecredential/src/lib.rs:605-627`) runs on the credential's finalizer path
and deletes by `{SOURCE_CREDENTIAL_LABEL}={name}` — **a different label**, which
the copy does not carry — across `ARGOCD_NAMESPACE` and the credential's own
namespace, **neither of which is the app namespace** the copy was written to.
Two misses, independently sufficient.

### Why it probably happened, and why the naive fix is wrong

The copy is named `app_pull_secret_name(&cred_name)` — **after the credential,
not after the Application** (`lib.rs:1462`). It is therefore shared by every app
in that namespace pulling through the same credential. Adding a controlling
ownerRef to one Application would be actively harmful: deleting that app would
cascade-delete a Secret its neighbours still need, and their pods would start
failing `ImagePullBackOff` on the next node reschedule.

That is very likely the reasoning that left the ownerRef off — and it is sound
as far as it goes. What is missing is the other half: having correctly decided
that no single Application owns this object, nobody assigned it an owner at all.
A shared child needs a **reference count or a sweep**, not an ownerRef, and it
got neither.

### The fix

Two shapes, and the choice is a real one:

1. **Namespace sweep** — after the reconcile, LIST Secrets in the namespace
   carrying `apprafter.io/managed-by: apprafter` and typed
   `kubernetes.io/dockerconfigjson`, and delete any that no Deployment in that
   namespace references via `imagePullSecrets`. Keeps sharing, costs one LIST.
2. **Per-app copies** — name the copy after the Application and give it a
   controlling ownerRef, restoring the cascade and letting D4's generic
   owned-children diff cover it for free. Costs one Secret per app per
   credential instead of one per credential.

(2) is the smaller change to reason about and folds this defect into D4's fix
rather than adding a fourth special case; (1) avoids duplicating credential
material. Either way it ships with a test that deletes an Application and
asserts no `dockerconfigjson` Secret survives in its namespace — which is the
assertion that would have caught all three of D4, D12 and D13.

