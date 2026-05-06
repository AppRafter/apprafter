// SPDX-License-Identifier: FSL-1.1-MIT

package v1alpha1

// ServiceProviderPlugin extends the platform with new service types
// or backends, distributed as OCI images implementing the
// ServiceProviderInterface gRPC protocol. See spec.md §3.6 and
// phase 7.1 / 7.2.
#ServiceProviderPlugin: {
	#TypeMeta
	kind:     "ServiceProviderPlugin"
	metadata: #ObjectMeta
	spec: {
		// New service type registered by this plugin (e.g., "mysql",
		// "mongodb", "kafka"). Becomes available as `needs.<type>` on
		// Applications.
		type: string

		// OCI image reference of the gRPC plugin sidecar.
		implementation: string

		// Plugin-specific configuration, validated by the plugin's
		// own Schema RPC.
		config?: _
	}

	status?: {
		ready?: bool
	}
}
