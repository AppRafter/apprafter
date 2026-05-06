// SPDX-License-Identifier: FSL-1.1-MIT

package v1alpha1

// InfrastructureProviderPlugin extends `platform-cli` with support
// for an additional cloud or substrate, typically by wrapping an
// OpenTofu module. See ADR 0011 and spec.md §3.7 / §4.12.
#InfrastructureProviderPlugin: {
	#TypeMeta
	kind:     "InfrastructureProviderPlugin"
	metadata: #ObjectMeta
	spec: {
		// Cloud / virtualization platform identifier (e.g., "ovh",
		// "scaleway", "vsphere", "proxmox").
		provider: string

		// OCI image reference that contains the CUE→OpenTofu translator
		// and state-importer.
		implementation: string

		// OpenTofu module wrapped by the plugin (registry reference
		// or git URL).
		tofuModule?: string

		// Plugin-specific configuration.
		config?: _
	}
}
