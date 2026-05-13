# ADR 0014: Why AppRafter, not Cozystack

## Status

Accepted (2026-05-12).

## Context

Cozystack is a CNCF Sandbox project providing a batteries-included PaaS distribution on Kubernetes. Its stack — Talos + FluxCD + KubeVirt + Cilium + Kube-OVN + LINSTOR + VictoriaMetrics + Kamaji — overlaps reasonably with AppRafter's intended technology selection. Both projects target the gap between vanilla Kubernetes and proprietary PaaS.

When evaluating whether to build AppRafter or contribute to Cozystack as an extension, several differences justify separate development.

## Decision

AppRafter is built as a separate project rather than as a Cozystack extension or distribution.

## Rationale

### Different target user

Cozystack primarily targets hosting providers and private clouds where multi-tenancy is a P0 architectural concern from day one. AppRafter primarily targets solo founders and small product teams where single-tenant is the P0 deployment mode, with multi-tenancy as a Phase 2+ addition.

This reframes core decisions:
- Cozystack assumes a management cluster that hosts customer control planes; AppRafter assumes a single CLI bootstraps the cluster directly.
- Cozystack designs for tens-to-hundreds of tenants per installation; AppRafter optimises Tier 1 single-VPS footprint.

### Tier model

Cozystack's deployment baseline is bare metal. AppRafter Tier 1 is a single VDS at €5/month. This forces different decisions across the stack:
- Control plane storage (kine+NATS in AppRafter vs etcd in Cozystack).
- Default runtime (containerd at Tier 1 vs Kata at all tiers in Cozystack).
- Bootstrap experience (single-binary CLI vs Cluster API + FluxCD).

### License model

Cozystack uses Apache 2.0. AppRafter uses FSL-1.1-MIT (auto-converts to MIT 2 years after each release).

Apache 2.0 alone exposes the project to traction-without-revenue risk — cloud vendors can rebrand and offer the platform as their managed product with no contribution back. FSL-1.1-MIT provides a 2-year commercial-use protection window per release while preserving full OSS access for individuals and on-prem deployments.

### API surface

Cozystack exposes the underlying Kubernetes API plus Helm charts. AppRafter abstracts via custom CRDs (`Application`, `ResourceClaim`, `AccessGrant`, `Tenant`) — developers don't see Kubernetes primitives unless they want to.

### Control plane storage

Cozystack uses etcd. AppRafter uses kine+NATS JetStream as a unified event substrate (same NATS that backs platform services and apps).

### Operator language

Cozystack uses Go with FluxCD as the GitOps engine. AppRafter uses Rust with kube-rs for idiomatic systems-level work, with Argo CD as the GitOps engine.

## Where Cozystack is studied and borrowed from

Cozystack remains a useful reference for specific patterns:
- KubeVirt integration patterns for VM workloads.
- LINSTOR for replicated block storage at Tier 3+.
- VictoriaMetrics + ClickHouse observability stack — same choice in AppRafter.
- Talos as base OS at Tier 3+.
- Kamaji multi-tenancy pattern — adopted for hard multi-tenancy at Tier 2+ (see ADR 0023).
- Philosophy "don't hide Kubernetes, educate users" — shared principle.

## Consequences

**Positive:**
- AppRafter has freedom to optimise for solo/small-team use case without compromising Cozystack's hosting-provider focus.
- Both projects can evolve independently and benefit from cross-pollination of patterns.

**Negative:**
- AppRafter pays the cost of building parallel infrastructure that Cozystack already has.
- Different community split.
- Migration tooling between Cozystack and AppRafter is not provided initially.

**Trade-offs:**
- Engineering cost vs alignment with target user mental model — AppRafter chose alignment.

## Risk

Main risk: spending engineering cycles re-implementing what Cozystack already has. Mitigation: borrow specific architectural patterns where they fit, rather than reinventing.

## Owner

Core platform team.

## Re-evaluation triggers

- If the solo-founder/small-team segment proves insufficient to sustain the project, and hosting-provider segment becomes dominant, the "separate project" decision may need reconsideration.
- If Cozystack adds first-class single-VPS Tier 1-equivalent support, differentiation narrows and reconsideration may be warranted.

## References

- Cozystack project page.
- ADR 0001 (License decision — FSL-1.1-MIT rationale).
- ADR 0023 (Multi-tenancy via Kamaji — pattern borrowed from Cozystack).
