// SPDX-License-Identifier: MIT
//
// Shared CUE for the helper-sub-package fixture. This file is what makes
// the fixture interesting: it imports the AppRafter schema, so it carries
// the ADR 0063 manifest marker and the entrypoint's directory search
// returns `./apprafter/lib` alongside `./apprafter`. Nesting is what
// tells the two apart — a directory under a package directory is part of
// that package's tree.

package lib

import v1alpha1 "apprafter.io/schemas/v1alpha1"

// Constrain the shared values against the schema, the reason a helper
// would import it in the first place.
#Spec: v1alpha1.#ApplicationSpec

Namespace: "helper-demo"
Image:     "nginxdemos/hello:plain-text"
