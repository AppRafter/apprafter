// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Package platformstack is the CUE source of truth for the
// AppRafter platform-stack umbrella Helm chart.
//
// Per ADR 0028, the only artifacts under version control here are
// CUE files. The rendered Helm chart (`Chart.yaml`,
// `values.yaml`, `templates/applications.yaml`,
// `values.schema.json`, `compatibility.yaml`) lives in `dist/`
// (gitignored) and is published to OCI on tag.
//
// See `platform-stack/README.md` for the contribution model.
package platformstack

// #Version is the public version of the platform-stack umbrella
// chart. The first published version is 0.1.0 — minor tracks
// the AppRafter monorepo phase (Phase 1.5 → chart 0.1.x;
// chart MINOR bumps to 0.2.0 alongside the `v0.2.0-services`
// milestone when Phase 2 services land). Chart patch versions
// are independent of the monorepo patch stream (`v0.1.x`); the
// two share only MINOR/MAJOR semantics.
//
// Semver semantics:
//
// - MAJOR — incompatible change to the chart values shape, the
//   component-set contract, or the PlatformStack CRD payload it
//   produces. Operators must read `compatibility.yaml` before
//   upgrading.
// - MINOR — additive component changes (new component, new
//   optional tier overlay, new optional value).
// - PATCH — bug fixes within a component, version bumps to
//   curated dependencies that don't affect the chart shape.
#Version: string & =~"^[0-9]+\\.[0-9]+\\.[0-9]+(-[0-9A-Za-z.-]+)?$"

// #Channel is the release channel a published version may flow
// through. `stable` is the default for end-user installs;
// `edge` is for development / pre-release pinning. The channel
// is metadata only — actual version selection happens via OCI
// tag resolution.
#Channel: "stable" | "edge"

// #Tier is the AppRafter deployment tier (solo / team / prod /
// regulated). The same chart renders different
// `values.components` subsets per tier through the `cue/tier_*.cue`
// overlays.
#Tier: 1 | 2 | 3 | 4

// #ComponentSource describes where Argo CD pulls a component's
// manifests from. Three shapes are supported today:
//
// 1. Helm chart from an HTTPS repo (`repoURL` ends in a Helm
//    repository index URL, `chart` names the chart in it).
// 2. Helm chart from OCI (`repoURL` starts with `oci://`).
// 3. Plain Kubernetes manifests under a Git repo path
//    (`repoURL` is a Git remote, `path` names the directory).
//
// The `templates/applications.yaml` chart template picks the
// right Argo CD spec.source shape based on which fields are
// set; CUE validates here that the combination is consistent.
#ComponentSource: {
	// HTTPS Helm repo or OCI registry URL.
	repoURL: string
	// Chart name for Helm sources, path under repo for Git
	// sources. Mutually informative with `repoURL`'s scheme.
	chart?: string
	path?:  string
}

// #Component is one entry in the umbrella chart's
// `values.components` map. Each component renders to exactly
// one Argo CD `Application` resource.
//
// `enabled` lets a tier overlay turn a component off without
// removing the component definition from the source (which
// would force a chart-shape change).
#Component: {
	// Component identifier. Surfaces as the Argo CD
	// `metadata.name` of the rendered Application. DNS-1123
	// constrained because Argo CD enforces it server-side.
	name: string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$"

	// True iff the component should be installed in the
	// current tier overlay. False values keep the source
	// declaration but skip the rendered Application.
	enabled: bool | *true

	// Kubernetes namespace where Argo CD installs the
	// component. Argo CD creates the namespace when missing
	// (via `syncPolicy.syncOptions: ["CreateNamespace=true"]`
	// in the template).
	namespace: string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$"

	// Source repository / chart / path. See #ComponentSource.
	source: #ComponentSource

	// Chart version (Helm) or Git revision (Git). Forwarded
	// verbatim into Argo CD's `spec.source.targetRevision`.
	// Pinned to a concrete version string; the umbrella chart
	// itself is the single source of "what version of Cilium
	// do we ship" — components don't drift on `latest`.
	version: string

	// Free-form values forwarded into Argo CD's
	// `spec.source.helm.valuesObject`. Tier overlays may merge
	// on top.
	values: {...}

	// Sync-policy knobs. Defaults below match the v0.1.x
	// `cluster-bootstrap` behaviour (auto-prune, self-heal,
	// auto-create namespace).
	syncPolicy: {
		automated: {
			prune:    bool | *true
			selfHeal: bool | *true
		}
		syncOptions: [...string] | *["CreateNamespace=true", "ServerSideApply=true"]
	}

	// Argo CD AppProject this component's Application belongs
	// to. Defaults to `"platform"` per Track B.1.79a — all
	// chart-managed components are platform-internal. Tier
	// overlays and ServiceProvider charts may override to
	// `"platform-providers"` (for example, when Phase 2 CNPG /
	// Dragonfly / NATS components land in the umbrella). User-
	// app Applications go through `apprafter app add` and
	// get `project: "apps"` directly, not via #Component.
	//
	// AppProjects (`platform`, `platform-providers`, `apps`,
	// + legacy `default`) are declared in
	// `_loaderValues.argocd.values.configs.projects`, which
	// guarantees their creation by the initial Argo CD install
	// before the first component Application
	// syncs.
	project: string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$" | *"platform"

	// Argo CD sync-wave for the rendered Application. Argo CD
	// sorts Applications by `argocd.argoproj.io/sync-wave`
	// annotation ascending; a wave starts only after the
	// previous wave's Applications report `Sync=Synced`.
	// Practical use today:
	//
	//   - cilium    → -20  (CNI, prerequisite for everything)
	//   - argocd    → -15  (self-management adopt)
	//   - cert-manager → -10  (CRDs + webhook live before
	//                          any cert-manager.io/v1 resource
	//                          is applied)
	//   - default    → 0    (operator, admission-webhook,
	//                        network-policies, backstage)
	//
	// Without this, the admission-webhook chart's `Certificate`
	// resource is applied before cert-manager's validating
	// webhook service has endpoints, and the request fails
	// with `failed calling webhook "webhook.cert-manager.io":
	// no endpoints available`.
	syncWave: int | *0

	// Optional `spec.ignoreDifferences` — list of fields Argo CD
	// should exclude from drift comparison. Practical use today
	// is muting Kubernetes-version-skew fields the Argo CD
	// `2.13.1` schema does not yet know about (e.g.
	// `Deployment.status.terminatingReplicas` added in
	// Kubernetes 1.31 and surfaced by k3s v1.35; Argo CD
	// reports `field not declared in schema` when the live
	// object carries it but the structured-merge differ doesn't
	// recognise it). Per-component because the noisy fields
	// differ by component shape — operator's Deployment doesn't
	// expose the same fields as Cilium's DaemonSet.
	//
	// Empty list = no ignored differences (the common case;
	// most components don't need this).
	ignoreDifferences: [...{
		group: string
		kind:  string
		jsonPointers?: [...string]
		jqPathExpressions?: [...string]
	}] | *[]
}

// #ComponentSet is the umbrella chart's `values.components`
// map. Plain `[string]: #Component` pattern — each component's
// `name` field is set explicitly in `cue/component_<name>.cue`
// rather than via an autobinding `[NAME=string]: #Component & {
// name: NAME }` form. The autobinding form was rejected on the
// pre-1.66 design walk because it re-applied `#Component` to
// every map entry during the per-tier overlay unification and
// stripped concrete `namespace` / `version` fields contributed
// by the per-component declarations.
#ComponentSet: {
	[string]: #Component
}

// #ComponentOverride is one entry in the umbrella chart's
// optional `values.overrides` map. PlatformController writes
// this block onto the parent platform Application's
// `helm.valuesObject` per the B.1.73 design — at chart render
// time the Helm template merges these onto the per-component
// `_components` projection (override wins on `pin`, deep-merges
// on `values`, replaces on `enabled`).
//
// The chart's per-component pin (`_components.<name>.version`)
// is the "curated bundle" version; an operator wanting a
// component to differ from the bundle declares the override
// here via PlatformStack.spec.overrides.<name>.
#ComponentOverride: {
	// Force this component to a specific upstream chart version,
	// bypassing the umbrella chart's curated pin. Used for
	// security backports out-of-band of the umbrella release.
	pin?: string

	// Component-specific values merged onto `_components.<name>.values`
	// at template-render time (override wins on collisions). Free-
	// form per component; the chart does not validate the shape
	// (mirror'ed from the upstream component chart's own values).
	values?: {...}

	// Disable a component entirely. Per-component `enabled`
	// inside `_components` is the chart-side default; the
	// PlatformStack-side override here wins when set.
	enabled?: bool
}

// #PlatformValues is the full shape of the umbrella chart's
// rendered `values.yaml`. Tier overlays project into this
// shape; the chart template iterates `components`.
#PlatformValues: {
	// Chart version emitted into `Chart.yaml` and any rendered
	// labels.
	version: #Version

	// Tier this rendering targets. Used by the template only
	// for labelling — actual component selection happens via
	// the overlay setting `enabled: false` per component.
	tier: #Tier

	// Channel metadata. Defaults to `stable`; `edge` overlays
	// may override.
	channel: #Channel | *"stable"

	// All declared components. Tier overlays may switch
	// individual components off (`enabled: false`) but cannot
	// remove the declaration — that's a chart-shape change.
	components: #ComponentSet

	// Optional per-component overrides — PlatformController
	// writes this block at runtime via SSA on the parent
	// platform Application. The chart's `_applicationsTemplate`
	// reads `(index .Values.overrides $name)` at install time
	// and threads `pin` / `values` / `enabled` into the rendered
	// child Application.
	//
	// Empty by default — the chart's curated bundle stands on
	// its own without any override block.
	overrides?: [string]: #ComponentOverride

	// Argo CD `AppProject` definitions shipped as standalone
	// umbrella manifests at sync-wave -30 (walk-fix #2 post-
	// B.1.79a, chart 0.1.41). Iterated by the chart's
	// `templates/appprojects.yaml` template into one
	// `kind: AppProject` per entry.
	//
	// **Why both here AND in `_loaderValues.argocd.values.
	// configs.projects`?** The argocd subchart's
	// `configs.projects` mechanism renders AppProjects ONLY
	// when the Argo CD chart syncs, which happens at sync-wave
	// -15. Child Applications at sync-wave 0 (admission-webhook,
	// operator, etc.) reference these projects but are sometimes
	// applied before wave -15 finishes when Argo CD does NOT
	// strictly serialise inter-Application sync-waves on the
	// app-of-applications pattern. Result: walk-found bug —
	// `Unable to refresh admission-webhook: app is not allowed
	// in project "platform", or the project does not exist`.
	//
	// Fix: the umbrella chart itself emits AppProject CRs at
	// sync-wave -30 (earliest possible — before Cilium at -20).
	// configs.projects in argocd subchart stays for initial
	// loader install (when there's no umbrella yet). Once the
	// umbrella adopts on first sync, the umbrella-managed
	// AppProjects take ownership — Argo CD's reconciler treats
	// the two as the same logical resource (same group/kind/
	// name/namespace), so this is byte-equivalent on steady
	// state, deterministically-ordered on first sync.
	appProjects: [string]: #AppProjectSpec

	// ServiceProvider CRs seeded by the umbrella so a fresh
	// cluster has at least the launch-default backends declared.
	// Iterated by `templates/serviceproviders.yaml` into one
	// `kind: ServiceProvider` per entry. Empty by default; tiers
	// set `serviceProviders: _serviceProviders`.
	serviceProviders: [string]: #ServiceProviderSeed
}

// `#AppProjectSpec` — the small shape `templates/appprojects.
// yaml` iterates. Mirrors Argo CD's AppProject v1alpha1 CRD
// only on the fields the umbrella actually sets.
#AppProjectSpec: {
	description: string
	sourceRepos: [...string]
	destinations: [...{
		namespace: string
		server:    string
	}]
	clusterResourceWhitelist: [...{
		group: string
		kind:  string
	}]
	namespaceResourceWhitelist: [...{
		group: string
		kind:  string
	}]
}

// `#ServiceProviderSeed` — the small shape
// `templates/serviceproviders.yaml` iterates. The umbrella seeds
// one `kind: ServiceProvider` (apprafter.io/v1alpha1) per entry so
// the 2.3 scheduler has a provider to match claims against on a
// fresh cluster. Namespaced into `apprafter-system` (the operator's
// namespace, where platform providers live). `labels` become the
// CR's `metadata.labels` — the scheduler matches a claim's selector
// as a subset of these, so the launch pg provider carries
// `tier: integrated`. `config` is opaque (the CNPG cluster
// coordinates the 2.4c provisioner reads); `type` mirrors the
// closed `#PlatformServiceType` enum but is kept a plain string
// here because the umbrella does not import the schemas module.
#ServiceProviderSeed: {
	// Namespace the CR lands in. Defaults to the operator's
	// namespace; the scheduler lists providers cluster-wide.
	namespace: string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$" | *"apprafter-system"

	// `metadata.labels` of the rendered CR. The scheduler matches
	// `claim.spec.selector` ⊆ these. Must be non-empty: beyond the
	// scheduler needing `tier: integrated` to match, the template
	// appends static `apprafter.io/*` labels after
	// `{{ toYaml $sp.labels }}`, so an empty map would emit a flow
	// `{}` followed by block keys — invalid YAML.
	labels: [string]: string

	// `spec.type` — a built-in platform-service type.
	type: string

	// `spec.backend` — implementation identifier (e.g.
	// "cloudnative-pg").
	backend: string

	// `spec.config` — backend-specific, opaque to the umbrella.
	config: {...}

	// Argo CD sync-wave. Positive so the CR applies after the
	// apprafter-operator child Application (wave 0) has installed
	// the ServiceProvider CRD; paired with
	// SkipDryRunOnMissingResource in the template for CRD-race
	// resilience.
	syncWave: int | *5
}

// `currentVersion` is THE single source of truth for the chart
// version being built. Every other place that needs to mention
// the version must reference this:
//
//   - `tier_solo.cue` + `tier_team.cue` use `version:
//     currentVersion` instead of a string literal.
//   - The renderer (`render_tool.cue`) computes its `dist/`
//     subdir from `currentVersion`.
//   - The `platform-stack-publish` workflow reads the value
//     via `cue export -e currentVersion`.
//
// Bumping the chart version is a one-line edit here PLUS adding
// the matching `compatibility[currentVersion]` entry below.
// CUE enforces the pairing (see the `compatibility:
// (currentVersion): #VersionRecord` line in `compatibility.cue`)
// — a bump that forgets the compatibility entry fails `cue vet
// -c` with an "incomplete value" error pointing at the missing
// fields, before the publish workflow ever runs.
currentVersion: #Version & "0.2.12"

// `_components` is the package-level base set, populated by
// every `cue/component_<name>.cue` file declaring
// `_components: <name>: #Component & { … }`. The leading
// underscore makes the field hidden — it doesn't appear in
// rendered output, only feeds the tier overlays via
// `components: _components & { <overlay> }`.
//
// Pattern-constraint typing was tried (`_components:
// #ComponentSet`) on the pre-1.66 design walk but was reverted
// — re-applying `#Component`'s pattern at every per-tier
// unification stripped concrete `namespace` / `version`
// fields. Each entry's `#Component` conformance is enforced
// locally at the declaration site in `cue/component_*.cue`
// instead.
_components: {}
