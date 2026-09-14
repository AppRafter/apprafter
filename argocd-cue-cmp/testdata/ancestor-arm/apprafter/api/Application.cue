// SPDX-License-Identifier: MIT
//
// cue-cmp ancestor-arm fixture (ADR 0063 §Decision 2). Exercises the
// arm that reads ARGOCD_APP_SOURCE_PATH, which is the ONLY way the
// entrypoint can see the part of the registered path at or ABOVE the
// working directory.
//
// The directory tree is `apprafter/api/`, so a real registration of
// `--path apprafter/api` puts the working directory here with basename
// `api` — the `${PWD##*/}` arm cannot fire, and only the environment
// variable carries the `apprafter` component.
//
// This file sits at DEPTH 1 and is NOT named `apprafter*.cue`, so it
// satisfies the ancestor arm's predicate (`-name '*.cue'`) and fails the
// other one. The sibling `helpers/apprafter-shared.cue` is the reverse.
// So the selected directory — and therefore the manifest emitted —
// differs by arm:
//
//   ARGOCD_APP_SOURCE_PATH=apprafter/api  → renders `.`        → ancestor-registered
//   ARGOCD_APP_SOURCE_PATH unset          → renders ./helpers  → ancestor-fallback
//
// Style A (unwrapped) so the marker comes from `apiVersion` itself and
// no schema injection is needed: this fixture leaves nothing behind.

package apprafter

apiVersion: "apprafter.io/v1alpha1"
kind:       "Application"
metadata: {
	name:      "ancestor-registered"
	namespace: "ancestor-demo"
}
spec: base: {
	image:    "nginxdemos/hello:plain-text"
	replicas: 1
}
