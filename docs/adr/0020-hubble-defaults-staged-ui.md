# ADR 0020: Hubble defaults and staged UI strategy

## Status

Accepted (2026-05-12).

## Context

Hubble is Cilium's native eBPF-based network observability layer. It provides L3/L4 flow logs, L7 HTTP visibility, and security observability — observed live from the same datapath that handles workload traffic, at minimal overhead.

Spec.md §4.10 (rev.5) mentioned Hubble in passing: "Network observability: Hubble (Cilium-native, eBPF)". Spec.md §4.1 Tier 3 explicitly enables it. Tier 2 was ambiguous. Tier 1 was unspecified.

Additionally, the choice of UI surface (Hubble UI standalone, Backstage flow visualizer plugin, possible future custom OneBun portal) needed explicit positioning.

## Decision

### Defaults per tier

- **Tier 1 production:** opt-in (default off).
- **Tier 1 / dev mode:** opt-in (default off).
- **Tier 2+:** enabled by default.
- **Managed Ops / Turnkey:** enabled by default.

### Staged UI strategy

Hubble UI surface is delivered in three stages, sequential and additive:

**Stage 1 — Hubble UI standalone.** Cilium ships Hubble UI as part of its Helm chart, controlled by `hubble.ui.enabled=true`. This provides a graph view, flow table, namespace/identity filters, mature L3-L7 query interface. Stage 1 is essentially free: one Helm value, no AppRafter-specific development.

**Stage 2 — Backstage flow visualizer plugin.** AppRafter ships a Backstage plugin that shows Hubble flows on the Application page (embedded, not separate UI). The plugin adds a "convert observed flow to explicit policy" button: developers see actual traffic, click the button, the plugin generates a Pull Request that adds the observed destination to the Application's `connects.egress` declaration.

**Stage 3 (possible, contingent on Managed Strategy §13.5) — Custom OneBun portal flow visualizer.** If the project moves to a custom OneBun-based portal as a replacement for Backstage, the flow visualizer is re-implemented there. This stage is not committed; it depends on the broader Backstage-vs-custom-portal decision in Managed Strategy.

## Rationale

### Hubble itself is the killer feature of Cilium

Cilium is already chosen as CNI (see spec.md §1.1). Hubble runs in the same eBPF datapath at minimal overhead. Not enabling Hubble means leaving free observability on the table.

### Tier 2+ default reflects "growth pathway"

Tier 1 is the simplification tier; observability stack is opt-in to keep footprint low. Tier 2+ is the "team grows, needs visibility" tier; Hubble default reflects that.

### Staged UI minimises blocking dependencies

Hubble UI standalone is shipped by Cilium — no AppRafter-side work to make it available. Backstage flow visualizer is a meaningful but bounded effort. Custom portal is contingent on a larger strategic decision and is not blocked by Hubble.

Each stage adds value without breaking previous stages. Users can stop at any stage and have a working experience.

### "Convert observed flow to policy" closes the loop on default-deny

AppRafter's default-deny network policy (via Cilium NetworkPolicy) means users must explicitly declare what their applications connect to (`connects.egress` in Application). Hubble UI shows what they actually try to connect to. The Backstage plugin button turns this from "look up what you forgot" to "one-click add to declaration" — significantly improves the default-deny developer experience.

## Consequences

**Positive:**
- Network observability available at no extra ops cost (eBPF, same datapath).
- "Convert flow to policy" workflow makes default-deny tolerable for developers.
- Staged delivery means each phase has working state.

**Negative:**
- Tier 1 users who want network observability must opt in explicitly (extra step in solo-tier onboarding).
- Backstage plugin development takes engineering time (Stage 2).

**Trade-offs:**
- Footprint optimisation at Tier 1 (opt-in) traded against immediate availability for solo users.

## Risk

- Hubble UI security exposure if accidentally exposed without auth. Mitigation: Hubble UI ingress requires AccessGrant (no public exposure by default).
- Backstage plugin maintenance burden (custom code in Backstage's evolving ecosystem). Mitigation: keep plugin scope narrow; depend on stable Backstage APIs.

## Owner

Core platform team; Stage 1 in Phase 3.7a, Stage 2 in Phase 3.7b.

## Re-evaluation triggers

- Cilium drops Hubble as a feature (unlikely; Hubble is core Cilium).
- Backstage-vs-custom-portal decision (Managed Strategy §13.5) resolves, triggering Stage 3 work.
- Customers report that Hubble UI is sufficient and Backstage plugin is not adopted — could lead to Stage 2 deprioritisation.

## References

- Hubble documentation: https://docs.cilium.io/en/stable/observability/hubble/
- ADR 0023 (Multi-tenancy — Hubble visibility scoped per Kamaji TCP).
- spec.md §4.3 Networking (default-deny + Hubble observability).
- spec.md §4.10 Observability.
- spec.md §4.11 UX Layer (Backstage plugins).
- Managed Strategy §13.5 (Backstage vs custom portal open question).
