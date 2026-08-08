// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// Dragonfly operator — the launch-default in-cluster Redis-compatible
// backend (ADR 0042). Always-on at every tier (the operator is a single
// small Deployment); the actual shared `Dragonfly` instances are NOT
// seeded here — the resourceclaim-provisioner (plan.md 2.6-3) creates a
// pool of shared instances lazily on the first matched redis claim, so
// solo clusters with no redis apps pay no Dragonfly-pod cost. The
// matching `redis-integrated` ServiceProvider CR is seeded by
// `service_providers.cue` via the umbrella's templates.
//
// Chart `dragonfly-operator` v1.5.0 bundles operator appVersion v1.5.0
// and installs the `dragonflydb.io/v1alpha1` CRD bundle (kind
// `Dragonfly`, plural `dragonflies`) the 2.6-3 provisioner manages.
_components: "dragonfly-operator": #Component & {
	name:      "dragonfly-operator"
	enabled:   bool | *true
	namespace: "dragonfly-system"
	project:   "platform-providers"
	source: {
		// OCI Helm chart — repoURL is the oci:// registry path; the
		// umbrella renders an Argo CD Application with chart + version.
		repoURL: "ghcr.io/dragonflydb/dragonfly-operator/helm"
		chart:   "dragonfly-operator"
	}
	version: "v1.5.0" // discovered from the upstream chart's Chart.yaml
	values: {
		// 2.16d resource requests/limits for the operator MANAGER
		// container (measured 16Mi → req 16Mi / limit 64Mi, cpu 25m,
		// no cpu limit). The chart nests the manager under `manager.*`
		// (top-level `resources` is NOT read); its default is `{}`
		// (BestEffort) so this seed is what lifts the pod to Burstable.
		// The chart's kube-rbac-proxy sidecar (`rbacProxy.enabled: true`)
		// already ships its own requests+limits, so it is not BestEffort
		// and is left at the chart default. This sizes the OPERATOR pod
		// only; the shared Dragonfly instances the provisioner creates get
		// their Guaranteed resources from the redis-integrated
		// ServiceProvider config (service_providers.cue).
		manager: resources: {
			requests: {
				cpu:    "25m"
				memory: "16Mi"
			}
			limits: memory: "64Mi"
		}
	}

	// Operator + its CRD bundle must exist before any ServiceProvider CR
	// or (later) the lazily-created shared Dragonfly instances reference
	// the `Dragonfly` kind.
	syncWave: -5

	ignoreDifferences: [{
		group: "apps"
		kind:  "Deployment"
		jsonPointers: ["/status/terminatingReplicas"]
	}]
}
