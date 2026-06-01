// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// gitops-walk e2e fixture — minimal AppRafter Application manifest.
//
// Style A (unwrapped): apiVersion + kind declared at the top level
// so the cue-cmp entrypoint emits this document verbatim without
// needing the monorepo's vendored schemas. The CUE module boundary
// is in the sibling cue.mod/ directory so `cue export ./...` works
// when the repo-server's cwd is this apprafter/ directory.
//
// The operator reconciler renders this into a Deployment named
// "gitops-app" in the "apprafter" namespace. nginxdemos/hello is
// used because it pulls quickly (~4 MB) and responds on port 80.

package apprafter

apiVersion: "apprafter.io/v1alpha1"
kind:       "Application"
metadata: {
	name:      "gitops-app"
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
