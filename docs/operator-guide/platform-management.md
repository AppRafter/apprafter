# Platform management

AppRafter manages the platform stack — Cilium, cert-manager, the
AppRafter operator, the admission webhook, and Argo CD itself —
through a declarative in-cluster resource: the `PlatformStack` CR.
You do not hand-install or hand-upgrade platform components; you
edit the `PlatformStack` (or use the `apprafter platform` CLI) and
Argo CD reconciles the rest.

## The PlatformStack resource

`cluster-bootstrap` creates exactly one `PlatformStack` named
`default` in `apprafter-system`. It is the single source of truth
for the platform version and configuration.

```yaml
apiVersion: apprafter.io/v1alpha1
kind: PlatformStack
metadata:
  name: default
  namespace: apprafter-system
spec:
  channel: stable       # stable | beta | edge
  pin: ""               # unset = follow channel; set = freeze to this version
  autoUpgrade: false    # when true, safe diffs apply automatically
  source:
    upstream: oci://ghcr.io/apprafter/platform-stack
    repoURL: oci://ghcr.io/apprafter/platform-stack
    checkInterval: 6h
  values:
    tier: solo
    domain: ""          # optional; if set, Argo CD UI gets a real cert
  overrides:
    # Per-component freezes or value tweaks.
    # Example: keep Cilium at a specific version.
    # cilium:
    #   pin: "1.16.5"
status:
  currentVersion: "0.2.0"
  availableVersion: "0.2.1"
  lastUpstreamCheck: "2026-05-13T14:30:00Z"
  components:
    - name: cilium
      desiredVersion: "1.16.5"
      observedVersion: "1.16.5"
      status: Healthy
  conditions:
    - type: Ready
      status: "True"
    - type: UpgradeAvailable
      status: "True"
      message: "0.2.0 → 0.2.1 (safe)"
```

See [`spec.md` §3.11](https://github.com/apprafter/apprafter/blob/master/spec.md)
and [ADR 0026](../adr/0026-platformstack-crd.md) for the full field
reference and design rationale.

## Release channels

Three channels control which platform-stack versions the
`PlatformController` considers:

| Channel  | Semantics                                                    |
| -------- | ------------------------------------------------------------ |
| `stable` | Tested component combinations; recommended for production.   |
| `beta`   | Newer combinations, mostly stable; recommended for staging.  |
| `edge`   | Latest commits; breaking changes allowed; for development.   |

When `spec.pin` is unset, the controller tracks the latest version
available in `spec.channel`. When `spec.pin` is set, the channel
still governs what `status.availableVersion` reports for visibility,
but the cluster stays on the pinned version.

## CLI: platform subcommands

The `apprafter platform` group wraps common `PlatformStack` operations:

```sh
# Show current version, available version, and component health.
apprafter platform status

# Upgrade to a specific version (sets spec.pin).
apprafter platform upgrade --to 0.2.1

# Clear spec.pin and return to channel-following mode.
apprafter platform upgrade

# Freeze a single component at its current effective version.
apprafter platform freeze cilium

# Freeze a component at an explicit version.
apprafter platform freeze cilium --version 1.16.5

# Remove a component freeze; it falls back to the chart's curated pin.
apprafter platform unfreeze cilium
```

You can also edit the `PlatformStack` CR directly with `kubectl edit`
or `kubectl patch` — the CLI is a thin ergonomic wrapper over the
same resource.

## Upgrade strategy

### Safe (non-destructive) diffs

When the diff between the current and target platform-stack version
is classified as `safe` — no restarts, no data migration, no
breaking changes — the `PlatformController` patches the umbrella
Argo CD Application directly and Argo CD reconciles the change.
With `spec.autoUpgrade: true`, safe diffs are applied automatically
as new versions become available.

### Destructive diffs

When the diff is classified as `requires-restart`, `data-migration`,
or `breaking`, the controller creates a `MigrationPlan` resource
and pauses the upgrade. The platform continues running the current
version until you approve or reject the plan.

```sh
# List pending plans.
apprafter migration list

# Review and approve a plan.
apprafter migration approve <plan-name>

# Reject a plan (reverts PlatformStack.spec.pin to the previous value).
apprafter migration reject <plan-name>
```

See [`migration-plans.md`](./migration-plans.md) for approval
semantics and the full CLI reference.

### Version history and rollback

`status.versionHistory` on the `PlatformStack` is a ring buffer of
recent version transitions. To roll back, pin to a previous version:

```sh
apprafter platform upgrade --to 0.1.5
```

If the rollback diff is destructive, a `MigrationPlan` gates it
the same way as a forward upgrade.

## Component freezes and overrides

You can hold one component at a specific version while letting the
rest of the stack upgrade normally. This is useful when a security
backport is needed without waiting for the next full stack release,
or when a chart update regresses a workload.

```sh
# Lock Cilium at 1.16.5.
apprafter platform freeze cilium --version 1.16.5
```

This writes `spec.overrides.cilium.pin: "1.16.5"` into the
`PlatformStack`. All other components continue to track the
curated bundle.

To remove the lock:

```sh
apprafter platform unfreeze cilium
```

## Emergency recovery

If Argo CD itself becomes unable to reconcile — a corrupted
ConfigMap, a stale chart, or a pod-eviction loop that the normal
upgrade path cannot resolve — use the rescue command:

```sh
apprafter platform rescue --yes
```

This re-runs the `cluster-bootstrap` loader path (Argo CD Helm
install) against the active target. After Argo CD is running again,
the root Application resumes normal platform-stack reconciliation.
The command requires explicit confirmation because it overwrites the
live Argo CD installation.

## Fork support

Forking the platform repository is a power-user path (plan item
1.80) not yet shipped.

## Where to look next

- [`migration-plans.md`](./migration-plans.md) — approve and reject
  destructive-change gates.
- [`quickstart.md`](./quickstart.md) — initial cluster provisioning.
- [ADR 0026](../adr/0026-platformstack-crd.md) — PlatformStack
  design rationale.
- [ADR 0025](../adr/0025-gitops-control-surface.md) — why the
  platform reconciles itself through Argo CD.
- [ADR 0028](../adr/0028-platform-stack-distribution.md) — OCI
  chart distribution model.
- `spec.md` §3.11 (PlatformStack), §3.8 (MigrationPlan).
