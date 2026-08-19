// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// AppRafter Application manifest for the documentation site — the
// MkDocs-Material build (`mkdocs build --strict`) served by Caddy out
// of the image `docs-site/Dockerfile` produces. Vets against
// schemas/v1alpha1/application.cue.
//
// Image streams, published by `.github/workflows/release-docs.yml`:
//
//   :latest      rolling — republished on every gated documentation
//                change on master → this manifest (Argo CD watches
//                :latest)
//   :<git sha>   immutable per-build pin, for rollback and for a
//                rollout that wants a fixed artefact
//
// The workflow's image base and this manifest's `image` are two
// copies of one string: a mismatch deploys nothing and reports
// success, so they are compared literally whenever either moves.
//
// There is no :prod stream and no docs-vX.Y.Z stream, and that is a
// considered difference from the landing rather than an omission.
// The landing's :preview → :prod retag exists because a human
// promotes CMS content that never passed CI. Documentation is
// git-resident and gated — `just lint` and .github/workflows/docs.yml
// run the same `mkdocs build --strict` and drift gate on the very
// commit that publishes it — so master IS the promoted state, and a
// second stream would be a manual step with nothing left to decide.
//
// A rolling tag redeploys because the operator resolves `base.image`'s
// tag to its current registry digest on every reconcile (ADR 0040:
// `imagePolicy.resolve` defaults to "digest"), so a moved :latest
// rolls the Deployment with no manifest edit. `imagePolicy` is
// therefore deliberately absent — the default is the wanted
// behaviour, and restating it would be a second place to change.
//
// Also deliberately absent, each for its own reason:
//
//   needs      the site is fully static. Caddy serves a tree baked
//              into the image; nothing is read or written at runtime,
//              so there is no database, cache, bucket or disk to
//              declare.
//   env        nothing is configurable at runtime either. The one
//              build-time input, `site_url`, is baked in by
//              `mkdocs build`.
//   resources  no measurement to set them from. A request invented
//              here would be a number nobody re-derived; take it off
//              a running pod first.
//
// docs.apprafter.dev needs no new zone and no new certificate: it is
// a subdomain of the already-registered `apprafter.dev` apex, and
// registering a zone gives the Gateway "an apex + wildcard `:443`
// listener pair" (docs/operator-guide/connect-a-domain.md) against an
// origin certificate minted for "`<zone>` and `*.<zone>`"
// (docs/operator-guide/cloudflare-origin-cert.md). The `docs` DNS
// record itself lives in the operator's Cloudflare account, outside
// this repository.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

docs: v1alpha1.#Application & {
	metadata: {
		// The logical name every CLI verb takes (`apprafter app
		// status docs`), the Deployment and Service name, and the
		// stem of the Argo CD application (`docs-<env>`). It matches
		// the image `ghcr.io/apprafter/docs` and the host
		// docs.apprafter.dev, so one word names the thing everywhere
		// it appears; the directory is `docs-site/` only because
		// `docs/` is the documentation corpus root.
		name: "docs"

		// The namespace landing-web and landing-cms already run in.
		// These are first-party public web properties on one cluster
		// with nothing to isolate from one another — this app holds
		// no secret, claim or volume that a namespace boundary would
		// protect. Egress is per-application (ADR 0045), not
		// per-namespace, so the boundary would buy nothing there
		// either.
		namespace: "apprafter"

		// Discoverability only (`kubectl get app -L`); nothing
		// consumes these. Deliberately WITHOUT the landing's
		// `apprafter.io/hostname` label: that predates 1.83b, when
		// the hostname became a consumed field (`expose.hostname`
		// below) rather than a hint for a Gateway layer that did not
		// exist yet. `git grep apprafter.io/hostname` finds only the
		// two landing manifests and docsgen's identifier fixtures,
		// and plan.md's landing-migration item calls for dropping it
		// there — copying it here would add a second, unreconciled
		// copy of the hostname.
		labels: {
			"apprafter.io/component": "docs"
			"apprafter.io/role":      "web"
		}
	}
	spec: {
		base: {
			image: "ghcr.io/apprafter/docs:latest"

			// Two pods so that a single eviction or crash-restart
			// does not blank the site — the Service keeps an
			// endpoint while the other is rescheduled. On a
			// single-node tier-1 cluster that is pod-level
			// resilience only; losing the node loses both. Cheap
			// either way: the container is Caddy plus a static tree.
			replicas: 2

			expose: {
				// Caddy listens on :80 inside the image
				// (docs-site/Caddyfile's site block, and the
				// Dockerfile's EXPOSE). TLS terminates on the
				// platform Gateway's :443 listener, not here, which
				// is why `tls` is left at its default.
				port:     80
				network:  "public"
				hostname: "docs.apprafter.dev"
			}
		}

		// Only `dev` deviates. There is deliberately no `prod` entry:
		// base IS the production shape, and an override restating it
		// would silently pin the old value the first time base moves.
		// An environment absent from this map deploys base alone —
		// that is the documented behaviour of `spec.environment`, not
		// a gap.
		environments: {
			// One replica, and no public route: two Applications
			// claiming docs.apprafter.dev would emit two HTTPRoutes
			// for one host. The `hostname` inherited from base is
			// inert here — the operator reads it only when the
			// effective network is `public`.
			dev: {
				replicas: 1
				expose: network: "internal"
			}
		}
	}
}
