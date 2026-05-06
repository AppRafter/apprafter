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
	}
}
