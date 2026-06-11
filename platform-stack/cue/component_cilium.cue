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
	// `cli-providers/src/k8s/loader_values.rs`). The gateway/L2
	// extras (gatewayAPI, l2announcements, externalIPs) only
	// matter once the upstream Gateway API CRDs are installed —
	// which the bootstrap does AFTER Cilium — so they're chart-
	// only and never enter the loader export (it stays byte-
	// identical to the v0.1.x bootstrap install, no CLI change).
	values: _loaderValues.cilium.values & {
		gatewayAPI: enabled:      bool | *true
		l2announcements: enabled: bool | *true
		externalIPs: enabled:     bool | *true
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
