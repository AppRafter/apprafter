// SPDX-License-Identifier: MIT
//
// The second FILE of the order fixture. Two files, not one, because a
// single-file package is the shape where every cue version agrees:
// the export follows declaration order there, and the rule under test
// cannot fail. The divergence measured in `Application.cue`'s header
// needs the file boundary to appear at all.
//
// `Application-preview.cue` sorts BEFORE `Application.cue` (`-` is
// 0x2D, `.` is 0x2E), so file order, key order and both evaluators'
// export orders are four different sequences over these four keys.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

cacheTier: v1alpha1.#Application & {
	metadata: {
		name:      "keyorder-yankee"
		namespace: "keyorder"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}

jobsTier: v1alpha1.#Application & {
	metadata: {
		name:      "keyorder-xray"
		namespace: "keyorder"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
