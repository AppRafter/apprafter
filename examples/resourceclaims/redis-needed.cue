// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Example ResourceClaim the Application operator generates when an app
// declares `needs.redis` (Phase 2.6 / ADR 0042). Lives in the consuming
// app's namespace, selects the in-cluster Dragonfly provider by label,
// carries the `persistent` passthrough copied from the originating need,
// and (post-provision) records the Dragonfly allocation under `status`
// (`instance` + numbered logical DB `$N`). Used as a `cue vet` fixture
// for the new spec.persistent + status.{instance,dbnum} fields.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

resourceClaimRedisNeeded: v1alpha1.#ResourceClaim & {
	metadata: {
		name:      "web-redis"
		namespace: "demo"
	}
	spec: {
		type: "redis"
		selector: {tier: "integrated"}
		persistent: true
	}
	status: {
		provider:            "redis-integrated"
		connectionSecretRef: "web-redis-conn"
		ready:               true
		instance:            "platform-redis-persistent-000"
		dbnum:               7
	}
}
