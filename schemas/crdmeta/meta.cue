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

		// `needs.jetstream.streams[].subjects`: the hand-rolled non-empty
		// rule the CUE type deliberately omits — see the comment on
		// `#JetStreamStream.subjects` in `schemas/v1alpha1/application.cue`
		// for why `& [_, ...]` can't live in the CUE type itself (it breaks
		// `cue export --out openapi`). Same precedent as SourceCredential's
		// `git.repoPrefixes` below: restore `minItems` here so the
		// generated CRD enforces "at least one subject" even though CUE
		// exports a bare `{type: array, items: {type: string}}`. Two
		// entries because `needs` is duplicated under both `base` and
		// `environments[*]` in the rendered schema (same reason `image`'s
		// pattern needs two entries above). `streams[]` descends into the
		// array's `items`, matching MigrationPlan's `changes[].from`
		// precedent for a multi-level path.
		"base.needs.jetstream.streams[].subjects": {minItems: 1}
		"environments[*].needs.jetstream.streams[].subjects": {minItems: 1}

		// `streams[].name` and `consume[].durable`: DNS-1123-label format
		// rule, restored the same way as `subjects`' `minItems` above and
		// for the same reason CUE can't carry it (this repo's validation
		// philosophy routes format rules to the CRD + webhook, never a
		// half-measure CUE regex — see #JetStreamStream.name's comment).
		// The rule exists because the provisioner composes each of these
		// into a NATS-side identifier joined on `_`
		// (`nats_stream_name`/`nats_durable_name` in
		// `resourceclaim-provisioner::nats_accounts`) — a `_` in the
		// declared name would make that join ambiguous and let two
		// different applications compose the identical NATS name, which
		// decides who may read/purge a stream or share a consumer's
		// cursor (ADR 0061 §4.2's soundness proof for the deny vector's
		// position patterns depends on this). `validate_jetstream_need`
		// enforces the same rule again — see the `subjects` comment above
		// for why that redundancy is kept (an older CRD without this
		// patch). Regex is the standard Kubernetes DNS-1123 label
		// (`[a-z0-9]` endpoints, `[-a-z0-9]*` between); this repo's own
		// `is_dns_1123_label` enforces the same alphabet and anchoring —
		// NOT byte-for-byte the same rule, though (round-7 review): the
		// Rust helper ALSO caps the input at 63 characters, which this
		// regex does not, so the two agree on every string this regex
		// accepts but the Rust helper is strictly narrower on length.
		"base.needs.jetstream.streams[].name": {pattern: "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$"}
		"environments[*].needs.jetstream.streams[].name": {pattern: "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$"}
		"base.needs.jetstream.consume[].durable": {pattern: "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$"}
		"environments[*].needs.jetstream.consume[].durable": {pattern: "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$"}
	}

	// `status.lastAppliedSpec` is the 2.16b migration baseline — a raw
	// `ApplicationSpec` snapshot the operator stamps and later diffs against.
	// CUE's `lastAppliedSpec?: {...}` open struct exports as a closed
	// `{type: object}`; nested under the status root's
	// x-kubernetes-preserve-unknown-fields that bare `type: object` is a
	// STRUCTURAL PRUNING BOUNDARY — the apiserver strips every key inside it,
	// so the baseline round-trips as `{}` and gating never fires (walk-found).
	// Restore `{type: object, x-kubernetes-preserve-unknown-fields: true}` ON
	// the node itself so its children survive (same shape as MigrationPlan's
	// `previousSpecSnapshot`). Paths are relative to the `status` node.
	statusSchemaPatches: {
		"lastAppliedSpec": {type: "object", "x-kubernetes-preserve-unknown-fields": true}
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

_crdMetas: RetainedClaim: {
	group:   "apprafter.io"
	version: "v1alpha1"
	scope:   "Namespaced"
	names: {
		plural:   "retainedclaims"
		singular: "retainedclaim"
		kind:     "RetainedClaim"
		listKind: "RetainedClaimList"
		shortNames: ["rclaim"]
	}
	annotations: _syncWave
	// NO `subresources` — RetainedClaim has no status subresource; the
	// GC reads `spec` only. Omitting the field keeps `subresources` out
	// of the generated CRD (matches the hand-rolled mirror).
	printerColumns: [
		{name: "SourceClaim", type: "string", jsonPath: ".spec.claimRef.name"},
		{name: "Backend", type: "string", jsonPath: ".spec.backend"},
		{name: "RetainUntil", type: "date", jsonPath: ".spec.retainUntil"},
		{name: "Age", type: "date", jsonPath: ".metadata.creationTimestamp"},
	]

	// CRD-only SPEC constraints CUE deliberately omits (CUE-validation
	// philosophy — no half-measure stubs). Paths are relative to `spec`;
	// "" patches the spec root node itself.
	schemaPatches: {
		// Immutability: the hand-rolled CRD pinned the whole `spec` to
		// `self == oldSelf` via an `x-kubernetes-validations` CEL rule (the
		// snapshot is a fixed record of a deleted claim). CUE cannot express
		// a same-as-old cross-version rule, so restore it here. The webhook
		// layers the same guard (clearer message + the operator-only CREATE
		// gate); this is the CRD half.
		"": {"x-kubernetes-validations": [{
			rule:    "self == oldSelf"
			message: "RetainedClaim spec is immutable"
		}]}
	}
}

_crdMetas: MigrationPlan: {
	group:   "apprafter.io"
	version: "v1alpha1"
	scope:   "Namespaced"
	names: {
		plural:   "migrationplans"
		singular: "migrationplan"
		kind:     "MigrationPlan"
		listKind: "MigrationPlanList"
		shortNames: ["mp", "migplan"]
	}
	annotations: _syncWave
	subresources: status: {}
	printerColumns: [
		{name: "Scope", type: "string", jsonPath: ".spec.scope.type"},
		{name: "Classification", type: "string", jsonPath: ".spec.risks.classification"},
		{name: "Phase", type: "string", jsonPath: ".status.phase"},
		{name: "Age", type: "date", jsonPath: ".metadata.creationTimestamp"},
	]

	// CRD-only SPEC constraints CUE deliberately omits (CUE-validation
	// philosophy — no half-measure stubs). Paths are relative to `spec`;
	// `name[*]` descends into a map's additionalProperties, `name[]` into an
	// array's items (2.16b S1.2). A whole-array patch can still merge an
	// `items:` object onto the array node directly.
	schemaPatches: {
		// `trigger.from` / `trigger.to` are the CUE top type (`from?: _`,
		// `to?: _`), which `cue export --out openapi` emits as bare,
		// non-structural nodes the apiserver would reject. Restore the
		// hand-rolled CRD's `{x-kubernetes-preserve-unknown-fields: true}`
		// (same "free-form JSON" acceptance set). `type` is intentionally
		// omitted to match the hand-rolled mirror exactly.
		"trigger.from": {"x-kubernetes-preserve-unknown-fields": true}
		"trigger.to": {"x-kubernetes-preserve-unknown-fields": true}

		// 2.16b S1.2: `changes[].from` / `changes[].to` are the same CUE
		// top type (`from?: _`, `to?: _`) as the trigger's — restore the
		// same `{x-kubernetes-preserve-unknown-fields: true}` so the array
		// items' free-form JSON payloads survive apiserver structural
		// validation. `[]` descends into the array `items` (`changes` is an
		// array of #MigrationChange, unlike the map-valued `[*]` fields).
		"changes[].from": {"x-kubernetes-preserve-unknown-fields": true}
		"changes[].to": {"x-kubernetes-preserve-unknown-fields": true}

		// `previousSpecSnapshot?: {...}` is an open struct; `cue export`
		// emits `{type: object}` with no additionalProperties, which the
		// apiserver treats as a closed empty object (rejects every key).
		// Restore the hand-rolled `{type: object,
		// x-kubernetes-preserve-unknown-fields: true}` ("free-form JSON").
		"previousSpecSnapshot": {type: "object", "x-kubernetes-preserve-unknown-fields": true}
	}
}

_crdMetas: SourceCredential: {
	group:   "apprafter.io"
	version: "v1alpha1"
	scope:   "Namespaced"
	names: {
		plural:   "sourcecredentials"
		singular: "sourcecredential"
		kind:     "SourceCredential"
		listKind: "SourceCredentialList"
		shortNames: ["srccred"]
	}
	annotations: _syncWave
	subresources: status: {}
	printerColumns: [
		{name: "Age", type: "date", jsonPath: ".metadata.creationTimestamp"},
	]

	// CRD-only SPEC constraints CUE deliberately omits (CUE-validation
	// philosophy — no half-measure stubs). Paths are relative to `spec`;
	// `[]` for array items is NOT a navigable segment, so a constraint
	// inside an array's items is restored by merging an `items:` object
	// onto the array node.
	//
	// `repoPrefixes` / `hosts`: the hand-rolled CRD enforced
	// `minItems: 1` (each half must list at least one coverage entry) and
	// `items.pattern: "^.+$"` (each entry non-empty). CUE exports
	// `[...string]` as `{type: array, items: {type: string}}` — no
	// minItems, no item pattern. Restore both so the generated CRD keeps
	// the same acceptance set; the webhook layers the same two rules
	// (clearer per-entry messages). The `backend` discriminator
	// (oneOf in CUE) collapses to x-kubernetes-preserve-unknown-fields,
	// which drops the hand-rolled inner patterns + `required: [name]` to
	// the webhook — same as MigrationPlan's scope discriminator; no patch.
	schemaPatches: {
		"git.repoPrefixes": {minItems: 1, items: {type: "string", pattern: "^.+$"}}
		"registry.hosts": {minItems: 1, items: {type: "string", pattern: "^.+$"}}
	}

	// `status.lastAppliedSpec` is the 2.16b-sc migration baseline — a raw
	// `SourceCredentialSpec` snapshot the operator stamps and later diffs
	// against. CUE's `lastAppliedSpec?: {...}` open struct exports as a closed
	// `{type: object}`; nested under the status root's
	// x-kubernetes-preserve-unknown-fields that bare `type: object` is a
	// STRUCTURAL PRUNING BOUNDARY — the apiserver strips every key inside it,
	// so the baseline round-trips as `{}` and gating never fires (the same
	// walk-found bug class as Application's baseline). Restore
	// `{type: object, x-kubernetes-preserve-unknown-fields: true}` ON the node
	// itself so its children survive. Paths are relative to the `status` node.
	statusSchemaPatches: {
		"lastAppliedSpec": {type: "object", "x-kubernetes-preserve-unknown-fields": true}
	}
}

_crdMetas: PlatformStack: {
	group:   "apprafter.io"
	version: "v1alpha1"
	scope:   "Namespaced"
	names: {
		plural:   "platformstacks"
		singular: "platformstack"
		kind:     "PlatformStack"
		listKind: "PlatformStackList"
		shortNames: ["ps", "pstack"]
	}
	annotations: _syncWave
	subresources: status: {}
	printerColumns: [
		{name: "Channel", type: "string", jsonPath: ".spec.channel"},
		{name: "Pin", type: "string", jsonPath: ".spec.pin"},
		{name: "Current", type: "string", jsonPath: ".status.currentVersion"},
		{name: "Available", type: "string", jsonPath: ".status.availableVersion"},
		{name: "Age", type: "date", jsonPath: ".metadata.creationTimestamp"},
	]

	// CRD-only SPEC constraints CUE deliberately omits (CUE-validation
	// philosophy — no half-measure stubs), restored so the generated CRD
	// keeps the hand-rolled mirror's acceptance set. Paths are relative to
	// `spec`; `[*]` descends into a map's additionalProperties.
	//
	// `source.upstream` / `source.repoURL` / `source.checkInterval`: the
	// hand-rolled CRD enforced `pattern: ^oci://.+$` (the two OCI URLs) and
	// `pattern: ^[0-9]+(h|m|s)$` (the Go-duration check interval). CUE keeps
	// these bare `string`s (it carries only a `*default`, no `=~`), so the
	// export drops the patterns — restore them. The webhook layers the
	// stronger `checkInterval >= 1h` numeric rule (OpenAPI v3 can't compare
	// durations); the `pin` semver pattern already survives from CUE's `=~`.
	//
	// `values` and `overrides[*].values` are open structs (the top-level
	// `tier` + optional `domain` plus tier-specific chart fields; the
	// per-component `values?: {...}` free-form merge map) — the hand-rolled
	// CRD carried `x-kubernetes-preserve-unknown-fields: true` on both so new
	// fields need no CRD bump. CUE exports the `{...}`-less / `{...}` open
	// structs as closed objects, so restore preserve-unknown to keep the same
	// "accept extra keys" set. (The `tier` enum + `required: [tier]` survive
	// the CUE export — no patch.)
	schemaPatches: {
		"source.upstream": {pattern: "^oci://.+$"}
		"source.repoURL": {pattern: "^oci://.+$"}
		"source.checkInterval": {pattern: "^[0-9]+(h|m|s)$"}
		"values": {"x-kubernetes-preserve-unknown-fields": true}
		"overrides[*].values": {"x-kubernetes-preserve-unknown-fields": true}
	}
}

_crdMetas: SharedVolume: {
	group:   "apprafter.io"
	version: "v1alpha1"
	scope:   "Namespaced"
	names: {
		plural:   "sharedvolumes"
		singular: "sharedvolume"
		kind:     "SharedVolume"
		listKind: "SharedVolumeList"
		shortNames: ["sv"]
	}
	annotations: _syncWave
	subresources: status: {}
	printerColumns: [
		{name: "Ready", type: "string", jsonPath: ".status.ready"},
		{name: "Size", type: "string", jsonPath: ".spec.size"},
		{name: "Refs", type: "integer", jsonPath: ".status.refCount"},
		{name: "Age", type: "date", jsonPath: ".metadata.creationTimestamp"},
	]

	// `status.conditions` as a server-side-merged list keyed by `type` (the
	// standard Kubernetes conditions pattern). CUE→OpenAPI cannot emit the
	// `x-kubernetes-list-*` markers, so restore them here. Without the
	// listMap the list is ATOMIC under SSA: each manager's apply replaces
	// the whole array. SharedVolume's conditions (Ready + CapacityWarning)
	// are written by a single manager today, but a keyed list is the
	// correct, future-proof shape (and keeps it consistent with
	// ResourceClaim, whose two managers actively conflict). Paths are
	// relative to the `status` node.
	statusSchemaPatches: {
		"conditions": {
			"x-kubernetes-list-type": "map"
			"x-kubernetes-list-map-keys": ["type"]
		}
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

	// CRD-only SPEC constraints CUE omits. Paths are relative to `spec`;
	// `[]` descends into an array's `items` (matching Application's own
	// `needs.jetstream.streams[]` precedent — see that CRD's comment for
	// why the CUE type can't carry these rules itself).
	schemaPatches: {
		// The hand-rolled CRD enforced `selector.minProperties: 1` (a
		// non-empty routing map), but `selector: [string]: string` in
		// CUE exports no minProperties (an empty map satisfies the open
		// struct). Restore it so the generated CRD keeps the same
		// acceptance set; the webhook also enforces it (clearer
		// message).
		"selector": {minProperties: 1}

		// 2.5d prerequisite (ADR 0061 §6 amendment): `#ResourceClaimJetStream`
		// reuses `#JetStreamStream` / `#JetStreamConsume` verbatim, so it
		// needs the SAME three restored rules Application's own
		// `needs.jetstream.streams[]` / `.consume[]` do, for the identical
		// reasons (see that entry's comment above) — just at THIS claim's
		// own single `jetstream.*` path, not duplicated under `base.`/
		// `environments[*].` the way Application's is, because a
		// ResourceClaim carries exactly one effective declaration, not a
		// base-plus-per-environment tree.
		"jetstream.streams[].subjects": {minItems: 1}
		"jetstream.streams[].name": {pattern: "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$"}
		"jetstream.consume[].durable": {pattern: "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$"}
	}

	// `status.conditions` as a server-side-merged list keyed by `type` (the
	// standard Kubernetes conditions pattern). REQUIRED here (walk-found):
	// the SCHEDULER (`[Scheduled]`) and the PROVISIONER (`[Ready]`) write
	// `status.conditions` under DIFFERENT field managers. Without
	// `x-kubernetes-list-type: map` the list is ATOMIC under SSA, so each
	// forced apply REPLACES the whole array and the two managers overwrite
	// each other (only `[Scheduled]` survived → the durable `Ready=False /
	// AwaitingSharedVolume` condition was pruned). The listMap makes both
	// conditions coexist (merged by their `type` key). CUE→OpenAPI cannot
	// emit these markers, so restore them here. Paths are relative to the
	// `status` node.
	statusSchemaPatches: {
		"conditions": {
			"x-kubernetes-list-type": "map"
			"x-kubernetes-list-map-keys": ["type"]
		}
	}
}
