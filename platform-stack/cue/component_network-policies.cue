// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// Default-deny NetworkPolicy bundle. Plain manifests under
// `manifests/tier-1/network-policies/`. The umbrella chart's
// template turns this into one Argo CD Application that syncs
// the directory.
//
// The bundle includes:
//
//   - Default-deny on the `default` namespace (every pod is
//     denied egress + ingress until an Application's own
//     NetworkPolicy opens specific peers).
//   - Allow-DNS to `kube-system/kube-dns` so workloads can
//     resolve names without each Application declaring its
//     own DNS allowance.
//   - Allow-Argo-CD egress to GitHub / OCI registries so the
//     bootstrap Application keeps syncing under default-deny.
//
// Tier 2+ overlays may extend this with NetworkPolicies for
// observability backends; tier-1 ships only the minimum.
_components: "network-policies": #Component & {
	name:      "network-policies"
	enabled:   bool | *true
	namespace: "default"
	source: {
		repoURL: "https://github.com/apprafter/apprafter.git"
		path:    "manifests/tier-1/network-policies"
	}
	version: "v0.1.91"
	values: {
		// No values today; the manifests are static. Kept as
		// an empty object so the chart template's
		// `valuesObject` lookup doesn't choke.
	}
	syncPolicy: {
		automated: {
			// Prune ON — stale NetworkPolicies actively break
			// security model invariants, so leaving them
			// behind on chart removal is the wrong default.
			prune:    bool | *true
			selfHeal: bool | *true
		}
		syncOptions: [...string] | *["CreateNamespace=true", "ServerSideApply=true"]
	}
}
