// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package v1alpha1

// SharedDatabase is one database several Applications may bind (2.29 /
// ADR 0066). Created explicitly, with its own lifecycle, independent of any
// Application — the `SharedVolume` shape (ADR 0049 §1), applied to a
// database.
//
// It departs from `SharedVolume` in one visible way: **its status carries no
// `connectionSecretRef`**. A shared volume publishes a `pvcRef` because every
// consumer mounts the identical object; here every consumer gets its OWN
// credential (ADR 0066 §3), so a single shared Secret in the status would be
// exactly the thing this CRD exists to avoid. The status reports what was
// provisioned, not how to reach it — that is each binding claim's own
// connection Secret.
//
// Bound from an Application through `needs.<type>.ref`; `rm` is refused while
// `status.refCount > 0`.
#SharedDatabase: {
	#TypeMeta
	kind:     "SharedDatabase"
	metadata: #ObjectMeta

	spec: {
		// Which backend. Only these two are implemented; the webhook rejects
		// a `ref` naming a SharedDatabase from a need type that has none.
		type: "pg" | "redis"

		// Requested size class. Optional — tier-aware platform defaults fill
		// it, exactly as on `#ServiceNeed`.
		size?: #Size

		// Label selector matched against `ServiceProvider.metadata.labels`.
		// Optional; the controller injects `{tier: integrated}` when absent.
		selector?: [string]: string

		// redis only: route to the persistent pool instance rather than an
		// ephemeral one (ADR 0042). Rejected on `pg` by the webhook — CNPG
		// storage is not a per-database choice.
		persistent?: bool

		// pg only (2.29 / ADR 0066 §4). Extensions to create in this
		// database. Bounded by the provider seed's allow list, which is a
		// REFUSAL and not a preference: `platform-postgres` is one cluster
		// for every tenant in the Kubernetes cluster and CNPG runs
		// `CREATE EXTENSION` as superuser, so `dblink`, `file_fdw` and the
		// untrusted procedural languages would hand an application a
		// privilege its own role categorically does not have.
		extensions?: [...#PgExtension]
	}

	status?: {
		ready?: bool

		// How many Applications currently bind this database. DERIVED from
		// the live claims carrying the reference and recomputed on reconcile,
		// never incremented and decremented — that is the counter that
		// drifts, and this one gates a destructive `rm`.
		refCount?: int & >=0

		// What was provisioned. pg: the database name in the shared cluster.
		// redis: the pool instance and the `$N` every consumer is pinned to.
		database?: string
		instance?: string
		dbnum?:    int & >=0 & <1024

		conditions?: [...#SharedDatabaseCondition]
	}
}

#SharedDatabaseCondition: {
	type:               string
	status:             "True" | "False" | "Unknown"
	lastTransitionTime: string
	reason?:            string
	message?:           string
}

// #PgExtension — one PostgreSQL extension to create in a database (2.29 /
// ADR 0066 §4). Maps 1:1 onto CNPG's `Database.spec.extensions[]`.
//
// `ensure` is deliberately NOT exposed: removing an entry from the list means
// absent, and a manifest that says `ensure: absent` while still listing the
// extension is a shape with two ways to say one thing.
//
// Removal never cascades. `DROP EXTENSION` without `CASCADE` fails when
// dependent objects exist and names the dependency (measured, PostgreSQL
// 18.4); the platform surfaces that failure rather than dropping a user's
// column default because the author of a manifest edit was tidying a list.
#PgExtension: {
	name: string
	// Absent installs whatever the extension's control file calls default.
	// Whether a given version exists is a property of the operand image, and
	// is reported as a condition rather than promised.
	version?: string
	schema?:  string
}
