# ADR 0027: MigrationPlan unification with scope discriminator

## Status

Draft.

## Context

Spec §3.8 introduces a `MigrationPlan` resource for gating destructive changes to user Applications (selector changes for stateful claims, major version upgrades of platform services, storage class changes). The pattern is: when a reconciler detects a destructive change, it creates a `MigrationPlan` and pauses execution until an approver acts.

ADR 0026 introduces `PlatformStack` with a similar requirement: destructive platform changes (Cilium major version, Kubernetes minor version, storage backend swap) must be gated by explicit approval before being applied.

The spec.md §3.8 wording also revealed a timing issue: "Argo CD syncs a destructive change … reconciler creates a MigrationPlan instead of applying immediately." But Argo CD has already applied the `Application` CR to the cluster by the time the reconciler sees it — the destructive change is in the cluster's etcd, not pending. The actual gate must be inside our reconcilers, not at the Argo CD sync layer.

Two separate CRDs (`MigrationPlan` + `PlatformMigrationPlan`) would duplicate machinery. One unified CRD with a scope discriminator captures both use cases and leaves room for future scope types (Tenant, ServiceProvider, AccessGrant).

## Decision

A single CRD `apprafter.io/v1alpha1.MigrationPlan` with a discriminator field `spec.scope`:

```yaml
apiVersion: apprafter.io/v1alpha1
kind: MigrationPlan
metadata:
  name: parser-pg-migration-2026-05-13
  namespace: apprafter-system
spec:
  scope:
    type: application                   # application | platform
    # application-scope-specific fields:
    application:
      ref:
        name: parser
        namespace: default
      environment: prod
  trigger:
    kind: selector-change               # taxonomy-specific
    field: needs.pg.selector
    from: { tier: integrated }
    to: { tier: managed-aws }
  risks:
    classification: data-migration       # safe | requires-restart | data-migration | breaking
    estimatedDowntime: "5–15 minutes"
    dataVolume: "12 GB"
    reversible: false
    requiresFullBackup: true
  plan:
    - step: 1
      action: "Snapshot source DB to S3"
      estimatedDuration: "2m"
      reversible: true
    - step: 2
      action: "Provision target RDS instance"
      estimatedDuration: "5m"
      reversible: true
    # ...
  approvers:
    - alice@example.com
status:
  phase: pending-approval          # pending-approval | approved | rejected | executing | completed | failed
  executedSteps: []
  approvedBy: null
  approvedAt: null
```

A single controller (`MigrationController`, sibling to `PlatformController` within the `apprafter-operator` workspace) reconciles all MigrationPlans, dispatching to scope-specific logic through a Rust trait.

Approval and rejection semantics differ by scope:

- **`application` scope:** approve-only. There is no explicit reject. The application's manifest lives in the user's Git repository; if the user wants to reverse a change, they revert the commit in the source repo. Argo CD synchronizes the reverted manifest, the reconciler observes it as a non-destructive (or different-destructive) change, and the original `MigrationPlan` is superseded.
- **`platform` scope:** approve or reject. The platform target lives in the cluster (PlatformStack CR), not in a user repository. Reject means "revert `spec.pin` to the value stored in the migration's `status.previousSpec` annotation." A future `skip` action is anticipated but out of scope for this ADR (see Still open).

The gate is implemented inside the reconcilers, not at the Argo CD layer:

- For `application` scope: the AppRafter operator's `Application` reconciler refuses to patch child resources (Deployment, Service, HTTPRoute) while a `pending-approval` MigrationPlan exists for that Application. The Application CR exists in etcd with the new `spec`, but its child resources continue running the previous version. Status reflects `phase: AwaitingMigrationApproval`.
- For `platform` scope: the `PlatformController` refuses to patch component Argo CD Applications. The umbrella chart values do not change, so Argo CD continues reconciling the previous component versions.

## Rationale

### Single CRD reduces machinery

One CRD schema, one admission webhook, one set of RBAC, one Backstage UI view. Future scope types (Tenant, ServiceProvider, AccessGrant) are added as enum variants without new CRDs.

### Rust trait dispatch for type-specific logic

The controller's per-scope logic lives in trait implementations:

```rust
trait MigrationStrategy {
    fn detect_destructive(&self, ctx: &Context) -> Result<Option<DestructiveChange>>;
    fn create_plan(&self, change: DestructiveChange) -> Result<MigrationPlan>;
    fn execute_step(&self, step: &MigrationStep) -> Result<StepStatus>;
    fn reject(&self, ctx: &Context) -> Result<()>;  // platform-only; application impl is no-op
}

impl MigrationStrategy for ApplicationMigrationStrategy { /* ... */ }
impl MigrationStrategy for PlatformMigrationStrategy { /* ... */ }
```

Adding a new scope is `enum MigrationScope { … }` + new impl. Compiler-enforced exhaustive matching prevents missed cases.

### Asymmetric reject semantics

Application manifests live in user Git repositories under user control. Argo CD synchronizes whatever the user pushes. Adding an "AppRafter rejects your Application change" path creates a confusing concept: a CR was synced to the cluster but is not in effect, and the only way to "really revert" is through a Git operation the user must perform anyway. Better to keep it simple: approve, or revert in Git. The MigrationPlan is superseded when the reconciler observes the new (reverted) Application state.

Platform changes live in the `PlatformStack` CR inside the cluster, not in Git. The user changes `spec.pin` (or the controller auto-bumps based on `spec.channel`), the controller detects destructive, creates a MigrationPlan. If the user reviews and decides not to proceed, there is no Git revert to do — the controller must explicitly revert the `PlatformStack.spec.pin` to the previous value. That is what `reject` does.

### Gate at reconciler level, not Argo CD

Argo CD remains a simple transport. It synchronizes whatever is in the source. Our reconcilers gate the propagation from "CR in cluster" to "child resources reflect the CR's spec." This keeps the Argo CD layer decoupled and avoids fighting Argo CD's automated sync.

### No expiration, manual reject only

A `MigrationPlan` in `pending-approval` state remains there indefinitely. Auto-rejection would harm solo founders who travel and return to a forgotten upgrade prompt. In the default configuration, the next reconcile after auto-reject would simply create a new MigrationPlan with the same content, producing churn.

If the user wants to dismiss a plan without acting, manual reject (for platform scope) or Git revert (for application scope) is the path.

### Multiple approver model: emails for now

Approvers are listed as email addresses in `spec.approvers`. When the AccessGrant subsystem is delivered in Phase 4, the field will accept identity references (`subject: alice@example.com` or `subject: group:platform-admins`) and approval will route through the same identity layer. For Phase 1–3, plain emails are sufficient.

## Implementation outline

| Step | Description | Size |
|---|---|---|
| 1 | CRD schema in CUE with discriminator validation via OpenAPI v3 `oneOf` | S |
| 2 | Admission webhook for deeper validation (scope-specific required fields, approver email format) | S |
| 3 | `MigrationStrategy` trait + impls for `ApplicationMigrationStrategy` and `PlatformMigrationStrategy` | M |
| 4 | `MigrationController` reconciler in `apprafter-operator` workspace | M |
| 5 | Application reconciler integration: detect destructive, create plan, pause child reconcile, resume on approve | M |
| 6 | PlatformController integration (ADR 0026) — same detect/pause/resume hook | S |
| 7 | Backstage plugin: MigrationPlan queue view + approve/reject buttons | M (Phase 2–3) |
| 8 | Argo CD Resource Action via Lua script: "Approve" button in Argo CD UI for users who do not use Backstage | XS |
| 9 | CLI commands: `apprafter migration list`, `apprafter migration approve <name>`, `apprafter migration reject <name>` | S |

## Consequences

**Positive:**
- One CRD, one mental model, unified UI.
- Trait dispatch is type-safe at compile time.
- Reject asymmetry matches the natural ownership boundary (Git vs in-cluster).
- No surprise auto-reject for users who took a break.

**Negative:**
- Discriminator-based JSONSchema validation in Kubernetes <1.27 has limited support for `oneOf` validation; the admission webhook fills the gap.
- A single CRD for sufficiently divergent scope types may become awkward over time. Re-evaluation trigger covers this.

## Risk

**Main risk:** OpenAPI v3 `oneOf` discriminator validation gap on older Kubernetes versions. **Mitigation:** the AppRafter admission webhook performs the deeper validation (which scope-specific fields are required, which are forbidden). The CRD OpenAPI schema validates only the base structure.

**Secondary risk:** an in-cluster MigrationPlan with a wrong `approvers` list (user typo in email) results in no one being able to approve. **Mitigation:** CLI fallback `apprafter migration approve <name>` for cluster admins. Backstage UI can show an "override approve" button gated by RBAC.

## Owner

Core platform team.

## Re-evaluation triggers

- If three or more scope types become divergent enough that a shared CRD creates more pain than benefit, split into separate CRDs and retire the discriminator.
- If reject semantics for platform scope are exercised rarely and confuse users — re-evaluate whether to remove the reject action entirely and require manual `spec.pin` revert.
- If `approvers` need richer expressions before AccessGrant is delivered (groups, conditions) — extend the field schema, do not wait for AccessGrant.

## Still open

- **Future enhancement: skip.** A "skip this update, wait for the next one" action would let users acknowledge an available upgrade without approving or rejecting. The controller would record the skipped version in `PlatformStack.status.skippedVersions` and only propose the next version when one becomes available. Useful when users see an upgrade in flight but want to wait one cycle.
- **Future enhancement: partial migration.** When a platform upgrade touches multiple components, allow per-component approval. Currently the plan is atomic. Partial flow would split into sub-plans or per-component approval entries.
- **Approval expiration warnings.** Even without auto-reject, a MigrationPlan pending for >30 days might warrant a reminder notification (via the notifications service, Phase 2.15). Not a hard expiration, just a nudge.

## References

- Spec §3.8 (original MigrationPlan design).
- ADR 0025 (GitOps control surface) — gate-at-reconciler rationale.
- ADR 0026 (PlatformStack) — primary platform-scope consumer.
