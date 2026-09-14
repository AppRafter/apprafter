// SPDX-License-Identifier: MIT
//
// cue-cmp nested-bundle fixture (ADR 0063 §Decision 3) — the DUAL of
// the helper-subpackage fixture. Same shape on disk: a package
// directory with a marker-bearing directory inside it. The difference
// is what that inner directory holds — `extra/` holds a second real
// Application, not shared CUE.
//
// The nesting rule folds it into this package either way, because the
// marker cannot tell the two apart. What it must NOT do is fold it away
// in silence: a `cue export .` here renders this manifest only, so
// `inner-bundle` never deploys, and the operator has to be able to find
// out why from the sync log.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

outer: v1alpha1.#Application & {
	metadata: {
		name:      "outer-bundle"
		namespace: "nested-demo"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
