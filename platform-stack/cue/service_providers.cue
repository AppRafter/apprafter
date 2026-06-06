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
}
