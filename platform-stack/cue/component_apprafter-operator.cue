// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// AppRafter operator — the `apprafter.io/v1alpha1.Application`
// reconciler. Pulled from our public GHCR registry. The chart
// itself lives in `operator/chart/` in the AppRafter monorepo
// and is published via `.github/workflows/release-operator.yml`
// in lockstep with the `apprafter` CLI version (the cli has a
// `RELEASED_OPERATOR_VERSION` constant that drives the default
// tag).
//
// platform-stack 0.1.0 pins the operator at v0.1.91 (the
// closing tag of Track A). Future platform-stack versions bump
// this together with their own `version` field — the
// compatibility metadata in `cue/compatibility.cue` records the
// pairing.
_components: "apprafter-operator": #Component & {
	name:      "apprafter-operator"
	enabled:   bool | *true
	namespace: "apprafter-system"
	source: {
		// OCI registry — without the `oci://` scheme prefix.
		// Argo CD identifies OCI vs HTTPS chart repos from the
		// `Secret(argocd.argoproj.io/secret-type: repository)`
		// + `enableOCI: "true"` registration in
		// `component_argocd.cue`, not from the URL scheme.
		repoURL: "ghcr.io/apprafter"
		chart:   "apprafter-operator"
	}
	version: "v0.1.92"
	values: {
		image: {
			repository: string | *"ghcr.io/apprafter/apprafter-operator"
			// Image tag pinned to the monorepo tag that ships
			// the fresh build with explicit-command Dockerfile.
			// v0.1.91 image lacked usable ENTRYPOINT in its
			// manifest on k3s v1.35 containerd (walk-found bug
			// v0.1.104 → v0.1.105: pods failed with
			// `failed to generate spec: no command specified`).
			// Chart 0.1.92 also hard-wires the command in the
			// deployment template as defence in depth.
			tag: string | *"v0.1.105"
		}
		// Leader-election guards (10s renew / 30s expiry) match
		// the in-tree settings; tier overlays may bump replicas.
		replicas: int | *1
	}

	// Same Kubernetes 1.31+ field skew as `component_cilium.cue`
	// and `component_argocd.cue`. The operator Deployment
	// surfaces `status.terminatingReplicas` on k3s v1.35; Argo
	// CD 2.13.1 doesn't know the field and reports
	// `ComparisonError: field not declared in schema`. The
	// sync itself succeeds (it's a diff-side error, not an
	// apply-side error), but the noise prevents Healthy.
	ignoreDifferences: [{
		group: "apps"
		kind:  "Deployment"
		jsonPointers: ["/status/terminatingReplicas"]
	}]
}
