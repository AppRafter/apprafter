// SPDX-License-Identifier: MIT
//
// cue-cmp claim/secret inject fixture (2.12f / ADR 0046). The bare
// `claim.pg.url` / `claim.pg.host` / `claim.pg.main.url` selectors and
// the braceless `secret: "stripe/api-key"` form are RESOLVED by
// entrypoint.sh's schema + claim-binding injection — the user repo
// vendors NEITHER the schema nor the `claim` binding. Asserts the
// rendered Application carries {claim: "pg.url"} /
// {secret: "stripe/api-key"} markers.
//
// Coverage:
//   * base: unnamed pg claim (claim.pg.url, claim.pg.host),
//           named pg claim (claim.pg.main.url),
//           external secret (secret: "stripe/api-key").
//   * environments.dev: a redis need declared ONLY in the dev env,
//     referenced as claim.redis.url — proves the entrypoint unions
//     base.needs with environments[<active env>].needs when building
//     the `claim` binding (the per-env path, exercised with
//     APPRAFTER_APP_ENV=dev).

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

claimApp: v1alpha1.#Application & {
	metadata: {
		name:      "claim-app"
		namespace: "apprafter"
	}
	spec: {
		base: {
			image: "nginxdemos/hello:plain-text"
			needs: {
				pg: [{}, {name: "main"}]
			}
			env: {
				LOG_LEVEL:    "info"
				DATABASE_URL: claim.pg.url
				DB_HOST:      claim.pg.host
				MAIN_URL:     claim.pg.main.url
				STRIPE_KEY: secret: "stripe/api-key"
			}
		}
		environments: {
			dev: {
				needs: {
					redis: {}
				}
				env: {
					REDIS_URL: claim.redis.url
				}
			}
		}
	}
}
