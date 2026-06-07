// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// per-env gitops-walk e2e fixture — one AppRafter Application
// manifest deployed per environment (subphase 2.9, ADR 0044).
//
// Style A (unwrapped): apiVersion + kind declared at the top level
// so the cue-cmp entrypoint emits this document verbatim (then
// stamps spec.environment + the apprafter.io/environment label when
// APPRAFTER_APP_ENV is set). The CUE module boundary is in the
// sibling cue.mod/ directory so `cue export ./...` works when the
// repo-server's cwd is this apprafter/ directory.
//
// DELIBERATELY NO metadata.namespace: ADR 0044 places each
// env-deployment in its OWN, user-chosen namespace (`app add
// --namespace web-dev|web-prod`); the AppRafter CR keeps the bare
// `web` name in each. Argo CD's destination namespace (from the
// Argo Application's spec.destination.namespace, which the CLI sets
// from --namespace) supplies the namespace at apply time, so the
// SAME rendered manifest lands self-contained in two namespaces.
//
// Per-env difference (assertable by the walk):
//   * base        — replicas 1                (the un-pinned default)
//   * dev         — replicas 1, env.TIER=dev
//   * prod        — replicas 2, env.TIER=prod
// The operator unifies environments[<env>] onto base before
// rendering (operator-rendering effective_spec), so the dev
// Deployment renders replicas 1 + TIER=dev and the prod Deployment
// replicas 2 + TIER=prod from this single source.
//
// nginxdemos/hello is used because it pulls quickly (~4 MB) and
// responds on port 80.

package apprafter

apiVersion: "apprafter.io/v1alpha1"
kind:       "Application"
metadata: name: "web"
spec: {
	base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
		expose: {
			port:   80
			public: false
		}
	}
	environments: {
		dev: {
			replicas: 1
			env: TIER: "dev"
		}
		prod: {
			replicas: 2
			env: TIER: "prod"
		}
	}
}
