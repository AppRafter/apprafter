// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Minimal CUE module for the 2.16c expose deep-merge e2e fixture
// (e2e/expose-deep-merge-walk.sh). A standalone module (no imports)
// so the fixture is self-contained and does not need the monorepo's
// vendored schemas — mirrors the per-env-app / gitops-app fixture
// module convention. The cue-cmp sidecar (and `apprafter app add`)
// inject the shared schema at render/validate time.

module: "expose-deep-merge-app.e2e.apprafter.io"

language: {
	version: "v0.10.0"
}
