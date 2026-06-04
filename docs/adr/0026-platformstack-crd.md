# ADR 0026: PlatformStack CRD as platform version control plane

## Status

Draft.

## Amendment (2026-06-04): Tier-1 bootstrapped default is `autoUpgrade: true`

The original decision below sets `autoUpgrade: false` as the default and treats `true` as opt-in. That remains the **bare-schema default** for raw or non-bootstrap creation (and for any tier other than Tier 1). However, the value `cluster-bootstrap` **emits into the bootstrapped Tier-1 `PlatformStack` is now `autoUpgrade: true` (opt-out)**.

Rationale: the MigrationPlan gate (see "Safe auto-upgrade" below and ADR 0027) ensures that only diffs classified as `safe` auto-advance; any `requires-restart` / `data-migration` / `breaking` diff is gated behind a MigrationPlan rather than applied automatically. On a Tier-1 single-VPS deployment this makes unattended auto-advance safe, and it gives the push-and-it-converges UX that matches the platform's converge-by-default posture. An operator opts out explicitly with `spec.autoUpgrade: false`.

Scope: this affects only what the bootstrapper writes for Tier 1. The CUE/CRD schema default stays `false` (safe for raw creation and for currently-unwired higher tiers).

## Context

ADR 0025 establishes that the platform stack is managed via Argo CD `Application` CRs in the cluster. This leaves an open question: how does the user manage the **version** of the platform — bumping Cilium, applying security patches, opting in to a new tier feature?

The naive answer ("the CLI binary knows the platform version") is wrong on two counts:

1. **CLI and platform must be independent.** Users do not upgrade the CLI on every platform release. By analogy: an Ansible playbook ships independently of the `ansible` binary; Helm charts ship independently of the `helm` binary.
2. **CLI is a thin client over a declarative resource.** Anything stored only in the CLI is invisible to UI, audit log, and other tooling. The control plane must live in the cluster.

A declarative resource for platform version is needed, with a controller reconciling actual cluster state toward it, performing diff classification (destructive vs non-destructive — see ADR 0027), and surfacing upstream availability for visibility.

## Decision

Introduce a new CRD `apprafter.io/v1alpha1.PlatformStack`. Exactly one instance exists per cluster, named `default`, in the `apprafter-system` namespace.

```yaml
apiVersion: apprafter.io/v1alpha1
kind: PlatformStack
metadata:
  name: default
  namespace: apprafter-system
spec:
  # Channel selection. If `pin` is unset, the controller resolves to the
  # latest version of this channel.
  channel: stable                  # stable | beta | edge

  # Optional explicit version freeze. When set, channel is ignored for
  # resolution; channel still affects what the controller reports as
  # `availableVersion`.
  pin: "0.2.0"

  # Default false. When true, the controller automatically bumps to the
  # latest channel version, unless the diff is classified as destructive
  # (in which case a MigrationPlan is created and the bump is gated).
  autoUpgrade: false

  # Where the chart is pulled from. Defaults to the AppRafter upstream.
  # Forks override `repoURL`; tracking forks may keep `upstream` for
  # availability visibility (see ADR 0028).
  source:
    upstream: oci://ghcr.io/apprafter/platform-stack
    repoURL: oci://ghcr.io/apprafter/platform-stack
    checkInterval: 6h

  # Global values passed to the umbrella chart.
  values:
    tier: solo
    domain: example.com

  # Per-component overrides: freezes, value tweaks, enable/disable.
  overrides:
    cilium:
      pin: "1.16.5"              # do not move this component even on stack bump
    backstage:
      enabled: false             # opt out entirely

status:
  currentVersion: "0.2.0"
  availableVersion: "0.2.1"
  lastUpstreamCheck: "2026-05-13T14:30:00Z"
  components:
    - name: cilium
      desiredVersion: "1.16.5"
      observedVersion: "1.16.5"
      status: Healthy
    - name: cert-manager
      desiredVersion: "1.16.2"
      observedVersion: "1.16.2"
      status: Healthy
  versionHistory:               # last N transitions, ring buffer
    - version: "0.2.0"
      appliedAt: "2026-05-10T09:00:00Z"
      transition: install
    - version: "0.1.5"
      appliedAt: "2026-04-12T11:30:00Z"
      transition: upgrade
  conditions:
    - type: Ready
      status: "True"
    - type: UpgradeAvailable
      status: "True"
      message: "0.2.0 → 0.2.1 (safe)"
```

A new component, `PlatformController` (delivered as part of the `apprafter-operator` binary or a sibling controller in the same workspace), reconciles `PlatformStack` CRs.

## Rationale

### Declarative platform version

`kubectl get platformstack default -o yaml` shows the full current state of the platform. `kubectl edit` is the change interface. Audit logging is native through the Kubernetes API audit subsystem. Backstage and other UIs read from the same source.

### CLI and platform truly decoupled

The CLI knows only the CRD API version (a stable contract). The CLI does not embed a default platform version; on first bootstrap, it creates a `PlatformStack` resource without `spec.pin`, with `spec.channel: stable`, and the controller resolves the current latest version at apply time. A CLI binary from six months ago still works correctly with a freshly published platform-stack version.

### No explicit `spec.version` field

The version targeted by the user is implicit:
- If `spec.pin` is set, that exact version is targeted.
- Otherwise, the latest version available in `spec.channel` from `spec.source.upstream` is targeted.

This eliminates a redundant field. The OCI registry tag is the canonical source of version truth; `status.currentVersion` reports what is actually applied. `status.versionHistory` keeps a ring buffer of recent transitions for rollback and audit.

### npm-style CLI version awareness

The CLI separately tracks its own version against upstream releases on every invocation (with a 24h TTL cache), and warns when an update is available, in the manner of `npm`:

```
apprafter 0.5.2 is available (you have 0.3.0). Run `apprafter self-upgrade`.
```

This is unrelated to platform version and follows the user's own update cadence.

### Channels

Three channels, mirroring the Talos and Kubernetes release-channel patterns:

| Channel | Semantics |
|---|---|
| `stable` | Tested combinations of component versions; recommended for production. |
| `beta` | Newer combinations, mostly stable but may have bugs; recommended for staging. |
| `edge` | Latest commits; breaking changes allowed; for development/contribution. |

Channel determines resolution when `spec.pin` is unset, and determines what `status.availableVersion` reports for visibility.

### Safe auto-upgrade

When `spec.autoUpgrade: true`, the controller bumps `status.currentVersion` toward `status.availableVersion` only if the diff is classified as `safe` (see compatibility metadata in ADR 0028). Any other classification (`requires-restart`, `data-migration`, `breaking`) triggers a `MigrationPlan` (ADR 0027) instead of an automatic bump. This makes `autoUpgrade: true` safe to enable on Tier 1 single-VPS deployments without risking unattended destructive changes.

### Curated upstream combinations

The platform-stack chart at version `X.Y.Z` ships a tested combination of component versions. Cross-component compatibility (e.g., Cilium 1.17 requires Kubernetes 1.30+) is validated upstream during release. Users do not need to think about which Cilium pairs with which cert-manager; the curated bundle is the contract.

The controller does perform an environment-diff check at apply time: before patching component Applications, it confirms the cluster's current Kubernetes version satisfies the chart's `minimumKubernetesVersion` (declared in chart metadata). If not, the upgrade is blocked with a clear diagnostic, and the user is directed to upgrade k8s first (a Tier 2+ concern requiring node-level work).

## Implementation outline

| Step | Description | Size |
|---|---|---|
| 1 | `PlatformStack` CRD schema in CUE + generated OpenAPI v3 | S |
| 2 | Admission webhook validation (single instance per cluster, valid channel enum, sane interval values) | S |
| 3 | `PlatformController` Rust crate: kube-rs reconcile loop with leader election | M |
| 4 | OCI registry client (pull chart by tag, list available tags by channel) | S |
| 5 | Helm render + diff logic: render umbrella chart, compute diff vs current cluster state | M |
| 6 | Periodic upstream check task with configurable interval | S |
| 7 | Status updates: `currentVersion`, `availableVersion`, `versionHistory`, `components`, `conditions` | S |
| 8 | CLI thin wrappers: `apprafter platform {status,upgrade,channel,freeze,unfreeze}` | S |
| 9 | Bootstrap integration: `cluster-bootstrap` creates default `PlatformStack` CR | XS |

Total: roughly L overall, with one M-sized core reconcile loop and several S-sized supporting pieces. Distributed-systems penalty applies for the reconciler portion (new distributed component).

## Consequences

**Positive:**
- Full declarative control over platform version and configuration.
- CLI binary is decoupled from platform versioning.
- Day-2 platform operations (upgrade, freeze a component, change channel) flow through standard k8s mechanics.
- Audit and history are native.
- Auto-upgrade is safe by default through MigrationPlan integration.

**Negative:**
- A new distributed component (`PlatformController`) requires testing, monitoring, and leader election.
- Periodic upstream checks consume OCI registry quota; rate limiting must be handled gracefully.
- Compatibility metadata classification (ADR 0028) requires discipline at release time.

## Risk

**Main risk:** controller failures impair visibility. Mitigation: `PlatformController` is not on the data path — Argo CD continues to reconcile platform components from their existing Application CRs even if the controller is down. The controller only updates `status` and patches Applications on `spec` changes; both are recoverable on restart.

**Secondary risk:** OCI registry rate limits on large fleets (managed offering). Mitigation: aggressive caching of resolved tag lists; webhook-driven update notifications considered for managed contexts.

## Owner

Core platform team. PlatformController may become a sub-team responsibility in Phase 3+ when managed-offering scaling concerns mature.

## Re-evaluation triggers

- If three channels (`stable`/`beta`/`edge`) prove insufficient or excessive — re-evaluate. Possible additions: `lts` for long-term support tracks.
- If periodic upstream checks cause rate-limit issues at managed-fleet scale — switch to webhook-driven update notifications via a dedicated event channel.
- If discriminator-based diff classification proves too coarse — extend the taxonomy.

## Still open

- **Cross-tier `PlatformStack` semantics.** When a user upgrades from Tier 1 to Tier 2, does the existing `PlatformStack` mutate (`spec.values.tier: solo → team`) or is a new instance created? Lean toward in-place mutation with MigrationPlan gating; will revisit in tier-upgrade design work for Phase 3.
- **Multi-cluster `PlatformStack` aggregation.** The managed offering (ADR 0034; the cross-cluster aggregator) requires viewing platform versions across many customer clusters. Out of scope for this ADR; addressed in the managed-offering control-plane design (ADR 0037).

## References

- ADR 0025 (GitOps control surface).
- ADR 0028 (Distribution and packaging).
- [Talos release channels](https://www.talos.dev/v1.7/learn-more/release-notes/) — reference pattern.
- [Argo CD `Application` Helm source](https://argo-cd.readthedocs.io/en/stable/user-guide/helm/) — underlying mechanism.
