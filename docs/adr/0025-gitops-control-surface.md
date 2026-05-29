# ADR 0025: GitOps control surface via in-cluster Argo CD Applications

## Status

Draft.

## Context

Spec §1.4 states "GitOps as the only control surface; `kubectl apply` to production is an anti-pattern." Spec §1.5 states "No imperative scripts in the happy path."

The current Phase 1 implementation of `platform-cli cluster-bootstrap` (delivered in sub-phases 1.5–1.18 between v0.1.13 and v0.1.65) contradicts these principles. The command performs nine imperative steps in sequence:

1. `helm install cilium`
2. `kubectl apply` Gateway API CRDs
3. `kubectl apply` the AppRafter `Application` CRD
4. `kubectl apply` default-deny `NetworkPolicy`
5. `helm install argocd`
6. `helm install cert-manager`
7. `kubectl apply` self-signed `ClusterIssuer`
8. `helm install apprafter-operator`
9. `kubectl apply` admission-webhook stack

None of these components are reconciled by Argo CD after installation. Argo CD itself sits in the cluster as an optional appendix that may track a single user-supplied Git repository. There is no drift correction for platform components, no declarative path to change Cilium or cert-manager configuration, and no audit trail for platform-level changes. Modifying platform settings currently requires rebuilding the `platform-cli` binary.

## Decision

After `cluster-bootstrap` completes, all resources in the cluster — including the platform stack itself (Cilium, cert-manager, AppRafter operator, admission webhook, default NetworkPolicies, Backstage, and Argo CD itself) — are managed via Argo CD `Application` custom resources.

`platform-cli cluster-bootstrap` is reduced to a minimal bootstrap loader that performs exactly one Helm release: Argo CD itself. After Argo CD is running, the CLI applies a root `Application` resource that points to the platform-stack OCI chart (see ADR 0028). All subsequent platform reconciliation flows through Argo CD.

The source of truth for the platform stack is the cluster itself, not the CLI binary and not a file on the user's disk.

## Rationale

### Alignment with stated principles

Spec §1.4 and §1.5 become enforceable instead of aspirational. Every platform change becomes a Git commit (to the platform-stack repository upstream, or to a fork — see ADR 0028) or a `kubectl edit` on the PlatformStack CR (see ADR 0026), with k8s API audit logging recording every transition.

### Drift correction

Argo CD's `selfHeal` reconciliation reverts unauthorized changes to platform components. An operator who runs `kubectl edit ds cilium` to "quickly fix" something has their change reverted on the next reconcile cycle, with the event surfaced in the Argo CD UI and exportable to the audit log.

### Unified mental model

Platform components and user applications appear side by side in the Argo CD UI. An operator investigating "is Cilium healthy?" uses the same interface and the same status fields as "is the parser service healthy?". There is no separate diagnostic flow for "platform vs workload."

### Decoupled platform versioning

Changing Cilium values no longer requires rebuilding the CLI binary. Edit the relevant Argo CD `Application` CR (or, more typically, edit the PlatformStack CR which the PlatformController translates into an Application patch — see ADR 0026), and Argo CD reconciles.

### No additional vendor lock-in

The pattern does not require any user-side Git repository for platform manifests. Platform components are pulled from a public OCI registry; users who want full control can fork the OCI chart (see ADR 0028). No runtime calls to AppRafter-controlled infrastructure are required.

## Implementation outline

| Step | Description | Size |
|---|---|---|
| 1 | Reduce `cluster-bootstrap` to: install Argo CD via Helm (bootstrap loader), apply root `Application` CR | M |
| 2 | Root `Application` points to OCI chart from ADR 0028 | S |
| 3 | Wrap existing platform components (Cilium, cert-manager, AppRafter operator, admission webhook, Backstage, NetworkPolicies) as templated Argo CD `Application` resources inside the chart | M |
| 4 | Self-managing Argo CD: ship Argo CD itself as one of the Applications in the chart so that future Argo CD upgrades flow through normal reconcile | S |
| 5 | Migrate existing Helm values builders (`cli-providers::k8s::*_values_yaml`) from CLI source into chart values templates | M |
| 6 | `apprafter bootstrap-all` progress UX: indicatif `MultiProgress` showing both loader install and post-loader reconcile phases | S |

### Bootstrap loader scope

The bootstrap loader installs **only Argo CD itself**. cert-manager is not part of the loader; it arrives later through Argo CD reconciliation. Argo CD's web UI can use a self-signed certificate on first boot (acceptable for port-forward access; users without a configured domain do not need cert-manager at this stage). Once cert-manager is reconciled by Argo CD and a `ClusterIssuer` is in place, the Argo CD UI certificate can be reissued through standard `cert-manager.io/Certificate` resources, also managed by Argo CD.

This keeps the loader truly minimal: one Helm release, one `kubectl apply` for the root Application, and the rest is handled by Argo CD.

### Argo CD self-management

Argo CD reconciles its own Argo CD `Application` resource, allowing version bumps and configuration changes to flow through the same path as other components. To avoid a self-destructive update breaking the reconciliation loop, the self-managing Application uses `syncPolicy.automated.prune: false` and version changes go through MigrationPlan classification as `requires-restart` or higher (see ADR 0027), forcing explicit approval for non-trivial changes.

This is the standard pattern used by Argo CD Autopilot and similar tools; not a new invention.

## Consequences

**Positive:**
- Spec §1.4 and §1.5 become operationally true.
- Drift correction works cluster-wide.
- Platform changes go through declarative resources with native audit logging.
- CLI binary becomes smaller and less stateful.
- Multi-node, multi-cluster, and managed-offering use cases inherit the same model.

**Negative:**
- Bootstrap is observably two-phase: loader installs Argo CD (~30s), then Argo CD reconciles the rest (~2–5 min on Tier 1). Progress UX is mandatory.
- Phase 1 sub-phases 1.5–1.18 are partially rewritten. Helm values builders are reused as chart values; orchestration code is removed.
- Chicken-and-egg for Argo CD self-managing requires care during upgrades. Mitigation: explicit approval (MigrationPlan) for Argo CD version changes.

## Risk

**Main risk:** poor bootstrap UX during the reconciliation phase. Users see "nothing is happening" between loader completion and platform-stack ready.

**Mitigation:** `apprafter bootstrap-all` uses `indicatif::MultiProgress` to show both the loader phase and the post-loader reconcile phase. The reconcile-phase progress queries Argo CD via API for the root Application's child statuses and updates the bar as each platform component reports `Healthy`. Idempotent resume on any step (a pre-launch P1 requirement).

**Secondary risk:** an Argo CD self-update that produces a broken Argo CD pod that cannot recover itself. Mitigation: Argo CD version changes are classified as at least `requires-restart` in MigrationPlan (ADR 0027), so approval is explicit and the user has been warned about the procedure. A documented manual recovery path (`apprafter platform rescue`) reinstalls Argo CD from the loader in case of total Argo CD failure.

## Owner

Core platform team.

## Re-evaluation triggers

- If Argo CD upstream changes its CRD versioning or self-managing pattern in a way that makes the approach unsupported.
- If drift correction on platform components produces operational pain (false positives, reconcile storms, conflicts with admission webhooks) that outweighs the benefit. Mitigation path: selective `syncPolicy.automated.selfHeal: false` per component.
- If a competing GitOps engine (Flux, Kargo) becomes preferable for self-managing patterns, re-evaluate against ADR 0001's "one of each architectural slot" principle.

## Still open

- **Cert-manager bootstrap path for users with a domain from day one.** A user who configures `spec.argocd.domain` at bootstrap time expects HTTPS via cert-manager immediately. Currently the flow is: bootstrap → Argo CD reconciles cert-manager → Argo CD UI eventually serves a real cert. The window between Argo CD coming up (self-signed) and cert-manager issuing the real cert (a few minutes) needs documented UX.
- **Future enhancement: version skip.** Allow users to skip an available platform update entirely (ignore until the next one). Adds a `status.skippedVersions` list. Useful when users see an update in MigrationPlan but want to defer past it without rejecting.
- **Future enhancement: partial migration.** Allow per-component approval when a platform update touches multiple components. Currently all-or-nothing per MigrationPlan; partial flow would split into per-component sub-plans.

## References

- [Argo CD Autopilot](https://github.com/argoproj-labs/argocd-autopilot) — reference for self-managing pattern.
- Spec §1.4 (GitOps as control surface), §1.5 (decl-first).
- ADR 0023 (Kamaji multi-tenancy) — interaction with per-tenant Argo CD scope is addressed in Phase 2 design work.
- ADR 0026 (PlatformStack CRD) — declarative interface for managing the components installed via this mechanism.
- ADR 0028 (Distribution) — where platform-stack chart artifacts come from.
