// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Example Application declaring `needs.redis` (Phase 2.6 / ADR 0042).
// The base need is ephemeral; the `prod` environment opts into a
// persistent pool instance via `persistent: true`. Used as a `cue vet`
// fixture for the new `#ServiceNeed.persistent` field.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

redisApp: v1alpha1.#Application & {
	metadata: {
		name:      "web"
		namespace: "demo"
	}
	spec: {
		base: {
			image:    "ghcr.io/example/web:latest"
			replicas: 2
			expose: {
				port:     3000
				network:  "public"
				hostname: "web.demo.apprafter.dev"
			}
			needs: {
				redis: {selector: {tier: "integrated"}}
			}
		}
		environments: {
			prod: {
				replicas: 3
				needs: {
					// Persistent pool instance (snapshot→PVC) for prod.
					redis: {selector: {tier: "integrated"}, persistent: true}
				}
			}
		}
	}
}
