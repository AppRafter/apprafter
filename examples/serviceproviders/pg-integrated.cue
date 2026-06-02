// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Example ServiceProvider — the launch-default in-cluster Postgres
// backend (CloudNativePG). ResourceClaims select it via the
// `tier: integrated` label. Used as a `cue vet` fixture.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

serviceProviderPgIntegrated: v1alpha1.#ServiceProvider & {
	metadata: {
		name:      "pg-integrated"
		namespace: "apprafter-system"
		labels: {tier: "integrated", location: "in-cluster"}
	}
	spec: {
		type:    "pg"
		backend: "cloudnative-pg"
		config: {
			cluster: "platform-postgres"
			nodes:   3
		}
	}
}
