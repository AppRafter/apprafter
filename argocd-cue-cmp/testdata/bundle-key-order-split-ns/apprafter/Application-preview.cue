// SPDX-License-Identifier: MIT
//
// The second FILE of the refusing order fixture — the preview
// namespace half. Its two workloads declare `preview` where
// `Application.cue`'s declare `prod`, which is the divergence the
// namespace check refuses.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

cacheTier: v1alpha1.#Application & {
	metadata: {
		name:      "keyorder-yankee"
		namespace: "preview"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}

jobsTier: v1alpha1.#Application & {
	metadata: {
		name:      "keyorder-xray"
		namespace: "preview"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
