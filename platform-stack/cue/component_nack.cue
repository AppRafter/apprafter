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
		jetstream: nats: url: "nats://nats.nats-system.svc:4222"
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
