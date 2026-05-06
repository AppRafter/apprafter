// SPDX-License-Identifier: FSL-1.1-MIT
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

infra: v1alpha1.#Infrastructure & {
	metadata: {
		name: "platform-1"
	}
	spec: {
		provider: "hetzner-cloud"
		nodes: [{
			role:  "control-plane"
			type:  "cx22"
			count: 1
		}]
		osImage: "ubuntu-24.04"
	}
}
