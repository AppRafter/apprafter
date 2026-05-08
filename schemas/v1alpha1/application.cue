// SPDX-License-Identifier: FSL-1.1-MIT

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
// Fields removed from the v1alpha1 surface: `needs`, `autoscale`,
// `confidential`. They re-appear in their owning subphases (2.x for
// ResourceClaim wiring, 4.x for confidential workloads).
#Application: {
	#TypeMeta
	kind:     "Application"
	metadata: #ObjectMeta

	spec: {
		base?: #ApplicationSpec

		environments?: [string]: #ApplicationSpec
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
}
