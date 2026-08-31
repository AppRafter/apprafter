// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// image-rollback-walk e2e fixture — ADR 0059.
//
// Style A (unwrapped): apiVersion + kind at the top level so the
// cue-cmp entrypoint emits this document verbatim without needing
// the monorepo's vendored schemas.
//
// The image is a PINNABLE PAIR. The walk starts here on 1.27 and
// then edits the manifest to 1.28, which is what makes the two
// resolutions genuinely differ — the operator retains 1.27's digest
// as the rollback target, and `apprafter app rollback` holds the
// application there while the manifest still says 1.28.
//
// Two public tags rather than a pushed pair: the operator resolves
// digests over HTTPS against webpki roots with no escape hatch, so
// an in-cluster plain-HTTP registry cannot be used — and its failure
// would be SILENT (resolution falls back to the verbatim tag and the
// rollout proceeds), so the walk would look green while testing
// nothing. Official `nginx` alpine tags pull quickly and need no
// credentials, which keeps this walk hermetic.

package apprafter

apiVersion: "apprafter.io/v1alpha1"
kind:       "Application"
metadata: {
	name:      "image-rollback-app"
	namespace: "apprafter"
}
spec: base: {
	image:    "nginx:1.27-alpine"
	replicas: 1
	expose: {
		port: 80
	}
}
