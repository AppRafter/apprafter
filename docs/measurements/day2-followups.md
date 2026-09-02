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

Fixes are planned as **`plan.md` §2.22**, grouped by shared fix and release
chain rather than one entry at a time — several of these share a root cause and
would otherwise touch the same code three times.

| | Entry | Status | Planned as |
| --- | --- | --- | --- |
| **D1** | VPA in-place right-sizing has never run: wrong feature-gate name | RESOLVED | — |
| **D2** | `--cron` / `--check-cron` are the wrong surface, not an unvalidated one | RESOLVED 2.22g | 2.22g |
| **D3** | Two diagnostics whose `help:` text describes a layout that moved | RESOLVED | — |
| **D4** | Removing a `needs.*` entry orphans its ResourceClaim forever | FIXED 2.22b — walk-verified | 2.22b |
| **D5** | A Dragonfly restart drops every claim's ACL user | RESOLVED 2.22f — walk GREEN | 2.22f |
| **D6** | Rotating a secret does not take effect until something else restarts the pods | FIXED 2.22c — walk owed | 2.22c |
| **D7** | The CLI cannot answer the question its own error asks | FIXED 2.22c — walk owed | 2.22c |
| **D8** | A node-scoped warning published through an optional object | FIXED 2.22d — walk step still owed | 2.22d |
| **D9** | There is nothing to roll a moving tag back to | RESOLVED 2.22e — walk GREEN | 2.22e |
| **D10** | The applying half of right-sizing has never been observed to apply anything | FIXED 2.22d — walk rewritten, not yet run | 2.22d |
| **D11** | 584 failures share one catch-all, and the cheap checks run last | FIXED 2.22a | 2.22a |
| **D12** | Removing `expose` leaves the Service behind | FIXED 2.22b — walk-verified | 2.22b |
| **D13** | A registry credential copy that nothing ever reclaims | FIXED 2.22b — walk step still owed | 2.22b |
| **D14** | Re-sealing a secret performs a gated change through an ungated door | RESOLVED (decision + disclosure landed 2.22c) |
| **D15** | The destructive-change gate never engaged for a base-only app | RESOLVED |
| **D16** | A reconcile that fails leaves no trace outside the operator's log | open — high |
| **D17** | A Dragonfly tenant can read every other tenant's key counts | FIXED 2.22f — and it was larger than key counts | 2.22f |
| **D18** | Two git-ownership guards could never fire: kubectl hides `managedFields` | FIXED 2.22e | 2.22e |
| **D19** | The 2.22d size sampler deletes a live claim's whole allocation | FIXED 2.22f — same day, self-inflicted | 2.22f |

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
**Status:** RESOLVED 2026-09-01 (2.22g). The CLI half finally landed, closing
the split that made this entry nearly forgettable — the symptom had stopped
being visible while the defect stayed.

`--cron` / `--check-cron` are GONE, replaced by `--at HH:MM` + `--timezone` +
`--check off|HH:MM`. The CLI composes both crons and writes
`spec.backup.timeZone`, which the chart renders onto both CronJobs; an empty
`checkSchedule` now omits the check CronJob entirely, so disabling it no longer
requires a date that never arrives.

**The design's own load-bearing claim was refuted in a place nobody expected,
and it decided a feature.** `spec.backup` is FULLY structural in the CRD — the
only preserve-unknown markers are on `spec.overrides.*.values`, `spec.values`
and `status` — so an operator whose CRD predates `timeZone` gets HTTP 200,
every field it knows stored, and this one silently dropped. Worse than
"nothing happens": `backup_enable_patch` emits all seven CRD-required keys, so
the command HALF-SUCCEEDS — backups genuinely enabled, running in the wrong
zone, with the CLI reporting the zone it thought it set. And `kubectl` writes
the pruning warning to stderr, which `kubectl_merge_patch` reads only on
failure. So `enable` now READS THE FIELD BACK and fails loudly if it did not
survive, and `scripts/validate-crds.sh` asserts the round trip on a real
apiserver.

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
**Status:** FIXED 2026-08-31 (2.22b). Generalised owned-child prune in the
Application controller; **walk-verified** by `e2e/needs-removal-walk.sh`.
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
**Status:** RESOLVED 2026-08-31 (2.22f, ADR 0042 §10). `e2e/needs-redis-walk.sh`
**GREEN on kind+podman**, 106 assertions. The decisive one: with the operator
scaled to ZERO — so nothing could re-pin at runtime — a tenant authenticated on
the FIRST attempt after its instance restarted, on both the ephemeral and the
persistent arm. The credential could only have come from the file.

The walk also found a product defect on its first run, recorded below.

**Two claims in the analysis below were REFUTED while implementing, both in the
dangerous direction.** They are corrected inline, but read the ADR §10 text as
the record.

**And the walk found a third thing neither the analysis nor the design
anticipated.** The first design had the resync loop add `aclFromSecret` to the
CR — which makes the dragonfly-operator roll the StatefulSet. So the FIRST
claim on a fresh instance was handed a working connection Secret and then had
its instance restarted out from under it, seconds later. The walk hit it
head-on: the provision-phase ACL assertion ran against a pod still pulling its
image. Fixed by having the CR carry the field from birth, with the provisioner
seeding a default-only file immediately before the apply (create-if-absent, so
the loop remains the only writer of the CONTENTS). A fresh instance now never
rolls for this reason; the one-time roll is confined to instances that predate
the feature, which is where the release notes put it.
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
**Status:** FIXED 2026-08-31 (2.22c). Config drift is visible without rolling
anything (`status.envConfig.changedAt` vs each pod `status.startTime`). Walk owed
at the 2.22 close.
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

### What this is not: there is no rotation feature

The title says "rotating a secret", which overstates what exists. There is no
`apprafter secret rotate` verb and no rotation policy. The CLI ships `seal`
(create-or-replace) and `remove`; "rotation" is a developer re-sealing the same
name with a new value. So this entry is the **propagation leg of an explicit
re-seal**, not a half-built feature.

The only `rotate` in the tree is `apprafter repo creds rotate`, which re-seals a
SourceCredential's token. Whether *that* propagates is a separate question —
Argo CD's repo-server may read credentials per-operation rather than at start —
and is worth checking rather than assuming.

**The platform-owned half is unbuilt and tracked elsewhere.** `plan.md` 2.16a
("Per-claim password rotation", all boxes unchecked, pulled into launch as SR:A
off the 2.4g walk) records that a claim role's password is generated **only on
(re)provision** — `should_provision` is false once the claim is ready — so it is
**static for the entire life of the claim**. That is the generation gap; this
entry is the propagation gap. Both are needed and they are different fixes.

**And 2.16a's acceptance criterion is wrong for the same reason this entry
exists.** It reads, in translation: *"the application keeps connecting (it
re-reads the updated Secret)"*. A pod does not re-read a Secret consumed as an
environment variable — `valueFrom.secretKeyRef` is resolved once at pod start
and never again (only *volume*-mounted Secrets are refreshed by the kubelet),
and ADR 0046's renderer emits exactly `EnvVar{valueFrom: secretKeyRef}`. So
2.16a as planned would rotate the credential in the database and leave every
running pod authenticating with the old one — silently, in the same way this
entry describes. **This fix is a prerequisite for 2.16a**, and 2.16a's
acceptance line needs correcting before it is built.

### Batching: the decoupling already exists, and it is not the MigrationPlan

A developer who re-seals four secrets in a row should get one rollout, not four.
Two facts settle how:

1. **The operator does not watch Secrets.** The controller is
   `Controller::new(apps).owns(claims).owns(plans)` (`lib.rs:159-163`) — no
   `.watches(Secret)`. A Secret changing wakes nothing.
2. **It requeues every 60s** (`lib.rs:959`). So a content hash is recomputed
   within a minute of any re-seal without needing a Secret watch at all.

An earlier version of this section called that 60-second window "the batch".
**It is not, and the correction matters more than the original claim.** The
window is a coincidence, not a barrier: the requeue can fire between the first
seal and the second, which is *worse* than either extreme. The app then rolls
into a configuration that was never an intended state — one new credential and
two old ones — and for values that must move together (migrating three
third-party services from a dev contour to a prod one, say) that is a partial
cutover: production Sentry with a development Stripe key. Nothing about a timer
can fix this, because the timer does not know where the developer's sequence
ends.

**The atomicity primitive already exists and the docs do not use it.** A
Kubernetes Secret is written atomically — every key changes in one apiserver
write — and `secret seal` already *forces* all-of-one-secret at once, since
re-sealing replaces rather than merges. So values that must switch together
belong in **one secret with several keys**, not several secrets. The manifest
syntax already supports it (`secret:"thirdparty/sentry-dsn"`,
`secret:"thirdparty/stripe-key"`); nothing requires one secret per value, and
`docs/dev-guide/secrets.md` reads as if it did because its examples happen to be
single-value. Grouping makes the coordinated cutover atomic by construction,
with no new machinery and no dependence on a human's timing.

That leaves the ordering question only for values that genuinely cannot share a
Secret, and the honest answer is that a timer must not be what decides. Two
requirements pull in opposite directions and one mechanism cannot serve both:

- The **leaked-credential** case wants the roll automatic and immediate.
  Forgetting is the failure, so it must not depend on memory. This is the case
  that opened this entry.
- The **coordinated-cutover** case wants the roll explicit and human-timed.
  Automatic is the failure, because only the developer knows where the sequence
  ends.

What makes both safe is neither default but the thing missing underneath: **the
state is currently invisible in both directions**, which this entry already says
is its worst property. Stamp the resolved-config hash *and surface it* — pods
running revision `abc` while the current resolved config is `def`.

### Why the automatic roll should not be the default after all

Two further objections from the owner, both of which the draft above failed:

**A secret is not owned by one application.** Nothing stops three Applications
in a namespace from referencing `secret:"thirdparty/sentry-dsn"`. Re-sealing it
therefore rolls all three — possibly across teams — and **the person sealing has
no way to know that**. `secret seal` prints nothing about who consumes the
secret, and the operator holds no reverse index: `check_env_secret_refs` reads
per-application, so "which applications reference secret X" is answerable
(LIST Applications, scan `env`) but is not answered anywhere today. An automatic
roll with an unknowable blast radius is not a safe default.

**And `rollout: manual` had nowhere to live.** The draft proposed it as an
Application field, but the Application is per-app and the secret is shared: app
A declaring `manual` does not constrain app B, which will still roll. The
setting logically belongs to the *secret*, and **secrets have no declarative
surface at all** — `secret seal` is an imperative command with no manifest
behind it. The knob was proposed with no valid home, which is a design answer in
itself.

So the order inverts. **Visibility first, action explicit:**

1. The resolved-config hash on the pod template, surfaced in `app status` as
   drift — nothing silent in either direction.
2. The reverse index, surfaced at the point of use: `secret seal` reports which
   applications resolve this secret and that they are now running an older
   revision. This is also what D14 needs, for a different reason.
3. The roll is then an explicit act by someone who can see what it will touch.
   Whether that is `app restart` or something better is open, but the objection
   that an imperative restart verb is usually a symptom does not apply here — a
   coordinated credential cutover is not a symptom, it is the operation.

An automatic roll may still be right later, but **not at Tier 1** — see the ADR
0007 amendment of 2026-08-30 and D14. `apprafter secret` is an operator tool
standing in for OpenBao on a single node; the conditions that would make an
automatic roll safe (observable revisions, an authenticated and audited actor)
arrive with OpenBao and the portal at Tier 2+, where the right shape is a
notification offering a restart plus an opt-in knob — per application or
cluster-wide, on the shape of `autoRestartAppsOnEnvChanges`. At Tier 1 the roll
stays an explicit act.

Adding a Secret *watch* remains the wrong move regardless: it fires once per
Secret and tightens the fan-out into an even smaller window.

**The roll is not gated behind a MigrationPlan, and must not be.** The sentence
above about respecting destructive-change gating means the narrow thing: a
rotation must not become a side door that pushes an already-pending gated change
through. It does not mean the rotation itself needs approval. MigrationPlan
gates changes that can *lose data*; a rolling restart to pick up a new
credential loses nothing. And an approval step sitting between "revoke" and
"revoked" is backwards for the case that motivates the whole feature — a leaked
key is rotated precisely because it must take effect now.

Ships with a walk step that seals a new value and asserts the running pod picks
it up without any manual action. No current walk rotates a secret.

**Release chain:** this is operator behaviour, so it carries the full
operator → `appVersion` → platform-stack → compatibility chain. It was
deliberately kept out of the 2.20 documentation track for that reason.

## D7. The CLI cannot answer the question its own error asks

**Opened:** 2026-08-30 (2.20c, correcting `docs/dev-guide/secrets.md`).
**Status:** FIXED 2026-08-31 (2.22c). `EnvSecretMissing` names the cause, the
namespace and the available keys; `apprafter secret list` answers it directly.
Walk owed at the 2.22 close.
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

### Why it bites so often

`secret seal` and `secret remove` both default `--namespace` to
`apprafter-system`, which is right for platform credentials and wrong for every
application secret. Sealing into the wrong namespace is therefore the
single most likely way to arrive at `EnvSecretMissing` — and sealed secrets are
namespace-bound, so the recovery is to re-seal rather than move. The one mistake
the default invites is the one the CLI cannot help diagnose.

### The message is not ambiguous by necessity — the operator throws the answer away

This entry originally called the condition message "deliberately ambiguous",
which implies a reason. There isn't one. `check_env_secret_refs`
(`operator-controllers/application/src/lib.rs:1553-1578`) **has both facts in
hand and discards the distinction**:

```rust
let exists = match api.get_opt(secret_name).await? {
    Some(secret) => { /* … key present in data or string_data? … */ }
    None => false,          // ← the Secret does not exist
};
if !exists {
    missing.push(format!(
        "env {} → secret \"{}/{}\": Secret \"{}\" not found or missing key \"{}\"",
        …
    ));
}
```

The `None` arm and the failed key lookup are two different answers collapsed
into one `bool`, and then reported as "not found **or** missing key". The
namespace is in scope on the line above and is not named either, even though
sealing into the wrong namespace is the most likely way to get here.

So the first fix is not a new command. It is naming what the code already
knows.

### One index, read in two directions

The second question — *which secrets does this application consume* — is the
same lookup D6 and D14 need, read the other way round:

| direction | question | needed by |
| --- | --- | --- |
| app → secrets | which secrets does this app resolve, and do they exist? | **D7** (diagnosis) |
| secret → apps | which apps resolve this secret, and are they on an older revision? | **D6** (blast radius), **D14** (disclosure) |

Neither direction exists today. Both fall out of a single pass — LIST
Applications, scan each `env` for `EnvValue::Ref(EnvRef::Secret(_))` — which the
operator is already positioned to do, since it watches every Application anyway.
Building one direction and not the other would be the waste.

### The fix, cheapest first

1. **Name the cause and the namespace** in the `EnvSecretMissing` message. Two
   lines in `check_env_secret_refs`, and it removes the need for a second
   command in the common case entirely. Carries the operator release chain.
2. **A listing subcommand** under `apprafter secret` — name, namespace and
   **key names, never values** — with an all-namespaces flag. Answers "what is
   sealed and where" *before* an error rather than after, and collapses the
   guide's "telling a wrong namespace from a wrong key" section into one
   command. CLI-only.
3. **Both directions of the index**, surfaced where each is asked: the app→secrets
   view in `app status`, the secret→apps view at the point of sealing (D6) and in
   whatever discloses a seal (D14).

**The demand was already recorded twice before this entry.** ADR 0057's 2.19h
amendment predicted the guide would be unable to say what is sealed and in which
namespace; the 2.19j walk observed exactly that. Counting this entry, three
independent observations of one missing verb.

## D8. A node-scoped warning published through an optional object

**Opened:** 2026-08-30 (2.20c, correcting `docs/operator-guide/shared-volumes.md`).
**Re-diagnosed 2026-08-30** after the owner asked whether `CapacityWarning`
covers the volume or the machine's disk. It covers the machine's disk, and that
inverts what is wrong here.

**Status:** FIXED 2026-08-31 (2.22d) — code complete, **walk step still owed**.
The node signal moved to the `PlatformStack` singleton and warns on every
command; owned disks, Postgres databases and Dragonfly logical DBs all report a
figure in `app status`. No walk asserts the banner yet: the "Ships with" note
below was a plan, not a delivery.
**Severity:** medium-high. The original entry said the signal reaches nobody
through the CLI. It is worse: for most clusters the signal is never raised at
all.

### `CapacityWarning` is the node's disk, not the volume's

`node_free_fraction` (`resourceclaim-provisioner/src/capacity.rs:52-59`) reads
`/node/fs/availableBytes` ÷ `/node/fs/capacityBytes` from the kubelet Summary
API — **the node's root filesystem** — and `is_capacity_warning` fires below
`DEFAULT_NODE_FREE_THRESHOLD = 0.15`, i.e. when the node is more than 85% full.
The module's own docstring states the blast radius:

> A node's local-path filesystem can fill up; when it does, the local-path PVCs
> backing a `SharedVolume` **(and owned disks)** silently stop accepting writes.

So the answer to "is there an equivalent for the node's disk?" is inverted:
**the node's disk is the only thing warned about**, and it is stamped on a
`SharedVolume`.

### The defect: a node fact carried by an optional object

A cluster with no `SharedVolume` gets **no node-disk warning at all**. Nothing
else computes it, and nothing else carries it. Yet everything on a Tier-1 node
shares that filesystem: owned `needs.disk` PVCs, CNPG data, Dragonfly snapshots,
container images, logs. The docstring names owned disks explicitly as affected,
and owned disks have no carrier for the signal.

Attaching a node-scoped fact to an application-scoped, optional CR is the
whole bug. The CLI gap the original entry recorded is downstream of it.

### There is no per-volume fullness warning

`pvc_usage` (`capacity.rs:64-77`) samples `(usedBytes, capacityBytes)` per PVC
and `shared_volume.rs:200` writes it to `status.capacity`. But
`is_capacity_warning` takes **only** `node_free_fraction` — the per-volume
numbers are sampled, stored, and **never thresholded**. A volume can be at 99%
of its own request on a healthy node and nothing says so.

### What the owner asked for

1. **The node-disk warning belongs on every command that touches the target**,
   in yellow, on the model of the CLI's own new-version notice —
   `commands::version_check::maybe_warn_about_newer_version()`, called once from
   `lib.rs:92` at the top of dispatch. That is the right shape and the right
   hook: one call site, every command, non-fatal, impossible to miss and
   impossible to be in the wrong place for. It also removes the dependency on
   any particular CR existing.
2. **The per-volume signal belongs in two places** — `apprafter volume status`
   for the volume itself, and in the status of each application that consumes
   it. Neither mentions capacity today.
3. **`app status` should report the application's full state, including every
   claim it consumes and that claim's own condition** — for example the size of
   the database or the volume.

On (3), the foundation is already there and is narrower than it sounds.
`print_resource_claims` (`cli/platform-cli/src/commands/app.rs:1605`) already
prints a claim table — `NAME / PROVIDER / READY / SCHEDULED / SECRET`. What is
missing is the *state* column: the claim is shown as provisioned or not, never
as how full it is. For volumes and owned disks the platform already samples the
numbers (`pvc_usage`) and simply does not route them to an application-side
reader. For a database size it would need a new probe — CNPG and Dragonfly both
expose one.

### The fix, in dependency order

1. **Move the node signal off the `SharedVolume`.** It is node-scoped, so it
   belongs somewhere node-scoped — a condition on the `PlatformStack` singleton
   is the natural carrier, since that object already exists on every cluster and
   is what `apprafter platform status` reads. Keep stamping the `SharedVolume`
   too if useful, but stop making it the only path.
2. **Warn on every command**, via the `version_check` hook shape, reading that
   condition. Yellow, non-fatal, and best-effort in the same sense the sampler
   already is — a cluster that cannot be reached must not turn a warning into an
   error.
3. **Threshold the per-volume sample** the platform already takes, and surface it
   in `volume status` alongside the existing `Used`/`Free` line.
4. **Extend the `app status` claim table with state.**

   *Corrected 2026-08-31 while implementing:* the premise that "the volume and
   disk numbers already exist" is wrong. `ResourceClaimStatus` carries no size
   and no usage; a PVC carries its provisioned size but not its fullness; and
   fullness comes from the kubelet Summary API, which only the operator
   samples. So the CLI cannot show a claim's capacity today at any price.

   What shipped instead is a **`BACKING` column** naming the concrete resource
   serving each claim — the pooled instance and logical DB for redis, the
   standalone PVC for a disk — which is in the claim's own status, costs
   nothing, and turns a row reading `true true` into one an operator can act
   on. Two apps on one Dragonfly instance are isolated only by that DB number,
   and it was previously invisible.

   Printing a claim's provisioned size alone was considered and rejected: it
   answers the easy half of the question the column exists for and reads as if
   it had answered both.

   **Then done properly, same day, after asking what actually blocked it.**
   The provisioner now samples an owned disk's own PVC while provisioning it —
   the kubelet Summary call it already makes for SharedVolumes — and writes
   `status.capacity` on the claim, which `app status` renders as
   `pvc/<name> (91% full)`.

   **Disk only, and that is the finding rather than a shortcut.** An owned
   disk has its own PVC and therefore its own denominator, so a percentage is
   a judgement. A `pg` or `redis` claim is a tenant of a *shared* backend:
   `pg_database_size` would give bytes with no per-tenant limit to read them
   against, and a tenant at 3 GB is fine or fatal depending on a shared PVC it
   does not own. The actionable figure there is the backend's own fullness,
   which is a backend-level fact and is sampled where the backend lives.

   Two further costs argued for deferring per-tenant size entirely. **Both
   were wrong, and the owner rejected them the same day.**

   *Corrected 2026-08-31, twice, both times after being pushed on it.*

   The first correction is that the question had been conflated. "Am I about
   to run out" needs a denominator; "how much data do I have" does not. The
   second sizes a backup, shows growth and explains a bill on its own, and it
   is the one that was asked for.

   **Postgres — the objection collapsed on inspection.** The claim was that
   the operator has no SQL client, so per-database size means a connection
   path, tenant credentials held in-process and a new failure surface inside a
   reconcile loop. But CloudNativePG's instance manager already runs a
   Prometheus exporter on every instance pod, always on, and
   `cnpg_pg_database_size_bytes{datname}` is one of its DEFAULT metrics. One
   HTTP GET through the apiserver pod-proxy — the shape `operator-core::capacity`
   already uses for the kubelet Summary API — returns every tenant database's
   size from a single scrape. It is also *better* than the client that was
   rejected, not merely cheaper: the exporter holds its own connection, so a
   scrape costs nothing from the shared cluster's `max_connections`, which a
   client of ours would have taken from the tenants it was measuring.

   **Redis — the conclusion held, the reasoning did not.** The original text
   ("Dragonfly does not report per-DB memory") was inferred from our own trait,
   not from upstream. Re-derived against the v1.37.0 source and re-checked
   against `main`, every route to per-logical-DB BYTES is genuinely closed:

   - Per-DB byte figures *do* exist — `obj_memory_usage` and `table_mem_usage`
     live per database in `Metrics.db_stats[i]` and are still in scope at the
     very loop that emits the `db`-labelled metrics — but every emission site
     sums across databases before printing. Nothing reaches a client per-DB.
     Exposing them upstream is a ~3-line patch at an existing loop; the
     blocker is that it is not upstreamed, not that the data is unreachable.
   - All seven `db`-labelled Prometheus metrics are counters.
   - Several `DEBUG` subcommands (`SEGMENTS`, `VALUES`, `TOPK`, `KEYS`,
     `COMPRESSION`) genuinely *are* scoped to the selected DB — a fact the
     first pass missed — but report slot capacity, logical lengths, or a
     Huffman-training sample. `COMPRESSION`'s `raw_size` is the trap: it looks
     like a byte total and is not, being truncated at 512 bytes per key,
     aborted mid-scan at a frequency cap with no flag in the reply, blind to
     anything stored inline (all keys ≤16 bytes contribute zero), and limited
     to one value type per call.
   - `INFO MEMORY` *is* scoped to the caller's dragonfly NAMESPACE, so
     namespaces would answer this. But on this version a non-default namespace
     is not snapshotted, not exported to `/metrics`, gets no TTL expiry or
     eviction, and — because the replication journal carries no namespace — is
     replayed into the replica's default namespace. That route costs data, not
     just accuracy.
   - No released version helps: `main` does not have it, `INFO KEYSPACE` is
     byte-identical from v1.37.0 through current `main`, and there is no open
     or merged upstream PR adding it.

   So redis reports a **key count**, and the CLI labels it `keys` rather than
   rendering it as a size. Real bytes would need the upstream patch or one
   Dragonfly instance per claim — which is ADR 0042 reversed, at 320Mi per
   tenant, and is not worth it on T1.

   Per-tenant size as **cost attribution** — where `spec.md` §7 files it —
   remains a separate feature from this capacity signal.

Ships with: a walk that fills a node past the threshold and asserts the warning
appears on an unrelated command (not just on `volume status`), and a walk step
asserting a cluster with no `SharedVolume` still warns.

## D9. There is nothing to roll a moving tag back to

**Opened:** 2026-08-30 (2.20c, correcting `docs/dev-guide/image-iteration.md`).
**Status:** RESOLVED 2026-08-31 (2.22e, ADR 0059). `e2e/image-rollback-walk.sh`
**GREEN on kind+podman** — 30 assertions, including the two that only a live
cluster could settle: the pin survives an Argo sync without drift, and the
`Suspended` tile aggregates to the user's own Argo Application while the sync
operation still reaches `Succeeded`.
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

Retaining the digest is the easy half. `StatusImage`
(`operator-core/src/application.rs:469-480`) already carries
`{tag, resolved, resolvedAt}` — the *current* resolution with its timestamp — so
a `previous` sibling (or a short bounded history) is a small additive change to
a struct that exists. `status.lastAppliedSpec` does not help here and should not
be reached for: it holds the previous *spec*, which carries the tag, and after a
same-tag push the tag is identical.

The hard half is what the owner's follow-up questions expose.

**A rollback against a moving tag must pin, so it is a mode change, not a value
change.** If the workload is merely set back to the older digest, the operator's
next reconcile re-resolves `:latest`, finds the bad build again, and rolls
forward — within the 60-second requeue. So rolling back necessarily takes the
application *off* the moving-tag train. That is not a side effect to tolerate;
it is the operation.

Three consequences follow, and all three are requirements rather than polish:

1. **A verb to get back on the train is mandatory, not a nice-to-have.** Without
   it, `rollback` is a one-way door out of the platform's headline feature, and
   the only documented way back is hand-editing the manifest — which is the same
   "abandon auto-deploy to recover from it" trap this entry already records,
   arrived at from the other side.
2. **The pin must be visible.** `ImageResolved` is documented as *"absent when
   `imagePolicy.resolve: off`"*, and an absent condition is not something a human
   reads. Without a positive statement — `app status` saying, in words, that this
   application is pinned at a digest and is no longer following `:latest` — the
   next failure is silent: a developer pushes the fix, nothing deploys, and
   nothing says why. That is the same shape as D6, D8 and D10 in this file.
3. **`--to` is already taken, by a Git revision.** The flag exists today and
   documents itself as "commit SHA / tag / branch". A digest is also a hash, so
   `--to <hash>` is ambiguous the moment images enter. The clean discriminator is
   the OCI form itself: `--to sha256:…` is an image digest, anything else is a
   Git revision. Worth deciding explicitly rather than discovering later.

### Where the pin lives — and why the platform's own pin was easy

The owner's intuition that the platform version pin is the same mechanism is
correct, and their diagnosis of why it does not transfer directly is also
correct. Both are worth writing down, because the platform pin is a working,
production-proven instance of exactly what this needs.

**How the platform pin reaches Argo — a three-hop chain.**

1. The CLI writes `spec.pin` on the `PlatformStack` singleton with a plain
   `kubectl_merge_patch` (`commands/platform.rs`).
2. The `PlatformController` reads `spec.channel` / `spec.pin`, resolves a target
   version, and **SSA-patches the parent `platform` Argo CD Application** in the
   `argocd` namespace under the dedicated field manager `platform-controller`,
   setting `spec.source.{targetRevision, helm.valuesObject}`
   (`platform-stack/src/reconcile.rs:6-9`, `:386-402`).
3. Argo CD propagates the change to the children.

**Why that was easy, and the app case is not.** The `PlatformStack` CR is
created by `cluster-bootstrap` step 5 and **lives only in the cluster** — no Git
repository contains it, so nothing contests a CLI write. The application's
AppRafter `Application` CR is the opposite: it is rendered by the CUE CMP from
the user's Git repository, so Argo owns it and the next sync reverts anything
the CLI writes into its `spec`. That asymmetry is the whole reason
`platform pin` was a merge-patch and an app-level pin is a design question.

**The owner's proposal — hold app pins in a cluster-only object, on the grounds
that a rollback pin is an intermediate state rather than normal cluster
operation — is sound**, and the reasoning is the part to keep: a pin that
contradicts the repository should not be written into the repository.

Two candidate homes, with the honest objection to each:

- **An annotation on the `Application` CR under a dedicated SSA field manager.**
  Cheapest, and this repository already has *empirical* evidence it works: ADR
  0048's anchor stamping established that an SSA-written annotation survives an
  Argo sync without `ignoreDifferences` — and that finding is on record
  specifically because only the live probe was authoritative about it. The pin
  then lives next to the thing it pins and disappears with it, which sidesteps
  D4's ownership problem for free.
- **A per-app pin map on the `PlatformStack`.** Works, and matches the
  cluster-only property exactly, but puts application-scoped data on the
  platform's own object and needs its own garbage collection — an app deleted
  while pinned leaves a stale entry, which is D4's defect class in a new place.
  Prefer the annotation unless something forces this.

**A consequence of either, which argues for the visibility requirement above.**
A pin held outside Git is invisible *to* Git: a reader of the repository sees
`:latest` and cannot know the cluster is holding an older digest. So the status
surface is not decoration here — it is the only place the truth exists. The
owner's requirement is that `app status` mark a rolled-back application **in
yellow**, naming the version it is held at, on the model of the other
attention-warranting states. Worth surfacing in `platform status` as well, so an
operator can see every pinned application in one place rather than per-app.

### Surfacing it in Argo CD: yes, and the carrier already ships

The owner asked whether a rolled-back application can be marked in the Argo UI.
It can, and it is a new branch in a script that already exists.

`resource.customizations.health.apprafter.io_Application`
(`platform-stack/cue/component_argocd.cue:142`) already reads `obj.status.phase`
and, for `AwaitingMigrationApproval`, returns `Degraded` with a message lifted
from the `MigrationPending` condition. A pinned application is the same shape:
one more branch, reading status the operator already writes.

**And it bubbles up, unlike the ADR 0048 case — which is the point.** That work
found the MigrationPlan's health did *not* reach the root Application, because
Argo aggregates from an app's **managed** resource set (`status.resources`), and
the anchored plan was a live tree child that nothing managed. The AppRafter
`Application` CR is the opposite: it is rendered by the CMP from the user's
repository, so it *is* a managed resource of their Argo Application. Its health
therefore aggregates, and the application's own tile in the Argo list changes
state — top-level visibility, without the A5 problem that defeated the earlier
attempt.

Two cautions, both learned the expensive way on that ADR:

- **`Suspended`, not `Degraded`.** A pinned app is deliberately held, not
  broken; `Degraded` would be wrong on the merits and would trip health-gated
  automation and alerting. ADR 0048 chose `Suspended` for exactly this
  distinction.
- **Because it *does* aggregate here, it changes the user's Argo app health**,
  which can interact with auto-sync, self-heal and anything gated on health.
  That interaction is precisely the class of claim ADR 0048's A5 finding was
  empirically disproven on, so it must be settled by a live probe rather than by
  reading the aggregation rules.

**On "icon": achievable, but not a custom one.** Argo has no per-resource custom
icon; what you get is the icon bound to the health status, and `Suspended` has
its own distinct one. **On "label": two real mechanisms.** The health script's
`hs.message`, shown on the node, and `spec.info` on the Argo Application, which
renders as name/value rows in the app detail view — currently unused anywhere in
this repository, and written through the same patch route the pin already needs,
so it is nearly free once that path exists.

Ships with: a walk step that pushes a second image to the same tag and asserts
the rollback returns the workload to the first **and that it stays there across
at least two reconciles** (the naive fix passes the first assertion and fails the
second); a step asserting the un-pin verb returns the app to following the tag;
and an assertion that `app status` states the pinned mode in words.

*Amended 2026-08-31 while implementing.* Three corrections, each found by
checking a claim above rather than by building on it.

**The same-tag push is not achievable in a hermetic walk, and the failure would
be silent.** The operator resolves digests over HTTPS against webpki roots with
no CA or insecure escape hatch (`oci_resolve.rs` hardcodes the scheme), so an
in-cluster `registry:2` is unreachable to it — and resolution failure is SOFT
by design (ADR 0040): it renders the verbatim tag and the rollout proceeds. A
walk pointed at a plain-HTTP registry would therefore have gone GREEN while
testing nothing. `e2e/image-rollback-walk.sh` moves the MANIFEST from
`nginx:1.27-alpine` to `1.28-alpine` instead, which drives the same retention
and shift code path with two public tags, no credentials and no new product
surface. Funding a CA-bundle escape hatch in the resolver is a real product
gap (private registries with their own CA are unsupported today) but it is a
separate change with its own security review.

**`Suspended` DOES stall a sync — just not ours.** Read out of gitops-engine at
the shipped version: a non-hook task settles only on `Healthy` or `Degraded`, so
a permanently-`Suspended` managed resource parks the operation in `Running`
whenever the task set spans more than one wave or phase. `CreateNamespace=true`
does NOT arm it (`syncNamespace` returns unmodified for an existing namespace
without `managedNamespaceMetadata`); waves, hooks and
`managedNamespaceMetadata` do. The shipped app shape has none, so this is an
INVARIANT to keep rather than a hazard to accept, and the walk asserts
`operationState.phase == Succeeded` rather than only the tile state.

**The tile signal is conditional.** `Suspended` is the second-healthiest code in
gitops-engine's ordering, so it overrides `Healthy` and nothing else. A pinned
app whose sibling managed resource is `Progressing` shows `Progressing` — which
includes a repository rendering several applications into one Argo Application.
The health branch is therefore keyed on the pin marker alone and evaluated above
the `Progressing` fall-through; nested under a phase check it would read
`Progressing` exactly while the rollback rolled pods.

## D10. The applying half of right-sizing has never been observed to apply anything

**Opened:** 2026-08-30 (re-reading **D1** against the tree).
**Status:** FIXED 2026-08-31 (2.22d) — code complete, **walk rewritten but not
yet run**. In-place resize needs a real Hetzner node (kind/k3d cannot do it), so
the rewritten `e2e/vpa-walk.sh` runs in the batch at the 2.22 close.
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

   *Shipped as:* a second managed app in the same walk. An explicit large
   request was not usable — it puts the app in pro-mode, which prunes the VPA
   (that is what check #10 asserts) — and the schema expresses no
   `command`/`args`, so the footprint has to come from the image itself:
   `rabbitmq:3-alpine`, whose Erlang VM idles well above the 32Mi floor with
   no arguments and no required env. It is deployed alongside the tiny app so
   its history accumulates in the window the recommender is already warming
   up in.

   The walk now asserts the pair genuinely differs *before* asserting the
   resize, and names which of the two failed — a degenerate pair is a
   harness problem, not a resize failure, and conflating them sends the next
   reader to debug the wrong component. It then polls
   `status.containerStatuses[0].resources`, checks the pod uid is unchanged
   (an `InPlace` resize that evicted would be a defect in itself), and dumps
   the pod's resize conditions plus the updater log on failure.

   The tiny app's observation stays, but can no longer report `ok:` from the
   degenerate case — it prints `inconclusive:` and says why.
2. **Emit the deferred signal.** Wire the `notApplied` probe the ADR
   specified and the whole downstream path already implements.

   *Corrected 2026-08-31 while implementing:* this line said to read "the
   VPA's own in-place condition". **There is no such condition.** Checked
   against the updater at tag `vertical-pod-autoscaler-1.7.1`: its only
   observable outputs on the in-place path are log lines
   (`"In-place update deferred"`, `"In-place update infeasible…"`), an Event
   recorded on the *pod*, and its own Prometheus counters. Nothing is written
   to the `VerticalPodAutoscaler` CR status at all.

   The kubelet states it directly instead, and first-hand: `PodResizePending`
   with reason `Infeasible` (will not fit as things stand) or `Deferred` (not
   now, possibly later), and `PodResizeInProgress` with reason `Error` for an
   actuation failure. Conditions rather than the older `status.resize` field;
   the feature is GA as of Kubernetes 1.35.

   That also fixes a second thing the old code got wrong. The hardcoded
   message said "node capacity" for *every* block, which is only ever right
   for `Infeasible` — a `Deferred` resize reported as a capacity failure
   sends someone to resize a node that was never the problem.
3. **Preflight the Kubernetes prerequisite** the way the swap path already
   preflights its own: a version and feature check at bootstrap, and a walk
   assertion, so an unpinned upstream moving under us is a finding rather
   than a silence.

   *Shipped as:* a startup probe in the operator (`apiserver_version` →
   `(major, minor)` → ≥1.33, the release from which the gate is on by
   default) folded into the **same** `notApplied` field, so it reuses the
   whole downstream path — CRD field, operator type, CLI rendering — and adds
   no new surface. Plus a direct walk assertion on the server minor version.
   An unreadable version degrades to "supported": refusing to claim a defect
   we cannot evidence, since a wrong "your cluster is too old" on every app
   would be worse than the silence it replaces.

   One case had to be carved out. In observe-only mode (`platform autoscale
   set off`) the VPA is emitted with `updateMode: Off` and nothing is applied
   *by design*, so the probe is skipped entirely — otherwise a deliberate
   setting would be dressed up as a fault, and on an older cluster the
   unsupported-Kubernetes message would be an outright wrong diagnosis.

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

**Status:** FIXED 2026-08-31 (2.22a). Preflight before every prompt and every
billable step; typed `kubectl`/`restic` classifiers at the choke points; `doctor`
fails rather than warns on a binary the CLI cannot work without.
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

**Status:** FIXED 2026-08-31 (2.22b), by the same owned-child prune as D4;
**walk-verified**.
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

**Status:** FIXED 2026-08-31 (2.22b). The pull-secret copy carries an owner per
consuming app (reference counting, since cross-namespace ownerRefs are
forbidden) instead of a new shape. **Walk step still owed** — the
needs-removal walk does not assert the reclaim.
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

## D14. Re-sealing a secret performs a gated change through an ungated door

**Opened:** 2026-08-30, raised by the project owner. Reframed the same day —
the first draft led with the exfiltration threat and proposed egress
allowlisting as the control. The owner's framing is stronger and is now the
entry: this is not a new threat to mitigate, it is **an inconsistency in a
policy this platform already ratified**.

**Status:** RESOLVED BY DECISION 2026-08-30 — the asymmetry is accepted at
Tier 1, deliberately, and the outstanding work is disclosure rather than a gate.
See the ADR 0007 amendment of the same date. The three deliverables it names are
still open; they are tracked here and in D6.
**Severity while read as a security hole:** high. Read correctly it is not one —
see "Why this is not the hole it looks like" below.

### The same change is the most severe class through one door and unclassified through the other

`ApplicationMigrationStrategy` already treats *where an environment variable's
value comes from* as a security question, and treats it as the **most severe**
one it has. `classification_severity` (`operator-core/src/migration.rs:90-94`)
ranks `security-boundary` at 4, above `data-migration`. Three env transitions
earn it (`migration/src/strategy.rs:413-509`):

| trigger | transition | classification |
| --- | --- | --- |
| `env-secret-ref-add` | non-Secret → `Ref(Secret(a))` | `security-boundary` |
| `env-ref-downgrade` | `Ref(_)` → `Literal(_)` | `security-boundary` |
| `env-secret-ref-retarget` | `Ref(Secret(a))` → `Ref(Secret(b))`, a≠b | `security-boundary` |

The third is the one that matters. **Pointing an env var at a different secret
is gated behind a MigrationPlan and an explicit approval.** The care taken is
visible in the code: `from`/`to` carry only ref *sentinels*, never a literal
value, so the plan cannot leak the credential it describes.

Now do the same thing the other way: leave the manifest untouched and re-seal
secret `a` with different contents. The environment variable resolves to a
different credential — **the identical outcome the retarget trigger exists to
gate** — and there is no plan, no approval, no classification, and no record
beyond the SealedSecret's `resourceVersion`. `apprafter secret seal` writes not
even an annotation saying who sealed it or when.

One door treats this as the most severe class of change the platform knows. The
other has no gate at all. Whatever the right answer is, it cannot be both.

### What the ungated door permits

**Availability.** Re-seal a working credential as a broken one: the application
fails. Loud, recoverable.

**Redirection.** Re-seal a credential as somebody else's account of the *same*
service — their Sentry DSN, their S3 endpoint and keys, their OTLP collector.
The application keeps working perfectly, `Ready` stays true, and data begins
arriving in another tenancy. Sentry payloads alone carry request bodies, user
identifiers and stack-local variables.

**Substitution.** Re-seal a credential that *is* a trust boundary rather than a
destination — a JWT signing key, a TLS client certificate, a webhook signing
secret, an SSH deploy key. Nothing is redirected anywhere; the attacker gains
the ability to forge tokens the application will honour, or to decrypt what it
protects.

The actor needs only permission to create a SealedSecret in the application's
namespace: an ordinary developer role, a compromised CI token, a stolen
kubeconfig. Not cluster-admin.

### What will not fix it

**Gating the secret itself.** A reviewer would be approving an opaque blob they
cannot diff, and it taxes the one operation that must be fast — revoking a leak.
Recorded here so it is not proposed again.

**Egress allowlisting.** An earlier draft named this "the enforceable control".
That was overreach, on two counts the owner identified.

First, it does not exist and was not planned. What 2.10 / ADR 0045 shipped is a
per-application CNP **derived from `needs`**, plus a cluster-wide
`EgressProfile` with exactly three values — `Internet` (default: DNS + same-ns +
`world` + needs), `Internal`, `Strict` (`operator-core/src/platform_stack.rs:58-66`).
None names permitted hosts, and the profile is not per-app. The `network:` field
in `schemas/v1alpha1/application.cue:102` is *ingress* exposure, not egress
destinations. A manifest-declared destination allowlist would be net-new work;
the only trace of the idea is a note in the chart source
(`platform-stack/cue/render_tool.cue:633`, "tighten to toFQDNs when the endpoint
is known").

Second, and decisively: **it covers one of the three vectors above.** Destination
allowlisting can refuse a connection to `attacker.example`. It does nothing about
substitution, where no new destination is contacted at all and the credential
itself is the thing being forged with. It remains worth building on its own
merits — the difference between "this app may reach the internet" and "this app
may reach Stripe and Sentry" is real — but it is defence in depth for one vector,
not the answer to this entry.

### Why this is not the hole it looks like

The owner's resolution, ratified as an amendment to ADR 0007 on 2026-08-30:
**`apprafter secret` is an operator toolkit, not a developer one, and it exists
only at Tier 1 as a deliberate primitive standing in for OpenBao.**

ADR 0007 already stated the premise in its own Negative consequences —
SealedSecrets "does not provide dynamic credentials, automatic rotation, or
fine-grained ACL" — and Tier 1 accepts that because a KMS-less single node
cannot carry OpenBao's unseal UX or footprint. **A primitive chosen for that
reason cannot then be asked to carry an authorization story.** Trying to build
one produces the worst outcome available: a control that looks like a boundary
and is not.

So the analysis above measured an operator tool against developer-tool
expectations. The actor in every scenario is someone holding a kubeconfig that
can create SealedSecrets in the namespace — which at Tier 1 means someone who
already holds every credential in the cluster. There is no boundary being
crossed. Where operator and developer are the same person, the tier's design
centre, there is nothing to gate; where a team separates the roles on Tier 1,
Kubernetes RBAC is the mechanism, not a gate inside `apprafter secret`.

What survives is the **asymmetry in what we say**, not in what we enforce. The
MigrationPlan path treats an env retarget as `security-boundary` because a
manifest change is reviewable; the seal path cannot be reviewed, so it must be
*disclosed* instead.

### The outstanding work is disclosure

Per the ADR amendment, in order:

1. **Say it.** Every surface presenting `apprafter secret` states that it is a
   Tier-1 operator tool standing in for OpenBao, carrying no fine-grained access
   control or audit, replaced at Tier 2+. ADR 0007's promised Backstage banner
   is the Tier-2-approach half and is still unshipped with the rest of the
   portal, so the CLI and the docs carry this until then.
2. **Move the page.** `docs/dev-guide/secrets.md` is in the wrong guide and
   written to the wrong reader ("your manifest, your repository"). The binding
   half — `secret: "<name>/<key>"` and reading `EnvSecretMissing` — stays with
   developers; the sealing workflow moves to the operator guide.
3. **Make the blast radius visible** — which applications resolve this secret,
   and which are still on an older revision. Shared with D6, and an operations
   obligation rather than a security one.

Attribution on the seal (who, when) is still worth stamping, for the
insider-mistake and forensics cases rather than as a control.

**Rejected, and recorded so it is not proposed again:** gating the seal behind
an approval. The reviewer would be approving an opaque blob they cannot diff,
and it taxes the one operation that must be fast — revoking a leak.

### What changes at Tier 2+

With OpenBao's revisions and the portal, the platform can watch a secret's
revision and **notify** the owners of dependent applications, offering a restart
rather than performing one. An opt-in knob — per application in the manifest or
cluster-wide, on the shape of `autoRestartAppsOnEnvChanges` — is the right home
for automation there, because the two things that make it unsafe at Tier 1 are
gone: revisions are observable, and the identity performing the change is
authenticated and audited.

## D15. The destructive-change gate never engaged for a base-only app

**Opened:** 2026-08-31, found on the FIRST run of the 2.22b needs-removal
walk — by a walk written to prove a different fix.

**Status:** RESOLVED the same day (schema fix + apiserver regression probe).
**Severity:** high. Not a missing feature: a **reconcile freeze**, and the
whole approval gate silently inert for the common case.

### What was wrong

`reconcile` derives the environment as
`app.spec.environment.clone().unwrap_or_default()`
(`application/src/lib.rs:244`), so an Application with no `spec.environment`
— a base-only deploy, which is the default — yields `""`. The comment there
says so plainly: *"or the empty string for the base/default env — the same
value `create_plan_for` / the plan scope carry"*. The whole operator agrees:
`env_owned`, `PlanKey`, `plan_name` and `plans_to_delete` all key on `""`
for base.

The CRD did not. `schemas/v1alpha1/migrationplan.cue` constrained
`scope.application.environment` to
`^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$`, which rejects the empty string, so the
apiserver refused every plan the operator built for such an app:

```text
MigrationPlan.apprafter.io "parser-migration-1788139452" is invalid:
spec.scope.application.environment: Invalid value: "":
  in body should match '^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$'
```

### What it cost

The operator detected the change correctly and logged
`destructive change detected — creating gating MigrationPlan
trigger=needs-removal` — and then failed the apply, returned the error, and
retried. Every thirty seconds, with a fresh plan name, indefinitely. The walk
log shows four cycles in two minutes before it gave up.

So for any base-only Application:

1. **The gate never engaged.** A destructive edit could not be approved,
   because the object that carries the approval could not be created.
2. **The reconcile froze.** The error returns before the render and apply
   steps, so the application stopped converging on anything at all — the same
   shape as the ADR 0048 anchor-403 freeze, reached from a different
   direction.
3. **Silently.** Nothing surfaced on the Application; the evidence lived only
   in the operator's log.

### Why nothing caught it

`e2e/app-migration-walk.sh` is the walk built for this machinery, and every
Application in it carries an environment (`mig-dev`, `mig-prod`). The
empty-environment path — the default one — was never exercised. Unit tests
did not catch it either, for the reason unit tests never catch this class:
`create_plan_for` returns a well-formed struct, and only an apiserver
enforces the pattern.

That is the third instance of the rule this file keeps re-learning, in a new
place: **only a live apiserver validates a CRD schema.**

### The same assumption, twice

Fixing the CRD pattern did not fix the bug — it **moved the rejection one
layer up**. The admission webhook carried an independent copy of the same
belief (`validator_migrationplan.rs`, in both the typed and the raw-fallback
branch):

```text
admission webhook "migrationplans.apprafter.io" denied the request:
spec.scope.application.environment: environment is required
```

And a unit test asserted exactly that, so the webhook half was **defended on
every run** — the third instance in this session of a test that pins a defect,
after D3's `nodes[0].kind` and `doctor`'s WARN-on-missing-kubectl.

The comment above the webhook check named the duplication as deliberate:
*"the CRD pattern also rejects it — defence-in-depth"*. Defence in depth is
the right instinct and it multiplies a wrong premise as faithfully as a right
one. Only the second walk run showed the second layer, because the first
never got past the first.

### The fix

`environment` now accepts the base sentinel:
`^([a-z0-9][a-z0-9-]{0,62}[a-z0-9])?$`. The CRD is aligned with what the
operator has always written, rather than the reverse — `""` for base is the
established internal contract in four places, and the schema was the outlier.

The webhook's requirement is removed in both branches, its defect-pinning
test is flipped to assert that both shapes the operator can emit (present-and-
empty, and absent) are accepted, and a sibling test asserts the `ref` guards
that share the branch still fire — so dropping the wrong check did not weaken
the right ones.

Guarded by an apiserver probe in `scripts/validate-crds.sh`, alongside the
`imagePolicy.resolve: "off"` one, so `just crd-validate` fails if the pattern
tightens again. The red state was not synthesised: the walk log carries the
apiserver's own 422 against exactly this object.

**Also worth keeping:** the walk that found this was written to prove D4 and
D12, and never reached its own subject. A walk exercising a real default is
worth more than the assertion it was aimed at.

## D16. A reconcile that fails leaves no trace outside the operator's log

**Opened:** 2026-08-31, from the 2.22b walk. Three consecutive runs had the
operator 403 on every claim delete, every thirty seconds, and the only place
that was visible was `kubectl logs deploy/apprafter-operator`.

**Status:** FIXED 2026-09-01 (2.22h). `status.recentProblems[]` + yellow lines
in `apprafter app status`, plus a cluster-wide roll-up in
`apprafter platform status` naming every application currently reporting
problems; walk-proven in `e2e/needs-removal-walk.sh` phase 9. Events are
deliberately unused (the ledger is an in-memory map flushed to the object) —
see the correction under "The proposal" below.
**Severity:** high — not because any single failure is severe, but because it
makes every future failure cost a log read to find.

### The distinction that matters

It is not that the platform surfaces nothing. **Designed** failure modes are
surfaced well: `AwaitingResourceClaim`, `EnvSecretMissing`, `ImageResolved`,
`PublicRouteReady`, `MigrationPending` all reach `status.conditions` and are
rendered by `apprafter app status`.

What vanishes is the **undesigned** failure — the one that reaches
`error_policy` through a `?`. And those are precisely the ones nobody
anticipated, so they are the ones worth reporting.

`error_policy` (`operator-controllers/application/src/lib.rs:1018-1031`) is the
whole of it:

```rust
warn!(%name, %namespace, %err, "reconcile error");
ctx.metrics.reconcile_total.with_label_values(&[KIND, &namespace, "error"]).inc();
ctx.metrics.reconcile_errors.with_label_values(&[KIND]).inc();
Action::requeue(Duration::from_secs(30))
```

A log line, two counters, a retry. No Event, no condition, no status write.
There are **48** `?`/`return Err` sites in that reconcile funnelling into it,
and **2** `recorder.publish` calls in the whole controller — both advisory
notices about design-time choices (`SoftDestructiveChange`,
`SelectorChangeUnderMultipleProviders`), neither about a failure.

The Prometheus counter is real but answers the wrong question: it says *how
many* errors, cluster-wide, never *which object* or *what happened*.

### What it cost, concretely

The 2.22b walk. The prune issued a delete, the apiserver refused it 403
because the branch RBAC had not reached the cluster, the operator warned and
retried — correctly, per the ADR 0048 lesson that a decorative failure must
not freeze a reconcile. The result was a **silent no-op**: `app status` said
Ready, the Application's conditions were clean, and the claim simply never
went away. Three runs and a log read to find it.

The same shape has now cost this project four times: the ADR 0048 anchor 403,
the 0.2.31 MigrationPlan GC, D15's plan rejection loop, and this. Every one
was a repeating error visible only in the log.

### The proposal

> **Corrected during implementation.** The premise below — that an Event from
> `error_policy` is the single bottleneck covering every path — is WRONG, and
> was disproved before any code was written. The incident that opened D16 never
> reaches `error_policy`: `prune_orphaned_claims` warns and returns `Ok(())`, as
> ADR 0048 requires, and four other sites share that shape. A design hooked only
> on `error_policy` would have shipped D16 without covering D16. What shipped is
> an in-memory ledger callable from BOTH classes — `error_policy` is synchronous
> and cannot await, but it can mutate memory, which is what a ledger is.

Keep the last few problems on the object, with expiry, and print them where an
operator already looks — the owner's framing, and it maps onto machinery that
mostly exists.

- **Emit a Kubernetes Event from `error_policy`.** It is the single choke
  point: every one of those 48 paths passes through it, so one change covers
  them all — the same leverage as classifying at the kubectl/restic choke
  points in 2.22a. Events bring dedup, `count`, `firstTimestamp`/`lastTimestamp`
  and apiserver-side TTL for free, so "predict expiry" needs no new mechanism.
  **RBAC note:** the operator holds `create` on events and nothing else
  (`rbac.yaml:521-526`); reading them back needs `get`/`list`, and that verb
  ships with the code that needs it.
- **Keep a bounded `status.recentProblems[]`** alongside, because the event TTL
  (an hour by default) is shorter than the interval of a slow-recurring
  failure, and because a condition on the object survives an event GC that a
  reader cannot control. Bounded — last N distinct reasons with `lastSeen` —
  so it cannot grow without limit, and pruned by age so a fixed problem does
  not linger as a scar.
- **Print both in `apprafter app status` and `apprafter platform status`.** No
  CLI surface reads Events today at all (a repo-wide grep for an events read in
  `cli/` returns nothing), so the platform emits two events that no first-class
  surface has ever shown.

Two things to get right. A recurring error must **deduplicate rather than
spam** — thirty-second retries would otherwise write a status update twice a
minute forever, which is its own kind of damage. And a problem that stops
recurring must **age out on its own**, or the surface becomes a list of things
that used to be wrong, which readers learn to ignore.

Ships with a walk assertion that a deliberately-induced reconcile failure —
the 403 is the obvious fixture, since it is reproducible by withholding one
RBAC verb — shows up in `app status` without reading a log.

## D17. A Dragonfly tenant can read every other tenant's key counts

**Opened:** 2026-08-31, found while investigating whether Dragonfly reports
per-database size for D8. Not the thing being looked for, which is how this
kind of finding usually arrives.

**Status:** FIXED 2026-08-31 (2.22f, ADR 0042 §11) — and the finding was
larger than recorded here. `PUBSUB CHANNELS` output is not filtered by the
user's `&{user}:*` patterns, and channel names carry the Kubernetes namespace
and application name, so it returned every pub/sub tenant's IDENTITY, not just
key counts. Both `+info` and `PUBSUB` are dropped. `CLIENT` remains granted and
cannot be scoped to subcommands at this version — recorded as an accepted risk
in ADR 0042 §11 rather than inherited silently.
**Severity:** medium. Metadata only — no keys, no values — but it is an
isolation boundary the platform claims and does not hold.

### What is wrong

Per-claim ACL users are granted `+info`
(`resourceclaim-provisioner/src/dragonfly.rs`, the `acl_setuser_args`
vector). `INFO KEYSPACE` on Dragonfly v1.37.0 iterates **every** database on
the instance regardless of which one the connection selected
(`src/server/server_family.cc:3429-3444` loops `m.db_stats` for all `i`),
and the ACL does not filter its output by database.

The `$N` isolation ADR 0042 built is real for data — a tenant cannot read
another's keys — and does not extend to this. On a shared pool instance every
tenant can see, for every other tenant: key count, expiring-key count, hits,
misses and hit ratio.

### What it discloses

Not data, but not nothing. Key counts and hit ratios over time are a usage
signal: they show whether a neighbour is growing, idle, or being hammered.
On a platform whose whole shape is "several tenants share one instance", the
fact that tenancy is visible at all is the part worth fixing.

`POOL_INSTANCE_INDEX` is hardcoded to `0`, so a cluster runs one ephemeral
and one persistent instance and *every* redis tenant shares them. The blast
radius is the whole cluster, not a subset.

### The fix, and what it costs

Dropping `+info` is the obvious move and it is not free: `INFO` is how a
client library discovers server capabilities, and some drivers call it on
connect. Removing it blind would break tenants for a metadata leak.

Dragonfly's ACL cannot scope `INFO` to a section or a database, so the
options are: drop `+info` entirely and document it; grant it and accept the
disclosure, recorded rather than unnoticed; or wait for upstream to filter
`INFO KEYSPACE` by the selected DB and pin the version that does.

Worth deciding deliberately rather than inheriting. It should not block
2.22d, and it should not stay unwritten either.

*Re-filed 2026-08-31:* the index had this under **2.22c**, which closed on the
secrets work without touching it. Its home is **2.22f** — that subphase is
already an ADR-first decision about the same ACL vector, so the `+info`
question is decided there rather than inherited.

### Related, same investigation

Dragonfly exempts `/metrics` from HTTP auth by an explicit special case
(`src/server/main_service.cc:2886-2899`: `if (path == "/metrics") return
true;`). Today that endpoint is only on the admin port 9999, because
dragonfly-operator v1.5.0 hardcodes `--primary_port_http_enabled=false`, and
9999 is not in the operator-created Service. So it is not currently exposed —
but anyone who flips that flag to reach the metrics publishes every tenant's
`dragonfly_db_keys{db=…}` unauthenticated on the tenant-facing port. That is
worth a comment wherever someone would be tempted, which is exactly where
this platform will be tempted if it ever wants those metrics.

## D18. Two git-ownership guards could never fire

**Opened:** 2026-08-31, found by the 2.22e walk asserting the field manager
that owns the pin annotation. The assertion read `<none>` while the write had
plainly succeeded.
**Status:** FIXED 2026-08-31 (2.22e), same day, in the change that found it.
**Severity:** medium. No data loss and no wrong behaviour — but two warnings
that exist specifically to stop a user writing something Git will silently
revert had never once been reachable, so the failure they guard against
produced no warning at all.

### What is wrong

**`kubectl` strips `metadata.managedFields` from `get -o json` by default.** It
has since 1.21, to keep output readable; `--show-managed-fields` restores it.
Measured on the walk's own cluster: the same object read without the flag
reports zero `managedFields` entries and with it reports three.

Both of this repository's field-ownership guards read `metadata.managedFields`
out of `kubectl_get_json`, which does not pass the flag:

- `egress_field_appears_git_managed` (`platform.rs`), shipped in **2.10**, warns
  that an infra repository declares `spec.network.egress.profile` so
  `apprafter platform egress set` will be reverted on the next sync.
- `pin_appears_git_managed` (`app.rs`), written in **2.22e** hours earlier,
  refuses to write an image pin the user's own manifest declares.

Both saw an empty list, concluded nobody owned anything, and returned `false`
every time.

### What it costs

The guard's whole job is to catch the case where the CLI would report success
for a write that Git reverts within a sync. With the guard dead, that case
produces a cheerful `✓` and then silently un-does itself — which is worse than
having no guard, because the message trained the operator to expect a warning
that could not come.

The 2.10 one has been dead since it shipped. Nothing detected it: it is
unit-tested against hand-built JSON that carries `managedFields`, so the tests
pass and prove only that the predicate is correct given input it never receives.

### The fix

`kubectl_get_json_showing_managed_fields` — the same getter with the flag — and
both call sites moved onto it. Kept as a separate function rather than making
the flag universal: every other caller would pay a larger payload for a field
it does not read.

### The shape worth carrying

This is the D10 shape at a different address: a signal built end-to-end, unit
tested, and unreachable in production because one layer below the tested
boundary discards its input. In D10 it was a hardcoded `false` at the only
call site; here it is a CLI flag nobody passed. **A unit test that constructs
the input cannot tell you the input never arrives** — only a live probe can,
which is why the walk asserted the field manager rather than just the
annotation value.

## D19. The size sampler deletes a live claim's whole allocation

**Opened:** 2026-08-31, by an adversarial verifier checking an unrelated D5
claim. It was looking at whether `status.ready` is sticky, noticed that the
2.22d sampler applies a partial status under the provisioner's own field
manager, and said so.
**Status:** FIXED 2026-08-31 (2.22f), the same day, before the subphase that
found it did anything else.
**Severity:** **critical, and self-inflicted.** Data loss plus a tenant
isolation breach, on every provisioned claim, within one tick of an upgrade.
Introduced by 2.22d and never shipped.

### What is wrong

Server-side apply REPLACES a field manager's owned field-set on every apply. It
does not accumulate. So a body carrying only `status.size`, applied under the
manager that also owns the claim's allocation, deletes the allocation.

2.22d added two such applies — `refresh_claim_size` (Postgres, on the 60s ready
gate) and `refresh_claim_keys` (redis, on the 300s ACL loop) — both using
`apply_params()`, which is `PatchParams::apply(FIELD_MANAGER)`, the same manager
that writes the terminal status.

Measured on a real apiserver (Kubernetes 1.35, this repository's own CRD).
Before:

```text
{conditions, connectionSecretRef, dbnum, instance, ready: true}
```

After one size-only apply under the same manager:

```text
{size: {keys, measuredAt}}
```

Nothing else survives.

### What it costs

The consequences compound rather than stopping at a missing field.

`should_provision` returns false only while `status.ready == true`. Prune
`ready` and a live, working claim is re-provisioned. Allocation `FLUSHDB`s the
logical DB as its recycle-safety invariant — **so the tenant's data is
destroyed**. And the freed `dbnum` returns to the pool while the original
tenant's application is still connected to it, which is precisely the 2.6
isolation breach that commit `7890d9e` was written to close.

The trigger is not rare. `keys_write_is_worth_it` and `size_write_is_worth_it`
both return true on a first sample, so this fires on **every existing claim**
within one tick of the upgrade that carries 2.22d.

### Why nothing caught it

The codebase already documents this exact hazard — `status_apply_body`'s
doc-comment in `reconcile.rs` explains, at length and correctly, that a terminal
apply omitting `instance`/`dbnum` would prune the allocation and name the
consequences. 2.22d added a new writer at a new address and did not apply the
lesson.

Every gate passed. The unit tests exercise the deadband predicates and the
body shape, both of which are correct — **the defect is which manager carries a
correct body**, which is not observable in process. `cargo test`, `clippy`, the
CRD check and the docs gate have nothing to say about field-manager choice. No
walk covers it either: the size feature shipped without a walk step (recorded
against D8 as a debt at the time), and even a walk that asserted the size
appears would have passed, because the size *does* appear — it is everything
else that vanishes.

### The fix

A dedicated field manager, `resourceclaim-provisioner-size`, for the sample and
nothing else. SSA then merges rather than prunes: the size manager owns exactly
`status.size`, and the provisioner keeps owning the allocation. Verified on the
same apiserver — the full status survives, and a second sample updates the size
without disturbing it.

This is the `apprafter-cli-egress` rule (2.10) and the `apprafter-cli-pin` rule
(2.22e / ADR 0059) at a third address: **a partial apply gets its own manager,
and that manager writes nothing else, ever.**

### The shape worth carrying

Three defects in this file now share one root: **SSA field-set replacement is
not intuitive, and the codebase keeps re-learning it.** 2.10 (egress pruned
`source`/`values`), 2.22e (the pin manager, designed correctly *because* 2.10
was remembered), and now 2.22d's sampler, written by someone — me — who had
just read the doc-comment warning about it while fixing `status.image` for the
very same reason in 2.22e.

Reading the warning was not enough. The guard that would have helped is a test
that asserts the *wiring*, which is what shipped with this fix: any
`patch_status` whose body mentions `size` must use `size_apply_params`. It was
verified by reintroducing the defect and watching it name the offending line.

---

## D20. An approved migration can sit unexecuted, with no requeue behind it

**Opened:** 2026-09-01, by the second run of the 2.22h `needs-removal` walk.
**Status:** OPEN, and deliberately not "fixed" — see the evidence gap below.
**Severity:** low frequency, high confusion. Observed once in five runs; when it
happens the user's change simply never takes effect and nothing says why.

### What was seen

The walk approved an app-scope `MigrationPlan` and then waited five minutes for
the gate to open. It never did. Across that window the Application controller
reconciled the app repeatedly and never logged the prune, so the removed need's
claim survived and the walk failed downstream — on an absent `RetainedClaim`,
which is three steps away from the actual stall.

Runs 3, 4 and 5 drove the identical path in **five milliseconds**:
`pending-approval → approved → executing → completed`.

### The evidence gap, stated plainly

The failing run's diagnostics tail the operator log at 120 lines, and that
window began *after* the approval. So the MigrationController's own lines for
that plan — the ones that would say whether it ever saw the approval — are not
in the record. **The cause is unproven.** What is established is that the gate
stayed shut for five minutes on a path that normally opens instantly.

### Why it is plausible rather than dismissable

`reconcile.rs`'s `pending-approval` arm returns `Action::await_change()`: no
requeue, no floor. The only thing that moves a plan out of that state is a watch
event for the approval patch. If that event is missed, nothing retries it on a
timer — recovery depends entirely on the watcher's next relist. Every other
gate in this operator that can strand a user's change carries a requeue.

### What would settle it

A periodic requeue on the `pending-approval` arm (say 60s) would make a missed
event self-healing and cost one no-op reconcile per plan per minute, for objects
that are rare and short-lived. That is a small, safe change — but it should be
made against a reproduction, not against a five-minute observation, or the
"fix" is unfalsifiable. The walk now fails **at the gate** with the stalled
phase named, so the next occurrence will be recorded properly instead of
surfacing as a retention-path timeout.

### The shape worth carrying

The walk blamed the wrong subsystem for five minutes because it asserted an
*effect* three steps downstream of the thing that stalled. The assertion added
here — gate opened, before anything that depends on the gate — is the general
form: **assert the precondition you are about to rely on, at the point you
start relying on it.**

### The second half, added on request (2026-09-01)

The per-application surface answered "what is wrong with THIS application". It
did not answer "is anything wrong at all", and finding that out meant running
`app status` once per application — which is the same log-reading cost this
defect was opened to remove, wearing a different hat.

`platform status` now carries the roll-up. Three of its design choices are
deliberate inversions of the `pinned_app_rows` precedent it sits beside,
because that one is decorative and this one is a health signal:

* **It prints when nothing is wrong.** The per-application surface deliberately
  does not — there, the surrounding output already proves the command ran. In a
  cluster roll-up an absent section is indistinguishable from "the check did
  not run" and "this CLI is too old to have the check". Silence is not an
  answer to "is anything wrong?".
* **It is loud when it cannot read.** Copying the precedent's silent
  `else { return; }` would render an RBAC denial, an apiserver timeout, or a
  missing CRD as a clean bill of health.
* **It names applications the way the reader must type them.** The CR's
  `metadata.name` is author-chosen CUE and is NOT the argument `app status`
  takes; a row naming it would send the reader to a command that errors. The
  join is the one `app status` already performs, run backwards. An application
  that does not resolve through it is listed and labelled, never dropped —
  those are the ones most likely to be broken.

**The invariant that made this safe to add:** both surfaces call one filter
(`live_problems`), so the roll-up cannot name an application whose own
`app status` prints nothing, or stay silent about one that would. Re-deriving
the horizon in the roll-up would have made the first retune of that constant
produce exactly that contradiction. A test asserts the agreement across all
four filter cases and was verified by reintroducing a divergence.

---

## D21. A claim deleted twice inside its grace window wedges forever

**Opened:** 2026-09-01, by the 2.22 local e2e battery, on `needs-disk-walk`.
**Status:** FIXED the same day, with a source-level wiring test and a
deterministic walk phase (`needs-disk-walk` Phase 9b).
**Severity:** high. A hard deadlock with no user-facing recovery: the
Application never becomes Ready again and clearing it requires a human editing
a finalizer off an object by hand.

### What is wrong

`snapshot_retained_claim` runs on the deletion path, BEFORE the provisioner
releases its finalizer, and it applied the snapshot blind. Its comment stated
the assumption plainly:

> an idempotent SSA-apply of a deterministic-named object — a crash before
> un-finalizing simply re-applies the byte-identical RetainedClaim

Every clause of that is true except the last one, and the last one is the one
it relies on. The snapshot's name is deterministic per (namespace, claim), a
`RetainedClaim` spec is **immutable** by admission (CEL `self == oldSelf`), and
`retainUntil` is derived from **this** deletion's `deletionTimestamp`. So a
second, genuine deletion is never byte-identical — its timestamp differs by
construction:

```text
RetainedClaim.apprafter.io "claim-demo-sqlite-disk" is invalid:
  spec: Invalid value: RetainedClaim spec is immutable
```

The apply is upstream of the finalizer release, so the claim never finishes
deleting. It sits in `Terminating`; its Application sits at
`Ready=False: paused awaiting ResourceClaim provisioning`; the reconcile
retries the same 422 every thirty seconds forever.

### How it is reached

A re-provision cancels the surviving snapshot — that is what the
cancel-on-reprovision arms are for. The window is a deletion that arrives
before an intervening provision completes: delete, recreate, delete again in
quick succession. Argo sync churn, a fast `app remove` / `app add`, or a
rapidly re-applied manifest all produce it.

Observed as two deletions **42 seconds apart**: the claim carried
`deletionTimestamp: 16:49:55` while the surviving snapshot's `retainUntil` had
been computed from `16:49:13`.

### Not disk-specific

The disk walk is simply where the churn happened. The deterministic name and
the `retainUntil` derivation are shared by every backend, so a pg or redis
claim deleted twice inside grace wedges identically. `needs-pg-walk` and
`needs-redis-walk` were green throughout because each deletes exactly once.

### Why nothing caught it

`needs-disk-walk` had not been run since 2.6b — the code was last touched in
2.16b and the walk in the meantime only ever deleted once per run. Neither the
unit suite nor the CRD gates can see it: the rejection lives in admission, and
a unit test over the built body passes, because the body is correct. What was
wrong was *whether the write should happen at all*.

### The fix

Create-if-absent. `GET` first; if a snapshot exists, log it and return without
applying. A lost creation race (both reconciles see absent) is tolerated on
409/422 rather than propagated, since the outcome — a snapshot exists — is
what the finalizer release actually requires.

**The existing snapshot wins deliberately.** It already points at the same
retained resources, and the grace clock must run from the FIRST deletion: a
second delete silently restarting a seven-day window is how "seven days"
becomes unbounded.

### The shape worth carrying

This is the fourth defect in this file whose root is *a write that should not
have happened*, and the second where a comment asserting idempotence was the
thing that stopped anyone checking. An SSA apply is idempotent only if every
input is; `now()` and a deletion timestamp are not inputs you can assume
stable. Where an object is immutable by design, the write must be
create-if-absent, not apply-and-hope.

---

## D22. The disk half of D8 never populated, and no local walk could have noticed

**Opened:** 2026-09-01, by the 2.22 local e2e battery.
**Status:** FIXED the same day — one product fix, one harness fix, and an
assertion in `needs-disk-walk` that fails without either.
**Severity:** medium. No data is at risk; a shipped signal simply never
appeared, and the surface that was supposed to carry it read as "nothing to
report" rather than "not measured".

### What was wrong, in the product

`refresh_claim_size` opened with `if claim.spec.type_ != "pg" { return; }`. So
of D8's three figures only two ever refreshed:

* **pg bytes** — the 60s ready-gate. Worked, once its RBAC was in place.
* **redis keys** — the 300s ACL resync loop, a separate path. Worked.
* **disk used/total** — sampled in exactly ONE place, inside `provision_disk`.

That call runs once, at provisioning, and the kubelet reports volume statistics
only for volumes it has **mounted**. At provisioning no pod exists yet — the
Application is still paused waiting for the claim — so the sample is always
`None`. A ready claim never provisions again, and the 60s gate returned early
for every non-`pg` type. `status.capacity` therefore never populated on any
cluster: the one D8 figure with a meaningful denominator, the one `volume
status` and the `app status` claims table are built to show.

The fix gives disk its own arm on the same 60s gate, with a materiality-only
deadband — `ClaimCapacity` carries no `measuredAt`, so the staleness clause the
size arm uses has nothing to read and would degrade into an unconditional write
every tick.

### What was wrong, in the harness — and this is the more useful half

The pg figure did not appear either, and the reason was **not** in the product:
`pods/proxy` was forbidden in `cnpg-system`. The verb is granted in the branch
chart, in the same commit as the code that needs it. But
`APPRAFTER_E2E_LOCAL_OPERATOR` swaps the operator **image** while the cluster
keeps the RBAC the **published** chart installed — and 0.2.59 is not published.

So every local walk ran a new binary against an old ClusterRole. Any rule added
alongside its code was structurally unverifiable locally, which is precisely
the class this repository has already been bitten by twice (ADR 0048's anchor
403, the 0.2.31 MigrationPlan GC 403) and whose lesson was recorded as "only a
live cluster catches it". It need not have been: the walks apply the branch
CRDs and simply never applied the branch RBAC.

`e2e/lib.sh` now has `apply_branch_operator_rbac`, wired into all eight walks
that already apply branch CRDs. `-n apprafter-system` is load-bearing —
without it `helm template` renders the ClusterRoleBinding subject into the
wrong namespace and the binding grants nothing (walk-fix `3ac1972`, learned
once already).

### Why nothing caught it

No walk read `status.size` or `status.capacity`. The pg walk asserted
`spec.size` — the REQUESTED size — which is a different field with a similar
name, and it passes whether or not any sampler works. The assertions added
here read the sampled figures, and the disk one is deliberately placed after
the pod has mounted the volume, because before that an absent figure is
correct rather than a defect.

### The pg half: a correction to this entry's own first diagnosis

The Postgres figure did not appear either, and the first two explanations
written here were both wrong. Recorded because the wrong turns cost four e2e
rounds and the reasoning is the reusable part.

1. *"`pods/proxy` is forbidden"* — true, and fixed by applying branch RBAC, but
   it was only the outermost of three layers.
2. *"`cnpg_pg_database_size_bytes` is not a CNPG default, so declare our own
   monitoring ConfigMap"* — **wrong, and the fix made things worse.** The
   evidence for it was that the metric listed `[app, postgres, template1]` in
   one run and was absent in the next. The custom ConfigMap that followed
   reused the query name `pg_database`, which collides with CNPG's own default
   query, and carried no `cnpg.io/reload` label; after it the metric vanished
   entirely. Reverted.

The actual cause is the **scrape cache**. `MetricsCache` holds a body for 300s
and every tenant on that backend shares it, so a claim provisioned a minute ago
asks about a database that did not exist when the body was captured — and a
cached body that predates an object is indistinguishable from a metric that is
not published. That is also real user-facing behaviour, not just a test
artefact: a freshly provisioned database would report no size for up to five
minutes.

The fix is one re-scrape: on a miss, bypass the cache ONCE and look again. If
the value is still absent from a body taken just now, it is genuinely absent.
Live-proven — `sampled database size = 8017599 bytes`.

### The shape worth carrying

Three D-entries in this file are now the same sentence: **a signal was built,
unit-tested, and never reached production** (D10's hardcoded `false`, D18's two
dead guards, and this). The common cause is not carelessness in the code — each
was carefully written — it is that nothing downstream ever asserted the
*output*. A figure that no test reads is indistinguishable from a figure that
is never produced.

And a second, sharper one from the wrong turns above: **when a diagnosis is
built on "the metric is absent", check what else could make it absent before
changing the producer.** Two of the three layers here were caching and
permissions — neither of them anything to do with the metric.

---

## D23. One object's data can crash-loop the entire operator

**Opened:** 2026-09-01, by the 2.22 local e2e battery — a walk FIXTURE
triggered it, which is the only reason it was seen at all.
**Status:** FIXED the same day at both sites, with unit tests at each.
**Severity:** **high.** Not a wrong value, not one broken reconcile: a panic on
a tokio worker takes the whole process down, so every controller in the
operator stops — Application, PlatformStack, provisioner, scheduler,
SourceCredential — for as long as the offending object exists. A cluster
recovers only when a human finds and edits that object.

### What is wrong

kube-runtime schedules requeues through a tokio `DelayQueue`, which **panics**
on a deadline it cannot represent:

```text
thread 'tokio-rt-worker' panicked at kube-runtime-0.95.0/src/scheduler.rs:100:43:
invalid deadline; err=Invalid
```

Two places computed a requeue from DATA and handed it over unbounded:

1. **`gc.rs`** — the remaining grace on a `RetainedClaim`, derived from its
   `retainUntil`. A snapshot dated 2031 produced `requeue_secs=136698895`
   (4.3 years) and crash-looped the operator: five restarts in seven minutes,
   `CrashLoopBackOff`.
2. **`platform-stack/reconcile.rs`** — `parse_check_interval`, which reads
   `spec.checkInterval` (a user-facing string) and did `value * 3600`. A single
   mistyped `checkInterval: "999999h"` reaches the same panic, and the
   multiplication itself overflows before it gets there.

Production values never approach either bound — `retainUntil` is always
deletion + 7 days, and a sane poll interval is minutes. That is exactly why
the guard is worth having: the values that DO get here are hand-edited
objects, clock skew, restores from an old backup, and typos — the situations
where the operator most needs to stay up.

### How it was found, and what that says

Not by review, and not by any assertion aimed at it. The 2.22 walk phase for
D21 plants a `RetainedClaim` with a far-future `retainUntil` to prove the
finalizer still releases; the fixture crashed the operator, and the walk
reported the symptom three steps downstream ("the claim is still present after
180s"). The panic was visible only in the pod's previous-instance log.

The general lesson is uncomfortable and worth stating: **an adversarial value
in a test fixture found a production crash that no amount of reading the code
had.** The two clamps are one line each; nobody wrote them because nobody
imagined the number being large, and the type system had nothing to say.

### The fix

A ceiling at each site — one hour for the grace requeue, one day for the check
interval — plus `saturating_mul` on the interval parse. Both are documented as
crash guards, not tuning knobs, so a future reader does not "optimise" them
away. The 2031 fixture stays in `needs-disk-walk` Phase 9b: it now doubles as
the live regression guard for the clamp.

### Worth auditing next

Every remaining `Action::requeue` whose duration is computed rather than
constant. Two were found here by grep; the audit is cheap and should be
repeated whenever a new one is introduced.

---

## D24. A walk built an artefact and tested a different one

**Opened:** 2026-09-01, by the 2.22 local e2e battery.
**Status:** FIXED (harness). No product change — the product was right and the
walk was pointing at the wrong field.
**Severity:** low impact, high consequence-if-unnoticed. Nothing was broken;
the local backup walk simply never exercised the code it spent four minutes
building.

### What was wrong

`backup-s3-sequential-kind` builds the `apprafter-backup` runner image from the
working tree and then merge-patched `PlatformStack.spec.backup.image`, with a
comment describing that field as "the dev/fork escape hatch".

That field does not exist. It is absent from `schemas/v1alpha1/platformstack.cue`,
from the generated CRD, and from the operator's `BackupConfig`. The apiserver
accepted the patch and pruned the key, so the CronJob kept running the
**published** runner image while the walk asserted — against an empty string —
that it was running the local one.

The chart reads the runner image from `.Values.backup.image`
(`render_tool.cue`), and `PlatformStack.spec.values` is the passthrough for
chart values — the same route 1.83f used for `gateway.allowedDomains`. The walk
now patches there.

### Why it matters more than it looks

Every local run of the backup runner was testing a binary from a published
release rather than the one in the tree. A change to the runner would have
passed this walk without ever executing.

That is the same shape as the branch-RBAC gap recorded under D22 — a walk that
looks like it exercises local code and does not — and the two were found in the
same afternoon by the same means. Together they suggest a standing check worth
making explicitly:

> **When a walk builds an artefact, it must assert that the cluster is running
> THAT artefact** — by digest, tag, or a value the artefact alone produces.
> "I built it and applied something" is not the same claim.

### Correction: there is no override at all, and my first fix was worse

Routing the image through `spec.values.backup.image` — the passthrough 1.83f
used for `gateway.allowedDomains` — does not work either, and actively breaks
the cluster. The operator projects `spec.backup` into `.Values.backup` itself,
so writing `spec.values.backup` COLLIDES with that projection and hands the
chart a `backup` values object carrying nothing but an image. The platform app
then goes `SyncError: one or more synchronization tasks are not valid` and no
CronJob is rendered at all. Reverted.

`spec.overrides` is not a route either: it is per-component, and backup is not
a component — the platform-stack chart renders it directly.

So the honest statement is stronger than the original finding: **the backup
runner image cannot be overridden by any supported mechanism.** Not a CLI flag,
not a CR field, not chart values, not a component override. A fork or a
locally-built runner cannot be exercised anywhere, which is why no local walk
can test a change to it. Closing that needs a real `image` field on
`BackupConfig` (CUE + CRD + Rust), not a harness trick. The walk now asserts
the CronJob EXISTS and carries some runner image, and says in its own output
that the local build is not what runs.

### And the reason that walk had never passed locally

`backup enable` makes the chart render a **CiliumNetworkPolicy**
(`apprafter-backup-egress`, gated on `backup.enabled`). The kind walks bootstrap
with `APPRAFTER_BOOTSTRAP_SKIP_CILIUM=1`, so the CRD does not exist, so the
resource cannot be applied — and Argo fails the WHOLE root sync over it, taking
the backup CronJob with it:

```text
CiliumNetworkPolicy apprafter-system/apprafter-backup-egress -> SyncFailed
The Kubernetes API could not find cilium.io/CiliumNetworkPolicy
```

Not a product defect: every AppRafter cluster runs Cilium by construction, and
the policy is correct. It is a structural incompatibility between this walk and
the no-Cilium kind bootstrap, and it is why the walk was red before any of my
changes. The path to green is `kind_up_cilium` + `bootstrap_with_cilium` under
`sandbox-run` (the recipe `needs-networkpolicy-walk` already uses) — a
substantial change to a two-cluster walk, and OWED rather than done.

That generic Argo message is worth its own note: "one or more synchronization
tasks are not valid" names nothing. The per-resource `syncResult` carries the
real reason, and printing it turned three rounds of guessing into one run.

### Related

The 2.22g read-back guard is what surfaced the neighbouring half of this: when
the same walk ran against the published CRD, `backup enable --timezone` refused
with "the cluster did not store the timezone (it reads back as None)" instead
of silently losing it. The guard worked exactly as designed, on the first
cluster that could disprove it.

---

## D25. The real-Hetzner backup walk is blocked until 0.2.59 publishes

**Opened:** 2026-09-02, running the owed Hetzner batch.
**Status:** RESOLVED the same day. 0.2.59 published, the walk re-ran, and it is
**GREEN end-to-end on real Hetzner** — 43 assertions, zero failures. Kept here
because the reasoning is the reusable part, not the outcome.

The full disaster-recovery loop is now proven on hardware: backup enabled with
`--at 22:30 --timezone Europe/Berlin`, the CronJob wired and triggered, a
snapshot written to S3, the **source cluster destroyed**, then
`restore --reprovision` onto a genuinely new box (server id 164332736 →
164333316) where the CMS came back Ready, the re-sealed `PAYLOAD_SECRET`
decrypted to its original value, and the known marker row was identical to the
one written before the destroy. 2.22g's own content rode along and is visible
in `backup status`: `daily at 22:30 Europe/Berlin`.

`backup-s3-hetzner.sh` provisions against **published** artifacts on purpose:
its whole value is validating what a user actually gets from the channel. But
2.22g's `spec.backup.timeZone` ships in 0.2.59, which is not published, so the
cluster's PlatformStack CRD does not have the field — and `backup enable
--timezone` correctly refuses:

```text
the cluster did not store the timezone 'Europe/Berlin' (it reads back as None).
This operator's PlatformStack CRD predates the `spec.backup.timeZone` field,
so the apiserver accepted the write and discarded it.
Upgrade the platform, then re-run this command.
```

The walk reached this after eleven minutes with everything before it green:
target registered, cluster bootstrapped, CMS app deployed with a `secret:` ref,
pg claim ready, a known marker row written, and the S3 credential Secret sealed
and unsealed in `apprafter-system`.

So the sequence was fixed: **publish 0.2.59, then run this walk** — which is
exactly what happened. On kind the same field is reachable because the walks
side-load the branch CRDs; on real hardware that would defeat the walk's
premise, so waiting was the only correct move.

One transient on the way, worth naming so it is not mistaken for a defect next
time: Argo CD failed to clone the public CMS repo with `could not read Username
for 'https://github.com'`, which is what GitHub returns to an anonymous request
it is refusing. The repository is public and `git ls-remote` answered
anonymously from the same machine minutes later; the next run cloned it fine.

### What this DOES prove

2.22g's read-back guard fired on the first real cluster that could disprove it,
in exactly the situation it was written for — a CRD without the field, where
the apiserver accepts the write and silently discards it. Twice now, in two
independent walks. Without it, the backup would have been scheduled in the
kube-controller-manager's zone with nothing anywhere saying so.

### Two walks could not provision at all, for an unrelated reason

Both `vpa-walk.sh` and `backup-s3-hetzner.sh` called `target add` without
`--server-type`. Since 2.16h-a removed the implicit default (Decision 0),
`apply` refuses with `server_type_not_selected` — so neither walk could reach
its first assertion, on any cluster, since that change shipped. That is the
concrete reason 2.22d recorded the VPA walk as "code ready, never run": it was
not merely unrun, it was unrunnable. Both now pass the SKU explicitly, with the
same default and override as `mvp.sh`.

---

## D26. A `sequential` backup restores empty, and says it succeeded

**Opened:** 2026-09-02, by `e2e/backup-s3-sequential-kind.sh` — the first run
in the walk's life to reach its own subject.
**Status:** FIXED 2026-09-02, and the walk that found it is now GREEN
end-to-end — 33 assertions, both clusters, `merging 2 per-claim snapshot(s)`
followed by the known row surviving into the fresh cluster.
**Severity:** high for anyone who sets it, none for anyone who does not.
`stagingMode` defaults to `monolithic` (`schemas/v1alpha1/platformstack.cue:92`,
`backup.rs:100`), and monolithic restores correctly — proven end-to-end on real
Hetzner the same day. But `sequential` is a shipped, documented opt-in
(`apprafter backup enable --staging-mode sequential`, and an enum value in the
CRD), and choosing it produces backups that cannot be restored.

### The evidence chain

The BACKUP half is correct, and the walk proves it:

```text
ok: restic lists 3 snapshots (>=3 for 2 claims + manifest)
ok: all sequential snapshots share exactly ONE run tag = 1
ok: the FINAL (latest) snapshot is the manifest/commit snapshot (carries manifest.json)
```

The RESTORE half never learned the format. `RestoreStep::RestoreArtifact`
(`restore.rs:240-246`) runs `restic restore` for exactly ONE snapshot — `latest`,
which is the commit/manifest snapshot — into a temp root, and `LoadData`
(`restore.rs:296-304` → `load_data` at :747 → `load_pg_dumps` at :761) then reads
`data/pg/<ns>/<claim>.dump` out of that extracted directory. It never invokes
restic itself. For a monolithic backup that is right: one snapshot holds
everything. For a sequential one the per-claim payloads are in the OTHER
snapshots, so `data/pg` does not exist, `load_pg_dumps` iterates an empty set,
and the restore finishes reporting success:

```text
✓ PlatformStack applied
✓ namespaces ensured: demo
✓ 1 app(s) applied gated (replicas=0, Argo auto-sync stripped)
✓ 2 claim(s) ready
✓ Restored backup of cluster 'platform' into target 'fresh'
  mode:       full
```

and the restored database is empty:

```text
psql said: ERROR:  relation "app_data" does not exist
```

`grep -rni sequential` over the whole restore path
(`platform-cli/src/commands/restore.rs`, `backup-core/src/restore.rs`,
`backup-core/src/extract.rs`) returns **nothing**. The concept does not exist
there.

### The fix

`RestoreArtifact` now resolves a RUN rather than a snapshot.
`backup_core::restore::resolve_run_snapshots` groups a `restic snapshots
--json` listing by the run tag: the requested snapshot (`latest`, or an id
prefix) is the commit point, and every other snapshot sharing a tag with it is
one of its per-claim payloads. The restore fetches the commit point, then each
sibling into its own temp root, and merges the payload trees into the data
directory the loader reads.

**Grouped by the run TAG, deliberately, and not by a new manifest field.** The
tag is already written by the backup engine, so this repairs backups ALREADY
SITTING IN REPOSITORIES. A manifest flag would only have fixed runs taken after
the fix — no use to anyone holding a sequential backup today.

Three properties the unit tests pin, because getting them wrong is worse than
the original defect: a monolithic run yields no siblings (nothing changes for
the default path); a second run in the same repository is never dragged in (or a
restore would load another backup's data over this one's); and an untagged
snapshot groups with nothing rather than guessing. The merge refuses to
overwrite an existing file for the same reason.

The per-claim payload layout is byte-identical to the monolithic one — the
writer reuses `run_extraction` on a one-element slice — so a plain merge is all
that is needed. A per-claim snapshot carries no `manifest.json`, so locating its
payload needs its own probe (`pg`/`redis`/`disk`) rather than `find_data_dir`.

### Why it took this long to see

The walk that tests it had never once reached Phase 5. It was blocked first by
an unguarded CiliumNetworkPolicy that wedged the whole platform sync (fixed in
platform-stack 0.2.60), then by its own fixture declaring the redis claim
ephemeral while demanding its data survive a restore, then by a psql readiness
race whose stderr was discarded — each failure masking the next. A backup format
shipped with a CLI flag and a CRD enum had no working end-to-end test, and the
first time one ran it found the format unrestorable.

**The pattern is the one this file keeps recording**: not a careless
implementation — the sequential writer is careful and correct — but an output
nobody ever read back. Same shape as D10, D18, D19 and D22.
