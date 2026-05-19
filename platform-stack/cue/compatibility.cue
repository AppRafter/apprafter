// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// Change classification per published version. Operators
// upgrading platform-stack consult the entry for their target
// version to understand what's safe to upgrade automatically
// and what requires manual intervention.
//
// The categories are:
//
//   - `safe`     — version may be applied via Argo CD automated
//                  sync; no manual step. Default for additive
//                  component changes and intra-component patch
//                  bumps that don't reshape the chart.
//   - `caution`  — operator should pre-read this entry's
//                  `notes`. Examples: a component bumping a
//                  major CNI version, a default value flipping
//                  (e.g. Hubble enabled-by-default in a tier
//                  overlay).
//   - `breaking` — chart values shape changed or a component
//                  was removed. Operator must update their
//                  `Infrastructure.cue` overlay before
//                  upgrading.
#ChangeClass: "safe" | "caution" | "breaking"

// #VersionRecord is one row in the compatibility table.
#VersionRecord: {
	// Platform-stack version this record describes.
	version: #Version

	// Classification of the change vs. the previous version.
	change: #ChangeClass

	// Pinned operator + admission-webhook version this stack
	// installs. Listed here so a glance at the compatibility
	// file shows the operator-stack pairing.
	operatorVersion: string

	// Free-form notes shown to operators on upgrade. Keep
	// each entry under ~10 lines; longer migration stories
	// belong in a dedicated ADR.
	notes: string

	// References to ADRs / changelog entries that justify
	// this version's design decisions.
	references: [...string]
}

// `compatibility` is the indexed history. Keys are versions
// for traversal stability. CI tags the OCI artifact with the
// same key.
compatibility: [VERSION=string]: #VersionRecord & {
	version: VERSION
}

// The current chart version MUST have a matching compatibility
// record — otherwise `PlatformController` (Phase 2+) has no
// classification to gate automated upgrades on, and the publish
// workflow's pre-flight script would fail at runtime. Surfacing
// the invariant in CUE turns a missing-entry bump into a `cue
// vet -c` failure at edit time, not a CI failure on push.
//
// The pattern `(currentVersion): #VersionRecord` uses CUE's
// dynamic-field syntax — the key is computed from
// `currentVersion` declared in `platform.cue`. The unification
// passes when the explicit `compatibility: "0.1.0": { … }`
// entry below provides all required #VersionRecord fields;
// fails with an "incomplete value" diagnostic that points at
// the offending field if the entry is missing or partial.
compatibility: (currentVersion): #VersionRecord

// Initial entry — platform-stack 0.1.0 is the first published
// version. Minor tracks the AppRafter monorepo phase (we're
// in Phase 1.5; chart MINOR bumps to 0.2.0 alongside the
// `v0.2.0-services` milestone when Phase 2 services land).
// Chart patch versions are independent of the monorepo
// patch stream (`v0.1.x`); the two share only MINOR/MAJOR
// semantics.
compatibility: "0.1.0": {
	change:          "safe"
	operatorVersion: "v0.1.91"
	notes: """
		First published platform-stack version. Bundles the
		v0.1.x cluster-bootstrap components (Cilium 1.16.5,
		cert-manager v1.16.2, Argo CD 7.7.7, apprafter-operator
		+ admission-webhook v0.1.91) without behavioural change
		— operators upgrading from a v0.1.x in-tree bootstrap
		see the same component versions, just sourced via
		Argo CD instead of direct `helm upgrade --install`.

		argocd-cue-cmp is declared but disabled by default; it
		activates in a follow-up version once the sidecar
		wiring (plan.md 1.69) lands.
		"""
	references: [
		"docs/adr/0028-platform-stack-distribution.md",
		"docs/adr/0026-platformstack-crd.md",
		"docs/changelog/UNRELEASED.md#v0192",
	]
}
