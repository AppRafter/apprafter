// SPDX-License-Identifier: MIT
//
// cue-cmp intra-bundle consistency fixture (ADR 0063 §Decision 5) —
// TWO workloads in one package declaring DIFFERENT namespaces.
//
// A manifest package is one bundle: one registration, one namespace
// (ADR 0062). Argo CD applies every document this render emits into the
// single `destination.namespace` of the registration, and the CLI's
// namespace picker reads one value too — so a second namespace declared
// here cannot be honoured by either. Before the guard both documents
// rendered and exit was 0; the disagreement only showed up later, as a
// workload sitting in a namespace nobody registered.
//
// The render layer is the only place that sees both documents at once:
// the admission webhook gets one object per AdmissionReview and has no
// sibling to compare against.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

nsOne: v1alpha1.#Application & {
	metadata: {
		name:      "split-ns-one"
		namespace: "one"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}

nsTwo: v1alpha1.#Application & {
	metadata: {
		name:      "split-ns-two"
		namespace: "two"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
