// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// AppRafter admission webhook — enforces cross-field invariants
// the OpenAPI v3 CRD layer can't express (image required, env
// names DNS-1123, env keys `^[A-Z_][A-Z0-9_]*$`). Distinct
// binary from the operator; same image registry, same release
// cadence.
//
// platform-stack 0.1.0 pins the webhook at v0.1.91 alongside
// the operator. Cert-manager handles the TLS Certificate
// (`apprafter-system/admission-webhook-tls`) and rotates it
// automatically; the chart values surface only the image refs.
_components: "admission-webhook": #Component & {
	name:      "admission-webhook"
	enabled:   bool | *true
	namespace: "apprafter-system"
	source: {
		// OCI registry — without the `oci://` scheme prefix.
		// Argo CD reads the OCI flag from the `apprafter-charts`
		// repository Secret registered by `loader_values.cue`.
		// Charts live under `/charts` (not the org root) so a
		// chart tag never collides with the same-named container
		// image at `ghcr.io/apprafter/apprafter-admission-webhook`
		// — see `component_apprafter-operator.cue` for the full
		// rationale.
		repoURL: "ghcr.io/apprafter/charts"
		chart:   "apprafter-admission-webhook"
	}
	version: "v0.2.7"
	values: {
		image: {
			repository: string | *"ghcr.io/apprafter/apprafter-admission-webhook"
			// `tag:` omitted — falls back to `.Chart.AppVersion`
			// (same pattern as component_apprafter-operator.cue;
			// see that file's comment for the full rationale).
		}
		// Two replicas by default so a single pod restart
		// doesn't gap the validating webhook. Tier overlays
		// may turn this into 1 on tier-1 single-node where
		// the gap is tolerable.
		replicas: int | *2
	}

	// Same Kubernetes 1.31+ field skew. The webhook
	// Deployment also surfaces `status.terminatingReplicas`
	// on k3s v1.35.
	ignoreDifferences: [{
		group: "apps"
		kind:  "Deployment"
		jsonPointers: ["/status/terminatingReplicas"]
	}]
}
