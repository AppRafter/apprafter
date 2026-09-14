// SPDX-License-Identifier: MIT
//
// The counterweight for the ancestor-arm fixture. Named `apprafter*.cue`
// and one level DOWN, so it is what the non-ancestor predicate finds when
// ARGOCD_APP_SOURCE_PATH is absent — and what the ancestor arm must NOT
// select, because a depth-1 manifest short-circuits the search.
//
// Emitting a distinct metadata.name is the whole point: the assertion
// names the manifest, so it can only pass if the right directory was
// chosen.
//
// `package helpers`, NOT `package apprafter`, and that is required
// rather than stylistic: CUE unifies a package with same-named packages
// in ANCESTOR directories up to the module root, and the module root
// here is `api/`. Sharing the name would make `cue export .` from this
// directory pull in `../Application.cue` and die on
// `metadata.name: conflicting values` — measured.

package helpers

apiVersion: "apprafter.io/v1alpha1"
kind:       "Application"
metadata: {
	name:      "ancestor-fallback"
	namespace: "ancestor-demo"
}
spec: base: {
	image:    "nginxdemos/hello:plain-text"
	replicas: 1
}
