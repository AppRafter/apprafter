// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

import "apprafter.io/argocd-cue-cmp:argocdcuecmp"

// Argo CD Config Management Plugin (CMP) sidecar that teaches
// Argo CD to render CUE source repositories. See ADR 0029 for
// the design rationale.
//
// The CMP container runs as a sidecar of `argocd-repo-server`,
// NOT as a standalone Argo CD Application. This file therefore
// declares the component with `enabled: false` so the umbrella
// chart's `templates/applications.yaml` skips it — the sidecar
// wiring lives in `cue/component_argocd.cue` via
// `values.repoServer.extraContainers`, which reads `image.*`
// from this file via the `_components` map for traceability.
//
// The image is built and published independently of the
// platform-stack chart, on its own `argocd-cue-cmp/v*` tag
// series. Bumping the cue-cmp image version is a one-line
// edit to `argocd-cue-cmp/version.cue`; this file and the
// publish workflow both derive from that single CUE source.
_components: "argocd-cue-cmp": #Component & {
	name: "argocd-cue-cmp"
	// Default OFF — this component is a sidecar, not a
	// standalone Application. `component_argocd.cue` reads
	// `values.image.*` to inject the sidecar into
	// `repoServer.extraContainers`.
	enabled:   bool | *false
	namespace: "argocd"
	source: {
		// Source URL is informational — the sidecar is not
		// pulled as a Helm chart, it's a container image
		// the chart's argocd values reference directly. The
		// repoURL points operators at the source of the
		// image so `cue/compatibility.cue` retrospect is
		// possible.
		repoURL: "https://github.com/apprafter/apprafter/tree/main/argocd-cue-cmp"
		path:    "argocd-cue-cmp"
	}
	// The `v` prefix is added here; the SoT version in
	// `argocd-cue-cmp/version.cue` is bare semver (e.g.
	// "0.1.1"). v0.1.0 image existed on ghcr.io but was
	// tagged `:0.1.0` (no `v` prefix) due to a workflow
	// bug — chart's `:v0.1.0` pin produced
	// MANIFEST_UNKNOWN at pull time. v0.1.1 onward uses
	// the corrected `:v<version>` tag form (walk-found
	// bug v0.1.107 → v0.1.108).
	version: "v\(argocdcuecmp.version)"
	values: {
		image: {
			repository: string | *"ghcr.io/apprafter/argocd-cue-cmp"
			// Tag is `v` + the SoT version from
			// `argocd-cue-cmp/version.cue`. A single bump
			// there propagates here and into the publish
			// workflow's `detect` step (B.1.71b
			// drift-class closure).
			tag: string | *"v\(argocdcuecmp.version)"
		}
		// CUE evaluation timeout per repo render. Default
		// 57s matches Argo CD's repo-server hard timeout
		// (60s) less the 2-3s overhead of sidecar plumbing.
		cueTimeoutSeconds: int | *57
	}
}
