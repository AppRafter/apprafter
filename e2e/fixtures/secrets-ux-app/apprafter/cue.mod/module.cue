// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Minimal CUE module for the secrets-ux-walk e2e fixture
// (e2e/secrets-ux-walk.sh — the CLI half of day-2 ledger entries
// D6 + D7). A standalone module with no imports so the fixture is
// self-contained and needs no vendored schema — mirrors the
// per-env-app / gitops-app fixture convention.

module: "secrets-ux-app.e2e.apprafter.io"

language: {
	version: "v0.10.0"
}
