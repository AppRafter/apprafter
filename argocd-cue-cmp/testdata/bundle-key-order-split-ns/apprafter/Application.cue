// SPDX-License-Identifier: MIT
//
// cue-cmp bundle fixture — the ORDER rule on the REFUSAL side.
//
// Same four keys and the same two files as `bundle-key-order/`, with
// one field changed: the workloads split across two `metadata.
// namespace` values, so the bundle is refused and nothing reaches
// stdout. What is asserted here is the summary line — the one Argo CD
// truncates onto the Application tile, and the one `apprafter app
// validate` reproduces locally. Both must name the four workloads in
// the same sequence, or one finding reads as two.
//
// The consistent half cannot cover this: a refusal emits no documents,
// and the emitted documents are all the consistent half has. The two
// fixtures are one question asked on both exits.
//
// Key order is sorted; nothing else is (see `bundle-key-order/
// apprafter/Application.cue` for the three sequences these same keys
// take across cue versions).

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

webTier: v1alpha1.#Application & {
	metadata: {
		name:      "keyorder-whiskey"
		namespace: "prod"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}

apiTier: v1alpha1.#Application & {
	metadata: {
		name:      "keyorder-zulu"
		namespace: "prod"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
