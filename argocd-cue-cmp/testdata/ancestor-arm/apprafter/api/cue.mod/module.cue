// SPDX-License-Identifier: MIT
//
// Minimal CUE module for the ancestor-arm fixture. Rooted at `api/` so
// it also covers `api/helpers/` when the render runs from there — both
// candidate directories resolve through this one module, and no schema
// injection is needed (Style A manifests, no imports).

module: "ancestor-arm.cue-cmp.apprafter.io"

language: {
	version: "v0.10.0"
}
