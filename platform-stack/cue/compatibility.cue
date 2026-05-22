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
