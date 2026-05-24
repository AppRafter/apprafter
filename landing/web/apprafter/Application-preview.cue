// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// AppRafter Application manifest for the landing PREVIEW host
// (Astro 5 static output, same image base as prod). Vets against
// schemas/v1alpha1/application.cue.
//
// What's different from Application.cue (the prod sibling):
//
//   image      → :preview (every CMS save rebuilds this)
//   name       → landing-web-preview
//   labels     → role=web-preview
//
// What's the same:
//
//   namespace, port, basic shape — preview should reproduce prod
//   exactly except for the content baked in. Argo CD watches
//   :preview here, watches :prod in the sibling manifest.
//
// Gating preview.apprafter.dev:
//
//   v1alpha1 doesn't model HTTP middleware (basic-auth, IP
//   allowlist, etc.) — those live at the Gateway/Caddy layer.
//   See landing/DEPLOY.md for the Caddyfile snippet that
//   protects preview.apprafter.dev with basic-auth so search
//   engines and casual visitors never see unreleased copy.
//
// Hostname routing (preview.apprafter.dev → this Application)
// is also a platform-side concern — v1alpha1's expose has only
// port/public/network. Until the Gateway shape lands in 2.x,
// the binding is done by the deploy host's outer Caddy via the
// labels declared here.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

landingWebPreview: v1alpha1.#Application & {
	metadata: {
		name:      "landing-web-preview"
		namespace: "apprafter"
		labels: {
			"apprafter.io/component": "landing"
			"apprafter.io/role":      "web-preview"
			// Hint for the Gateway layer: which public host this
			// Application should be reachable under. Not consumed
			// by v1alpha1 yet — kept as a label so the binding is
			// discoverable from `kubectl get app -L`.
			"apprafter.io/hostname": "preview.apprafter.dev"
		}
	}
	spec: {
		base: {
			// Rolling preview tag — bumped by
			// .github/workflows/landing-preview-build.yml on
			// every Payload content-global save.
			image:    "ghcr.io/apprafter/landing-web:preview"
			replicas: 1
			expose: {
				port:    80
				public:  true
				network: "public"
			}
		}
		environments: {
			dev: {
				replicas: 1
				expose: {
					port:    80
					public:  false
					network: "internal"
				}
			}
			// Single replica — preview is a low-traffic
			// internal-review host.
			prod: {
				replicas: 1
			}
		}
	}
}
