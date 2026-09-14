// SPDX-License-Identifier: MIT
//
// cue-cmp intra-bundle consistency fixture (ADR 0063 §Decision 5) —
// TWO workloads in one package declaring DIFFERENT `spec.environment`.
//
// A manifest package is one bundle: one registration, one environment
// (ADR 0062). The environment is a property of the REGISTRATION
// (`apprafter app add --env`), and `inject_env` stamps the registered
// value onto every document unconditionally — so the guard has to run
// BEFORE injection or there is nothing left to compare: after
// `.spec.environment = $e` both documents read the same and the
// divergence is erased without a trace.
//
// Before the guard: exit 0 and two documents that deploy under two
// different environment semantics from one registration — or, with a
// registered env, two documents whose declared values were silently
// overwritten.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

envOne: v1alpha1.#Application & {
	metadata: {
		name:      "split-env-one"
		namespace: "env-demo"
	}
	spec: {
		environment: "dev"
		base: {
			image:    "nginxdemos/hello:plain-text"
			replicas: 1
		}
	}
}

envTwo: v1alpha1.#Application & {
	metadata: {
		name:      "split-env-two"
		namespace: "env-demo"
	}
	spec: {
		environment: "prod"
		base: {
			image:    "nginxdemos/hello:plain-text"
			replicas: 1
		}
	}
}
