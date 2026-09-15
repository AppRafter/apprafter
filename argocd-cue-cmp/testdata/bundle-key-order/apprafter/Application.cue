// SPDX-License-Identifier: MIT
//
// cue-cmp bundle fixture — the ORDER rule, and the only fixture here
// that can fail when the rule is dropped.
//
// Both layers list a bundle's workloads: the sidecar emits one YAML
// document per workload and names them in a refusal summary, the CLI
// prints a roster. ADR 0063 §Decision 5 asks the two to agree, and
// both used to derive their sequence from `cue export . --out json`'s
// own key order. That order is not a CUE contract — it is an evaluator
// detail, and it MOVED:
//
//   keys sorted        apiTier cacheTier jobsTier webTier
//   cue v0.10.0        cacheTier webTier jobsTier apiTier
//   cue v0.16.0        cacheTier jobsTier webTier apiTier
//
// v0.10.0 is what the sidecar image pins and what CI installs; a
// developer shell resolves whatever nixpkgs currently carries. So the
// two halves agreed only while both ends happened to run the same cue,
// and the assertions written against one version went red on the other
// (`argocd-cue-cmp-check` + `test`, 2026-09-15).
//
// Both layers now sort by TOP-LEVEL KEY, which no cue version gets to
// move. This fixture is what proves it: its four keys come out in three
// different sequences above, none of them sorted, so an implementation
// that goes back to reading the export's order fails here on any cue.
//
// `metadata.name` deliberately runs OPPOSITE to the key order
// (apiTier → keyorder-zulu … webTier → keyorder-whiskey): sorting the
// rows by what is printed instead of by the key they are keyed on is
// also a red test, not a coincidence that passes.
//
// Consistent in every respect the four intra-bundle checks look at —
// one namespace, distinct names, no `spec.environment`, no
// package-scope manifest — because a refusal would emit nothing and
// there would be no order to read. `bundle-key-order-split-ns/` is the
// refusal half of the same question.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

webTier: v1alpha1.#Application & {
	metadata: {
		name:      "keyorder-whiskey"
		namespace: "keyorder"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}

apiTier: v1alpha1.#Application & {
	metadata: {
		name:      "keyorder-zulu"
		namespace: "keyorder"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
