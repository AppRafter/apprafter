// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Example Application declaring `needs.jetstream` (Phase 2.5 / ADR 0061
// §6): a declared stream carrying subjects/storage/retention/maxAge/
// maxBytes/allowPurge, a `consume` entry pulling from another
// application's stream, and `dynamicStreams`. Used as a `cue vet`
// fixture for `#JetStreamNeed` / `#JetStreamStream` / `#JetStreamConsume`
// — before this fixture, `cue vet ./examples/...` never evaluated real
// data against any of the three, so a typo'd field path, a dropped enum
// value (e.g. `"workqueue"` off `retention`), or a renamed field
// (`allowPurge` → anything else) would pass every other gate silently.
// Verified (round-5 review): it DOES catch tightenings and renames this
// way. It does NOT catch the opposite direction — a required field
// relaxed to optional (`maxBytes: string` → `maxBytes?: string`) still
// passes `cue vet` here, because a fixture that SUPPLIES a field can't
// observe that field becoming optional; `crdgen`'s own field comparison
// checks `{path → kind}`, not required-ness, either. That specific gap
// is closed instead by `crdgen`'s own
// `jetstream_required_fields_survive_crd_generation` test
// (`operator/crdgen/src/main.rs`), which asserts the generated CRD's
// `required` lists directly.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

jetstreamApp: v1alpha1.#Application & {
	metadata: {
		name:      "orders-service"
		namespace: "demo"
	}
	spec: {
		base: {
			image:    "ghcr.io/example/orders-service:latest"
			replicas: 1
			expose: {
				port: 8080
			}
			needs: {
				jetstream: {
					size: "small"
					// Own streams only — no `$JS.API.STREAM.CREATE/UPDATE.>`
					// grant (ADR 0061 §6).
					dynamicStreams: false
					streams: [
						{
							name: "orders"
							// A subject outside the app's own prefix is fan-in
							// (ADR 0061 §6) — legitimate here, gated elsewhere.
							subjects: ["shop.orders.>", "billing.orders.>"]
							storage:    "file"
							retention:  "workqueue"
							maxAge:     "24h"
							maxBytes:   "1Gi"
							allowPurge: true
						},
					]
					// Reads another application's stream via its own durable.
					consume: [
						{from: "feeder", stream: "blocks-head", durable: "indexer"},
					]
				}
			}
		}
	}
}
