// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// Vertical Pod Autoscaler (VPA) — right-sizes app requests in-place (2.16e /
// ADR 0054). Official upstream chart `vertical-pod-autoscaler` 0.11.0 (VPA
// appVersion 1.7.1). The operator emits one VerticalPodAutoscaler per managed
// app-env (updateMode InPlace, RequestsOnly, containerName "*"); this component
// only installs the three controllers + the CRDs.
//
// Self-managed webhook cert (certGen + registerWebhook:false, the CNPG
// precedent) — NO cert-manager dependency, so no ClusterIssuer-at-wave-0
// ordering trap and no caBundle Argo drift. syncWave -4: the ONLY ordering
// constraint is VPA CRDs Established before the operator emits any VPA CR, and
// -4 < 0 (workloads) satisfies it (-5 holds CNPG/Dragonfly operators, not any
// AppRafter CRD).
//
// In-place resize is an upstream ALPHA feature gated behind
// `--feature-gates=InPlaceOrRecreate=true` on the updater + admission
// controller — re-read the gate name on every chart bump. `failurePolicy:
// Ignore` (chart default) keeps a down admission pod from deadlocking
// cluster-wide pod creation. Recommender resources are load-bearing — MEASURE
// on a real node (2.16e T13) and pin; the values below are a generous seed.
_components: "vpa": #Component & {
	name:      "vpa"
	enabled:   bool | *true
	namespace: "vpa"
	project:   "platform"
	source: {
		repoURL: "https://kubernetes.github.io/autoscaler"
		chart:   "vertical-pod-autoscaler"
	}
	version: "0.11.0"
	values: {
		admissionController: {
			replicas:        1
			registerWebhook: false
			certGen: enabled: true
			mutatingWebhookConfiguration: {
				failurePolicy: "Ignore"
				// H3: never intercept control-plane pod CREATEs — a down
				// admission pod there could deadlock the cluster (failurePolicy
				// Ignore already fails open, this narrows the surface too).
				namespaceSelector: matchExpressions: [{
					key:      "kubernetes.io/metadata.name"
					operator: "NotIn"
					values: ["kube-system"]
				}]
			}
			extraArgs: ["--feature-gates=InPlaceOrRecreate=true"]
			// 2.16d: no platform pod BestEffort (the admission controller is a
			// lightweight webhook).
			resources: {
				requests: {cpu: "25m", memory: "32Mi"}
				limits: memory: "128Mi"
			}
		}
		recommender: {
			extraArgs: [
				"--memory-aggregation-interval=24h",
				"--memory-aggregation-interval-count=14",
				"--memory-histogram-decay-half-life=168h",
				"--memory-saver=true",
			]
			resources: {
				// measured idle ~12Mi (2.16e T13 walk); sized above that for the
				// recommender's per-VPA histogram growth on larger clusters.
				requests: {
					cpu:    "50m"
					memory: "64Mi"
				}
				limits: memory: "256Mi"
			}
		}
		updater: {
			replicas: 1
			extraArgs: [
				"--in-place-skip-disruption-budget=true",
				"--feature-gates=InPlaceOrRecreate=true",
			]
			// 2.16d: no platform pod BestEffort (the updater watches + patches).
			resources: {
				requests: {cpu: "25m", memory: "32Mi"}
				limits: memory: "128Mi"
			}
		}
	}

	syncWave: -4

	ignoreDifferences: [{
		group: "apps"
		kind:  "Deployment"
		jsonPointers: ["/status/terminatingReplicas"]
	}]
}
