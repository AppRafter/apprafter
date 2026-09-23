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

	// Edge-firewall posture of the node this cluster runs on (A4).
	//
	// Nothing in the cluster reads this. The firewall itself is a CLOUD
	// object the CLI reconciles out-of-cluster from the operator's target
	// store, which is the only place the intent used to live — so a backup
	// could not see it, a restore onto a new target re-provisioned the node
	// with 80/443 open to the internet, and nothing said so.
	//
	// Recording it HERE is what makes it travel: `PlatformStack/default` is
	// the first object every backup captures, including the scheduled
	// in-cluster runner's, which has no target store to read. A restore
	// replays the CR and reconciles the destination target's firewall from
	// it.
	//
	// `cloudflareOrigin` is deliberately three-valued: absent means the
	// cluster never recorded an answer (a CR written before this field
	// existed), NOT "the firewall was off". A reader that collapses the two
	// tells an operator the source had its ports open when the snapshot
	// simply never said.
	firewall?: {
		// Is the node's 80/443 restricted to Cloudflare's IP ranges
		// (`apprafter target firewall cloudflare-origin`, 1.83d)?
		cloudflareOrigin?: bool
	}

	// Cluster-wide vertical-autoscaling posture (2.16e / ADR 0054). Absent =
	// operator uses compiled-in tier defaults (read-with-fallback; the operator
	// never writes this back). `mode` selects the VPA update policy the operator
	// stamps on every managed app's VerticalPodAutoscaler.
	resources?: {
		autoscale?: {
			mode: "full" | "up-only" | "off" | *"full"
			minAllowed?: [string]: string
			maxAllowed?: [string]: string
		}
	}

	// Opt-in automated off-site backup (2.6d-4). Absent = disabled. The
	// platform-stack `backup` component is templated from this + gated on
	// `enabled`. Credentials are NEVER here — only a `credentialRef` name.
	backup?: {
		enabled:  bool | *false
		schedule: string | *"0 3 * * *"

		// How long one scheduled backup Job may run before Kubernetes
		// stops it and fails it with reason `DeadlineExceeded` — the
		// CronJob's `jobTemplate.spec.activeDeadlineSeconds`. Absent means
		// the platform default, six hours. It is also how long each backup
		// helper pod lives, the scheduled runner's and the CLI's alike, so
		// it bounds a single claim's dump as well as the whole run.
		//
		// The CronJob never starts a run while the previous one is still
		// going, so a run that never ends would otherwise suppress every
		// later backup without anything failing. Keep it shorter than the
		// interval between two runs of `schedule`, so a stuck run is
		// stopped before the next slot, and longer than the slowest backup
		// expected to succeed, which it would otherwise stop too. Keep it
		// below the time from a backup's start to the next check's start
		// as well: the check takes the repository's exclusive lock and a
		// backup still running then fails it (three hours under the
		// default schedules). At least ten minutes.
		activeDeadlineSeconds?: int & >=600

		// IANA timezone the two schedules are interpreted in, written to
		// `CronJob.spec.timeZone` (2.22g / D2). Absent means the CronJob
		// runs in the kube-controller-manager's zone, which is what every
		// cluster did before this field existed and is exactly the trap:
		// an operator writing `0 3 * * *` means three in the morning THEIR
		// time and has no way to learn what the three means.
		//
		// The CLI composes it from the machine running the command, so it
		// is not the operator's problem; a hand-written PlatformStack may
		// set it directly. Left optional rather than defaulted, because a
		// default here would be a guess about somebody's location.
		timeZone?: string
		bucket:    string

		// Human name this cluster's snapshots are listed under in the
		// repository — the restic `--host` on every snapshot of every
		// run. Absent keeps the fixed `apprafter-backup` host every
		// cluster used before this field existed.
		//
		// Two clusters can legitimately share one repository (the
		// "move to a bigger machine" runbook has both alive at once),
		// and a listing where every row reads `apprafter-backup`
		// cannot be read. This is the label that fixes that.
		//
		// It is NOT an identity. It lives in `spec.backup`, so a
		// restore replays it and a clone inherits the source's name.
		// Snapshots are attributed by the cluster's own `kube-system`
		// namespace UID, which leads the restic tag and which a clone
		// cannot inherit; the restore summary says so when it happens.
		clusterName?: string
		credentialRef: {name: string}
		stagingMode:       "monolithic" | "sequential" | *"monolithic"
		stagingSizeLimit?: string
		retention?: {
			keepDaily?:   int & >0
			keepWeekly?:  int & >0
			keepMonthly?: int & >0

			// Who prunes the repository. Absent means the platform
			// default, `check`; set, it is kept across upgrades.
			//
			// - `check`: the weekly check Job, after a check that
			//   passed, as far as the cluster's S3 key may delete. The
			//   scoped key ADR 0050 recommends may not, and then nothing
			//   is deleted and the `BackupRetention` condition says
			//   retention is not enforced. A check that fails never
			//   prunes.
			// - `cluster`: the backup Job, after every backup. Needs a
			//   key that may delete; a prune that fails fails the backup.
			// - `operator`: nothing in the cluster prunes. Retention is
			//   `apprafter backup prune`, run with full credentials.
			//
			// Optional rather than defaulted, so that a CR which sets
			// only a keep count is valid and an absent value stays
			// distinguishable from an explicit `operator`.
			enforce?: "check" | "cluster" | "operator"
		}
		checkSchedule: string | *"0 6 * * 0"

		// `activeDeadlineSeconds` for the weekly integrity check Job, with
		// the same rule against `checkSchedule`. Absent means six hours.
		// A running check holds the repository's exclusive lock, which
		// fails any backup that starts meanwhile — so keep it below the
		// time from the check's start to the next backup's start
		// (twenty-one hours under the default schedules), and raise it for
		// a long full-read check (`checkReadData`) deliberately, not by
		// removing it.
		checkActiveDeadlineSeconds?: int & >=600

		// Deep verify: re-download and re-hash EVERY pack on each weekly
		// check. Complete, and proportionally expensive — a full repo's
		// worth of egress every week. Overrides `checkReadDataSubset`
		// when both are set, so an operator who asks for the whole thing
		// gets the whole thing.
		checkReadData: bool | *false

		// Deep verify a RANDOM SUBSET each week: `"10%"`, `"2.5%"`,
		// `"n/t"` for a fixed part, or a byte size (`"500M"`) — restic's
		// own `--read-data-subset` grammar.
		//
		// The platform default is 10%, which is not a compromise so much
		// as an admission: a structural check never reads a byte of the
		// data it certifies, so bit-rot is invisible to it forever, while
		// a full read every week bills a repository-sized egress for a
		// problem that is rare. Ten percent finds a rotted pack in five
		// weeks on average and covers the repository in ten.
		//
		// Empty string means "structure only" — the pre-2.67 behaviour.
		checkReadDataSubset?: string

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
