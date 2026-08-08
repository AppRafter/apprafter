// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// 2.16c expose deep-merge e2e fixture — one AppRafter Application
// manifest deployed per environment (subphase 2.16c). Proves that a
// per-environment `expose` override is now PARTIAL: an env carries
// only the diff and deep-merges onto `base.expose`.
//
// Style A (unwrapped): apiVersion + kind declared at the top level so
// the cue-cmp entrypoint emits this document verbatim (then stamps
// spec.environment + the apprafter.io/environment label when
// APPRAFTER_APP_ENV is set). The CUE module boundary is in the sibling
// cue.mod/ directory so `cue export ./...` works when the repo-server's
// cwd is this apprafter/ directory.
//
// DELIBERATELY NO metadata.namespace: ADR 0044 places each
// env-deployment in its OWN, user-chosen namespace (`app add
// --namespace web-dev|web-prod`); the AppRafter CR keeps the bare
// `web` name in each.
//
// Per-env expose difference (the 2.16c load-bearing point):
//   * base / prod — expose { port: 8080, network: "public",
//                            hostname: "x.example.com" }
//                    → public HTTPRoute on x.example.com
//   * dev         — expose { network: "internal" }   ← DIFF ONLY
//                    → inherits port 8080 (+ hostname, but network
//                      internal makes the hostname inert), NO public
//                      HTTPRoute.
//
// Pre-2.16c, the dev override had to re-declare the WHOLE expose incl.
// `port` (base.expose.port is required); 2.16c makes the override a
// partial `#ApplicationEnvOverride` (expose.port OPTIONAL), so the env
// carries only `{network:"internal"}` and inherits `port`.
//
// nginxdemos/hello:plain-text listens on 80, but the workload port is
// irrelevant to the assertion — we prove the RENDERED container/Service
// port equals the INHERITED base value (8080), i.e. the deep-merge, not
// a live HTTP response.

package apprafter

apiVersion: "apprafter.io/v1alpha1"
kind:       "Application"
metadata: name: "web"
spec: {
	base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
		expose: {
			port:     8080
			network:  "public"
			hostname: "x.example.com"
		}
	}
	environments: {
		// prod re-uses base verbatim (no expose diff) — stays public.
		prod: {
			replicas: 1
		}
		// dev override carries ONLY the diff: flip to internal. `port`
		// (and `hostname`) are inherited from base.expose by the
		// operator's effective_spec deep-merge (2.16c). Because the
		// effective network is `internal`, the inherited hostname is
		// inert and NO public HTTPRoute is emitted.
		dev: {
			expose: {
				network: "internal"
			}
		}
	}
}
