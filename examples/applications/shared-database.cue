// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// The `SharedDatabase` CRs that `shared-database-app.cue`'s applications
// bind (Phase 2.29 / ADR 0066). A `cue vet` fixture for
// `#SharedDatabase`.
//
// These are namespaced and live in the SAME namespace as their
// consumers: ADR 0066 scopes sharing to one namespace, and a
// cross-namespace reference is refused with its own message rather than
// resolved.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

// The shared PostgreSQL database. Extensions belong here rather than on
// a consumer, because two consumers of one database could otherwise ask
// for different sets and the last reconcile would win.
ordersDatabase: v1alpha1.#SharedDatabase & {
	metadata: {
		name:      "orders"
		namespace: "shop"
	}
	spec: {
		type: "pg"
		size: "small"
		extensions: [
			{name: "vector"},
			{name: "pg_trgm"},
		]
	}
}

// The shared Redis keyspace. `persistent` is a redis concept — on a pg
// database the webhook refuses it rather than ignoring it, because a
// silently-dropped durability request is the kind that is noticed after
// a restart.
eventsCache: v1alpha1.#SharedDatabase & {
	metadata: {
		name:      "events"
		namespace: "shop"
	}
	spec: {
		type:       "redis"
		persistent: true
	}
}
