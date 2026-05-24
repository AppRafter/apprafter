// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// Tier-2 (team) overlay. Targets the ≥3-node Hetzner cluster
// where the team-of-developers audience wants the full
// AppRafter experience minus the regulated-tier compliance
// stack.
//
// Default decisions vs `tier_solo.cue`:
//
//   - Hubble on (relay + UI) — multi-node networking is
//     opaque without it, and a team can afford the memory.
//   - Backstage on by default (with the operator supplying
//     `values.backstage.domain` from the user's
//     Infrastructure manifest).
//   - admission-webhook replicas = 2 (gap-free validating
//     webhook during rolling restarts).
//   - cert-manager replicaCount = 2 (controller HA).
//   - apprafter-operator replicas = 2 (leader-election keeps
//     a single active reconciler, but a second pod takes
//     over instantly on the active leader's eviction).
//
// Future tier additions (Kamaji multi-tenancy, Capsule, etc.)
// belong in `cue/tier_prod.cue` and `cue/tier_regulated.cue`
// which land alongside the Phase 2 / Phase 3 work that
// actually wires those components in.

tier2: #PlatformValues & {
	version:     currentVersion
	tier:        2
	channel:     "stable"
	appProjects: _appProjects
	components: _components & {
		cilium: values: hubble: {
			enabled: true
			relay: enabled: true
			ui: enabled:    true
		}
		backstage: enabled: true
		// argocd-cue-cmp stays disabled at tier-2 too until
		// the sidecar wiring (plan.md 1.69) lands. Pin
		// explicitly so `cue vet -c` accepts the default
		// instead of reporting `bool | *false` as incomplete.
		"argocd-cue-cmp": enabled: false
		"admission-webhook": values: replicas:  2
		"cert-manager": values: replicaCount:   2
		"apprafter-operator": values: replicas: 2
	}
}
