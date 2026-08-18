# Migration plans

A `MigrationPlan` is a declarative resource that gates destructive
changes — to user Applications or to the platform stack — behind
explicit approval. When a reconciler detects a destructive change
it creates a `MigrationPlan` and pauses the change; the previous
version keeps running until you act.

See [ADR 0027](../adr/0027-migrationplan-unification.md) for the
unified-CRD design rationale, [ADR 0051](../adr/0051-app-scope-migration.md)
for application-scope detection and gating, and
[`spec.md` §3.8](https://github.com/apprafter/apprafter/blob/master/spec.md)
for the full field reference.

## What counts as destructive

The `MigrationController` classifies changes into four risk levels:

| Classification      | Examples                                                   |
| ------------------- | ---------------------------------------------------------- |
| `safe`              | Env-var additions, replica count changes, label updates.   |
| `requires-restart`  | Public hostname change, scale-to-zero, major Argo CD bump. |
| `data-migration`    | Storage-class change, `needs.pg` removal.                  |
| `breaking`          | Kubernetes minor upgrade, Cilium major version change.     |

Only `requires-restart`, `data-migration`, and `breaking` changes
create a `MigrationPlan`. Safe changes are applied immediately.

Platform-stack specific triggers (applied when diffing a
`PlatformStack` upgrade):

- Any diff classified as `requires-restart`, `data-migration`, or
  `breaking` in the chart's compatibility metadata.

### Application triggers (ADR 0051)

For an application edit, the diff is taken between the last applied
spec and the new spec, each evaluated under its own environment, so a
change in one environment gates only that environment's deployment.
The following gate:

- **Removing a `needs.*` dependency** — the backing platform service
  (its `ResourceClaim` and data) is garbage-collected (`data-migration`).
- **Changing or removing `expose.hostname` on a publicly-routed app**
  (one with `expose.network: "public"`) — the app becomes unreachable
  on the old hostname (`requires-restart`). On a non-public app a
  hostname edit is inert and does not gate.
- **Changing `expose.network` from `public` to a non-public value**
  (`internal` or `vpn`) — removes external reachability
  (`requires-restart`).
- **Scaling to zero** (`replicas` N → 0) — a deliberate outage
  (`requires-restart`).
- **Changing the image repository** (the path, not the tag) — a
  different image (`requires-restart`).
- **Removing an env value that is a reference** (a `claim.*` selector
  or a `secret: "name/key"` reference) — the workload loses a wired
  dependency (`requires-restart`).

These edits do **not** gate — they auto-apply, and the soft-destructive
ones emit a `SoftDestructiveChange` Kubernetes Event you can see with
`kubectl get events`:

- Any addition (a new `needs`, env var, or expose rule).
- Scaling from zero (0 → N) or down to a non-zero count.
- Removing an env *literal* (a plain string, not a reference).
- Changing only the image **tag** on the same repository — the operator
  resolves the tag to a digest and rolls it out automatically.
- Changing `needs.*.size` — storage is expansion-only, so a shrink is
  rejected at the provisioner layer.

Removal or narrowing of a `SourceCredential` that active applications
depend on is classified as destructive today; live gating for the
`sourcecredential` scope is not implemented yet.

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
is denied at the API server layer (per ADR 0027). There is no
`apprafter migration reject` for application scope.

The plan is created in the **application's own namespace** with a
controlling `ownerReference` back to the `Application` CR (ADR 0051).
Kubernetes garbage-collects it if the application is deleted, and it
renders inside the user's Argo CD application tree without any extra
anchor resource, so the "Approve" resource action appears on the plan
node.

While a `MigrationPlan` is pending, the application's
`status.phase` reads `AwaitingMigrationApproval` and a
`MigrationPending` condition is emitted with the plan name. Child
resources (Deployment, Service) continue running the previous spec.
On approval the operator applies the new spec, re-stamps its baseline,
and deletes the plan — the plan is a one-shot ticket, so approving it
applies-and-clears rather than re-creating a new gate.

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

```text
pending-approval → approved → executing → completed
                → rejected (platform scope only)
                → failed
```

Plans in `pending-approval` state remain there indefinitely — there
is no automatic expiration. If you want to dismiss a platform-scope
plan without approving it, use `apprafter migration reject`. For an
application-scope plan, revert the triggering commit in Git.

For an application-scope plan the operator **deletes** the plan once it
applies the approved spec (the plan is a consumed ticket, ADR 0051), so
an approved application plan does not linger in `completed`.

## CLI surface

```sh
# List MigrationPlans across ALL namespaces, with namespace, name,
# scope, classification, and current phase. Platform-scope plans
# live in apprafter-system; application-scope plans live in the
# application's own namespace.
apprafter migration list

# Approve a plan. The namespace is resolved automatically from the
# listing (pass -n <namespace> to disambiguate). MigrationController
# transitions it through executing → completed.
apprafter migration approve <plan-name>

# Reject a plan (platform scope only). Reverts spec.pin to the
# previous value. Application-scope plans have no reject command —
# revert the change in Git instead.
apprafter migration reject <plan-name>
```

You can also inspect and patch plans directly with `kubectl`. Use the
plan's own namespace (`apprafter-system` for platform scope, the
application's namespace for application scope):

```sh
kubectl get migrationplans -A

kubectl describe migrationplan <plan-name> -n <namespace>

# Approve manually (equivalent to `apprafter migration approve`):
kubectl patch migrationplan <plan-name> -n <namespace> \
    --type merge -p '{"status":{"phase":"approved"}}'
```

## Approval surfaces — today and later

Two approval surfaces ship today:

- **CLI** — `apprafter migration list/approve/reject` and direct
  `kubectl` access to the `MigrationPlan` CR.
- **Argo CD UI** — a Lua-script resource action ("Approve") on the
  `MigrationPlan` node, plus a Degraded health signal on the affected
  resource. A pending **platform** plan surfaces under the
  platform-stack tree (ADR 0048); a pending **application** plan
  surfaces on the app node in the user's own Argo application tree,
  and the app's health goes Degraded with an "awaiting MigrationPlan
  approval" message (ADR 0051). Click "Approve" on the plan node to
  approve without leaving the Argo CD console.

Later approval surface (not yet shipped):

- **Backstage** — a MigrationPlan queue view across both scopes with
  approve buttons, surfacing the risk breakdown, estimated downtime,
  and data-volume information from the plan. Follows in the post-launch
  portal bundle.

## Where to look next

- [`platform-management.md`](./platform-management.md) — upgrade
  strategy and the conditions under which destructive diffs are
  created.
- [ADR 0027](../adr/0027-migrationplan-unification.md) — design
  rationale, including the asymmetric reject semantics and the
  gate-at-reconciler principle.
- [ADR 0051](../adr/0051-app-scope-migration.md) — application-scope
  destructive detection: the baseline, the per-environment diff, the
  taxonomy, and the app-namespace / ownerRef plan placement.
- [ADR 0025](../adr/0025-gitops-control-surface.md) — why the gate
  lives inside the operator/controller rather than at the Argo CD
  sync layer.
- `spec.md` §3.8 — full field reference for the `MigrationPlan` CRD.
