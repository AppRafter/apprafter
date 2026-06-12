// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// Upstream Gateway API CRDs (EXPERIMENTAL channel). These MUST exist
// before Cilium reconciles `gatewayAPI.enabled: true` — the Cilium
// agent fails to register its Gateway API controller when the
// `gateway.networking.k8s.io` CRDs are absent. So this component
// syncs at wave -25, strictly BEFORE cilium (-20).
//
// We ship the EXPERIMENTAL channel (a strict SUPERSET of standard:
// it adds TLSRoute / TCPRoute / UDPRoute + BackendTLSPolicy) rather
// than the standard channel, because cilium 1.16.5's gateway
// controller runs a required-resources check at startup that includes
// `tlsroutes.gateway.networking.k8s.io` (v1alpha2) — a CRD that lives
// ONLY in the experimental channel. With the standard channel that
// check fails (`error="...tlsroutes...not found"`), the controller
// never Accepts the `cilium` GatewayClass, and no Gateway ever reaches
// Programmed (live-walk finding 2026-06-12).
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
// Source is a Git path (not Helm): `config/crd/experimental` under
// the upstream `gateway-api` repo at tag v1.2.1. That directory holds
// the experimental-channel CRD yamls — the full standard set
// (GatewayClass / Gateway / HTTPRoute / ReferenceGrant / GRPCRoute)
// PLUS TLSRoute / TCPRoute / UDPRoute + BackendTLSPolicy, the superset
// cilium 1.16.5 requires (see header). Unlike `standard`, the
// `experimental` directory HAS a `kustomization.yaml`, so Argo CD
// treats it as a kustomize source rather than a plain directory source
// and renders the kustomization — this is fine; the result is the same
// CRD yamls applied to the cluster. `version` (→ Argo targetRevision)
// is the upstream git tag and tracks `GATEWAY_API_VERSION` in
// `cli-providers::k8s::kubectl`.
_components: "gateway-api-crds": #Component & {
	name:      "gateway-api-crds"
	namespace: "default" // CRDs are cluster-scoped; namespace is nominal (Argo requires one)
	source: {
		repoURL: "https://github.com/kubernetes-sigs/gateway-api"
		path:    "config/crd/experimental" // experimental channel: standard set + TLSRoute/TCPRoute/UDPRoute/BackendTLSPolicy (cilium 1.16.5 requires TLSRoute)
	}
	version:  "v1.2.1" // upstream git tag → Argo targetRevision; matches gateway_api_crds_url() in cli-providers
	syncWave: -25      // BEFORE cilium (-20)
	values: {} // non-helm (git directory source) — empty values so the template's lookup doesn't choke
}
