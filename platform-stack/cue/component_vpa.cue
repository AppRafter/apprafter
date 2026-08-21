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
// `--feature-gates=InPlace=true` on the updater + admission controller.
//
// The gate is named `InPlace`, matching the `updateMode` the operator
// renders. It was `InPlaceOrRecreate` when this component was written, and
// upstream renamed it; 1.7.1 rejects the old name outright — not with a
// warning, but by refusing to start. ADR 0054 anticipated exactly this
// ("semantics moved between VPA minors — re-read the gate name on every
// chart bump") and the instruction was not followed, so both the updater
// and the admission controller sat in CrashLoopBackOff from the day this
// component shipped until 2026-08-21. The recommender was unaffected, so
// recommendations kept accruing and nothing ever applied them.
//
// Two things made that silent, and both are worth knowing before changing
// anything here. `failurePolicy: Ignore` on the admission webhook — correct,
// it stops a down admission pod deadlocking cluster-wide pod creation — also
// means a dead webhook admits VPA objects unmutated rather than failing. And
// the CRDs install from the chart independently of the controllers, so every
// "is VPA installed?" check that looks for the CRD passes while nothing runs.
// **The tell is `kubectl -n vpa get pods`, not the CRD.**
//
// The valid gates are printed by the binary itself on a bad one — read them
// from the crash log rather than from upstream docs, which describe a
// different minor:
//
//   kubectl -n vpa logs deploy/vpa-vertical-pod-autoscaler-updater \
//     | grep -A6 'feature-gates mapStringBool'
//
// Recommender resources are load-bearing — MEASURE on a real node
// (2.16e T13) and pin; the values below are a generous seed.
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
			// A PDB with minAvailable:1 against replicas:1 can never be
			// satisfied — `kubectl drain` blocks forever on a node whose
			// only copy of the pod is the one being evicted. The chart
			// ships all three enabled (`*.podDisruptionBudget.enabled`).
			// `minAvailable: 0` is NOT the alternative: the template gates
			// on truthiness, so 0 renders a selector-only PDB that permits
			// everything while reading as misconfigured to the next
			// operator.
			//
			// THIS TRADE-OFF IS ONLY VALID AT ONE REPLICA. If any of the
			// three components is ever raised above `replicas: 1` — a
			// multi-node tier overlay is the likely trigger, see
			// tier_team.cue — re-enable that component's PDB in the same
			// edit. The admission controller is the sharp case: at two
			// replicas with no PDB a drain can evict both webhook pods at
			// once, and the `failurePolicy: Ignore` below means the cluster
			// then admits VPA objects UNMUTATED with no error — the same
			// silent-failure class this file's header warns about.
			podDisruptionBudget: enabled: false
			certGen: {
				enabled: true
				// 2.16d: the one-shot cert-generation Job pod must not be
				// BestEffort either (transient, but the invariant is absolute).
				resources: {
					requests: {cpu: "10m", memory: "16Mi"}
					limits: memory: "64Mi"
				}
			}
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
			extraArgs: ["--feature-gates=InPlace=true"]
			// 2.16d: no platform pod BestEffort (the admission controller is a
			// lightweight webhook).
			resources: {
				requests: {cpu: "25m", memory: "32Mi"}
				limits: memory: "128Mi"
			}
		}
		recommender: {
			// The other two components in this file pin `replicas: 1`; this
			// one did not, and the upstream chart defaults ALL THREE to 2
			// (`recommender.replicas`). A second recommender on a
			// single-node cluster is a hot standby with no failure domain to
			// stand by for — the chart auto-enables leader election above
			// one replica — so it costs 50m + 64Mi of node allocatable to do
			// nothing. That is the RESERVATION, which is the scarce thing on
			// a saturated node; live usage was 19Mi (leader) and 8Mi
			// (standby), two pods sharing one ReplicaSet hash.
			replicas: 1
			// See `admissionController.podDisruptionBudget` for why.
			podDisruptionBudget: enabled: false
			extraArgs: [
				"--memory-aggregation-interval=24h",
				"--memory-aggregation-interval-count=14",
				"--memory-histogram-decay-half-life=168h",
				"--memory-saver=true",
				//
				// 2026-08-21 incident: upstream's own minimum-recommendation
				// floors are `--pod-recommendation-min-memory-mb=250` and
				// `--pod-recommendation-min-cpu-millicores=25` (VPA 1.7.1
				// pkg/recommender/config/config.go). Re-read both defaults on
				// every chart bump: the pin below fixes the BEHAVIOUR, but the
				// numbers quoted here go stale silently. Despite the `-mb`
				// spelling the flag is MiB — upstream multiplies by 1024², so
				// 250 renders as `250Mi`.
				//
				// Unset, they made EVERY VPA on a live T1 cluster report an
				// identical 250Mi/25m target across workloads whose real
				// working sets ranged 6Mi-82Mi. `minAllowed: 32Mi` could not
				// bind under any input — structurally dead code, shadowed by
				// an invisible floor 7.8x higher. (`maxAllowed: 512Mi` is a
				// different case: it still binds whenever a recommendation
				// exceeds it, and simply never did here because every
				// recommendation sat at the floor. The documented
				// `[32Mi, 512Mi]` correction range is real — see
				// docs/dev-guide/resources-and-autoscaling.md.)
				//
				// Pinning the memory floor to the 2.16d seed (32Mi) removes
				// the shadow. NOTE THE COUPLING THIS CREATES: the floor is now
				// hard-pinned in chart source, so `minAllowed` is authoritative
				// only UPWARD. Raise it above 32Mi and it binds; lower it below
				// and this pin silently overrides it — the same failure mode
				// this change fixes, relocated from 250Mi to 32Mi. The two must
				// move together (compiled-in default: `default_autoscale_config`
				// in operator/operator-controllers/application/src/lib.rs; per-cluster
				// override: PlatformStack.spec.resources.autoscale.minAllowed).
				//
				// The CPU floor is pinned at the value it already defaults to.
				// That is deliberate: it buys immunity from a silent upstream
				// default change on the next chart bump — the failure mode that
				// produced BOTH this bug and the feature-gate bug amended into
				// ADR 0054 on the same day. That coincidence, upstream's CPU
				// default of 25m being exactly the seed's 25m, is also why
				// nobody caught the memory column: one axis read correct, so
				// the other was assumed correct by association.
				"--pod-recommendation-min-memory-mb=32",
				"--pod-recommendation-min-cpu-millicores=25",
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
			// See `admissionController.podDisruptionBudget` for why.
			podDisruptionBudget: enabled: false
			extraArgs: [
				"--in-place-skip-disruption-budget=true",
				"--feature-gates=InPlace=true",
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
