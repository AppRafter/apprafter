# ADR 0016: Hetzner and AWS as exclusive built-in infra providers in v1

## Status

Accepted (2026-05-12). Supersedes ADR 0011.

## Context

ADR 0011 specified a hybrid infrastructure provider model: native Rust SDK for Hetzner Cloud, Hetzner Robot, and AWS as built-in providers, with community providers implemented as `InfrastructureProviderPlugin` wrapping OpenTofu modules.

Practice and analysis revealed:
1. **Architectural leak.** Two state models (`.apprafter/state.json` + OpenTofu state), two error models (Rust `Result` + Terraform diagnostics), two lifecycle models (native idempotency labels + Tofu state files). This is a leak of abstraction that compounds maintenance cost.
2. **Crossplane alternative disqualified.** Crossplane requires a management cluster to provision the first VPS — incompatible with Tier 1 single-VDS bootstrap model where one CLI on a laptop spins up a cluster from zero.
3. **Cluster API as more suitable Turnkey foundation.** For managed/hosted customer cluster scenarios (Phase 5+), Cluster API is the mainstream choice, not Crossplane. CAPI is a separate decision at Phase 5+; it does not motivate community plugins in v1.
4. **Target audience reality.** Hetzner Cloud, Hetzner Robot, and AWS cover 90%+ of the target audience (solo founders + small business on Hetzner, regulated workloads on AWS for compliance).

## Decision

v1 ships **only** native Rust SDK providers: Hetzner Cloud, Hetzner Robot, and AWS.

The `InfrastructureProviderPlugin` interface and OpenTofu shim are not shipped in v1. Phase 7.5 and 7.6 of the plan (which would have implemented these) are removed.

If concrete demand for additional clouds emerges before v2, new cloud support is added as a fourth native Rust provider implementation (the `cli-providers::Provider` trait is preserved as a generic extension point). No external plugin contract or SDK is shipped in v1.

## Rationale

### Native SDK preserves UX where it matters

For the two primary providers (Hetzner, AWS), tight integration with native APIs gives the best operational experience: precise error handling, transactional semantics, no subprocess invocation latency. Wrapping these through OpenTofu would degrade the experience.

### OpenTofu shim costs exceed value for v1

Implementing the `InfrastructureProviderPlugin` contract would require:
- A CUE-to-OpenTofu translator layer.
- State reconciliation between OpenTofu state and AppRafter state.
- Error mapping from Terraform diagnostics to AppRafter error model.
- Subprocess invocation lifecycle management.
- Per-provider validation of behaviour.

This is substantial work for value that is uncertain — multi-cloud beyond Hetzner+AWS may or may not be needed by actual users in v1.

### `Provider` trait keeps the door open

The internal `Provider` trait in `cli-providers` is a generic extension point. Adding a fourth native cloud is straightforward when demand materialises. This is **not** "we cannot add clouds", it is "we don't add them speculatively".

### Managed offering is unaffected

Managed Ops and Turnkey Cloud use the same native `Provider` trait, executed from a managed-side controller with customer's (or AppRafter HQ's) cloud credentials. No managed offering depends on community plugins.

## Consequences

**Positive:**
- Reduction in Phase 7 scope (7.5 + 7.6 removed).
- One state model, one error model, one lifecycle model across all infrastructure operations.
- Clearer documentation: "AppRafter supports Hetzner and AWS" is unambiguous.

**Negative:**
- "Multi-cloud" support is explicitly deferred to v2 — some marketing positioning becomes narrower.
- Users wanting Scaleway/OVH/DigitalOcean/etc. have no path in v1 (besides "wait for v2" or "request a native provider").

**Trade-offs:**
- Breadth (every cloud has a path) traded for depth (Hetzner and AWS have the best experience).

## Risk

- Tier 3+ customer requests an unsupported cloud (e.g. "we need Equinix Metal"). Mitigation: add fourth native provider on demand; cost is bounded since `Provider` trait is generic.
- v2 plugin contract design suffers from lack of v1 dogfooding. Mitigation: v2 design informed by accumulated customer demand patterns, not speculation.

## Owner

Core platform team; AWS provider in Phase 6.2, additional native providers on demand.

## Re-evaluation triggers

- Concrete Tier 3+ customer commits to AppRafter conditional on a specific cloud not currently supported.
- Multi-cloud becomes a documented user demand pattern (more than ad-hoc requests).
- v2 planning explicitly revisits plugin contract design after accumulated experience.

## References

- ADR 0011 (Hybrid native-SDK + OpenTofu-shim) — superseded by this ADR.
- ADR 0023 (Kamaji multi-tenancy — informed Turnkey/CAPI separation reasoning).
- spec.md §3.7 Infrastructure (provider model).
- spec.md §4.12 Infrastructure Tooling.
- spec.md Appendix B Non-goals (multi-cloud entry).
- The marketing strategy (Turnkey Cloud — CAPI dependency for Phase 5+).
