# ADR 0062: A manifest package is a bundle — one registration, one namespace, one environment

## Status

`Accepted` (2026-09-14).

ADR for subphase 2.27 (`plan.md` §2.27). It records a decision the product
had already made implicitly and never named, and which the CLI alone failed
to honour.

## Context

`apprafter app scaffold` emits a **named-wrapper** manifest —
`cli/platform-cli/templates/application/default.cue.hbs:17` is
`{{app_name_camel}}: v1alpha1.#Application & {`. Pasting a second wrapper
beside the first is the obvious next move for anyone with two services, and
every layer below the CLI already handles it:

- the cue-cmp sidecar enumerates every k8s-shaped top-level key and emits a
  multi-document stream (`argocd-cue-cmp/entrypoint.sh:457-509`), with a
  regression fixture for exactly this shape
  (`argocd-cue-cmp/testdata/inject-fixture-multi/`);
- the operator is fully multi-CR-native — every child is named, laboured and
  owned off the CR's own `(name, namespace, uid)`, and it has no concept of
  the Argo CD Application at all.

The CLI does not. Its unit of registration is the **Argo CD Application**
(one per repo + path + environment), whose name is derived from the git repo
basename or `--name`. The user's unit is the **AppRafter `Application` CR**,
whose name they wrote in CUE. At N=1 the two coincide by convention. At N=2
they cannot, and three code paths collapse N to 1:

1. `cli/cli-core/src/manifest.rs:288-306` returns the first value whose
   `kind == "Application"`. `cli/Cargo.toml` carries `serde_json` without
   `preserve_order`, so `Map` is a `BTreeMap` and "first" is **alphabetical**,
   not declaration order — the opposite of what
   `argocd-cue-cmp/entrypoint.sh:461-463` deliberately preserves. The same
   function also cannot parse an unwrapped (Style A) manifest at all.
2. `cli/platform-cli/src/commands/app_open.rs:205-215`
   `find_apprafter_app_name` returns `Option<String>`. N is not
   representable in that type. Its own test,
   `find_apprafter_app_name_picks_first_when_multiple_apprafter_applications`,
   carries the comment *"Walk-fix #2 post-B.1.79b territory if operators
   actually do this; for now, take the first deterministic-order entry."*
3. `cli/platform-cli/src/commands/app.rs:891-925` `app list` enumerates Argo
   CD Applications. The second CR is present in the JSON the command already
   fetched (`status.resources[]`) and is discarded.

The consequence is not cosmetic. `app status <registration>` resolves the
first CR and scopes all five downstream reads — pods, services, claims,
secret bindings, advisories — to it, so a sibling in `CrashLoopBackOff`
prints as a clean healthy block. `app logs`, `app open` and the two **write**
verbs (`app rollback`, `app unpin`) silently act on that same first CR.
`app remove` prints `single_remove_prompt_line` (`app.rs:3037-3047`) — one
line, in the singular — before deleting a registration that owns N
production workloads.

This is live in this repository, not hypothetical: `landing/web/apprafter/`
is already a two-workload package (`Application.cue` → `landingWeb`,
`Application-preview.cue` → `landingWebPreview`), one namespace, one
registration. By name ordering the production CR wins every "first wins"
path, so `landing-web-preview` is unreachable from the CLI.

Separately, the same divergence produces a data-integrity defect that has
nothing to do with multi-app: `cli/platform-cli/src/commands/restore.rs:2945`
takes a **CR** name and `:3019` feeds it to the selector
`apprafter.io/application={name}`, whose value on the Argo object is the
**registration** name. The repo's own test at `app.rs:4756-4776` asserts that
those differ ("Argo CD app `cms` renders an AppRafter Application
`landing-cms`"). On a mismatch the selector returns empty,
`suspend_patches` (`restore.rs:2729-2751`) emits the scale-to-zero but not
the auto-sync-disable patch, and `restore --data-only` loads Postgres while
Argo CD self-heals the replica count and pods write to it — the exact failure
the function's own comment at `:2725-2727` exists to prevent.

## Decision

**A CUE package under `apprafter/` is a bundle. One package = one
registration = one namespace = one environment. Every workload in it is
added, synced and removed together.**

We will not give workloads inside a bundle independent lifecycles. A user
who wants two namespaces or two environments splits into separate
directories and registers each ([ADR 0063](0063-manifest-discovery.md) makes
that layout work).

### Vocabulary

No new user-facing noun. The bundle **is** "the application" — the thing
`app list` rows, the thing `app add` registers, the thing the positional
argument names. Each inner `apprafter.io/Application` CR is a **workload**,
which is already this repository's word for that object at that granularity
(`app.rs:1738` `Workload pods (…)`, `app.rs:1900` `Workload services (…)`,
`resolve_logs_workload`, `operator_workload_selector`,
`suspend_running_workloads`). "service" and "component" are both taken —
by Kubernetes `Service` objects two lines below where the word would appear,
and by `platform freeze <component>` respectively.

### Addressing

> The positional argument of every `app` verb is the **registration** name.
> It is never a workload name. A workload is addressed only by
> `--workload <name>`, a disambiguator in the same sense as `--env`.

This dissolves the collision by construction rather than by a precedence
rule. `derive_app_name` (`app.rs:3187-3206`) and the scaffold both default
to the repo basename, so "registration name equals a workload name" is the
normal state, not an edge. Under this rule no input can mean either.

When the positional fails to resolve, the error resolves it *as a workload*
and names the command that would have worked — the courtesy lives on the
error path, not in the grammar.

### Read surfaces

- `app list` keeps **one row per registration**. It gains `NAMESPACE` and a
  `WORKLOADS` count, drops `PROJECT` and `REV`, and strips the URL scheme
  from `REPO`. No time column.
- The `HEALTH` cell becomes **cardinal and pin-aware**, folded over the
  `apprafter.io/Application` entries of `status.resources[]` — rendered
  exactly as today when all N agree, so N=1 is byte-identical. This is
  required, not decorative: `platform-stack/cue/component_argocd.cue:158-162`
  already records that a pin is masked whenever a sibling is `Progressing`,
  naming *"a repository that renders several apps into one Argo
  Application"*.
- `app status <registration>` on a multi-workload bundle prints a **summary**
  — one row per workload with phase, pods and image — and points at
  `--workload` for detail. It does not dump N full blocks. A single-workload
  bundle is byte-identical to today, guaranteed by calling the same extracted
  function body rather than by copying it.

### Write surfaces

- `app remove` is the twin of `app add` and operates on the **whole bundle**.
  Removing one workload of many is **refused**, with the git steps printed:
  `selfHeal: true` (`app.rs:3690-3699`) makes any CLI-side CR delete an
  illusion that lasts one reconcile.
- `app rollback` / `app unpin` require an unambiguous workload. The
  git-revision branch of `rollback` moves the whole bundle and must say so.

### Consistency enforcement

Intra-bundle inconsistency is caught at the **render layer** — the cue-cmp
sidecar, with `apprafter app validate` as its local twin — and **not** at
the admission webhook. See [ADR 0063](0063-manifest-discovery.md) §Decision
for the enforcement table and the reasoning; the short form is that a
`ValidatingWebhook` receives one object per `AdmissionReview` and cannot see
a sibling, and giving it a cluster client under `failurePolicy: Fail` trades
a UX gap for an availability hazard this repository has already paid for once
(the ADR 0048 anchor-403 GitOps deadlock).

## Consequences

**Easier.** Every `app` verb answers about the thing the user wrote. The
second workload of a bundle stops being invisible. `app status` stops
reporting a clean bill of health for a broken sibling. The `restore
--data-only` suspend path gets a join that is correct by construction. A
monorepo with three services is one registration and one mental object
instead of three registrations the CLI cannot tell apart.

**Harder.** `app list`'s `NAME` column changes where the registration name
and the CR name differ — that is the one visible break, and it changes the
name to the one that works everywhere else. Removing a single workload now
requires a git push; this will be reported as a missing feature and is not
one.

**Neutral.** `app list` gains a cluster-wide `applications.apprafter.io`
read. `app_rollup.rs:52-62` and `secret.rs:160-163` already do exactly that,
so the RBAC and the cost are precedented — but `app list` is the most-run
command in the CLI, and this doubles its reads.

## Alternatives considered

**One Argo CD Application per workload.** Teach the sidecar to render a
selected subset (an `APPRAFTER_APP_NAME` plugin env var) and have `app add`
register N Applications. This is the only option that makes per-workload
environments, revisions and removal real. Rejected: it puts a second render
mode in the core render path selected by configuration, which is what spec
§1.1 exists to prevent; it cannot retire the first mode, so two registration
models coexist permanently behind a `--legacy-` flag plus a migration verb;
and its safety rests entirely on one guard — `entrypoint.sh:470-477` is
`[ -z "$keys" ] && exit 0`, so a selector that matches nothing renders an
empty stream into a registration carrying `prune: true`. It also does not
survive a platform-stack rollback below the sidecar version that understands
the selector: an old sidecar renders all N documents for each of the N
registrations, which SSA-thrash each other permanently and present as
flapping rather than as a version problem.

**Make the bundle an explicit new noun** (`app sources`, a `SOURCE` column
always populated). Rejected as a cost with no buyer: the owner's position is
that a three-service manifest looking like one application is correct, and a
noun that renders as `—` for the entire existing universe is a word every
single-app user must learn before they can ignore it.

**Refuse multi-app outright and require one directory per app.** Rejected on
the evidence: the CMP was built for the multi-document case and has a
fixture for it; the operator is already multi-CR-native; `needs.disk.ref` /
SharedVolume is a genuinely coupled case that wants one package; and the
divergence is not multi-app-exclusive — `app.rs:4756-4776` asserts the N=1
name-mismatch shape is supported, and the `restore.rs` defect above is
reachable at N=1. Refusing multi-app fixes that defect in zero cases; fixing
the join fixes it in all of them.

**Per-workload environment pinning** (a manifest-declared `spec.environment`
overriding the registration's `--env`). Rejected by the bundle decision
itself. Worth recording that the implementation we would have needed is
*also* wrong: making the sidecar stop stamping `spec.environment` when the
key is absent from `spec.environments` contradicts
`schemas/v1alpha1/application.cue:31-35`, which states verbatim that the
field *"may name an env absent from `environments` (base-only deploy) — NOT
rejected"*. The operator's fallback to `base` is the contract, not a defect.

## Risks

**`app list`'s `NAME` column changes for existing registrations whose repo
basename differs from the CR name.** Mitigation: the registration name stays
addressable — resolution falls back registration-name → grouping-label — so
every string a user has in their shell history still resolves, and the
changed rows are the ones where the old name was already failing for
`app open` and the workload selectors.

**Resolution has to carry the whole backward-compatibility story.** A missed
fallback arm reads as "`app status <name>` stopped working" across a fleet.
Mitigation: the resolver is pure and cluster-free, and is table-tested
exhaustively including the pre-2.9 unlabelled registration.

**A user reads the summary as a full status and misses a problem the full
block would have shown.** The per-workload block carries the pod `STATUS`
reason, the restart count and the stale-config marker; the summary must keep
enough of those to be actionable. Mitigation: the summary carries a not-Ready
roll-up line and prints it only when something is not Ready, so its presence
is the signal.

**Accepted:** `app list`'s `HEALTH` has never seen pod state and still will
not. The Lua check reads `status.phase`
(`platform-stack/cue/component_argocd.cue:168-205`) and the operator writes
`phase = Ready` when the Deployment is applied
(`operator/operator-controllers/application/src/lib.rs:1072`), so a
CrashLooping app has always rendered `Synced / Healthy` — documented at
`docs/changelog/history.md:7222-7232` and never fixed. This ADR makes the
cell cardinal and stops it *implying* it can see pods; it does not make it
see them.

## Owner

Andrey Ryahovskiy.

## Re-evaluation

Revisit if a second independent request arrives for per-workload
environments or per-workload removal, or if a bundle in the field grows past
roughly ten workloads — at which point the summary stops fitting a screen and
the "one row per registration" trade changes shape.

## References

- `plan.md` §2.27.
- [ADR 0063](0063-manifest-discovery.md) (the discovery convention and the
  enforcement table), [ADR 0064](0064-app-restart.md) (the restart verb),
  [ADR 0029](0029-cue-cmp.md) (the CMP),
  [ADR 0044](0044-per-environment-deploy.md), [ADR 0046](0046-env-value-references.md),
  [ADR 0048](0048-argo-platform-upgrade-approval-surface.md) (the
  anchor-403 deadlock), [ADR 0059](0059-application-image-pin.md) (the CR
  annotation + dedicated field-manager precedent).
- `cli/cli-core/src/manifest.rs`,
  `cli/platform-cli/src/commands/{app.rs,app_open.rs,app_validate.rs,app_rollup.rs,restore.rs}`,
  `argocd-cue-cmp/entrypoint.sh`,
  `operator/operator-rendering/src/lib.rs`,
  `platform-stack/cue/component_argocd.cue`.
