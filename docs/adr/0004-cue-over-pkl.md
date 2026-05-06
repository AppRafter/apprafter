# ADR 0004: CUE as the configuration language

## Status

`Accepted`. Date: 2026-05-06.

## Context

Every platform manifest (Application, ServiceProvider, ResourceClaim,
AccessGrant, MigrationPlan, ExternalSurface, Infrastructure) is typed
and declarative. We need a configuration language that:

- Validates against schemas at author time, not at apply time.
- Supports unification (per-environment overrides without text
  templating).
- Has reasonable Kubernetes ecosystem integration.
- Can be exported to JSON / YAML for Kubernetes API consumers.

Candidates: **CUE** (cuelang.org) and **Pkl** (Apple's typed config
language).

## Decision

The platform commits to **CUE** for all manifests and schema
definitions. A single CUE module under `schemas/` is the source of
truth: it generates OpenAPI v3 CRD definitions and feeds the
operator's renderer.

## Consequences

Positive:

- CUE unification gives us per-environment overrides natively, with
  no Helm-style text templating.
- The CUE ecosystem has mature k8s integrations (`cue cmd`,
  `cue import` for Kubernetes types).
- Schema validation at author time catches errors before commit.

Negative:

- CUE has a steeper learning curve than YAML for newcomers.
  Mitigated by golden-path templates and comprehensive examples.
- Pkl has stronger IDE tooling in the Apple ecosystem, which we are
  giving up.

## Alternatives considered

- **Pkl** (Apple). More polished IDE support but a younger ecosystem
  and fewer Kubernetes integrations.
- **YAML + JSON Schema.** Rejected: no unification, no first-class
  type system at author time.
- **Helm templates.** Rejected: text templating is error-prone and
  not type-safe.
- **Jsonnet.** Viable, but its functional model has been shown to
  scale less well than CUE's lattice-based unification for large
  config trees.

## Risks

- CUE's ecosystem is younger than Helm's; we may hit edge cases in
  the language itself. Mitigated by keeping our use focused on
  schema + unification, not advanced computation.

## Owner

Schemas maintainers.

## Re-evaluation

Re-evaluate at platform M5 (Tier 3 milestone) with a written follow-up
ADR comparing CUE and Pkl on actual production schema complexity.

## References

- `spec.md` §1.1, §5, §7 ("CUE vs Pkl"), §8 ("Why per-environment
  overrides via CUE unification").
- <https://cuelang.org/>
- <https://pkl-lang.org/>
