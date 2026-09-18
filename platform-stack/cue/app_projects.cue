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
	// constrained to Application / ConfigMap / Secret /
	// HTTPRoute. RBAC enforcement via AppProject is not
	// active in M1.5 — Phase 4 materialises that through
	// AccessGrant. The tightening here is structural
	// foundation rather than runtime enforcement.
	//
	// Walk-fix #11 post-B.1.79a (chart 0.1.47): `Namespace`
	// added to `clusterResourceWhitelist`. Before this, an
	// empty list blocked the synthetic `Namespace` resource
	// Argo CD generates from `CreateNamespace=true` for any
	// user app whose destination namespace doesn't exist
	// yet — landing apps failed with `SyncFailed: resource
	// :Namespace is not permitted in project apps`.
	//
	// 0.2.76: the namespace-scoped list stopped being a list
	// of "the four kinds a user app renders" the moment a
	// user-declarable CRD landed that wasn't on it. The
	// whitelist was written in B.1.79a against the Phase-1
	// surface — `Application` was then the only `apprafter.io`
	// kind a bundle could hold — and neither ADR 0049
	// (`SharedVolume`, 2.6c) nor ADR 0066 (`SharedDatabase`,
	// 2.29) came back to it. Both are namespaced CRs a bundle
	// may declare beside the applications that bind them
	// (`examples/applications/shared-database.cue` is exactly
	// that shape), and both failed the whole sync with
	// `resource apprafter.io:SharedDatabase is not permitted
	// in project apps` — every workload left Missing, not just
	// the database.
	//
	// The rule the list now follows, so the next CRD doesn't
	// repeat this: a kind belongs here when a USER's bundle can
	// legitimately render it. That is the `apprafter.io` kinds
	// with a public `#Definition` a manifest instantiates, plus
	// what the platform tells people to commit beside them.
	// Operator-authored kinds (`ResourceClaim`, `MigrationPlan`,
	// `RetainedClaim`) stay off it: the operator writes them
	// with its own field manager and no bundle should be able
	// to hand Argo CD a competing copy. `SourceCredential`
	// stays off deliberately too — it is a registry credential
	// in `apprafter-system` that `apprafter registry add`
	// creates, not app payload.
	//
	// `SealedSecret` is the "commit it beside the app" half of
	// `apprafter secret seal --stdout`, which the secrets guide
	// documents for exactly that. It is strictly narrower than
	// the plaintext `Secret` already permitted above it: the
	// ciphertext is bound to its own namespace and name and is
	// readable only by the in-cluster controller's private key.
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
			group: "apprafter.io"
			kind:  "SharedDatabase"
		}, {
			group: "apprafter.io"
			kind:  "SharedVolume"
		}, {
			group: ""
			kind:  "ConfigMap"
		}, {
			group: ""
			kind:  "Secret"
		}, {
			group: "bitnami.com"
			kind:  "SealedSecret"
		}, {
			group: "gateway.networking.k8s.io"
			kind:  "HTTPRoute"
		}]
	}
}
