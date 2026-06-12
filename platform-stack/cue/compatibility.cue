// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// Change classification per published version. Operators
// upgrading platform-stack consult the entry for their target
// version to understand what's safe to upgrade automatically
// and what requires manual intervention.
//
// The categories mirror the Rust `ChangeClass` enum
// (`operator-controllers/platform-stack/src/compatibility.rs`)
// and the MigrationPlan CRD's `spec.risks.classification` enum
// (`schemas/v1alpha1/migrationplan.cue`). All three must
// agree on the same vocabulary, otherwise the operator's
// fail-closed default (`Breaking`) fires on legitimate
// classifications.
//
// The categories are:
//
//   - `safe` — version may be applied via Argo CD automated
//     sync; no manual step. Default for additive component
//     changes and intra-component patch bumps that don't
//     reshape the chart.
//   - `requires-restart` — chart change requires component
//     pod restarts but no data migration or backups. Operator
//     approval gates the bump.
//   - `data-migration` — chart change includes a destructive
//     data migration (e.g. database schema change, storage
//     class flip). Operator approval gates the bump; a
//     MigrationPlan describes the migration steps.
//   - `breaking` — chart values shape changed or a component
//     was removed. Operator must update their
//     `Infrastructure.cue` overlay before upgrading.
#ChangeClass: "safe" | "requires-restart" | "data-migration" | "breaking"

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

	// Yank marker. Defaults to false. Set to true via a
	// `compatibility.cue`-only PR (no `currentVersion` bump)
	// to soft-recall a published version. PlatformController
	// then:
	//
	//   * skips this version in `availableVersion`
	//     channel-latest resolution — fresh clusters never
	//     resolve to a yanked version.
	//   * raises `YankedVersion=True` (informational, not
	//     `Ready=False`) on clusters whose
	//     `status.currentVersion` matches this entry.
	//   * does NOT force-upgrade existing clusters — yanked
	//     is metadata, never a policy override.
	//
	// Analogous to `cargo yank` / `npm deprecate` / PyPI
	// yank. The OCI tag itself stays published (tags are
	// immutable in OCI distribution); yank is purely a
	// chart-author hint surfaced via the compatibility
	// metadata.
	yanked: bool | *false

	// Mandatory when `yanked: true`. Free-form one-line
	// reason shown verbatim in the `YankedVersion` condition
	// message, in `apprafter platform status`, and in the
	// Backstage UI banner.
	//
	// The CUE constraint here is "optional string", because
	// CUE cannot express "required iff yanked=true" with a
	// single field constraint. The publish workflow's CI
	// guard enforces the conditional invariant by failing
	// any `yanked: true` entry whose `yankedReason` is
	// missing or empty (see
	// `.github/workflows/platform-stack-publish.yml`).
	yankedReason?: string
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
// **Known issue (fixed in 0.1.4 / 0.1.5):** Argo CD generates a
// malformed `helm pull --repo oci://ghcr.io/apprafter <chart>`
// for `repoURL: oci://ghcr.io/apprafter` and the root
// Application reports `ComparisonError: object required`. The
// fix in 0.1.4 registers the repo via
// `configs.repositories.apprafter` (bare URL + `enableOCI:
// "true"`) and drops the `oci://` scheme prefix in all chart
// `repoURL` fields. 0.1.5 layers further child-Application
// fixes on top — see 0.1.5 notes. Bump straight to 0.1.5 if
// installing fresh.
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

		Known issue carried into 0.1.4: child Applications
		(`cilium`, `argocd`, `apprafter-operator`,
		`admission-webhook`, `network-policies`) reported
		`Sync=Unknown` after the OCI fix. Three independent
		causes — see 0.1.5 notes. Bump straight to 0.1.5 for
		a clean fresh install.
		"""
	references: [
		"docs/changelog/UNRELEASED.md#v01100",
	]
}

// 0.1.5 — child-Application syncability hotfix on top of
// 0.1.4. Three independent walk-found defects:
//
//   1. `apprafter-operator` and `apprafter-admission-webhook`
//      helm charts were never published to OCI — only the
//      container images were. Argo CD failed `helm pull
//      oci://ghcr.io/apprafter/apprafter-operator` with
//      `not found`. Fixed by adding a `helm-charts` job to
//      `release-operator.yml` that packages + pushes both
//      charts via `helm push oci://ghcr.io/<owner>`. The
//      existing `apprafter-operator` chart is bumped to
//      `v0.1.91` to match the platform-stack pin; a new
//      `apprafter-admission-webhook` chart is created from
//      scratch (templates derived from the v0.1.x in-tree
//      `cli-providers::k8s::admission_webhook_yaml` renderer
//      — Namespace dropped because Argo CD creates it via
//      `CreateNamespace=true`; Certificate, Service,
//      Deployment, ValidatingWebhookConfiguration templated).
//   2. `cilium` + `argocd` child Applications failed
//      structured-merge diff with `terminatingReplicas: field
//      not declared in schema` — k3s v1.35 surfaces the
//      Kubernetes 1.31+ field on Deployment/DaemonSet/StatefulSet
//      `.status`, Argo CD 2.13.1 doesn't yet know it. Fixed
//      by adding `ignoreDifferences` blocks in the affected
//      components; `#Component` schema in `platform.cue`
//      grows an optional `ignoreDifferences` field, and
//      `render_tool.cue` renders it into the Argo CD
//      Application spec.
//   3. `network-policies` Application failed with `app path
//      does not exist` — `component_network-policies.cue`
//      pointed at `manifests/tier-1/network-policies/`
//      which had never been created when the v0.1.97
//      imperative-to-GitOps rewrite migrated inline manifests
//      out of the CLI. Fixed by creating
//      `manifests/tier-1/network-policies/default-deny.yaml`
//      (content lifted from
//      `cli-providers::k8s::network_policy::default_deny_network_policy_yaml`).
compatibility: "0.1.5": {
	change:          "safe"
	operatorVersion: "v0.1.91"
	notes: """
		Child-Application syncability hotfix. Without 0.1.5 a
		fresh install on Kubernetes 1.31+ ends with the root
		`platform` Application Synced but five of six children
		stuck on `ComparisonError` or `Unknown`:

		* `apprafter-operator` + `admission-webhook` —
		  Helm chart never published to OCI; only the
		  container image existed. 0.1.5 adds a `helm-charts`
		  job to `release-operator.yml` that packages + pushes
		  both charts; the existing `apprafter-operator` chart
		  is bumped to `v0.1.91` and a new
		  `apprafter-admission-webhook` chart is created.
		* `cilium` + `argocd` —
		  `Deployment.status.terminatingReplicas` (Kubernetes
		  1.31+, surfaced by k3s v1.35) wasn't in the Argo CD
		  2.13.1 schema. 0.1.5 adds `ignoreDifferences` blocks
		  to both components covering Deployment / DaemonSet /
		  StatefulSet `/status/terminatingReplicas`.
		* `network-policies` —
		  `manifests/tier-1/network-policies/` didn't exist
		  in the monorepo. 0.1.5 creates
		  `default-deny.yaml` mirroring the v0.1.x in-tree
		  `default_deny_network_policy_yaml` baseline.

		Schema impact: `#Component` grows an optional
		`ignoreDifferences: [...{group, kind, jsonPointers?,
		jqPathExpressions?}]` field. Empty list (the
		default) is a no-op; existing components are
		unaffected.

		Operators on 0.1.4 stuck with `ComparisonError`:
		upgrade to 0.1.5 (or to the matching CLI v0.1.101+
		which pins this chart version).

		Known issues carried into 0.1.5 (all fixed in 0.1.6):

		* `admission-webhook` Deployment failed validation
		  with `selector does not match template labels` —
		  the chart's `_helpers.tpl` defined `labels` without
		  including `selectorLabels`.
		* `apprafter-operator` + `admission-webhook` still
		  reported `terminatingReplicas: field not declared
		  in schema` — `ignoreDifferences` was only added to
		  cilium + argocd in 0.1.5.
		* `network-policies` failed `app path does not exist`
		  because the git pin (`version: v0.1.91`) predates
		  the directory `manifests/tier-1/network-policies/`,
		  which was created only in v0.1.101.
		* `admission-webhook` `Certificate` resource hit
		  `no endpoints available for service
		  cert-manager-webhook` — chart did not order
		  cert-manager before dependents.
		"""
	references: [
		"docs/changelog/UNRELEASED.md#v01101",
	]
}

// 0.1.6 — chart-template hygiene + sync ordering hotfix on
// top of 0.1.5. Five independent defects all surfaced on the
// fresh walk against 0.1.5:
//
//   1. `apprafter-admission-webhook` chart's `_helpers.tpl`
//      defined `labels` without including `selectorLabels`,
//      so `Deployment.spec.template.metadata.labels` did not
//      contain `app.kubernetes.io/name` / `app.kubernetes.io/instance`
//      that the selector matched on. Kubernetes API rejected
//      the Deployment with `selector does not match template
//      labels`. Fixed by mirroring the operator chart's
//      `labels` definition that already pulled
//      `selectorLabels` in via `include`.
//   2. `component_apprafter-operator.cue` +
//      `component_admission-webhook.cue` lacked
//      `ignoreDifferences` for
//      `Deployment.status.terminatingReplicas` (added to
//      cilium + argocd in 0.1.5, missed for these two).
//      Same Kubernetes 1.31+ field skew, same fix.
//   3. `component_network-policies.cue` pinned `version:
//      v0.1.91` — the operator chart's AppVersion anchor —
//      but that monorepo tag predates the
//      `manifests/tier-1/network-policies/` directory
//      (created in v0.1.101 for chart 0.1.5). Bumped to
//      `v0.1.102`, the tag that ships 0.1.6.
//   4. `admission-webhook` `Certificate` resource failed
//      with `no endpoints available for service
//      cert-manager-webhook` because Argo CD applied it
//      before cert-manager's webhook had endpoints. Fixed
//      by adding `argocd.argoproj.io/sync-wave` ordering:
//      cilium = -20, argocd = -15, cert-manager = -10,
//      everyone else = 0 (default). `#Component` schema in
//      `platform.cue` gains an optional `syncWave: int |
//      *0` field; `render_tool.cue` emits it as a metadata
//      annotation on the rendered Application.
//   5. Side benefit of (4): cilium reconciles before
//      cert-manager / operator / webhook even when their
//      Argo CD adopt happens first, eliminating a class of
//      `not-ready taint` races on slow image-pull paths.
//
// No new components; no API breaks for existing
// `_components` entries that don't set `syncWave` /
// `ignoreDifferences` (both default to safe values).
compatibility: "0.1.6": {
	change:          "safe"
	operatorVersion: "v0.1.91"
	notes: """
		Chart-template hygiene + sync ordering hotfix.

		* `apprafter-admission-webhook` chart's `_helpers.tpl`
		  now includes `selectorLabels` inside `labels` (same
		  shape as the operator chart). Fixes the
		  `selector does not match template labels` failure.
		* `component_apprafter-operator` +
		  `component_admission-webhook` gain
		  `ignoreDifferences` for
		  `Deployment.status.terminatingReplicas` (Kubernetes
		  1.31+ field, k3s v1.35).
		* `component_network-policies` git pin bumped from
		  `v0.1.91` to `v0.1.102` (the monorepo tag that
		  ships chart 0.1.6 and the
		  `manifests/tier-1/network-policies/` directory).
		* `#Component` grows an optional `syncWave: int | *0`
		  field; cilium = -20, argocd = -15, cert-manager =
		  -10. The admission-webhook `Certificate` no longer
		  applies before cert-manager's webhook has
		  endpoints.

		Schema: existing `_components` entries that don't set
		`syncWave` or `ignoreDifferences` continue to work
		unchanged (both default to safe values: 0 and `[]`
		respectively).

		Operators on 0.1.5 stuck with `selector does not
		match template labels` or `app path does not exist`:
		upgrade to 0.1.6 (or to the matching CLI v0.1.102+
		which pins this chart version).

		Known issues carried into 0.1.6 (all fixed in 0.1.7):

		* `cilium-operator` CrashLoopBackOff with
		  `KUBERNETES_SERVICE_HOST=auto` env var — chart's
		  `component_cilium.cue` values diverged from the
		  CLI loader's (`k8sServiceHost: "auto"` vs
		  `"127.0.0.1"`, missing `ipv4`/`ipv6` flags). Argo
		  CD applied chart-overlay manifests on top of the
		  loader Deployment, breaking Cilium agent.
		* `cert-manager` reported `terminatingReplicas:
		  field not declared in schema` — same field skew as
		  the other components but `component_cert-manager.cue`
		  lacked `ignoreDifferences` in 0.1.5 / 0.1.6.
		"""
	references: [
		"docs/changelog/UNRELEASED.md#v01102",
	]
}

// 0.1.7 — Cilium chart-overlay alignment hotfix on top of
// 0.1.6. Walk #6 surfaced that Argo CD applies
// `component_cilium.cue` values as a plain-manifest overlay
// on top of the loader-installed Cilium release, NOT as a
// helm upgrade of the existing release. The two value sets
// MUST therefore be byte-identical for tier-1 — any drift
// reconfigures the live Cilium operator + agent and the
// operator crashes with `unable to load in-cluster
// configuration, KUBERNETES_SERVICE_HOST and KUBERNETES_SERVICE_PORT
// must be defined` (because the chart was setting
// `k8sServiceHost: "auto"` which gets emitted as a literal
// env-var string).
//
//   1. `component_cilium.cue` values rewrite to mirror
//      `cli-providers::k8s::cilium_values_yaml` byte-by-byte:
//      `kubeProxyReplacement: true` (bool, not string),
//      `k8sServiceHost: "127.0.0.1"` (not `"auto"`),
//      `k8sServicePort: 6443` (added, was missing),
//      `ipv4.enabled: true` + `ipv6.enabled: true` (added,
//      were missing — drove the ConfigMap's
//      `enable-ipv6: "false"` divergence).
//   2. `component_cert-manager.cue` gains `ignoreDifferences`
//      for `Deployment.status.terminatingReplicas` (oversight
//      in 0.1.5 / 0.1.6).
//
// Note for B.1.71: the duplication between
// `cli-providers::k8s::cilium_values_yaml` and
// `component_cilium.cue` is exactly what B.1.71's
// "migration of values from CLI to chart" eliminates. Until
// then, every edit to either side MUST be paired (commented
// banner in `component_cilium.cue` reminds the next reader).
compatibility: "0.1.7": {
	change:          "safe"
	operatorVersion: "v0.1.91"
	notes: """
		Cilium chart-overlay alignment hotfix.

		* `component_cilium.cue` now mirrors the CLI loader's
		  `cilium_values_yaml` byte-by-byte. The chart's
		  former `k8sServiceHost: "auto"` was reaching live
		  pods as literal env-var
		  `KUBERNETES_SERVICE_HOST=auto`, crashing
		  cilium-operator with `unable to load in-cluster
		  configuration`. Fixed by setting `k8sServiceHost:
		  "127.0.0.1"`, `k8sServicePort: 6443`, restoring
		  `ipv4.enabled: true` + `ipv6.enabled: true`, and
		  flipping `kubeProxyReplacement` from string back to
		  bool.
		* `component_cert-manager.cue` gains
		  `ignoreDifferences` for
		  `Deployment.status.terminatingReplicas`.

		Operators on 0.1.6 stuck with cilium-operator
		CrashLoopBackOff (and the cascade of
		`cilium-cni: unable to connect to Cilium agent`
		failures on every other pod schedule): upgrade to
		0.1.7 (or to the matching CLI v0.1.103+).

		Known issue carried into 0.1.7 (fixed in 0.1.8):

		* On a fresh cluster the root `platform` Application
		  fails with `Application referencing project default
		  which does not exist`. Chart 7.7.7 does not
		  auto-create the `default` AppProject and Argo CD
		  2.13.1's server does not recreate it on startup.
		  Previous walks may have hit this lazily; v0.1.103
		  ran into it deterministically.
		"""
	references: [
		"docs/changelog/UNRELEASED.md#v01103",
	]
}

// 0.1.8 — default AppProject hotfix on top of 0.1.7. Argo CD
// chart 7.7.7 ships `configs.projects: {}` by default and
// the upstream argocd-server 2.13.1 does not re-create the
// `default` AppProject on startup, so every Application
// rendered with `spec.project: default` (incl. the root
// `platform` Application the CLI loader applies) fails with
// `Application referencing project default which does not
// exist`. Walk #7 surfaced this — earlier walks hit it
// lazily and Argo CD's reconciler appeared to handle it.
//
// Fix: `component_argocd.cue` gains a `configs.projects.default`
// values block mirroring Argo CD's historical implicit
// default (`sourceRepos: ["*"]`, `destinations: [{namespace: "*",
// server: "*"}]`, all-kinds whitelists). The same block is
// added to `cli-providers::k8s::argocd_loader_values_yaml`
// so the loader install creates the AppProject before the
// root Application apply, and the chart's self-reconcile
// keeps it alive after adoption.
//
// Future: an admin who wants to restrict the default project
// (per-tenant `sourceRepos`, namespace lockdown, RBAC) edits
// the same block in their fork's `component_argocd.cue`
// overlay.
compatibility: "0.1.8": {
	change:          "safe"
	operatorVersion: "v0.1.91"
	notes: """
		Default AppProject hotfix.

		Chart 7.7.7 ships `configs.projects: {}`; Argo CD
		2.13.1's server does not auto-create `default` on
		startup. Every Application with `spec.project:
		default` (incl. the root `platform` Application the
		CLI loader applies) fails with `Application
		referencing project default which does not exist`.

		0.1.8 adds an unrestricted `default` AppProject to
		`configs.projects` in both the chart's
		`component_argocd.cue` values overlay and the CLI
		loader's `argocd_loader_values_yaml`. The loader
		creates it on initial `helm install`; the chart's
		self-reconcile keeps it alive on adopt. Operators
		who want to restrict the default project edit the
		overlay in their fork.

		Operators on 0.1.7 stuck with `Application
		referencing project default which does not exist`:
		upgrade to 0.1.8 (or to the matching CLI v0.1.104+).
		Manual one-time recovery: `kubectl apply -f -` an
		AppProject named `default` in namespace `argocd`.

		Known issues carried into 0.1.8 (fixed in 0.1.9):

		* `apprafter-operator` pods failed
		  `CreateContainerError` with `failed to generate
		  spec: no command specified`. The
		  `ghcr.io/apprafter/apprafter-operator:v0.1.91`
		  container image was broken — `kubectl run` against
		  the image showed `stat /apprafter-operator: no such
		  file or directory`. The binary never made it into
		  the image when v0.1.91 was tagged.
		* `admission-webhook` pods stuck with `MountVolume.SetUp
		  failed: secret "admission-webhook-tls" not found`.
		  cert-manager Certificate was issuing but the
		  referenced `apprafter-selfsigned` ClusterIssuer
		  didn't exist on the cluster — the chart never
		  shipped it.
		"""
	references: [
		"docs/changelog/UNRELEASED.md#v01104",
	]
}

// 0.1.9 — operator + webhook chart hardening on top of
// 0.1.8. Walk #8 surfaced two defects tied to chart-side
// blind spots:
//
//   1. `ghcr.io/apprafter/apprafter-operator:v0.1.91`
//      container image was broken — `kubectl run` showed
//      `stat /apprafter-operator: no such file or directory`.
//      The binary was missing from the image manifest at
//      that tag (likely a stale or partially-built artefact
//      from when v0.1.91 was first published). Fix: bump
//      both `operator/charts/apprafter-operator` and
//      `operator/charts/apprafter-admission-webhook` to
//      chart version `v0.1.92` with `appVersion: v0.1.105`
//      (the fresh monorepo tag whose
//      release-operator.yml workflow rebuilds the images
//      with the cargo-chef Dockerfile that
//      reliably produces working binaries). Both chart
//      templates also gain an explicit
//      `command: [...]` field that does NOT rely on the
//      image manifest's ENTRYPOINT field, as defence in
//      depth against future image-build edge cases.
//   2. The `apprafter-selfsigned` ClusterIssuer that
//      `apprafter-admission-webhook`'s `Certificate`
//      template references didn't exist on a fresh
//      cluster — cert-manager never received an Issuer
//      definition and the Certificate stayed in `Issuing`
//      forever. Fix: ship a `clusterissuer.yaml` template
//      in the webhook chart that creates the ClusterIssuer
//      alongside the Certificate it serves. The
//      `selfSigned: {}` spec matches Argo CD's historical
//      v0.1.x baseline.
//
// Also: `cli-providers::k8s::RELEASED_OPERATOR_VERSION`
// bumped from `v0.1.64` to `v0.1.105` (was three months
// stale per CLAUDE.md sync rule).
compatibility: "0.1.9": {
	change:          "safe"
	operatorVersion: "v0.1.105"
	notes: """
		Operator + admission-webhook chart hardening.

		* Operator + webhook charts bumped to v0.1.92 with
		  appVersion v0.1.105 — fresh monorepo tag whose
		  workflow rebuilds the container images with the
		  cargo-chef Dockerfile that reliably produces a
		  working binary at /apprafter-operator and
		  /admission-webhook. The v0.1.91 image was broken
		  (binary missing).
		* Both chart templates gain explicit
		  `command: [...]` field — defence in depth against
		  future image-manifest edge cases.
		* New `clusterissuer.yaml` template in the webhook
		  chart ships the `apprafter-selfsigned` ClusterIssuer
		  that this chart's Certificate references. Without
		  it cert-manager could not issue the TLS Secret and
		  the webhook pod stayed `MountVolume.SetUp failed`.

		Operators on 0.1.8 stuck with `no command specified`
		(operator pods) or `MountVolume.SetUp failed`
		(webhook pods): upgrade to 0.1.9 (or to the matching
		CLI v0.1.105+). No manual cluster surgery needed —
		the chart's self-reconcile applies both fixes.

		Known issue carried into 0.1.9 (fixed in 0.1.10):

		* `admission-webhook` v0.1.105 image panics at TLS
		  init with `Could not automatically determine the
		  process-level CryptoProvider`. The webhook's
		  `main.rs` was never updated for the rustls 0.23
		  API (the operator's v0.1.61 fix only touched its
		  own crate). Never surfaced before because the
		  v0.1.91 image was broken and the webhook code
		  never executed. 0.1.10 mirrors the operator's
		  fix into the webhook crate.
		"""
	references: [
		"docs/changelog/UNRELEASED.md#v01105",
	]
}

// 0.1.10 — webhook rustls CryptoProvider fix on top of
// 0.1.9. The walk #8 v0.1.105 image finally ran the
// webhook code (after months of broken v0.1.91 image
// shielding the bug) and surfaced a rustls 0.23 panic
// the operator binary fixed in v0.1.61 but the webhook
// binary never got. Fix: mirror the operator's
// `install_rustls_crypto_provider()` helper into the
// `admission-webhook` crate and call it in `main` before
// any rustls-using API. New direct dependency on
// `rustls = { version = "0.23", features = ["aws-lc-rs"] }`
// in `operator/admission-webhook/Cargo.toml`.
//
// Chart-side: operator + webhook charts both bump to
// `version: v0.1.93` with `appVersion: "v0.1.106"`
// (lockstep with the new monorepo tag). Operator chart
// bumps even though only the webhook binary changed —
// keeps the two appVersions in sync so a future bug in
// either binary lands at the same monorepo tag.
compatibility: "0.1.10": {
	change:          "safe"
	operatorVersion: "v0.1.106"
	notes: """
		Webhook rustls CryptoProvider fix.

		The v0.1.105 webhook image panicked at TLS init:
		`Could not automatically determine the process-level
		CryptoProvider from Rustls crate features`. The
		operator binary already had the fix since v0.1.61
		but the webhook crate didn't — never surfaced
		earlier because the v0.1.91 webhook image was
		broken and the binary never executed.

		0.1.10's webhook image (v0.1.106) calls
		`install_rustls_crypto_provider()` in `main` before
		any rustls-using API. Both operator + webhook charts
		bump to v0.1.93 / appVersion v0.1.106 in lockstep.

		Operators on 0.1.9 with `admission-webhook` panic at
		startup: upgrade to 0.1.10 (or to the matching CLI
		v0.1.106+).

		Known issue carried into 0.1.10 (fixed in 0.1.11):

		* `argocd` Application reports `Synced/Degraded`
		  because the new `argocd-repo-server` pod (with
		  cue-cmp sidecar) is stuck in `Init:0/1`. kubelet
		  events show `MountVolume.SetUp failed for volume
		  "cue-cmp-config": configmap
		  "cue-cmp-plugin-config" not found`. The
		  `component_argocd.cue` chart references the
		  ConfigMap but never created it. 0.1.11 ships it
		  via `extraObjects`.
		"""
	references: [
		"docs/changelog/UNRELEASED.md#v01106",
	]
}

// 0.1.11 — cue-cmp ConfigMap ship hotfix on top of 0.1.10.
// The `component_argocd.cue` overlay added a cue-cmp
// sidecar to `argocd-repo-server` in 0.1.2 with a
// volumeMount pointing at ConfigMap `cue-cmp-plugin-config`,
// but the ConfigMap was never declared anywhere — the
// repo-server Deployment couldn't start because kubelet
// failed to mount a non-existent ConfigMap. The bug was
// masked through walks #5-9 because earlier blockers
// (broken images, missing ClusterIssuer, rustls panic)
// halted reconciliation before the new repo-server pod
// got a chance to schedule.
//
// Fix: chart's `extraObjects` value now ships a ConfigMap
// `cue-cmp-plugin-config` with the verbatim
// `argocd-cue-cmp/plugin.yaml` content. ConfigMap lives in
// the same release as the Deployment that mounts it.
compatibility: "0.1.11": {
	change:          "safe"
	operatorVersion: "v0.1.106"
	notes: """
		cue-cmp ConfigMap ship hotfix.

		The chart's argocd component carried a `cue-cmp`
		sidecar with a volumeMount on ConfigMap
		`cue-cmp-plugin-config` since chart 0.1.2 (Track
		B.1.69), but the ConfigMap was never created. The
		repo-server pod failed `MountVolume.SetUp` and the
		Argo CD Application stuck on `Synced/Degraded`.

		0.1.11 ships the ConfigMap via the upstream chart's
		`extraObjects` value. Content is verbatim from
		`argocd-cue-cmp/plugin.yaml`; an edit there must be
		mirrored here until a `cue cmd` step in the chart
		renderer reads it directly.

		Operators on 0.1.10 with stuck `argocd-repo-server`
		pod (Init:0/1) and `Synced/Degraded` argocd
		Application: upgrade to 0.1.11 (or to the matching
		CLI v0.1.107+).

		Known issue carried into 0.1.11 (fixed in 0.1.12):

		* `argocd-repo-server` cue-cmp sidecar fails image
		  pull with `MANIFEST_UNKNOWN` for
		  `ghcr.io/apprafter/argocd-cue-cmp:v0.1.0`. The
		  v0.1.0 publish workflow tagged the image as
		  `:0.1.0` (no `v` prefix); chart pinned `:v0.1.0`.
		"""
	references: [
		"docs/changelog/UNRELEASED.md#v01107",
	]
}

// 0.1.12 — cue-cmp image tag form correction on top of
// 0.1.11. The `argocd-cue-cmp-publish.yml` workflow tagged
// the v0.1.0 image as `:0.1.0` (no `v` prefix) while the
// git tag form was `argocd-cue-cmp/v0.1.0` and every other
// AppRafter container image uses the `v<version>` form
// (operator + admission-webhook from
// `release-operator.yml`). Chart 0.1.2-0.1.11 pinned
// `:v0.1.0` and the cue-cmp sidecar failed `helm pull` with
// `MANIFEST_UNKNOWN`. Latent since chart 0.1.2 (Track
// B.1.69) but masked through walks #5-10 by upstream
// blockers in the same repo-server pod (broken
// ConfigMap mount in 0.1.10).
//
// Fix:
//   1. `argocd-cue-cmp-publish.yml` tag pattern changed from
//      `${IMAGE}:${VERSION}` to `${IMAGE}:v${VERSION}`. The
//      v0.1.0 image stays as a historical artefact; v0.1.1
//      onward uses the consistent `:v<version>` form.
//   2. `argocd-cue-cmp/VERSION` bumped `0.1.0` → `0.1.1` so
//      the workflow's `detect` job actually triggers a fresh
//      publish (would otherwise see `argocd-cue-cmp/v0.1.0`
//      git tag exists and skip).
//   3. `component_argocd-cue-cmp.cue` pin bumped `v0.1.0` →
//      `v0.1.1` to consume the v-prefixed image.
//
// Image source code didn't change — v0.1.1 is a re-publish
// of v0.1.0's source with the corrected tag form.
compatibility: "0.1.12": {
	change:          "safe"
	operatorVersion: "v0.1.106"
	notes: """
		cue-cmp image tag form correction.

		The `argocd-cue-cmp-publish.yml` workflow tagged the
		v0.1.0 image as `:0.1.0` (no `v` prefix); chart
		pinned `:v0.1.0`. `helm pull` failed with
		`MANIFEST_UNKNOWN` because the manifest at the
		v-prefixed tag never existed.

		0.1.12 ships chart pin `v0.1.1` consuming a
		freshly-published `ghcr.io/apprafter/argocd-cue-cmp:v0.1.1`
		image. The workflow now tags new images with the
		`v<version>` form matching the operator + webhook
		convention. v0.1.0 image stays as a historical
		artefact for traceability.

		Operators on 0.1.11 with `argocd-repo-server` pod
		stuck on `Init:0/1` and image-pull `MANIFEST_UNKNOWN`:
		upgrade to 0.1.12 (or to the matching CLI v0.1.108+).
		"""
	references: [
		"docs/changelog/UNRELEASED.md#v01108",
	]
}

// 0.1.13 — Track B.1.71 closure: chart values become the
// single source of truth. The chart's existing component
// values are unchanged; the CLI loader now reads them via
// `cli/cli-providers/build.rs` (cue export at compile time)
// instead of carrying a parallel copy. No rendered chart
// YAML changes; no operator / webhook image bump; no
// runtime behavioural change.
//
// Drift classes eliminated:
//   - Cilium chart-overlay drift (walk-fix #6, v0.1.103) —
//     `_components.cilium.values ≡ _loaderValues.cilium`
//     invariant + reference unification.
//   - Argo CD loader-subset drift (walk-fixes #1, #3, #5,
//     #7) — chart's `component_argocd.cue` derives
//     `values:` as `_loaderValues.argocd & { ...extras... }`.
//   - `RELEASED_PLATFORM_STACK_VERSION` manual lockstep
//     rule — now derived from chart's `currentVersion`.
compatibility: "0.1.13": {
	change:          "safe"
	operatorVersion: "v0.1.106"
	notes: """
		Track B.1.71 closure: chart-as-single-source-of-truth.

		The platform-stack chart's CUE source becomes the
		only place platform component values are defined.
		The CLI loader (`apprafter cluster-bootstrap`) used
		to carry hand-maintained copies of the same Cilium +
		Argo CD values in `cli-providers/src/k8s/*_yaml.rs`
		— 12 dead renderer files surfaced after the v0.1.97
		GitOps refactor and survived the 11-walk-fix
		cascade only because most weren't actually called.
		B.1.71 removes them entirely.

		Cilium + Argo CD loader values now derive from new
		`_loaderValues.{cilium,argocd}` fields in
		`platform-stack/cue/loader_values.cue` via
		`cli/cli-providers/build.rs` running `cue export -e
		_loaderValues.<comp> --out yaml` at compile time.
		The chart's `component_cilium.cue` and
		`component_argocd.cue` reference the same
		`_loaderValues` source — chart and loader can no
		longer diverge.

		`RELEASED_PLATFORM_STACK_VERSION` is also
		chart-derived now (extracted from `currentVersion`
		field). Bumping the chart's `currentVersion`
		automatically bumps the CLI's pin at next compile —
		no more CLAUDE.md "Bump in lockstep" hand-discipline.

		Rendered chart YAML is byte-equivalent to 0.1.12
		(verified by `cue export -e _components.argocd.values
		--out yaml` diff on either side of Task 5's refactor
		— empty). Operators on 0.1.12 can upgrade without
		any cluster-side action.
		"""
	references: [
		"docs/superpowers/plans/2026-05-20-track-b-1-71-chart-as-sot.md",
		"docs/changelog/UNRELEASED.md#v01109",
	]
}

// 0.1.14 — Track B.1.71b closure: remaining version drift
// classes from B.1.71's "deferred follow-ups" all closed.
// Cilium + Argo CD upstream chart versions now live in
// `_loaderValues.{cilium,argocd}.chartVersion` (CUE single
// source, build.rs-derived in CLI). Operator + admission-
// webhook container image tags now live in
// `operator/charts/<chart>/Chart.yaml#appVersion` (Helm
// chart standard, build.rs-derived in CLI; chart's
// `values.image.tag` field dropped so the chart template's
// `.Chart.AppVersion` fallback drives the deployed image).
// cue-cmp image version now lives in
// `argocd-cue-cmp/version.cue` (single SoT for chart +
// publish workflow + check workflow). No rendered chart
// YAML change vs 0.1.13.
compatibility: "0.1.14": {
	change:          "safe"
	operatorVersion: "v0.1.106"
	notes: """
		Track B.1.71b closure: closed the remaining 6 version-
		duplication classes that B.1.71's structural refactor
		had deferred. Single source of truth now established
		for:

		* Cilium + Argo CD upstream chart versions
		  (`_loaderValues.{cilium,argocd}.chartVersion` in
		  CUE; build.rs-derived in CLI).
		* Operator + admission-webhook container image tags
		  (operator chart's `Chart.yaml#appVersion`;
		  build.rs-derived in CLI; chart `values.image.tag`
		  dropped so the Helm template's `.Chart.AppVersion`
		  fallback wins).
		* cue-cmp image version
		  (`argocd-cue-cmp/version.cue`; CUE-imported by
		  chart, `cue export`-read by both publish and
		  check workflows).

		Drift between any of these places now fails at:
		* `cue vet` (chart-side invariants for
		  Cilium / Argo CD).
		* `build.rs` assertion (operator + webhook
		  Chart.yaml `appVersion` must agree).
		* CI workflow setup (cue-cmp version invocations
		  resolve from a single CUE file).

		Rendered chart YAML byte-equivalent to 0.1.13
		(verified by `cue export -e _components.<comp>`
		diff). Operators on 0.1.13 upgrade without any
		cluster-side action.
		"""
	references: [
		"docs/superpowers/plans/2026-05-20-track-b-1-71b-close-remaining-version-drift.md",
		"docs/changelog/UNRELEASED.md#v01110",
	]
}

// 0.1.15 — Track B.1.72 closure: PlatformStack CRD + Application
// CRD restoration + admission-webhook PlatformStack validator +
// loader default CR. Operator + admission-webhook images bump to
// v0.1.111 (the new operator chart appVersion). Chart shape
// changes vs 0.1.14: operator chart now ships two CRD templates
// (applications.apprafter.io + platformstacks.apprafter.io); the
// admission-webhook ValidatingWebhookConfiguration now declares
// two webhook entries. No user-facing values schema change.
compatibility: "0.1.15": {
	change:          "safe"
	operatorVersion: "v0.1.111"
	notes: """
		Track B.1.72 closure: ship the PlatformStack CRD per
		spec §3.11 + restore the Application CRD that B.1.71
		dropped (no shipper after the imperative kubectl-apply
		path was removed). Both CRDs ship from the operator
		Helm chart at sync-wave -5 so cert-manager (-10) is
		up first and the operator + admission-webhook
		Deployments (default 0) see the schemas registered.

		Admission webhook gains PlatformStack validation:
		singleton (name=default, namespace=apprafter-system),
		channel enum, pin semver shape, source.checkInterval
		>= 1h. Webhook dispatch routes by kind. The
		ValidatingWebhookConfiguration template carries a
		second webhook entry pointing at the same /validate
		endpoint.

		Loader (cluster_bootstrap.rs) gains step 5: apply the
		default PlatformStack CR via SSA with field manager
		`apprafter-cli` after the platform Application reports
		Healthy. PlatformController reconciliation logic lands
		in B.1.73; until then the CR sits with empty status.

		No rendered chart values change vs 0.1.14 for any
		existing component — every diff is additive (new CRD
		templates + new webhook entry).
		"""
	references: [
		"docs/superpowers/plans/2026-05-19-track-b-1-72-platformstack-crd.md",
		"docs/changelog/UNRELEASED.md#v01111",
	]
}

// 0.1.16 — Track B.1.73 closure: PlatformController landed in
// the apprafter-operator binary. Reconciles `PlatformStack/default`
// by SSA-patching the parent `platform` Argo CD Application's
// `spec.source.{targetRevision, helm.valuesObject}` with field
// manager `platform-controller`. Three-version status model
// (currentVersion / targetVersion / availableVersion) populated;
// conditions `Synced`, `UpgradeAvailable`, `MigrationPending`,
// `UnauthorizedSourceModification` maintained per k8s convention.
// Chart-shape change vs 0.1.15: `_applicationsTemplate` reads
// `.Values.overrides.<component>.{pin, values, enabled}` and
// projects each onto the rendered child Application (pin
// replaces targetRevision, values mergeOverwrite onto component
// values, enabled gates emission). Operator + admission-webhook
// image bump to v0.1.114 (matches the new monorepo tag).
compatibility: "0.1.16": {
	change:          "safe"
	operatorVersion: "v0.1.114"
	notes: """
		Track B.1.73 closure — PlatformController core. Watches
		PlatformStack/default in apprafter-system; SSA-patches
		parent platform Application's spec.source with field
		manager `platform-controller`; populates three-version
		status (current/target/available) + conditions
		(Synced, UpgradeAvailable, MigrationPending,
		UnauthorizedSourceModification).

		Chart-shape change vs 0.1.15: `_applicationsTemplate`
		consumes `.Values.overrides` so PlatformController can
		patch per-component pin/values/enabled through the
		parent Application's helm.valuesObject. Schema
		(`values.schema.json`) declares `overrides` as an
		optional top-level object with the same key shape as
		`components`.

		Out of scope for 1.73 (deferred):
		  * Yanking field + skip-yanked logic → 1.74a.
		    `NoOpHooks::is_yanked` always returns false here.
		  * MigrationPlan auto-create → 1.74.
		    `NoOpHooks::request_migration_plan` is a no-op;
		    breaking-diff signal lives in MigrationPending=True
		    condition.
		  * Multi-stack support — singleton enforced by
		    webhook.
		  * Rollback flow (downgrade via lower pin) — needs
		    dedicated design for stateful components.

		Rendered chart vs 0.1.15: existing children unchanged
		when `.Values.overrides` is empty; the new template
		degrades cleanly with `default (dict)` so omitted
		overrides ⇒ original component values. Operators on
		0.1.15 upgrade without any cluster-side action; the
		PlatformController takes ownership of
		`spec.source.targetRevision` on first reconcile.
		"""
	references: [
		"docs/superpowers/plans/2026-05-20-track-b-1-73-platform-controller.md",
		"docs/changelog/UNRELEASED.md#v01114",
	]
}

// 0.1.17 — Track B.1.73 walk-fix #1: first post-closure walk
// surfaced PlatformController's `.status` empty even hours after
// CR creation. Root cause: operator chart's ClusterRole granted
// permissions only for `apprafter.io/applications` (B.1.71-era
// scope); PlatformController's watcher on
// `apprafter.io/platformstacks` failed silently on its first
// list/watch call (Forbidden), so the reconciler never ran.
// Same chart also lacked `argoproj.io/applications` get/patch
// needed for the SSA patch on the parent `platform` Application.
//
// Fix: ClusterRole gains two new rule blocks (platformstacks +
// argoproj.io/applications). Same chart bump also lowers
// liveness/readiness initialDelaySeconds on both operator and
// admission-webhook Deployments (10s/5s → 2s/1s) and introduces
// a startupProbe (1s period, 30s failureThreshold) to give cold
// boots a grace window without taxing every restart. Workspace-
// wide release profile sets `panic = "abort"` to shave a few MB
// off the musl-static images.
compatibility: "0.1.17": {
	change:          "safe"
	operatorVersion: "v0.1.115"
	notes: """
		Track B.1.73 walk-fix #1. Three changes, all in the
		operator chart + operator workspace:

		1. Operator ClusterRole gains rules for
		   `apprafter.io/platformstacks` (+ /status) and
		   `argoproj.io/applications` — without these
		   PlatformController fails silently on first watch
		   and `.status` never populates.
		2. Operator + webhook Deployments lose the 5-10s
		   readiness/liveness initial-delay padding; new
		   startupProbe (1s period × 30s failureThreshold)
		   keeps cold-boot tolerance without paying the
		   cost on every restart.
		3. Operator workspace `[profile.release]` sets
		   `panic = "abort"` — smaller binary, faster
		   image pull, panic-on-abort matches the
		   pod-restart contract.

		Rendered chart vs 0.1.16: existing children
		byte-equivalent (no per-component values change).
		Operators on 0.1.16 upgrade in place — chart sync
		applies the new ClusterRole rules + Deployment probe
		blocks; the operator pod restarts once when the new
		image arrives and starts reconciling PlatformStack
		correctly.
		"""
	references: [
		"docs/superpowers/plans/2026-05-20-track-b-1-73-platform-controller.md",
		"docs/changelog/UNRELEASED.md#v01115",
	]
}

// 0.1.18 — Track B.1.73 walk-fix #2: second post-closure walk
// (now with RBAC + faster probes from v0.1.115) surfaced
// PlatformController failing every reconcile with
// `ApiError: invalid object type: /, Kind=` (BadRequest 400)
// from the apiserver. Root cause: `write_status` SSA-patch body
// was `{"status": {...}}` only — missing apiVersion + kind +
// metadata.name. SSA REQUIRES these three TypeMeta fields in
// every patch body so the apiserver can resolve the resource's
// schema before merging. The Application reconciler's
// `apply_status` had carried this correctly since v0.1.31; we
// dropped it in B.1.73's new `write_status`.
//
// Fix: extracted `build_status_patch(name, status)` and
// `build_application_patch(desired)` helpers that always emit
// the full TypeMeta + metadata.name block. Two regression-guard
// tests pin the contract (`build_status_patch_includes_apiversion_kind_and_name`,
// `build_application_patch_includes_apiversion_kind_name_and_source`).
compatibility: "0.1.18": {
	change:          "safe"
	operatorVersion: "v0.1.116"
	notes: """
		Track B.1.73 walk-fix #2. PlatformController's
		`write_status` SSA patch missed the
		apiVersion + kind + metadata.name TypeMeta block —
		apiserver rejected every reconcile with
		`invalid object type: /, Kind=` BadRequest 400.

		Fix landed entirely operator-side: `write_status`
		now uses a `build_status_patch` helper that emits
		the full SSA-compliant body. `patch_application`
		got the same treatment (its TypeMeta was already
		correct but now sits behind the same helper
		convention). Two regression tests pin both helpers'
		output shape so future refactors can't strip
		TypeMeta silently.

		Rendered chart vs 0.1.17: existing children
		byte-equivalent (no chart-shape change). Operators
		on 0.1.17 upgrade in place — the operator pod
		restarts on new image and PlatformController starts
		populating `.status` correctly within the first
		reconcile cycle (seconds).
		"""
	references: [
		"docs/superpowers/plans/2026-05-20-track-b-1-73-platform-controller.md",
		"docs/changelog/UNRELEASED.md#v01116",
	]
}

// 0.1.19 — Track B.1.73 walk-fix #3: third post-closure walk
// (with RBAC + SSA TypeMeta from previous walk-fixes)
// surfaced two semantic bugs in the reconcile loop:
//
//   1. `UpgradeAvailable=True` fired for first-reconcile even
//      when `currentVersion == availableVersion`. Root cause:
//      old logic used `values_differ()` as the "is bump needed"
//      gate, but loader-created parent App lacks
//      `helm.valuesObject` (null vs `{tier: 1}` looks like a
//      diff). Fix: `UpgradeAvailable` is now a STRICT semver
//      comparison `channel_latest > target_for_patch`,
//      independent of values diffs.
//   2. PlatformController never patched the parent Application
//      when `pin==None && autoUpgrade==false`, so
//      `helm.valuesObject` stayed unset and
//      `platform-controller` never registered as a field manager
//      (visible in `managedFields[*].manager`). Fix: SSA patch
//      ALWAYS owns both `targetRevision` (kept at current when
//      policy forbids bump) AND `helm.valuesObject`. Values are
//      runtime config, not a version bump — they are not gated
//      by pin/autoUpgrade.
//
// Side effect: `MigrationPending` now also has an explicit
// `False/Clean` representation (was previously absent when no
// migration was pending). `Synced.reason` switches between
// `Patched` and `Reconciled` depending on whether the cycle
// actually issued an SSA patch.
compatibility: "0.1.19": {
	change:          "safe"
	operatorVersion: "v0.1.117"
	notes: """
		Track B.1.73 walk-fix #3. Reconcile-loop semantic
		refactor:

		* `UpgradeAvailable` condition is now a strict semver
		  comparison (`channel_latest > target_for_patch`).
		  Walk-found bug v0.1.116 → v0.1.117 where condition
		  fired True on first reconcile even when
		  current==available. Fail-safe on unparseable
		  versions — `semver_gt` returns false rather than
		  flapping the condition.

		* SSA ALWAYS owns `targetRevision` and
		  `helm.valuesObject`. `targetRevision` in the patch
		  body is kept at current value when policy
		  (pin/autoUpgrade) forbids bump; values are unconditional.
		  This guarantees `platform-controller` registers as
		  field manager on first reconcile so subsequent
		  foreign writes get caught by outside-writer
		  detection.

		* `MigrationPending` has an explicit `False/Clean`
		  branch. `Synced.reason` distinguishes `Patched` (we
		  issued an SSA patch this cycle) from `Reconciled`
		  (parent already matched desired state).

		8 new regression tests pin the new semantics:
		`semver_gt_*` (5 tests) + `platform_controller_owns_source_*`
		(3 tests). Total unit tests 32 → 40.

		Rendered chart vs 0.1.18: byte-equivalent — no chart-
		shape change. Operators on 0.1.18 upgrade in place;
		operator pod restarts on new image and the next
		reconcile correctly populates status.conditions and
		registers `platform-controller` as field manager on
		the parent platform Application.
		"""
	references: [
		"docs/superpowers/plans/2026-05-20-track-b-1-73-platform-controller.md",
		"docs/changelog/UNRELEASED.md#v01117",
	]
}

// 0.1.20 — Track B.1.73 walk-fix #4: fourth post-closure walk
// finally produced PlatformController logs that pinpointed the
// remaining issue — `compatibility.yaml` parsing was failing on
// every reconcile that reached the change-class classifier
// (target_changed && allow_target_bump). Five fixes land
// together so the same walk doesn't surface them iteratively.
//
// 1. **compatibility.yaml parser** — the rendered shape is a
//    top-level map keyed by version string, NOT wrapped in a
//    `compatibility:` object as the old `CompatibilityDoc`
//    struct expected. Direct `BTreeMap<String, VersionRecord>`
//    type alias replaces the wrapper.
//
// 2. **Observability** — info!() logs at PlatformController
//    spawn (lib.rs::run), Controller::run start, every
//    reconcile fire/finish, every SSA patch. Previously a
//    silent reconcile (RBAC failure, parse error, etc.) left
//    operators staring at a static status with no logs;
//    walk-fix #4 surfaces every reconcile attempt at info
//    level.
//
// 3. **Loader uses SSA + apprafter-cli whitelist** — step 3
//    (root Application) switches from client-side `kubectl
//    apply -f` to `apply_manifest_server_side` with field
//    manager `apprafter-cli` (same convention as step 5's
//    PlatformStack apply). PlatformController's
//    `detect_outside_writer` now whitelists `apprafter-cli`
//    alongside `platform-controller` and
//    `argocd-application-controller`. Closes the false-
//    positive `UnauthorizedSourceModification=True` that
//    every fresh bootstrap was emitting.
//
// 4. **Controller watches parent Application** — Argo CD
//    Application changes (manual kubectl-patch, foreign
//    writes, sync events) now trigger PlatformController
//    reconciles immediately via the `watches_with` clause.
//    Was: 6h checkInterval wait before detecting any
//    parent-App tampering. Now: ms-latency.
//
// 5. **Single-writer SSA pattern** — `patch_application`
//    always uses `force=true`. The old "patch without force,
//    then force-revert if foreign detected" two-step
//    deadlocked when the loader's `kubectl-client-side-apply`
//    already owned spec.source.targetRevision (409 Conflict
//    before reaching the revert path). PlatformController
//    is now THE single writer for
//    `spec.source.{targetRevision, helm.valuesObject}`;
//    foreign detection only surfaces the audit condition,
//    not the patch decision.
compatibility: "0.1.20": {
	change:          "safe"
	operatorVersion: "v0.1.118"
	notes: """
		Track B.1.73 walk-fix #4. Five fixes:

		1. compatibility.yaml parser accepts the actual
		   top-level-version-map shape the chart renders
		   (was looking for a `compatibility:` wrapper key
		   that doesn't exist).
		2. info!() observability throughout PlatformController
		   so silent reconciles can be diagnosed from logs.
		3. Loader SSA's the root Application with field
		   manager `apprafter-cli`; the operator
		   whitelists it. No more false-positive
		   `UnauthorizedSourceModification` on bootstrap.
		4. Controller watches the parent platform
		   Application — foreign writes (kubectl-patch /
		   kubectl-edit) trigger immediate reconcile +
		   revert instead of waiting for
		   spec.source.checkInterval.
		5. `patch_application` always uses force=true.
		   PlatformController is the single writer for the
		   parent App's spec.source. SSA conflict
		   negotiation removed.

		Known limitation: if an operator manually
		`kubectl-patch`-es the parent platform
		Application to an OLD platform-stack chart version
		that doesn't ship the PlatformStack RBAC
		(pre-0.1.17), Argo CD will overwrite the
		ClusterRole, PlatformController loses watch
		permissions, and the controller can no longer
		auto-revert. Recovery: manually `kubectl-patch`
		the parent target back to a known-good chart
		version. This is a degenerate case (operator
		actively downgrading past PlatformController-
		aware versions) and accepted as documented
		behaviour rather than engineered around.

		Rendered chart vs 0.1.19: byte-equivalent (no
		chart-shape change beyond version bumps).
		Operators on 0.1.19 upgrade in place; the
		operator pod restarts on new image and
		PlatformController begins reconciling correctly
		within the first cycle.
		"""
	references: [
		"docs/superpowers/plans/2026-05-20-track-b-1-73-platform-controller.md",
		"docs/changelog/UNRELEASED.md#v01118",
	]
}

// 0.1.21 — Track B.1.73 walk-fix #5: fifth post-closure walk
// finally showed clean Phase 1-3 status (RBAC + TypeMeta +
// semver + compatibility parser + observability all working),
// but the controller was burning hundreds of reconciles per
// second in a tight loop visible in the logs (`reconcile fired`
// + `reconcile completed` lines repeating ~3 per second).
//
// Root cause: every reconcile unconditionally queried OCI for
// channel-latest AND stamped `status.lastUpstreamCheck =
// Utc::now()`. The status SSA patch bumped the resource
// version, the watcher fired a fresh event, the next reconcile
// did the same — tight self-feedback loop.
//
// Two-pronged fix:
//   1. **OCI poll throttle** — `MIN_OCI_POLL_INTERVAL_SECS=60`
//      floor between actual OCI queries. Intermediate
//      reconciles re-use the cached `status.availableVersion`
//      and skip the lastUpstreamCheck timestamp update.
//   2. **Skip status write when unchanged** — new
//      `write_status_if_changed` short-circuits when the
//      computed `new_status` is byte-equal to the stored
//      `stack.status`. Combined with (1) + `condition()`'s
//      transition-time preservation, a no-op reconcile produces
//      an identical status, the patch never fires, no watch
//      event, loop dies.
//
// Side-benefit: foreign-writer revert (kubectl-patch on parent
// App) is no longer races against hundreds of in-flight
// reconciles competing for SSA ownership. The watch event
// fires a single reconcile, force-revert lands cleanly.
compatibility: "0.1.21": {
	change:          "safe"
	operatorVersion: "v0.1.119"
	notes: """
		Track B.1.73 walk-fix #5 — break the
		hundred-reconciles-per-second loop.

		Reconcile body now:
		* Throttles OCI poll to once per 60s
		  (MIN_OCI_POLL_INTERVAL_SECS). Intermediate
		  reconciles re-use the cached availableVersion
		  and don't touch lastUpstreamCheck.
		* Skips the status SSA patch when the computed
		  new_status is byte-equal to the stored status
		  (`write_status_if_changed`).

		Net behaviour: in steady state PlatformController
		emits ONE reconcile per minute (driven by the
		60s OCI poll throttle) instead of hundreds per
		second. Spec edits / parent App events still
		fire reconciles immediately as before, but the
		status write that follows is now idempotent —
		no self-triggering watch event.

		Rendered chart vs 0.1.20: byte-equivalent
		(no chart-shape change beyond version bumps).
		Operators on 0.1.20 upgrade in place; pod
		restarts on new image, reconcile loop drops
		from 100+ Hz to steady-state ~0.017 Hz
		(1/60s).
		"""
	references: [
		"docs/superpowers/plans/2026-05-20-track-b-1-73-platform-controller.md",
		"docs/changelog/UNRELEASED.md#v01119",
	]
}

// 0.1.22 — Track B.1.73 walk-fix #6: observability polish for
// foreign-writer detection. After walk-fix #5 closed the
// reconcile loop and the cluster reached steady state, a real-
// world test (manual `kubectl patch parent target=0.1.19`)
// showed PlatformController DID force-revert the parent
// Application — visible by eye in the Argo CD UI — but the
// `UnauthorizedSourceModification=True` condition was already
// cleared back to `False/Clean` by the next reconcile cycle
// (sub-second), and the WARN log line emitted at detection
// time was potentially lost during the operator pod's
// restart cascade. Net effect: zero durable audit trace for
// the violation.
//
// Fix: emit two Kubernetes Events per detected violation:
//
//   1. `Warning/ForeignFieldManager` — at detection, naming
//      the offending field manager + the parent target it
//      tried to set.
//   2. `Normal/SourceReverted` — after force-revert SSA
//      succeeds, naming the desired target the controller
//      restored.
//
// Events publish through `kube::runtime::events::Recorder`
// (kube-rs canonical helper), targeting the PlatformStack
// singleton with the parent Argo CD Application as
// `secondary` (Kubernetes `related`) so operators can
// correlate `kubectl describe platformstack default` ↔
// `kubectl describe application platform -n argocd`.
//
// Operator chart's ClusterRole gains a second `events`
// rule for the `events.k8s.io` apiGroup (kube-rs Recorder
// uses the v1 events API, which lives in `events.k8s.io`,
// not the legacy `""` core group).
compatibility: "0.1.22": {
	change:          "safe"
	operatorVersion: "v0.1.120"
	notes: """
		Track B.1.73 walk-fix #6 — observability polish.

		PlatformController now emits two Kubernetes Events
		per foreign-writer detection:

		* `Warning/ForeignFieldManager` at detection
		  (naming the offending manager).
		* `Normal/SourceReverted` after the force-revert
		  SSA patch succeeds.

		Both events target the PlatformStack singleton
		with the parent Argo CD Application as
		`secondary` (`related`). Visible via:

		    kubectl describe platformstack default -n apprafter-system
		    kubectl get events -n apprafter-system

		Events survive the brief `UnauthorizedSourceModification`
		condition flip-back-to-False that the next
		reconcile cycle does after the revert, and
		survive operator pod restarts (per Kubernetes
		event TTL, default 1h on most clusters).

		Operator chart's ClusterRole gains
		`events.k8s.io/events` create+patch rules
		(kube-rs `Recorder::publish` uses the v1 events
		API).

		Rendered chart vs 0.1.21: byte-equivalent
		operator+webhook component values; only RBAC
		ClusterRole grew by one apiGroup block.
		"""
	references: [
		"docs/superpowers/plans/2026-05-20-track-b-1-73-platform-controller.md",
		"docs/changelog/UNRELEASED.md#v01120",
	]
}

// 0.1.23 — Track B.1.74 closure: PlatformController status
// observability polish. Two surface additions on top of the
// already-shipping B.1.73 reconcile machinery (which already
// covers periodic check, OCI tag list, channel filter,
// availableVersion / lastUpstreamCheck updates, UpgradeAvailable
// condition, and safe-class auto-upgrade):
//
//   1. `status.versionHistory` ring buffer (capped at 10
//      entries). On each successful SSA patch that ACTUALLY
//      changes `targetRevision`, append
//      `{version, appliedAt, outcome: "succeeded"}`. Oldest
//      entries drop from the front when the cap is exceeded.
//      Feeds rollback decisions + audit. Empty until the first
//      version bump.
//
//   2. `Ready` condition mirroring parent's aggregate health.
//      True iff `parent.status.health.status == "Healthy"`
//      (Argo CD's own aggregation from child Applications +
//      their workloads). Surfaces alongside the other four
//      conditions in `kubectl describe platformstack default`.
//
// Skipped (per plan.md "Size: S" + YAGNI per CLAUDE.md):
//   * ETag-aware OCI requests — the existing throttle
//     (`MIN_OCI_POLL_INTERVAL_SECS=60`) + cached availableVersion
//     reuse already saturate the bandwidth concern; an ETag
//     pathway would shave bytes-per-poll without changing the
//     poll cadence. Deferred to a future perf pass.
//   * Breaking-class MigrationPlan auto-create — covered by
//     B.1.75 (MigrationPlan CRD + admission). B.1.74 keeps the
//     existing behaviour of pushing `MigrationPending=True`
//     condition + the `Normal/SourceReverted`-style audit
//     events from walk-fix #6.
//
// Rendered chart vs 0.1.22: byte-equivalent (no chart-shape
// change). Operator-binary change only; image v0.1.120 → v0.1.121
// via the standard chart appVersion lockstep.
compatibility: "0.1.23": {
	change:          "safe"
	operatorVersion: "v0.1.121"
	notes: """
		Track B.1.74 closure. PlatformController gains
		two status surface additions:

		* `status.versionHistory` ring buffer (cap 10).
		  Appended on each successful targetRevision
		  bump. Visible via:
		      kubectl get platformstack default \\
		          -n apprafter-system \\
		          -o jsonpath='{.status.versionHistory}'
		* `Ready` condition. True iff parent platform
		  Application reports Healthy; False with
		  `ParentNotHealthy` reason during sync /
		  Degraded states. Joins the four pre-existing
		  conditions (Synced / UpgradeAvailable /
		  MigrationPending / UnauthorizedSourceModification).

		Skipped: ETag-aware OCI requests (existing throttle
		+ cached availableVersion saturate the bandwidth
		concern; deferred). Breaking-class MigrationPlan
		auto-create still routes through the
		`MigrationPending=True` condition placeholder —
		B.1.75 lands the actual MigrationPlan CRD +
		controller logic.

		Acceptance walk piggybacks on B.1.73 walk-fix #6
		Event audit-trail verification (per user's
		earlier "test as regression during 1.74 walk"
		request).

		Rendered chart vs 0.1.22: byte-equivalent.
		Operator-binary change only; chart appVersion
		bump propagates the new image.
		"""
	references: [
		"docs/superpowers/plans/2026-05-20-track-b-1-73-platform-controller.md",
		"docs/changelog/UNRELEASED.md#v01121",
	]
}

// Walk-fix #7 follow-up to B.1.74. The versionHistory ring
// buffer surfaced a controller-cache race: a follow-up
// reconcile fires from our own status write, reads a STALE
// `PlatformStack` snapshot from the watcher cache (missing
// the entry the prior reconcile just persisted), and the
// subsequent SSA patch clobbers `versionHistory` with the
// stale vector. The fix omits `versionHistory` from the SSA
// patch body whenever the current reconcile cycle did NOT
// append a new entry — SSA preserves field values not
// present in the patch. Operator-binary change only; image
// v0.1.121 → v0.1.122 via the standard chart appVersion
// lockstep.
// Walk-fix #8b post-B.1.79a — cue-cmp image bump to v0.1.4
// alongside chart 0.1.45. Walk-fix #8 (chart 0.1.44)
// changed plugin.yaml without bumping the cue-cmp image
// version — argument was "ConfigMap mount overrides the
// image's plugin.yaml at runtime, so image content does
// not matter". cue-cmp-check workflow's drift detection
// disagreed: image-source files (Dockerfile, entrypoint.sh,
// plugin.yaml) changed since the published v0.1.3 tag
// without a matching version.cue bump → red CI.
//
// The drift check has the right policy: an operator who
// installs cue-cmp manually (without the chart's
// ConfigMap mount overlay) pulls the image and runs
// against its baked-in plugin.yaml. Stale image content
// = wrong behaviour for that path. Bumping the image
// version-with-source-change keeps the v0.1.4 ghcr tag
// honest.
//
// Effect on the chart: same plugin.yaml content via the
// ConfigMap (unchanged from chart 0.1.44), plus image
// pin advances from v0.1.3 → v0.1.4. cue-cmp sidecar pod
// rotates on chart upgrade. Operator workflow same as
// 0.1.44 (`apprafter platform upgrade --to 0.1.45` +
// `kubectl rollout restart deployment/argocd-repo-server
// -n argocd`).
// Walk-fix #11 post-B.1.79b-Part-3b — CMP entrypoint cds
// into the `apprafter/` package directory before `cue
// export`. The scaffold (walk-fix #5 post-Part-3b) vendors
// AppRafter schemas as a `cue.mod/` INSIDE `apprafter/`,
// which defines a CUE module boundary; `cue export ./...`
// run from a parent (Argo CD's cwd when `spec.source.path`
// is the repo root) does not cross into the nested module
// and fails with `cue: "./..." matched no packages`. The
// entrypoint now mirrors the discover convention: if cwd
// isn't already `apprafter/` and `./apprafter/` holds `.cue`
// files, cd into it. Self-healing — existing registrations
// (any source.path) work after the image upgrade without
// re-registering. cue-cmp image v0.1.5 → v0.1.6 (entrypoint
// .sh change baked into the image), chart pin follows via
// the `argocdcuecmp.version` CUE import.
// 0.2.1 — Phase 2 opens with the ServiceProvider CRD (plan.md
// 2.1). First 0.2-series platform-stack release; operator +
// admission-webhook images move to v0.2.1 in lockstep.
// 0.2.18 — Phase 2.6 (needs.redis) seed: adds the dragonfly-operator
// component + the redis-integrated ServiceProvider so a fresh cluster
// has the Redis stack ready for the 2.6-3 provisioner. Platform-stack-
// only — no operator binary change (operator stays v0.2.16).
// 0.2.19 — Phase 2.6 (needs.redis) close: the coordinated operator
// release carrying the Dragonfly backend (lazy pool + $N per-DB
// isolation + FLUSHDB GC) and the CRD-additive schema fields. Re-syncs
// the operator/platform-stack lockstep the 0.2.17 platform-stack-only
// yank broke — operator + admission-webhook images move to v0.2.19.
// 0.2.20 — Phase 2.6 walk-fix release. The full needs.redis -> Dragonfly
// chain is validated end-to-end on a live cluster for the first time
// (kind/podman walk green): provision -> $N isolation -> EVAL confinement ->
// client-init/queues -> restart re-pin -> persistent restart-durable ->
// snapshot -> GC FLUSHDB/DELUSER. Five operator/webhook bugs the live walk
// surfaced (none caught by unit/CRD gates): Dragonfly CR `spec.replicas`
// (omitted -> 0-replica StatefulSet -> no instance pod); the bogus
// `--maxmemory_policy` arg Dragonfly rejects (-> crashloop); invalid
// `+client|subcommand` ACL grants Dragonfly rejects; the dbnum allocator race
// under concurrent provisioning (-> two tenants on one DB, an isolation
// breach) fixed by serializing reconciles; and the operator-only-CREATE
// webhooks now accept `kubeadm:cluster-admins` break-glass (k8s 1.35).
// CRD-compatible with 0.2.19; existing claims keep their allocation.
// 0.2.26 — ADR 0048 (Argo CD platform-upgrade approval surface). The
// coordinated operator + chart release that makes the platform-stack
// MigrationPlan approvable from the Argo CD UI. Operator + webhook images
// move v0.2.24 -> v0.2.25 (new operator image -> operator pods restart on
// upgrade, hence requires-restart). Chart-delivered, no CLI / re-bootstrap.
compatibility: "0.2.30": {
	change:          "requires-restart"
	operatorVersion: "v0.2.28"
	notes: """
		ADR 0048 (revised) — the ROOT platform App's Argo TILE now reflects a
		pending upgrade. The prior `argoproj.io_Application` health banner was
		empirically disproven (kind+Argo): Argo applies that customization only
		to Application resources appearing as CHILDREN in another app's tree,
		never to a top-level app's OWN tile (whose health is the worst-of
		aggregate of its managed `.status.resources`) — so the root App stayed
		Healthy despite the annotation. Validated fix: the operator stamps
		`apprafter.io/upgrade-pending` (+from/to/class/plan) on the
		chart-managed `platform-migration-anchor` ConfigMap (it IS in the root
		App's `.status.resources`), and a `ConfigMap` health customization
		returns Suspended for it → the root App tile rolls up to **Suspended**
		(purple pause/attention) in the Applications list, nudging the operator
		to open + Approve. The dead `argoproj.io_Application` customization is
		removed. Confirmed live: the operator's SSA annotation survives Argo
		syncs with no OutOfSync (no ignoreDifferences needed); the key is
		`ConfigMap` (core/empty group — NOT `_ConfigMap`, which silently
		yields nil). Operator RBAC gains `configmaps` update/patch.

		ALSO: platform MigrationPlan GC is broadened + now runs every reconcile
		— it keeps only the current gate + any mid-rollout (approved/executing)
		plan and deletes the rest (stale pending + terminal
		completed/rejected/failed), so the Argo tree shows at most one active
		gate (walk-found: 24-25 + 25-26 + 26-28 completed plans all lingered).

		Operator + admission-webhook images move v0.2.27 → v0.2.28 (operator
		pods restart), hence requires-restart. Chart-delivered, no CLI /
		re-bootstrap.
		"""
	references: ["docs/changelog/UNRELEASED.md", "docs/adr/0048-argo-platform-upgrade-approval-surface.md"]
}

compatibility: "0.2.29": {
	change:          "requires-restart"
	operatorVersion: "v0.2.27"
	notes: """
		Two walk-found operator fixes. (1) GC of superseded platform
		MigrationPlans: the plan name is keyed on (from→to), so when the
		channel-latest target advances (a yank moved it 0.2.27→0.2.28) the
		prior pending-approval plan is orphaned beside the new one — the
		controller now lists platform plans and deletes any pending-approval
		plan that is not the current gate. (2) Stable minimal Deployment
		selector: the rendered Deployment reused the FULL label-set as its
		IMMUTABLE `spec.selector`, so 2.9 widening the label-set (added
		`apprafter.io/application` + `.../environment`) wedged every
		Deployment created under the prior set (422 "field is immutable"
		every reconcile — no image roll, no spec change). The selector is now
		the stable minimal `{apprafter.io/application: <name>}` (unique per
		namespace, never grows); the Application controller delete+recreates
		any Deployment whose live selector differs.

		**Upgrade impact:** the selector normalization recreates EVERY
		existing Deployment ONCE on this upgrade (delete + owns-watch
		recreate) — a brief per-app downtime. Operator + admission-webhook
		images move v0.2.26 → v0.2.27 (operator pods restart), hence
		requires-restart. Chart-delivered, no CLI / re-bootstrap.
		"""
	references: ["docs/changelog/UNRELEASED.md"]
}

compatibility: "0.2.28": {
	change:          "requires-restart"
	operatorVersion: "v0.2.26"
	notes: """
		Platform status/upgrade force-recheck. The operator now honors an
		`apprafter.io/recheck-requested` annotation on `PlatformStack/default`
		to force an immediate OCI upstream re-poll, bypassing the ~6h
		`source.checkInterval` poll cadence. `apprafter platform status` and
		`apprafter platform upgrade` stamp that annotation and wait briefly so
		they show / act on a FRESH `availableVersion` instead of a possibly
		hours-stale cached value; a `--cached` flag opts out of the round-trip.

		ALSO fixes the walk-found reconcile-FREEZE that yanks 0.2.26 and
		0.2.27: operator v0.2.25 lacked `configmaps` RBAC and treated the
		ADR 0048 anchor-ConfigMap GET as fatal, so on a gated upgrade the
		PlatformController 403'd reading platform-migration-anchor and
		aborted BEFORE the status write — availableVersion froze and the
		cluster could not even create the gate it needed approved (GitOps
		deadlock). 0.2.28 adds the `configmaps` get/list/watch ClusterRole
		rule AND makes the anchor ownerRef best-effort (any GET error ⇒
		un-owned, still CLI-approvable plan; the reconcile + status write
		always proceed).

		Operator + admission-webhook images change v0.2.25 → v0.2.26 (new
		operator image ⇒ operator pods restart on upgrade, hence
		requires-restart); CLI bumps v0.2.12 → v0.2.13.
		"""
	references: ["docs/changelog/UNRELEASED.md", "docs/adr/0048-argo-platform-upgrade-approval-surface.md"]
}

compatibility: "0.2.27": {
	change:          "requires-restart"
	operatorVersion: "v0.2.25"
	notes: """
		1.83a T8 live-Hetzner walk-fixes (2026-06-12) — the host-network
		platform Gateway never reached `Programmed` on a real cluster; two
		chart fixes (operator image unchanged, v0.2.25). **F1 (blocker):**
		`gateway-api-crds` now installs the **experimental** Gateway API
		channel (`config/crd/experimental`) instead of standard — Cilium
		1.16.5's gateway controller fails its startup required-resources
		check without `tlsroutes.gateway.networking.k8s.io` (v1alpha2), which
		exists only in the experimental channel; without it the `cilium`
		GatewayClass never Accepts and no Gateway is Programmed. Experimental
		is a superset of standard. **F2 (operability):** cilium-operator now
		carries an `operator.podAnnotations.apprafter.io/cilium-config-rev`
		so Argo rolls it when the cilium gateway/host-network config changes
		— the operator reads cilium-config only at start and otherwise keeps
		a stale config (gateway controller silently never starts). On upgrade
		both the Gateway API CRDs are replaced standard→experimental (additive
		superset) and cilium-operator rolls once. (Cosmetic OutOfSync of the
		Gateway/HTTPRoute — finalizer + API-defaulting drift — is tracked
		separately as a root-Application `ignoreDifferences` follow-up.)
		"""
	references: ["plan.md#1.83a", "docs/adr/0048-argo-platform-upgrade-approval-surface.md"]
	yanked:       true
	yankedReason: "Ships the same operator v0.2.25 as 0.2.26 (F1/F2 were chart-only gateway fixes, no operator change), so it carries the same anchor-403 reconcile-freeze: on a gated upgrade the PlatformController 403s reading the ADR 0048 platform-migration-anchor ConfigMap and aborts before the status write, freezing availableVersion. Fixed in 0.2.28 (operator v0.2.26)."
}

compatibility: "0.2.26": {
	change:          "requires-restart"
	operatorVersion: "v0.2.25"
	notes: """
		ADR 0048 — Argo CD platform-upgrade approval surface: the platform
		MigrationPlan is anchored into the platform-stack root Application's
		resource tree (via a chart `ConfigMap/platform-migration-anchor` it
		ownerReferences) so its approve/reject buttons become clickable in the
		Argo UI; a custom `apprafter.io_MigrationPlan` health labels the node
		with the upgrade (from→to, class) + approval hint; the operator stamps
		`apprafter.io/upgrade-*` annotations on the root Application, and a
		custom `argoproj.io_Application` health renders a "platform update
		pending approval" banner (the root-level signal; live-walk-validated
		safe). Approval still goes through the same `apprafter migration
		approve` / status.phase=approved path (ADR 0027 webhook guard).
		Operator + webhook images change (v0.2.24 → v0.2.25); chart-delivered,
		no CLI/re-bootstrap.
		"""
	references: ["docs/adr/0048-argo-platform-upgrade-approval-surface.md"]
	yanked:       true
	yankedReason: "operator v0.2.25 lacks `configmaps` RBAC AND treats the ADR 0048 anchor GET as fatal: on any gated (requires-restart) upgrade the PlatformController 403s reading platform-migration-anchor and aborts the reconcile BEFORE the status write — availableVersion freezes and the cluster can't even create the gate it needs to be approved (GitOps deadlock; manual configmaps-RBAC break-glass required). Fixed in 0.2.28 (operator v0.2.26: configmaps RBAC + best-effort anchor)."
}

compatibility: "0.2.25": {
	change:          "requires-restart"
	operatorVersion: "v0.2.24"
	notes: """
		Phase 1.83a (Public ingress MVP, order 3.5) — platform-stack-only;
		no operator release (operator + admission-webhook stay pinned
		v0.2.24). Adds a platform `Gateway` on the Cilium GatewayClass in
		**host-network mode** — Envoy binds the node host ports 80/443
		directly, so there is no LoadBalancer Service / LB-IPAM / L2
		announcement (L2/ARP does not work on cloud SDNs). Per
		`gateway.allowedDomains` entry the chart emits an apex + wildcard
		HTTPS listener pair (imported-cert mode, `certificateRefs` to a
		per-domain `kubernetes.io/tls` Secret) plus an http→https 301
		redirect HTTPRoute. The upstream Gateway API v1.2.1 CRDs ship as a
		new wave -25 Argo component. Empty `allowedDomains` (the default) ⇒
		no Gateway resources are rendered.

		**Upgrade impact:** the Argo-managed cilium component reconfigures
		to host-network Gateway/Envoy on auto-update (Envoy DaemonSet roll
		— `gatewayAPI.hostNetwork.enabled` + Envoy NET_BIND_SERVICE caps)
		on EVERY cluster, including clusters with no configured domain
		(those get the cilium reconfigure but no Gateway). Hence
		`requires-restart`, not `safe`: the change is purely additive at the
		umbrella-resource level but rolls the running CNI/Envoy DaemonSet on
		upgrade. The bootstrap CLI loader values are byte-identical to
		0.2.24 — no re-bootstrap and no CLI bump. CRD-compatible with
		0.2.24 (purely additive umbrella resources + the Gateway API CRDs).
		"""
	references: ["plan.md#1.83a"]
}

compatibility: "0.2.24": {
	change:          "safe"
	operatorVersion: "v0.2.24"
	notes: """
		Phase 2.12 (Application.env value references, ADR 0046) close — the
		last Phase-2 subphase. `Application.spec.*.env` values may now be a
		literal string, a claim reference (bare CUE selector `claim.<type>.<field>`
		or named `claim.<type>.<name>.<field>` — pg/redis only at launch), or an
		external-secret reference (`secret: "<name>/<key>"`). At the CR level each
		ref is a string-discriminated marker (`{claim: "pg.url"}` / `{secret:
		"stripe/api-key"}`); the operator renderer resolves it to a container
		`EnvVar{valueFrom: secretKeyRef}`. The provisioner now writes CANONICAL
		decomposed connection-Secret keys (`url user pass host port db` + redis
		`channelPrefix`) and DROPS the old composed `DATABASE_URL`/`REDIS_URL`/
		`REDIS_CHANNEL_PREFIX` key names; `acl_reconcile` reads `pass` directly.

		**The 2.4e implicit `DATABASE_URL` auto-injection is REMOVED** — a
		`needs.pg` app no longer auto-gets `DATABASE_URL`; it must declare it
		explicitly (`env: {DATABASE_URL: claim.pg.url}`). Apps relying on the
		auto-injection must add the explicit ref before upgrading (a one-line
		manifest change). The 2.4e literal-`DATABASE_URL`-collision webhook guard
		is also gone.

		The cue-cmp (**v0.1.8 → v0.1.9**) now injects the current schema + a
		generated `claim` binding (from the manifest's effective needs) into its
		ephemeral render workspace so bare `claim.*` selectors resolve to markers;
		`apprafter app scaffold` stops vendoring the schema, and a new
		`apprafter app validate` runs the same injection locally. Validation is
		layered: `cue vet` (claim refs, undeclared need / non-enum field) +
		admission webhook (format) + runtime (missing external Secret → Application
		`Ready=False` reason `EnvSecretMissing`).

		CRD change is additive (the `env` value node gains
		`x-kubernetes-preserve-unknown-fields: true`) — CRD-compatible with 0.2.23;
		existing literal-only `env` maps validate unchanged. Operator + webhook +
		cue-cmp binaries change; chart appVersion bump propagates the new images.
		"""
	references: ["plan.md#2.12", "docs/adr/0046-env-value-references.md"]
}

compatibility: "0.2.23": {
	change:          "safe"
	operatorVersion: "v0.2.23"
	notes: """
		Phase 2.10 (needs → CiliumNetworkPolicy egress auto-derivation,
		ADR 0045) close. The Application controller now SSA-applies one
		per-Application `CiliumNetworkPolicy` (`<rendered-name>-egress`,
		owner-referenced to the Application) that makes the app's pods
		egress default-deny and allows only: DNS, same-namespace
		(profile-gated), the external internet (profile-gated), and one
		rule per declared `needs` entry (pg → cnpg-system:5432, redis →
		dragonfly-system:6379; disk has no network surface). The apply is
		gated on the `ciliumnetworkpolicies.cilium.io` CRD being served —
		a one-time startup probe — so non-Cilium clusters render nothing.

		Breadth is config-driven via a new optional
		`PlatformStack.spec.network.egress.profile`
		(`internet` (default) | `internal` | `strict`), resolved cluster-
		wide by the controller. `internet` keeps the open-world default on
		every tier; `internal` drops the world rule (DNS + same-ns +
		needs); `strict` additionally drops same-namespace egress. Managed
		from the CLI by `apprafter platform egress show` / `set <profile>`
		(no raw kubectl) — `set` applies under a dedicated field manager so
		it never prunes the bootstrap-owned `spec.source`/`spec.values`.

		CRD change is additive (optional
		`PlatformStack.spec.network.egress.profile`) — CRD-compatible with
		0.2.22; a stack with no profile renders the internet default,
		unchanged. New operator ClusterRole grants: `ciliumnetworkpolicies`
		(get/list/watch/create/patch/delete) + `customresourcedefinitions`
		get (the CRD probe). Operator + webhook binary change; chart
		appVersion bump propagates the new images.
		"""
	references: ["plan.md#2.10", "docs/adr/0045-needs-networkpolicy-egress.md"]
}

compatibility: "0.2.22": {
	change:          "safe"
	operatorVersion: "v0.2.22"
	notes: """
		Phase 2.9 (per-environment deploy, ADR 0044) close — the coordinated
		operator + cue-cmp release. Environment selection becomes a
		DEPLOY-TIME, per-Application property: a new optional
		`Application.spec.environment` (injected by the cue-cmp from the Argo
		Application's APPRAFTER_APP_ENV plugin env) selects which
		`environments.<env>` override unifies onto base; the inert cluster-wide
		`APPRAFTER_ENV` operator var is removed. The operator reads the per-CR
		field via `effective_spec`, stamps the existing
		`apprafter.io/application` + `apprafter.io/environment` labels on
		children + surfaces `status.environment`, and the application-scoped
		MigrationPlan gate now matches the per-CR env. A new optional
		`PlatformStack.spec.defaultEnvironment` is the CLI's soft default. The
		admission webhook rejects a `spec.environment` not in
		`spec.environments`.

		**cue-cmp bumped to v0.1.8** (CRITICAL): the in-cluster injection was
		inert before — Argo CD exposes `spec.source.plugin.env` vars prefixed
		with `ARGOCD_ENV_`, so the sidecar read the bare name and never
		injected `spec.environment` (every deploy rendered base, `--env`
		silently ignored). v0.1.8 resolves the `ARGOCD_ENV_`-prefixed form.
		platform-stack derives the cue-cmp image tag from
		`argocd-cue-cmp/version.cue`, so this stack ships v0.1.8.

		CRD changes are additive (optional `Application.spec.environment` +
		`status.environment`, optional `PlatformStack.spec.defaultEnvironment`)
		— CRD-compatible with 0.2.21; existing deployments (no
		`spec.environment`) render base, unchanged. CLI surface:
		`app add --env` (one Argo app `<name>-<env>` per env-deployment) +
		interactive env/namespace pickers + `app status`/`remove` aggregate by
		`apprafter.io/application`.
		"""
	references: ["plan.md#2.9", "docs/adr/0044-per-environment-deploy.md"]
}

compatibility: "0.2.21": {
	change:          "safe"
	operatorVersion: "v0.2.21"
	notes: """
		Phase 2.6b (needs.disk + named multi-claims, ADR 0043) close — the
		coordinated operator + chart release. `needs` generalizes to a closed
		struct of `(type, name)` entries (scalar OR array per type): the
		Application controller generates one ResourceClaim per entry
		(`<app>-<type>[-<name>]`) and the renderer disambiguates injected env
		vars (`DATABASE_URL_<NAME>` for named pg, unnamed stays DATABASE_URL).
		`needs.disk` provisions an UNOWNED ReadWriteOnce PVC via the new
		`Backend::Disk` provisioner arm (matched to the seeded `disk-local`
		ServiceProvider; class local / local-path on T1); the renderer mounts
		it and pins the Deployment to strategy:Recreate. Deleting the app
		snapshots a disk-shaped RetainedClaim (volumeClaimRef + namespace) and
		the unowned PVC survives the 7-day grace; a redeploy reattaches the
		same PVC (cancel-on-reprovision), and force-GC drops the PVC + the
		snapshot.

		CRD changes are additive and backward-compatible: the `needs` keys
		accept scalar-or-array (x-kubernetes-preserve-unknown-fields per key),
		ResourceClaim gains `status.volumeClaimRef` + a relaxed `spec.size`
		(quantity for disk, t-shirt for service), the ServiceProvider `type`
		enum gains `disk`, and RetainedClaim gains optional disk fields
		(volumeClaimRef / volumeClaimNamespace). The `disk-local`
		ServiceProvider seed ships in this release. CRD-compatible with
		0.2.20; existing pg/redis claims are unaffected.
		"""
	references: ["plan.md#2.6b", "docs/adr/0043-needs-disk-named-claims.md"]
}

compatibility: "0.2.20": {
	change:          "safe"
	operatorVersion: "v0.2.20"
	notes: """
		Phase 2.6 walk-fix release — the needs.redis -> Dragonfly chain is now
		validated live (kind/podman walk green). Operator fixes: Dragonfly CR
		sets spec.replicas (was 0 -> no pod); drops the bogus --maxmemory_policy
		arg (crashloop); valid ACL grant (no +client|subcommand); serialized
		provisioner reconciles so concurrent claims never collide on a dbnum
		(was an isolation breach); webhook break-glass accepts
		kubeadm:cluster-admins. CRD-compatible with 0.2.19.
		"""
	references: ["plan.md#2.6", "docs/adr/0042-needs-redis-dragonfly.md"]
}

compatibility: "0.2.19": {
	change:          "safe"
	operatorVersion: "v0.2.19"
	yanked:          true
	yankedReason:    "Dragonfly CR omitted spec.replicas → 0-replica StatefulSet → no instance pod → needs.redis never provisions (the dragonfly-operator wires replicas with no default). Found on the first live needs-redis walk; fixed in v0.2.20."
	notes: """
		Phase 2.6 (needs.redis → Dragonfly, ADR 0042) close — the
		coordinated operator + chart release. The provisioner gains a
		`Backend::Dragonfly` arm: it lazily creates a shared Dragonfly
		instance per persistence class, allocates each claim its own
		numbered logical DB, drives a `$N`-pinned ACL user over the Redis
		protocol (hard per-DB keyspace isolation), and writes a connection
		Secret carrying REDIS_URL + REDIS_CHANNEL_PREFIX. A reconcile loop
		re-pins claim users after an instance restart; the 7-day-grace GC
		reclaims via FLUSHDB + ACL DELUSER.

		CRD changes are additive and backward-compatible — existing claims
		are unaffected: `persistent?` on the needs schema, `status.{instance,
		dbnum}` allocation fields on ResourceClaim, and a relaxed
		RetainedClaim (CNPG-specific fields made optional + new optional
		dragonfly allocation fields; the `required` list narrows to
		[claimRef, provider, backend, retainUntil]).

		Re-syncs the operator/platform-stack lockstep the 0.2.17
		platform-stack-only yank broke: operator + admission-webhook images
		move to v0.2.19. The CLI is unchanged (redis claims surface via the
		generic 2.4g `app status` claims display) — no cli/Cargo.toml bump,
		no monorepo tag.
		"""
	references: [
		"docs/superpowers/plans/2026-06-05-2.6-needs-redis-dragonfly.md",
		"docs/adr/0042-needs-redis-dragonfly.md",
		"plan.md#2.6-8",
	]
}
compatibility: "0.2.18": {
	change:          "safe"
	operatorVersion: "v0.2.16" // operator binary unchanged in 2.6-1
	notes: """
		dragonfly-operator component (always-on, dragonfly-system, sync-wave -5)
		+ redis-integrated ServiceProvider seed (backend dragonfly, dbnum=1024 /
		num_shards=1 tier-tunable knobs). Platform-stack-only; no operator binary
		change. Seeds the Redis stack so fresh clusters are ready for the 2.6-3
		provisioner. No shared Dragonfly instance is created until the first
		needs.redis claim (lazy).
		"""
	references: ["plan.md#2.6-1", "docs/adr/0042-needs-redis-dragonfly.md"]
}
compatibility: "0.2.17": {
	change:          "safe"
	operatorVersion: "v0.2.16"
	notes: """
		Platform-stack metadata release: publishes the `0.2.15` yank
		(that release shipped an apiserver-invalid Application CRD) in
		the cumulative compatibility doc. Same operator + components as
		0.2.16 — no code change. A `compatibility.cue` edit (incl. a
		yank) changes the chart source, so it must ride a
		`currentVersion` bump; the platform-stack drift guard enforces
		this. Safe to auto-sync.
		"""
	references: ["platform-stack/cue/compatibility.cue"]
}
compatibility: "0.2.16": {
	change:          "safe"
	operatorVersion: "v0.2.16"
	notes: """
		Hotfix for v0.2.15: the 2.4h `Application` CRD shipped
		`imagePolicy` (base + per-env) and `status.image` with both
		`properties` and `additionalProperties: false`, which the
		apiserver rejects ("additionalProperties and properties are
		mutual exclusive") — so the v0.2.15 CRD apply failed
		server-side, the operator child app never synced, and image
		resolution silently did nothing (verbatim tag, no
		`status.image`). Removes the three invalid lines (closed CRD
		objects rely on structural-schema pruning). Validated against a
		real apiserver (`just crd-validate`). Same operator code as
		v0.2.15; chart-only CRD fix. A cluster stuck on the failed
		v0.2.15 sync recovers automatically once v0.2.16 is the
		channel-latest (or pin/upgrade to it). Safe to auto-sync.
		"""
	references: [
		"operator/charts/apprafter-operator/templates/crd-application.yaml",
		"scripts/validate-crds.sh",
	]
}
compatibility: "0.2.15": {
	change:          "safe"
	operatorVersion: "v0.2.15"
	yanked:          true
	yankedReason:    "Invalid Application CRD (imagePolicy/status.image set additionalProperties alongside properties) — the apiserver rejects the CRD apply, so the operator never rolls to this version. Superseded by 0.2.16."
	notes: """
		Image tag→digest resolution & auto-rollout (ADR 0040). The
		Application CRD gains two OPTIONAL, additive fields
		(`spec.base.imagePolicy.resolve: digest|off` and
		`status.image.{tag,resolved,resolvedAt}`) plus an
		`ImageResolved` condition — no breaking schema change, no data
		migration. The Application controller now resolves
		`spec.base.image`'s tag to its current registry digest each
		reconcile (throttled to ~60s, anonymous or via a covering
		`SourceCredential`) and pins the Deployment to
		`repo@sha256:<digest>`, so a moved tag auto-rolls the workload
		(push→deploy). **Default-ON, all tiers:** after this upgrade,
		every Application with a mutable tag begins resolving on its
		next reconcile — a workload that had a moved tag will roll once
		to the current digest. Opt out per-app with
		`imagePolicy.resolve: off` (renders the verbatim reference, no
		registry poll). Resolution is best-effort and NEVER blocks the
		rollout (failure → verbatim tag + `ImageResolved=False`). Safe
		to auto-sync.
		"""
	references: [
		"docs/adr/0040-image-digest-resolution.md",
		"operator/operator-controllers/application/src/oci_resolve.rs",
		"operator/operator-controllers/application/src/lib.rs",
	]
}
compatibility: "0.2.14": {
	change:          "safe"
	operatorVersion: "v0.2.14"
	notes: """
		Operator-only PlatformController resilience fix (no CRD
		change). Version DETECTION (resolving the channel-latest /
		availableVersion from the OCI upstream) is decoupled from
		ENFORCEMENT (deploying the policy target): a resolver failure
		no longer aborts the reconcile, so a pinned stack still
		converges to its pin and an unpinned one keeps its last-known
		target, instead of crash-looping before the pin is applied
		(the 2.4g-walk deadlock that froze every status condition at
		its last-good value). The transition-classification fetch
		fails closed (holds current, never bumps unclassified), and a
		new UpstreamReachable condition surfaces a degraded poll in
		`apprafter platform status`. Pure reconcile-loop fix over the
		0.2.13 controller set; same CRDs, RBAC, and components. Safe
		to auto-sync.
		"""
	references: [
		"operator/operator-controllers/platform-stack/src/reconcile.rs",
		"operator/operator-controllers/platform-stack/src/status.rs",
	]
}
compatibility: "0.2.13": {
	change:          "safe"
	operatorVersion: "v0.2.13"
	notes: """
		Operator-only OCI version-resolver upgrade to the ADR-0041
		channel-tag fast path (no CRD change). The PlatformStack
		resolver reads the channel-latest in O(1) from the moving
		`<repo>:<channel>` compatibility doc, with the paginated tag
		listing kept only as the pre-contract fallback. Manifest
		not-found is now classified structurally off the OCI error
		code (not Display text), so a blob-pull error can no longer be
		swallowed into the fallback; a phantom compat-doc key above the
		published channel-latest is capped out via the chart's own
		`org.opencontainers.image.version` annotation. The publish
		workflow moves the channel tags with `oras tag` (a Helm-OCI
		image-manifest carbon-copy), not `docker buildx imagetools
		create` (which would re-wrap it in an image index the resolver
		rejects). Pure reconcile-loop / OCI-client fix over the 0.2.12
		controller set; same CRDs, RBAC, and components. Safe to
		auto-sync.
		"""
	references: [
		"operator/operator-controllers/platform-stack/src/compatibility.rs",
		"operator/operator-controllers/platform-stack/src/reconcile.rs",
		".github/workflows/platform-stack-publish.yml",
		"docs/adr/0041-channel-tag-version-resolution.md",
	]
}
compatibility: "0.2.12": {
	change:          "safe"
	operatorVersion: "v0.2.12"
	notes: """
		Operator-only OCI version-resolver pagination fix (no CRD
		change). The PlatformStack version resolver now paginates all
		OCI tag pages (following the last-cursor / next-page link)
		instead of reading only the first page, so newly-published
		platform versions that land on a later tag page are seen —
		fixes the stale availableVersion symptom (available=0.2.2 while
		a newer version had already been published to GHCR). Pure
		reconcile-loop / OCI-client fix over the 0.2.11 controller set;
		same CRDs, RBAC, and components. Safe to auto-sync.
		"""
	references: ["operator/operator-controllers/platform-stack/src/oci.rs"]
}
compatibility: "0.2.11": {
	change:          "safe"
	operatorVersion: "v0.2.11"
	notes: """
		2.4f GC correctness fix (operator-only, no CRD change). Fix A:
		re-provisioning a claim now cancels its pending RetainedClaim and
		the GC controller guards against dropping a role/DB still bound to
		a live claim — closes a recovery time-bomb that dropped a
		re-attached claim's database. Fix B: GC now drops the Postgres
		role via ensure:absent (remove-from-managed-roles alone left the
		role behind) so roles no longer leak after the grace window. Both
		are reconcile-loop fixes over the 0.2.10 RetainedClaim machinery;
		same CRDs, RBAC, and controller set. Safe to auto-sync.
		"""
	references: ["plan.md 2.4f"]
}
compatibility: "0.2.10": {
	change:          "safe"
	operatorVersion: "v0.2.10"
	notes: """
		2.4f — RetainedClaim CRD + 7-day grace GC. A deleted pg
		ResourceClaim is snapshotted into a new immutable RetainedClaim
		(apprafter-system) by the provisioner finalizer; a new GC
		controller drops the Postgres role + database (ensure:absent) +
		password Secret after retainUntil, then deletes the snapshot.
		Closes the 2.4c cleanup skeleton (role/DB no longer leak on
		delete). New operator-only RetainedClaim CRD (immutable spec),
		RBAC, and a 7th controller. Safe to auto-sync.
		"""
	references: ["plan.md 2.4f"]
}
compatibility: "0.2.9": {
	change:          "safe"
	operatorVersion: "v0.2.9"
	notes: """
		2.4e — needs.pg DSN injection. The Application controller now
		injects DATABASE_URL into a needs.pg workload's Deployment via
		valueFrom.secretKeyRef pointing at the provisioned connection
		Secret (key DATABASE_URL), once the claim is ready. The admission
		webhook now rejects an Application that declares needs.pg AND sets
		a literal env DATABASE_URL (reserved — hard reject; revisit at UX
		polish). pg-only (jetstream/redis are 2.5/2.6); not the full 2.12
		claim.* reference engine. No CRD schema change. Safe to auto-sync.
		"""
	references: ["plan.md 2.4e"]
}
compatibility: "0.2.8": {
	change:          "safe"
	operatorVersion: "v0.2.8"
	notes: """
		2.4d — Application generates child ResourceClaims from
		`spec.*.needs` and pauses in a new AwaitingResourceClaim phase
		until each claim is provisioned (status.ready +
		connectionSecretRef), resuming via an owns-watch. Also fixes a
		2.4b gap: effective_spec now merges `needs` on env override.
		needs.*.selector changes are treated as non-destructive (no
		MigrationPlan gate) in 2.4d — revisit in 2.5+. New operator RBAC:
		create/patch/watch resourceclaims. No CRD schema change. DSN
		injection into the Deployment is 2.4e — a needs.pg app resumes
		WITHOUT DATABASE_URL until then. Safe to auto-sync.
		"""
	references: ["plan.md 2.4d"]
}
compatibility: "0.2.7": {
	change:          "safe"
	operatorVersion: "v0.2.7"
	notes: """
		2.4c — resourceclaim-provisioner controller. Additive: a 6th
		in-cluster controller that provisions each Scheduled pg
		ResourceClaim into the shared CloudNativePG cluster
		(created lazily on the first claim) — per-claim role + database
		+ a connection Secret with DATABASE_URL — and writes
		status.ready / connectionSecretRef / Ready under its own field
		manager. New RBAC: postgresql.cnpg.io clusters+databases CRUD,
		secrets create/update. No CRD change, no data migration — safe
		to auto-sync. The Application still does not generate claims
		until 2.4d, so the provisioner is a no-op on a fresh cluster.
		"""
	references: ["plan.md 2.4c"]
}
compatibility: "0.2.6": {
	change:          "safe"
	operatorVersion: "v0.2.6"
	notes: """
		2.4b — re-add the Application `needs` schema. Additive: the
		Application CRD gains `spec.base.needs` + per-environment
		`needs`, a typed map keyed by platform-service type
		(`{pg|jetstream|clickhouse|redis|s3|notifications}: {selector?,
		size?}`). The admission webhook rejects unknown needs keys; the
		CRD enforces selector minProperties 1 + the size enum. Operator
		+ admission-webhook images rebuild (kube-rs ServiceNeed type +
		webhook rule) — operatorVersion v0.2.4 -> v0.2.6 (re-aligned
		after the platform-stack-only 2.4a at 0.2.5). Pure schema: no
		controller generates claims from `needs` until 2.4d, so `needs`
		is inert on a fresh cluster. No existing field reshaped, no data
		migration — safe to auto-sync.
		"""
	references: ["plan.md 2.4b"]
}
compatibility: "0.2.5": {
	change:          "safe"
	operatorVersion: "v0.2.4"
	notes: """
		2.4a — CloudNativePG operator + pg-integrated ServiceProvider
		seed. Additive: a new always-on `cloudnative-pg` component
		(CNPG operator chart 0.28.2, namespace cnpg-system, project
		platform-providers, sync-wave -5) plus a new data-driven
		`templates/serviceproviders.yaml` that seeds one
		`pg-integrated` ServiceProvider CR (type pg, backend
		cloudnative-pg, label tier=integrated) into apprafter-system.
		NO shared Postgres Cluster is seeded — the 2.4c provisioner
		creates `platform-postgres` lazily on the first matched pg
		claim, so solo clusters with no pg apps pay no Postgres-pod
		cost. operatorVersion unchanged (v0.2.4): no operator image
		bump. No existing resource or values reshaped, no data
		migration — safe to auto-sync. Nothing creates pg claims
		until 2.4d, so the seed is inert on a fresh cluster.
		"""
	references: ["plan.md 2.4a"]
}
compatibility: "0.2.4": {
	change:          "safe"
	operatorVersion: "v0.2.4"
	notes: """
		2.3 — ResourceClaim scheduler controller. Additive: a new
		in-cluster controller (resourceclaim-scheduler, a fifth peer
		reconciler in the apprafter-operator binary) that matches each
		ResourceClaim to a ServiceProvider by service-type equality +
		label-superset and records the winner in status.provider plus a
		Scheduled=True condition. No match -> Scheduled=False + a
		NoMatchingServiceProvider Warning event + the new
		apprafter_claim_unmatched_total metric. New ClusterRole rules
		(serviceproviders read; resourceclaims + /status write). No
		existing resource or values reshaped, no data migration — safe
		to auto-sync. Nothing creates ResourceClaims until 2.4, so the
		scheduler is a no-op on a fresh cluster. status.ready /
		connectionSecretRef and actual provisioning are 2.4.
		"""
	references: ["plan.md 2.3"]
}
compatibility: "0.2.3": {
	change:          "safe"
	operatorVersion: "v0.2.3"
	notes: """
		2.2 — ResourceClaim CRD. Additive: a new namespaced
		v1alpha1 ResourceClaim CRD (CUE schema + hand-rolled
		OpenAPI v3 CRD at sync-wave -5 + kube-rs type) plus a new
		validating-webhook entry enforcing operator-only CREATE
		(rejects user-authored claims; UPDATE ungated). No
		existing resource or values reshaped, no data migration —
		safe to auto-sync. status.conditions ships schema-only (no
		writer until 2.3); nothing creates ResourceClaims yet (the
		Application->claim generator lands in 2.4), so the
		operator-only rule is a forward-looking guard.
		"""
	references: ["plan.md 2.2"]
}
compatibility: "0.2.2": {
	change:          "safe"
	operatorVersion: "v0.2.2"
	notes: """
		2.1 re-release — fixes the v0.2.1 publish defect. The
		ServiceProvider CRD payload is unchanged from 0.2.1
		(additive, safe). The fix is in the release pipeline:
		the operator + admission-webhook Helm charts now publish
		to the `ghcr.io/apprafter/charts` OCI sub-namespace
		instead of the org root. In 0.2.1 the chart and the
		container image shared the repo path
		(`ghcr.io/apprafter/apprafter-operator`) and, with chart
		version == appVersion, the chart push OVERWROTE the
		image tag with the chart .tgz — the operator + webhook
		pods crash-looped `exec: "/<bin>": no such file`. With
		charts under `/charts`, chart version == appVersion is
		safe. 0.2.1 is abandoned (its operator/webhook image
		tags are poisoned); upgrade straight to 0.2.2.
		"""
	references: ["plan.md 2.1"]
}
compatibility: "0.2.1": {
	change:          "safe"
	operatorVersion: "v0.2.1"
	notes: """
		2.1 — ServiceProvider CRD. Additive: a new namespaced
		v1alpha1 ServiceProvider CRD (CUE schema + hand-rolled
		OpenAPI v3 CRD at sync-wave -5 + kube-rs type) plus a new
		validating-webhook entry for it (closed built-in type
		enum pg|jetstream|clickhouse|redis|s3|notifications,
		non-empty backend). No existing resource or values
		reshaped, no data migration — safe to auto-sync. First
		0.2-series platform-stack release; operator + admission
		webhook images move to v0.2.1.
		"""
	references: ["plan.md 2.1"]
}
compatibility: "0.1.52": {
	change:          "safe"
	operatorVersion: "v0.1.137"
	notes: """
		English-only cleanup — purge Cyrillic from chart/sidecar
		source. Comment / doc-string only across platform-stack
		CUE (homoglyph letters + a few translated Russian
		comments) plus the argocd-cue-cmp image bumped 0.1.6 ->
		0.1.7 (entrypoint/Dockerfile comment cleanup only — a
		functionally identical rebuild). No rendered k8s resources
		reshaped beyond the sidecar image tag, no values reshaped,
		no data migration, classification vocabulary unchanged.
		Operator image unchanged (v0.1.137) — safe to auto-sync.
		"""
	references: ["plan.md 1.79c"]
}
compatibility: "0.1.51": {
	change:          "safe"
	operatorVersion: "v0.1.137"
	notes: """
		1.79c S5 — SourceCredential operator-side closure.
		Additive + behavioural, no values reshaped, no data
		migration:

		  * live validity probe — for each covered prefix the
		    operator finds a representative repo/image from a
		    matching Argo / AppRafter Application and probes it
		    (git smart-HTTP / scoped registry v2 token exchange);
		    GitValid/RegistryValid + lastValidated now reflect it.
		    Conservative mapping — a blocked egress reports
		    Unverified, never Invalid;
		  * derived-Secret GC finalizer — on SourceCredential
		    delete the operator GCs the cross-namespace Argo
		    repo-creds + canonical dockerconfigjson, then releases
		    the finalizer (no ownerReference cascade is possible
		    across namespaces). RBAC gains `delete` on secrets;
		  * scoped CLI credential-author Role seed (unbound);
		  * SourceCredentialMigrationStrategy.detect_destructive
		    classifier (live plan-creation wiring co-deferred with
		    application-scope B.1.77).

		Operator image v0.1.136 → v0.1.137 — safe to auto-sync.
		"""
	references: ["plan.md 1.79c"]
}
compatibility: "0.1.50": {
	change:          "safe"
	operatorVersion: "v0.1.136"
	notes: """
		1.79c walk-fix #4 — operator skips reconcile for an
		Application carrying a deletionTimestamp. Without it, the
		Argo CD cascade-deletion finalizer (set by `app add`)
		hangs: the operator keeps re-applying the Deployment a
		cascade is removing, so the managed-resource tree never
		empties. Operator image v0.1.135 → v0.1.136; behavioural
		bugfix only, no values reshaped — safe to auto-sync.
		"""
	references: ["plan.md 1.79c"]
}
compatibility: "0.1.49": {
	change:          "safe"
	operatorVersion: "v0.1.135"
	notes: """
		1.79c — private-repo credential flow (`SourceCredential`,
		ADR 0039). Additive across the board:

		  * new `sealed-secrets` component (bitnami controller +
		    its `SealedSecret` CRD) — the Tier-1 secret backend;
		  * new `SourceCredential` CRD + controller in the
		    operator image (derives prefix-matched Argo
		    `repo-creds` and host-matched workload pull-secrets
		    from sealed material) — operator v0.1.134 → v0.1.135;
		  * new admission validator for `SourceCredential`.

		No component values reshaped, no data migration, nothing
		removed — safe to apply via Argo CD automated sync.
		"""
	references: ["ADR 0039", "plan.md 1.79c"]
}
compatibility: "0.1.48": {
	change:          "safe"
	operatorVersion: "v0.1.134"
	notes: """
		Walk-fix #11 post-B.1.79b-Part-3b — CMP entrypoint
		runs `cue export` from inside the `apprafter/`
		package directory (cue-cmp v0.1.6).

		**Symptom.** Operator scaffolded a user app
		(`apprafter app scaffold`), which now vendors the
		AppRafter v1alpha1 schemas as a `cue.mod/` INSIDE
		`apprafter/` (walk-fix #5 post-Part-3b — typed CUE
		without external module setup). After pushing the
		manifest and hard-refreshing, Argo CD surfaced:

			```
			plugin sidecar failed. error generating manifests
			in cmp: ... `sh -c /usr/local/bin/entrypoint.sh`
			failed exit status 1: ::cue-cmp:: CUE compile
			failed: cue: "./..." matched no packages
			```

		**Root cause.** A nested `cue.mod/` defines a CUE
		MODULE BOUNDARY. Argo CD sets the CMP working
		directory to the Application's `spec.source.path`
		(repo-root-relative). For a scaffolded repo with the
		manifest at `apprafter/Application.cue` and
		`source.path` = repo root, the entrypoint ran
		`cue export ./...` from the repo root — which does
		NOT descend into the nested `apprafter/` module,
		hence "matched no packages". (Local `cd apprafter &&
		cue vet` worked, which is why the scaffold smoke
		didn't catch it — the smoke ran from inside the
		package dir.)

		**Fix.** The entrypoint now resolves the package
		directory before `cue export`, mirroring the
		`discover.find.command` convention:

		  * cwd basename `apprafter` → run here.
		  * else if `./apprafter/` holds `.cue` files → cd
		    into it.
		  * else → run in cwd (filename-prefix / fixture).

		After the cd, `cue export ./...` resolves the module
		by walking UP — so it works whether the `cue.mod/` is
		the vendored one inside `apprafter/` (external
		scaffolded repo) OR a repo-root module shared across
		many apps (the AppRafter monorepo's own landing
		manifests, `spec.source.path` = `landing/web`). The
		`cue.mod/pkg/` dependency cache is excluded from
		`./...`, so the vendored schema package is never
		emitted as a manifest.

		**Self-healing.** Existing registrations work after
		the image upgrade without re-registering, regardless of
		the `spec.source.path` they were created with —
		`apprafter platform upgrade --to 0.1.48` +
		`kubectl rollout restart deployment/argocd-repo-server
		-n argocd` + hard-refresh the affected apps.

		**Regression guard.** `argocd-cue-cmp-check.yml`
		gains a second entrypoint smoke that builds the exact
		scaffold layout (apprafter/ + nested cue.mod with
		vendored schemas + a typed manifest that `import`s
		`apprafter.io/schemas/v1alpha1`), runs the entrypoint
		with cwd = the PARENT, and asserts it cds in and
		renders the typed manifest. Before this fix the new
		smoke fails with "matched no packages".

		**Rendered chart vs 0.1.47.** cue-cmp sidecar image
		pin flips v0.1.5 → v0.1.6 (entrypoint.sh change baked
		into the image; the ConfigMap-mounted plugin.yaml is
		unchanged). argocd-repo-server's sidecar rotates on
		chart upgrade.
		"""
	references: [
		"docs/adr/0029-cue-cmp.md",
		"argocd-cue-cmp/entrypoint.sh",
		".github/workflows/argocd-cue-cmp-check.yml",
		"docs/changelog/UNRELEASED.md",
	]
}

// Walk-fix #11 post-B.1.79a — `Namespace` cluster resource
// added to `apps` AppProject's `clusterResourceWhitelist`.
// Wizard-generated user apps carry `syncOptions:
// CreateNamespace=true`, so Argo CD generates a synthetic
// `Namespace` resource for the destination namespace; the
// previous empty whitelist blocked it with `SyncFailed:
// resource :Namespace is not permitted in project apps`.
// Permission narrows to `Namespace` only — everything else
// cluster-scoped stays denied (Phase 4 AccessGrant will
// layer real RBAC on top later).
compatibility: "0.1.47": {
	change:          "safe"
	operatorVersion: "v0.1.134"
	notes: """
		Walk-fix #11 post-B.1.79a — `Namespace` cluster
		resource permission for `apps` AppProject.

		**Symptom.** After walk-fix #10 (chart 0.1.46) fixed
		CMP discover and landing apps got past `package.json`
		fallback, sync still failed:

			```
			SyncFailed
			resource :Namespace is not permitted in project apps
			```

		**Root cause.** Wizard (`apprafter app add`) sets
		`destination.namespace` to the app name (e.g.
		`apprafter-landing-web`) and ships `syncOptions:
		[CreateNamespace=true, ServerSideApply=true]`. Argo
		CD on first sync generates a synthetic `Namespace`
		resource for the destination namespace and tries to
		apply it as a cluster-scoped resource. The `apps`
		AppProject's `clusterResourceWhitelist: []` blocked
		every cluster resource, including Namespace, so the
		sync attempt errored out before any namespaced
		resource was applied.

		**Fix.** Narrow allow-list for `apps`:

			```cue
			apps: #AppProjectSpec & {
			  ...
			  clusterResourceWhitelist: [{
			    group: ""
			    kind:  "Namespace"
			  }]
			  ...
			}
			```

		Only `Namespace` from the core API group. Nothing
		else cluster-scoped (no CRD installs, no
		ClusterRole/Binding, no PersistentVolume) reaches
		the user-app surface. Phase 4 AccessGrant layers
		runtime RBAC on top of this structural foundation.

		**Caveat — orphan destination namespace.** The
		wizard's defaulting of `destination.namespace` to
		the app name (`apprafter-landing-web`) is mismatch'ed
		with the manifest's actual `metadata.namespace`
		(typically `apprafter` for landing apps). After this
		walk-fix, sync succeeds:

		  1. Argo CD creates `Namespace/apprafter-landing-
		     web` (now permitted).
		  2. Applies the Application CR to its manifest
		     namespace (`apprafter`).
		  3. AppRafter operator reconciles, lays down
		     Deployment + Service in `apprafter`.

		Result: `apprafter-landing-web` namespace exists
		but is empty. Harmless but ugly — wizard polish
		(separate walk-fix) should either drop
		`CreateNamespace=true` for user apps or read
		manifest's metadata.namespace at registration time.

		**Rendered chart vs 0.1.46.** Only the `apps`
		AppProject's `clusterResourceWhitelist` flips from
		`[]` to `[{group: "", kind: "Namespace"}]`. No other
		structural changes.
		"""
	references: [
		"docs/changelog/UNRELEASED.md",
		"platform-stack/cue/app_projects.cue",
	]
}

// Walk-fix #10 post-B.1.79a — discover stdout fix in CMP
// plugin.yaml. v0.1.4's snippet exited 0 on match but printed
// nothing (`| grep -q .` swallowed find's output). Argo CD's
// MatchRepository requires non-empty stdout to claim the repo,
// so the cue plugin never engaged; landing apps fell back to
// directory mode and choked on `package.json` exactly as in
// walk-fix #8. Drop `| grep -q .`, let `find -print -quit`
// speak for itself. Image bumps v0.1.4 → v0.1.5; chart pulls
// the new tag via `_components.argocd-cue-cmp.values.image.
// tag = "v" + argocdcuecmp.version`.
compatibility: "0.1.46": {
	change:          "safe"
	operatorVersion: "v0.1.134"
	notes: """
		Walk-fix #10 post-B.1.79a — CMP discover stdout fix
		(cue-cmp v0.1.4 → v0.1.5, chart 0.1.45 → 0.1.46).

		**Symptom.** Operator on chart 0.1.45 (which carried
		walk-fix #8's command-based discover plus the v0.1.4
		image rebuild from #8b) still saw landing apps fail
		with the original `Failed to unmarshal "package.json":
		Object 'Kind' is missing` error (cached). After
		`kubectl exec deployment/argocd-redis -- redis-cli
		FLUSHDB` and a `argocd.argoproj.io/refresh=hard`
		annotation on each Application, the error reproduced
		identically — i.e. not a stale cache, the CMP plugin
		was actually never matching.

		**Diagnosis.** `kubectl -n argocd logs deployment/
		argocd-repo-server -c cue-cmp` showed the discover
		shell snippet running, exiting 0, but every entry
		followed by:

			```
			level=warning msg="Plugin command returned zero
			output" command="{[sh -c if [ ... ] ; then find
			. -maxdepth 1 ... -print -quit | grep -q .
			else find . -type f -name '*.cue' \\( -path '*/
			apprafter/*' -o -name 'apprafter*.cue' \\) -print
			-quit | grep -q . fi] []}" execID=... stderr=
			```

		Argo CD's `MatchRepository` in `cmpserver/plugin/
		plugin.go` keys on **stdout emptiness**, not exit
		code. `grep -q` is silent (the `-q` flag suppresses
		stdout entirely), so even when the find command
		matched a real `landing/web/apprafter/Application.
		cue`, no output reached Argo CD's runCommand return
		value, the plugin was marked `IsSupported: false`,
		and the directory-mode fallback re-fired the
		`package.json` parse.

		**Fix.** Drop the `| grep -q .` pipe entirely.
		`find -print -quit` already prints the first
		matched path to stdout on hit and prints nothing on
		miss (both code paths exit 0). Stdout emptiness IS
		the signal Argo CD reads. The discover snippet is
		now:

			```sh
			if [ "$(basename "$PWD")" = "apprafter" ]; then
			  find . -maxdepth 1 -type f -name '*.cue' \\
			      -print -quit
			else
			  find . -type f -name '*.cue' \\
			      \\( -path '*/apprafter/*' \\
			         -o -name 'apprafter*.cue' \\) \\
			      -print -quit
			fi
			```

		Image bumps `argocd-cue-cmp` v0.1.4 → v0.1.5
		(plugin.yaml-only change but baked into the
		container's `/home/argocd/cmp-server/config/
		plugin.yaml` per the Dockerfile; chart-managed
		ConfigMap mount overlays it at runtime so existing
		installs pick up the fix without an image roll, but
		the published v0.1.5 image is right for installers
		that bypass the chart).

		**Regression guard.** New `argocd-cue-cmp/test-
		discover.sh` extracts the discover shell snippet
		from the in-tree `plugin.yaml`, runs it against
		fixture directories that exercise both conventions
		(`apprafter/Application.cue` sibling, `apprafter*.
		cue` filename prefix, cwd-is-apprafter, plain repo
		with no `.cue` files), and asserts stdout non-
		emptiness matches the expected match/no-match
		signal. Wired into `argocd-cue-cmp-check.yml` so
		every PR touching plugin.yaml must keep this
		discipline.

		**Rendered chart vs 0.1.45.** Sidecar image tag
		flips `v0.1.4` → `v0.1.5`. ConfigMap `cue-cmp-
		plugin-config` content updates the inline shell
		snippet (drops `| grep -q .` from both branches).
		"""
	references: [
		"docs/adr/0029-cue-cmp.md",
		"argocd-cue-cmp/plugin.yaml",
		"argocd-cue-cmp/test-discover.sh",
		"docs/changelog/UNRELEASED.md",
	]
}

compatibility: "0.1.45": {
	change:          "safe"
	operatorVersion: "v0.1.134"
	notes: """
		Walk-fix #8b — cue-cmp image bump to v0.1.4.

		Follow-up to walk-fix #8 (chart 0.1.44) which
		changed plugin.yaml content without a matching cue-cmp
		image version bump. cue-cmp-check CI workflow's
		drift detection flagged it red:

			```
			Error: Image source under argocd-cue-cmp/ changed
			since argocd-cue-cmp/v0.1.3 was published, but
			version.cue is still 0.1.3.
			```

		The drift policy is right — an operator who installs
		cue-cmp manually (without the chart's ConfigMap
		overlay) pulls the image and runs against its baked-
		in plugin.yaml. Stale image content = wrong behaviour
		for that install path. Bumping the image version
		alongside source changes keeps the ghcr tag honest.

		Image rebuild: cue-cmp v0.1.3 → v0.1.4. Same
		Dockerfile, same entrypoint.sh, new plugin.yaml
		(with command-based discover from walk-fix #8). The
		cue-cmp publish workflow auto-fires on the new tag.

		Chart 0.1.44 → 0.1.45 follows the cue-cmp pin
		via `_components.argocd-cue-cmp.values.image.tag =
		"v" + argocdcuecmp.version`. Operator on 0.1.44
		can upgrade to 0.1.45 directly — argocd-repo-server
		Deployment's sidecar image reference flips from
		v0.1.3 → v0.1.4 and kubelet rolls the pod.

		Rendered chart vs 0.1.44: sidecar image tag pin
		flipped. plugin.yaml ConfigMap content unchanged
		(same command-based discover from walk-fix #8).
		"""
	references: [
		"docs/adr/0029-cue-cmp.md",
		".github/workflows/argocd-cue-cmp-check.yml",
		"docs/changelog/UNRELEASED.md#v01158",
	]
}

// Walk-fix #8 post-B.1.79a — CMP discover switches from glob
// to command. Chart 0.1.43 shipped `discover.find.glob:
// "{**/apprafter*.cue,**/apprafter/**/*.cue}"` — brace
// alternation that doublestar v4 supports. Argo CD 2.13.1's
// vendored doublestar either treats the braces literally or
// never received the v4 brace support — operator's actual
// cluster returned no-match on `landing/cms/apprafter/
// Application.cue`, Argo CD fell back to default directory
// mode, choked on `package.json`. New shape: command-based
// discovery using `find` with an inline shell snippet,
// supports both path-is-parent-of-apprafter and
// path-is-apprafter-itself conventions, no doublestar
// dependency.
//
// Image unchanged (cue-cmp stays at v0.1.3) — the fix is
// entirely in the chart-managed ConfigMap content.
compatibility: "0.1.44": {
	change:          "safe"
	operatorVersion: "v0.1.134"
	notes: """
		Walk-fix #8 post-B.1.79a — CMP discover switches
		from glob to command for cross-doublestar-version
		robustness.

		**Symptom.** Operator on chart 0.1.43 registered
		landing apps via `apprafter app add --path
		landing/cms`; Argo CD UI surfaced:

			```
			Failed to load target state: failed to generate
			manifest for source 1 of 1: rpc error: code =
			FailedPrecondition desc = Failed to unmarshal
			"package.json": Object 'Kind' is missing
			```

		**Root cause.** Chart 0.1.42-0.1.43 plugin.yaml used
		`discover.find.glob: "{**/apprafter*.cue,**/
		apprafter/**/*.cue}"` — brace alternation that
		doublestar v4 supports. Argo CD 2.13.1's vendored
		doublestar either treats the braces literally or
		never received the v4 brace support. CMP returned
		no-match for both apps; Argo CD fell back to default
		directory mode, walked `landing/cms/`, and tried to
		parse `package.json` as a k8s manifest.

		Diagnostics confirmed: configmap had the new glob,
		repo-server's `MatchRepository` gRPC calls returned
		OK (success) with no match (silent — the protocol
		has no "explicit no-match" log entry, only "matched"
		activity downstream).

		**Fix.** Switch `discover.find.glob` to `discover.
		find.command`. The command runs from the path's
		directory; exit 0 means match, non-zero means no
		match. Inline shell handles both convention shapes
		in one expression:

			```sh
			if [ "$(basename "$PWD")" = "apprafter" ]; then
			  find . -maxdepth 1 -type f -name "*.cue" \\
			      -print -quit | grep -q .
			else
			  find . -type f -name "*.cue" \\
			      \\( -path "*/apprafter/*" \\
			         -o -name "apprafter*.cue" \\) \\
			      -print -quit | grep -q .
			fi
			```

		* cwd basename `apprafter` (operator pointed path
		  directly at convention dir) → match any `.cue`
		  file at depth 1.
		* otherwise → match any `.cue` file inside an
		  `apprafter/` subdirectory OR with a filename
		  starting `apprafter`. Covers both path = parent
		  (`landing/cms`) and filename-prefix (`apprafter-
		  web.cue`) conventions.

		`-print -quit | grep -q .` short-circuits after the
		first match — discovery doesn't need a full scan,
		just a yes/no signal.

		**Image unchanged.** cue-cmp Docker image stays at
		v0.1.3; the entire fix lives in the chart-managed
		`cue-cmp-plugin-config` ConfigMap content. Upgrading
		the chart to 0.1.44 + restarting `argocd-repo-server`
		deployment is sufficient to pick up the new
		discovery rule.

		**Wizard polish bundled.** `apprafter app add`
		wizard's `detect_path_relative_to_repo_root` now
		strips a trailing `apprafter` segment from the
		suggested path — operator running the command from
		inside `landing/cms/apprafter/` gets default
		`landing/cms`. (CMP would handle either case with
		the new command-based discover, but the parent
		form is the cleaner convention.)

		Rendered chart vs 0.1.43: ConfigMap `cue-cmp-plugin-
		config` content changed (glob → command stanza).
		Other manifests byte-identical.
		"""
	references: [
		"docs/adr/0029-cue-cmp.md",
		"argocd-cue-cmp/plugin.yaml",
		"docs/changelog/UNRELEASED.md#v01157",
	]
}

// Walk-fix #5b post-B.1.79a — argocd-cue-cmp entrypoint
// accepts both source-layout styles. Walk-fix #5 (chart
// 0.1.42, cue-cmp v0.1.2) added jq-based enumeration of
// top-level k8s-shaped values for source files using the
// named-wrapper convention (`landingWeb: v1alpha1.
// #Application & { ... }`). Argo CD's own CMP-check
// smoke test fixture, however, uses the **unwrapped**
// convention — `apiVersion + kind + metadata + spec`
// declared directly at package scope:
//
//   ```cue
//   package app
//   apiVersion: "apprafter.io/v1alpha1"
//   kind:       "Application"
//   metadata: name: "hello"
//   spec: image: "..."
//   ```
//
// For style A the top-level JSON itself IS the manifest;
// the v0.1.2 entrypoint's key-enumeration logic skipped
// it because `apiVersion` / `kind` themselves aren't
// objects-with-`apiVersion`+`kind` — fell through to the
// "no manifests" branch, smoke test failed with
// `entrypoint did not render Application kind`.
//
// Fix: dispatch on whether the top-level JSON object
// carries `apiVersion` + `kind`. Style A — emit `cue
// export ./... --out yaml` verbatim. Style B — fall
// through to the existing per-key enumeration logic.
//
// argocd-cue-cmp v0.1.2 → v0.1.3. Chart pin tracks via
// the CUE import.
compatibility: "0.1.43": {
	change:          "safe"
	operatorVersion: "v0.1.134"
	notes: """
		Walk-fix #5b — entrypoint accepts both unwrapped
		and named-wrapper CUE source layouts.

		**Symptom.** v0.1.42 chart + cue-cmp v0.1.2 CI
		`argocd-cue-cmp-check` smoke test failed:

			```
			Error: entrypoint did not render Application kind
			Exception: Process completed with exit code 1.
			```

		**Root cause.** The smoke fixture writes apiVersion,
		kind, metadata, spec directly at package scope —
		CUE export emits the whole package as ONE flat JSON
		object. v0.1.2 entrypoint's logic walked
		`to_entries[]` looking for nested objects-with-
		apiVersion+kind; `apiVersion` and `kind` themselves
		weren't objects, so the filter rejected everything
		and the loop exited without emitting anything.

		**Fix.** Dispatch before enumeration:

		  ```sh
		  is_top_level_manifest=$(jq -r '
		    if (type == "object" and has("apiVersion") and
		        has("kind"))
		    then "yes" else "no" end' "$json_out")
		  if [ "$is_top_level_manifest" = "yes" ]; then
		    echo "---"
		    cue export ./... --out yaml
		    exit 0
		  fi
		  # else fall through to per-key enumeration ...
		  ```

		Style A — single manifest, emit verbatim. Style B
		(`landingWeb: …`) — existing per-key logic. Both
		verified locally against `/tmp/cue-smoke/apprafter/
		Application.cue` (style A) and
		`landing/web/apprafter/` (style B); each emits
		well-formed YAML stream with `apiVersion:` at the top
		level of every doc.

		**Image bump.** `argocd-cue-cmp/version.cue` v0.1.2
		→ v0.1.3; chart pin follows automatically.

		Rendered chart vs 0.1.42: image tag pin bumped from
		v0.1.2 → v0.1.3. No other manifest changes.
		"""
	references: [
		"docs/adr/0029-cue-cmp.md",
		"argocd-cue-cmp/entrypoint.sh",
		"docs/changelog/UNRELEASED.md#v01153",
	]
}

// Walk-fix #5 post-B.1.79a — argocd-cue-cmp emits flat k8s
// manifest stream. Chart 0.1.41 shipped the cue-cmp sidecar
// embedded in the argocd subchart, but the sidecar's
// entrypoint script ran `cue export ./... --out yaml` raw —
// which for typical apprafter manifests (`landingWeb:
// v1alpha1.#Application & { ... }`) outputs a nested YAML
// document with the manifest under the field key, not the flat
// document Argo CD expects. Plus the discovery glob
// (`**/apprafter*.cue`) only matched files starting with
// "apprafter", missing the `apprafter/<resource>.cue`
// directory-as-marker convention AppRafter's own landing
// manifests use (`landing/web/apprafter/Application.cue`).
//
// Fix lands across three artefacts:
//   * `argocd-cue-cmp/entrypoint.sh` — enumerates top-level
//     k8s-shaped values (objects with `apiVersion` + `kind`)
//     via `cue export --out json | jq`, re-exports each via
//     `cue export -e <key> --out yaml`, separates each with
//     `---`.
//   * `argocd-cue-cmp/Dockerfile` — adds `jq` to the image
//     for the key enumeration step (~600 KiB).
//   * `argocd-cue-cmp/plugin.yaml` (and the chart's embed
//     of the same in `component_argocd.cue`'s extraObjects)
//     — extends glob to `{**/apprafter*.cue,**/apprafter/**/*.cue}` via doublestar v4 brace alternation.
//
// argocd-cue-cmp image bumps v0.1.1 → v0.1.2. Chart points
// `_components.argocd-cue-cmp.values.image.tag` to the new
// version through `argocd-cue-cmp/version.cue` (single SoT
// for the image tag — chart pin tracks automatically).
compatibility: "0.1.42": {
	change:          "safe"
	operatorVersion: "v0.1.134"
	notes: """
		Walk-fix #5 post-B.1.79a — argocd-cue-cmp emits
		flat k8s manifest stream + accepts `apprafter/`
		directory-marker layout.

		**Symptom.** Operator runs `apprafter app add` against
		the AppRafter monorepo pointing at
		`landing/web/apprafter/Application.cue`. Two failure
		modes:

		  1. CMP discovery glob `**/apprafter*.cue` doesn't
		     match the file (basename is `Application.cue`).
		     Argo CD falls through to default raw-YAML
		     handling, finds nothing, syncs the Application
		     with zero rendered manifests.

		  2. Even with a matching filename (e.g.
		     `apprafter-web.cue` workaround), the entrypoint's
		     `cue export ./... --out yaml` emits the
		     manifest nested under a top-level field name
		     (`landingWeb: …`). Argo CD treats that as ONE
		     invalid document — no `apiVersion` at the top
		     level → sync fails.

		**Root cause.** Both behaviours were unsurfaced
		because no user app actually exercised the CMP path
		end-to-end before — the sidecar was wired into the
		chart in B.1.69 but nothing reached its render step.
		The landing-stream Application manifests are the
		first real test.

		**Fix.** Three coordinated artefact updates:

		  * `argocd-cue-cmp/entrypoint.sh` rewritten to
		    enumerate top-level k8s-shaped values via `cue
		    export --out json | jq`, re-export each via `cue
		    export -e <key> --out yaml`, emit a `---`
		    separator per doc. Filter: only values whose JSON
		    is an object and has both `apiVersion` and `kind`
		    fields — helper top-level values (shared
		    constants, library defs) are silently skipped.

		  * `argocd-cue-cmp/Dockerfile` adds `jq` to the
		    Alpine base layer (~600 KiB) for the enumeration
		    step.

		  * `argocd-cue-cmp/plugin.yaml` extends glob to
		    `{**/apprafter*.cue,**/apprafter/**/*.cue}` via
		    doublestar v4 brace alternation. Now matches both
		    filename-prefix convention (`apprafter-web.cue`)
		    and directory-marker convention
		    (`landing/web/apprafter/Application.cue`).

		  * `platform-stack/cue/component_argocd.cue` mirrors
		    the new glob inside the embedded `plugin.yaml`
		    `extraObjects` ConfigMap that the chart applies to
		    the argocd-repo-server sidecar.

		**Image bump.** `argocd-cue-cmp/version.cue` v0.1.1
		→ v0.1.2; chart's
		`_components.argocd-cue-cmp.values.image.tag` follows
		automatically via the CUE import.

		**Verification.** Local `bash argocd-cue-cmp/entrypoint.sh` against `landing/web/apprafter/` emits two
		well-formed YAML docs (landing-web + landing-web-
		preview), each with `apiVersion: apprafter.io/v1alpha1`
		at the top level and a `---` separator between them.

		Rendered chart vs 0.1.41: the embedded plugin.yaml
		ConfigMap's `data."plugin.yaml"` string changed; the
		image tag pin in argocd subchart values bumped from
		v0.1.1 → v0.1.2. No other manifest changes.
		"""
	references: [
		"docs/adr/0029-cue-cmp.md",
		"argocd-cue-cmp/entrypoint.sh",
		"argocd-cue-cmp/plugin.yaml",
		"docs/changelog/UNRELEASED.md#v01152",
	]
}

// Walk-fix #2 post-B.1.79a — AppProjects as standalone umbrella
// manifests at sync-wave -30. Chart 0.1.40 shipped 4 AppProjects
// via the argocd subchart's `configs.projects` mechanism, which
// renders them only when the argocd Application syncs at sync-
// wave -15. Child Applications at sync-wave 0 (admission-webhook,
// apprafter-operator, etc.) reference `spec.project: platform`
// but Argo CD does NOT strictly serialise sync-waves across
// Applications in the app-of-applications pattern — these
// children sometimes try to apply before wave -15 finishes,
// triggering `Unable to refresh admission-webhook: app is not
// allowed in project "platform", or the project does not exist`.
//
// Fix: the umbrella chart itself emits AppProject CRs at sync-
// wave -30 — before Cilium at -20 — via a new
// `templates/appprojects.yaml` template iterating
// `.Values.appProjects`. `configs.projects` in the argocd
// subchart stays for initial loader install (when no umbrella
// exists yet). Both sites consume the new shared `_appProjects`
// map (`app_projects.cue`), so definitions are byte-identical
// and idempotent — Argo CD's reconciler treats both as the same
// logical resource.
compatibility: "0.1.41": {
	change:          "safe"
	operatorVersion: "v0.1.134"
	notes: """
		Walk-fix #2 post-B.1.79a — AppProjects as
		standalone umbrella manifests.

		**Symptom.** Operator upgrades from chart 0.1.39 →
		0.1.40. Child Application `admission-webhook` (sync-
		wave 0) gets reapplied with `spec.project: platform`
		(new default after part 1). Argo CD UI surfaces
		refresh errors:

			```
			Unable to refresh admission-webhook: app is not
			allowed in project "platform", or the project
			does not exist
			```

		**Root cause.** Chart 0.1.40 shipped 4 AppProject
		definitions via `_loaderValues.argocd.values.configs.
		projects`. The argocd subchart renders these as
		AppProject CRs only when it syncs at sync-wave -15.
		Argo CD's app-of-applications model does NOT strictly
		serialise sync-waves between separate Applications —
		only within a single Application's child-resource
		sync. Result: admission-webhook (wave 0) and argocd
		(wave -15) race; the former sometimes tries to refresh
		before the AppProject `platform` has landed.

		**Fix.** Ship AppProject manifests as part of the
		umbrella chart itself, at sync-wave -30 (earlier than
		even Cilium's -20). New template
		`templates/appprojects.yaml` iterates `.Values.
		appProjects` and emits one `kind: AppProject` per
		entry. New shared `_appProjects` map in
		`app_projects.cue` is consumed by both the loader
		(`_loaderValues.argocd.values.configs.projects`) and
		the umbrella template — definitions are byte-identical
		and idempotent. Argo CD's reconciler treats the two
		render sites as the same resource (same
		group/kind/name/namespace).

		**Why both sites stay.** The argocd subchart's
		`configs.projects` covers the initial `apprafter
		cluster-bootstrap` install — before any umbrella sync
		has run. The umbrella's manifest renderer covers every
		subsequent sync. Removing the loader path would mean
		an initial cluster has no AppProjects until the first
		umbrella sync completes — a regression vs 0.1.40.

		**Workaround for 0.1.40 operators (not needed on
		0.1.41+).** Apply the four AppProjects manually with
		`kubectl apply -f` — Argo CD picks them up immediately
		and the refresh-error condition clears on the next
		reconcile.

		Rendered chart vs 0.1.40: new `templates/appprojects.
		yaml` template (additive — no existing template
		changed). values.yaml grows a new top-level
		`appProjects` map (4 entries, byte-identical to
		`configs.projects` in the argocd subchart values).
		Operator binary unchanged.
		"""
	references: [
		"docs/adr/0025-argo-cd.md",
		"docs/adr/0026-platformstack-crd.md",
		"plan.md#179a-cli-apprepo-subcommands--appprojects",
		"docs/changelog/UNRELEASED.md#v01146",
	]
}

// Track B.1.79a (part 1) — AppProjects + per-component project
// field. Chart adds three new AppProject resources alongside
// the existing `default` one: `platform` (chart-internal
// components), `platform-providers` (Phase 2+ ServiceProviders),
// `apps` (user Applications via `apprafter app add`). All
// existing components transition `spec.project: default → platform`
// on the next sync; CLI loader's root platform
// Application also moves to `platform`. **Safe class** —
// AppProject change on existing Application is a metadata-only
// drift handled by Argo CD as a normal sync (no pod restart,
// no resource churn).
compatibility: "0.1.40": {
	change:          "safe"
	operatorVersion: "v0.1.134"
	notes: """
		Track B.1.79a part 1 — AppProjects + per-component
		project field.

		Chart surface:

		* `_loaderValues.argocd.values.configs.projects` gains
		  three AppProject entries: `platform`,
		  `platform-providers`, `apps`. Existing `default`
		  retained as legacy + ad-hoc fallback. All four
		  projects ship in the initial Argo CD install so
		  they exist before any Application
		  that references them syncs for the first time.

		* `#Component.project: string | *"platform"` (new
		  optional field with default). render_tool.cue
		  template emits `spec.project: {{ component.project
		  | default "platform" }}` per Application. All
		  current components (cilium, argocd, cert-manager,
		  network-policies, apprafter-operator,
		  admission-webhook, backstage, argocd-cue-cmp)
		  inherit the default → land in `platform` project.

		CLI surface (binary v0.1.137):

		* `cluster_bootstrap::render_root_application` —
		  bootstrap "platform" Application teraz writes
		  `spec.project: platform` instead of `default`.
		  Safe because AppProject ships in the initial
		  Argo CD install (see above).

		Upgrade impact: existing operators upgrading
		0.1.39 → 0.1.40 see every chart-managed
		Application drift `spec.project` from `default`
		to `platform`. Argo CD reconciles via the normal
		sync path; pod-level workload unaffected. The
		root platform Application also drifts (CLI loader
		re-renders on next `apprafter cluster-bootstrap`
		or `bootstrap-all` invocation).

		RBAC and AccessGrant enforcement via AppProject
		is not activated in M1.5 — Phase 4 materialises it
		via the AccessGrant CRD. Projects currently serve
		a visual role (the UI selector in Argo CD groups
		Applications by project), plus lay the structural
		foundation for future enforcement.

		Rendered chart vs 0.1.39: only `values.yaml`
		`configs.projects` map grows from 1 → 4 entries;
		`templates/applications.yaml` template gains a
		`{{ default "platform" $component.project | quote }}`
		render. Operator + webhook binaries unchanged.
		"""
	references: [
		"docs/adr/0025-argo-cd.md",
		"docs/adr/0026-platformstack-crd.md",
		"plan.md#179a-cli-apprepo-subcommands--appprojects",
		"docs/changelog/UNRELEASED.md#v01137",
	]
}

// Track B.1.79 — CLI thin wrappers + Argo CD MigrationPlan
// Lua action. Operator binary unchanged (still v0.1.134); CLI
// `apprafter` binary gains `platform`/`migration`/`open argocd`
// subcommands and a npm-style newer-release courtesy banner.
// Chart delta is a single Lua resource-action block for the
// MigrationPlan CR in `configs.cm` — Argo CD's UI Approve /
// Reject buttons mirror the CLI verbs (ADR 0027 webhook
// denial of application-scope rejects surfaces in the UI
// the same way it does on the CLI).
compatibility: "0.1.39": {
	change:          "safe"
	operatorVersion: "v0.1.134"
	notes: """
		Track B.1.79 — CLI thin wrappers + Argo CD
		MigrationPlan resource-action buttons.

		CLI surface (out-of-chart, binary v0.1.135):

		* `apprafter platform status` — reads
		  `PlatformStack/default` via kubectl shellout,
		  prints channel / pin / autoUpgrade / current /
		  target / available versions + conditions table +
		  recent versionHistory (last 5).
		* `apprafter platform upgrade --to <v>` — merge-
		  patches `spec.pin: <v>`. Without `--to` — clears pin
		  and flips autoUpgrade=true (channel-following).
		* `apprafter migration list` — table of
		  MigrationPlans in apprafter-system (name / scope /
		  classification / phase).
		* `apprafter migration approve <name>` /
		  `reject <name>` — patches `status.phase` via
		  the `status` subresource. Application-scope
		  rejects denied by the existing webhook (ADR 0027)
		  with the verbatim apiserver message.
		* `apprafter open argocd` — spawns kubectl port-
		  forward to `svc/argocd-server -n argocd 8080:443`,
		  prints credentials, opens default browser (xdg-
		  open / open / cmd), blocks until Ctrl+C.
		* Every invocation runs a fail-quiet npm-style
		  newer-release check (24h cache in
		  `~/.cache/apprafter/version-check.json`).

		Chart surface (this entry):

		* `configs.cm.resource.customizations.actions.
		  apprafter.io_MigrationPlan` — Argo CD Lua
		  resource-action block. Discovery disables
		  Approve / Reject once `status.phase` leaves
		  `pending-approval`; action bodies mutate
		  `status.phase` to `approved` / `rejected`. Argo
		  CD routes the mutation via the status
		  subresource automatically; webhook denial of
		  application-scope rejects surfaces in the UI
		  verbatim.

		Rendered chart vs 0.1.38: a single new entry
		under `configs.cm` in `values.yaml`; all
		templates byte-equivalent. No operator change.
		"""
	references: [
		"docs/adr/0027-migrationplan-unified.md",
		"plan.md#179-cli-thin-wrappers",
		"docs/changelog/UNRELEASED.md#v01135",
	]
}

// Walk-fix #8 post-B.1.78 — PlatformController's destructive
// classification was per-target-version (single record's
// `change` field), not per-transition. Version jumps that
// span an intermediate destructive version silently bypassed
// the gate if the target itself was safe. Fix: replace
// `fetch_change_class(url, target)` with
// `fetch_path_max_change_class(url, from, to)` — pulls the
// compat doc and reduces to the MOST DESTRUCTIVE class in the
// half-open range `(from, to]`. Operator-binary change → image
// v0.1.133 → v0.1.134 via standard chart appVersion lockstep.
compatibility: "0.1.38": {
	change:          "safe"
	operatorVersion: "v0.1.134"
	notes: """
		Walk-fix #8 post-B.1.78 — path-aware destructive
		classification.

		**Symptom (theoretical, demonstrated via reasoning
		on 2026-05-23 walk).** Cluster on 0.1.35 with
		walk-platform-1's `platform-0-1-35-to-0-1-36`
		MigrationPlan in rejected phase. Chart 0.1.37
		published as `safe`. autoUpgrade detects channel-
		latest=0.1.37 and tries bump 0.1.35 → 0.1.37.
		Plan name `platform-0-1-35-to-0-1-37` doesn't
		exist (different pair). `fetch_change_class`
		returns Safe for 0.1.37's single record. Straight
		bump to 0.1.37 — silently jumping over 0.1.36's
		breaking content and bypassing operator's reject
		decision.

		**Root cause.** Classification was per-target-
		version, not per-transition. spec.md §3.11
		semantics implied path-aware ("any path to 0.1.37
		must respect the strictest class encountered"),
		but code looked up only the destination record.

		**Fix.** New `fetch_path_max_change_class(url,
		from, to)` reads the compat doc at `to`'s tarball
		and reduces the records in `(from, to]` to their
		strictest class via `path_max_change_class(doc,
		from, to)` pure helper. Reconcile body's
		destructive check replaces single-target call.

		Edge cases:

		* `from == to` (no-op transition) → Safe.
		* `from > to` (downgrade) → Safe. spec.md doesn't
		  address downgrade destructiveness; conservative
		  default. Future work can extend semantics if a
		  real use case surfaces.
		* Unparseable version key in doc → skipped without
		  affecting other entries' contribution to the
		  max.

		Classification ordering (Safe < RequiresRestart <
		DataMigration < Breaking) lives inside the module
		via `class_order` helper rather than exposed on
		`ChangeClass` itself — only path_max needs it.

		**Tests.** +8 unit tests in
		`operator-controllers/platform-stack/compatibility.rs`:
		* `path_max_change_class_picks_strictest_in_range`
		* `_excludes_from_version`
		* `_returns_safe_for_no_op_transition`
		* `_returns_safe_for_downgrade`
		* `_picks_requires_restart_over_safe`
		* `_picks_data_migration_over_requires_restart`
		* `_picks_breaking_over_data_migration`
		* `_skips_unparseable_version_keys`

		Total platform-stack crate: 68 → 76.

		**Acceptance walk regression coverage.** A
		cluster carrying a rejected
		`platform-0-1-35-to-0-1-36` plan (snapshot.pin=
		null) and pinning autoUpgrade=true should NOT
		auto-bump to 0.1.37 once 0.1.37 publishes. New
		path-max classification surfaces 0.1.36's
		Breaking → PlatformController creates a fresh
		`platform-0-1-35-to-0-1-37` plan and blocks the
		jump. Operator's reject decision on 0.1.36's
		content carries forward — operator must
		explicitly decide whether 0.1.35→0.1.37 (which
		includes 0.1.36's breaking content) is acceptable.

		Rendered chart vs 0.1.37: byte-equivalent
		templates. Operator-binary change only.
		"""
	references: [
		"docs/adr/0027-migrationplan-unified.md",
		"plan.md",
		"docs/changelog/UNRELEASED.md#v01134",
	]
}

// Walk-fix #7 post-B.1.78 — PlatformMigrationStrategy.reject's
// SSA-apply of `spec.pin: null` for channel-following clusters
// failed apiserver validation (`spec.pin must be of type
// string: "null"`), error_policy retried forever, walk-fix #3
// sealing (`status.rejectedAt` write) never ran. Fix: read
// PlatformStack first; if current pin already matches snapshot
// pin (both string-equal or both null/missing), no-op return
// Ok. Otherwise, set-to-string uses SSA force=true (existing
// path); clear-to-null uses JSON merge-patch (RFC 7396 — null
// means "delete this field", works regardless of CRD
// `nullable`). Operator-binary change only → image v0.1.132 →
// v0.1.133 via standard chart appVersion lockstep.
compatibility: "0.1.37": {
	change:          "safe"
	operatorVersion: "v0.1.133"
	notes: """
		Walk-fix #7 post-B.1.78 —
		`PlatformMigrationStrategy.reject` fix for
		channel-following clusters (`snapshot.pin = null`).

		**Symptom.** Walk Phase B.1.78 reject test on
		v0.1.132 chart 0.1.36 — operator approved
		application-scope reject denial earlier (walk-fix
		#2 + #6 OK), but platform-scope reject on
		`platform-0-1-35-to-0-1-36` (cluster channel-
		following, snapshot.pin=null) looped:
		`PlatformStack.apprafter.io "default" is invalid:
		spec.pin: Invalid value: "null": spec.pin in body
		must be of type string`. Error_policy retried
		every 15s; walk-fix #3 sealing's
		`status.rejectedAt` write never landed → plugin
		not sealed → infinite retry loop.

		**Root cause.** Original strategy.reject ALWAYS
		built SSA-apply body with `spec.pin: <snapshot_pin
		or null>`. When snapshot.pin=null, body was
		`{"spec":{"pin":null}}`. CRD's PlatformStack
		schema:
		```yaml
		pin:
		  type: string
		  pattern: "..."
		```
		— `type: string` without `nullable: true` → apiserver
		rejects explicit `null` value as schema
		violation.

		**Fix.** Three-branch dispatch in
		`PlatformMigrationStrategy.reject`:

		1. Read current PlatformStack via Api::get.
		2. Compare current spec.pin vs snapshot.pin via
		   `pins_equal` helper (treats missing /
		   explicit null / both as equivalent).
		3. Dispatch:
		   * Pins equal → no-op return Ok (idempotent;
		     no patch to apiserver). Sealing write fires.
		   * snapshot.pin = Some(String) and pin differs
		     → SSA-apply with force=true (existing path).
		   * snapshot.pin = None / Null and pin is set →
		     JSON merge-patch `{"spec":{"pin":null}}`.
		     RFC 7396 semantics: `null` in merge-patch
		     deletes the field. Works regardless of
		     CRD `nullable`.

		Pre-walk-fix #7 path was always SSA-apply,
		which forced explicit `null` value into the
		field — schema validation rejected. Post-fix,
		clearing via merge-patch routes around the
		schema constraint.

		**Tests.** +4 unit tests in
		`operator-controllers/migration/src/strategy.rs`:
		`pins_equal_treats_missing_and_null_and_explicit_null_as_equivalent`,
		`pins_equal_treats_same_string_as_equal`,
		`pins_equal_distinguishes_different_strings`,
		`pins_equal_distinguishes_null_from_string`.
		Total migration crate: 14 → 18.

		**Sealing side-effect.** With the strategy.reject
		now returning Ok cleanly for null-snapshot
		clusters, the walk-fix #3 sealing path
		(`status.rejectedAt` write) runs to completion.
		Subsequent reconciles see the marker and skip
		strategy.reject — confirmed-fix to the original
		B.1.76 walk-fix #3 logic that was masked by this
		bug.

		Rendered chart vs 0.1.36: byte-equivalent
		templates. Operator-binary change only.
		"""
	references: [
		"docs/adr/0027-migrationplan-unified.md",
		"plan.md",
		"docs/changelog/UNRELEASED.md#v01133",
	]
}

// B.1.78 acceptance walk reject-flow fixture — chart 0.1.36
// classified `breaking` to exercise PlatformController auto-
// create → operator-reject → rejected-plan-blocks-retry
// chain end-to-end. No operator-binary change; same image
// v0.1.132 pinned by chart components 0.1.113. Reject flow
// reads `previousSpecSnapshot.pin` (null since channel-
// following) and SSA-patches `PlatformStack.spec.pin` to it
// (no-op when already null). Walk-fix #3 sealing sets
// `status.rejectedAt` so subsequent operator pod restarts
// don't re-invoke `strategy.reject`. Next PlatformController
// reconcile sees the rejected plan by name + blocks the
// same transition forever; operator clears by deleting
// plan or pinning to explicit version.
compatibility: "0.1.36": {
	change:          "breaking"
	operatorVersion: "v0.1.132"
	notes: """
		Walk fixture — synthetic `breaking` classification
		to exercise B.1.78's reject flow end-to-end.
		Same operator + webhook image (v0.1.132) as
		0.1.34 / 0.1.35; chart content and templates
		byte-equivalent. Classification difference is
		the only test surface.

		Expected walk behavior on cluster running chart
		0.1.35 (autoUpgrade=true, pin=null):

		* PlatformController OCI poll sees channel-
		  latest=0.1.36.
		* `fetch_change_class` returns ChangeClass::Breaking.
		* Plan name synthesizes to
		  `platform-0-1-35-to-0-1-36`.
		* 404 → SSA-create plan with classification=
		  breaking, previousSpecSnapshot.pin=null.
		* parent platform Application's
		  spec.source.targetRevision stays at 0.1.35.
		* Conditions: UpgradeAvailable=True/
		  BlockedByMigrationPlan, MigrationPending=True/
		  breaking.

		Reject flow: `kubectl patch migrationplan
		platform-0-1-35-to-0-1-36 --subresource=status
		--type=merge -p '{"status":{"phase":"rejected"}}'`
		→ webhook allows (platform-scope per ADR 0027) →
		MigrationController reads phase=rejected →
		PlatformMigrationStrategy.reject reads
		previousSpecSnapshot.pin=null + SSA-patches
		spec.pin=null (no-op, already null). Walk-fix #3
		sealing sets `status.rejectedAt` timestamp;
		subsequent operator pod restarts find the marker
		and skip re-invocation of strategy.reject —
		previously this clobbered operator pin changes.

		Sealing verification: PlatformController next
		reconcile finds the rejected plan by name →
		blocks the same transition → MigrationPending=
		True/breaking + UpgradeAvailable=True/
		BlockedByMigrationPlan persist; cluster stays
		on 0.1.35. To clear, operator either deletes the
		plan (re-triggers same destructive transition,
		creates fresh plan in pending-approval) or pins
		to an explicit version different from 0.1.36.

		Rendered chart vs 0.1.35: byte-equivalent
		templates. ONLY classification differs in
		compatibility.yaml.
		"""
	references: [
		"docs/adr/0027-migrationplan-unified.md",
		"plan.md#178-platformcontroller-migrationplan-integration",
	]
}

// B.1.78 acceptance walk fixture — chart 0.1.35 classified
// as `requires-restart` to exercise PlatformController's
// destructive-transition gate end-to-end against a real
// cluster. No operator-binary change; same image v0.1.132
// pinned by chart components. PlatformController sees
// autoUpgrade=true + non-safe classification → creates
// MigrationPlan instead of immediately bumping the parent
// `platform` Application's targetRevision. Walk approves via
// `kubectl patch migrationplan ... --subresource=status` →
// MigrationController completes plan → PlatformController
// next reconcile bumps. Walk verifies acceptance criteria
// from plan.md §1.78.
compatibility: "0.1.35": {
	change:          "requires-restart"
	operatorVersion: "v0.1.132"
	notes: """
		Walk fixture — synthetic `requires-restart`
		classification to exercise the B.1.78
		destructive-transition gate end-to-end. Same
		operator + webhook image (v0.1.132) as 0.1.34;
		chart content and templates byte-equivalent.
		Classification difference is the only test
		surface.

		Per spec.md §3.11: classification ∈
		{requires-restart, data-migration, breaking}
		triggers MigrationPlan creation; classification
		== safe → auto-bump.

		Expected walk behavior on cluster running chart
		0.1.34 (or any 0.1.x with autoUpgrade=true):

		* PlatformController's next OCI poll sees
		  channel-latest=0.1.35.
		* `fetch_change_class` returns
		  ChangeClass::RequiresRestart.
		* Plan name synthesizes to
		  `platform-<current>-to-0-1-35`.
		* `Api::get` of plan in apprafter-system returns
		  404 → create branch fires.
		* SSA-create plan with scope.type=platform,
		  scope.platform.components=[platform-stack],
		  trigger=platform-classification on spec.pin,
		  classification=requires-restart, previousSpecSnapshot.pin
		  = current spec.pin (or JSON null).
		* `target_for_patch = current_target` (no bump).
		* Conditions:
		  UpgradeAvailable=True/BlockedByMigrationPlan
		  with `apprafter-system/<plan>` in message;
		  MigrationPending=True/requires-restart
		  with same plan reference.
		* parent platform Application's
		  spec.source.targetRevision stays at current
		  (NOT 0.1.35).

		Approve flow: `kubectl patch migrationplan
		<name> --subresource=status --type=merge -p
		'{"status":{"phase":"approved"}}'` →
		MigrationController transitions through
		executing → completed → next PlatformController
		reconcile sees plan completed → proceeds with
		bump → parent App's targetRevision to 0.1.35 →
		Argo CD reconciles → operator pod stays on
		v0.1.132 (no image change since chart 0.1.34 +
		0.1.35 both pin v0.1.132).

		Reject flow not exercised by this fixture alone —
		`PlatformMigrationStrategy.reject` (B.1.76)
		reverts `spec.pin` to snapshot. If snapshot.pin
		is null (cluster was channel-following), pin
		stays null and rejected plan's name blocks
		future same-transition attempts; operator
		deletes plan or pins to specific version to retry.

		Rendered chart vs 0.1.34: byte-equivalent
		templates (components, values, RBAC unchanged).
		ONLY classification differs in
		compatibility.yaml. Safe to downgrade back via
		`kubectl patch platformstack default --type=merge
		-p '{"spec":{"pin":"0.1.34"}}'` once acceptance
		walk completes.
		"""
	references: [
		"docs/adr/0027-migrationplan-unified.md",
		"plan.md#178-platformcontroller-migrationplan-integration",
	]
}

// Track B.1.78 closure — PlatformController MigrationPlan
// integration per spec.md §3.11 + ADR 0027. PlatformController
// reconcile loop now:
//
//   * Pre-checks by deterministic `platform-<from>-to-<to>`
//     plan name. If plan exists and phase != `completed`,
//     block bump + surface MigrationPending=True/<class> +
//     UpgradeAvailable=True/BlockedByMigrationPlan
//     conditions with `apprafter-system/<plan-name>` in the
//     message. If plan exists and phase == completed,
//     proceed with bump (operator approved + ran the migration).
//   * No existing plan + classification destructive
//     (`breaking | data-migration | requires-restart` per
//     spec.md §3.11) — CREATE a MigrationPlan CR in
//     `apprafter-system` namespace, block bump, surface
//     conditions.
//   * No existing plan + safe classification — bump as
//     before.
//
// Reject flow (B.1.76 PlatformMigrationStrategy.reject) reads
// previousSpecSnapshot.pin and SSA-patches PlatformStack.spec.pin
// back to it. B.1.78 populates the snapshot during plan
// creation via `current_pin` arg.
//
// Removed: PolicyHooks trait + NoOpHooks. Was a placeholder
// from B.1.73 that never had a real impl; replaced by inline
// plan creation in the reconcile body. operator-controllers/
// platform-stack/src/policy.rs deleted.
//
// RBAC: ClusterRole's migrationplans rule gains `create`
// verb. operator chart's rbac.yaml template extended.
//
// Operator-binary change → image v0.1.131 → v0.1.132 via
// standard chart appVersion lockstep.
compatibility: "0.1.34": {
	change:          "safe"
	operatorVersion: "v0.1.132"
	notes: """
		Track B.1.78 closure — PlatformController
		MigrationPlan integration.

		PlatformController gains a destructive-transition
		gate per spec.md §3.11 + ADR 0027. Before the
		bump cycle's target_for_patch decision, the
		reconciler:

		1. Synthesizes a deterministic plan name from
		   `(from, to)` pair: `platform-<from>-to-<to>`
		   with dots → dashes for DNS-1123.
		2. GETs the named plan in `apprafter-system`.
		   * Exists + `phase=completed` → proceed bump
		     (operator approved + ran the migration).
		   * Exists + any other phase (pending /
		     approved / executing / failed / rejected)
		     → block bump, set MigrationPendingState
		     with the existing plan name + classification.
		   * Not found → fetch_change_class and classify.
		3. No existing plan + class is destructive
		   (`breaking | data-migration | requires-restart`)
		   → CREATE MigrationPlan via SSA-create with
		   `spec.scope.type=platform`,
		   `spec.scope.platform.components=["platform-stack"]`,
		   trigger fields, classification, and
		   `previousSpecSnapshot.pin` = current `spec.pin`
		   (or JSON null).
		4. No existing plan + class is `safe` → bump
		   as before.

		Conditions surface plan name to the operator:

		* `UpgradeAvailable=True/BlockedByMigrationPlan`
		  with message `... blocked by MigrationPlan
		  apprafter-system/<plan>`.
		* `MigrationPending=True/<classification>` with
		  message `... see MigrationPlan
		  apprafter-system/<plan>`.

		Plan-name idempotency makes repeated reconciles
		on the same transition safe — they find the
		existing plan via GET and don't create a duplicate.
		Rejected plans block the same transition forever
		(operator's explicit decision); transitioning to
		a different target produces a different name +
		fresh plan.

		Removed: PolicyHooks trait + NoOpHooks stub from
		`operator-controllers/platform-stack/src/policy.rs`.
		Was a forward-compat placeholder from B.1.73 that
		never had a real impl. Replaced by inline plan
		creation in the reconcile body. policy.rs deleted;
		Context.hooks field removed.

		RBAC: operator chart's ClusterRole's `migrationplans`
		rule gains `create` verb (was `get list watch patch
		update`). Without `create`, PlatformController
		would 403 forbidden silently and skip the gate.

		**Tests.** +7 unit tests:
		`synthesize_platform_plan_name_replaces_dots_with_dashes`,
		`_is_deterministic`,
		`change_class_to_string_round_trips_known_classes`,
		`build_platform_migration_plan_cr_shape_matches_crd_schema`,
		`_snapshot_pin_is_null_when_unpinned`,
		`plan_classification_returns_string_when_risks_set`,
		`_returns_none_when_risks_absent`. -1 test removed
		(`no_op_hooks_succeed_on_migration_plan_request`
		from deleted policy.rs). Total platform-stack
		crate: 62 → 68.

		**Acceptance gate.** Until B.1.78's walk lands
		on a real published breaking chart, smoke
		verification via:

		   * `kubectl get migrationplan -n apprafter-system`
		     after applying a chart whose
		     `compatibility.yaml` classifies the transition
		     as `breaking` → plan exists with expected
		     scope + trigger + snapshot.
		   * `kubectl get platformstack default -n
		     apprafter-system -o yaml` shows
		     UpgradeAvailable=True/BlockedByMigrationPlan
		     and MigrationPending=True with classification.
		   * Approve via `kubectl patch migrationplan
		     <name> --subresource=status --type=merge
		     -p '{"status":{"phase":"approved"}}'` →
		     MigrationController executes → plan reaches
		     `completed` → next PlatformController
		     reconcile proceeds with bump.
		   * Reject (platform-scope only per ADR 0027)
		     → strategy.reject reverts spec.pin → next
		     reconcile sees the rejected plan name
		     blocking same transition; operator must
		     delete plan or pin to a different target to
		     retry.

		Rendered chart vs 0.1.33: byte-equivalent
		templates. Operator-binary change only;
		chart appVersion lockstep propagates new image.
		"""
	references: [
		"docs/adr/0027-migrationplan-unified.md",
		"plan.md#178-platformcontroller-migrationplan-integration",
		"docs/changelog/UNRELEASED.md#v01132",
	]
}

// Walk-fix #6 post-B.1.77 — webhook config missing
// `migrationplans/status` resource. Walk-fix #2 added the
// validator's ADR 0027 app-scope reject guard, but
// ValidatingWebhookConfiguration rule listed only
// `resources: [migrationplans]`. `kubectl patch
// --subresource=status -p '{"status":{"phase":"rejected"}}'`
// routes via separate `/status` endpoint, bypassing
// webhook entirely → ADR 0027 guard never invoked → app-
// scope plans transitioned to rejected without admission check.
// Fix: add `migrationplans/status` to rules.resources list.
// Operator binary unchanged (no Rust code change); image
// stays at v0.1.130 binary content tagged v0.1.131 via
// lockstep chart appVersion bump.
compatibility: "0.1.33": {
	change:          "safe"
	operatorVersion: "v0.1.131"
	notes: """
		Walk-fix #6 post-B.1.77 — ValidatingWebhookConfiguration
		missing `migrationplans/status` resource rule.

		**Symptom (walk Phase 3.4 retest):** an
		application-scope MigrationPlan applied + patched
		with `kubectl patch --subresource=status --type=merge
		-p '{"status":{"phase":"rejected"}}'`. Webhook
		should have denied per ADR 0027 (walk-fix #2
		validator logic). Actual: patch succeeded, plan
		transitioned to `phase=rejected` without admission
		check.

		**Root cause:** ValidatingWebhookConfiguration's
		`migrationplans.apprafter.io` webhook listed
		`resources: [migrationplans]`. `kubectl patch
		--subresource=status` routes via the apiserver's
		`/status` SUB-resource endpoint, which is a
		separate path from the main resource endpoint.
		Webhook configs must explicitly list
		`<resource>/status` to intercept status-subresource
		writes. Without it, status patches bypass the
		webhook entirely.

		**Fix:** add `migrationplans/status` to rules.resources
		alongside `migrationplans`. Validator code
		(walk-fix #2 ADR 0027 guard, phase transition FSM)
		unchanged — was correct, just never invoked for
		status patches.

		**No Rust code change.** Operator + webhook
		binaries identical to v0.1.130. Image tag bumped
		to v0.1.131 via standard chart lockstep
		(appVersion bump propagates new tag, same binary
		content).

		**Bonus benefit:** chart 0.1.33 pins same image
		v0.1.131 as the operator on a v0.1.130 cluster
		uses after pulling 0.1.32. Pin'ing to 0.1.33
		triggers no pod restart (identical image),
		enabling clean isolation testing for walk-fix #5
		(versionHistory SSA ownership merge) on stable
		pod without chart-upgrade pod-cycle artifacts.

		Rendered chart vs 0.1.32: byte-equivalent for
		operator-chart's templates + values; webhook
		chart's `templates/validatingwebhookconfiguration.yaml`
		gains `migrationplans/status` to the migrationplans
		webhook's resources list.
		"""
	references: [
		"docs/adr/0027-migrationplan-unified.md",
		"plan.md",
		"docs/changelog/UNRELEASED.md#v01131",
	]
}

// Walk-fix #5 post-B.1.77 — versionHistory SSA ownership-
// release bug. Walk-fix #7 (v0.1.122) introduced "omit
// versionHistory from SSA patch when it did not append this
// cycle" to prevent cache-stale-overwrite race. SSA Apply
// semantics (per Kubernetes docs): when manager re-applies
// without a previously-owned field, ownership relinquished;
// if no other manager owns, **apiserver removes field**.
// So bump cycle's append + write claimed ownership, but
// the very next reconcile (settled state) stripped field +
// released ownership → apiserver deleted entry. Within ~30s
// of any bump, versionHistory disappeared. Walk-fix #4
// observability confirmed `include_version_history=false`
// every settled reconcile.
// Fix: read server's authoritative `versionHistory` BEFORE
// each SSA-apply; merge with local entries; ALWAYS include
// field in patch body. `merge_version_history` helper dedupes
// by `(version, appliedAt)` pair, preserves cap. Extra
// `Api::get_status` round-trip per write; settled cycles
// skip via existing `write_status_if_changed` shortcut.
// Operator-binary change only → image v0.1.129 → v0.1.130
// via standard chart appVersion lockstep.
compatibility: "0.1.32": {
	change:          "safe"
	operatorVersion: "v0.1.130"
	notes: """
		Walk-fix #5 post-B.1.77 — versionHistory SSA
		ownership-release bug fix.

		**Symptom.** Walk Phase 6 (artificial pin
		downgrade + upgrade testing on v0.1.129)
		showed `PlatformStack.status.versionHistory` stays
		`null` across multiple successful targetRevision
		bumps. managedFields never showed
		`platform-controller` claiming `f:versionHistory`
		— ownership released ~30s after every bump.

		**Root cause.** Walk-fix #7 (v0.1.122) introduced
		conditional `versionHistory` stripping in SSA
		patch body: when reconcile cycle did not append a
		new entry (settled state, `appended_history=false`),
		field removed from patch to "preserve server-side
		value across cache-stale-overwrite races". Per
		Kubernetes SSA spec:

		> If a field is no longer in the applied
		> configuration, the field manager's ownership
		> is removed. If no field manager owns the field
		> after that operation, the field is removed.

		Pattern was incorrect for SSA Apply semantics —
		bump cycle's append + write claimed ownership,
		next settled-state write released ownership, no
		other manager owned → apiserver deleted field.

		**Fix.** Drop "omit field" pattern. Always include
		`versionHistory` in SSA body. Race protection
		via server-state read + merge:

		1. Before each `patch_status`, `Api::get_status`
		   reads authoritative server state.
		2. `merge_version_history(server, local)` produces
		   merged vector — preserves server entries,
		   appends local-only entries, dedupes by
		   `(version, appliedAt)`, enforces cap.
		3. Patch body always includes the merged
		   `versionHistory`.

		Cost: extra `Api::get_status` per write.
		`write_status_if_changed` shortcut skips no-op
		writes, so steady-state reconciles don't pay
		the round-trip.

		**Tests.** +4 unit tests in
		`operator-controllers/platform-stack/status.rs`:
		`merge_version_history_keeps_server_entries_when_local_is_empty`
		(load-bearing settled-state guard),
		`merge_version_history_appends_local_only_entries`,
		`merge_version_history_dedupes_by_version_and_applied_at`,
		`merge_version_history_caps_at_max`.
		Total platform-stack crate: 58 → 62.

		**Compat with walk-fix #7's race protection.**
		The original race (stale cache → write stale
		vector clobbering apiserver's entry) is now
		impossible — we always merge against fresh
		server state, not cache.

		Rendered chart vs 0.1.31: byte-equivalent
		templates. Operator-binary change only.
		"""
	references: [
		"plan.md",
		"docs/changelog/UNRELEASED.md#v01130",
	]
}

// Walk-fix #3 + #4 post-B.1.77. #3: MigrationController seals
// rejected plans via `status.rejectedAt` marker — prior code
// re-invoked `strategy.reject()` on every reconcile (cold-
// start cache replay → reject re-applied → PlatformStack.spec.pin
// re-patched to snapshot value, overriding any subsequent
// operator pin patches). #4: PlatformController bump-cycle
// observability logs (`target_changed`, `appended_history`,
// `target_for_patch`, `current_target`, history lengths) to
// help diagnose missing versionHistory entries in future walks.
// MigrationPlan CRD schema + CUE schema extended with optional
// `status.rejectedAt: string format=date-time`. Operator-binary
// change only; image v0.1.128 → v0.1.129 via standard chart
// appVersion lockstep.
compatibility: "0.1.31": {
	change:          "safe"
	operatorVersion: "v0.1.129"
	notes: """
		Walk-fix #3 + #4 post-B.1.77.

		**#3: MigrationController rejected-plan seal.**

		Symptom (walk-found, 2026-05-22 acceptance walk
		of B.1.74→B.1.77): user's `kubectl patch
		platformstack default --type=merge -p
		'{"spec":{"pin":null}}'` was silently overridden
		— PlatformController kept landing on
		`spec.pin="0.1.25"` (the snapshot value of a
		previously-rejected platform-scope plan). Logs
		showed `PlatformMigrationStrategy.reject —
		reverted PlatformStack.spec.pin pin_value="0.1.25"`
		on EVERY reconcile of the rejected plan.

		Root cause: MigrationController's "rejected"
		branch unconditionally called `strategy.reject()`
		on each reconcile. Operator pod restarts (chart
		auto-upgrade replacing the Deployment image)
		trigger cold-start cache replay → watcher fires
		on every existing MigrationPlan → rejected plans
		get re-rejected. For platform scope this means
		`spec.pin` reverts to the plan's snapshot pin
		value every restart, locking the cluster on that
		version.

		Fix: persistent `status.rejectedAt` marker. First
		reconcile that sees `phase=rejected` AND no
		`rejectedAt` set: calls `strategy.reject()`,
		then writes `status.rejectedAt=now`. Subsequent
		reconciles see the marker, skip the strategy
		call, await_change.

		CRD schema (operator chart + CUE source): adds
		`status.rejectedAt: string format=date-time` to
		MigrationPlanStatus. Rust type
		(`operator_core::MigrationPlanStatus`) gains
		corresponding `rejected_at: Option<String>`.

		+2 regression unit tests pin the marker logic:
		`rejected_plan_with_rejected_at_marker_is_sealed`,
		`rejected_plan_without_rejected_at_marker_is_not_sealed`.

		**#4: PlatformController bump-cycle observability.**

		Walk Phase 6 (artificial pin downgrade + upgrade)
		uncovered that `status.versionHistory` stays empty
		across multiple successful targetRevision bumps —
		expected to have entries for every flip. Diagnosis
		from logs alone was inconclusive (logs showed only
		generic "reconcile fired"/"reconcile completed").

		To help future walks diagnose: two `info!()` logs
		around the bump decision and the status write:

		* Before append: `PlatformController bump decision`
		  — surfaces `target_changed`, `appended_history`,
		  `target_for_patch`, `current_target`,
		  `prior_history_len`.
		* After conditions + assignments, before write:
		  `PlatformController writing status` — surfaces
		  `include_version_history`, `new_history_len`.

		Production-useful logs (not debug). Future
		walk-fix may follow with the actual versionHistory
		write fix once these logs reveal the offending
		branch.

		Rendered chart vs 0.1.30: byte-equivalent
		templates (except CRD's MigrationPlanStatus
		schema addition). Operator-binary change
		propagates new image.
		"""
	references: [
		"plan.md",
		"docs/changelog/UNRELEASED.md#v01129",
	]
}

// Walk-fix #2 post-B.1.77 — webhook FSM permitted app-scope
// rejected via first-write fast-path, bypassing ADR 0027.
// On a fresh MigrationPlan CR without status, `kubectl patch
// --subresource=status -p '{"status":{"phase":"rejected"}}'`
// slipped through the FSM's permissive `old_phase.is_empty()`
// branch — webhook accepted, controller silently no-op'd
// reject (ApplicationMigrationStrategy::reject returns Ok per
// design), but the ADR 0027 intent ("application-scope plans
// cannot be rejected") was violated in audit terms. Fix: scope
// guard `new_phase=="rejected" && scope=="application" → false`
// applied BEFORE the empty-old-phase fast-path. +3 regression
// tests (first-write to rejected on app-scope blocked; same
// allowed on platform-scope; approved→rejected on app-scope
// blocked). Operator-binary change only → image v0.1.127 →
// v0.1.128 via the standard chart appVersion lockstep.
compatibility: "0.1.30": {
	change:          "safe"
	operatorVersion: "v0.1.128"
	notes: """
		Walk-fix #2 post-B.1.77 — webhook FSM closes
		ADR 0027 bypass on first-write to status.phase.

		Symptom (walk-found, 2026-05-22 acceptance walk
		of B.1.74→B.1.77 on v0.1.127): Phase 3.4 of the
		runbook applied a fresh app-scope MigrationPlan
		then immediately patched `status.phase=rejected`
		via `kubectl patch --subresource=status`. The
		webhook accepted the patch — should have denied
		per ADR 0027.

		Root cause: `is_allowed_phase_transition`'s
		first-write branch (matched when
		`oldObject.status.phase == ""` — fresh CR without
		status) returned `true` for any plausible target
		phase including `rejected`, REGARDLESS of scope.
		The ADR 0027 scope check applied only in the
		`pending-approval → rejected` match arm later in
		the function, so the first-write path bypassed it.

		Fix: ADR 0027 guard now runs FIRST, before the
		first-write fast-path. `new_phase=="rejected" &&
		scope_type=="application" → false` covers any
		path to `rejected` on app-scope:

		* fresh CR + patch to rejected — blocked.
		* pending-approval → rejected — blocked.
		* approved → rejected (defensive) — blocked.
		* executing → rejected (defensive) — blocked.

		Error message extended to reference ADR 0027 for
		any new_phase=rejected on app-scope (was only the
		`pending-approval → rejected` case).

		+3 regression unit tests:
		`rejects_application_scope_first_write_to_rejected_per_adr_0027`,
		`allows_platform_scope_first_write_to_rejected`,
		`rejects_application_scope_approved_to_rejected_per_adr_0027`.

		Total in admission-webhook lib: 75 → 78.

		No code damage from the slipped reject — the
		controller's `ApplicationMigrationStrategy.reject`
		is Ok-no-op per ADR 0027 design (defensive belt-
		and-braces from B.1.76). User who walked through 3.4
		of the runbook saw the plan transition to phase=
		rejected without any side-effect; semantically just
		an audit-trail violation.

		Rendered chart vs 0.1.29: byte-equivalent
		templates. Operator + webhook binary change;
		chart appVersion bump propagates new images.
		"""
	references: [
		"docs/adr/0027-migrationplan-unified.md",
		"plan.md",
		"docs/changelog/UNRELEASED.md#v01128",
	]
}

// Walk-fix #1 post-B.1.76 — MigrationController status SSA
// missing `.force()`. External actors (Backstage, CLI,
// `kubectl patch --subresource=status`) own `status.phase`
// after writing it; controller's own SSA patch carrying the
// next phase value 409s with a managedFields conflict;
// error_policy retries forever; plan freezes at `approved`.
// Fix: add `.force()` to MigrationController's write_status
// + preventively to PlatformController's write_status (which
// hasn't surfaced the bug but is structurally identical).
// Operator-binary change only → image v0.1.126 → v0.1.127
// via the standard chart appVersion lockstep.
compatibility: "0.1.29": {
	change:          "safe"
	operatorVersion: "v0.1.127"
	notes: """
		Walk-fix #1 post-B.1.76 — SSA `.force()` on
		MigrationController + PlatformController status
		writes.

		Symptom (walk-found, 2026-05-22 acceptance walk
		of B.1.74→B.1.77): after `kubectl patch
		migrationplan ... --subresource=status --type=merge
		-p '{"status":{"phase":"approved"}}'`, the plan
		stayed at phase=`approved` forever. Controller
		logs showed `migration reconcile completed` on
		the watch-fired UPDATE event, but no subsequent
		transition to `executing` or `completed`.

		Root cause: kubectl's merge-patch registers the
		field manager `kubectl-patch` as the owner of
		`status.phase`. MigrationController's own SSA
		patch (under `migration-controller` field manager)
		carrying `phase=executing` 409s against that
		ownership without `.force()`. The error propagates
		through `reconcile`, error_policy requeues 15s,
		next reconcile hits the same conflict — infinite
		loop.

		Fix:

		* `operator-controllers/migration::write_status`:
		  add `.force()` to `PatchParams::apply(FIELD_MANAGER)`.
		* `operator-controllers/platform-stack::write_status`:
		  same. Preventive — PlatformController has been
		  the sole writer of `PlatformStack.status` in
		  every walk so far, but the conflict shape is
		  structurally identical and a single manual
		  `kubectl patch --subresource=status` would
		  freeze the loop.

		Application controller's `apply_status` already
		uses `.force()` for exactly this reason — the
		walk-fix brings the migration + platform paths
		in line.

		Rendered chart vs 0.1.28: byte-equivalent
		templates. Operator-binary change only; chart
		appVersion bump propagates the new image.
		"""
	references: [
		"plan.md",
		"docs/changelog/UNRELEASED.md#v01127",
	]
}

// Track B.1.77 closure — Application reconciler gate
// (pause/resume) + Argo CD UI surfacing of pending
// MigrationPlans. ApplicationController now checks for
// unsealed MigrationPlans matching the reconciled
// Application + environment pair BEFORE patching child
// resources; pauses with `status.phase =
// AwaitingMigrationApproval` + `MigrationPending=True`
// condition. argocd-cm gains a custom Lua health script
// that surfaces the pause as `Degraded` in the Argo CD UI
// with the MigrationPlan name in the card message.
// Detection (`detect_destructive`) wired through
// `ApplicationMigrationStrategy::detect_destructive` but
// always returns None in 1.77 — current Application
// v1alpha1 schema has no destructive surface. Phase 2.x
// services wire detection alongside the schema fields.
// Chart side ships only the Lua addition (byte-equivalent
// templates otherwise). Operator-binary change → image
// v0.1.125 → v0.1.126 via the standard chart appVersion
// lockstep.
compatibility: "0.1.28": {
	change:          "safe"
	operatorVersion: "v0.1.126"
	notes: """
		Track B.1.77 closure — Application reconciler
		gate (pause/resume) + Argo CD UI integration.

		ApplicationController gains a pause gate that
		runs BEFORE child resource patches. The gate
		lists MigrationPlans in `apprafter-system`,
		filters to ones whose `spec.scope.type ==
		application` matches this Application's name +
		namespace + environment, and pauses when any
		unsealed plan (phase != completed && phase !=
		rejected) is found. Pause behaviour:

		* `Application.status.phase` flips to
		  `AwaitingMigrationApproval`.
		* `Ready=False` condition with reason
		  `MigrationPending` + message naming the plan.
		* `MigrationPending=True` condition with reason
		  `MigrationPlanPending` + plan name in message
		  (k8s-convention `lastTransitionTime` preserved
		  when condition is already True).
		* Child Deployment / Service / HTTPRoute patches
		  are SKIPPED — children keep running their
		  prior spec.
		* `endpointURL` is preserved.
		* Requeue after 30s so plan-phase changes are
		  picked up promptly.

		Argo CD UI surfacing: `argocd-cm` ConfigMap gains
		a custom resource-health Lua script under the key
		`resource.customizations.health.apprafter.io_Application`.
		Returns `Degraded` with the MigrationPlan name in
		the card message when the Application's phase is
		`AwaitingMigrationApproval`; `Healthy` on
		`Ready`; `Progressing` otherwise.

		`ApplicationMigrationStrategy::detect_destructive`
		concrete fn lands on the strategy struct with the
		full `(old, new)` signature, but the impl returns
		`None` unconditionally in 1.77 — the v1alpha1
		Application schema (image / replicas / expose /
		env) carries no destructive operations per
		spec.md §3.8. Phase 2.x services (`needs.*`,
		storage classes, breaking image migrations)
		populate the diff logic. The strategy also gains
		`create_plan_for` — a `MigrationPlan` CR builder
		used by future callers in Phase 2 once detection
		actually finds destructive diffs.

		`DestructiveChange` type lands in `operator-core`
		(trigger_type + field + from + to +
		classification). Mirrors the
		`MigrationPlan.spec.trigger` + `spec.risks.classification`
		shape so `create_plan_for` is a thin rollup.

		Rendered chart vs 0.1.27: byte-equivalent for
		every component except `argocd` (gains the Lua
		script in `configs.cm`). Operator-binary change
		propagates the new image; chart appVersion
		lockstep.
		"""
	references: [
		"docs/adr/0027-migrationplan-unified.md",
		"plan.md#177-application-reconciler-integration-gate-pauseresume",
		"docs/changelog/UNRELEASED.md#v01126",
	]
}

// Track B.1.76 closure — MigrationController + strategy
// dispatch. Third reconciler in the apprafter-operator
// binary owns the MigrationPlan phase FSM: external actors
// flip `pending-approval → approved | rejected`, controller
// drives `approved → executing → completed/failed` and runs
// `PlatformMigrationStrategy.reject` on platform-scope
// rejects (SSA-patches PlatformStack.spec.pin back to the
// snapshot). Webhook FSM extension blocks application-scope
// rejects (ADR 0027) + sealed-state mutations. RBAC
// extends with migrationplans + /status verbs.
// Operator-binary change only → image v0.1.124 → v0.1.125;
// chart appVersion bump propagates the new image.
compatibility: "0.1.27": {
	change:          "safe"
	operatorVersion: "v0.1.125"
	notes: """
		Track B.1.76 closure — MigrationController +
		strategy dispatch.

		Third reconciler in the apprafter-operator binary
		(peer to ApplicationController + PlatformController),
		gated by the same leader Lease. Owns the
		`MigrationPlan.status.phase` FSM:

		    pending-approval ─[external: phase=approved]──→ approved
		                     ─[external: phase=rejected,
		                       platform scope only]──────→ rejected
		    approved        ─[controller]─────────────────→ executing
		    executing       ─[controller, last step]──────→ completed
		    executing       ─[controller, step failed]────→ failed
		    rejected        ─[controller: strategy.reject]─→ rejected (sealed)

		`MigrationStrategy` trait in `operator-core` covers
		execute_step + reject. Two impls in
		`operator-controllers/migration`:

		* `ApplicationMigrationStrategy`: execute_step is
		  Succeeded (free-form action text, no machine
		  semantics in 1.76); reject is Ok-no-op per ADR
		  0027 (admission webhook also blocks the
		  transition).
		* `PlatformMigrationStrategy`: execute_step is
		  Succeeded; reject reads
		  `plan.spec.previousSpecSnapshot.pin` and
		  SSA-patches `PlatformStack.spec.pin` back to that
		  value (or `null` when the snapshot has no pin).
		  Field manager `migration-controller-strategy`
		  distinguishes the patch from
		  PlatformController's `platform-controller`
		  manager. Idempotent — repeated rejects after a
		  successful revert produce byte-equivalent SSA
		  patches.

		Admission webhook (`validator_migrationplan.rs`)
		gains a `status.phase` transition FSM (paired
		with the existing `spec.scope` immutability):

		* `pending-approval → approved` allowed (any scope).
		* `pending-approval → rejected` allowed for
		  platform scope; rejected for application scope
		  with an ADR-0027 reference in the error message.
		* Sealed states (`completed`, `failed`, `rejected`)
		  are immutable.
		* Controller transitions allowed without identity
		  gating — trust RBAC for that surface.

		RBAC ClusterRole extends with
		`migrationplans` + `migrationplans/status` (get,
		list, watch, patch, update).

		Detection (`detect_destructive` for both scopes)
		intentionally NOT in the trait. Detection
		signatures differ per scope, so each strategy
		exposes a concrete fn that callers in B.1.77
		(Application reconciler) and B.1.78
		(PlatformController) wire in.

		Rendered chart vs 0.1.26: byte-equivalent
		templates. Operator-binary change only (third
		controller spawn + new crate + webhook FSM
		extension); chart appVersion bump propagates the
		new image.
		"""
	references: [
		"docs/adr/0027-migrationplan-unified.md",
		"plan.md#176-migrationcontroller--strategy-dispatch",
		"docs/changelog/UNRELEASED.md#v01125",
	]
}

// Track B.1.75 closure — unified MigrationPlan CRD +
// admission webhook validation. New CRD shipped by the
// operator chart (sync-wave -5 alongside Application +
// PlatformStack). Admission webhook gains a third dispatch
// branch enforcing the scope discriminator, approver-email
// format, and `spec.scope` immutability across UPDATE.
// PlatformStack reconcile behaviour unchanged; this is a
// schema + webhook landing only — MigrationController logic
// lands in B.1.76. Operator-binary change (webhook +
// operator-core types) → image v0.1.123 → v0.1.124 via
// the standard chart appVersion lockstep.
compatibility: "0.1.26": {
	change:          "safe"
	operatorVersion: "v0.1.124"
	notes: """
		Track B.1.75 closure — unified MigrationPlan CRD +
		admission webhook validation.

		Adds the third AppRafter CRD shipped via the
		operator chart: `migrationplans.apprafter.io`
		(short names `mp`, `migplan`), with OpenAPI v3
		schema mirroring `schemas/v1alpha1/migrationplan.cue`.
		Scope discriminator `spec.scope.type`:
		`application` | `platform`. CRD applied at
		sync-wave -5 alongside the two existing CRDs.

		Admission webhook (`apprafter-admission-webhook`)
		extends the `ValidatingWebhookConfiguration` with
		a third entry covering `migrationplans` CREATE +
		UPDATE. Validation:

		* Scope discriminator: `type: application`
		  requires a populated `scope.application` block
		  (with `ref.{name,namespace}` + `environment`);
		  `type: platform` requires `scope.platform.components`
		  non-empty. The mismatched sub-object is rejected.
		* Approver emails: light RFC5322 — single `@`,
		  non-empty local + domain, dot in domain.
		* `spec.scope` immutability on UPDATE — comparing
		  `request.object` vs `request.oldObject` from the
		  AdmissionReview. Other spec fields stay mutable
		  in 1.75; 1.76 tightens those once the controller
		  exists.

		MigrationController logic + status writes ship in
		B.1.76. PlatformController behaviour unchanged in
		this release.

		Rendered chart vs 0.1.25: byte-equivalent
		templates. Operator + webhook binary change
		(+ new validator module + dispatch wiring +
		operator-core MigrationPlan types); chart appVersion
		bump propagates the new images.
		"""
	references: [
		"docs/adr/0027-migrationplan-unified.md",
		"plan.md#175-unified-migrationplan-crd-admission-webhook",
		"docs/changelog/UNRELEASED.md#v01124",
	]
}

// Track B.1.74a — yanking support. Schema extension: every
// #VersionRecord gains an optional `yanked: bool | *false` plus
// `yankedReason?: string`. The CUE constraint is "optional
// string"; the conditional invariant "yankedReason required
// when yanked: true" is enforced at PR + publish time by the
// `platform-stack-{check,publish}.yml` workflows
// (`cue export ./platform-stack/cue/... -e compatibility | jq`
// over the rendered map). PlatformController consumes the
// fields: `resolve_non_yanked_latest` filters yanked versions
// out of channel-latest resolution; the `YankedVersion`
// condition surfaces a verbatim `yankedReason` when
// `status.currentVersion` matches a yanked entry. Operator-
// binary change only; image v0.1.122 → v0.1.123 via the
// standard chart appVersion lockstep.
compatibility: "0.1.25": {
	change:          "safe"
	operatorVersion: "v0.1.123"
	notes: """
		Track B.1.74a closure — yanking support.

		Schema extension (#VersionRecord):

		* `yanked: bool | *false` — chart-author hint that a
		  published version should not be resolved by fresh
		  installs. Default false; older compatibility.yaml
		  tarballs without the field are tolerated.
		* `yankedReason?: string` — verbatim text surfaced
		  in the `YankedVersion` condition message + CLI /
		  Backstage UI banners.

		PlatformController behaviour:

		* `availableVersion` resolution walks
		  channel-matching tags newest-first, skips entries
		  with `yanked: true`. Fresh clusters never land on
		  a yanked version.
		* `YankedVersion` condition (informational, NOT
		  `Ready=False`): True iff `status.currentVersion`
		  matches a yanked entry; the condition `message`
		  carries `yankedReason` verbatim.
		* Upgrade flow unchanged — yank is metadata, not a
		  policy override. Pin to a yanked version stays
		  honoured; warning surfaces, controller takes no
		  action.

		CI guard: PR-time + publish-time workflow step
		fails when any `yanked: true` entry has missing or
		empty `yankedReason`. Mirrors the rationale of the
		compatibility-gate sanity check.

		Architecture note: yank handling moved INLINE in
		the reconcile loop, not behind `PolicyHooks::is_yanked`.
		The trait gained no method; the prior stub method
		was deleted. Rationale: yank is a pure-lookup on
		an already-pulled `CompatibilityDoc`, not an
		extensibility seam.

		Rendered chart vs 0.1.24: byte-equivalent.
		Operator-binary change only; chart appVersion
		bump propagates the new image.
		"""
	references: [
		"plan.md#1.74a-yanking-support",
		"docs/changelog/UNRELEASED.md#v01123",
	]
}

compatibility: "0.1.24": {
	change:          "safe"
	operatorVersion: "v0.1.122"
	notes: """
		Walk-fix #7 v0.1.122 — versionHistory race fix.

		B.1.74 acceptance walk surfaced a controller-cache
		race: PlatformController's own status write triggers
		a follow-up reconcile via the watcher cache (kube-rs
		`Controller`). The follow-up reconcile reads the
		cached `PlatformStack` which lags the apiserver,
		then writes back the stale `versionHistory` vector
		— the just-persisted entry vanishes.

		Fix (Option A): the SSA patch body now OMITS the
		`versionHistory` field when the current reconcile
		cycle did not append a new entry. Server-side apply
		preserves field values that are ABSENT from the
		patch, so the prior reconcile's entry stays
		authoritative on the apiserver.

		Two regression tests guard the new behaviour:

		* `build_status_patch_omits_version_history_when_not_appended`
		* `build_status_patch_includes_version_history_when_appended`

		Rendered chart vs 0.1.23: byte-equivalent.
		Operator-binary change only; chart appVersion
		bump propagates the new image.
		"""
	references: [
		"docs/superpowers/plans/2026-05-20-track-b-1-73-platform-controller.md",
		"docs/changelog/UNRELEASED.md#v01122",
	]
}
