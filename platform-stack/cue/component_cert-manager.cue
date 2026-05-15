// SPDX-License-Identifier: FSL-1.1-MIT

package platformstack

// cert-manager + the AppRafter self-signed `ClusterIssuer`.
// Pinned to v1.16.2 — same as v0.1.x cluster-bootstrap.
//
// The chart itself only installs the cert-manager controllers
// and the `crds: enabled: true` flag wires its CRD bundle. The
// `apprafter-selfsigned` ClusterIssuer is rendered separately
// by the umbrella chart's `templates/applications.yaml` chart
// template (Argo CD treats it as a plain manifest).
_components: "cert-manager": #Component & {
	name:      "cert-manager"
	enabled:   bool | *true
	namespace: "cert-manager"
	source: {
		repoURL: "https://charts.jetstack.io"
		chart:   "cert-manager"
	}
	version: "v1.16.2"
	values: {
		crds: {
			enabled: true
			keep:    true
		}
		// Single-replica matches tier-1 baseline (cpx22 has 4
		// GiB RAM — three controller replicas is wasteful).
		// Tier 2+ overlays bump this to 2+.
		replicaCount: int | *1
	}
}
