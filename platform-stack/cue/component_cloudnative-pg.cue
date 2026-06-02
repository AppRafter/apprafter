// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// CloudNativePG operator — the launch-default in-cluster Postgres
// backend. Always-on at tier-1 (the operator is a single small
// Deployment; the actual Postgres `Cluster` is NOT seeded here —
// the resourceclaim-provisioner (plan.md 2.4c) creates the shared
// `platform-postgres` Cluster lazily on the first matched pg
// claim, so solo clusters with no pg apps pay no Postgres-pod
// cost). The matching `pg-integrated` ServiceProvider CR is seeded
// by `service_providers.cue` via the umbrella's
// `templates/serviceproviders.yaml`.
//
// Chart `cloudnative-pg` 0.28.2 bundles operator appVersion 1.29.1
// (declarative `Database` CRD + managed roles available — used by
// the 2.4c provisioner). `crds.create: true` (default) installs
// the CNPG CRD bundle; CNPG manages its own webhook certificates,
// so there is no cert-manager dependency.
_components: "cloudnative-pg": #Component & {
	name:      "cloudnative-pg"
	enabled:   bool | *true
	namespace: "cnpg-system"
	project:   "platform-providers"
	source: {
		repoURL: "https://cloudnative-pg.github.io/charts"
		chart:   "cloudnative-pg"
	}
	version: "0.28.2"
	values: {
		// Default CRD install — ships Cluster/Database/etc.
		crds: create: true
		// Single operator replica matches the tier-1 baseline
		// (cpx22, 4 GiB). Tier 2+ overlays may bump for HA.
		replicaCount: int | *1
	}

	// The operator + its CRD bundle must exist before any
	// ServiceProvider CR or (later) the lazily-created
	// `platform-postgres` Cluster references CNPG kinds.
	syncWave: -5

	// CNPG ships a single controller Deployment; k3s v1.35
	// surfaces `status.terminatingReplicas`, the same skew every
	// Deployment-bearing component mutes.
	ignoreDifferences: [{
		group: "apps"
		kind:  "Deployment"
		jsonPointers: ["/status/terminatingReplicas"]
	}]
}
