# ADR 0048: Argo CD platform-upgrade approval surface

## Status

`Accepted` (2026-06-12).

ADR-first for the Argo CD platform-upgrade approval surface (plan
`docs/superpowers/plans/2026-06-12-argo-upgrade-approval-surface.md`). This is
an interim, Argo-CD-native surface for an existing capability (CLI-only approval
of gated platform upgrades); the eventual richer home is the Phase-3 Backstage
approval UI. It carries no `spec.md` §6 milestone box and ships as an
operator + chart release, no CLI.

## Context

When the `PlatformController` gates a platform upgrade behind a `MigrationPlan`
— a non-`safe` `change` classification, e.g. `requires-restart` — the
`MigrationPlan` is created by the operator in-cluster, in `apprafter-system`,
with no `ownerReferences` and no Argo CD tracking label. Argo CD builds a
resource tree per Application by walking `ownerReferences` from the
chart-managed roots; an object with neither an ownerRef chain back to a managed
resource nor a tracking label appears in **no** Application resource tree.

Two consequences follow. First, the approve/reject Lua action buttons registered
for `apprafter.io_MigrationPlan` (B.1.79, `platform-stack/cue/component_argocd.cue`)
are unreachable in the UI: an Argo action button only renders on a node that is
in some Application's tree, and `orphanedResources` is not enabled on the
`platform` project. Second, nothing in the Argo UI signals that a gated upgrade
is even pending — the platform-stack root Argo Application (`argocd/platform`),
which is the resource that actually re-syncs once the plan is approved, shows no
trace of the held upgrade.

Today approval is therefore CLI-only: `apprafter migration approve <name>`. The
gate itself lives in the reconciler (ADR 0025), the `MigrationPlan` scope and its
admission-webhook reject-guard are fixed by ADR 0027. Users reasonably expect the
platform-stack root Application — the thing they already watch for platform state
— to **show** that a gated upgrade exists, show its details, and let them approve
it from Argo CD without dropping to the CLI.

## Decision

We will surface a gated platform upgrade in Argo CD by anchoring the
operator-created `MigrationPlan` into the platform-stack root Application's
resource tree and labelling it there, so the already-registered approve action
becomes clickable and the root Application reflects the pending upgrade.

> Live-walk (kind + Argo CD) confirmation: the grandchild approve button is
> discoverable and works on the anchored MigrationPlan node, and the
> `argoproj.io_Application` banner's pass-through branch is safe on healthy,
> degraded, and pending apps.

1. **Tree anchor.** The argocd component emits a trivial chart-managed
   `ConfigMap/platform-migration-anchor` in `apprafter-system` (via its
   `extraObjects`). The `PlatformController` sets each platform `MigrationPlan`'s
   `ownerReferences[0]` to this anchor (same namespace — see boundaries), so
   Argo's ownerRef walk pulls the plan into the platform-stack root Application's
   resource tree. The anchor carries no data; it is a tree anchor only. The plan
   is created 404/None-tolerant of the anchor — if the anchor is absent it is
   created un-owned and stays CLI-approvable.

2. **MigrationPlan node health.** A custom
   `resource.customizations.health.apprafter.io_MigrationPlan` health script
   labels the node — `Upgrade <from>→<to> (<class>) awaiting approval` — derived
   from `spec.trigger.{from,to}`, `spec.risks.classification`, and
   `status.phase`. This drives the in-tree row state and is what makes the
   approve action discoverable on the node (Decision 1). It does **not** bubble
   to the root Application: Argo CD aggregates an Application's health from its
   **managed** resource set (`status.resources`), not from arbitrary
   `ownerReference` tree children, and the anchored MigrationPlan is a live tree
   node but is not managed — so a `Suspended` MigrationPlan leaves the root
   Application `Healthy`. The root-level "an update is pending" signal therefore
   comes solely from the Decision-4 banner, not from aggregation. (The live walk
   confirmed this: a `Suspended` anchored plan did not change the root App's
   health.)

3. **Root-Application annotations.** While an upgrade is pending, the operator
   stamps machine-readable `apprafter.io/upgrade-{pending,from,to,class,plan}`
   annotations on the root Argo Application (`argocd/platform`), cleared
   (SSA-pruned by the `platform-controller` field manager) on completion. These
   are the durable machine-readable state of the held upgrade and the input the
   load-bearing root-App banner (Decision 4) reads.

4. **Root-App banner (load-bearing root-level signal).** A custom
   `resource.customizations.health.argoproj.io_Application` health script banners
   the root Application with the upgrade message when our annotation is present
   and otherwise forwards Argo CD's own computed Application health. Because the
   anchored MigrationPlan does **not** aggregate to the root App (Decision 2),
   this banner is the **only** root-level "an update is pending" signal — it is
   load-bearing, not additive. This **overrides Argo CD's built-in Application
   health cluster-wide** — for every Argo Application, including every
   `apprafter app add` user app — so it was gated on a live kind + Argo CD walk
   proving the pass-through branch faithfully reproduces Argo's health across
   healthy, degraded, and pending apps. The walk proved the pass-through safe, so
   the banner ships.

## Consequences

Positive:

- The approve/reject action buttons registered in B.1.79 stop being dead: the
  MigrationPlan node now exists in a tree, so the existing approve action is
  clickable. No new action surface is built.
- A pending platform upgrade is visible at the root Application via the
  Decision-4 banner (which reads the Decision-3 annotations), and the upgrade's
  `from→to (class)` is readable on the anchored MigrationPlan node.
- The held-upgrade state is machine-readable on the root Application
  (Decision 3), driving the banner and available to any future consumer
  (e.g. the Phase-3 Backstage approval UI) without re-deriving it.
- No second approval path is introduced: approval stays on the MigrationPlan via
  the existing action + the existing CLI, so the ADR-0027 state machine and its
  webhook reject-guard remain the single arbiter.

Negative / neutral:

- A new chart-managed ConfigMap and two Lua health customizations (the
  MigrationPlan node label and the root-App banner) to own in
  `component_argocd.cue`.
- The banner replaces Argo CD's built-in Application health cluster-wide — a
  broad blast radius that was gated behind a live walk (see Risks) and proved
  safe there.
- The human-readable upgrade `notes` (from `compatibility.cue`) are **not**
  surfaced on the node yet; that is a deferred follow-up (see References /
  Re-evaluation) because it needs a new `#MigrationPlanSpec.notes` CRD field,
  which touches the crdgen drift gate (ADR 0047). The node label degrades
  gracefully without it — `from→to (class)` is enough.
- Release is operator + chart, **no CLI**: the operator image is
  chart-delivered, and `RELEASED_OPERATOR_VERSION` is auto-derived with no
  production caller, so no `cli/Cargo.toml` bump and no monorepo `v0.x.y` tag.

## Alternatives considered

- **`orphanedResources` surfacing.** Enable Argo CD's orphaned-resource view on
  the `platform` project so the un-owned MigrationPlan appears. Rejected: the
  `platform` project is wide-open (`namespace: "*"`), so orphan surfacing would
  clutter it with every unmanaged object; it relies on an orphan-scan
  precondition we cannot reliably verify; and the surfaced node has no logical
  parent, so the approval node still floats without context. The chart-emitted
  anchor gives the node a real, namespaced parent under the root Application.
- **An action bridge on the root Application.** Register an approve action on the
  `argoproj.io_Application` node that flips the MigrationPlan. Rejected: an Argo
  action Lua mutates only the object it runs on, so an action on the Application
  cannot approve a MigrationPlan in another namespace; any controller-bridged
  annotation to relay the intent would **split the approval state machine across
  two objects and bypass the ADR-0027 admission-webhook reject-guard**. Approval
  must live on the MigrationPlan node, never the root Application node.

## Risks

- **R1 — the banner's cluster-wide health override mis-reports other apps.** The
  `argoproj.io_Application` health customization runs for every Argo Application;
  if the pass-through branch fails to reproduce Argo's own computed health, it
  silently mis-reports every user app's health. *Mitigation:* the banner was
  walk-gated — it shipped only after a live kind + Argo CD walk confirmed the
  pass-through is faithful across healthy, degraded, and pending apps. The walk
  confirmed it; the banner is the load-bearing root-level signal (the anchored
  MigrationPlan does not aggregate to the root App), so there is no aggregation
  fallback to drop back to.
- **R2 — grandchild button rendering.** The anchor → MigrationPlan path is tree
  depth 2; kind-scoped Argo actions should fire regardless of depth, but only a
  live Argo CD confirms the approve button is discoverable on a grandchild node.
  *Mitigation:* the live walk validated the approve button is available and works
  on the grandchild MigrationPlan node; if it had not been, the node is still
  visible with details + the CLI approve, and surfacing `PlatformStack/default`
  as the managed parent is a documented fallback.
- **R3 — cross-namespace ownerReference deletes the plan.** A cross-namespace
  ownerRef makes the namespace-scoped k8s garbage collector silently delete the
  MigrationPlan when its owner-scan finds no owner in the plan's namespace.
  *Mitigation:* the anchor ConfigMap and the MigrationPlan are **both** pinned to
  `apprafter-system`; the ownerRef is same-namespace by construction, with
  `controller: false` / `blockOwnerDeletion: false`.

## Owner

Andrey Ryahovskiy.

## Re-evaluation

Revisit when the Phase-3 Backstage approval UI lands (it is the eventual richer
home for upgrade approval and may subsume this Argo-native surface), or when the
deferred `#MigrationPlanSpec.notes` enrichment is taken up (it surfaces the
`compatibility.cue` upgrade description on the node and touches the crdgen drift
gate).

## References

- ADR 0025 — GitOps control surface via in-cluster Argo CD Applications (the
  upgrade gate lives in the reconciler; the root Application re-syncs on
  approval).
- ADR 0027 — MigrationPlan unification with scope discriminator (the
  MigrationPlan reject scope and its admission-webhook reject-guard, which the
  action bridge alternative would bypass).
- ADR 0047 — CRD codegen from CUE (the crdgen drift gate the deferred `notes`
  enrichment must clear).
- B.1.79 — the registered `apprafter.io_MigrationPlan` approve/reject actions in
  `platform-stack/cue/component_argocd.cue`.
- `docs/superpowers/plans/2026-06-12-argo-upgrade-approval-surface.md` — the
  implementation plan.
