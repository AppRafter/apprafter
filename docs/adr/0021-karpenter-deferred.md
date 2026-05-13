# ADR 0021: Karpenter deferred to Phase 6.2 AWS native; cluster-autoscaler not supported

## Status

Accepted (2026-05-12).

## Context

Tier 3+ multi-node clusters can benefit from node autoscaling. Two mainstream tools exist:

- **cluster-autoscaler** — classical, works with fixed node pools / ASGs. Cycle ~5-10 minutes. Bin-packing is weak.
- **Karpenter** — modern, native AWS-first. Provisions individual nodes per-pod-request via cloud API, bin-packs aggressively, drift-detects, consolidates. Cycle in seconds-minutes.

Karpenter on AWS is a first-class deployment. Karpenter on Hetzner requires Cluster API + a Karpenter-CAPI provider — transitively depends on CAPI infrastructure being available.

CAPI is positioned as a Turnkey Phase 5+ concern (see ADR 0016 and ADR 0023). It is not part of OSS core in v1.

## Decision

### Not shipped in v1

Karpenter is **not** part of v1 OSS core. cluster-autoscaler is also not part of v1.

### Phase 6.2 — AWS native

When Phase 6.2 (AWS provider) is implemented, Karpenter is included standalone as part of the AWS stack. Tier 4 AWS deployments use Karpenter natively.

### Phase 5+ — Hetzner via CAPI

When CAPI infrastructure becomes part of the platform (during Turnkey work in Phase 5+), Karpenter on Hetzner becomes available as an opt-in for OSS Tier 2+ clusters. The exact subphase is added when CAPI is ready, not pre-scheduled.

### Managed offering

Managed Ops and Turnkey Cloud — Karpenter default enabled (we automate cost optimisation for customers).

### cluster-autoscaler — not supported

cluster-autoscaler is not adopted at any tier. Reasons:
- Karpenter supersedes it functionally with significantly better behaviour.
- Maintaining two autoscaling tools adds operational complexity.
- For deployments where Karpenter is unavailable (T1, T2 pre-CAPI), manual node scaling via `platform-cli scale` is the path.

### Bare metal slow autoscaling — research item

Tier 3 bare metal cannot be autoscaled at Karpenter speeds (Hetzner Robot server orders take minutes-to-hours). A design constraint is recorded: bare metal autoscaling, if implemented, must not degrade UX/DX compared to faster tiers (Application behaviour identical, slow provisioning hidden through capacity headroom and predictive scaling). Implementation is a research item for Phase 6+ or v2, not a Karpenter pattern.

### Suggestions advisor — managed-side feature

A separate "you should scale up" advisor service is **not** shipped in OSS. OSS users either use Karpenter (when available) or manage capacity manually. The advisor is a managed-side feature, leveraging observability data (VictoriaMetrics + ClickHouse logs) plus potential LLM analytics. Phase 4+ managed work, recorded in Managed Strategy open questions.

## Rationale

### Per-tier applicability is narrow

- **T1 single VDS:** no autoscaling possible (one machine).
- **T2 (3+ nodes, heterogeneous):** Karpenter is the killer use case — bin-packing across heterogeneous nodes, reactive scaling. But requires CAPI (Hetzner path) which is Phase 5+.
- **T3 bare metal:** Karpenter pattern doesn't fit (hours to provision); separate research.
- **T4 hyperscalers:** Karpenter native, works out of the box.

Net: Karpenter is genuinely valuable on T2 (post-CAPI) and T4 (native), not universally.

### CAPI is the gating dependency on Hetzner

Karpenter-on-Hetzner via CAPI is a transitive dependency chain. Since CAPI is positioned as Phase 5+ Turnkey foundation, Karpenter naturally inherits that timing.

### AWS native path is independent and earlier

AWS doesn't need CAPI for Karpenter (Karpenter has AWS-native cloud provider). Phase 6.2 AWS provider can include Karpenter directly.

### Skip cluster-autoscaler — Karpenter is functionally a superset

cluster-autoscaler's only advantage is wider provider support. With Hetzner needing CAPI either way, the wider provider support of cluster-autoscaler does not actually help — both tools end up needing the same underlying machinery on Hetzner. May as well skip the inferior tool.

## Consequences

**Positive:**
- v1 OSS doesn't carry autoscaling code complexity.
- Phase 6.2 AWS gets a high-quality autoscaler natively.
- Managed offering has clear differentiator: customers get Karpenter-by-default without configuring it.
- Bare metal autoscaling design constraint is recorded explicitly, preventing accidental degradation.

**Negative:**
- T2 OSS users have no autoscaling in early v1 (must manually scale via `platform-cli scale`).
- Karpenter advisor in managed is a separate engineering effort.

**Trade-offs:**
- Coverage breadth (autoscaling everywhere) traded for v1 focus (autoscaling where it matters most: T4 AWS and managed offerings).

## Risk

- T2 OSS user demand for autoscaling before CAPI is ready — manual `platform-cli scale` workaround works but is friction. Mitigation: document workaround clearly; consider lightweight scale automation if demand emerges.
- Karpenter project changes course (e.g. AWS reduces investment). Mitigation: Karpenter is CNCF Sandbox with multi-vendor contribution; reasonable durability expected.

## Owner

Core platform team; AWS native in Phase 6.2, Hetzner via CAPI when CAPI lands in Phase 5+, managed advisor as separate Managed Strategy work.

## Re-evaluation triggers

- T2 OSS demand for autoscaling becomes high before CAPI work (could trigger interim manual-scale automation).
- Karpenter project loses momentum (would trigger reconsideration of cluster-autoscaler or custom solution).
- Bare metal slow autoscaling research completes — informs whether a separate primitive is needed or Karpenter-style works.

## References

- Karpenter project: https://karpenter.sh
- Cluster API Karpenter provider (work in progress, community-driven).
- ADR 0016 (Hetzner+AWS native — Karpenter inherits this provider boundary).
- ADR 0023 (Multi-tenancy / CAPI dependency for Hetzner path).
- spec.md §4.1 Compute Substrate (per-tier).
- spec.md §5 Tech Stack (Node autoscaling row).
- Managed Strategy §13 (Karpenter advisor open question).
