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

_Nothing pending. Note that this file has sections for 0.1.0 and
0.2.56 only — the versions between them were never written up
here. Their operator-facing notes live in `cue/compatibility.cue`
(read by `PlatformController`, and the authoritative per-version
record) and in `docs/changelog/UNRELEASED.md`._

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

## 0.2.56 — resource governance corrected (2026-08-21)

Reclaims **256Mi of memory and 100m of CPU** in platform requests
on a single-replica (solo) tier, and repairs a defect that made
vertical autoscaling recommend a constant instead of a
measurement. Found on a live ~4GB Tier-1 node that had booked
1952Mi of its 1959Mi allocatable memory in requests, with
Postgres `Pending` for four days and no running instance;
platform infrastructure held 86.9% of those requests, user
application code 13.1%.

**The load-bearing item — VPA's own recommendation floors.** All
five `VerticalPodAutoscaler` objects on that cluster reported an
*identical* `250Mi / 25m` target, across workloads whose real
working sets ranged from 6Mi to 82Mi. That constant is upstream's
own minimum-recommendation floor:
`--pod-recommendation-min-memory-mb` defaults to **250** and
`--pod-recommendation-min-cpu-millicores` to **25** (VPA 1.7.1,
`pkg/recommender/config/config.go`), and neither was ever set.
Upstream applies those floors **first** and clamps into
`[minAllowed, maxAllowed]` **afterwards** — so with
`32 < 250 < 512`, the `minAllowed: 32Mi` the operator renders on
every app VPA could not bind under any input. (`maxAllowed: 512Mi`
is a different case: it still binds whenever a recommendation
exceeds it, and simply never did here because every
recommendation sat at the floor.) Both floors are now pinned on
the recommender: memory to the 32Mi application seed, CPU at the
value it already defaults to. Full account, including the
coupling this creates, in
[ADR 0054](../docs/adr/0054-vpa-vertical-autoscaling.md).

**0.2.55 is yanked.** Its one-string feature-gate fix is correct,
but it started vertical autoscaling for the first time on every
cluster carrying it. With the shipped default `mode: full` and
these floors unset, every managed pod would have been admitted at
250Mi instead of the 32Mi seed. A VPA targets a *Deployment* and
every pod of that Deployment is admitted at the recommendation, so
the five objects above cover five Deployments totalling **eight
pods** (three of the five applications run two replicas):
`8 × (250 − 32)` = +1744Mi on a node with 7Mi free. At a 250Mi
floor the admissible application count on this tier is zero.

Reclaims, each against a measured live footprint:

- **VPA recommender `replicas` 2 → 1** — 64Mi + 50m. Upstream
  defaults all three VPA components to 2; the admission
  controller and updater were already pinned and the recommender
  was missed. Live 19Mi (leader) and 8Mi (standby).
- **sealed-secrets 64Mi → 32Mi** — the 2.16d baseline measured
  22Mi and chose 32Mi; 64Mi shipped by mistake. Live 14Mi.
- **dragonfly kube-rbac-proxy 64Mi → 16Mi** — the upstream
  default, never pinned here. Live 9Mi, and that is a
  zero-traffic figure: nothing scrapes the endpoint it guards
  today. It stays enabled; re-measure if `serviceMonitor` is ever
  turned on.
- **apprafter-operator 128Mi → 64Mi** — 2.16d measured 12Mi and
  live is 15Mi, but **64Mi is deliberate headroom for reconcile
  storms, not a measurement**, and it deviates upward from ADR
  0053's "request ≈ measured RSS × 0.8" rule on purpose. Kubelet
  ranks evictions by usage above request, and this is a
  single-replica control plane.
- **admission-webhook 64Mi → 16Mi** — 2.16d measured 1Mi; live
  falls below `kubectl top`'s 5Mi reporting cutoff.
- **cilium `cni.resources` pinned** — the remaining 50m of CPU. A
  pod's CPU request is `max(sum of containers, max of init
  containers)`, and `install-cni-binaries` inherited the chart
  default of 100m, which shadowed the 50m pinned for the agent.
  Effective pod request goes 100m → 50m; memory was unaffected.

Also in this release:

- **The three VPA PodDisruptionBudgets are disabled.** Upstream
  ships all three `enabled: true` with `minAvailable: 1`, which
  against our pinned `replicas: 1` can never be satisfied —
  `kubectl drain` blocks forever on the node holding the only
  copy. This is valid **only at one replica**: raising any of the
  three above `replicas: 1` must re-enable that component's PDB
  in the same edit.
- **`limits.cpu` removed** from apprafter-operator (500m),
  admission-webhook (200m) and the dragonfly kube-rbac-proxy
  (500m, via a Helm null-override), per ADR 0053 §1 — platform
  components get a modest CPU request and no CPU limit. Memory
  limits are unchanged.
- **The admission-webhook chart pin is corrected v0.2.38 →
  v0.2.42.** It sat four versions behind the shipped chart tree
  through an oversight repeated across three releases. The
  intervening versions were functionally identical republishes,
  so this carries no unreviewed behaviour change — but the
  webhook image on an existing cluster does roll v0.2.38 →
  v0.2.42 on upgrade.

**Upgrade hazard on an already-saturated node.** The VPA chart's
cert-generation Job is a Helm `pre-upgrade` hook (PreSync for
Argo CD) requesting 16Mi, and a hook that cannot schedule means
the sync phase never runs — so the cluster that most needs this
fix is the one that can fail to install it. If the `vpa`
Application hangs with a `Pending` `...admission-certgen-...`
pod, free a little allocatable first
(`apprafter platform autoscale set off`, then reclaim any
orphaned backend) and the sync proceeds.

**Change class:** `requires-restart`, **correcting the `safe`
that 0.2.55 shipped**. A cluster-wide mutating admission webhook
on pod CREATE plus an updater that mutates live pods is not a
`safe` auto-sync, and this is the release in which that machinery
first runs with usable floors.

**Operator-version pin:** v0.2.42.

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
