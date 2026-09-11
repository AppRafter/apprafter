// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Example Application declaring `needs.jetstream` (Phase 2.5 / ADR 0061
// §6): a declared stream carrying subjects/storage/retention/maxAge/
// maxBytes/allowPurge, a `consume` entry pulling from another
// application's stream, and `dynamicStreams`. Used as a `cue vet`
// fixture for `#JetStreamNeed` / `#JetStreamStream` / `#JetStreamConsume`
// — `crdgen`'s field comparison checks `{path → kind}`, not
// required-ness, so a CUE-side relaxation of a required field (e.g.
// `maxBytes: string` to `maxBytes?: string`) would regenerate the CRD,
// leave the Rust kind unchanged, and pass `crd-check` on both assertions
// with no gate going red; this fixture is the layer that notices, by
// actually evaluating data against the type.
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
