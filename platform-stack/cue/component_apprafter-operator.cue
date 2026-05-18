// SPDX-License-Identifier: FSL-1.1-MIT

package platformstack

// AppRafter operator — the `apprafter.io/v1alpha1.Application`
// reconciler. Pulled from our public GHCR registry. The chart
// itself lives in `operator/chart/` in the AppRafter monorepo
// and is published via `.github/workflows/release-operator.yml`
// in lockstep with the `apprafter` CLI version (the cli has a
// `RELEASED_OPERATOR_VERSION` constant that drives the default
// tag).
//
// platform-stack 0.1.0 pins the operator at v0.1.91 (the
// closing tag of Track A). Future platform-stack versions bump
// this together with their own `version` field — the
// compatibility metadata in `cue/compatibility.cue` records the
// pairing.
_components: "apprafter-operator": #Component & {
	name:      "apprafter-operator"
	enabled:   bool | *true
	namespace: "apprafter-system"
	source: {
		repoURL: "oci://ghcr.io/apprafter"
		chart:   "apprafter-operator"
	}
	version: "v0.1.91"
	values: {
		image: {
			repository: string | *"ghcr.io/apprafter/apprafter-operator"
			// Defaults to the chart version unless explicitly
			// overridden (fork / dev builds).
			tag: string | *"v0.1.91"
		}
		// Leader-election guards (10s renew / 30s expiry) match
		// the in-tree settings; tier overlays may bump replicas.
		replicas: int | *1
	}
}
