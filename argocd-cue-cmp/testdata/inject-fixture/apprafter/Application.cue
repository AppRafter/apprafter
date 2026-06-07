// SPDX-License-Identifier: MIT
//
// cue-cmp inject-fixture — minimal AppRafter Application manifest.
//
// Style A (unwrapped): apiVersion + kind declared at the top level
// so the cue-cmp entrypoint emits this document verbatim without
// needing the monorepo's vendored schemas. The CUE module boundary
// is in the sibling cue.mod/ directory so `cue export ./...` works
// when the repo-server's cwd is this apprafter/ directory.
//
// test-inject.sh renders this twice: once with APPRAFTER_APP_ENV
// set (asserting spec.environment + the apprafter.io/environment
// label are injected) and once without it (asserting the base
// manifest is emitted unchanged).

package apprafter

apiVersion: "apprafter.io/v1alpha1"
kind:       "Application"
metadata: {
	name:      "inject-fixture"
	namespace: "apprafter"
}
spec: base: {
	image:    "nginxdemos/hello:plain-text"
	replicas: 1
	expose: {
		port:   80
		public: false
	}
}
