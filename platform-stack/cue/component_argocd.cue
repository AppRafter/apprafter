// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// Argo CD's own Application. Self-managing: Argo CD reconciles
// its own chart definition, prune=false so a stale chart can't
// delete the controllers responsible for re-installing it (a
// foot-gun the v0.1.x bootstrap already avoids by installing
// Argo CD via direct `helm upgrade --install` rather than
// through Argo CD itself; here we make the self-management
// explicit via `prune: false`).
//
// Pinned to v7.7.7 — same as v0.1.x cluster-bootstrap.
_components: argocd: #Component & {
	name:      "argocd"
	enabled:   bool | *true
	namespace: "argocd"
	source: {
		repoURL: "https://argoproj.github.io/argo-helm"
		chart:   "argo-cd"
	}
	version: "7.7.7"
	values: {
		// Single replicas on tier-1 (cpx22 RAM budget). Tier
		// 2+ overlays scale these up; Dex stays off until
		// OIDC SSO lands in Phase 3.
		controller: replicas:         int | *1
		repoServer: replicas:         int | *1
		server: replicas:             int | *1
		applicationSet: replicaCount: int | *1
		notifications: replicas:      int | *1
		dex: enabled:                 bool | *false
		// CMP sidecar configuration lives in
		// argocd-cue-cmp.cue and is merged into the same
		// values object via overlay.
	}
	syncPolicy: {
		automated: {
			// Self-managing: NEVER prune. A stale upstream chart
			// must not delete the controllers responsible for
			// re-installing it; manual cleanup is the contract.
			prune:    false
			selfHeal: bool | *true
		}
		syncOptions: [...string] | *["CreateNamespace=true", "ServerSideApply=true"]
	}
}
