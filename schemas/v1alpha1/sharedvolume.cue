// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package v1alpha1

// SharedVolume is a cross-app shared persistent volume (2.6c / ADR 0043).
// Multiple Applications may reference one SharedVolume; the Application
// controller mounts the backing PVC into each consumer workload via a
// `needs.disk.sharedVolume` reference. The provisioner creates a single
// RWX PVC for the volume and tracks `refCount` in status.
#SharedVolume: {
	#TypeMeta
	kind:     "SharedVolume"
	metadata: #ObjectMeta
	spec: {
		// Kubernetes quantity (e.g. "5Gi"). Required; the provisioner
		// creates the backing PVC with this storage request.
		// (quantity format enforced by the admission webhook, not CUE).
		size: string
		// Storage class hint. Only "local" is wired in Phase 2.6c.
		class?: string
	}

	status?: {
		ready?:    bool
		pvcRef?:   string
		refCount?: int & >=0
		capacity?: {
			usedBytes?:     int
			capacityBytes?: int
		}
		conditions?: [...#SharedVolumeCondition]
	}
}

#SharedVolumeCondition: {
	type:               string
	status:             "True" | "False" | "Unknown"
	lastTransitionTime: string
	reason?:            string
	message?:           string
}
