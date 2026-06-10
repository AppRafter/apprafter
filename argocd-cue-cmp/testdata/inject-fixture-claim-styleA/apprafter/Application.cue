// SPDX-License-Identifier: MIT
//
// cue-cmp Style-A claim fixture (2.12f / ADR 0046). UNWRAPPED layout:
// apiVersion/kind/spec are package-scope fields (no named wrapper), so
// the injected top-level `claim` binding is a SIBLING of the manifest
// fields. Regression guard: the entrypoint MUST strip `claim` from the
// Style-A emission (else it leaks into the rendered Application as
// `claim: {…}`), while still resolving the bare selectors.

package apprafter

apiVersion: "apprafter.io/v1alpha1"
kind:       "Application"
metadata: {
	name:      "style-a-claim"
	namespace: "apprafter"
}
spec: base: {
	image: "nginxdemos/hello:plain-text"
	needs: {
		pg: {}
	}
	env: {
		DATABASE_URL: claim.pg.url
		STRIPE_KEY: secret: "stripe/api-key"
	}
}
