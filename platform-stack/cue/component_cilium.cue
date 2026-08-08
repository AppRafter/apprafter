// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// Cilium CNI + kube-proxy replacement. Pinned to the version
// the v0.1.x `cluster-bootstrap` installs today (1.16.5) so
// platform-stack 0.1.0 is bit-for-bit compatible with the
// existing single-node tier-1 install.
//
// Values are the same baseline as
// `cli-providers::k8s::cilium_values_yaml` produces — namely:
// kube-proxy replacement on, IPAM kubernetes, hubble off by
// default (tier overlays may turn it on). The `cue/tier_*.cue`
// overlays may merge additional keys (e.g.
// `hubble.enabled: true` on tier 2+).
_components: cilium: #Component & {
	name:      "cilium"
	enabled:   bool | *true
	namespace: "kube-system"
	source: {
		repoURL: "https://helm.cilium.io/"
		chart:   "cilium"
	}
	// B.1.71b: single source via _loaderValues.cilium.chartVersion.
	// The literal lives in loader_values.cue; this field just
	// references it to preserve export field order.
	version: _loaderValues.cilium.chartVersion
	// `values:` is `_loaderValues.cilium.values` (the loader
	// subset — kube-proxy replacement, IPAM, hubble) unified
	// with the Argo-managed extras below. The loader subset
	// lives in `loader_values.cue` so the CLI's `build.rs` can
	// lift it out as a `const &str` for `cluster-bootstrap`
	// (extracted via `cue export -e _loaderValues.cilium.values`;
	// critical fields guarded by
	// `cilium_values_yaml_contains_loader_critical_fields` in
	// `cli-providers/src/k8s/loader_values.rs`).
	//
	// 1.83a — Gateway API HOST-NETWORK mode. The platform
	// Gateway is exposed by binding the Cilium Envoy proxy
	// listeners directly on the node's host network namespace
	// (host-netns ports 80/443) rather than fronting them with
	// a LoadBalancer Service + LB-IPAM/L2 announcements. A
	// LoadBalancer Service needs either a cloud LB controller
	// or bare-metal L2/BGP IPAM; on a single-node Hetzner Cloud
	// VDS the node already owns a routable public IP, so
	// host-network is the correct (and only working) mechanism
	// — `l2announcements`/`externalIPs` are bare-metal-only and
	// do NOT work on Hetzner Cloud, so they're removed here.
	//
	// Cilium 1.16.5 defaults Envoy to a STANDALONE DaemonSet
	// (`envoy.enabled: ~` → "true for new installation"), so
	// the NET_BIND_SERVICE grant for the privileged 80/443
	// listeners goes on the standalone path:
	// `envoy.securityContext.capabilities.{envoy,keepCapNetBindService}`.
	// The Helm capability list REPLACES the chart default, so
	// the full 1.16.5 default `envoy` capability list
	// (`NET_ADMIN`, `SYS_ADMIN`) is reproduced verbatim with
	// `NET_BIND_SERVICE` appended; `keepCapNetBindService` (chart
	// default `false`) is flipped on so the forked Envoy process
	// keeps it. `hostNetwork.nodes` is left unset → all nodes
	// (the single node on T1).
	//
	// These gateway/security-context extras are COMPONENT-only
	// (Argo-managed): they never enter the loader export, so
	// `cluster-bootstrap` stays minimal and the loader's 8 keys
	// remain byte-identical to the v0.1.x install (no CLI
	// change). NOTE: host-network Cilium cannot run in CI/kind
	// (no real host-netns port binding), so the exact capability
	// config is validated on a real Hetzner node (T8 e2e).
	values: _loaderValues.cilium.values & {
		// 2.16d resource requests/limits (measured RSS×0.8 request /
		// tight mem limit / modest cpu request / no cpu limit — see
		// docs/measurements/2.16d-baseline-*.md). Top-level `resources`
		// is the cilium-AGENT container (chart 1.16.5); the operator and
		// standalone-envoy DaemonSet carry their own keys below. No
		// component pod stays BestEffort.
		resources: {
			requests: {
				cpu:    "50m"
				memory: "106Mi"
			}
			limits: memory: "256Mi"
		}
		gatewayAPI: {
			enabled: bool | *true
			hostNetwork: enabled: bool | *true
		}
		// T8 walk-fix (Run 2, 2026-06-13 — F2 reframed + widened): cilium
		// reads its config FLAGS only at pod START, so enabling
		// gateway-api/host-network on a LIVE cluster requires ALL THREE
		// cilium pods to restart — the AGENT (`ds/cilium`, serves the
		// CiliumEnvoyConfig to Envoy over xDS), the OPERATOR (translates the
		// Gateway → CEC), and the standalone ENVOY (`ds/cilium-envoy`, binds
		// host-netns 80/443). The Run-2 walk hit exactly this: Gateway reached
		// `Programmed=True` but Envoy never bound 80/443 because the agent ran
		// on the stale (gateway-api-off) config and never pushed the CEC
		// listener — only a manual `rollout restart` of operator + agent +
		// envoy unblocked it. The standard cilium Helm values auto-roll each
		// pod when the `cilium-config` ConfigMap changes (a config checksum is
		// stamped on the pod template). This SUPERSEDES the earlier
		// `operator.podAnnotations.cilium-config-rev` rev-bump (which rolled
		// ONLY the operator). Fresh installs are unaffected (pods start with
		// the final config) — purely upgrade-correctness. Docs: cilium "many
		// configuration changes require an agent restart"; the Gateway API
		// enable guide literally rolls cilium-operator + ds/cilium.
		rollOutCiliumPods: true
		operator: {
			rollOutPods: true
			// 2.16d: cilium-operator resources (measured 58Mi → req 48Mi / limit 128Mi).
			resources: {
				requests: memory: "48Mi"
				limits: memory:   "128Mi"
			}
		}
		envoy: {
			enabled:     true
			rollOutPods: true
			// 2.16d: standalone cilium-envoy DaemonSet resources
			// (measured 18Mi → req 16Mi / limit 64Mi).
			resources: {
				requests: memory: "16Mi"
				limits: memory:   "64Mi"
			}
			securityContext: capabilities: {
				keepCapNetBindService: true
				// Cilium 1.16.5 chart default `envoy.securityContext.capabilities.envoy`
				// (NET_ADMIN, SYS_ADMIN) + NET_BIND_SERVICE for ports 80/443.
				envoy: ["NET_ADMIN", "SYS_ADMIN", "NET_BIND_SERVICE"]
			}
		}
	}

	// CNI is the prerequisite for every other component to
	// schedule pods. Sync first.
	syncWave: -20

	// Argo CD 2.13.1 (shipped by argo-cd chart 7.7.7) doesn't
	// know about `Deployment.status.terminatingReplicas` /
	// `DaemonSet.status.terminatingReplicas` — Kubernetes
	// 1.31+ fields surfaced by k3s v1.35. Without an explicit
	// ignore, structured-merge diff fails with
	// `field not declared in schema` and the Application
	// reports `ComparisonError`. The chart's adopt of Argo CD
	// itself fixes the schema as a side-effect of a future
	// upgrade, but until then we mute the field per-component.
	ignoreDifferences: [
		{
			group: "apps"
			kind:  "Deployment"
			jsonPointers: ["/status/terminatingReplicas"]
		},
		{
			group: "apps"
			kind:  "DaemonSet"
			jsonPointers: ["/status/terminatingReplicas"]
		},
	]
}
