// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Two applications sharing one PostgreSQL database and one Redis
// keyspace (Phase 2.29 / ADR 0066). A `cue vet` fixture for
// `#ServiceNeed.ref` / `.access` / `.extensions`, and for the rule that
// `ref` is incompatible with `size` / `persistent` / `selector`.
//
// The database itself is NOT declared here, deliberately. It is a
// `SharedDatabase` created out of band (`apprafter db create`), because
// it outlives every application bound to it — if the first application
// to mention a name created it, that application would own a database
// everybody else inherits, and deleting it would take the neighbours'
// data along. `shared-database.cue` in this directory is the CR.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

// The writer. `access` is omitted, which reads as `rw` — the manifest
// default and the wider of the two.
sharedDbWriter: v1alpha1.#Application & {
	metadata: {
		name:      "web"
		namespace: "shop"
	}
	spec: base: {
		image: "ghcr.io/example/web:latest"
		expose: port: 8080
		needs: {
			pg: ref: "orders"
			// A shared cache. Both consumers land on one `$N` and one
			// channel prefix, so they can publish to each other — which
			// a per-consumer prefix would prevent.
			redis: ref: "events"
		}
		env: {
			// The claim references resolve against THIS consumer's own
			// credential, not a shared one: same database, different
			// login, revocable per application.
			DATABASE_URL: {claim: "pg.url"}
			REDIS_URL: {claim: "redis.url"}
			REDIS_CHANNEL_PREFIX: {claim: "redis.channelPrefix"}
		}
	}
}

// The reader. Same database, read-only — an access level the platform
// enforces through group membership rather than through convention.
sharedDbReader: v1alpha1.#Application & {
	metadata: {
		name:      "reporter"
		namespace: "shop"
	}
	spec: base: {
		image: "ghcr.io/example/reporter:latest"
		needs: pg: {
			ref:    "orders"
			access: "ro"
		}
		env: DATABASE_URL: {claim: "pg.url"}
	}
}

// An application that OWNS its database rather than sharing one, with
// `pgvector` on it. Extensions belong to whoever owns the database:
// here, to this claim; on a shared database, to the `SharedDatabase`,
// because two consumers could otherwise ask for different sets of them.
ownedDbWithVector: v1alpha1.#Application & {
	metadata: {
		name:      "search"
		namespace: "shop"
	}
	spec: base: {
		image: "ghcr.io/example/search:latest"
		needs: pg: {
			size: "small"
			extensions: [
				{name: "vector"},
				{name: "pg_trgm"},
			]
		}
		env: DATABASE_URL: {claim: "pg.url"}
	}
}
