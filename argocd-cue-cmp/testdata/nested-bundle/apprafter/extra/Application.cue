// SPDX-License-Identifier: MIT
//
// The nested second bundle. A real Application, so it matches the
// narrower "would this have rendered?" pattern the fold notice is gated
// on — unlike helper-subpackage's `lib/consts.cue`, which mentions
// `#ApplicationSpec` and renders nothing.
//
// `package extra`, not `package apprafter`: CUE unifies same-named
// packages in ANCESTOR directories up to the module root, so sharing
// the name would make a render from here drag in `../Application.cue`.
// A separate name keeps this a genuinely separate bundle, which is the
// layout under test.

package extra

import v1alpha1 "apprafter.io/schemas/v1alpha1"

inner: v1alpha1.#Application & {
	metadata: {
		name:      "inner-bundle"
		namespace: "nested-demo"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
