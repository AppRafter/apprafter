// SPDX-License-Identifier: FSL-1.1-MIT

// Package v1alpha1 contains the AppRafter platform CRD schemas.
// This is a skeleton: full per-CRD field sets are filled in during
// the corresponding implementation phases (Application in 1.7,
// ServiceProvider/ResourceClaim in 2.1/2.2, AccessGrant in 4.5,
// MigrationPlan in 4.16, etc.).
package v1alpha1

#APIVersion: "apprafter.io/v1alpha1"

#TypeMeta: {
	apiVersion: #APIVersion
	kind:       string
}

// ObjectMeta is a minimal subset of metav1.ObjectMeta. The full
// imported Kubernetes type lands in schemas/k8s/ during phase 1.7.
#ObjectMeta: {
	name:       string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$"
	namespace?: string
	labels?: [string]:      string
	annotations?: [string]: string
}

#Tier: 1 | 2 | 3 | 4

// Built-in platform-service types. ServiceProviderPlugin may extend
// this set at runtime by registering additional types.
#PlatformServiceType:
	"pg" |
	"jetstream" |
	"clickhouse" |
	"redis" |
	"s3" |
	"notifications"

#Size: "nano" | "small" | "medium" | "large" | "xlarge"

#Capability: "read" | "write" | "exec" | "admin"
