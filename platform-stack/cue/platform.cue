// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Package platformstack is the CUE source of truth for the
// AppRafter platform-stack umbrella Helm chart.
//
// Per ADR 0028, the only artifacts under version control here are
// CUE files. The rendered Helm chart (`Chart.yaml`,
// `values.yaml`, `templates/applications.yaml`,
// `values.schema.json`, `compatibility.yaml`) lives in `dist/`
// (gitignored) and is published to OCI on tag.
//
// See `platform-stack/README.md` for the contribution model.
package platformstack

// #Version is the public version of the platform-stack umbrella
// chart. The first published version is 0.1.0 — minor tracks
// the AppRafter monorepo phase (Phase 1.5 → chart 0.1.x;
// chart MINOR bumps to 0.2.0 alongside the `v0.2.0-services`
// milestone when Phase 2 services land). Chart patch versions
// are independent of the monorepo patch stream (`v0.1.x`); the
// two share only MINOR/MAJOR semantics.
//
// Semver semantics:
//
// - MAJOR — incompatible change to the chart values shape, the
//   component-set contract, or the PlatformStack CRD payload it
//   produces. Operators must read `compatibility.yaml` before
//   upgrading.
// - MINOR — additive component changes (new component, new
//   optional tier overlay, new optional value).
// - PATCH — bug fixes within a component, version bumps to
//   curated dependencies that don't affect the chart shape.
#Version: string & =~"^[0-9]+\\.[0-9]+\\.[0-9]+(-[0-9A-Za-z.-]+)?$"

// #Channel is the release channel a published version may flow
// through. `stable` is the default for end-user installs;
// `edge` is for development / pre-release pinning. The channel
// is metadata only — actual version selection happens via OCI
// tag resolution.
#Channel: "stable" | "edge"

// #Tier is the AppRafter deployment tier (solo / team / prod /
// regulated). The same chart renders different
// `values.components` subsets per tier through the `cue/tier_*.cue`
// overlays.
#Tier: 1 | 2 | 3 | 4

// #ComponentSource describes where Argo CD pulls a component's
// manifests from. Three shapes are supported today:
//
// 1. Helm chart from an HTTPS repo (`repoURL` ends in a Helm
//    repository index URL, `chart` names the chart in it).
// 2. Helm chart from OCI (`repoURL` starts with `oci://`).
// 3. Plain Kubernetes manifests under a Git repo path
//    (`repoURL` is a Git remote, `path` names the directory).
//
// The `templates/applications.yaml` chart template picks the
// right Argo CD spec.source shape based on which fields are
// set; CUE validates here that the combination is consistent.
#ComponentSource: {
	// HTTPS Helm repo or OCI registry URL.
	repoURL: string
	// Chart name for Helm sources, path under repo for Git
	// sources. Mutually informative with `repoURL`'s scheme.
	chart?: string
	path?:  string
}

// #Component is one entry in the umbrella chart's
// `values.components` map. Each component renders to exactly
// one Argo CD `Application` resource.
//
// `enabled` lets a tier overlay turn a component off without
// removing the component definition from the source (which
// would force a chart-shape change).
#Component: {
	// Component identifier. Surfaces as the Argo CD
	// `metadata.name` of the rendered Application. DNS-1123
	// constrained because Argo CD enforces it server-side.
	name: string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$"

	// True iff the component should be installed in the
	// current tier overlay. False values keep the source
	// declaration but skip the rendered Application.
	enabled: bool | *true

	// Kubernetes namespace where Argo CD installs the
	// component. Argo CD creates the namespace when missing
	// (via `syncPolicy.syncOptions: ["CreateNamespace=true"]`
	// in the template).
	namespace: string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$"

	// Source repository / chart / path. See #ComponentSource.
	source: #ComponentSource

	// Chart version (Helm) or Git revision (Git). Forwarded
	// verbatim into Argo CD's `spec.source.targetRevision`.
	// Pinned to a concrete version string; the umbrella chart
	// itself is the single source of "what version of Cilium
	// do we ship" — components don't drift on `latest`.
	version: string

	// Free-form values forwarded into Argo CD's
	// `spec.source.helm.valuesObject`. Tier overlays may merge
	// on top.
	values: {...}

	// Sync-policy knobs. Defaults below match the v0.1.x
	// `cluster-bootstrap` behaviour (auto-prune, self-heal,
	// auto-create namespace).
	syncPolicy: {
		automated: {
			prune:    bool | *true
			selfHeal: bool | *true
		}
		syncOptions: [...string] | *["CreateNamespace=true", "ServerSideApply=true"]
	}

	// Argo CD AppProject this component's Application belongs
	// to. Defaults to `"platform"` per Track B.1.79a — all
	// chart-managed components are platform-internal. Tier
	// overlays and ServiceProvider charts may override to
	// `"platform-providers"` (for example, when Phase 2 CNPG /
	// Dragonfly / NATS components land in the umbrella). User-
	// app Applications go through `apprafter app add` and
	// get `project: "apps"` directly, not via #Component.
	//
	// AppProjects (`platform`, `platform-providers`, `apps`,
	// + legacy `default`) are declared in
	// `_loaderValues.argocd.values.configs.projects`, which
	// guarantees their creation by the initial Argo CD install
	// before the first component Application
	// syncs.
	project: string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$" | *"platform"

	// Another component whose `overrides.<name>.enabled` ALSO
	// governs this one.
	//
	// The template resolves an override by component NAME
	// (`index $overrides $name`), so a component meant to be
	// switched on together with a different one had no way to say
	// so — and `nack` was written on the assumption that it did.
	// `component_nack.cue` stated outright that
	// `PlatformStack.spec.overrides.nats.enabled` was "one
	// override covers both components; there is no separate
	// `overrides.nack`", the provisioner's `ensure_nats_component
	// _enabled` patches exactly that one key, and the template
	// looked up `overrides.nack`, found nothing, and kept nack's
	// literal `enabled: false`. So the `jetstream.nats.io` CRDs
	// were never installed on any cluster, and every jetstream
	// claim sat at `AwaitingNackCrds` — a reason whose own message
	// says "waiting … to be Established", forever.
	//
	// Declared here rather than special-cased in the template
	// because a name-specific branch in a generic loop is a second
	// invisible contract, and this one already cost a subphase.
	// It also fixes the HUMAN path, not only the provisioner's: an
	// operator who writes `overrides.nats.enabled: true` by hand —
	// which is what every comment and ADR 0061 §1 tell them to do
	// — now gets nack too.
	//
	// Precedence, narrowest first: this component's OWN
	// `overrides.<self>.enabled` wins if present, then the
	// `enabledFrom` component's, then the literal below. So an
	// operator can still pin one of a pair independently.
	enabledFrom?: string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$"

	// Argo CD sync-wave for the rendered Application. Argo CD
	// sorts Applications by `argocd.argoproj.io/sync-wave`
	// annotation ascending; a wave starts only after the
	// previous wave's Applications report `Sync=Synced`.
	// Practical use today:
	//
	//   - cilium    → -20  (CNI, prerequisite for everything)
	//   - argocd    → -15  (self-management adopt)
	//   - cert-manager → -10  (CRDs + webhook live before
	//                          any cert-manager.io/v1 resource
	//                          is applied)
	//   - default    → 0    (operator, admission-webhook,
	//                        network-policies, backstage)
	//
	// Without this, the admission-webhook chart's `Certificate`
	// resource is applied before cert-manager's validating
	// webhook service has endpoints, and the request fails
	// with `failed calling webhook "webhook.cert-manager.io":
	// no endpoints available`.
	syncWave: int | *0

	// Optional `spec.ignoreDifferences` — list of fields Argo CD
	// should exclude from drift comparison. Practical use today
	// is muting Kubernetes-version-skew fields the Argo CD
	// `2.13.1` schema does not yet know about (e.g.
	// `Deployment.status.terminatingReplicas` added in
	// Kubernetes 1.31 and surfaced by k3s v1.35; Argo CD
	// reports `field not declared in schema` when the live
	// object carries it but the structured-merge differ doesn't
	// recognise it). Per-component because the noisy fields
	// differ by component shape — operator's Deployment doesn't
	// expose the same fields as Cilium's DaemonSet.
	//
	// Empty list = no ignored differences (the common case;
	// most components don't need this).
	ignoreDifferences: [...{
		group: string
		kind:  string
		jsonPointers?: [...string]
		jqPathExpressions?: [...string]
	}] | *[]
}

// #ComponentSet is the umbrella chart's `values.components`
// map. Plain `[string]: #Component` pattern — each component's
// `name` field is set explicitly in `cue/component_<name>.cue`
// rather than via an autobinding `[NAME=string]: #Component & {
// name: NAME }` form. The autobinding form was rejected on the
// pre-1.66 design walk because it re-applied `#Component` to
// every map entry during the per-tier overlay unification and
// stripped concrete `namespace` / `version` fields contributed
// by the per-component declarations.
#ComponentSet: {
	[string]: #Component
}

// #ComponentOverride is one entry in the umbrella chart's
// optional `values.overrides` map. PlatformController writes
// this block onto the parent platform Application's
// `helm.valuesObject` per the B.1.73 design — at chart render
// time the Helm template merges these onto the per-component
// `_components` projection (override wins on `pin`, deep-merges
// on `values`, replaces on `enabled`).
//
// The chart's per-component pin (`_components.<name>.version`)
// is the "curated bundle" version; an operator wanting a
// component to differ from the bundle declares the override
// here via PlatformStack.spec.overrides.<name>.
#ComponentOverride: {
	// Force this component to a specific upstream chart version,
	// bypassing the umbrella chart's curated pin. Used for
	// security backports out-of-band of the umbrella release.
	pin?: string

	// Component-specific values merged onto `_components.<name>.values`
	// at template-render time (override wins on collisions). Free-
	// form per component; the chart does not validate the shape
	// (mirror'ed from the upstream component chart's own values).
	values?: {...}

	// Disable a component entirely. Per-component `enabled`
	// inside `_components` is the chart-side default; the
	// PlatformStack-side override here wins when set.
	enabled?: bool
}

// #PlatformValues is the full shape of the umbrella chart's
// rendered `values.yaml`. Tier overlays project into this
// shape; the chart template iterates `components`.
#PlatformValues: {
	// Chart version emitted into `Chart.yaml` and any rendered
	// labels.
	version: #Version

	// Tier this rendering targets. Used by the template only
	// for labelling — actual component selection happens via
	// the overlay setting `enabled: false` per component.
	tier: #Tier

	// Channel metadata. Defaults to `stable`; `edge` overlays
	// may override.
	channel: #Channel | *"stable"

	// All declared components. Tier overlays may switch
	// individual components off (`enabled: false`) but cannot
	// remove the declaration — that's a chart-shape change.
	components: #ComponentSet

	// Optional per-component overrides — PlatformController
	// writes this block at runtime via SSA on the parent
	// platform Application. The chart's `_applicationsTemplate`
	// reads `(index .Values.overrides $name)` at install time
	// and threads `pin` / `values` / `enabled` into the rendered
	// child Application.
	//
	// Empty by default — the chart's curated bundle stands on
	// its own without any override block.
	overrides?: [string]: #ComponentOverride

	// Argo CD `AppProject` definitions shipped as standalone
	// umbrella manifests at sync-wave -30 (walk-fix #2 post-
	// B.1.79a, chart 0.1.41). Iterated by the chart's
	// `templates/appprojects.yaml` template into one
	// `kind: AppProject` per entry.
	//
	// **Why both here AND in `_loaderValues.argocd.values.
	// configs.projects`?** The argocd subchart's
	// `configs.projects` mechanism renders AppProjects ONLY
	// when the Argo CD chart syncs, which happens at sync-wave
	// -15. Child Applications at sync-wave 0 (admission-webhook,
	// operator, etc.) reference these projects but are sometimes
	// applied before wave -15 finishes when Argo CD does NOT
	// strictly serialise inter-Application sync-waves on the
	// app-of-applications pattern. Result: walk-found bug —
	// `Unable to refresh admission-webhook: app is not allowed
	// in project "platform", or the project does not exist`.
	//
	// Fix: the umbrella chart itself emits AppProject CRs at
	// sync-wave -30 (earliest possible — before Cilium at -20).
	// configs.projects in argocd subchart stays for initial
	// loader install (when there's no umbrella yet). Once the
	// umbrella adopts on first sync, the umbrella-managed
	// AppProjects take ownership — Argo CD's reconciler treats
	// the two as the same logical resource (same group/kind/
	// name/namespace), so this is byte-equivalent on steady
	// state, deterministically-ordered on first sync.
	appProjects: [string]: #AppProjectSpec

	// Plain Kubernetes namespaces shipped as standalone umbrella
	// manifests at sync-wave -30, same as `appProjects` — earliest
	// possible, before Cilium. Iterated by
	// `templates/namespaces.yaml` into one `kind: Namespace` per
	// entry. Unlike `appProjects`, these are not consumed by any
	// component's own `enabled` gate — a namespace shipped here
	// exists regardless of whether the component that will use it is
	// currently on. See `namespaces.cue` for why (`nats-system` /
	// ADR 0061 §1, §2.1: a lazily-enabled component's own
	// `CreateNamespace=true` cannot stand up a namespace something
	// else needs to write into BEFORE that component is ever
	// enabled).
	namespaces: [...string]

	// ServiceProvider CRs seeded by the umbrella so a fresh
	// cluster has at least the launch-default backends declared.
	// Iterated by `templates/serviceproviders.yaml` into one
	// `kind: ServiceProvider` per entry. Empty by default; tiers
	// set `serviceProviders: _serviceProviders`.
	serviceProviders: [string]: #ServiceProviderSeed

	// Public-ingress Gateway config (1.83a). Self-defaults to an
	// empty `allowedDomains` list so the rendered `values.yaml`
	// always carries a concrete `gateway: {allowedDomains: []}` —
	// the chart's `{{- if .Values.gateway.allowedDomains }}` guard
	// nil-panics if `.Values.gateway` is absent.
	gateway: #GatewayValues

	// Off-site scheduled-backup config (2.6d-4). Self-defaults to
	// `enabled: false` so the rendered `values.yaml` always carries
	// a concrete `backup: {enabled: false, image: …}` — the chart's
	// `{{- if .Values.backup.enabled }}` guard nil-panics if
	// `.Values.backup` is absent. The operator's PlatformController
	// projects `PlatformStack.spec.backup` onto this key
	// (`values["backup"] = serialize(spec.backup)` in
	// `platform-stack::desired.rs::build()`); the chart is a
	// default-off consumer of that block.
	backup: #BackupValues
}

// `#AppProjectSpec` — the small shape `templates/appprojects.
// yaml` iterates. Mirrors Argo CD's AppProject v1alpha1 CRD
// only on the fields the umbrella actually sets.
#AppProjectSpec: {
	description: string
	sourceRepos: [...string]
	destinations: [...{
		namespace: string
		server:    string
	}]
	clusterResourceWhitelist: [...{
		group: string
		kind:  string
	}]
	namespaceResourceWhitelist: [...{
		group: string
		kind:  string
	}]
}

// One registrable zone admitted to the platform Gateway (1.83a/1.83f).
// Shape mirrors the 4.1b #DomainEntry minus the computed `wildcard`,
// with certMode pinned to the slice-only literal "imported".
#AllowedDomainEntry: {
	domain:          string // apex registrable, no "*." prefix
	certMode:        "imported"
	importedCertRef: string // kubernetes.io/tls Secret name in apprafter-system
	addedAt:         string
	addedBy:         string
}

#GatewayValues: {
	allowedDomains: [...#AllowedDomainEntry] | *[]
	// The node's public IP(s); the CLI sets these at bootstrap so the
	// LB-IPAM pool announces the node's own IP (single-node T1).
	nodePublicIP?:   string
	nodePublicIPv6?: string
}

// `#BackupValues` — the off-site scheduled-backup config the chart's
// `templates/backup.yaml` consumes (2.6d-4). The operator's
// PlatformController projects `PlatformStack.spec.backup` onto
// `.Values.backup`; this shape mirrors that typed CR block. Every
// field self-defaults so a default tier render always carries a
// concrete `backup:` object with `enabled: false` — the chart guard
// `{{- if .Values.backup.enabled }}` nil-panics on a missing
// `.Values.backup`, and a default render must emit NO backup resources.
//
// SECRETS NEVER LIVE HERE: only `credentialRef.name` names a Secret in
// `apprafter-system`; the CronJob templates mount it via
// `envFrom: secretRef` at install time. The passphrase + S3 keys are
// operator-owned (H1) and reach the cluster only through that Secret.
#BackupValues: {
	// Opt-in master switch. Default false — the whole
	// `templates/backup.yaml` block is guarded by this, so an
	// unconfigured cluster ships no ServiceAccount / RBAC / CronJobs /
	// CNP.
	enabled: bool | *false

	// Runner container image (the `apprafter-backup` binary + restic +
	// a shell). `release-backup-runner.yml` publishes this tag from
	// `cli/Cargo.toml` `workspace.package.version` on every master push
	// touching `cli/apprafter-backup/**` or `cli/backup-core/**`, and
	// THIS LINE is the only thing that decides which of those published
	// images a cluster actually runs.
	//
	// The lockstep the previous comment asserted was never enforced, and
	// it broke: the pin sat at v0.2.33 from 2026-07-17 to 2026-09-02,
	// fifteen runner images were published past it (v0.2.34..v0.2.53) and
	// eighteen chart versions shipped without moving it (0.2.43..0.2.60).
	// Across that window `cli/backup-core/src/extract.rs` gained
	// persistent-Redis extraction (T12, 5cec8c6), so the scheduled CronJob
	// kept running a binary that could not do what the release notes said
	// it did — silently, because a stale image is a working image.
	//
	// `scripts/check-backup-runner-pin.sh` now fails CI when a published
	// runner is newer than this line, so the next drift is loud.
	image: string | *"ghcr.io/apprafter/apprafter-backup:v0.2.77"

	// Cron schedule for the full backup Job. Default nightly 03:00.
	schedule: string | *"0 3 * * *"

	// How long one backup Job may run before Kubernetes stops it →
	// `jobTemplate.spec.activeDeadlineSeconds`; the Job then fails with
	// reason `DeadlineExceeded`. Default six hours. The same value reaches
	// the runner as APPRAFTER_BACKUP_DEADLINE_SECONDS: it records a run
	// stopped at the deadline (lastFailure, the failure webhook) in the pod's
	// 90 s grace period, and keeps each helper pod — and so each single
	// claim's dump — alive exactly this long. It also reaches BOTH runners
	// as APPRAFTER_BACKUP_RUN_DEADLINE_SECONDS: a prune leaves a run with no
	// manifest alone until its newest snapshot is older than this (never
	// less than six hours) plus an hour, because a backup may still be
	// writing it.
	//
	// The CronJob is `concurrencyPolicy: Forbid`, so without a deadline one
	// run that never ends suppresses every later scheduled run, silently.
	// The deadline is the bound that holds whatever the run is stuck on.
	// Keep it SHORTER than the time between two scheduled runs, so a stuck
	// run is stopped before the next slot, and LONGER than the slowest
	// backup that is expected to succeed, which it would otherwise kill:
	// six hours leaves the nightly default's next slot eighteen hours clear.
	// It must ALSO stay below the time from a backup's start to the next
	// check's start: the check (and the prune after it) takes restic's
	// exclusive lock, and neither Job retries a lock. A backup holds a lock
	// only while a restic command of its own runs — not while it dumps a
	// claim — so a backup still running when the check starts costs one of
	// the two: the check fails on the lock of a backup that is uploading,
	// and a backup that is dumping fails on the check's lock at its next
	// upload. Under the default schedules that gap is three hours
	// (03:00 → Sunday 06:00), shorter than this default — a Sunday backup
	// past three hours costs that week's check or that backup; move the
	// check later if backups take that long.
	// With a schedule more frequent than the deadline, a stuck run still
	// costs the slots that fall while it is active: `Forbid` starts none of
	// them, and when the run ends (no `startingDeadlineSeconds` is set) the
	// CronJob controller starts the most recent one at once and drops the
	// earlier ones — five hourly runs lost and the sixth late under the
	// default. A run merely slower than the interval delays the slot it
	// overlaps, as it always has under `Forbid`.
	//
	// Ten minutes at least: below that, a normal run's helper-pod start
	// and repository open are at risk, and a value that small is far more
	// likely a unit mistake (minutes for seconds) than an intent.
	activeDeadlineSeconds: int & >=600 | *21600

	// Restic repository URL, e.g. `s3:https://<endpoint>/<bucket>` —
	// NO credentials (those come from `credentialRef`). Empty until the
	// operator enables backup; the guard keys off `enabled`, not this.
	bucket: string | *""

	// Human name this cluster's snapshots are listed under: the restic
	// `--host` on every snapshot, and the `clusterId` stamped into the
	// backup manifest. Empty falls back to the fixed `apprafter-backup`
	// host (and the Helm release name in the manifest) — what every
	// cluster wrote before this field existed, so upgrading the chart
	// never silently re-groups an existing repository.
	//
	// A LABEL, not an identity. A repository can be shared by two
	// clusters, and a listing where every row reads `apprafter-backup`
	// cannot be read — that is what this fixes. Attribution is by the
	// cluster's own `kube-system` namespace UID, which the runner reads
	// at run time and puts at the head of the restic tag; a restored
	// clone inherits this name but never that UID.
	clusterName: string | *""

	// Secret in `apprafter-system` carrying `RESTIC_PASSWORD` + `AWS_*`
	// (+ endpoint/region). Consumed via `envFrom: secretRef`.
	credentialRef: {
		name: string | *""
	}

	// Staging mode: `monolithic` (one snapshot/run, default) or
	// `sequential` (per-claim snapshot-set). Forwarded to the runner
	// as `APPRAFTER_BACKUP_STAGING_MODE`.
	stagingMode: "monolithic" | "sequential" | *"monolithic"

	// `emptyDir.sizeLimit` for the staging volume of both Jobs: the
	// backup's dumps, and restic's cache and temporary files in each.
	// The runner stops a run whose volume outgrows it, and the Job fails
	// at once (a podFailurePolicy on the runner's exit code 3) instead of
	// retrying into the same limit; the error suggests raising this or
	// switching to `sequential`.
	stagingSizeLimit: string | *"10Gi"

	// Retention policy: who prunes, and what is kept. `keep*` are
	// optional — the runner defaults to 7/4/6 when unset, so the chart
	// only threads them through when explicitly configured. `enforce`
	// reaches BOTH CronJobs as APPRAFTER_BACKUP_ENFORCE:
	//
	// - `check` (default, WI-389): the weekly check Job runs the
	//   run-aware prune after a check that PASSED, as far as the
	//   cluster's key may delete. Under the scoped key ADR 0050
	//   recommends the first delete is refused, nothing is deleted, and
	//   the runner records `not-permitted` — the operator's
	//   BackupRetention condition and `apprafter backup status` then say
	//   retention is not enforced, with the repository's growth. A check
	//   that fails never prunes.
	// - `cluster`: the backup Job prunes after every backup (a prune
	//   that fails fails the backup); needs full credentials in the
	//   cluster Secret.
	// - `operator`: nothing in the cluster prunes; retention is the
	//   operator-side `apprafter backup prune` with full creds.
	//
	// The default was `operator` until WI-389. A PlatformStack that set
	// `operator` explicitly keeps it; one that never set it gets
	// `check`, and a cluster whose key MAY delete starts removing
	// snapshots beyond the keep policy at its next weekly check.
	retention: {
		keepDaily?:   int
		keepWeekly?:  int
		keepMonthly?: int
		enforce:      "check" | "cluster" | "operator" | *"check"
	}

	// Cron schedule for the weekly check Job — `restic check`, then,
	// under `retention.enforce: check`, the prune. Default Sunday
	// 06:00 (staggered clear of the daily backup). Empty omits the
	// CronJob, and with it the in-cluster prune of `enforce: check`.
	checkSchedule: string | *"0 6 * * 0"

	// `activeDeadlineSeconds` for the weekly check Job — the check and,
	// under `enforce: check`, the prune after it — with the same
	// reasoning and the same floor. It must ALSO stay below the time from
	// the check's start to the next backup's start: a check still holding
	// restic's exclusive lock fails that backup (no --retry-lock). Default
	// six hours: under the default schedules a stuck check is stopped by
	// Sunday noon, well inside the twenty-one hours to Monday's 03:00
	// backup. Raise it for `checkReadData: true` on a repository that takes
	// longer than that to download — and keep it under that gap.
	checkActiveDeadlineSeconds: int & >=600 | *21600

	// IANA timezone both schedules run in → `CronJob.spec.timeZone`
	// (2.22g / D2). Empty = omit the field, which means the CronJob runs
	// in the kube-controller-manager's zone — every cluster's behaviour
	// before this existed, and exactly the trap: an operator writing
	// `0 3 * * *` means three in the morning THEIR time.
	timeZone: string | *""

	// Opt-in FULL re-download `restic check --read-data`: every pack,
	// every week. Wins over `checkReadDataSubset` when both are set —
	// an operator who asks for the whole repository gets it.
	checkReadData: bool | *false

	// Deep-verify a random SUBSET on each weekly check
	// (`--read-data-subset`): `"10%"`, `"n/t"`, or a byte size.
	//
	// Default 10%, and the reasoning is worth stating because the old
	// default was "nothing". A structural check never reads a byte of
	// the data it certifies, so bit-rot stays invisible to it forever;
	// a full read every week bills a repository-sized egress against a
	// rare fault. Ten percent finds a rotted pack in five weeks on
	// average, covers the repository in ten, and costs a tenth of the
	// bandwidth. Set to `""` for the pre-0.2.68 structure-only check.
	checkReadDataSubset: string | *"10%"

	// Optional URL the runner POSTs a JSON failure report to. When set,
	// the CNP also allows egress to its host (folded into world:443).
	failureWebhook?: string
}

// `#ServiceProviderSeed` — the small shape
// `templates/serviceproviders.yaml` iterates. The umbrella seeds
// one `kind: ServiceProvider` (apprafter.io/v1alpha1) per entry so
// the 2.3 scheduler has a provider to match claims against on a
// fresh cluster. Namespaced into `apprafter-system` (the operator's
// namespace, where platform providers live). `labels` become the
// CR's `metadata.labels` — the scheduler matches a claim's selector
// as a subset of these, so the launch pg provider carries
// `tier: integrated`. `config` is opaque (the CNPG cluster
// coordinates the 2.4c provisioner reads); `type` mirrors the
// closed `#PlatformServiceType` enum but is kept a plain string
// here because the umbrella does not import the schemas module.
#ServiceProviderSeed: {
	// Namespace the CR lands in. Defaults to the operator's
	// namespace; the scheduler lists providers cluster-wide.
	namespace: string & =~"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$" | *"apprafter-system"

	// `metadata.labels` of the rendered CR. The scheduler matches
	// `claim.spec.selector` ⊆ these. Must be non-empty: beyond the
	// scheduler needing `tier: integrated` to match, the template
	// appends static `apprafter.io/*` labels after
	// `{{ toYaml $sp.labels }}`, so an empty map would emit a flow
	// `{}` followed by block keys — invalid YAML.
	labels: [string]: string

	// `spec.type` — a built-in platform-service type.
	type: string

	// `spec.backend` — implementation identifier (e.g.
	// "cloudnative-pg").
	backend: string

	// `spec.config` — backend-specific, opaque to the umbrella.
	config: {...}

	// Argo CD sync-wave. Positive so the CR applies after the
	// apprafter-operator child Application (wave 0) has installed
	// the ServiceProvider CRD; paired with
	// SkipDryRunOnMissingResource in the template for CRD-race
	// resilience.
	syncWave: int | *5
}

// `currentVersion` is THE single source of truth for the chart
// version being built. Every other place that needs to mention
// the version must reference this:
//
//   - `tier_solo.cue` + `tier_team.cue` use `version:
//     currentVersion` instead of a string literal.
//   - The renderer (`render_tool.cue`) computes its `dist/`
//     subdir from `currentVersion`.
//   - The `platform-stack-publish` workflow reads the value
//     via `cue export -e currentVersion`.
//
// Bumping the chart version is a one-line edit here PLUS adding
// the matching `compatibility[currentVersion]` entry below.
// CUE enforces the pairing (see the `compatibility:
// (currentVersion): #VersionRecord` line in `compatibility.cue`)
// — a bump that forgets the compatibility entry fails `cue vet
// -c` with an "incomplete value" error pointing at the missing
// fields, before the publish workflow ever runs.
currentVersion: #Version & "0.2.80"

// `_components` is the package-level base set, populated by
// every `cue/component_<name>.cue` file declaring
// `_components: <name>: #Component & { … }`. The leading
// underscore makes the field hidden — it doesn't appear in
// rendered output, only feeds the tier overlays via
// `components: _components & { <overlay> }`.
//
// Pattern-constraint typing was tried (`_components:
// #ComponentSet`) on the pre-1.66 design walk but was reverted
// — re-applying `#Component`'s pattern at every per-tier
// unification stripped concrete `namespace` / `version`
// fields. Each entry's `#Component` conformance is enforced
// locally at the declaration site in `cue/component_*.cue`
// instead.
_components: {}
