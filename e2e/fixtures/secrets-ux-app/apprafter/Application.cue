// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// secrets-ux-walk e2e fixture — ONE base-only Application whose env
// carries a `secret:` value reference (2.12 / ADR 0046), so the whole
// D6 + D7 CLI surface has something real to describe.
//
// Style A (unwrapped): apiVersion + kind at the top level so the
// argocd-cue-cmp sidecar emits this document verbatim. The CUE module
// boundary is the sibling cue.mod/ directory, so `cue export ./...`
// works with the repo-server's cwd set to this apprafter/ directory.
// No vendored schema (2.12 / ADR 0046 #7 stopped scaffold vendoring) —
// the CMP injects the schema at render time.
//
// DELIBERATELY NO `environments` block and no `metadata.namespace`:
//
//   * no environments  -> `app add` (no --env) registers ONE Argo CD
//     Application named `shop`, the base-only deploy. Every `apprafter
//     app <verb> shop` in the walk then resolves the single deployment
//     without --env, which is the logical-name UX the walk exercises.
//   * no namespace     -> Argo CD's destination namespace (from
//     `app add --namespace`) places the CR, exactly as the per-env
//     fixture does.
//
// `imagePolicy.resolve: "off"` is LOAD-BEARING, not tidiness (ADR 0040).
// With the default `digest` policy the operator re-resolves
// `nginxdemos/hello:plain-text` to its current registry digest on every
// reconcile, and an upstream re-tag mid-walk would roll the Deployment.
// The D6 leg asserts that a rotated secret does NOT replace the pod —
// a pod replaced for an unrelated reason would read as a D6 regression
// AND would destroy the `pod.startTime < envConfig.changedAt` ordering
// the `← old config` flag is computed from. Pinning the resolution off
// removes that whole class of interference.
//
// The `secret:` payload below MUST stay in sync with the walk's
// SECRET_NAME / SECRET_KEY constants (e2e/secrets-ux-walk.sh) — the walk
// seals `shop-api` with key `token` before registering this app, then
// re-seals it under a different key to break the binding on purpose.
//
// nginxdemos/hello is used because it pulls quickly (~4 MB) and responds
// on port 80; the walk never talks to it — it only needs a pod that
// starts, stays Running, and holds its start time.

package apprafter

apiVersion: "apprafter.io/v1alpha1"
kind:       "Application"
metadata: name: "shop"
spec: base: {
	image: "nginxdemos/hello:plain-text"
	imagePolicy: resolve: "off"
	replicas: 1
	expose: port: 80
	env: {
		// A literal, so the rendered container env is not exclusively
		// references (and a broken ref cannot be confused with an empty
		// env map).
		LOG_LEVEL: "info"
		// The 2.12 braceless secret reference — already the marker form
		// `{secret: "<name>/<key>"}`, so no cue-cmp claim two-pass is
		// involved. This single binding is what D7's diagnostic explains
		// and what D6's rotation moves.
		API_KEY: secret: "shop-api/token"
	}
}
