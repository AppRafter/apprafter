// SPDX-License-Identifier: MIT
//
// cue-cmp intra-bundle consistency fixture (ADR 0063 §Decision 5) —
// TWO workloads in one package sharing one (namespace, name).
//
// Two CUE values, two rendered YAML documents — but ONE apiserver
// identity. An Application is identified by (namespace, name), so
// whichever document Argo CD applies last silently overwrites the
// other and one of the two workloads never runs. The images differ so
// that "which one won" is observable at all; nothing in the render or
// the sync reports that a choice was made.
//
// The webhook cannot catch this either: below the render layer it sees
// a CREATE followed by an UPDATE, which is indistinguishable from an
// ordinary edit.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

dupOne: v1alpha1.#Application & {
	metadata: {
		name:      "dup-app"
		namespace: "dup-demo"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}

dupTwo: v1alpha1.#Application & {
	metadata: {
		name:      "dup-app"
		namespace: "dup-demo"
	}
	spec: base: {
		image:    "nginxdemos/hello:0.4"
		replicas: 3
	}
}
