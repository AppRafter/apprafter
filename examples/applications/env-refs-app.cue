// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Example Application demonstrating the `#EnvValue` union (Phase 2.12 /
// ADR 0046): literal strings, claim references ({claim: "<type>.<field>"}),
// and external-secret references ({secret: "<name>/<key>"}) can all appear
// in the same `env` map. Used as a `cue vet` fixture for `#EnvValue` /
// `#EnvRef`. The bare-selector sugar (`claim.pg.url`) is injected at
// render/validate time by the cue-cmp / CLI — a later task.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

envRefsApp: v1alpha1.#Application & {
	metadata: {
		name:      "web"
		namespace: "demo"
	}
	spec: {
		base: {
			image:    "ghcr.io/example/web:latest"
			replicas: 1
			needs: {
				pg: {}
			}
			env: {
				// literal: a plain quoted string
				LOG_LEVEL: "info"
				// claim ref: resolved to secretKeyRef at render time
				DATABASE_URL: {claim: "pg.url"}
				DB_HOST: {claim: "pg.host"}
				// external secret ref: "<secret-name>/<key>"
				STRIPE_KEY: {secret: "stripe/api-key"}
			}
		}
	}
}
