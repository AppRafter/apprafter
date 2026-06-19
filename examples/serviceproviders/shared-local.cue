// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Example ServiceProvider — the 2.6c shared-disk provider that backs
// `needs.sharedVolume` claims with a single RWX PVC mounted by
// multiple pods simultaneously. Used as a `cue vet` fixture to guard
// the `shared-disk` type against accidental removal from
// `#PlatformServiceType`.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

serviceProviderSharedLocal: v1alpha1.#ServiceProvider & {
	metadata: {
		name:      "shared-local"
		namespace: "apprafter-system"
		labels: {tier: "integrated", location: "in-cluster"}
	}
	spec: {
		type:    "shared-disk"
		backend: "shared-disk"
		config: {
			storageClass: "local-path"
		}
	}
}
