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
}
