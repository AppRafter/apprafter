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
				projects: {
					// `default` стартовала как единственный AppProject
					// в чарте (v0.1.7 hotfix); сохранён как
					// неограниченный fallback для ad-hoc Applications,
					// которые юзеры могут применить вне платформенного
					// chart pipeline'а. Платформенные компоненты сами
					// переехали в `platform` в чарте 0.1.40
					// (Track B.1.79a).
					default: {
						description: "Default project — Argo CD baseline, unrestricted (legacy + ad-hoc fallback)."
						sourceRepos: ["*"]
						destinations: [{
							namespace: "*"
							server:    "*"
						}]
						clusterResourceWhitelist: [{
							group: "*"
							kind:  "*"
						}]
						namespaceResourceWhitelist: [{
							group: "*"
							kind:  "*"
						}]
					}

					// `platform` — для core platform components
					// рендеримых umbrella chart'ом (cilium,
					// argocd self-adopt, cert-manager, network-
					// policies, operator, admission-webhook,
					// backstage, argocd-cue-cmp). #Component-ы
					// получают `project: "platform"` по дефолту.
					// sourceRepos широкие — компоненты тянутся
					// откуда угодно (Argo CD upstream chart с
					// argoproj.github.io, cilium с helm.cilium.io,
					// cert-manager с charts.jetstack.io, наш OCI
					// pull для apprafter-operator). RBAC через
					// AppProject не enforces в M1.5 — Phase 4
					// материализует через AccessGrant.
					platform: {
						description: "Platform components — umbrella chart payload."
						sourceRepos: ["*"]
						destinations: [{
							namespace: "*"
							server:    "https://kubernetes.default.svc"
						}]
						clusterResourceWhitelist: [{
							group: "*"
							kind:  "*"
						}]
						namespaceResourceWhitelist: [{
							group: "*"
							kind:  "*"
						}]
					}

					// `platform-providers` — для ServiceProvider
					// operators (CNPG, Dragonfly, NATS, Kamaji…)
					// которые приедут в Phase 2. Разделение
					// чисто визуальное + lifecycle-категорийное:
					// permissions те же что у `platform`. Project
					// заводится сейчас (а не лениво в Phase 2),
					// чтобы операторы видели его в UI selector
					// сразу после bootstrap'а и не возникало
					// удивления когда provider'ы посыпятся туда
					// поштучно.
					"platform-providers": {
						description: "Platform service providers (CNPG, Dragonfly, NATS, Kamaji, …)."
						sourceRepos: ["*"]
						destinations: [{
							namespace: "*"
							server:    "https://kubernetes.default.svc"
						}]
						clusterResourceWhitelist: [{
							group: "*"
							kind:  "*"
						}]
						namespaceResourceWhitelist: [{
							group: "*"
							kind:  "*"
						}]
					}

					// `apps` — для пользовательских Applications
					// зарегистрированных через `apprafter app add`.
					// Ужесточено по сравнению с `platform`:
					// destinations лочены на in-cluster API
					// server, clusterResourceWhitelist пустой
					// (юзеры не создают cluster-scoped ресурсы),
					// namespaceResourceWhitelist ограничен типами
					// которые user-app может легитимно
					// применить (`Application` + `ConfigMap` +
					// `Secret` + `HTTPRoute`). Webhook + Argo CD
					// RBAC сейчас не enforce'ят это (нет AccessGrant
					// в M1.5), но whitelist уже стоит — кладёт
					// фундамент под Phase 4 без миграции в момент
					// его enforcement'а.
					apps: {
						description: "User applications registered via `apprafter app add`."
						sourceRepos: ["*"]
						destinations: [{
							namespace: "*"
							server:    "https://kubernetes.default.svc"
						}]
						clusterResourceWhitelist: []
						namespaceResourceWhitelist: [{
							group: "apprafter.io"
							kind:  "Application"
						}, {
							group: ""
							kind:  "ConfigMap"
						}, {
							group: ""
							kind:  "Secret"
						}, {
							group: "gateway.networking.k8s.io"
							kind:  "HTTPRoute"
						}]
					}
				}
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
