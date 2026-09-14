// SPDX-License-Identifier: MIT
//
// cue-cmp helper-sub-package fixture (ADR 0063 / ADR 0029). ONE bundle
// whose package directory contains a helper sub-package: `apprafter/`
// holds the manifest, `apprafter/lib/` holds shared CUE the manifest
// imports. The helper imports the schema, so it carries the ADR 0063
// marker and the directory search finds BOTH directories — but `lib/`
// is part of this bundle's tree, not a second bundle, so the render
// must proceed rather than refuse.
//
// Rendered identically whether registered at the repository root or at
// `--path apprafter`; test-inject.sh asserts both.

package apprafter

import (
	v1alpha1 "apprafter.io/schemas/v1alpha1"
	"helper-subpackage.cue-cmp.apprafter.io/lib"
)

helperApp: v1alpha1.#Application & {
	metadata: {
		name:      "helper-app"
		namespace: lib.Namespace
	}
	spec: base: {
		image:    lib.Image
		replicas: 1
	}
}
