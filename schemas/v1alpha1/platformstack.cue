// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package v1alpha1

// PlatformStack is the singleton declarative control plane for
// the platform version. Per spec §3.11 + ADR 0026.
//
// Exactly one CR exists per cluster, named `default` in
// namespace `apprafter-system`. The singleton constraint is
// enforced by the admission webhook (name + namespace match),
// not by CRD validation (CRDs are namespaced; only name+ns
// uniqueness is k8s-native).
//
// PlatformController (B.1.73) reconciles spec changes against
// the upstream OCI chart and patches the umbrella Application.
// In B.1.72 the CR exists but no controller runs; status fields
// remain empty until the controller lands.
#PlatformStack: {
	#TypeMeta
	kind:     "PlatformStack"
	metadata: #ObjectMeta

	spec: #PlatformStackSpec

	// Status is the controller's report surface. Empty until
	// PlatformController lands in 1.73.
	status?: #PlatformStackStatus
}

#PlatformStackSpec: {
	// Release channel. Default `stable`. The PlatformController
	// resolves the latest version in this channel when `pin` is
	// unset. When `pin` is set, channel is ignored for
	// resolution but still informs which channel
	// `status.availableVersion` reports against.
	channel: "stable" | "beta" | "edge" | *"stable"

	// Optional explicit version freeze. Semver string when set.
	// Overrides `channel` for version resolution.
	pin?: string & =~"^[0-9]+\\.[0-9]+\\.[0-9]+(-[0-9A-Za-z.-]+)?$"

	// Default false. When true, PlatformController bumps
	// automatically iff the upstream diff classifies as `safe`
	// in the chart's `compatibility.yaml`. Non-safe diffs
	// surface as MigrationPlan instead.
	autoUpgrade: bool | *false

	// Soft per-cluster default environment (ADR 0044) — a CLI pre-selection
	// convenience for `app add`, never a hard gate.
	defaultEnvironment?: string

	// Cluster-wide egress posture for app-derived CiliumNetworkPolicies (2.10).
	network?: {
		egress?: {
			profile?: "internet" | "internal" | "strict"
		}
	}

	// Opt-in automated off-site backup (2.6d-4). Absent = disabled. The
	// platform-stack `backup` component is templated from this + gated on
	// `enabled`. Credentials are NEVER here — only a `credentialRef` name.
	backup?: {
		enabled:  bool | *false
		schedule: string | *"0 3 * * *"
		bucket:   string
		credentialRef: {name: string}
		stagingMode:       "monolithic" | "sequential" | *"monolithic"
		stagingSizeLimit?: string
		retention?: {
			keepDaily?:   int & >0
			keepWeekly?:  int & >0
			keepMonthly?: int & >0
			enforce:      "operator" | "cluster" | *"operator"
		}
		checkSchedule:   string | *"0 6 * * 0"
		checkReadData:   bool | *false
		failureWebhook?: string
	}

	// Chart pull source. Defaults match the canonical AppRafter
	// upstream; fork installs override `repoURL` while leaving
	// `upstream` pointing at canonical for availability
	// visibility (ADR 0028).
	source: {
		// Canonical AppRafter upstream URL — informational only.
		// Used by PlatformController to query availability even
		// when the cluster pulls from a fork's `repoURL`.
		upstream: string | *"oci://ghcr.io/apprafter/platform-stack"

		// Actual chart pull URL. May point at a fork.
		repoURL: string | *"oci://ghcr.io/apprafter/platform-stack"

		// How often PlatformController polls upstream for newer
		// versions. Go duration string. Default 6h; minimum 1h
		// (webhook-enforced cross-field rule; OpenAPI v3 can't
		// express duration parsing).
		checkInterval: string | *"6h"
	}

	// Global values projected onto the umbrella chart.
	values: {
		// AppRafter tier this cluster runs at. Numeric per
		// types.cue#Tier; the chart's tier overlay selects the
		// component subset.
		tier: #Tier
		// Public domain for ingress + cert-manager. Optional —
		// solo tier without a domain works fine.
		domain?: string
	}

	// Per-component overrides. Keyed by component name (must
	// match a `_components.<name>` entry in the umbrella chart;
	// advisory-only — webhook warns but does not reject when a
	// key references a component the current chart version
	// does not declare, so users can pre-declare for future
	// chart versions).
	overrides?: [string]: {
		// Component version freeze. When set, the umbrella
		// chart's value for this component is ignored.
		pin?: string
		// Component-specific values to merge into the umbrella
		// chart's component values. Free-form per component.
		values?: {...}
		// Disable the component entirely.
		enabled?: bool
	}
}

#PlatformStackStatus: {
	// Version currently deployed to the cluster (derived from
	// the umbrella Application's reconciliation state).
	currentVersion?: string

	// Version PlatformController is targeting. Differs from
	// `currentVersion` mid-roll. Differs from
	// `availableVersion` when a channel bump is gated by a
	// MigrationPlan or when `pin` is set behind upstream.
	targetVersion?: string

	// Latest version available in the configured channel from
	// the configured `source.upstream`. Updated by
	// PlatformController on every `source.checkInterval` poll.
	availableVersion?: string

	// RFC 3339 timestamp of the last upstream availability
	// check. Empty before the first poll.
	lastUpstreamCheck?: string

	// Per-component status. Empty until PlatformController
	// populates it; remains empty in 1.72.
	components?: [...{
		name:    string
		version: string
		ready:   bool
	}]

	// Ring buffer of recent version transitions for rollback +
	// audit. Empty until PlatformController writes entries.
	versionHistory?: [...{
		version:   string
		appliedAt: string
		outcome:   "succeeded" | "rolled-back" | "failed"
	}]

	// Standard Kubernetes conditions. Includes
	// `UpgradeAvailable` (set True when availableVersion >
	// currentVersion in the configured channel) and
	// `Reconciling` (set True mid-upgrade).
	conditions?: [...{
		type:               string
		status:             "True" | "False" | "Unknown"
		reason?:            string
		message?:           string
		lastTransitionTime: string
	}]
}
