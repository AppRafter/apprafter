// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// `_serviceProviders` — the package-level base set of
// ServiceProvider CRs the umbrella seeds. Mirrors `_appProjects`:
// hidden (leading underscore), consumed by the tier overlays via
// `serviceProviders: _serviceProviders` and iterated by
// `templates/serviceproviders.yaml`.
//
// `pg-integrated` is the launch-default in-cluster Postgres
// backend (CloudNativePG). The 2.3 scheduler matches a pg claim's
// selector (`{tier: integrated}` by default) against the CR's
// `metadata.labels`; the 2.4c provisioner reads `spec.config` to
// lazily create + provision into the shared `platform-postgres`
// CNPG Cluster. `instances` is tier-aware — tier-1 (4 GiB node)
// runs a single Postgres instance; the tier-2 overlay bumps it to
// 3 for HA.
_serviceProviders: {
	"pg-integrated": #ServiceProviderSeed & {
		namespace: "apprafter-system"
		labels: {
			tier:     "integrated"
			location: "in-cluster"
		}
		type:    "pg"
		backend: "cloudnative-pg"
		config: {
			// Coordinates of the shared CNPG Cluster the 2.4c
			// provisioner creates lazily + owns. Not seeded here.
			// `config` is open ({...}), so 2.4c can extend this
			// without a schema change — it will likely add a
			// `storageClass` (relying on the cluster-default class is
			// a cross-tier foot-gun) and a pinned Postgres major
			// version (CNPG pins the data-plane major via
			// imageName/imageCatalogRef; an unpinned default drifts
			// it across chart bumps).
			cluster:   "platform-postgres"
			namespace: "cnpg-system"
			instances: int | *1
			storage:   string | *"10Gi"
			// Guaranteed backend resources (2.16d). The 2.4c
			// provisioner reads each field independently from
			// `/resources/*` (see resourceclaim-provisioner
			// reconcile) and emits a Guaranteed (req==limit) CNPG
			// Cluster; `sharedBuffers` must stay coherent with
			// `memory`. These T1 seeds equal the code fallbacks —
			// making them explicit lets a tier overlay raise them
			// (like the `instances: 3` HA bump) without a code change.
			resources: {
				cpu:              string | *"100m"
				memory:           string | *"256Mi"
				ephemeralStorage: string | *"1Gi"
				sharedBuffers:    string | *"32MB"
			}
		}
	}
	"redis-integrated": #ServiceProviderSeed & {
		namespace: "apprafter-system"
		labels: {
			tier:     "integrated"
			location: "in-cluster"
		}
		type:    "redis"
		backend: "dragonfly"
		config: {
			// Dragonfly pool coordinates the 2.6-3 provisioner reads.
			// `config` is open ({...}) so this needs no ServiceProvider
			// schema change. The shared instances are NOT seeded here —
			// the provisioner creates them lazily per persistence class.
			namespace: "dragonfly-system"
			// Pool-instance flags (ADR 0042 Pre-merge #1/#2 measured):
			// dbnum=1024 is the hard max and free at idle; num_shards=1
			// default on ALL tiers (per-active-DB memory = 280kB×shards),
			// operator-tunable here for throughput without a code change.
			dbnum:     int | *1024
			numShards: int | *1
			// Instances per shared pool member. MUST be >= 1 — the
			// dragonfly-operator does not default replicas, so 0 means no
			// instance pod. Tier-1 = 1; HA tiers raise it via a tier overlay.
			replicas: int | *1
			// Guaranteed backend resources (2.16d). The 2.6-3 provisioner
			// reads each field independently from `/resources/*` (see
			// resourceclaim-provisioner reconcile) and emits a Guaranteed
			// (req==limit) Dragonfly with a `--maxmemory` RSS cap below the
			// memory limit. The T1 memory seed (320Mi) sits above the
			// ADR-0042 ~287MB structural floor at the 1024-claim cap. These
			// equal the code fallbacks — explicit so a tier overlay can raise
			// them without a code change.
			resources: {
				cpu:              string | *"50m"
				memory:           string | *"320Mi"
				ephemeralStorage: string | *"1Gi"
			}
		}
	}
	"jetstream-integrated": #ServiceProviderSeed & {
		namespace: "apprafter-system"
		labels: {
			tier:     "integrated"
			location: "in-cluster"
		}
		type:    "jetstream"
		backend: "nats"
		config: {
			// The account model the resourceclaim-provisioner (part 2)
			// authenticates against lives in this namespace, not
			// `apprafter-system` — ADR 0061's own "Namespaces" section.
			// `config` is open ({...}) so part 2 can extend this
			// without a ServiceProvider schema change.
			namespace: "nats-system"

			// PINNED server image, NOT left to the chart's default —
			// ADR 0061 §4.2/§4.4: the deny vector's position-pattern
			// completeness and the v1/v2 ack-subject forms are facts
			// about ONE exact server version, the same lesson ADR 0042
			// §10 learned for Dragonfly. REFERENCES
			// `component_nats.cue`'s own pin rather than repeating
			// it — that file is the single source of truth for "which
			// server binary actually runs" (its own comment records
			// WHY the reference runs in this direction: Task 2 had to
			// stand alone and pass its own `platform-stack-check`
			// before this file existed, so the numbers were defined
			// there first and are pulled in here, not the reverse).
			serverImage: "\(_components.nats.values.container.image.repository):\(_components.nats.values.container.image.tag)"

			// `#Size` → account-quota BYTES, tier-aware (ADR 0061 §6:
			// "an enum mapped to bytes in the jetstream-integrated
			// seed, tier-aware"). Each entry is satisfiable ALONE by a
			// single claim — `ceilingBytes` below is chosen to equal
			// the largest entry, so no size a manifest can legally
			// request is unconditionally impossible; summing two or
			// more claims in the same namespace is where the ceiling
			// actually bites. You are only defining the values here;
			// part 2's Rust half applies them (this task is CUE/chart
			// values only).
			sizeBytes: {
				nano:   67108864   // 64Mi
				small:  268435456  // 256Mi
				medium: 1073741824 // 1Gi
				large:  2147483648 // 2Gi
				xlarge: 4294967296 // 4Gi
			}

			// The ceiling clamps the SUM of a namespace's live claims'
			// `sizeBytes`, NOT each term (ADR 0061 §6, explicit — "the
			// tier ceiling clamping the sum rather than each term"):
			// two "medium" claims together in one namespace exceed
			// this even though each alone would not.
			//
			// 4Gi, chosen to equal `sizeBytes.xlarge` while staying
			// comfortably under `component_nats.cue`'s own
			// `fileStore.pvc.size` (5Gi) — that PVC is the PHYSICAL
			// disk this ceiling's promise is carved out of, SHARED
			// across every namespace on the one server, so it must
			// leave headroom for JetStream's own metadata/index
			// overhead on top of whatever file data every namespace
			// holds. This is a PER-NAMESPACE ceiling, not a
			// platform-wide one — flagged, not solved, here: a Tier-1
			// cluster running two namespaces each near the ceiling
			// would jointly ask the 5Gi PVC for close to 8Gi, which it
			// does not have. Nothing enforces a PLATFORM-WIDE total
			// today; that is a follow-up for part 2 or a later CUE
			// task, the same shape as `component_nats.cue`'s own
			// flagged gap between the per-account memory floor and the
			// server-wide memory ceiling.
			ceilingBytes: 4294967296 // 4Gi
		}
	}

	"disk-local": #ServiceProviderSeed & {
		namespace: "apprafter-system"
		labels: {
			tier:     "integrated"
			location: "in-cluster"
		}
		type:    "disk"
		backend: "disk"
		config: {
			// StorageClass the 2.6b-3 `Backend::Disk` provisioner stamps
			// onto the standalone RWO PVC it creates per disk claim.
			// `config` is open ({...}) so a per-tier overlay can point
			// disk at a different class (e.g. a replicated CSI class on
			// T2+) without a ServiceProvider schema change. Tier-1 uses
			// `local-path` — the StorageClass that ships on k3s/kind, so
			// no extra platform-stack component is needed at launch.
			// `local-path` binds WaitForFirstConsumer, which is why the
			// provisioner marks the claim ready on PVC-exists (not Bound)
			// — binding waits for the rendered pod.
			storageClass: string | *"local-path"
		}
	}
	"shared-local": #ServiceProviderSeed & {
		namespace: "apprafter-system"
		labels: {
			tier:     "integrated"
			location: "in-cluster"
		}
		type:    "shared-disk"
		backend: "shared-disk"
		config: {
			// StorageClass the 2.6c SharedVolume provisioner stamps onto
			// the shared RWX PVC it creates per shared-disk claim group.
			// `config` is open ({...}) so a per-tier overlay can point
			// shared-local at a different class (e.g. an NFS-backed CSI
			// class on T2+) without a ServiceProvider schema change.
			// Tier-1 uses `local-path` — the default k3s/kind class;
			// `local-path` PVCs are RWO-only so the provisioner must
			// request RWO and co-locate consumers on the same node.
			storageClass: string | *"local-path"
		}
	}
}

// 2.3's `matches()` (operator-core::matching.rs) requires a claim's
// selector to be a SUBSET of the matched provider's `metadata.labels`.
// A bare `needs.jetstream: {}` carries no selector in the manifest; the
// 2.4d controller injects `{tier: "integrated"}` as the default
// (schemas/v1alpha1/application.cue `#ServiceNeed.selector`'s own
// comment) — the same convention `pg-integrated`/`redis-integrated`/
// `disk-local`/`shared-local` already rely on. Asserted here rather
// than eyeballed: this fails `cue vet` the moment
// `jetstream-integrated`'s labels ever stop covering that default,
// which would otherwise strand every bare `needs.jetstream: {}` in
// `AwaitingResourceClaim` with no scheduler error pointing at why —
// the scheduler reports "no provider matched", not "your seed drifted".
_assertJetstreamIntegratedMatchesDefaultSelector: true & (_serviceProviders."jetstream-integrated".labels["tier"] == "integrated")
