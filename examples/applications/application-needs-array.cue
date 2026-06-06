// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Example Application declaring the generalized `needs` format
// (Phase 2.6b / ADR 0043): multiple named claims of one type via the
// array form, plus `needs.disk` (persistent block storage). Used as a
// `cue vet` fixture for `#ServiceNeed.name`, the scalar|array union,
// and `#DiskClaim`.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

needsArrayApp: v1alpha1.#Application & {
	metadata: {
		name:      "warehouse"
		namespace: "demo"
	}
	spec: {
		base: {
			image:    "ghcr.io/example/warehouse:latest"
			replicas: 1
			expose: {
				port:   8080
				public: false
			}
			needs: {
				// Array form: two named pg claims of one type.
				// (type, name) is the claim identity → `DATABASE_URL`
				// disambiguation downstream.
				pg: [
					{name: "primary", selector: {tier: "integrated"}},
					{name: "analytics", selector: {tier: "integrated"}, size: "small"},
				]
				// Persistent block storage, mounted into the workload.
				disk: [
					{name: "data", size: "1Gi", mountPath: "/data"},
				]
			}
		}
		environments: {
			prod: {
				replicas: 1
				needs: {
					// Scalar form still valid (single unnamed default).
					redis: {selector: {tier: "integrated"}}
					disk: [
						{name: "data", size: "10Gi", mountPath: "/data"},
						{size: "1Gi", mountPath: "/cache", class: "local", readOnly: true},
					]
				}
			}
		}
	}
}
