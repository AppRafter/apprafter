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

// 0.1.1 — license-only re-release per ADR 0032. SPDX header
// migrated FSL-1.1-MIT → FSL-1.1-Apache-2.0 across the chart
// source. Identical component versions, identical rendered
// Argo CD Applications, identical Helm values — the only
// byte-level difference vs the 0.1.0 OCI artifact is the
// SPDX comment line in every CUE source file, which doesn't
// reach the rendered chart in any operator-visible form.
compatibility: "0.1.1": {
	change:          "safe"
	operatorVersion: "v0.1.91"
	notes: """
		License-only re-release. SPDX header migration from
		FSL-1.1-MIT to FSL-1.1-Apache-2.0 across
		`platform-stack/cue/*.cue` and `Chart.yaml.tmpl` per
		ADR 0032. No behavioural delta — same Cilium 1.16.5,
		cert-manager v1.16.2, Argo CD 7.7.7, apprafter-operator
		+ admission-webhook v0.1.91; same Argo CD Application
		set per tier; same default values.

		Operators on 0.1.0 may upgrade by changing
		`spec.source.targetRevision: "0.1.1"` on their
		platform Application. No manifest changes required
		downstream.
		"""
	references: [
		"docs/adr/0032-license-fsl-1-1-apache-2-0.md",
	]
}

// 0.1.2 — wires the cue-cmp sidecar (ADR 0029) into
// argocd-repo-server's `extraContainers`. Pulls the
// `ghcr.io/<owner>/argocd-cue-cmp:v0.1.0` image which has
// its own publish track (`argocd-cue-cmp/v*` tag series,
// `.github/workflows/argocd-cue-cmp-publish.yml`).
//
// User apps that contain `apprafter*.cue` are now renderable
// by Argo CD directly — the sidecar runs `cue export ./...
// --out yaml` at sync time. User apps that stick with raw
// YAML continue to work unchanged; the CMP `discover.find.glob`
// rule skips repositories without `apprafter*.cue`.
//
// **Known issue (fixed in 0.1.3):** this version inherits
// the upstream `argo-cd` 7.7.7 default `redis-ha.enabled:
// true`, which on single-node clusters causes the Argo CD
// install to time out at the pre-install hook (3 redis pods
// can't schedule across 3 nodes when there's only one). 0.1.3
// disables redis-ha in tier-1 defaults; bump to 0.1.3 if
// installing on tier-1.
compatibility: "0.1.2": {
	change:          "safe"
	operatorVersion: "v0.1.91"
	notes: """
		Wires the argocd-cue-cmp sidecar (ADR 0029) into
		`argocd-repo-server.extraContainers`. The sidecar
		image is pinned to v0.1.0 of the
		`argocd-cue-cmp/v*` track. Apprafter-operator and
		admission-webhook are unchanged.

		Upgrade impact: a single argocd-repo-server pod
		restart adds the cue-cmp sidecar. The sidecar adds
		~50 MiB to the pod's memory footprint per ADR
		0029's estimate. CMP activates only when a user
		repository contains `apprafter*.cue`; raw-YAML user
		apps are unaffected.

		Operators on 0.1.0 or 0.1.1 may upgrade by
		changing `spec.source.targetRevision: "0.1.2"` on
		their platform Application.
		"""
	references: [
		"docs/adr/0029-cue-cmp.md",
		"argocd-cue-cmp/README.md",
	]
}

// 0.1.3 — tier-1 single-node hotfix on top of 0.1.2. Disables
// `redis-ha.enabled` (defaults `true` upstream, schedules 3
// anti-affinity-bound pods that can't coexist on one node),
// flips `notifications.enabled: false` (saves the deployment
// on tier-1 cpx22 RAM budget), and pins
// `server.service.type: ClusterIP` until the chart's own
// Gateway/HTTPRoute exposure lands. No new components, no
// version pins changed — pure tier-1 default refinement
// matching the v0.1.x in-tree baseline.
//
// **Known issue (fixed in 0.1.4):** Argo CD generates a
// malformed `helm pull --repo oci://ghcr.io/apprafter <chart>`
// for `repoURL: oci://ghcr.io/apprafter` and the root
// Application reports `ComparisonError: object required`. The
// fix in 0.1.4 registers the repo via
// `configs.repositories.apprafter` (bare URL + `enableOCI:
// "true"`) and drops the `oci://` scheme prefix in all chart
// `repoURL` fields. Bump to 0.1.4 if you hit `object required`.
compatibility: "0.1.3": {
	change:          "safe"
	operatorVersion: "v0.1.91"
	notes: """
		Tier-1 single-node hotfix. The chart at 0.1.2 inherited
		the upstream `argo-cd` 7.7.7 default
		`redis-ha.enabled: true`, which schedules 3 redis-ha
		pods with `requiredDuringSchedulingIgnoredDuringExecution`
		`podAntiAffinity`. On single-node k3s those pods never
		become Ready, the chart's pre-install hook waits on
		them, and `helm install` times out with
		`failed pre-install: timed out waiting for the
		condition`.

		0.1.3 sets `redis-ha.enabled: false` in the tier-1
		default values for `argocd` (matches the v0.1.x
		in-tree imperative baseline). Also flips
		`notifications.enabled: false` and pins
		`server.service.type: ClusterIP` for consistency with
		the CLI's loader values.

		Operators on 0.1.0–0.1.2 who hit the timeout: upgrade
		to 0.1.3 (or to the matching CLI v0.1.98+ which pins
		this chart version). The same `redis-ha: false`
		override could also be added to the operator's
		platform Application `helm.valuesObject` as a one-off
		workaround.
		"""
	references: [
		"docs/changelog/UNRELEASED.md#v0198",
	]
}

// 0.1.4 — OCI repo registration hotfix. Argo CD's
// `argocd-repo-server` does not infer the OCI Helm protocol
// from a `oci://...` `repoURL` scheme — it shells out to
// `helm pull --repo <url> <chart>` which is malformed for
// OCI. The fix:
//
//   1. Register `ghcr.io/apprafter` as a Helm OCI repository
//      via `configs.repositories.apprafter` (bare URL +
//      `enableOCI: "true"`) — added to `component_argocd.cue`
//      and mirrored in `cli-providers::k8s::argocd_loader_values_yaml`
//      so the chart's self-reconcile keeps the registration
//      alive when it adopts the loader Argo CD release.
//   2. All chart `repoURL` fields pointing at `ghcr.io/apprafter`
//      drop the `oci://` prefix (`component_apprafter-operator.cue`,
//      `component_admission-webhook.cue`).
//   3. CLI constant `APPRAFTER_PLATFORM_STACK_DEFAULT_REPO`
//      drops the prefix correspondingly; `helm push` workflows
//      prepend `oci://` independently when invoking
//      `helm push`.
//   4. `cluster-bootstrap` now waits for `Sync=Synced` before
//      `Health=Healthy` — a freshly-created root Application
//      reports `Healthy` trivially (zero children) while
//      `Sync=Unknown` on chart-pull failure, which was
//      catching the 0.1.3 failure as a false-positive.
compatibility: "0.1.4": {
	change:          "safe"
	operatorVersion: "v0.1.91"
	notes: """
		OCI repo registration hotfix. The chart at 0.1.3 carried
		`repoURL: oci://ghcr.io/apprafter` on
		`component_apprafter-operator` and
		`component_admission-webhook`, plus the root Application
		(rendered by the CLI loader) carried the same. Argo CD
		does not auto-detect OCI from the URL scheme — it runs
		`helm pull --repo oci://ghcr.io/apprafter <chart>` which
		`helm` rejects with `object required`. The Application
		reports `ComparisonError: object required` and
		`Sync=Unknown / Health=Healthy` (trivially Healthy
		because no children rendered).

		0.1.4 registers `ghcr.io/apprafter` via the Argo CD
		Helm repositories config (bare URL + `enableOCI:
		"true"`) and drops `oci://` from every chart `repoURL`.
		Argo CD's `argocd-repo-server` then runs the correct
		`helm pull oci://ghcr.io/apprafter/<chart>` form.

		Operators on 0.1.3 who hit `object required`: upgrade
		to 0.1.4 (or to the matching CLI v0.1.100+ which pins
		this chart version). Manual recovery is a one-time
		`kubectl apply` of a Secret(label=repository) +
		patching the root Application's `repoURL` to
		`ghcr.io/apprafter`; the chart's self-reconcile then
		owns it. CLI v0.1.100's bootstrap-all does all this on
		a fresh cluster.
		"""
	references: [
		"docs/changelog/UNRELEASED.md#v01100",
	]
}
