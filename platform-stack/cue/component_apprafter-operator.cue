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
	version: "v0.1.93"
	values: {
		image: {
			repository: string | *"ghcr.io/apprafter/apprafter-operator"
			// Image tag pinned to v0.1.106: the operator chart
			// itself is unchanged from v0.1.92 (deployment
			// template gained explicit `command:` then), but
			// the admission-webhook crate at v0.1.105 panicked
			// at runtime on rustls 0.23+'s missing default
			// CryptoProvider; v0.1.106 adds the same fix the
			// operator binary already had since v0.1.61.
			// Bumping operator chart version too (v0.1.92 →
			// v0.1.93) keeps the lockstep with webhook chart.
			tag: string | *"v0.1.106"
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
