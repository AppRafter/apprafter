// SPDX-License-Identifier: MIT
//
// cue-cmp intra-bundle consistency fixture (ADR 0063 §Decision 5) — the
// PARTIAL form of the environment divergence: one workload declares
// `spec.environment`, its sibling declares none.
//
// This is the case the guard's "absent counts as its own value" rule
// exists for, and it is deliberately a separate fixture from
// `bundle-split-env/` because it is the ambiguous one. The reasoning is
// stated at the check itself in entrypoint.sh; in short, an absent
// `spec.environment` is not "unspecified", it is the BASE-ONLY deploy —
// a different deployment semantic from `environment: "dev"`, not a
// blank waiting to be filled — so a bundle holding one of each is two
// environments, which ADR 0062 does not allow.
//
// `inject-fixture-multi/` is the negative twin already in the tree:
// two workloads, NEITHER declaring an environment, which is the normal
// single-environment bundle and must keep rendering both documents in
// silence.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

partialDeclared: v1alpha1.#Application & {
	metadata: {
		name:      "env-partial-declared"
		namespace: "env-partial-demo"
	}
	spec: {
		environment: "dev"
		base: {
			image:    "nginxdemos/hello:plain-text"
			replicas: 1
		}
	}
}

partialAbsent: v1alpha1.#Application & {
	metadata: {
		name:      "env-partial-absent"
		namespace: "env-partial-demo"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
