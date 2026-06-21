// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// bitnami-labs sealed-secrets controller — the Tier-1 secret
// backend (ADR 0007 / plan.md 2.11). Provides asymmetric
// at-rest sealing: the CLI seals material client-side with the
// controller's public cert, and only the in-cluster private key
// decrypts. It is the prerequisite carved ahead of the rest of
// 2.11 for the `SourceCredential` credential flow (1.79c / ADR
// 0039) — the operator reads the controller-unsealed material
// Secret and derives the Argo `repo-creds` + workload pull-secret.
//
// Pinned to chart 2.18.6 (appVersion 0.37.0). The `SealedSecret`
// CRD (`sealedsecrets.bitnami.com`) ships in the chart's `crds/`
// directory; Argo CD renders Helm sources with `--include-crds`
// by default, so the CRD is installed alongside the controller —
// no separate CRD-bundle flag is needed (unlike cert-manager,
// whose CRDs are gated behind a `crds.enabled` value).
//
// `fullnameOverride` pins every rendered name to
// `sealed-secrets-controller` so the CLI's cert-fetch path
// (`GET` via the kube-API service proxy
// `…/services/http:sealed-secrets-controller:http/proxy/v1/cert.pem`)
// is deterministic across installs.
_components: "sealed-secrets": #Component & {
	name:      "sealed-secrets"
	enabled:   bool | *true
	namespace: "apprafter-system"
	source: {
		// Chart repo moved bitnami-labs.github.io -> bitnami.github.io
		// (the `-labs` host 404s as of 2026-06; the new host serves the
		// identical chart 2.18.6 / appVersion 0.37.0). A fresh bootstrap
		// could not install the secrets controller from the dead host.
		repoURL: "https://bitnami.github.io/sealed-secrets"
		chart:   "sealed-secrets"
	}
	version: "2.18.6"
	values: {
		fullnameOverride: "sealed-secrets-controller"
		// Tier-1 baseline — the controller is a single tiny
		// reconciler; Tier 2+ overlays may raise this.
		resources: requests: {
			cpu:    "50m"
			memory: "64Mi"
		}
	}

	// No cert-manager dependency (the controller ships no
	// admission webhook of its own), but install ahead of the
	// operator (wave 0) so the `SealedSecret` CRD and the
	// controller exist before any `SourceCredential` material is
	// sealed at runtime.
	syncWave: -8

	// The controller Deployment surfaces
	// `status.terminatingReplicas` on k3s v1.35 — same schema
	// skew muted for every other Deployment-bearing component.
	ignoreDifferences: [{
		group: "apps"
		kind:  "Deployment"
		jsonPointers: ["/status/terminatingReplicas"]
	}]
}
