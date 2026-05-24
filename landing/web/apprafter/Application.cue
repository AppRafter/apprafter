// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// AppRafter Application manifest for the landing site (Astro 5
// static output). Vets against schemas/v1alpha1/application.cue.
//
// Image streams (see .github/workflows/landing-*.yml):
//
//   :prod              promoted preview, served at apprafter.dev
//                      → this manifest (Argo CD watches :prod)
//   :latest            alias of :prod (every promote retags both)
//   :landing-vX.Y.Z    pinned release from landing-vX.Y.Z tag
//   :preview           every CMS save — separate Application
//                      (see Application-preview.cue, TBD) at
//                      preview.apprafter.dev with basic-auth
//                      gating
//
// Promotion: admin ticks Publishing.promoteToProd in the CMS →
// landing-promote-to-prod.yml retags :preview → :prod (byte-
// identical image, just a registry tag move).
//
// Postgres is intentionally not declared — the web app has no
// runtime DB dependency. The image-build SSRs from the live CMS
// when LANDING_USE_FALLBACK=0 (preview-build path) or from JSON
// fallbacks when =1 (release-landing path).

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

landingWeb: v1alpha1.#Application & {
	metadata: {
		name:      "landing-web"
		namespace: "apprafter"
		labels: {
			"apprafter.io/component": "landing"
			"apprafter.io/role":      "web"
			"apprafter.io/hostname":  "apprafter.dev"
		}
	}
	spec: {
		base: {
			// :prod is the rolling prod tag — bumped both by
			// promotes (preview → prod retag) and by tagged
			// releases. Pin to :landing-vX.Y.Z for full
			// determinism in regulated rollouts.
			image:    "ghcr.io/apprafter/landing-web:prod"
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
