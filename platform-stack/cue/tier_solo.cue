// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// Tier-1 (solo) overlay. Targets a single Hetzner cpx22
// (2 vCPU / 4 GiB / 40 GiB) and the small-team / hobbyist
// audience that wants AppRafter without running a full
// platform team's worth of observability.
//
// Default decisions, all overridable via the user's
// `Infrastructure.cue` once the operator overlay lands:
//
//   - Backstage off (most solo installs don't run their own
//     DNS for an internal portal).
//   - Hubble off (saves the ~150 MiB the Hubble UI + relay
//     would cost).
//   - argocd-cue-cmp off until the sidecar wiring lands in
//     plan.md 1.69.
//   - Everything else (Cilium, cert-manager, Argo CD,
//     apprafter-operator, admission-webhook,
//     network-policies) ON with their default values.
//
// Imported as a renderable concrete value by the umbrella
// chart's `render.cue` command (lands in plan.md 1.67).
//
// `_components` is the package-level base — populated by every
// `cue/component_*.cue` file — and the tier instance unifies
// it with its own field overrides in a single expression
// (`_components & { ... overrides ... }`) so each cilium /
// backstage / … field merges atop the corresponding base
// declaration rather than shadowing it.

tier1: #PlatformValues & {
	version: currentVersion
	tier:    1
	channel: "stable"
	components: _components & {
		cilium: values: hubble: enabled: false
		backstage: enabled:        false
		"argocd-cue-cmp": enabled: false
		"admission-webhook": values: replicas:  1
		"cert-manager": values: replicaCount:   1
		"apprafter-operator": values: replicas: 1
	}
}
