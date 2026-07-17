# ADR 0051: application-scope destructive-change detection and gating

## Status

`Accepted` (2026-07-17).

ADR-first for subphase 2.16b (`plan.md` §2.16b — "App-scope migration:
auto-detect + Argo approval surface"). Ships as an operator release plus the
`spec.md` §3.8/§3.1 actualization. It builds directly on the unified
`MigrationPlan` (ADR 0027) and reuses the Argo CD approval surface established
for platform-scope plans (ADR 0048); it carries no new CRD and no new Argo Lua
customization.

## Context

ADR 0027 unified the `MigrationPlan` CRD across `application` and `platform`
scopes with a Rust-trait dispatch in `MigrationController`, and specified the
application-scope semantics: **approve-only**, with rejection expressed as a Git
revert (the application manifest lives in the user's repository, not in the
cluster). ADR 0048 then built the Argo CD-native approval surface — a clickable
"Approve" resource action plus a Degraded health signal — for platform-scope
plans.

Application-scope destructive detection was, however, present-but-disabled. The
`ApplicationMigrationStrategy::detect_destructive` classifier returned `None`
unconditionally, and `create_plan_for` was reachable only from tests. That was
correct at the time: when the classifier was written (the 1.76–1.78 unification),
the v1alpha1 `Application` schema carried no operations whose silent application
could destroy data or availability. The schema has since grown several:
`needs.*` platform-service dependencies (Phase 2.4) whose removal garbage-collects
a `ResourceClaim` and its data; public exposure via `expose.network`/`expose.hostname`
(1.83b); scale, and image references.

Meanwhile the MCP agentic-safety promise (ADR 0036, `spec.md` §3.8) — that a
destructive change is gated behind explicit human approval regardless of whether
a human or an automated agent authored it — applies to application edits just as
it does to platform upgrades. Enabling the classifier and wiring it into the
Application reconcile loop closes that gap.

Two mechanical questions had to be answered before the flip:

1. **What is the baseline to diff against?** The live `Application.spec` is the
   *new* state; there was no operator-owned record of the last successfully
   applied spec.
2. **Where does the plan live, and how is it garbage-collected?** ADR 0048's
   platform plan lives in `apprafter-system` with no natural owner, which is why
   it needed an anchor ConfigMap to appear in an Argo resource tree and to be
   GC-tracked. An application plan has a natural owner — the `Application` CR
   itself, which is already a managed node in the user's Argo application tree.

## Decision

Enable application-scope destructive detection and gating with the following
design.

**1. Baseline = `Application.status.lastAppliedSpec`.** The operator records the
last successfully applied `ApplicationSpec` (raw, verbatim) in `status`, where
Argo CD ignores it (no spurious `OutOfSync`). It is stamped only after a
successful apply of a non-blocked spec — never while a plan is live — so it is
always a GitOps-clean record of the state currently running. For a brand-new
Application (no baseline yet) the operator does not gate; it stamps the baseline
and proceeds.

**2. Diff = the effective spec, each side unified under its own environment.**
One logical app is a separate `Application` CR per environment (ADR 0044) sharing
an env-agnostic manifest. The classifier therefore compares the *effective*
spec — old baseline unified under the old environment, new spec unified under the
new environment — so that a dev-only edit gates the dev CR alone and never freezes
prod, and a `spec.environment` flip surfaces correctly. The classifier is
deterministic (fixed severity → op-class → field ordering) so that the same edit
always produces the same `DestructiveChange`.

**3. Plan location = the application's own namespace, owned by the Application
CR.** The auto-created `MigrationPlan` lands in the application's namespace with a
controlling `ownerReference` back to the `Application` CR (`controller: true`,
`blockOwnerDeletion: true`). Same-namespace ownership means Kubernetes GC never
spuriously deletes the plan and cascades it away cleanly when the Application is
deleted — no anchor ConfigMap is needed (the `Application` CR is already a managed
node in the user's Argo tree, so the existing `MigrationPlan` resource action
renders on the plan node as a grandchild). The controlling ownerRef also drives
the Application controller's `.owns(MigrationPlan)` watch for instant reaction.

**4. State machine = a total detect × plan-state consume-ticket machine.** Each
reconcile classifies the effective diff and buckets any existing plan by phase
into `none` / `blocking` / `completed` / `relic`. An approved plan reaches
`completed`; on the next reconcile the controller applies the new spec (including
any `needs` GC), stamps the new baseline, and **deletes the plan** — the plan is
a one-shot ticket that is consumed by the apply, so approval applies-and-clears
rather than looping. A superseding edit (the user pushes a *different* change
while a plan is pending) deletes the stale plan and creates a fresh one; a
non-destructive edit deletes any relic and proceeds. Ordering is apply → stamp →
delete, which is crash-safe in both windows. There is never more than one live
plan per `(app, namespace, environment)`.

**5. Reject = via Git (approve-only surface).** Consistent with ADR 0027, the
operator never patches the user's `Application.spec`. To reverse a pending
destructive change the user reverts the commit in their source repository; Argo
CD syncs the reverted manifest, the classifier observes a non-destructive (or
different) change, and the stale plan self-deletes. The approval surfaces are
`apprafter migration approve <name>` (CLI) and the Argo CD "Approve" resource
action; there is no application-scope reject command (the admission webhook
rejects a `status.phase=rejected` patch on an application-scope plan).

**6. Destructive taxonomy.** The classifier gates the following, over the
effective spec, choosing the highest-severity single change when several apply:

- **Data loss → `data-migration` (hard):** removal of any `needs.*` entry — the
  `ResourceClaim` and its data are garbage-collected.
- **Surface / availability → `requires-restart`:**
  - `expose.hostname` removal or change **of a publicly-routed app only** (gated
    only when the app's `expose.network == "public"`; on a non-public app no
    route is emitted, so the hostname is inert).
  - `expose.network` public → non-public (`public → internal` or `public → vpn`).
  - `replicas` N → 0 (scale-to-zero, deliberate downtime).
  - image **repository** (path) change — a different image, not the same image
    at a new tag.
  - removal of an env value that is a reference (a `claim.*` selector or a
    `secret: "name/key"` reference).

**Not gated (soft / deferred), emitting a `SoftDestructiveChange` Kubernetes
Event rather than a plan:** plain env *literal* removal; image *tag* change (the
2.4h tag → digest auto-rollout owns it); any add (`needs`, env, expose); scale
from zero (0 → N) and scale down to a non-zero replica count; a `needs.*.selector`
change (2.4d classified this non-destructive for the single integrated provider;
deferred, revisit post-2.5); a `needs.*.size` change (PVC and CNPG storage are
expansion-only, so a shrink is rejected at the provisioner layer — documented as
the active guard rather than gated).

## Consequences

Positive:

- A destructive application edit now pauses for explicit approval instead of
  silently destroying data or availability — the same human-in-the-loop guarantee
  the platform scope already had, extended to the application scope and to agent
  authors (ADR 0036).
- Reusing the `Application` CR's own ownerRef is simpler than ADR 0048's anchor
  ConfigMap: no extra resource, no extra RBAC verb (the operator ClusterRole
  already carries `migrationplans: {create, delete}`), and Kubernetes GC handles
  cascade-on-delete for free.
- The consume-ticket state machine makes approval a single, terminal action:
  approve → apply → plan deleted, with no re-creation loop and no dead-lock.

Negative / neutral:

- A destructive edit briefly pauses the affected environment's app at
  `AwaitingMigrationApproval` until approved; the previous spec keeps running in
  the meantime, so there is no outage, only a deferred rollout.
- The baseline is stamped on the first reconcile of a pre-existing app after the
  upgrade, which means one ungated window for that first edit (an accepted
  trade-off — there is no prior operator-owned record to diff against).
- Several edits are deliberately left ungated (env literal removal, image-tag
  change, adds, scale-from-zero, selector and size changes) to avoid approver
  fatigue; the soft ones surface as a Kubernetes Event so they are still visible.

## Alternatives considered

- **Anchor ConfigMap in `apprafter-system` (the ADR 0048 platform approach).**
  Rejected for application scope: the `Application` CR is already a managed node
  in the user's Argo tree and can carry a controlling ownerRef, so the anchor
  adds a resource and RBAC for no benefit. (The platform plan needed the anchor
  because the `PlatformStack` root had no suitable in-tree owner.)
- **Raw `spec` diff instead of the effective (per-env) diff.** Rejected: with the
  per-environment-CR model (ADR 0044) the raw specs are byte-identical across
  environments, so a dev-only edit would incorrectly freeze prod.
- **An operator-side reject that reverts `Application.spec`.** Rejected per ADR
  0027: the manifest is user-owned in Git; an in-cluster reject cannot change the
  source and would leave the CR and the repository disagreeing.

## Risks

- **Approver fatigue** if the trigger set is too broad. Mitigated by the
  soft/deferred carve-outs above — only genuinely data- or availability-affecting
  edits gate.
- **A misclassification could gate a benign edit or (worse) miss a destructive
  one.** Mitigated by an exhaustive unit table over every op class, edge case,
  and per-environment / env-flip scenario, plus a determinism test, and a live
  kind + Argo walk before release.

## Owner

Operator maintainers.

## Re-evaluation

- Revisit the `needs.*.selector` carve-out when a second (non-integrated)
  `needs.*` provider ships (post-2.5), since a selector change then becomes a
  genuine provider migration.
- Revisit the `needs.*.size` decision if the provisioner ever gains a shrink path.
- Fold in the deferred `SourceCredential`-scope wiring (its own `plan.md` item)
  when it lands — it mirrors this scope's ns / ownerRef / webhook-form / CLI
  contract for a new `sourcecredential` discriminator variant.

## References

- ADR 0027 (unified `MigrationPlan` with scope discriminator) — the approve-only /
  reject-via-Git application-scope semantics this ADR enables.
- ADR 0048 (Argo CD platform-upgrade approval surface) — the approval surface
  reused here (minus the anchor ConfigMap).
- ADR 0012 (MigrationPlan as a first-class concept), ADR 0036 (MCP agentic-safety),
  ADR 0044 (per-environment deploy), ADR 0046 (`Application.env` value references).
- `spec.md` §3.8 (MigrationPlan), §3.1 (Application).
- `docs/superpowers/specs/2026-07-17-2.16b-app-scope-migration-design.md`.
