// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// cert-manager — the controllers and CRDs behind the AppRafter
// self-signed `ClusterIssuer`.
//
// Pinned to v1.21.2. Upstream supports 1.21 on Kubernetes 1.33-1.36;
// the previous pin, v1.16.2, went end-of-life on 2025-06-10 and was
// only ever supported up to Kubernetes 1.32. The 1.16 -> 1.21 jump
// skips four minors, which upstream advises against ("one minor
// version at a time"); it was proven as a single step on a kind
// v1.36.4 apiserver instead: v1.16.2 installed, a certificate issued
// and CA-injected, then upgraded in place — every CRD Established,
// the existing certificate kept (not reissued), a new one issued.
//
// What 1.21 changes that is visible here: the aggregate
// `cert-manager-edit` ClusterRole no longer lets namespace editors
// create ACME Challenges or create/patch/update Orders
// (GHSA-8rvj-mm4h-c258), the controller's self-`tokenrequest`
// Role/RoleBinding is gone, and the metrics Service port is renamed
// `tcp-prometheus-servicemonitor` -> `http-metrics`.
//
// The chart itself only installs the cert-manager controllers
// and the `crds: enabled: true` flag wires its CRD bundle. The
// `apprafter-selfsigned` ClusterIssuer is NOT part of this
// component: it ships in the admission-webhook chart
// (`templates/clusterissuer.yaml`) beside the Certificate that uses it.
_components: "cert-manager": #Component & {
	name:      "cert-manager"
	enabled:   bool | *true
	namespace: "cert-manager"
	source: {
		repoURL: "https://charts.jetstack.io"
		chart:   "cert-manager"
	}
	version: "v1.21.2"
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
