// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// NATS server — the JetStream backend `needs.jetstream` claims resolve
// to (2.5c / ADR 0061). The ONLY component in this file that ships
// `enabled: false`: `nats-system` (namespaces.cue) exists
// unconditionally, but the server itself stays off until the
// resourceclaim-provisioner merge-patches
// `PlatformStack.spec.overrides.nats.enabled = true` on the first
// matched jetstream claim (ADR 0061 §1 — lazy enablement, "an
// always-on ~128Mi reservation on the Tier-1 node for a feature most
// clusters do not use regresses the ADR 0053 budget" is the Alternative
// this rejected). `#Component.enabled` schema-defaults to `bool | *true`
// (platform.cue); a component wanting the OPPOSITE default cannot
// itself carry `bool | *false` — unifying two conflicting defaults is
// an AMBIGUOUS default CUE refuses to resolve to a concrete value (the
// render step needs one to marshal `values.yaml`), confirmed by
// scratch-testing `cue export` against exactly this shape before
// writing it here. A plain concrete `enabled: false` unifies cleanly —
// concrete values simply select which branch of the schema's own
// default disjunction applies, no conflict. The override mechanism
// (`values.overrides.nats.enabled`, threaded by
// `_applicationsTemplate`) replaces this value wholesale regardless of
// whether the base was a literal or itself defaulted, so this choice
// changes nothing about how the provisioner turns the component on.
//
// Official chart `nats` 2.14.6 from
// https://nats-io.github.io/k8s/helm/charts/ — its OWN default image is
// `nats:2.14.6-alpine` (confirmed by `helm template`), which this
// values block deliberately does NOT use; see `container.image` below.
_components: "nats": #Component & {
	name:      "nats"
	enabled:   false
	namespace: "nats-system"
	project:   "platform-providers"
	source: {
		repoURL: "https://nats-io.github.io/k8s/helm/charts/"
		chart:   "nats"
	}
	// Discovered via `helm show chart nats/nats` (2026-09-11):
	// appVersion 2.14.6. The CHART version and the SERVER image below
	// are independent axes — pinning the chart is "which template/
	// values shape do we render", pinning the image is "which server
	// binary actually runs" (see `container.image`'s own comment).
	version: "2.14.6"
	values: {
		container: {
			// PINNED to v2.14.3, NOT the chart's own default
			// (2.14.6-alpine) — ADR 0061 §4.2/§4.4: the deny vector's
			// position-pattern completeness, the v1/v2 ack-subject
			// forms, and `feature_flags.js_ack_fc_v2`'s acceptance are
			// ALL facts measured against nats-server v2.14.3
			// specifically, the same lesson ADR 0042 §10 learned for
			// Dragonfly ("the expensive way" — an unpinned image drifts
			// the running server out from under a security model that
			// was verified against one exact version). Confirmed this
			// tag is real and IS v2.14.3 by pulling it and reading the
			// server's own startup banner (`Version: 2.14.3`).
			//
			// This is the canonical Tier-1 pin — `jetstream-integrated`
			// (service_providers.cue) REFERENCES it
			// (`_components.nats.values.container.image`) rather than
			// repeating it, so a later re-pin (a NATS security patch,
			// say) is a one-line edit that the seed picks up
			// automatically; ADR 0061 §4.4's own verification (the
			// provisioner asserting the observed ack form against the
			// configured one) reads the seed for what it should expect.
			image: {
				repository: "nats"
				tag:        "2.14.3-alpine"
			}
			// Guaranteed QoS (ADR 0053 §1: NATS is a STATEFUL BACKEND,
			// not a platform-component controller — dragonfly-operator/
			// cloudnative-pg's own pods are Burstable because THEY are
			// controllers; the shared instances they create are what
			// gets Guaranteed treatment, and here the chart-deployed
			// StatefulSet pod IS that instance). requests == limits on
			// every resource, per ADR 0053's own CNPG/Dragonfly example
			// — a CPU limit is included here (unlike the Burstable
			// platform-component pattern in dragonfly-operator.cue,
			// which deliberately omits one).
			//
			// 384Mi / 100m is a considered Tier-1 seed, not a measured
			// footprint (no live NATS pod has run on this platform yet
			// to measure): baseline nats-server RSS is small, but
			// `config.jetstream.memoryStore.maxSize` below (192Mi) must
			// fit comfortably under this limit with headroom left for
			// the server's own overhead and JetStream file-store
			// buffering — see that field's own comment for why the
			// headroom itself is load-bearing, not slack.
			resources: {
				requests: {
					cpu:    "100m"
					memory: "384Mi"
				}
				limits: {
					cpu:    "100m"
					memory: "384Mi"
				}
			}
			// Mounts the provisioner-owned accounts Secret as a WHOLE
			// DIRECTORY (never `subPath` — see the reasoning on
			// `podTemplate.patch` below, which this mirrors onto the
			// `nats` container itself). `reloader.natsVolumeMountPrefixes`
			// (chart default `["/etc/"]`) mirrors this SAME mount into
			// the reloader container automatically — confirmed by
			// rendering (`helm template` with these exact values shows
			// the mount on BOTH the `nats` and `reloader` containers),
			// not trusted from the chart's own docs. No `reloader.patch`
			// needed.
			patch: [{
				op:   "add"
				path: "/volumeMounts/-"
				value: {
					name:      "accounts-secret"
					mountPath: "/etc/nats-config/accounts-secret"
					readOnly:  true
				}
			}]
		}
		podTemplate: {
			// Adds the provisioner-owned `nats-accounts` Secret as a pod
			// volume. THE MOUNT ABOVE MUST STAY A WHOLE DIRECTORY, NEVER
			// A `subPath` MOUNT OF THIS SECRET: kubelet re-syncs
			// whole-directory Secret mounts only
			// (kubernetes/kubernetes#50345) — a `subPath: accounts.conf`
			// mount would look tidier (a single file at
			// `/etc/nats-config/accounts.conf`) and would SILENTLY never
			// receive updates when the provisioner rewrites the Secret,
			// defeating the reloader's entire purpose without any error
			// anywhere. Nothing in the chart or upstream docs explains
			// this; it is recorded here (and in ADR 0061 §1) because
			// there is nowhere else it would be.
			patch: [{
				op:   "add"
				path: "/spec/volumes/-"
				value: {
					name: "accounts-secret"
					secret: secretName: "nats-accounts"
				}
			}]
		}
		config: {
			jetstream: {
				enabled: true
				fileStore: pvc: {
					// Tier-1 seed. `storageClassName` matches
					// `disk-local`'s own choice (service_providers.cue)
					// — `local-path`, the class that ships on k3s/kind,
					// so JetStream needs no extra platform-stack
					// component at launch. Sized well above the
					// account-quota ceiling `jetstream-integrated`
					// defines (service_providers.cue) — the PVC is the
					// PHYSICAL disk every namespace's `max_file`
					// promise is carved out of, so it must exceed the
					// ceiling with room for JetStream's own metadata
					// overhead, not merely equal it.
					size:             "5Gi"
					storageClassName: "local-path"
					// Renders the volumeClaimTemplate exactly as the
					// apiserver stores it. The chart leaves out four
					// fields the apiserver adds: apiVersion, kind,
					// spec.volumeMode and status.phase. Argo CD 2.13's
					// server-side-apply diff replaces the whole template
					// list (it is atomic) with the rendered one. It
					// restores the two defaulted fields but not apiVersion
					// or kind, and it adds `metadata.creationTimestamp:
					// null`. So the `nats` Application sat OutOfSync on
					// every jetstream cluster, and self-heal re-synced it
					// every ~3 minutes, forever. When the rendered
					// template equals the stored one, the diff predicts no
					// change at all.
					//
					// This ignores nothing. A real template change, such
					// as `size`, still shows as OutOfSync. An
					// ignoreDifferences fix would have to hide all four
					// fields, volumeMode included; hiding only apiVersion
					// and kind leaves the creationTimestamp diff (measured
					// on kind, Kubernetes 1.36.4). `merge` is the chart's
					// own hook for this document.
					merge: {
						apiVersion: "v1"
						kind:       "PersistentVolumeClaim"
						spec: volumeMode: "Filesystem"
						status: phase:    "Pending"
					}
				}
				// Server-wide memory-store ceiling — REQUIRED, not
				// optional, the moment any account requests a nonzero
				// `max_mem`. Measured (podman, nats-server 2.14.3):
				// leaving this at the chart's own default
				// (`memoryStore.enabled: false`, which renders
				// `"max_memory_store": 0` in nats.conf) makes the
				// server FATAL-EXIT at JetStream startup —
				// `[FTL] Can't start JetStream: Error enabling
				// jetstream on configured accounts: insufficient
				// memory resources available (10028)` — the instant
				// the accounts file (nats_accounts::render_accounts_file,
				// part 1) grants ANY account a non-zero `max_mem`, which
				// it always does now (ACCOUNT_MAX_MEM_FLOOR_BYTES is
				// 64Mi, never 0). This is a WHOLE-SERVER failure, not a
				// per-stream one — worse than the opaque `10028` the
				// account-level fix (H3) was written to avoid, and it
				// would have shipped invisibly: nothing in Task 2's own
				// brief mentioned `memoryStore`, only `fileStore.pvc`.
				//
				// 192Mi leaves ~192Mi of the 384Mi pod limit above for
				// the server's own baseline + JetStream file-store
				// buffering, and covers roughly three namespaces each
				// sitting at the account-level 64Mi floor concurrently.
				// A FOURTH concurrent namespace at the floor (or fewer
				// namespaces asking for more) exceeds it — the server
				// would then fail to (re)start on the next accounts-file
				// write, cluster-wide, for every jetstream namespace at
				// once. Nothing on the Rust side (part 1's
				// `account_max_mem_bytes`) currently clamps the SUM
				// against this ceiling the way ADR 0061 §6 clamps the
				// FILE quota sum against its own ceiling — flagged as a
				// follow-up, not fixed here (this task is CUE/chart
				// values only).
				memoryStore: {
					enabled: true
					maxSize: "192Mi"
				}
			}
			// TOP-LEVEL `config.merge` (a sibling of `config.jetstream`,
			// NOT `config.jetstream.merge`) — both entries below render
			// as top-level `nats.conf` keys, confirmed by rendering the
			// whole ConfigMap, not assumed from the chart's own comments:
			//
			//   {
			//     include ./accounts-secret/accounts.conf;
			//     "feature_flags": { "js_ack_fc_v2": true },
			//     "jetstream": { ... },
			//     ...
			//   }
			//
			// which matches ADR 0061 §4.4 exactly: `js_ack_fc_v2` is
			// "not a top-level key and not a key inside `jetstream {}`"
			// — both rejected with `unknown field` — "but ... the server
			// accepts it inside a top-level `feature_flags {}` block."
			merge: {
				// A key ending in `$include` is chart-recognised syntax
				// (`nats.reloaderConfig` helper) that emits BOTH the
				// `include` directive in `nats.conf` AND an extra
				// `-config <path>` entry in the reloader's own argv —
				// the reloader is given explicit `-config` flags, never
				// a directory to watch, and there is no
				// `reloader.extraArgs` to add one by hand. Confirmed by
				// rendering: the reloader container's `args` carry BOTH
				// `-config /etc/nats-config/nats.conf` (the chart's own)
				// AND `-config
				// /etc/nats-config/accounts-secret/accounts.conf` (this
				// one) side by side.
				"accounts$include": "./accounts-secret/accounts.conf"
				// `feature_flags` is UNDOCUMENTED — found only by
				// negative control (ADR 0061 §4.4): a bare top-level
				// `js_ack_fc_v2` key and a `features` / `server_features`
				// block are BOTH rejected with `unknown field`, and only
				// nesting the flag inside a top-level `feature_flags {}`
				// block is accepted by nats-server 2.14.3. That negative
				// control is what makes this a positive identification
				// rather than a silent no-op — a future server release
				// could rename or drop the setting with no warning,
				// which is why ADR 0061 §4.4 also has the provisioner
				// assert the OBSERVED ack form against this CONFIGURED
				// one at startup, rather than trusting the flag forever.
				// `true`, not `false`: the value doesn't change what the
				// deny vector denies (both ack forms are always emitted
				// — nats_accounts::total_denial_patterns), only what
				// CLIENTS parse from the ack subject's tokens: v2 shifts
				// them by two, so this is worth taking now while nothing
				// depends on v1 (`@onebun/nats` already parses v2).
				feature_flags: js_ack_fc_v2: true
			}
		}
	}

	// The chart also ships a Deployment, `nats-box`, and its status
	// carries `terminatingReplicas` too (measured on Kubernetes 1.36.4),
	// a field Argo CD 2.13.1's schema lacks. A comparison that diffs it
	// fails with `field not declared in schema` and the Application
	// shows sync Unknown. The self-heal loop the volumeClaimTemplate
	// diff above caused hid this, because each re-sync refreshed the
	// comparison before it expired. With only that diff fixed, the
	// Application went Unknown at the next 3-minute comparison refresh.
	ignoreDifferences: [{
		group: "apps"
		kind:  "StatefulSet"
		jsonPointers: ["/status/terminatingReplicas"]
	}, {
		group: "apps"
		kind:  "Deployment"
		jsonPointers: ["/status/terminatingReplicas"]
	}]
}
