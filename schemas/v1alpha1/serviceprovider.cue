// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package v1alpha1

// ServiceProvider declares a backend implementation for a platform-
// service type (spec §3.2). Multiple providers may coexist; a
// `ResourceClaim`'s `spec.selector` is matched against each
// ServiceProvider's `metadata.labels` (the 2.3 scheduler). Namespaced
// — the platform's providers live in `apprafter-system`; the scheduler
// matches claims from app namespaces against them cross-namespace.
//
// There is intentionally no spec-level label bag: selector matching
// uses the provider's `metadata.labels` directly — do not add a
// `spec.labels` field.
//
// `config` is backend-defined and intentionally untyped here — the
// CRD declares `x-kubernetes-preserve-unknown-fields: true` on it.
// Non-empty `backend` and the closed `type` enum are enforced at the
// CRD + admission-webhook layers (CRD/webhook may be stricter than
// CUE — see CLAUDE.md). Tier-aware default provider definitions land
// with the concrete providers in 2.4–2.6, not here.
#ServiceProvider: {
	#TypeMeta
	kind:     "ServiceProvider"
	metadata: #ObjectMeta
	spec:     #ServiceProviderSpec
	status?:  #ServiceProviderStatus
}

#ServiceProviderSpec: {
	// Built-in platform-service type. Closed enum in v1alpha1;
	// ServiceProviderPlugin extends it at runtime in Phase 7.
	type: #PlatformServiceType

	// Implementation backend identifier. The wired in-cluster backends
	// are "cloudnative-pg" (pg), "dragonfly" (redis), and "disk"
	// (persistent block storage, 2.6b / ADR 0043); an external backend
	// such as "aws-rds" plugs in here too. The backend set is
	// intentionally OPEN (community/fork backends, gRPC sidecar providers
	// in Phase 7) — kept an open string at the CUE layer; the CRD +
	// webhook enforce only non-empty (CRD/webhook may be stricter than
	// CUE — see CLAUDE.md).
	backend: string

	// Backend-specific configuration, opaque to the platform.
	config?: _
}

#ServiceProviderStatus: {
	// Controller-reported backend health. Empty until the provider
	// scheduler/health probes land (2.3+).
	health?: "healthy" | "degraded" | "unhealthy" | "unknown"
}
