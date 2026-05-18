// SPDX-License-Identifier: FSL-1.1-MIT

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
		repoURL: "oci://ghcr.io/apprafter"
		chart:   "apprafter-admission-webhook"
	}
	version: "v0.1.91"
	values: {
		image: {
			repository: string | *"ghcr.io/apprafter/apprafter-admission-webhook"
			tag:        string | *"v0.1.91"
		}
		// Two replicas by default so a single pod restart
		// doesn't gap the validating webhook. Tier overlays
		// may turn this into 1 on tier-1 single-node where
		// the gap is tolerable.
		replicas: int | *2
	}
}
