// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package crdmeta

// CRD-generation metadata for `crdgen` (ADR 0047), read via
// `cue export ./schemas/crdmeta -e _crdMetas`.
//
// Kept OUT of `schemas/v1alpha1` on purpose: that package is bundled into
// the argocd-cue-cmp image (`schemas/v1alpha1/*.cue`), so crdgen-only
// metadata there would force a cue-cmp republish on every CRD migration
// and trip the cue-cmp drift guard. Each CRD contributes its kind's entry
// to this shared map.

// Shared Argo CD annotations for every generated CRD (sync-wave -5 +
// server-side apply). Each kind's `annotations` references this.
_syncWave: {
	"argocd.argoproj.io/sync-wave":    "-5"
	"argocd.argoproj.io/sync-options": "ServerSideApply=true"
}

_crdMetas: Application: {
	group:   "apprafter.io"
	version: "v1alpha1"
	scope:   "Namespaced"
	names: {
		plural:   "applications"
		singular: "application"
		kind:     "Application"
		listKind: "ApplicationList"
		shortNames: ["app", "apps"]
	}
	annotations: _syncWave
	subresources: status: {}
	printerColumns: [{name: "Age", type: "date", jsonPath: ".metadata.creationTimestamp"}]

	// CRD-only constraints kept out of the CUE type (image stays `string`
	// in cue vet per the validation policy; the non-empty rule lives in
	// the CRD + webhook). Paths are relative to `spec`; `[*]` descends
	// into a map's additionalProperties.
	schemaPatches: {
		"base.image": {pattern: "^.+$"}
		"environments[*].image": {pattern: "^.+$"}
	}
}

_crdMetas: ServiceProvider: {
	group:   "apprafter.io"
	version: "v1alpha1"
	scope:   "Namespaced"
	names: {
		plural:   "serviceproviders"
		singular: "serviceprovider"
		kind:     "ServiceProvider"
		listKind: "ServiceProviderList"
		shortNames: ["sp"]
	}
	annotations: _syncWave
	subresources: status: {}
	printerColumns: [
		{name: "Type", type: "string", jsonPath: ".spec.type"},
		{name: "Backend", type: "string", jsonPath: ".spec.backend"},
		{name: "Health", type: "string", jsonPath: ".status.health"},
		{name: "Age", type: "date", jsonPath: ".metadata.creationTimestamp"},
	]

	// CRD-only SPEC constraints CUE deliberately omits (backend stays a
	// bare `string` in cue vet per the validation policy; the non-empty
	// rule lives in the CRD + webhook). Paths are relative to `spec`.
	//
	// `config` is the CUE top type (`config?: _`), which `cue export
	// --out openapi` emits as a bare `{}` (an empty, non-structural
	// schema node). Restore the hand-rolled CRD's
	// `{type: object, x-kubernetes-preserve-unknown-fields: true}` so the
	// node is structurally valid AND keeps the same "any object, opaque
	// to the platform" acceptance set.
	schemaPatches: {
		"backend": {minLength: 1}
		"config": {type: "object", "x-kubernetes-preserve-unknown-fields": true}
	}
}

_crdMetas: ResourceClaim: {
	group:   "apprafter.io"
	version: "v1alpha1"
	scope:   "Namespaced"
	names: {
		plural:   "resourceclaims"
		singular: "resourceclaim"
		kind:     "ResourceClaim"
		listKind: "ResourceClaimList"
		shortNames: ["rc"]
	}
	annotations: _syncWave
	subresources: status: {}
	printerColumns: [
		{name: "Type", type: "string", jsonPath: ".spec.type"},
		{name: "Size", type: "string", jsonPath: ".spec.size"},
		{name: "Provider", type: "string", jsonPath: ".status.provider"},
		{name: "Ready", type: "string", jsonPath: ".status.ready"},
		{name: "Age", type: "date", jsonPath: ".metadata.creationTimestamp"},
	]

	// CRD-only SPEC constraint CUE omits: the hand-rolled CRD enforced
	// `selector.minProperties: 1` (a non-empty routing map), but
	// `selector: [string]: string` in CUE exports no minProperties (an
	// empty map satisfies the open struct). Restore it so the generated
	// CRD keeps the same acceptance set; the webhook also enforces it
	// (clearer message). Paths are relative to `spec`.
	schemaPatches: {
		"selector": {minProperties: 1}
	}
}
