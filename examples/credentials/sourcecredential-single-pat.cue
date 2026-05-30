// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Example SourceCredential — the launch-default single classic PAT
// used in both halves (ADR 0039). The git and registry backends both
// reference the same sealed material; the operator derives a
// prefix-matched Argo repo-cred and a host-matched pull-secret from
// it. Used as a `cue vet` fixture.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

sourceCredentialSinglePat: v1alpha1.#SourceCredential & {
	metadata: {
		name:      "acme"
		namespace: "apprafter-system"
	}
	spec: {
		git: {
			backend: sealedSecretRef: name: "srccred-acme-material"
			repoPrefixes: ["github.com/acme/"]
		}
		registry: {
			backend: sealedSecretRef: name: "srccred-acme-material"
			hosts: ["ghcr.io/acme/"]
		}
	}
}
