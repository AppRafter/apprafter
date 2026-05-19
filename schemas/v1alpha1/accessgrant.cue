// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package v1alpha1

// AccessGrant declares access for a human or external system. Replaces
// ad-hoc kubeconfig + VPN-credential distribution. See spec.md §3.4
// for the end-to-end flow and phase 4.5 for the operator
// implementation.
#AccessGrant: {
	#TypeMeta
	kind:     "AccessGrant"
	metadata: #ObjectMeta
	spec: {
		// Identity (typically email address).
		subject: string

		scope: {
			namespaces?: [...string]
			capabilities?: [...#Capability]
			resources?: [...string]
		}

		network?: {
			routes?: [...string]
			services?: [...string]
		}

		// MFA flag — boolean true is equivalent to "required".
		mfa?: bool | "required"

		// Duration (Go duration string) or absolute date (RFC3339).
		expiry?: string
	}

	status?: {
		phase?: "pending-activation" | "active" | "expiring" | "expired" | "revoked"
	}
}
