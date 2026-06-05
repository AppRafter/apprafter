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
			// Hand-pinned, reviewed releases: opt out of tag->digest
			// resolution so the operator renders this reference
			// verbatim and never polls the registry (ADR 0040). Drop
			// this line (or set resolve: "digest") for push->deploy
			// on a mutable tag.
			imagePolicy: {resolve: "off"}
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
