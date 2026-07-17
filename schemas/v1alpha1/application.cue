// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package v1alpha1

// Application is the dev-facing unit of deployment.
//
// v1alpha1 field set (see plan.md §1.7):
//   - image    — required-ish; the renderer fails if neither base
//                nor any environment override sets it.
//   - replicas — non-negative; defaults to 1 at render time.
//   - expose   — optional Gateway-side exposure (port + visibility).
//   - env      — string→#EnvValue map. Values may be literal strings,
//                claim references ({claim: "<type>.<field>"}), or
//                external secret references ({secret: "<name>/<key>"}).
//                See ADR 0046 and the #EnvValue / #EnvRef definitions.
//   - environments — per-environment overrides via CUE unification;
//                see spec.md §3.1 and ADR 0004.
//
// Fields removed from the v1alpha1 surface: `autoscale`,
// `confidential`. They re-appear in their owning subphases (4.x
// for confidential workloads; autoscale with KEDA in 2.6a).
#Application: {
	#TypeMeta
	kind:     "Application"
	metadata: #ObjectMeta

	spec: {
		base?: #ApplicationSpec

		environments?: [string]: #ApplicationSpec

		// Deploy-time env selector (ADR 0044): injected by the CMP from the
		// Argo Application's APPRAFTER_APP_ENV plugin env; selects which
		// `environments.<env>` override unifies onto base. Absent => base
		// only. Validated `environment in environments` by the webhook.
		environment?: string
	}

	status?: {
		phase?:       string
		environment?: string

		// Full spec snapshot the operator stamps after a successful apply
		// (2.16b). The app-scope migration classifier diffs the incoming
		// spec against this baseline. Free-form JSON here — the whole
		// `status` node is emitted x-kubernetes-preserve-unknown-fields by
		// crdgen (status is operator-owned, constrains no user input), so
		// this nested #ApplicationSpec snapshot is opaque in the CRD and
		// needs no structural schema (no crdmeta patch).
		lastAppliedSpec?: {...}
	}
}

#ApplicationSpec: {
	// OCI image reference. Deliberately unconstrained in CUE: a
	// regex like =~"^.+$" on an optional field is a half-measure
	// that hints at stricter validation without actually buying
	// it. Non-empty + the cross-field rule (image reachable
	// through `base.image` OR every `environments[*].image`) are
	// enforced at runtime by the OpenAPI v3 CRD's
	// `pattern: "^.+$"` and the admission webhook. Digest/tag
	// shape lands with the renderer + webhook in 1.7c.
	image?: string

	// Image resolution policy (ADR 0040). Default behaviour (absent or
	// resolve: "digest") = the operator resolves base.image's tag to its
	// current registry digest each reconcile (push->deploy). "off" =
	// render the reference verbatim, no registry poll.
	imagePolicy?: #ImagePolicy

	// Replica count. Zero is valid (scale-to-zero); negative is
	// rejected by CUE.
	replicas?: int & >=0

	expose?: {
		port: int & >0 & <=65535
		// Visibility. `public` → the operator emits an HTTPRoute on the
		// platform Gateway (1.83b); `internal` (default) → ClusterIP only;
		// `vpn` → reserved (the admission webhook rejects it until
		// AccessGrant/ExternalSurface lands). The OpenAPI v3 CRD enforces
		// the enum at the kube-apiserver layer.
		network?: "public" | "internal" | "vpn" | *"internal"
		// One or several public hostnames (1.83b). A bare string OR a list
		// of strings (the 2.6b `needs` OneOrMany union). Consumed only when
		// network == "public". Required-when-public + DNS-1123-subdomain are
		// enforced by the admission webhook; the CRD collapses the
		// scalar|array union to x-kubernetes-preserve-unknown-fields.
		hostname?: string | [...string]
		// Terminate TLS (1.83b). Minimal bool form; the full #TlsOptions
		// lands with 4.1b (deferred, not cancelled). Default on for public;
		// `tls: false` + network: public is webhook-rejected for now (no
		// HTTP-only public route in this slice — the route attaches to :443).
		tls?: bool | *true
	}

	// Env value map (ADR 0046). Each value is a literal string OR a
	// structured reference (claim / external secret). See #EnvValue.
	env?: [string]: #EnvValue

	// Declared platform-service dependencies, keyed by service
	// type. Each entry becomes a `ResourceClaim` of that type —
	// the Application controller generates the claims (2.4d wires
	// pg; 2.5 jetstream; 2.6 redis; 2.6b disk). Settable on `base`
	// and overridable per `environments[*]` (spec §3.1: dev vs prod
	// may select different providers). `needs: {pg: {}}` is valid
	// — tier-aware platform defaults supply selector + size.
	//
	// Each service key accepts a SCALAR `#ServiceNeed` (the single,
	// unnamed default claim — backward-compatible) OR an ARRAY of
	// named `#ServiceNeed`s. `(type, name)` is the claim identity
	// (2.6b / ADR 0043): the unnamed default keeps today's claim
	// name and env var (`<app>-pg` / `DATABASE_URL`); a named entry
	// gets `<app>-pg-<name>` / `DATABASE_URL_<NAME>`. `disk` is its
	// own value type (`#DiskClaim`) — claim-backed persistent block
	// storage mounted into the workload.
	//
	// This is an explicit CLOSED struct (was a pattern map keyed by
	// `#PlatformServiceType`): unknown keys are rejected under full
	// CUE evaluation. Because the OpenAPI v3 CRD's structural schema
	// is open on map keys (`additionalProperties` accepts any key),
	// the admission webhook re-enforces the key enum + the
	// `(type, name)` uniqueness invariants at the apiserver — that
	// is the runtime gate.
	needs?: #Needs
}

// 2.12 (ADR 0046): an env value is a literal string OR a structured
// reference. The reference is a single-key discriminated marker — the
// renderer resolves it to a Deployment EnvVar valueFrom.secretKeyRef.
#EnvValue: string | #EnvRef

#EnvRef: {claim: string} | {secret: string}
// claim payload:  "<type>.<field>"  or  "<type>.<name>.<field>"
// secret payload: "<name>/<key>"

// #ClaimFieldsFor — the per-network-need field set (the keys each
// provisioner writes into the connection Secret). pg + redis only ship
// connection Secrets today. This is the SINGLE SOURCE OF TRUTH for the
// claim field vocabulary, shared by:
//   - the operator renderer's secretKeyRef key resolution,
//   - the admission webhook's enum validation,
//   - the cue-cmp / `apprafter app validate` generated `claim` binding.
//
// It is an EXPORTED DEFINITION (`#`), not a hidden field, on purpose: the
// generated `claim` sibling file lives in the USER'S manifest package and
// must reference this table ACROSS the `apprafter.io/schemas/v1alpha1`
// import boundary. Hidden `_`-fields are package-private (invisible to
// importers); definitions are exported and — like all definitions — never
// appear in concrete `cue export` output, so the CRD is unaffected.
#ClaimFieldsFor: {
	pg: ["url", "user", "pass", "host", "port", "db"]
	redis: ["url", "user", "pass", "host", "port", "db", "channelPrefix"]
}

// Hidden mirror for same-package use inside #MkClaim below.
_fieldsFor: #ClaimFieldsFor

// _mkFields — markers for one (need[,name]) claim. Each field becomes a
// {claim: "<type>[.<name>].<field>"} marker (string-discriminated).
_mkFields: {
	_need: string
	_name: string | *""
	_fields: [...string]
	_seg: [if _name != "" {"\(_need).\(_name)"}, "\(_need)"][0]
	for f in _fields {(f): {claim: "\(_seg).\(f)"}}
}

// #MkClaim — derive a `claim` binding from an effective `needs` value, for
// SAME-PACKAGE use (e.g. a reference manifest co-located with the schema).
//
// CROSS-PACKAGE LIMITATION (de-risked on real cue, 2026-06-09 / 2.12f):
// CUE does NOT re-evaluate a struct-comprehension that consumes a field
// supplied via cross-package unification. `v1alpha1.#MkClaim & {_needs: …}`
// referenced from the user's manifest package yields `{}` — the
// `for type, n in _needs { … }` loop already ran (over an empty `_needs`)
// inside this package and unifying a value afterward across the import
// boundary does not re-trigger it. (Same-package the loop DOES re-run, so
// #MkClaim is usable from within v1alpha1.) This holds for plain fields,
// hidden fields, AND definitions — it is comprehension re-evaluation, not
// visibility, that the boundary blocks.
//
// Therefore the cue-cmp (at render) and `apprafter app validate` (locally)
// do NOT emit `claim: v1alpha1.#MkClaim & {_needs: …}`. They generate a
// sibling file that runs the comprehension IN the manifest's package,
// referencing only the exported `#ClaimFieldsFor` table cross-package:
//
//	claim: {
//	    for type, n in <manifest needs> if (v1alpha1.#ClaimFieldsFor[type] != _|_) {
//	        (type): { …same body as below, with #ClaimFieldsFor[type]… }
//	    }
//	}
//
// `#MkClaim` keeps that body as the canonical reference shape; the
// generators inline a structurally-identical comprehension. `#ClaimFieldsFor`
// is the single cross-package source of truth both depend on.
//
// Scalar `needs: {pg: {}}` → `claim.pg.url` (unnamed default).
// Named-only array `[{name:"main"},{name:"ro"}]` → `claim.pg.main.url` /
// `claim.pg.ro.url`; `claim.pg.url` is undefined (no unnamed default).
// Mixed array (one unnamed + named entries) → BOTH `claim.pg.url` (unnamed
// default) AND `claim.pg.<name>.url` (named entries).
//
// Array branch uses `let _nm = [if e.name != _|_ {e.name}, ""][0]` to
// coalesce each entry's name to "" when absent — avoids bottoming on
// `(e.name)` as a dynamic key for the unnamed-entry case.
#MkClaim: {
	_needs: {...}
	for type, n in _needs if (_fieldsFor[type] != _|_) {
		(type): {
			// scalar branch: n is a struct (one unnamed default claim)
			if (n & {...}) != _|_ {_mkFields & {_need: type, _fields: _fieldsFor[type]}}

			// array branch: n is a list; iterate entries, emitting named
			// sub-structs or default (type-level) fields as appropriate.
			if (n & [...]) != _|_ {
				for e in n {
					let _nm = [if e.name != _|_ {e.name}, ""][0]
					if _nm != "" {
						(_nm): {
							for f in _fieldsFor[type] {
								(f): {claim: "\(type).\(_nm).\(f)"}
							}
						}
					}
					if _nm == "" {
						for f in _fieldsFor[type] {
							(f): {claim: "\(type).\(f)"}
						}
					}
				}
			}
		}
	}
}

// #Needs — the closed set of declared dependency keys (2.6b /
// ADR 0043). A definition (closed by default) so an unknown key is
// rejected under full CUE evaluation. Each service key accepts a
// scalar `#ServiceNeed` (the unnamed default) or an array of named
// `#ServiceNeed`s; `disk` carries `#DiskClaim`.
#Needs: {
	pg?: #ServiceNeed | [...#ServiceNeed]
	jetstream?: #ServiceNeed | [...#ServiceNeed]
	clickhouse?: #ServiceNeed | [...#ServiceNeed]
	redis?: #ServiceNeed | [...#ServiceNeed]
	s3?: #ServiceNeed | [...#ServiceNeed]
	notifications?: #ServiceNeed | [...#ServiceNeed]
	disk?: #DiskClaim | [...#DiskClaim]
}

// #ServiceNeed — one declared platform-service dependency under
// `Application.spec.*.needs`. The 2.4d controller turns each entry
// into a `ResourceClaim` of the keyed type; the 2.3 scheduler
// routes it to a `ServiceProvider` via `selector`.
#ServiceNeed: {
	// `(type, name)` claim identity (2.6b / ADR 0043). Omit for the
	// single, unnamed default claim of this type (backward-compatible
	// — keeps `<app>-<type>` / `DATABASE_URL`). A named entry yields
	// `<app>-<type>-<name>` / `DATABASE_URL_<NAME>`. The webhook
	// validates `name` is env-foldable (a DNS-1123 label) and unique
	// within a type (at most one unnamed default).
	name?: string

	// Label selector matched against `ServiceProvider.metadata.labels`.
	// Optional in the manifest — the controller injects a default
	// `{tier: integrated}` when absent (2.4d). The generated
	// `ResourceClaim` requires a non-empty selector (CRD
	// `minProperties: 1`); the injected default guarantees it.
	selector?: [string]: string

	// Requested size class. Optional — tier-aware platform defaults
	// fill it when absent (spec §3.1: `needs.pg: {}` → tier sizing).
	size?: #Size

	// Persist the provisioned resource across Application deletion
	// (default false). redis: routes to a persistent pool instance
	// (snapshot→PVC) instead of an ephemeral one (ADR 0042).
	persistent?: bool
}

// #DiskClaim — one declared persistent-disk dependency under
// `Application.spec.*.needs.disk` (2.6b/2.6c / ADR 0043). Two
// discriminated shapes (the webhook enforces the disjunction; no
// half-measure CUE stubs for cross-field invariants).
#DiskClaim: {
	// Owned shape: the provisioner creates a standalone RWO PVC
	// (unowned, GC-managed for retention) and the renderer mounts it
	// into the workload Deployment (`replicas: 1`, `strategy:
	// Recreate`). Deferred fields (`shareMode`, `backup`, `autoExpand`,
	// replicated/shared classes) are intentionally absent until
	// implemented.

	// `(type, name)` claim identity. Omit → derived from the last
	// segment of `mountPath` (`/var/lib/uploads` → `uploads`);
	// explicit wins. Must be a DNS-1123 label (it becomes part of the
	// PVC name). The webhook enforces uniqueness within `disk`.
	name?: string

	// Requested capacity as a Kubernetes quantity (`"10Gi"`).
	// Required on the owned shape. The webhook validates it parses as
	// a quantity.
	size: string

	// Absolute in-container mount point (`"/data"`). Required and
	// unique within the app (webhook-enforced).
	mountPath: string

	// Storage class abstraction. `local` only at launch (maps to the
	// matched `disk-local` provider's `config.storageClass`); the
	// replicated/shared classes are deferred to T2+.
	class?: "local" | *"local"

	// Mount the volume read-only (default false).
	readOnly?: bool | *false
} | {
	// Referenced shape (2.6c): binds an existing SharedVolume by name.
	// The actual reference-claim generation is T9/T10.

	// Name of the SharedVolume to bind (`ref` is the wire key).
	// Required on this shape.
	ref: string

	// Absolute in-container mount point (`"/data"`). Required and
	// unique within the app (webhook-enforced).
	mountPath: string

	// Mount the volume read-only (default false).
	readOnly?: bool | *false
}

// #ImagePolicy — image-reference resolution policy under
// `Application.spec.base.imagePolicy` (ADR 0040).
#ImagePolicy: {
	// "digest" (default when absent) = the operator resolves
	// `base.image`'s tag to its current registry digest each
	// reconcile (push->deploy). "off" = render the reference
	// verbatim, no registry poll.
	resolve?: "digest" | "off"
}
