// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// AppRafter Application manifest for the landing site (Astro 5
// static output). Vets against schemas/v1alpha1/application.cue.
//
// Container image: built by
// `.github/workflows/release-landing.yml` on every `landing-v*`
// tag. The image bundles `landing/web/dist/` + Caddy on :80; see
// landing/web/Dockerfile.
//
// Image tag convention:
//   ghcr.io/apprafter/landing-web:landing-v0.1.0   (pinned, prod)
//   ghcr.io/apprafter/landing-web:latest           (head, dev)
//
// Postgres is intentionally not declared — the web app has no
// runtime DB dependency. The Astro build SSRs from the CMS at
// image-build time (or uses fallback JSON via
// LANDING_USE_FALLBACK=1, which is what the workflow does).

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
