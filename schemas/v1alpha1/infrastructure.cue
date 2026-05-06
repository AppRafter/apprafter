// SPDX-License-Identifier: FSL-1.1-MIT

package v1alpha1

// Infrastructure describes the substrate the platform runs on
// (provider, nodes, network, OS image). Applied by `platform-cli`.
// See spec.md §3.7 and phase 1.2 / 5.2 / 6.2.
#Infrastructure: {
	#TypeMeta
	kind:     "Infrastructure"
	metadata: #ObjectMeta
	spec: {
		// Provider identifier (built-in: "hetzner-cloud",
		// "hetzner-robot", "aws"; community via
		// InfrastructureProviderPlugin).
		provider: string

		nodes: [...{
			role:  "control-plane" | "worker" | "egress"
			type:  string
			count: int & >=1
		}]

		network?: {
			privateNetwork?: string
			floatingIPs?: [...string]
		}

		osImage?: string
	}
}
