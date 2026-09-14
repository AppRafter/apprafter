// SPDX-License-Identifier: MIT
//
// cue-cmp intra-bundle consistency fixture (ADR 0063 §Decision 5) — the
// NEGATIVE case for the cross-workload checks: one AppRafter workload
// beside an object that is NOT one.
//
// `kind: "Application"` alone does not make something a workload of this
// bundle. Argo CD's own CRD is `argoproj.io/v1alpha1, kind: Application`
// — the one foreign apiVersion that collides exactly with ours — and a
// package may legitimately ship one next to the workload it registers.
// It has its own namespace (`argocd`, where Argo CD's Applications must
// live) and no `spec.environment`, so a filter keyed on `kind` alone
// reads this package as TWO namespaces and TWO environments and refuses
// a bundle that is not inconsistent at all.
//
// The refusal direction is safe — nothing is applied — but it is a
// behaviour change from before the guards landed, and broader than ADR
// 0063 §Decision 5, whose table speaks of WORKLOADS. So the row filter
// carries an apiVersion predicate and this fixture pins it: rc=0, BOTH
// documents emitted.
//
// Deliberately NOT extended to the mixed-style guard, which matches any
// apiVersion+kind child whatever its API group — correctly, because such
// a child really would ride out as a stray top-level key and be pruned
// in silence no matter whose group it belongs to.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

web: v1alpha1.#Application & {
	metadata: {
		name:      "foreign-web"
		namespace: "apprafter"
	}
	spec: {
		environment: "prod"
		base: {
			image:    "nginxdemos/hello:plain-text"
			replicas: 1
		}
	}
}

argoApp: {
	apiVersion: "argoproj.io/v1alpha1"
	kind:       "Application"
	metadata: {
		name:      "foreign-argo"
		namespace: "argocd"
	}
	spec: {
		project: "default"
		destination: {
			server:    "https://kubernetes.default.svc"
			namespace: "apprafter"
		}
		source: {
			repoURL:        "https://example.invalid/repo.git"
			targetRevision: "HEAD"
			path:           "apprafter"
		}
	}
}
