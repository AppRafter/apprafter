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
the chosen breadth. There is no kube-rs type for CNP; following the established
external-CR precedent (CNPG, Argo CD, and Dragonfly objects are all built with
`serde_json::json!` → `DynamicObject` + `ApiResource`), the CNP is built the same
way, not as a hand-rolled typed mirror. Hand-rolled types are reserved for *our
own* CRDs (`operator-core`); a typed CNP mirror is deferred (YAGNI) until L7/mTLS
fields are actually modelled. The rendered structure is asserted by the
renderer's unit tests.

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
absent / CR missing → `internet`. The default lives in the infrastructure
config: the operator fallback (`internet`) is the documented default, **not in
git**; a non-default is set via the `apprafter` CLI, still operator-managed. The
explicit **git-management escape hatch** is declaring the field in an opt-in
infra-repo that Argo reconciles (then GitOps is authoritative and `set` warns —
see #4). The uniform `internet`
launch default is a function of **capability, not policy**: apps cannot yet
declare external destinations. Once app-level egress declaration ships
(`connects` / ExternalSurface), the Tier-2+ default flips to `internal`
(DNS + same-ns + needs, `world` closed) while Tier-1 retains `internet` for solo /
no-infra-repo ergonomics; the profile mechanism already pre-positions this
per-tier divergence and future per-app / per-target access control onto the same
CNP.

**4. A first-class CLI surface, `apprafter platform egress` (`show` / `set`),**
manages the profile so users never hand-edit Kubernetes objects. `set` patches
PlatformStack via server-side apply with field manager `apprafter-cli`. To
survive Argo self-heal, the platform-stack chart does **not** declare the field
by default (so Argo has no diff to revert), and `set` warns if it detects the
field is git-managed.

This reconciles with the §1.4 GitOps-only principle via a **per-tier
control-surface model**, not an override. The PlatformStack singleton is **seeded
by the `apprafter cluster-bootstrap` loader and driven by the operator's
PlatformController** — it is the declarative control plane, not a chart-templated,
Argo-reconciled object **by default** (an infra-repo may opt the field into
git-management — see #3). On Tier-1 a user may operate without an infrastructure
repository, so the `apprafter` CLI editing PlatformStack is the intended,
declarative control surface (§1.5 decl-first — not a `kubectl`-to-prod emergency
override). Opting in to an infra-repo that declares the field is explicit; that
makes it git-managed, at which point GitOps becomes authoritative and `set`
warns. On Tier-2+, manual mutation is gated by AccessGrant (when it ships) and
remains available to the operator during pre-production setup. Because changing
the egress posture is security-relevant, the change is auditable — `set` records
provenance (annotations + a Kubernetes Event) and the active profile is
observable — lightweight audit in the spirit of §1.4's loud-audit-on-manual-ops. The "live SSA patch survives
because the field is not git-declared" mechanism is therefore the deliberate
Tier-1 model, not a temporary gap.

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
- **CNP, not NetworkPolicy:** the operator gains `ciliumnetworkpolicies` RBAC and
  builds the CNP via `serde_json` → `DynamicObject` + `ApiResource` (the
  external-CR precedent), not a hand-rolled type. Plain-NetworkPolicy type safety
  from `k8s_openapi` is traded for the `world`-entity capability and Cilium
  alignment; the rendered structure is covered by unit tests.
- **Operator reads PlatformStack:** a new cross-CR read on the app reconcile path,
  with a safe `internet` fallback when absent.
- **Control surface, not a caveat:** on a Tier-1 cluster with no infra-repo the
  `apprafter` CLI is the intended surface for the PlatformStack control CR
  (operator-managed, not git-declared); an infra-repo opt-in (Tier-1) or
  AccessGrant gating (Tier-2+) restores GitOps authority and `set` warns — see
  §1.4 and Decision #4.
- **Release:** coordinated operator + webhook + platform-stack bump
  (`change: safe`, additive optional CRD field) plus a cli bump for the new
  command.

## Alternatives considered

- **Plain k8s `NetworkPolicy`.** Type-safe via `k8s_openapi`, consistent with the
  existing default-deny, sufficient for L3/L4 egress-to-service. Rejected: cannot
  express "external internet open, in-cluster closed" (no `world` entity), and
  diverges from the one-way-Cilium commitment / future L7 needs.
- **Hand-rolled typed `CiliumNetworkPolicy` mirror** in `operator-core`. Rejected:
  external CRs in this codebase (CNPG, Argo CD, Dragonfly) are uniformly built
  with `serde_json::json!` → `DynamicObject`; hand-rolled types are reserved for
  our own CRDs. A typed CNP mirror is YAGNI until L7/mTLS fields are modelled.
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
