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

_Scaffold under construction — first publish is 0.1.0 once the
publish workflow (plan.md 1.68) runs end-to-end._

**Build-tooling notes (not part of any chart release):**

- The chart version is the **only** field a maintainer needs to
  bump: `platform-stack/cue/platform.cue` →
  `currentVersion: #Version & "<new>"`, then add the matching
  `compatibility.cue` entry. `tier_solo.cue` / `tier_team.cue` /
  the renderer / the workflow all derive from `currentVersion`
  via CUE references — no string-literal drift possible.
- The publish workflow is `workflow_dispatch` only; it **writes
  the `platform-stack/v<version>` tag itself** as the final step
  via `gh release create`. Tag pushes do NOT trigger it. This
  inverts the older "tag → publish" model so an accident tag
  push can't ship a half-baked chart.
- `cue vet -c ./platform-stack/cue/...` enforces
  `compatibility: (currentVersion): #VersionRecord` — i.e. the
  current version MUST have a compatibility entry, caught at
  edit time before any CI runs.

## 0.1.0 (planned — first published chart release)

First published platform-stack version. Minor tracks the
AppRafter monorepo **phase** (Phase 1.5 → chart 0.1.x; chart
MINOR will bump to 0.2.0 alongside the `v0.2.0-services`
milestone when Phase 2 services land). Chart patch versions
are independent of the monorepo's `v0.1.x` patch stream.

Bundles the v0.1.x
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
