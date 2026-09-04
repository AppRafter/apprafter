// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// env-and-secrets-walk e2e fixture — ONE base-only Application that
// carries BOTH halves the merged walk proves (e2e/env-and-secrets-walk.sh):
//
//   * `needs.pg`  -> the 2.4 claim chain provisions a connection Secret
//     with the DECOMPOSED keys `url user pass host port db`
//     (2.12 / ADR 0046 #3), which is what gives the CLAIM-derived env
//     refs below something real to resolve against. Without a declared
//     need there is no connection Secret and the claim refs cannot be
//     asserted at all.
//   * an `env` map carrying ALL THREE ADR-0046 sources in one container:
//     a LITERAL, three CLAIM refs (composed `url` plus decomposed
//     `user`/`pass`), and one EXTERNAL `secret:` ref.
//
// Style A (unwrapped): apiVersion + kind at the top level so the
// argocd-cue-cmp sidecar emits this document verbatim. The CUE module
// boundary is the sibling cue.mod/ directory, so `cue export ./...`
// works with the repo-server's cwd set to this apprafter/ directory.
// No vendored schema (2.12 / ADR 0046 #7 stopped scaffold vendoring) —
// the CMP injects the schema AND the generated `claim` binding at render
// time, which is what turns the bare `claim.pg.url` selectors below into
// the CR-level markers `{claim: "pg.url"}`.
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
// SECRET_NAME / SECRET_KEY constants (e2e/env-and-secrets-walk.sh) — the
// walk seals `shop-api` with key `token` before registering this app,
// then re-seals it under a different key to break the binding on purpose.
// The claim refs likewise pin the walk's CONN_SECRET / PG_ROLE constants,
// which are derived from `(namespace, <app>-pg)` by the 2.4c provisioner.
//
// nginxdemos/hello is used because it pulls quickly (~4 MB), responds on
// port 80 and ships a shell + `printenv` (the walk execs it to read the
// RESOLVED env values). The walk never talks to it over HTTP — it only
// needs a pod that starts, stays Running, and holds its start time.

package apprafter

apiVersion: "apprafter.io/v1alpha1"
kind:       "Application"
metadata: name: "shop"
spec: base: {
	image: "nginxdemos/hello:plain-text"
	imagePolicy: resolve: "off"
	replicas: 1
	expose: port: 80

	// The declared Postgres dependency. `selector` + `size` are given
	// explicitly (rather than leaning on the controller's injected
	// `{tier: integrated}` default) so the walk's Phase-2 assertion that
	// the seeded `pg-integrated` ServiceProvider carries `tier=integrated`
	// is what the scheduler actually matches on.
	needs: pg: {
		selector: tier: "integrated"
		size: "small"
	}

	env: {
		// A literal, so the rendered container env is not exclusively
		// references (and a broken ref cannot be confused with an empty
		// env map). The walk asserts this stays a plain `value:` and does
		// NOT become a secretKeyRef.
		LOG_LEVEL: "info"

		// The 2.12 BARE claim selectors. The cue-cmp's generated `claim`
		// binding resolves each to a CR-level marker (`{claim: "pg.url"}`
		// …); the operator's `resolve_env` then expands the marker into a
		// container `EnvVar{valueFrom: secretKeyRef}` pointing at the
		// provisioned connection Secret's matching DECOMPOSED key.
		//
		// DATABASE_URL is declared EXPLICITLY on purpose: the 2.4e implicit
		// injection was removed in ADR 0046 #5, and the walk asserts there
		// is EXACTLY ONE env entry by that name — the one written here.
		DATABASE_URL: claim.pg.url
		DB_USER:      claim.pg.user
		DB_PASS:      claim.pg.pass

		// The 2.12 braceless secret reference — already the marker form
		// `{secret: "<name>/<key>"}`, so no cue-cmp claim two-pass is
		// involved. This single binding is what D7's diagnostic explains
		// and what D6's rotation moves.
		API_KEY: secret: "shop-api/token"
	}
}
