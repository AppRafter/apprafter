// SPDX-License-Identifier: FSL-1.1-MIT
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

infra: v1alpha1.#Infrastructure & {
	metadata: {
		name: "platform-1"
	}
	spec: {
		provider: "hetzner-cloud"
		region:   "nbg1"
		nodes: [{
			role:  "control-plane"
			type:  "cx22"
			count: 1
		}]
		network: {
			ip_range: "10.0.0.0/16"
			subnet: {
				ip_range: "10.0.0.0/24"
				zone:     "eu-central"
			}
			floatingIPs: ["egress"]
		}
		firewall: {
			ingress: [
				{
					port:     "22"
					protocol: "tcp"
					source_ips: ["0.0.0.0/0", "::/0"]
				},
				{
					port:     "443"
					protocol: "tcp"
					source_ips: ["0.0.0.0/0", "::/0"]
				},
			]
		}
		osImage: "ubuntu-24.04"

		// Optional: expose Argo CD UI through Gateway API + cert-manager,
		// and/or wire a bootstrap Application at a Git repo. Uncomment
		// + adjust the FQDN (DNS A/AAAA → Hetzner public IP) and the
		// repo URL to opt in.
		// argocd: {
		//     domain:        "argo.example.com"
		//     bootstrapRepo: "https://github.com/acme/platform-state.git"
		//     bootstrapPath: "clusters/tier-1"
		// }
	}
}
