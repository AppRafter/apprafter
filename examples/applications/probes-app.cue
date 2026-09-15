// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Example Application declaring probes (2.28 / ADR 0065 §1). A `cue vet`
// fixture for `#Probes` / `#Probe`: before it, nothing in this repository
// evaluated real data against either type, so a renamed field
// (`periodSeconds` → anything else), a dropped enum value off `scheme`, or a
// field moved between the two types would pass every other gate silently.
//
// It supplies BOTH forms deliberately — `readiness` and `liveness` are HTTP
// (they carry a `path`), `startup` is a TCP connect (it does not) — because
// the form is discriminated by `path`'s presence, and a fixture carrying one
// form cannot observe that discrimination breaking.
//
// Same blind spot `jetstream-app.cue` records, twice over. A fixture that
// SUPPLIES a field cannot observe that field becoming optional, and the
// leading-slash `pattern` on `path` is a CRD patch this fixture's valid
// paths cannot observe disappearing either. Both are the generated CRD's
// own gates to hold (`just crd-check`, `just crd-validate`), not this one's.
package examples

import v1alpha1 "apprafter.io/schemas/v1alpha1"

probesApp: v1alpha1.#Application & {
	metadata: {
		name:      "probed-service"
		namespace: "demo"
	}
	spec: {
		base: {
			image: "ghcr.io/example/probed-service:latest"
			expose: port: 8080
			probes: {
				// HTTP form, everything else left to the platform default —
				// the shape the manifest is meant to make easy.
				readiness: path: "/healthz"

				// HTTP form with an explicit budget and a header. A declared
				// liveness would normally derive a startup probe; the explicit
				// one below wins outright instead (ADR 0065 §1.4).
				liveness: {
					path:             "/livez"
					periodSeconds:    30
					timeoutSeconds:   3
					failureThreshold: 2
					headers: "X-Probe": "apprafter"
				}

				// TCP form — no `path`, so this is a connect rather than a GET.
				startup: {
					port:             8080
					periodSeconds:    5
					failureThreshold: 60
				}
			}
		}
		environments: dev: {
			// Partial override (2.16c): one number. `merge_probes` keeps the
			// base's `path` and keeps the liveness/startup probes this
			// environment never mentions.
			probes: readiness: periodSeconds: 3
		}
	}
}
