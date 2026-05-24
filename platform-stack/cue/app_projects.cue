// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// `_appProjects` — shared shape for the four Argo CD
// `AppProject` resources the umbrella chart guarantees exist
// in the `argocd` namespace. Walk-fix #2 post-B.1.79a
// (v0.1.146 / chart 0.1.41) added this to avoid the timing
// race where child Applications at sync-wave 0 reference
// projects that only exist inside the argocd subchart's
// sync-wave -15 reconcile.
//
// **Two sites consume this map:**
//
//   1. `templates/appprojects.yaml` (umbrella chart) emits one
//      `kind: AppProject` per entry at sync-wave -30 — earliest
//      possible, before even Cilium at -20. Ensures the
//      projects exist before any child Application references
//      them on every umbrella sync.
//
//   2. `_loaderValues.argocd.values.configs.projects` ships the
//      same definitions into the argocd subchart's startup
//      config — guarantees the projects exist on the initial
//      `apprafter cluster-bootstrap` install, before any
//      umbrella sync has run.
//
// On steady state both sites describe the same resources;
// Argo CD's reconciler treats them as one (same
// group/kind/name/namespace), so the duplication is
// idempotent.
//
// `default` is the legacy unrestricted project retained for
// ad-hoc Applications operators apply outside the umbrella
// pipeline.
_appProjects: {
	default: #AppProjectSpec & {
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

	// `platform` — for the core chart-managed components
	// (cilium, argocd self-adopt, cert-manager, network-
	// policies, apprafter-operator, admission-webhook,
	// backstage, argocd-cue-cmp). `#Component.project`
	// defaults to "platform" so existing components land
	// here automatically.
	platform: #AppProjectSpec & {
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

	// `platform-providers` — reserved for ServiceProvider
	// operators (CNPG, Dragonfly, NATS, Kamaji…) which land
	// in Phase 2+. Created early so the project selector in
	// Argo CD UI surfaces it on a fresh bootstrap rather
	// than appearing only when the first provider lands.
	"platform-providers": #AppProjectSpec & {
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

	// `apps` — user Applications registered via `apprafter
	// app add`. Tightened relative to `platform`: in-cluster
	// destinations only, narrow cluster-scoped surface
	// (`Namespace` only, so `CreateNamespace=true` in the
	// wizard-generated syncOptions can create destination
	// namespaces; nothing else is cluster-scoped that user
	// apps should touch), namespace-scoped resource whitelist
	// constrained к Application / ConfigMap / Secret /
	// HTTPRoute. RBAC enforcement через AppProject is not
	// active в M1.5 — Phase 4 materialises that through
	// AccessGrant. The tightening here is structural
	// foundation rather than runtime enforcement.
	//
	// Walk-fix #11 post-B.1.79a (chart 0.1.47): `Namespace`
	// added к `clusterResourceWhitelist`. Before this, an
	// empty list blocked the synthetic `Namespace` resource
	// Argo CD generates from `CreateNamespace=true` for any
	// user app whose destination namespace doesn't exist
	// yet — landing apps failed with `SyncFailed: resource
	// :Namespace is not permitted in project apps`.
	apps: #AppProjectSpec & {
		description: "User applications registered via `apprafter app add`."
		sourceRepos: ["*"]
		destinations: [{
			namespace: "*"
			server:    "https://kubernetes.default.svc"
		}]
		clusterResourceWhitelist: [{
			group: ""
			kind:  "Namespace"
		}]
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
