// SPDX-License-Identifier: FSL-1.1-MIT

package v1alpha1

// ServiceProvider declares a backend implementation for a platform-
// service type. Multiple providers may coexist; ResourceClaims select
// among them via labels.
//
// Skeleton schema; tier-aware defaults and per-backend config land in
// phase 2.1.
#ServiceProvider: {
	#TypeMeta
	kind:     "ServiceProvider"
	metadata: #ObjectMeta
	spec: {
		// Built-in service type. Plugins may extend this set via
		// ServiceProviderPlugin.
		type: #PlatformServiceType

		// Implementation backend identifier (e.g., "cloudnative-pg",
		// "aws-rds", "altinity-clickhouse-operator").
		backend: string

		// Selector labels — ResourceClaim.spec.selector matches against
		// these.
		labels?: [string]: string

		// Backend-specific configuration.
		config?: _
	}

	status?: {
		health?: "healthy" | "degraded" | "unhealthy" | "unknown"
	}
}
