// SPDX-License-Identifier: MIT
//
// The second FILE of the consistent multi-file bundle — the preview
// sibling, mirroring `landing/web/apprafter/Application-preview.cue`.
//
// Agrees with `Application.cue` on everything the four intra-bundle
// checks look at: same namespace, distinct (namespace, name), no
// `spec.environment` on either, no package-scope manifest. So the
// bundle must RENDER — and `app validate` must answer `2 workloads`,
// not `1`.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

multiFileWebPreview: v1alpha1.#Application & {
	metadata: {
		name:      "multi-file-web-preview"
		namespace: "multi-file"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
