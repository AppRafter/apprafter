// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// AppRafter Application manifest for the landing site (Astro 5
// static output). Vets against schemas/v1alpha1/application.cue.
//
// Built artefact: `bun run build:web` produces landing/web/dist/
// which is baked into the container image at deploy time. The
// container's static-file server (Caddy / nginx — left to the
// image builder) listens on :80 and serves dist/.
//
// Postgres is intentionally not declared — the web app has no
// runtime DB dependency. It does need to reach the cms host at
// build time for the SSR Payload fetch, but that happens inside
// the CI image-build step (PUBLIC_CMS_URL env), not at runtime.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

landingWeb: v1alpha1.#Application & {
	metadata: {
		name:      "landing-web"
		namespace: "apprafter"
		labels: {
			"apprafter.io/component": "landing"
			"apprafter.io/role":      "web"
		}
	}
	spec: {
		base: {
			// Replace with the actual GHCR tag pinned per release.
			// The image bundles dist/ + a static file server; no
			// node runtime needed at request time.
			image:    "ghcr.io/apprafter/landing-web:latest"
			replicas: 2
			expose: {
				port:    80
				public:  true
				network: "public"
			}
			// No runtime env — the page is fully prerendered.
			// PUBLIC_CMS_URL is baked in during `astro build`.
		}
		environments: {
			// Single replica is enough for the dev cluster.
			dev: {
				replicas: 1
				expose: {
					port:    80
					public:  false
					network: "internal"
				}
			}
			// Two replicas in prod for rolling restarts without
			// a window of unavailability. Astro output is static,
			// so the floor can stay low.
			prod: {
				replicas: 2
			}
		}
	}
}
