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

		environments?: [string]: #ApplicationEnvOverride

		// `environments.<env>` override deep-merges onto base (2.16c,
		// subfield override-wins). Absent => base only. The env is a
		// deploy-time per-CR scalar (ADR 0044) and may name an env absent
		// from `environments` (base-only deploy) — NOT rejected.
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

		// VPA recommendation mirror (2.16e). STRUCTURAL (typed), NOT a bare
		// `{...}`: a bare open struct renders as `type:object` with no
		// properties and no x-kubernetes-preserve-unknown-fields, so the
		// apiserver PRUNES the nested fields on a status server-side apply
		// (unlike `lastAppliedSpec`, which carries an explicit preserve-unknown
		// crdmeta patch). Declaring the full shape keeps them.
		recommendedResources?: {
			containerName?: string
			recommendation?: {
				target?: [string]:         string
				uncappedTarget?: [string]: string
			}
			notApplied?: string
		}
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

	// Container resource requests/limits (2.16d). Keys are resource
	// names (cpu, memory, ephemeral-storage); values are Kubernetes
	// quantities. Quantity format + request<=limit are webhook-enforced.
	resources?: #Resources

	// Liveness / readiness / startup probes (2.28 / ADR 0065). An absent
	// block still yields a default TCP-connect readiness probe when
	// `expose.port` is set — see `#Probes`. Every timing default lives in
	// `operator-rendering/src/probes.rs`, not here; a CUE `*x` default
	// would not reach the CRD anyway (crdgen strips them, R4-M2).
	probes?: #Probes

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
	// Most service keys accept a SCALAR `#ServiceNeed` (the single,
	// unnamed default claim — backward-compatible) OR an ARRAY of
	// named `#ServiceNeed`s. `(type, name)` is the claim identity
	// (2.6b / ADR 0043): the unnamed default keeps today's claim
	// name and env var (`<app>-pg` / `DATABASE_URL`); a named entry
	// gets `<app>-pg-<name>` / `DATABASE_URL_<NAME>`. `disk` is its
	// own value type (`#DiskClaim`) — claim-backed persistent block
	// storage mounted into the workload. `jetstream` is ALSO its own
	// value type (`#JetStreamNeed`, 2.5 / ADR 0061) — scalar only, no
	// array, no `(type, name)` identity.
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

// #ApplicationEnvOverride — the per-environment PARTIAL override type
// (2.16c). Every field is optional AND `expose.port` is optional, so an
// env carries only the diff; `effective_spec` deep-merges it onto base
// and the invariant "effective expose has a port" is enforced by the
// webhook (base.expose.port is required, so a base-only effective spec is
// never portless — the webhook only guards the base-absent case).
// Deliberately a SEPARATE type from #ApplicationSpec (R2-M4): base keeps
// `expose.port` required so "forgot the port in base" is caught by
// `cue vet` / `apprafter app validate` locally, not server-side.
#ApplicationEnvOverride: {
	image?:       string
	imagePolicy?: #ImagePolicy
	resources?:   #Resources
	// 2.28: reused verbatim from the base type. Every field of `#Probes`
	// and `#Probe` is already optional, so it is its own partial-override
	// type — the same reason `#Resources` and `#ImagePolicy` are reused
	// here rather than mirrored.
	probes?:   #Probes
	replicas?: int & >=0
	expose?: {
		port?:    int & >0 & <=65535
		network?: "public" | "internal" | "vpn"
		hostname?: string | [...string]
		tls?: bool
	}
	env?: [string]: #EnvValue
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
// provisioner writes into the connection Secret). pg and redis ship
// connection Secrets today; jetstream's entry below defines its
// VOCABULARY only (round-7 review: no provisioner writes a jetstream
// connection Secret yet — `nats_accounts::render_accounts_file` in
// resourceclaim-provisioner renders the SERVER-side accounts config, a
// different artifact, and has no caller wiring it into the reconciler
// yet either; both are part 2's work); clickhouse, s3 and
// notifications don't have an entry here yet. This is the SINGLE
// SOURCE OF TRUTH for the claim field vocabulary, shared by:
//   - the operator renderer's secretKeyRef key resolution,
//   - the cue-cmp / `apprafter app validate` generated `claim` binding.
//
// The admission webhook does NOT read this table (a Rust crate can't
// `cue export` at compile time, and evaluating CUE at runtime in the
// validation hot path was rejected as unnecessary weight for a fixed,
// rarely-changing list). It keeps its own hand copy
// (PG_FIELDS/REDIS_FIELDS/JETSTREAM_FIELDS in
// `operator/admission-webhook/src/validator.rs`), GATED against this
// table by `claim_fields_mirror_the_cue_source_of_truth` — a test that
// re-reads this file and fails the two copies apart the moment they
// drift, rather than trusting them to stay hand-synced silently.
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
	jetstream: ["url", "host", "port", "user", "pass", "account", "subjectPrefix", "inboxPrefix"]
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
// rejected under full CUE evaluation. Most service keys accept a
// scalar `#ServiceNeed` (the unnamed default) or an array of named
// `#ServiceNeed`s; `disk` carries `#DiskClaim`; `jetstream` carries its
// own scalar-only `#JetStreamNeed` (2.5 / ADR 0061 §6).
#Needs: {
	pg?: #ServiceNeed | [...#ServiceNeed]

	// Scalar only — no array. Two users of one application would share one
	// subject prefix and be indistinguishable (ADR 0061 §6). This narrows
	// ADR 0043's `#ServiceNeed | [...#ServiceNeed]` grammar, which that ADR
	// records as an amendment.
	jetstream?: #JetStreamNeed

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

// #JetStreamNeed — `Application.spec.*.needs.jetstream` (2.5 / ADR 0061).
// A dedicated type rather than `#ServiceNeed`, because jetstream carries
// declarations (`streams`, `consume`) and a capability flag that no other
// need has, and because two of `#ServiceNeed`'s fields are meaningless
// here.
//
// `name` and `persistent` are DECLARED IN ORDER TO BE REJECTED by the
// admission webhook (`validate_jetstream_need` in
// `admission-webhook/src/validator.rs`). Omitting them would hand the
// decision to the apiserver, whose structural schema PRUNES unknown
// fields before a validating webhook runs — so `persistent: true` would
// vanish silently and the manifest would appear to work. See ADR 0061 §6.
#JetStreamNeed: {
	selector?: [string]: string
	size?: #Size

	// Grant `$JS.API.STREAM.CREATE/UPDATE.>`. Default false. Enabling it
	// lets the application read every stream in its namespace and drain
	// every workqueue in it (ADR 0061 §4.1) — ADR 0061 §6 CLASSIFIES
	// flipping this from false/absent to true as ADR 0052 trigger #15
	// (`jetstream-dynamic-streams-enable`), a security-boundary change,
	// not a convenience. The Application controller's classifier detects
	// it (`ApplicationMigrationStrategy::detect_all` in
	// operator-controllers/migration), so enabling the flag now pauses the
	// application behind a MigrationPlan until a human approves it.
	// Turning it back off is a narrowing and stays soft.
	dynamicStreams?: bool | *false

	streams?: [...#JetStreamStream]
	consume?: [...#JetStreamConsume]

	// Present so the admission webhook CAN reject them (ADR 0061 §6) — and
	// does: `validate_jetstream_need` in admission-webhook/src/validator.rs
	// rejects either one outright on the typed path, and `check_scope_raw`
	// (same file) rejects their mere PRESENCE on the raw-decode-failure
	// path too — a scope dropping to raw is reachable in production (see
	// that function's own comment), not just a test/misconfiguration
	// case, so both paths matter. A manifest setting `persistent: true`
	// here is rejected before it ever reaches
	// `JetStreamNeed::as_service_need()` in operator-core, which would
	// otherwise silently drop it.
	name?:       string
	persistent?: bool
}

// #JetStreamStream — one declared stream under `#JetStreamNeed.streams`
// (2.5 / ADR 0061 §6). The provisioner creates (or updates) exactly this
// stream in the application's account; nothing else there is touched
// unless `dynamicStreams` is also set.
#JetStreamStream: {
	// Stream name. Two rules, neither carried in this CUE type (validation
	// philosophy — no half-measure CUE stubs; format/cross-field rules go
	// to the CRD + webhook):
	//   - Unique within the application — `validate_jetstream_need` in
	//     admission-webhook/src/validator.rs, webhook-only (a CUE
	//     cross-array-element uniqueness check isn't expressible here
	//     either).
	//   - A DNS-1123 label — restored as a `pattern` on the CRD via
	//     `schemas/crdmeta/meta.cue`'s `"…needs.jetstream.streams[].name"`
	//     schemaPatches (same apiserver-unconditional / webhook-redundant
	//     shape as `subjects`' `minItems` below), and re-enforced by
	//     `validate_jetstream_need`. This rule is NEW, not a restatement
	//     of prior behaviour: it exists because the provisioner composes
	//     this name into a NATS-side identifier joined on `_`
	//     (`nats_stream_name` in
	//     `resourceclaim-provisioner::nats_accounts`) — DNS-1123's
	//     alphabet (`[a-z0-9-]`) excludes `_`, which is what keeps that
	//     join injective. A `_` here would let two different applications
	//     compose the identical NATS stream name.
	name: string
	// Subjects the stream collects. A subject whose FIRST TOKEN is not the
	// owning application's name is fan-in — ADR 0061 §6 CLASSIFIES that as
	// ADR 0052 trigger #16 (`jetstream-foreign-subject`), same as
	// `allowPurge` below. The classifier detects it (same detector as
	// `dynamicStreams` above), so ADDING such a subject pauses the
	// application for approval; a subject already declared is not
	// re-gated, and an own-prefix subject never gates.
	//
	// Non-empty — enforced on BOTH paths, and they must agree: the CRD
	// `minItems: 1` (`schemas/crdmeta/meta.cue`'s
	// `"…needs.jetstream.streams[].subjects"` schemaPatches, same route as
	// `#SourceCredentialSpec.git.repoPrefixes`) is the apiserver-level
	// gate — it applies unconditionally, before any webhook runs.
	// `validate_jetstream_need` in admission-webhook/src/validator.rs
	// enforces the same rule again — redundant on a cluster running the
	// current CRD (schema validation runs before any validating webhook,
	// so the apiserver rejects `subjects: []` first, every time; the
	// webhook's more specific message naming the offending stream is
	// never actually seen). The redundancy is worth keeping anyway: it
	// protects a cluster whose CRD predates this `minItems` patch — an
	// operator upgrade landing ahead of the corresponding chart bump is
	// exactly the gap ADR 0047's CRD/webhook layering exists to catch. A
	// CUE-level `& [_, ...]` non-empty constraint can't live in the type
	// itself: it breaks `cue export --out openapi`, which can't
	// synthesize a concrete `default` element for a non-concrete-typed
	// minItems:1 list.
	subjects: [...string]
	storage?:   "file" | *"file" | "memory"
	retention?: "limits" | *"limits" | "interest" | "workqueue"
	maxAge?:    string
	// Required: a stream without it silently claims the whole account quota.
	maxBytes: string
	// Re-grant PURGE on this stream only. Name-scoped, so it can reach only
	// the declaring application's own data — unless the stream is fan-in,
	// in which case setting it is trigger #16 too and pauses for approval
	// (on a stream carrying only this application's own subjects it is
	// destructive-to-self, which ADR 0051's axis governs, and takes no
	// security trigger).
	allowPurge?: bool | *false

	// ── Tuning (2.28 / ADR 0065 §2.1) ───────────────────────────────
	// Each maps 1:1 onto the NACK `Stream.spec` field of the same name.
	// The enums below are NACK's own spellings, verified against the
	// pinned chart's CRD — not a re-invention, because these values are
	// passed through verbatim.
	maxMsgs?:           int
	maxMsgsPerSubject?: int
	maxMsgSize?:        int
	maxConsumers?:      int
	discard?:           "old" | *"old" | "new"
	// Requires `discard: "new"` AND `maxMsgsPerSubject > 0` — a server-side
	// rule (error 10052), measured on 2.14.3 and re-stated by the webhook so
	// the refusal names a field instead of surfacing through NACK.
	discardPerSubject?: bool
	duplicateWindow?:   string
	compression?:       "none" | *"none" | "s2"
	// Serve `$JS.API.DIRECT.GET` on this stream. Defaults OFF — today's
	// behaviour. The grant is already in the account's allow list and is
	// inert until a stream opts in, so flipping the default would change
	// every existing stream on upgrade for no asked-for reason.
	allowDirect?: bool | *false
	// Let a publisher collapse a subject with the `Nats-Rollup` header.
	// Same shape as `allowPurge` above, and for the same reason: on a
	// stream carrying only its owner's subjects this is destructive-to-self
	// (ADR 0051's axis, no security trigger); on a fan-in stream it deletes
	// a NEIGHBOUR's data, and is ADR 0052 trigger #16.
	allowRollup?:    bool | *false
	consumerLimits?: #JetStreamConsumerLimits
	description?:    string

	// ── Declared in order to be REJECTED (ADR 0065 §2.3) ────────────
	// `sources` and `mirror` were MEASURED to defeat the read half of the
	// deny vector (ADR 0061 §4.1); `republish` and `subjectTransform` are
	// the same class, re-emitting a stream's traffic under a subject the
	// application could not publish to; `placement` and `replicas` are
	// clustered topology (Tier 2+).
	//
	// Named here rather than omitted because a structural schema PRUNES an
	// unknown field BEFORE a validating webhook runs — a manifest setting
	// `mirror:` would have it vanish silently and appear to work, which is
	// exactly the failure ADR 0061 §6 established this pattern against.
	// These are the six a user plausibly writes, because they are prominent
	// in NATS's own documentation; the rest of NACK's surface is provider
	// plumbing nobody types into an application manifest and costs nothing
	// to prune. `validate_jetstream_need` rejects each one outright.
	sources?: [...{...}]
	mirror?: {...}
	republish?: {...}
	subjectTransform?: {...}
	placement?: {...}
	replicas?: int
}

// #JetStreamConsumerLimits — per-stream defaults NACK applies to consumers
// created on it (2.28). Durations are Go duration strings, as NACK expects.
#JetStreamConsumerLimits: {
	inactiveThreshold?: string
	maxAckPending?:     int
}

// #JetStreamDeadLetter — where a message goes when a consumer exhausts
// `maxDeliver` (2.28 / ADR 0065 §2.4).
//
// NATS has no dead-letter queue. What it has is an advisory published on
// `$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.<stream>.<consumer>` carrying
// the stream, the consumer, the stream sequence and the delivery count —
// METADATA, NOT THE MESSAGE BODY. This block makes that advisory
// addressable and the documentation says so plainly.
//
// The body is one fetch away: `$JS.API.STREAM.MSG.GET` at the advised
// `stream_seq` returns it, the grant is already in the allow list, and —
// measured on 2.14.3 — this holds under `retention: workqueue` too, where
// an exhausted message is NOT removed. It stays at its sequence while the
// consumer moves on, which also means nothing ever removes it until
// `maxAge`/`maxBytes` does: a workqueue accumulates its poison messages.
//
// MECHANISM, deliberately not new machinery: this materialises an ORDINARY
// DECLARED STREAM owned by the application, entering the provisioner's
// `ClaimView.streams` exactly as an entry of `streams[]` does — so the
// allow list, the deny vector, the quota pre-flight, the capture detector
// and the GC all treat it identically, with no new code path and no new
// permission. The only difference is that its `subjects` are composed by
// the platform: one exact advisory subject for this (stream, durable) pair,
// never a wildcard, so it can carry nothing but this application's own
// failures. Read it with an ordinary `consume` entry naming it.
#JetStreamDeadLetter: {
	// DLQ stream name. A DNS-1123 label, unique across this application's
	// `streams[].name` and its other `deadLetter.stream` values — it enters
	// the same `nats_stream_name(app, name)` namespace and inherits that
	// rule (webhook-enforced, same as `#JetStreamStream.name`).
	stream: string
	// Required, for the same reason it is required on a declared stream:
	// the DLQ counts against the namespace quota like any other.
	maxBytes: string
	maxAge?:  string
}

// #JetStreamConsume — one declared durable consumer under
// `#JetStreamNeed.consume` (2.5 / ADR 0061 §6). Grants read + ack on
// exactly the named stream, via the durable name; nothing else.
#JetStreamConsume: {
	// Application in the same namespace that declares `stream`. Omit for
	// the declaring application's own stream. Naming ANOTHER application
	// shares that stream between the two — stream-level permission denials
	// key on the stream — so ADR 0052 trigger #14
	// (`jetstream-consume-add`) pauses the first such entry per
	// `(from, stream)` pair for approval. An own-stream consume, and a
	// second durable on a stream already consumed, widen nothing and do
	// not gate.
	from?:  string
	stream: string
	// A DNS-1123 label — same rule, same reason, and the same
	// CRD-`pattern`-plus-webhook shape as `#JetStreamStream.name` above
	// (`"…needs.jetstream.consume[].durable"` in
	// `schemas/crdmeta/meta.cue`): the provisioner composes this into a
	// NATS-side durable name joined on `_` (`nats_durable_name` in
	// `resourceclaim-provisioner::nats_accounts`), and a `_` here would
	// let two different applications compose the identical NATS durable
	// name — silently sharing one consumer's delivery cursor.
	durable: string

	// ── Tuning (2.28 / ADR 0065 §2.2) ───────────────────────────────
	// Each maps 1:1 onto the NACK `Consumer.spec` field of the same name;
	// the enums are NACK's own spellings, verified against the pinned
	// chart's CRD. Durations are Go duration strings ("30s"), as NACK
	// expects — unlike `#Probe`, whose timings are integer seconds because
	// that is what a Kubernetes probe is specified in.
	//
	// These are not merely a convenience. An application CAN create its own
	// consumers, but NACK owns a declared durable and re-asserts its own
	// config on the next pass, so a client-side `ackWait` is reverted. Until
	// these fields existed the setting was neither client-owned nor
	// git-owned: it was unreachable.
	ackPolicy?: "explicit" | *"explicit" | "all" | "none"
	ackWait?:   string
	// `maxDeliver > len(backoff)`, STRICTLY — a server rule (error 10116),
	// measured on 2.14.3 and enforced by the webhook.
	maxDeliver?: int
	backoff?: [...string]
	maxAckPending?: int
	// Narrowing only — a filter can never widen what a durable sees, on an
	// own stream or a foreign one, so neither touches the permission model.
	// Mutually exclusive (the server rejects both), webhook-enforced.
	filterSubject?: string
	filterSubjects?: [...string]
	deliverPolicy?: "all" | *"all" | "last" | "new" | "byStartSequence" | "byStartTime" | "lastPerSubject"
	// Each requires its own `deliverPolicy`, and is REJECTED rather than
	// ignored under any other — an ignored start position is a consumer
	// that silently reads from the wrong place.
	optStartSeq?:        int
	optStartTime?:       string
	replayPolicy?:       "instant" | *"instant" | "original"
	maxWaiting?:         int
	maxRequestBatch?:    int
	maxRequestExpires?:  string
	maxRequestMaxBytes?: int
	inactiveThreshold?:  string
	rateLimitBps?:       int
	headersOnly?:        bool
	memStorage?:         bool
	sampleFreq?:         string
	description?:        string

	// Rejected without `maxDeliver` > 0: with no delivery ceiling the
	// advisory never fires and the DLQ is permanently empty — a manifest
	// that looks like it works and says nothing, which is the failure class
	// the declare-to-reject pattern exists for.
	deadLetter?: #JetStreamDeadLetter

	// ── Declared in order to be REJECTED (ADR 0065 §2.3) ────────────
	// Push-mode delivery is performed by the SERVER, outside the
	// application's publish permissions — a write channel into a
	// neighbour's prefix. These four are the push surface, and they are
	// named here rather than omitted for the reason ADR 0061 §6 gives: a
	// structural schema PRUNES an unknown field before a validating webhook
	// runs, so a manifest setting `deliverSubject` would have it vanish
	// silently and appear to work. `validate_jetstream_need` rejects each
	// one outright.
	deliverSubject?:    string
	deliverGroup?:      string
	flowControl?:       bool
	heartbeatInterval?: string
	// Clustered topology, Tier 2+. Declared for the same reason.
	replicas?: int
}

// #Resources — container resource requests/limits (2.16d). Keys are
// resource names (cpu, memory, ephemeral-storage); values are Kubernetes
// quantities ("100m", "128Mi", "1Gi"). Quantity format + request<=limit are
// enforced by the admission webhook; the CRD renders each map as
// additionalProperties:{type:string} (no preserve-unknown needed).
#Resources: {
	requests?: [string]: string
	limits?: [string]:   string
}

// #Probes — the three Kubernetes probes, each optional and each
// independently disableable (2.28 / ADR 0065 §1).
//
// An absent `readiness` with a declared `expose.port` renders a TCP-connect
// readiness probe on that port. Nothing is guessed — the port is the
// manifest's own — and without it every rolling update has a window in
// which the new pod is Ready and taking traffic while its process is still
// binding. Opt out with `readiness: {enabled: false}`.
//
// A declared, enabled `liveness` with no `startup` derives a startup probe
// against the same target with a long failure budget (ADR 0065 §1.4), so
// that adding one liveness probe cannot CrashLoop a slow-starting
// application. An explicit `startup` wins outright — there is no
// field-level merge between the two.
#Probes: {
	liveness?:  #Probe
	readiness?: #Probe
	startup?:   #Probe
}

// #Probe — one probe. The FORM is discriminated by `path`: present means an
// HTTP GET, absent means a TCP connect. There is no `httpGet` / `tcpSocket`
// nesting — the Kubernetes shape exists to carry five action kinds and this
// surface carries two (ADR 0065 §1.1).
//
// `path`'s leading `/`, the port-resolution rule and every cross-field
// invariant live in the CRD + the admission webhook, not here: a regex stub
// in CUE would hint at validation this type does not actually perform — the
// same policy `#ApplicationSpec.image` records above.
//
// Timings are integer SECONDS under Kubernetes' own field names, NOT the
// duration strings `#JetStreamStream.maxAge` uses. The inconsistency is
// deliberate: a Kubernetes probe is specified in whole seconds, so a string
// field would accept "500ms" and we would then have to reject it. Better
// that the wrong thing cannot be written down.
//
// EVERY default this type implies is applied by the renderer
// (`operator-rendering/src/probes.rs`), including the `*true` and `*"http"`
// written below. A CUE `*x` default does not reach the generated CRD —
// crdgen's `structural::resolve` strips it and the R4-M2 assertion fails
// the build if one survives, because behaviour belongs to the renderer and
// not to the apiserver (ADR 0047) — and `cue export` omits an optional
// field regardless. The markers below therefore document intent to a reader
// and to `cue vet`; they are not a second mechanism.
#Probe: {
	// Render this probe. `false` keeps the declaration in git while taking
	// the probe off the pod — the reason it is a field and not an omission.
	// Declaring it alongside a full probe body is NOT a contradiction.
	enabled?: bool | *true

	// HTTP request path ("/healthz"). Present => HTTP probe; absent => TCP.
	path?: string

	// Target port. Default: this scope's effective `expose.port`. A probe
	// with neither is rejected by the webhook, naming which probe.
	port?: int & >0 & <=65535

	// HTTP only — the webhook rejects either without `path` rather than
	// ignoring it, so a typo is not silently dropped.
	scheme?: "http" | *"http" | "https"
	headers?: [string]: string

	initialDelaySeconds?: int & >=0
	periodSeconds?:       int & >0
	timeoutSeconds?:      int & >0
	failureThreshold?:    int & >0
	successThreshold?:    int & >0
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
