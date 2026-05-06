# ADR 0002: Project codename "AppRafter"

## Status

`Accepted`. Date: 2026-05-06.

## Context

The project needs a stable codename that affects domain registration,
repository naming, and brand from day one. Decision criteria:

- Short and memorable.
- Available `.io` / `.dev` domain.
- No conflict with major OSS projects.
- Fits the platform metaphor — carrying many small things across
  rough water (a raft of applications across many tiers and
  substrates).

## Decision

The project's codename is **AppRafter**. The repository name, the
`LICENSE` copyright holder ("AppRafter Authors"), and future domain
registrations (`apprafter.dev`, `apprafter.io`) use this name.

## Consequences

Positive:

- Distinct: no Wikipedia disambiguation conflicts (cross-checked
  against Cumulus Networks, Bedrock Linux, Helix Editor, Atlas
  Toolkit, etc.).
- "Raft" carries the connotation of carrying many small things
  safely through rough water — fits the "many Applications across
  many tiers" model.
- Easy to pronounce and spell in English and other Latin-script
  languages.

Negative:

- "Raft" overlaps with the Raft consensus algorithm; readers may
  have initial confusion. Acceptable: we do not use Raft consensus
  ourselves at the platform layer (NATS uses a different mechanism),
  so the overlap is cosmetic.

## Alternatives considered

- `Cumulus` — collides with Cumulus Networks.
- `Bedrock` — collides with Bedrock Linux and Minecraft Bedrock
  Edition; SEO is hostile.
- `Helix` — collides with Helix Editor and several music brands.
- `Substrate` — collides with the Polkadot Substrate framework.
- `Atlas` — heavily overloaded (HashiCorp Atlas, MongoDB Atlas).

## Risks

- Future legal review may surface unrelated trademark conflicts.
  `NOTICE` is explicit that "AppRafter" is a project codename and
  that no trademark rights are granted.

## Owner

Project maintainers.

## Re-evaluation

Only if a credible trademark conflict surfaces.

## References

- `spec.md` §7 (open question 9, resolved as `AppRafter`).
