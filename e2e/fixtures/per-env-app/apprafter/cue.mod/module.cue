// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Minimal CUE module for the per-env gitops-walk e2e fixture
// (e2e/gitops-walk-per-env.sh, subphase 2.9, ADR 0044). A
// standalone module (no imports) so the fixture is self-contained
// and does not need the monorepo's vendored schemas — mirrors the
// gitops-app fixture's module convention.

module: "per-env-app.e2e.apprafter.io"

language: {
	version: "v0.10.0"
}
