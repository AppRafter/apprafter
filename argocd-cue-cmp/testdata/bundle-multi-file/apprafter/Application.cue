// SPDX-License-Identifier: MIT
//
// cue-cmp intra-bundle fixture — a CONSISTENT bundle whose two
// workloads live in TWO FILES of one package directory.
//
// Every other bundle-* fixture here is a single `.cue` file, and that
// gap is why `apprafter app validate` could report `1 workload` for a
// two-workload bundle for a whole release without a red test: its
// default discovery resolved the FILE `apprafter/Application.cue` and
// dropped every sibling, so all four cross-workload checks saw N=1 and
// none of them could fire.
//
// The shape is this repository's own `landing/web/apprafter/` reduced
// to its skeleton — prod in `Application.cue`, preview in
// `Application-preview.cue`, one package, one namespace — because that
// is the only multi-file bundle that exists today and is therefore the
// one the CLI has to be right about.
//
// It also pins ORDER. `Application-preview.cue` sorts BEFORE
// `Application.cue` (`-` is 0x2D, `.` is 0x2E), so `cue def ./...`
// names `multiFileWebPreview` first while `cue export . --out json` —
// what the sidecar hands `jq to_entries` — emits `multiFileWeb` first.
// A layer reading the wrong one prints the same finding in a different
// sequence from the Argo CD tile.

package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

multiFileWeb: v1alpha1.#Application & {
	metadata: {
		name:      "multi-file-web"
		namespace: "multi-file"
	}
	spec: base: {
		image:    "nginxdemos/hello:plain-text"
		replicas: 1
	}
}
