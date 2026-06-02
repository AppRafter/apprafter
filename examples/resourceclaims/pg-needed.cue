// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Example ResourceClaim — what the Application operator generates
// when an app declares `needs.pg` (the generation itself lands in
// 2.4). Lives in the consuming app's namespace, selects the
// in-cluster Postgres provider by label. Used as a `cue vet` fixture.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

resourceClaimPgNeeded: v1alpha1.#ResourceClaim & {
	metadata: {
		name:      "demo-web-pg"
		namespace: "demo"
	}
	spec: {
		type: "pg"
		selector: {tier: "integrated"}
		size: "small"
	}
}
