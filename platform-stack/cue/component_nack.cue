// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// NACK — the JetStream Kubernetes controller (`Stream`/`Consumer`/
// `Account` CRs) part 2's provisioner applies once an account's user
// can authenticate (ADR 0061 §1: NACK's `Account` CR is CONNECTION
// CONFIGURATION for `Stream`/`Consumer`, not an account itself — the
// account is the config-file account the provisioner's own
// `nats-accounts` Secret owns). Same shape as `component_nats.cue`:
// `enabled: false` (a plain concrete literal, not `bool | *false` —
// see that file's comment on `_components.nats.enabled` for why
// unifying a conflicting default with #Component's own `bool | *true`
// fails `cue export`/render with an ambiguous default), flipped by the
// SAME provisioner override
// (`PlatformStack.spec.overrides.nats.enabled` — one override covers
// both components; there is no separate `overrides.nack`).
//
// Chart `nack` from the same repo as `nats`
// (https://nats-io.github.io/k8s/helm/charts/), version 0.35.0
// (`helm search repo nats/`, appVersion 0.24.0 — image
// `natsio/jetstream-controller:0.24.0`). Namespace `nats-system`, same
// as the server — ADR 0061's own "Namespaces" section places the
// accounts Secret, the `mgr_<ns>` credential AND the NACK CRs there
// together.
_components: "nack": #Component & {
	name:      "nack"
	enabled:   false
	namespace: "nats-system"
	project:   "platform-providers"
	source: {
		repoURL: "https://nats-io.github.io/k8s/helm/charts/"
		chart:   "nack"
	}
	version: "0.35.0"
	values: {
		jetstream: {
			nats: url: "nats://nats.nats-system.svc:4222"

			// 2.5e walk finding — the flag without which the whole
			// `Account` CR mechanism ADR 0061 §1 rests on is INERT, and
			// NACK cannot talk to the server at all once the feature is
			// used. One flag, two failures:
			//
			// 1. `spec.account` on a `Stream`/`Consumer` is IGNORED without
			//    it. Read in the nack source at v0.24.0:
			//    `controller.go`'s `getAccountOverrides` opens with
			//    `if account == "" || !c.opts.CRDConnect { return
			//    overrides, nil }`, and `runWithJsmc` then uses the GLOBAL
			//    connection for every object. So the provisioner's
			//    `Account` CR — the per-namespace `mgr_<ns>` credential the
			//    ADR describes as "CONNECTION CONFIGURATION for
			//    Stream/Consumer" — was never consulted, and every declared
			//    stream came back `Errored`.
			// 2. WITHOUT it NACK opens a global connection at process start
			//    (`Run()` — the `!CRDConnect` branch dials and returns
			//    `failed to connect to nats` on error). A NATS server with
			//    no `accounts { }` block accepts unauthenticated
			//    connections; one WITH accounts-and-users rejects them. So
			//    NACK connected fine at bootstrap and was locked out
			//    PERMANENTLY the instant the first `needs.jetstream` claim
			//    made the provisioner write the accounts file:
			//    `Error: failed to connect to nats: nats: Authorization
			//    Violation`, CrashLoopBackOff, no `Stream` ever created.
			//    WITH it, `Run()` takes the other branch and builds a lazy
			//    connection pool instead — no global connection is opened at
			//    all, so there is nothing to reject.
			//
			// MEASURED both ways (podman, nats-server 2.14.3 +
			// `natsio/jetstream-controller:0.24.0`, against a server whose
			// accounts file grants no anonymous access): without the flag,
			// `Authorization Violation` at startup; with it, the controller
			// never touches NATS at startup at all.
			//
			// The measurement recorded below ("confirmed connected via its
			// own log line") was real — and taken against a server with no
			// accounts file, i.e. under exactly the condition that stops
			// holding the moment the feature is used.
			//
			// `jetstream.nats.url` above stays: it is the default `-s` for
			// an object with no `spec.account`. The provisioner always sets
			// one, so nothing should take that path — it is a fallback, not
			// the mechanism.
			additionalArgs: ["--crd-connect"]
		}

		// Burstable, NOT Guaranteed — mirrors
		// `component_dragonfly-operator.cue`'s `manager.resources`
		// exactly: NACK is a CONTROLLER (ADR 0053 §1's "platform
		// components" class — modest CPU request, no CPU limit, a
		// generous-but-present memory limit), not the stateful
		// backend itself. That distinction is `component_nats.cue`'s
		// job (the chart-deployed StatefulSet pod there IS the
		// backend instance and gets req==limit on every resource).
		//
		// MEASURED, not guessed, the way dragonfly-operator's own pin
		// is: a real `nack` 0.35.0 controller, in a throwaway `kind`
		// cluster, connected to a real `nats` 2.14.6 StatefulSet in
		// the same namespace (confirmed connected via its own log
		// line: "jetstream-controller connected to NATS Deployment:
		// <ip>:4222", not left to guess whether it was actually
		// reaching the server). Read via `crictl stats` against the
		// container's own cgroup (no metrics-server in a bare `kind`
		// cluster), two samples roughly a minute apart, both steady:
		// `rssBytes` 10.7 MB (~10.2Mi), `workingSetBytes` 11.7-11.8 MB
		// (~11.2Mi), CPU usage near zero at idle (0.23m instantaneous,
		// 83ms total CPU consumed over the whole measurement window).
		// Request
		// rounds the measured working set up slightly (12Mi); limit
		// leaves roughly 4x headroom (48Mi), matching the ratio
		// `dragonfly-operator.cue`'s own `manager.resources` (16Mi →
		// 64Mi) and `component_cloudnative-pg.cue`'s (24Mi → 128Mi)
		// both use.
		resources: {
			requests: {
				cpu:    "25m"
				memory: "12Mi"
			}
			limits: memory: "48Mi"
		}
	}

	// Mirrors `component_dragonfly-operator.cue`'s CRD-bundle
	// ordering exactly: the `jetstream.nats.io` CRDs (`streams`,
	// `consumers`, `accounts`, `streamtemplates`, `keyvalues`,
	// `objectstores` — confirmed via `helm template --include-crds`,
	// since a PLAIN `helm template` does not render a chart's `crds/`
	// directory at all, only `helm install`/Argo CD's own Helm source
	// handling does) must exist and be `Established` before part 2's
	// provisioner — or anything else — applies a `Stream` or
	// `Consumer` object. `-5`, same wave as dragonfly-operator: ahead
	// of the default wave-0 components that would otherwise reference
	// these kinds.
	syncWave: -5

	ignoreDifferences: [{
		group: "apps"
		kind:  "Deployment"
		jsonPointers: ["/status/terminatingReplicas"]
	}]
}
