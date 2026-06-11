// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// Upstream Gateway API CRDs (standard channel). These MUST exist
// before Cilium reconciles `gatewayAPI.enabled: true` — the Cilium
// agent fails to register its Gateway API controller when the
// `gateway.networking.k8s.io` CRDs are absent. So this component
// syncs at wave -25, strictly BEFORE cilium (-20).
//
// How the CRDs arrive in each case:
//
//   - New clusters: the CLI loader (`cluster-bootstrap`) installs a
//     minimal Cilium first (its loader values export carries NO
//     gatewayAPI/L2 keys — see `component_cilium.cue`), then hands
//     off to Argo CD. Argo applies this component at -25, the
//     upstream CRDs land, and the wave -20 cilium Application then
//     upgrades Cilium to the full gateway-enabled values.
//   - Existing clusters: this is how the CRDs arrive — the platform
//     auto-update rolls out the new platform-stack version and Argo
//     reconciles this Application into the cluster.
//
// Source is a Git directory (not Helm): `config/crd/standard` under
// the upstream `gateway-api` repo at tag v1.2.1. That directory holds
// the 5 standard-channel CRD yamls (GatewayClass / Gateway / HTTPRoute
// / ReferenceGrant / GRPCRoute) with NO kustomization.yaml, so Argo CD
// treats it as a plain directory source and applies the yamls directly
// — exactly the same CRD set the CLI's `gateway_api_crds_url()`
// (`standard-install.yaml`) ships. `version` (→ Argo targetRevision)
// is the upstream git tag and tracks `GATEWAY_API_VERSION` in
// `cli-providers::k8s::kubectl`.
_components: "gateway-api-crds": #Component & {
	name:      "gateway-api-crds"
	namespace: "default" // CRDs are cluster-scoped; namespace is nominal (Argo requires one)
	source: {
		repoURL: "https://github.com/kubernetes-sigs/gateway-api"
		path:    "config/crd/standard" // standard channel: GatewayClass/Gateway/HTTPRoute/ReferenceGrant/GRPCRoute
	}
	version:  "v1.2.1" // upstream git tag → Argo targetRevision; matches gateway_api_crds_url() in cli-providers
	syncWave: -25      // BEFORE cilium (-20)
	values: {} // non-helm (git directory source) — empty values so the template's lookup doesn't choke
}
