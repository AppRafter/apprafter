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
currentVersion: #Version & "0.1.1"

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
