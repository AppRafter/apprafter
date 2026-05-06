// SPDX-License-Identifier: FSL-1.1-MIT

package v1alpha1

// Application is the dev-facing unit of deployment.
//
// Per-environment overrides are expressed via CUE unification on the
// `environments` map; see spec.md §3.1 and ADR 0004.
//
// Skeleton schema. Full field set (env sources, connects, autoscale
// triggers, network egressIP) is filled in during phase 1.7 / 1.9.
#Application: {
	#TypeMeta
	kind:     "Application"
	metadata: #ObjectMeta

	base?: #ApplicationSpec

	environments?: [string]: #ApplicationSpec
}

#ApplicationSpec: {
	image?:    string
	replicas?: int & >=0
	expose?: {
		port:     int & >0 & <=65535
		public?:  bool | *false
		network?: "public" | "internal" | "vpn"
	}
	needs?: [string]: _
	env?: [string]:   string

	autoscale?: {
		on?:  string
		min?: int & >=0
		max?: int & >=1
	}

	// confidential opts the workload into Tier-4 attestation +
	// Kata-CC scheduling. See ADR 0006/0007 for secrets context and
	// spec.md §4.1 for the substrate model.
	confidential?: bool | *false
}
