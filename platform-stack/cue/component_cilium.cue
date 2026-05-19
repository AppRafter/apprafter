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
	values: {
		kubeProxyReplacement: "true"
		k8sServiceHost:       "auto"
		ipam: mode: "kubernetes"
		hubble: {
			enabled: bool | *false
			relay: enabled: bool | *false
			ui: enabled:    bool | *false
		}
		operator: replicas: int | *1
	}
}
