---
description: "Why the platform reconciles itself through Argo CD rather than through the CLI, where a chart version comes from, and what decides whether an upgrade applies or waits."
---

# How the platform upgrades itself

Why an upgrade behaves the way it does. The recipe is
[Platform management](../operator-guide/platform-management.md); none of this
is needed to run one.

Read it when an upgrade did not happen and you want to know which of the
several possible reasons applied, or when you are deciding whether to turn
`autoUpgrade` on.

## The CLI is not in the upgrade path

`apprafter platform upgrade` writes a field. That is all it does: it patches
`PlatformStack.spec.pin`, and then it is finished. Everything after that is the
in-cluster `PlatformController` noticing the change and Argo CD reconciling it.

This is deliberate, and it is [ADR 0025](../adr/0025-gitops-control-surface.md):
a control surface that only writes desired state converges the same way whether
the write came from the CLI, from a `kubectl edit`, or from a GitOps repository
that owns the resource — and an operator's laptop being closed halfway through
cannot leave a cluster half-upgraded. It is also why a `PlatformStack` that is
git-managed will have an imperative patch overwritten on the next sync: Git is
the writer there, and the CLI said so when it patched.

The singleton `PlatformStack` that holds all of this is
[ADR 0026](../adr/0026-platformstack-crd.md).

## Where a version comes from

The chart is distributed as an OCI artefact, not from a chart repository index
— [ADR 0028](../adr/0028-platform-stack-distribution.md). The controller polls
the upstream on `spec.source.checkInterval` (six hours by default), resolves the
channel to a concrete version, and records it as `status.availableVersion`.

Three things can make `availableVersion` and `currentVersion` differ without an
upgrade happening, and telling them apart is most of diagnosing a stuck cluster:

- **`spec.pin` is set.** A pin is enforced, not advisory: the cluster converges
  to the pinned version and stays there, whatever the channel offers.
- **`autoUpgrade` is false.** The default. A safe diff is detected and reported
  and then waits for `apprafter platform upgrade`.
- **The diff is not safe.** A `requires-restart`, `data-migration` or
  `breaking` classification creates a `MigrationPlan` and holds — see
  [How the approval gate works](the-approval-gate.md).

A fourth case looks like the others and is not: `UpstreamReachable=False` means
the poll itself failed, so `availableVersion` is stale rather than the upgrade
being held. Enforcement is unaffected — a pinned stack still converges, an
unpinned one keeps its last-known target — but the number you are reading is
old.

## The version the cluster is on is not always the one it wants

`status.currentVersion` is what is deployed; `status.targetVersion` is what the
controller is converging to. They differ during a rollout, and they differ
indefinitely if the target cannot be reached — which is the state a yanked
version leaves behind, since the resolver skips a yanked release when selecting
channel-latest and a cluster pinned to one reports `YankedVersion=True` rather
than silently moving.

## See also

- [Platform management](../operator-guide/platform-management.md) — channels,
  pins, freezes, and the commands that write them.
- [How the approval gate works](the-approval-gate.md) — what happens when the
  diff is not safe.
