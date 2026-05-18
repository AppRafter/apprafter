// SPDX-License-Identifier: FSL-1.1-MIT

package platformstack

// Argo CD Config Management Plugin (CMP) sidecar that teaches
// Argo CD to render CUE source repositories. See ADR 0029 for
// the design rationale.
//
// platform-stack 0.1.0 ships the CMP scaffold but doesn't yet
// wire it as an Application of its own — the CMP container
// runs as a sidecar of `argocd-repo-server`, configured via
// values merged into the Argo CD release. This component
// exists so the operator's view of "which CMP version do we
// ship" is centralised here, not duplicated in
// `cue/component_argocd.cue`.
//
// Wiring step (`templates/applications.yaml` reading this
// component) lands in plan.md sub-phase 1.69 alongside the
// repo-server sidecar values; today the component carries the
// definition without producing an Argo CD Application.
_components: "argocd-cue-cmp": #Component & {
	name: "argocd-cue-cmp"
	// Default OFF until the sidecar plumbing lands in 1.69.
	// Tier overlays may turn it on once they're ready to feed
	// user CUE repositories through Argo CD.
	enabled:   bool | *false
	namespace: "argocd"
	source: {
		repoURL: "oci://ghcr.io/apprafter"
		chart:   "apprafter-argocd-cue-cmp"
	}
	version: "v0.1.91"
	values: {
		image: {
			repository: string | *"ghcr.io/apprafter/argocd-cue-cmp"
			tag:        string | *"v0.1.91"
		}
		// CUE evaluation timeout per repo render. Default 60s
		// matches Argo CD's repo-server hard timeout less the
		// 2-3s overhead of sidecar plumbing.
		cueTimeoutSeconds: int | *57
	}
}
