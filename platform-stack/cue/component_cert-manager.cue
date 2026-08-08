// SPDX-License-Identifier: FSL-1.1-Apache-2.0

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
		// 2.16d resource requests/limits (measured RSS×0.8 request /
		// tight mem limit / modest cpu request / no cpu limit). The chart
		// splits the three Deployments: top-level `resources` is the
		// controller; webhook + cainjector carry their own keys. No pod
		// stays BestEffort.
		resources: {
			requests: memory: "24Mi"
			limits: memory:   "128Mi"
		}
		webhook: resources: {
			requests: memory: "16Mi"
			limits: memory:   "64Mi"
		}
		cainjector: resources: {
			requests: memory: "32Mi"
			limits: memory:   "128Mi"
		}
	}

	// cert-manager must be Synced before the admission-webhook
	// chart applies its `Certificate` resource — otherwise the
	// request fails with `no endpoints available for service
	// cert-manager-webhook` and Argo CD retries with backoff
	// for minutes before convergence.
	syncWave: -10

	// cert-manager ships three Deployments (controller,
	// webhook, cainjector); each surfaces
	// `status.terminatingReplicas` on k3s v1.35. Same skew as
	// every other component with a Deployment.
	ignoreDifferences: [{
		group: "apps"
		kind:  "Deployment"
		jsonPointers: ["/status/terminatingReplicas"]
	}]
}
