// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// `_namespaces` — plain Kubernetes namespaces the umbrella ships
// UNCONDITIONALLY, as standalone manifests (not as a component
// Application, and not gated by any component's `enabled`). Mirrors
// `_appProjects`: hidden (leading underscore), consumed by the tier
// overlays via `namespaces: _namespaces`, and iterated by
// `templates/namespaces.yaml` at the SAME sync-wave (-30) as
// AppProjects — earliest possible, before even Cilium (-20) — because
// the reason for existing this early is ordering, not ownership:
// whatever lands inside one of these namespaces must find it already
// there.
//
// A plain `[...string]` LIST rather than a `[string]: #Spec` map like
// `_appProjects`: an `AppProject` needs real per-entry fields
// (description, destinations, whitelists) the template renders; a bare
// Namespace needs nothing beyond its name — `templates/namespaces.yaml`
// supplies the static `apprafter.io/*` labels itself, the same way
// `templates/appprojects.yaml` does. If a namespace ever needs its own
// per-entry data (extra labels, annotations), this is the point to
// widen to a map — not before.
//
// `nats-system` (2.5c / ADR 0061 §1 "Deployment", §2 "Tenancy"): NATS
// and NACK ship `enabled: false` (`component_nats.cue` /
// `component_nack.cue`) — the resourceclaim-provisioner turns them on
// by merge-patching `PlatformStack.spec.overrides.nats.enabled` once a
// jetstream claim actually needs them. But the provisioner's own
// ordering is: write the `nats-accounts` Secret INTO `nats-system`,
// THEN flip the override, THEN await readiness (ADR 0061 §1 —
// reversing this deadlocks: the server's `include` of a missing
// accounts file is fatal, and a pod mounting an absent Secret stays
// `ContainerCreating` forever). That means the namespace must exist
// BEFORE either component is ever enabled, so it cannot come from
// either component's own `CreateNamespace=true` syncOption — that only
// fires once Argo CD actually renders the component's Application,
// which never happens while `enabled: false`. Shipping the namespace
// here, independent of both components, is what breaks the ordering
// deadlock.
//
// It also means the provisioner never needs a cluster-scoped
// `Namespace` CREATE grant to stand the namespace up itself — a real
// widening under ADR 0024's posture that a single fixed namespace name
// does not justify. A bare namespace costs no pods and no budget, so
// shipping it unconditionally (rather than gating it on jetstream
// actually being used) has no downside on a cluster that never touches
// `needs.jetstream`.
_namespaces: ["nats-system"]
