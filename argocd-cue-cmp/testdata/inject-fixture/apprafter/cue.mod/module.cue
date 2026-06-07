// SPDX-License-Identifier: MIT
//
// Minimal CUE module for the cue-cmp inject-fixture. Uses a
// standalone module (no imports) so the fixture is self-contained
// and does not need the monorepo's vendored schemas — the same
// convention as e2e/fixtures/gitops-app.

module: "inject-fixture.cue-cmp.apprafter.io"

language: {
	version: "v0.10.0"
}
