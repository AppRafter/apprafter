// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package v1alpha1

// ExternalSurface declares the platform-managed external contour:
// git host, container registry, access plane, synthetic monitoring,
// backups, notifications. See spec.md §3.5 and phase 4.1.
#ExternalSurface: {
	#TypeMeta
	kind:     "ExternalSurface"
	metadata: #ObjectMeta
	spec: {
		git?: {
			provider: string
			url:      string
			backups?: {
				to:       string
				schedule: string
			}
		}

		registry?: {
			provider:   string
			url:        string
			retention?: string
			signing?:   "required" | "optional"
		}

		access?: {
			provider: string
			network?: string
		}

		syntheticMonitoring?: {
			provider:  string
			location?: string
			endpoints?: [...string]
		}

		backups?: {
			destination: string
			retention?:  string
			encryption?: "required" | "optional"
		}

		notifications?: {
			providers: [...string]
			smtp?: {
				host: string
				from: string
			}
		}
	}
}
