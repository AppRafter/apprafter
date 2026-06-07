// SPDX-License-Identifier: MIT
//
// cue-cmp Style-B inject fixture — two named-wrapper manifests in
// one file. Exercises the entrypoint's per-key re-export loop so
// the test confirms injection applies to EVERY emitted document of
// a multi-manifest stream (not just the first).

package apprafter

appOne: {
	apiVersion: "apprafter.io/v1alpha1"
	kind:       "Application"
	metadata: name: "inject-multi-one"
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}

appTwo: {
	apiVersion: "apprafter.io/v1alpha1"
	kind:       "Application"
	metadata: name: "inject-multi-two"
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
