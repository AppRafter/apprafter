# Migration plans

A `MigrationPlan` is a declarative resource that gates destructive
changes — to user Applications or to the platform stack — behind
explicit approval. When a reconciler detects a destructive change
it creates a `MigrationPlan` and pauses the change; the previous
version keeps running until you act.

See [ADR 0027](../adr/0027-migrationplan-unification.md) for the
design rationale and [`spec.md` §3.8](https://github.com/apprafter/apprafter/blob/main/spec.md)
for the full field reference.

## What counts as destructive

The `MigrationController` classifies changes into four risk levels:

| Classification      | Examples                                                   |
| ------------------- | ---------------------------------------------------------- |
| `safe`              | Env-var additions, replica count changes, label updates.   |
| `requires-restart`  | Container image change, major Argo CD version bump.        |
| `data-migration`    | Storage-class change, `needs.pg` selector change.          |
| `breaking`          | Kubernetes minor upgrade, Cilium major version change.     |

Only `requires-restart`, `data-migration`, and `breaking` changes
create a `MigrationPlan`. Safe changes are applied immediately.

Platform-stack specific triggers (applied when diffing a
`PlatformStack` upgrade):

- Any diff classified as `requires-restart`, `data-migration`, or
  `breaking` in the chart's compatibility metadata.

Application-specific triggers:

- Image changes classified by the operator as non-trivial (major
  version bump or registry change).
- Storage selector changes (`needs.pg.selector`, `needs.redis.*`).
- Removal or significant narrowing of a `SourceCredential` that
  active applications depend on.

## Approval semantics by scope

`MigrationPlan` carries a `spec.scope.type` discriminator:
`application` or `platform`. The approval semantics differ.

### Application scope

**Approve only.** There is no reject action for application-scope
plans.

The application manifest lives in the user's Git repository. If
you want to reverse a change, revert the commit in your source repo.
Argo CD synchronizes the reverted manifest; the operator observes
it as a non-destructive (or differently-destructive) change and the
original `MigrationPlan` is superseded automatically.

The admission webhook enforces this model: attempting to patch
`status.phase=rejected` on an application-scope `MigrationPlan`
is denied at the API server layer (per ADR 0027). The CLI surfaces
the webhook denial message verbatim rather than silently failing.

While a `MigrationPlan` is pending, the application's
`status.phase` reads `AwaitingMigrationApproval` and a
`MigrationPending` condition is emitted with the plan name. Child
resources (Deployment, Service) continue running the previous spec.

### Platform scope

**Approve or reject.** The platform target lives in the cluster
(`PlatformStack` CR), not in a user-controlled Git repository.

- **Approve** — the `PlatformController` proceeds with the upgrade:
  it patches the umbrella Argo CD Application and Argo CD reconciles
  the new platform-stack version.
- **Reject** — the controller reverts `PlatformStack.spec.pin` to
  the value recorded in the plan's previous-spec snapshot. The
  cluster remains on the current version.

## Lifecycle

A `MigrationPlan` moves through these phases:

```
pending-approval → approved → executing → completed
                → rejected (platform scope only)
                → failed
```

Plans in `pending-approval` state remain there indefinitely — there
is no automatic expiration. If you want to dismiss a platform-scope
plan without approving it, use `apprafter migration reject`. For an
application-scope plan, revert the triggering commit in Git.

## CLI surface

```sh
# List all MigrationPlans in apprafter-system, with name, scope,
# classification, and current phase.
apprafter migration list

# Approve a plan. MigrationController transitions it through
# executing → completed.
apprafter migration approve <plan-name>

# Reject a plan (platform scope only). Reverts spec.pin to the
# previous value. The CLI surfaces the admission-webhook denial
# message verbatim if you attempt to reject an application-scope plan.
apprafter migration reject <plan-name>
```

You can also inspect and patch plans directly with `kubectl`:

```sh
kubectl get migrationplans -n apprafter-system

kubectl describe migrationplan <plan-name> -n apprafter-system

# Approve manually (equivalent to `apprafter migration approve`):
kubectl patch migrationplan <plan-name> -n apprafter-system \
    --type merge -p '{"status":{"phase":"approved"}}'
```

## Approval surfaces — today and later

The current approval surface is the CLI (`apprafter migration
list/approve/reject`) and direct `kubectl` access to the
`MigrationPlan` CR.

Later approval surfaces (not yet shipped):

- **Backstage** — a MigrationPlan queue view with approve/reject
  buttons, surfacing the risk breakdown, estimated downtime, and
  data-volume information from the plan.
- **Argo CD UI** — a Lua-script resource action ("Approve") in the
  Argo CD Application detail view for operators who prefer not to
  leave the Argo CD console.

## Where to look next

- [`platform-management.md`](./platform-management.md) — upgrade
  strategy and the conditions under which destructive diffs are
  created.
- [ADR 0027](../adr/0027-migrationplan-unification.md) — design
  rationale, including the asymmetric reject semantics and the
  gate-at-reconciler principle.
- [ADR 0025](../adr/0025-gitops-control-surface.md) — why the gate
  lives inside the operator/controller rather than at the Argo CD
  sync layer.
- `spec.md` §3.8 — full field reference for the `MigrationPlan` CRD.
