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
// `required` lists directly. The same blind spot applies to the
// `streams[].name` / `consume[].durable` DNS-1123-label `pattern`
// (round-7 review, ADR 0061 §4.2/§6): this fixture supplies valid names
// ("orders", "indexer"), so it can't observe that `pattern` disappearing
// either. `crdgen`'s `jetstream_declared_names_require_dns_1123_pattern`
// test closes that one the same way.
//
// EXTENDED for 2.28 (ADR 0065 §2): the tuning surface and a dead-letter
// queue. Two of the combinations here are deliberate worked examples of
// rules the SERVER enforces and the webhook mirrors — `discardPerSubject`
// alongside `discard: "new"` and a positive `maxMsgsPerSubject` (error
// 10052), and `maxDeliver: 5` against a 3-step `backoff`, which the server
// requires to be strictly greater (error 10116). Both were measured on
// 2.14.3; see `docs/measurements/2.28-jetstream-2026-09-15.md`.
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

							// 2.28 tuning (ADR 0065 §2.1). `discardPerSubject`
							// is supplied together with the two fields the
							// server requires alongside it — `discard: "new"`
							// and a positive `maxMsgsPerSubject` (error 10052,
							// measured) — so this fixture is also the worked
							// example of that combination.
							maxMsgs:           1000000
							maxMsgsPerSubject: 1000
							maxMsgSize:        1048576
							maxConsumers:      16
							discard:           "new"
							discardPerSubject: true
							duplicateWindow:   "2m"
							compression:       "s2"
							allowDirect:       true
							description:       "order events, fanned in from shop and billing"
						},
						// NO `consumerLimits` — the webhook refuses it. The
						// shipped JetStream controller runs its legacy
						// reconciler, which never forwards stream-level
						// consumer limits to the server. `maxAckPending` on
						// the `consume` entry below is the way that works.
					]
					// Reads another application's stream via its own durable,
					// with the redelivery contract the manifest could not
					// express before 2.28 — and a dead-letter queue for the
					// messages that exhaust it.
					//
					// `maxDeliver` is 5 and `backoff` has 3 steps: the server
					// requires maxDeliver to be STRICTLY greater than the
					// number of steps (error 10116, measured), so this is also
					// the worked example of that rule.
					consume: [
						{
							from:       "feeder"
							stream:     "blocks-head"
							durable:    "indexer"
							ackPolicy:  "explicit"
							ackWait:    "30s"
							maxDeliver: 5
							backoff: ["1s", "5s", "30s"]
							maxAckPending: 100
							filterSubject: "blocks.head.eu"
							deliverPolicy: "all"
							replayPolicy:  "instant"
							deadLetter: {
								stream:   "indexer-dlq"
								maxBytes: "128Mi"
								maxAge:   "168h"
							}
						},
						// Reading the DLQ is an ordinary consume entry naming
						// it — no new permission, no new machinery.
						{stream: "indexer-dlq", durable: "dlq-reader"},
					]
				}
			}
		}
	}
}
