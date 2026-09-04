// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Minimal CUE module for the env-and-secrets-walk e2e fixture
// (e2e/env-and-secrets-walk.sh — the Phase-2.12 env value-reference chain
// plus the CLI half of day-2 ledger entries D6 + D7). A standalone module
// with no imports so the fixture is self-contained and needs no vendored
// schema — mirrors the per-env-app / gitops-app fixture convention. The
// argocd-cue-cmp sidecar injects the schema AND the generated `claim`
// binding into this module at render time.

module: "env-and-secrets-app.e2e.apprafter.io"

language: {
	version: "v0.10.0"
}
