# ADR 0022: Tier model clarification — T1/T2/T3/T4 + orthogonal confidential

## Status

Accepted (2026-05-12).

## Context

Spec.md §0 (rev.5) target users table and §4.1 Compute Substrate carried definitions that conflated tier substrate with feature checklists:

- **Tier 2** described as "3× CCX or small dedicated" — implying a fixed 3-node configuration.
- **Tier 4** described as "AWS C8i (TDX), confidential bare metal" — implying confidential compute is the defining feature.

Discussion during multi-tenancy and confidential containers analysis surfaced that these descriptions were misleading and constrained subsequent decisions in ways that didn't match the actual design intent.

## Decision

Tier descriptions are refined to describe **compute substrate** only. Features (confidential, observability, multi-tenancy) are orthogonal concerns layered onto tiers, not tier-defining.

| Tier | Substrate | Quorum | Confidential | Note |
|---|---|---|---|---|
| **T1** | Single VDS | None | Opt-in if CPU supports (rare on entry-level VDS) | Floor tier; simplifications in favor of price |
| **T2** | 3+ nodes, heterogeneous allowed (mixed sizes) | HA | Hardware usually unavailable on VDS | Horizontal scaling pathway out of T1; **not** fixed 3 nodes |
| **T3** | Bare metal (EPYC dedicated) | HA | Opt-in when SEV-SNP-capable hardware available | Performance/sovereignty focus; bare metal advantages |
| **T4** | External hyperscalers (AWS, GCP, Azure) | HA | Opt-in (TDX/SEV-SNP instances) | Regulation/compliance primary driver |

### Confidential containers — orthogonal opt-in

Confidential compute is an Application-level opt-in feature, available where hardware supports it. It does **not** define a tier. See ADR 0015 for the full confidential stack.

### Tier capabilities — derived, not defining

- HA: T1 has none (single node); T2-T4 have it.
- Hard multi-tenancy: T1 structurally unavailable; T2+ via Kamaji (see ADR 0023).
- Hubble default: T2+ default; T1 opt-in (see ADR 0020).
- Kata default runtime: T3+ (see spec.md §4.1).
- KubeVirt: T3+ (see spec.md §4.1).

## Rationale

### Substrate ≠ feature checklist

Tiers describe the deployment environment. Features are individual decisions per Application or per cluster. Mixing them creates artificial constraints: "I want regulated compute on bare metal" gets labelled Tier 4 incorrectly; "I want confidential workloads on cloud VPS with appropriate CPU" becomes definitionally impossible.

### T2 horizontal scaling

The "Tier 2 = 3 nodes" framing implied a fixed configuration. Reality: T2 is the **growth pathway** out of T1. A small team might start with 3 small nodes, add bigger ones later (mixed sizes), end up with 15 heterogeneous nodes. All of this is T2.

### T4 regulation, not confidential

Confidential compute is one tool for regulated workloads, but not the only one. Many compliance regimes (SOC2, ISO 27001, regional sovereignty) require hyperscaler-class infrastructure for audit/contractual reasons without requiring confidential containers. Confidential remains an opt-in even at T4.

### Backward compatibility

The `manifests/tier-1/`, `manifests/tier-2/`, `manifests/tier-3/`, `manifests/tier-4/` repository structure remains unchanged. Only the documentation describing what each tier means is updated.

## Consequences

**Positive:**
- Tier descriptions become operationally meaningful (substrate, not feature lists).
- Feature decisions per tier are explicit and can be revised independently.
- Confidential compute decoupled from T4 — available where hardware supports.

**Negative:**
- Spec.md §0 and §4.1 require substantial rewrite.
- Users who internalized the prior model need to be re-educated.

**Trade-offs:**
- Conceptual clarity at cost of one-time documentation effort.

## Risk

- Documentation drift if all references to old tier descriptions aren't updated. Mitigation: comprehensive pass through spec.md, plan.md, DEV_MODE_SPEC.md, and the marketing strategy.

## Owner

Core platform team; spec.md updates as part of pre-Phase-2 cleanup.

## Re-evaluation triggers

- New tier needed (e.g. T5 for some new substrate class) — would extend rather than restructure.
- Confidential becomes mandatory at all tiers (regulatory shift) — would re-couple confidential to specific tiers.

## References

- ADR 0015 (Tier 4 confidential as orthogonal).
- ADR 0023 (Multi-tenancy availability per tier).
- ADR 0020 (Hubble per-tier defaults).
- spec.md §0 Vision (target users table).
- spec.md §1.8 Solo-tier adoption.
- spec.md §4.1 Compute Substrate.
- DEV_MODE_SPEC §1 (Tier vs Mode distinction — preserved).
