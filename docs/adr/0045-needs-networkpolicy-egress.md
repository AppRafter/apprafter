# ADR 0045: needs → CiliumNetworkPolicy egress auto-derivation, with a config-driven cluster egress profile

## Status

`Accepted`

Date: 2026-06-08.

## Context

The cluster ships a default-deny `NetworkPolicy`
(`manifests/tier-1/network-policies/default-deny.yaml`) that is **Ingress-only**:
it allows same-namespace ingress + kube-system and deliberately leaves egress
open, because restricting egress before the operator could auto-derive per-app
allows would silently break DNS and Service routing (the v0.1.50 → v0.1.51 fix).
The file's own comment defers the egress half to "phase 2.10".

Consequently any Application pod can today reach any in-cluster service —
including the shared Postgres (`cnpg-system`) and Redis (`dragonfly-system`) —
regardless of whether it declared `needs.pg` / `needs.redis`. The plan.md §2.10
goal is that declaring a need is what grants network reachability to that
service, and an app that did not declare it gets a Hubble drop.

A NetworkPolicy is an allow-list: gating egress to pg behind `needs.pg` requires
the app's pods to be **default-deny on egress**, with allowances limited to DNS,
same-namespace, the declared needs, and — by policy — the external internet.

Two cross-cutting concerns shaped the decision:

1. The platform commits to **one CNI: Cilium** (spec §1.1). The policy mechanism
   should be consistent with that and ready for L7/mTLS (3.3) and egress-gateway.
2. The egress posture is a security default that operators must be able to
   **change**, including on Tier-1, without editing raw Kubernetes objects — and
   the change must survive Argo CD self-heal (the 2.9 learning that Argo reverts
   a live `kubectl` patch of a git-managed field).

## Decision

**1. Derive a per-Application egress `CiliumNetworkPolicy` (cilium.io/v2), not a
plain k8s `NetworkPolicy`.** The operator emits one CNP per Application (per-CR,
not per-need) that selects the app's pods on egress. Cilium **entities**
(`world`) — unavailable in plain NetworkPolicy — let us express "external
internet open, in-cluster cross-service egress closed unless declared", which is
the chosen breadth. There is no kube-rs type for CNP, so we hand-roll a typed
serde mirror in `operator-core` (consistent with the hand-rolled CRD mirrors)
rather than using a raw `DynamicObject`.

**2. Breadth = internet open, in-cluster gated by needs (default).** The baseline
allows DNS → kube-dns, same-namespace, `toEntities: [world]`, and one rule per
declared network need (pg, redis, …). `disk` has no network target and is
excluded. Every Application gets the CNP — including needs-less ones — so that an
app that did not declare `needs.pg` cannot reach pg (Hubble drop) while still
reaching external APIs. Full default-deny egress is rejected for launch because
apps have no way yet to declare external destinations (that arrives with
`connects` / ExternalSurface).

**3. The egress posture is config-driven via `PlatformStack.spec.network.egress.profile`,
not a hard-coded constant.** An additive optional enum field with three presets —
`internet` (default: DNS + same-ns + world + needs), `internal` (DNS + same-ns +
needs), `strict` (DNS + needs; same-namespace also denied) — gates which baseline
rules the renderer emits. The operator reads the singleton PlatformStack; field
absent / CR missing → `internet`. The default lives in the infrastructure config
(operator fallback, optionally baked via `platform-stack` values), is the same on
all tiers at launch, and pre-positions per-tier hardening and future per-app /
per-target access control onto the same CNP mechanism.

**4. A first-class CLI surface, `apprafter platform egress` (`show` / `set`),**
manages the profile so users never hand-edit Kubernetes objects. `set` patches
PlatformStack via server-side apply with field manager `apprafter-cli`. To
survive Argo self-heal, the platform-stack chart does **not** declare the field
by default (so Argo has no diff to revert), and `set` warns if it detects the
field is git-managed.

The connection-target catalog (type → namespace + service selector + port) is a
static table in the operator, namespace-overridable from `ServiceProvider.spec.config`,
and threaded into the **pure** renderer — the same pattern as `needs_secrets`
(2.4e) and `disks` (2.6b). The CNP is owned by the Application (ownerReferences →
cascading delete) and applied via SSA with field manager `apprafter-operator`.

## Consequences

- **Security default tightens:** all Applications become egress-restricted for
  cross-namespace in-cluster traffic once the 2.10 operator rolls out. Internet,
  DNS, same-namespace, and declared needs remain open under the default profile.
  An app silently relying on an *undeclared* in-cluster service breaks — the
  intended hardening, surfaced via Hubble drops.
- **CNP, not NetworkPolicy:** the codebase grows a hand-rolled CNP type and the
  operator gains `ciliumnetworkpolicies` RBAC. Plain-NetworkPolicy type safety
  from `k8s_openapi` is traded for the `world`-entity capability and Cilium
  alignment.
- **Operator reads PlatformStack:** a new cross-CR read on the app reconcile path,
  with a safe `internet` fallback when absent.
- **GitOps caveat:** `platform egress set` is a live SSA patch that persists only
  while the field is not git-declared; a fully-GitOps CLI path is future work.
- **Release:** coordinated operator + webhook + platform-stack bump
  (`change: safe`, additive optional CRD field) plus a cli bump for the new
  command.

## Alternatives considered

- **Plain k8s `NetworkPolicy`.** Type-safe via `k8s_openapi`, consistent with the
  existing default-deny, sufficient for L3/L4 egress-to-service. Rejected: cannot
  express "external internet open, in-cluster closed" (no `world` entity), and
  diverges from the one-way-Cilium commitment / future L7 needs.
- **Full default-deny egress at launch.** Maximal lockdown, but strands apps that
  make external calls with no declaration mechanism. Deferred behind the `strict`
  profile and future `connects`/ExternalSurface.
- **Egress default in operator Helm values instead of PlatformStack.** Simpler
  (no cross-CR read), but changing it requires a helm-upgrade / GitOps commit
  rather than a live `kubectl edit` / CLI patch; less ergonomic for the
  user-overridable requirement.
- **Read the connection target from `ResourceClaim.status`** (per-instance
  precision). More plumbing and couples rendering to runtime status, for no
  launch benefit — we target at the service level.

## References

- plan.md `### 2.10 needs → NetworkPolicy auto-derivation`; design spec
  `docs/superpowers/specs/2026-06-08-2.10-needs-networkpolicy-design.md`.
- spec §1.1 (one way = Cilium); ADR 0020 (Hubble defaults, staged UI).
- ADR 0042 (needs.redis / Dragonfly), ADR 0043 (needs.disk + named claims),
  ADR 0044 (per-environment deploy — the Argo-reverts-live-patch learning).
