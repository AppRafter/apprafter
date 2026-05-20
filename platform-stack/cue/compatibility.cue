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
		"""
	references: [
		"docs/changelog/UNRELEASED.md#v01107",
	]
}
