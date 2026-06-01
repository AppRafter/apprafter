// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Minimal CUE module for the gitops-walk e2e fixture. Uses a
// standalone module (no imports) so the fixture is self-contained
// and does not need the monorepo's vendored schemas.

module: "gitops-app.e2e.apprafter.io"

language: {
	version: "v0.10.0"
}
