// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// AppRafter Application manifest for the landing CMS (Payload 3
// + Next 15). Vets against schemas/v1alpha1/application.cue.
//
// Container image: built by
// `.github/workflows/release-landing.yml` on every `landing-v*`
// tag. The image runs Next standalone on :3000 — see
// landing/cms/Dockerfile.
//
// Image tag convention:
//   ghcr.io/apprafter/landing-cms:landing-v0.1.0   (pinned, prod)
//   ghcr.io/apprafter/landing-cms:latest           (head, dev)
//
// Replicas: 1 in both environments. Payload caches in-process
// state (importMap, server functions) that doesn't shard across
// replicas without shared session storage — adding HA is a Phase
// 2+ exercise once we move file uploads to S3 and sessions to
// Redis.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

landingCms: v1alpha1.#Application & {
	metadata: {
		name:      "landing-cms"
		namespace: "apprafter"
		labels: {
			"apprafter.io/component": "landing"
			"apprafter.io/role":      "cms"
		}
	}
	spec: {
		base: {
			// Image carries the Next build output (.next/) + node
			// runtime. Built via `bun run build:cms`.
			image:    "ghcr.io/apprafter/landing-cms:latest"
			replicas: 1
			expose: {
				port:    3000
				network:  "internal"
			}
			needs: {
				pg: {},
			},
			env: {
				DATABASE_URL: claim.pg.url
				// DATABASE_URL auto-injected via needs.pg claim
				PAYLOAD_SECRET: secret: "apprafter-landing-cms-secrets/PAYLOAD_SECRET"
				// for test via port forwarding
				LANDING_CMS_CSRF_ORIGINS: "http://localhost:8080"

				// Server URL Payload reports back in admin links,
				// password-reset emails, etc.
				PAYLOAD_PUBLIC_SERVER_URL: "https://cms.apprafter.dev"
				// LANDING_CMS_PORT keeps payload.config + Next + the
				// healthcheck on the same port. Override in dev via
				// the env-file if needed.
				LANDING_CMS_PORT: "3000"
				// CORS allowlist for the landing web origin + future
				// preview branches.
				LANDING_CMS_CORS_ORIGINS: "https://apprafter.dev"
				// SMTP envelope sender for the discovery-call hook.
				SMTP_FROM: "noreply@apprafter.dev"
			}
		}
		environments: {
			dev: {
				replicas: 1
				env: {
					PAYLOAD_PUBLIC_SERVER_URL: "http://cms.dev.apprafter.local:3000"
					LANDING_CMS_PORT:          "3000"
					LANDING_CMS_CORS_ORIGINS:  "http://localhost:4321,http://localhost:4322"
					SMTP_FROM:                 "dev@apprafter.local"
				}
			}
			prod: {
				replicas: 1
			}
		}
	}
}
