# Changelog — platform-stack

> Operator-facing release notes for the AppRafter platform-stack
> umbrella chart. One entry per published version
> (`platform-stack/v<version>` tag). For source-tree-level changes
> tracked per AppRafter monorepo release, see
> `docs/changelog/UNRELEASED.md`.
>
> Format follows [Keep a Changelog] 1.1.0. Versioning follows
> semver: MAJOR for chart-shape / component-set incompatibilities,
> MINOR for additive component changes, PATCH for bug fixes and
> dependency bumps within the same chart shape.
>
> Each release also gets a `change` classification entry in
> `cue/compatibility.cue` consumed by `PlatformController` (Phase 2+)
> to gate automated upgrades. See [ADR
> 0028](../docs/adr/0028-platform-stack-distribution.md) for the
> distribution model.

## Unreleased

_Scaffold under construction — published versions start at 0.2.0
once the renderer (plan.md 1.67) and publish workflow (1.68)
land._

## 0.2.0 (planned — aligned with `v0.2.0-services` milestone)

First published platform-stack version. Bundles the v0.1.x
cluster-bootstrap component set unchanged, sourced via Argo CD
instead of direct `helm upgrade --install`:

- Cilium 1.16.5 — CNI + kube-proxy replacement.
- cert-manager v1.16.2 — controllers + self-signed
  `ClusterIssuer`.
- Argo CD 7.7.7 — single-replica controllers, Dex off.
- apprafter-operator v0.1.91 — Application reconciler.
- apprafter-admission-webhook v0.1.91 — Application
  validation.
- network-policies — default-deny on `default` namespace, DNS
  allowance, Argo CD egress allowance.
- backstage — declared, default OFF in tier-1 overlay
  (requires `values.backstage.domain`).
- argocd-cue-cmp — declared, default OFF (sidecar wiring
  lands in 1.69).

**Change class:** `safe`. Operators upgrading from v0.1.x
in-tree bootstrap see identical component versions; only the
delivery path changes.

**Operator-version pin:** v0.1.91.

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
