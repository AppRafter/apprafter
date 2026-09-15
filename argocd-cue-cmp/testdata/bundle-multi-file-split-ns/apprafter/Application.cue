// SPDX-License-Identifier: MIT
//
// cue-cmp intra-bundle fixture — an INCONSISTENT bundle spread over
// TWO FILES of one package directory: the two workloads declare
// different `metadata.namespace`.
//
// `bundle-split-ns/` already pins that divergence inside ONE file. The
// point of this one is the FILE BOUNDARY, which is what the CLI's
// default discovery used to lose: resolving the single file
// `apprafter/Application.cue` leaves the check with one row, and a row
// cannot disagree with itself. Measured on the pre-fix CLI, this
// fixture validated `✓ valid` while `sh entrypoint.sh generate` refused
// it — the local twin returning the OPPOSITE verdict from the layer it
// is a twin of.
//
// One manifest package is one bundle: one registration, one namespace
// (ADR 0062). Argo CD applies every document this render emits into the
// single `destination.namespace` of the registration, so the second
// namespace cannot be honoured whichever file it was written in.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

splitFileWeb: v1alpha1.#Application & {
	metadata: {
		name:      "split-file-web"
		namespace: "prod"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
