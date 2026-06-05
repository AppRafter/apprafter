// SPDX-License-Identifier: MIT
//
// AppRafter Application manifest for the bun-http-starter.
// Vets against schemas/v1alpha1/application.cue. Operators
// replace the placeholder `image` with their registry/tag and
// commit the file into their bootstrap repo (Argo CD picks it
// up via spec.argocd.bootstrapRepo from cluster-bootstrap).

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

bunHttpStarter: v1alpha1.#Application & {
	metadata: {
		name:      "bun-http-starter"
		namespace: "default"
	}
	spec: {
		base: {
			image: "ghcr.io/your-org/bun-http-starter:0.1.0"
			// push->deploy default (ADR 0040): the operator resolves
			// this tag to a registry digest each reconcile, so a fresh
			// push to a mutable tag rolls the Deployment automatically.
			// `digest` is also the default when imagePolicy is omitted.
			// Set resolve: "off" to opt out for hand-pinned, reviewed
			// releases — the operator then renders the reference verbatim
			// and never polls the registry.
			imagePolicy: {resolve: "digest"}
			replicas: 1
			expose: {
				port:    3000
				public:  false
				network: "internal"
			}
			env: {
				LOG_LEVEL: "info"
			}
		}
		environments: {
			prod: {
				replicas: 3
			}
		}
	}
}
