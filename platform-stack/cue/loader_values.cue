// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// `_loaderValues` exposes the subset of values the CLI loader
// (`apprafter cluster-bootstrap`) needs to install Cilium +
// Argo CD before Argo CD takes over reconciliation from the
// platform-stack chart. The CLI's `build.rs` reads these
// fields at compile time via `cue export -e _loaderValues.<comp>`
// and emits Rust `const &str` constants the loader uses
// verbatim.
//
// Schema: each component carries
//   - `chartVersion`: upstream Helm chart version (`--version`
//     arg to `helm install`)
//   - `values`: subset of Helm values the loader install
//     passes via `helm install -f`
//
// Why hidden (`_` prefix): operators don't override these. The
// loader is an internal CLI implementation detail; chart
// users interact through `_components.*.values` overlays
// instead.
//
// Walk-fix #6 (v0.1.103) surfaced the duplication-as-drift
// class: `component_cilium.cue` carried `k8sServiceHost:
// "auto"` while the CLI loader carried `"127.0.0.1"`; Argo
// CD applied the chart values on top of the loader's
// Deployment and cilium-operator crashed with `KUBERNETES_SERVICE_HOST=auto`.
// The invariant below makes that crash impossible by
// construction — chart values ≡ loader values for Cilium.
//
// Argo CD differs: the chart adds adopt-time extras on top
// of the loader subset (cue-cmp sidecar in
// `repoServer.extraContainers`, the `cue-cmp-plugin-config`
// ConfigMap in `extraObjects`) that the CLI loader does NOT
// need on first install — Argo CD has to be running first
// before it can pull the chart that defines them. So the
// chart's `component_argocd.cue` derives its `values:` as
// `_loaderValues.argocd.values & { ...sidecar extras... }` —
// unification, not equality. There is no invariant assertion
// for Argo CD; the `& {…}` pattern at the call site already
// encodes the subset relationship.
_loaderValues: {
	cilium: {
		// Upstream Cilium chart version. Bumped in lockstep
		// with Cilium support work (single source per
		// CLAUDE.md "one way to do things").
		chartVersion: "1.16.5"
		// Cilium values — byte-identical to the chart's
		// component values (B.1.71 invariant below).
		values: _components.cilium.values
	}

	argocd: {
		// Upstream Argo CD chart version (argo/argo-cd
		// from https://argoproj.github.io/argo-helm).
		chartVersion: "7.7.7"
		// Argo CD values — strict subset of `_components.argocd.values`.
		// The chart adds the cue-cmp sidecar + its ConfigMap on
		// top via `& { ... }`; loader doesn't ship those.
		//
		// Field order matches the pre-refactor `component_argocd.cue`
		// `values:` block so YAML export round-trips byte-equivalent
		// (CUE preserves declaration order on export).
		values: {
			controller: replicas: 1
			"redis-ha": enabled:  false
			server: replicas:     1
			server: service: type: "ClusterIP"
			applicationSet: replicaCount: 1
			notifications: enabled:       false
			dex: enabled:                 false
			configs: {
				repositories: apprafter: {
					url:       "ghcr.io/apprafter"
					type:      "helm"
					enableOCI: "true"
				}
				// Second OCI repo for the operator + admission-webhook
				// Helm charts, kept OUT of the org root. The charts
				// share a name with their container images
				// (`apprafter-operator`, `apprafter-admission-webhook`);
				// pushing a chart to `ghcr.io/apprafter/<name>:<ver>`
				// with `<ver>` equal to the image tag overwrites the
				// image with the chart .tgz (the pod then crash-loops
				// `exec: "/<name>": no such file`). Charts therefore
				// publish to `ghcr.io/apprafter/charts/<name>` and the
				// components point their `repoURL` here. Argo CD
				// matches the repository by exact `repoURL`, so this
				// dedicated registration is required for the
				// `enableOCI` treatment on the sub-path.
				repositories: "apprafter-charts": {
					url:       "ghcr.io/apprafter/charts"
					type:      "helm"
					enableOCI: "true"
				}
				// `_appProjects` — shared source of truth between
				// the loader install (this block) and the
				// umbrella's `templates/appprojects.yaml`
				// manifest renderer. Walk-fix #2 post-B.1.79a
				// (v0.1.146 / chart 0.1.41): keeping both sites
				// guarantees AppProjects exist (a) on initial
				// `apprafter cluster-bootstrap` before any
				// umbrella sync has run, AND (b) on every
				// subsequent umbrella sync via standalone
				// manifests at sync-wave -30. Each definition
				// is byte-identical between the two paths.
				projects: _appProjects
			}
			repoServer: replicas: 1
		}
	}
}

// B.1.71 invariant: chart's cilium values ARE the loader values.
// CUE unifies left + right; if any future edit makes them
// diverge, `cue vet` fails with `incompatible values`.
_components: cilium: values: _loaderValues.cilium.values

// B.1.71b invariant: chart's Argo CD upstream chart version IS
// the loader's chart version. Single source of truth.
// (Cilium's version is declared inline in component_cilium.cue
// as `version: _loaderValues.cilium.chartVersion` to preserve
// YAML export field order.)
_components: argocd: version: _loaderValues.argocd.chartVersion
