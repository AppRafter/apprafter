// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Example Application manifest used as a vet-time fixture. Mirrors
// the parser example from spec.md §3.1 in skeleton form; the full
// per-environment unification example lands in phase 1.9 / 2.9.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

parser: v1alpha1.#Application & {
	metadata: {
		name:      "parser"
		namespace: "demo"
	}
	spec: {
		base: {
			image:    "ghcr.io/example/parser:latest"
			replicas: 3
			expose: {
				port:   8080
				public: false
			}
			env: {
				LOG_LEVEL: "info"
			}
			needs: {
				pg: {selector: {tier: "integrated"}}
			}
		}
		environments: {
			dev: {
				replicas: 1
				expose: {
					port:    8080
					network: "vpn"
				}
			}
			prod: {
				replicas: 3
				needs: {
					pg: {selector: {tier: "integrated"}, size: "small"}
				}
			}
		}
	}
}
