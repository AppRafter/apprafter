// SPDX-License-Identifier: MIT
//
// cue-cmp intra-bundle consistency fixture (ADR 0063 §Decision 5) —
// Style A and Style B MIXED in one package.
//
// `apiVersion`/`kind`/`metadata`/`spec` at package scope (Style A, the
// unwrapped layout) PLUS a named wrapper carrying a second manifest
// (Style B). The entrypoint dispatches on whether the exported JSON
// itself carries apiVersion+kind, so the Style-A branch matches first
// and returns before the Style-B enumeration is ever reached.
//
// This is the one inconsistency the render layer is the ONLY possible
// place to catch: the wrapper is not dropped by a validator, it is
// emitted as a stray top-level key inside the Style-A document, and the
// apiserver PRUNES an unknown top-level key without an error. The
// discarded manifest therefore never becomes an API object at all —
// there is nothing downstream left to inspect it.
//
// Before the guard: exit 0, one document emitted, `mixed-wrapped`
// silently gone.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

apiVersion: "apprafter.io/v1alpha1"
kind:       "Application"
metadata: {
	name:      "mixed-package-scope"
	namespace: "mixed-demo"
}
spec: base: {
	image:    "nginxdemos/hello:plain-text"
	replicas: 1
}

wrapped: v1alpha1.#Application & {
	metadata: {
		name:      "mixed-wrapped"
		namespace: "mixed-demo"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
