// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// Backstage portal. Conditional on the operator setting
// `values.backstage.domain` — without a domain the gateway /
// HTTPRoute / Certificate triad has nowhere to attach.
//
// Defaults to disabled in `cue/tier_solo.cue` because most
// solo tier-1 installs don't run their own DNS; tier-2+
// overlays flip `enabled` on and rely on the operator setting
// `values.backstage.domain` from the user's
// `Infrastructure.cue`.
_components: backstage: #Component & {
	name:      "backstage"
	enabled:   bool | *false
	namespace: "backstage"
	source: {
		// Backstage charts ship as plain manifests in our
		// monorepo; ADR 0028 lists this as the "Git source"
		// shape. Path resolves at chart-render time.
		repoURL: "https://github.com/apprafter/apprafter.git"
		path:    "manifests/tier-1/backstage"
	}
	// Git source uses `targetRevision` as a Git ref; pinned
	// to the same v0.1.91 tag as the rest of the stack.
	version: "v0.1.91"
	values: {
		// Defaults the operator overlay fills in:
		// - domain — required when enabled=true.
		// - image — defaults to BACKSTAGE_DEFAULT_IMAGE from
		//   cli-providers if unset.
		domain?: string
		image:   string | *"ghcr.io/apprafter/backstage:v0.1.91"
	}
}
