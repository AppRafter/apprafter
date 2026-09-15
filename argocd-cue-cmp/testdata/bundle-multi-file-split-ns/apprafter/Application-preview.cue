// SPDX-License-Identifier: MIT
//
// The second FILE of the inconsistent multi-file bundle — same package,
// same directory, a DIFFERENT `metadata.namespace` from its sibling in
// `Application.cue`.
//
// This is the realistic form of the mistake: a preview manifest copied
// from its prod sibling and given a namespace of its own, in a file the
// author of the prod manifest never reopened.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

splitFileWebPreview: v1alpha1.#Application & {
	metadata: {
		name:      "split-file-web-preview"
		namespace: "preview"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
