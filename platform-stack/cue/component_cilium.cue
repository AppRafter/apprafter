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
	version: "1.16.5"
	// Values MUST mirror `cli-providers::k8s::cilium_values_yaml`
	// byte-by-byte for tier-1. The CLI's cluster-bootstrap
	// loader installs Cilium with those values; when the chart
	// adopts the loader release on first reconcile, helm
	// upgrade applies whatever this overlay produces. ANY drift
	// — `k8sServiceHost: "auto"` instead of `"127.0.0.1"`,
	// missing `ipv4`/`ipv6` enabled flags, `kubeProxyReplacement`
	// as string instead of bool — reconfigures the live Cilium
	// agent + operator and on cpx22 k3s reliably crashes
	// cilium-operator with `CrashLoopBackOff` (every new pod
	// then fails with `cilium-cni: unable to connect to Cilium
	// agent`). Walk-found bug v0.1.102 → v0.1.103.
	//
	// Once B.1.71 lands and these values are owned by ONE side
	// (chart values yaml, regenerated into the CLI loader via
	// `cue cmd`), the drift becomes impossible. Until then, any
	// edit here MUST be paired with the same edit in
	// `cli-providers::k8s::cilium_values_yaml`.
	values: {
		kubeProxyReplacement: bool | *true
		k8sServiceHost:       string | *"127.0.0.1"
		k8sServicePort:       int | *6443
		ipv4: enabled:      bool | *true
		ipv6: enabled:      bool | *true
		ipam: mode:         string | *"kubernetes"
		hubble: enabled:    bool | *false
		operator: replicas: int | *1
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
