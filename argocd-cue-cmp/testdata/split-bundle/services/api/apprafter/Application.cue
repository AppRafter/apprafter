// SPDX-License-Identifier: MIT
//
// cue-cmp split fixture (ADR 0063) — two SEPARATE bundles under one
// repository, each in its own directory. Registered individually they
// each render; registered together at the repository root they are an
// ambiguous path and the render must refuse rather than silently
// rendering one of them.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

api: v1alpha1.#Application & {
	metadata: {
		name:      "split-api"
		namespace: "split-demo"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
