// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package v1alpha1

// Application is the dev-facing unit of deployment.
//
// v1alpha1 field set (see plan.md §1.7):
//   - image    — required-ish; the renderer fails if neither base
//                nor any environment override sets it.
//   - replicas — non-negative; defaults to 1 at render time.
//   - expose   — optional Gateway-side exposure (port + visibility).
//   - env      — string→string map. Literals only; secret/configmap
//                refs land in 2.x with ResourceClaim and 4.x with
//                OpenBao.
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
		port:    int & >0 & <=65535
		public?: bool | *false
		// Defaults to "internal" when the field is unset in the
		// manifest. The OpenAPI v3 CRD enforces the enum at the
		// kube-apiserver layer; the renderer consumes the field
		// for HTTPRoute emission in phase 1.9c.
		network?: "public" | "internal" | "vpn" | *"internal"
	}

	// Literal string values only — no secret refs in v1alpha1.
	env?: [string]: string

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
// `Application.spec.*.needs.disk` (2.6b / ADR 0043). Each entry
// becomes a `ResourceClaim` of `type: disk`; the provisioner creates
// a standalone RWO PVC (unowned, GC-managed for retention) and the
// renderer mounts it into the workload Deployment (`replicas: 1`,
// `strategy: Recreate`). Deferred fields (`shareMode`, `backup`,
// `autoExpand`, replicated/shared classes) are intentionally absent
// until implemented — no half-measure CUE stubs.
#DiskClaim: {
	// `(type, name)` claim identity. Omit → derived from the last
	// segment of `mountPath` (`/var/lib/uploads` → `uploads`);
	// explicit wins. Must be a DNS-1123 label (it becomes part of the
	// PVC name). The webhook enforces uniqueness within `disk`.
	name?: string

	// Requested capacity as a Kubernetes quantity (`"10Gi"`).
	// Required. The webhook validates it parses as a quantity.
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
