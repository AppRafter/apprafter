# ADR 0001: FSL-1.1-MIT for the platform core, MIT for plugins

## Status

`Accepted`. Date: 2026-05-06.
`Superseded` by ADR 0032 (for the core base license choice; plugin MIT carve-out and FSL wrap rationale preserved). Date: 2026-05-19.

## Context

The platform core needs a license that:

- Allows the project to be self-hosted by anyone (including individuals
  on a single VDS), with no per-feature paywall.
- Discourages cloud vendors from rebranding the platform as their own
  managed offering during active development — the "Cozystack-style"
  risk where commodity hyperscalers extract value with no contribution
  back.
- Avoids OSI-disqualifying clauses (BSL non-compete) that block
  enterprise procurement.
- Has a well-understood story for the long term, including the case
  where the project is abandoned.

The plugin SDKs and community plugins have a different concern:
contribution friction must be minimal, including for commercial
adopters and downstream forks.

## Decision

The platform core (`cli/`, `operator/`, `schemas/`, `manifests/`, and
the platform-internal services) is licensed under the Functional
Source License v1.1 with MIT Future License (`SPDX: FSL-1.1-MIT`),
copyright "AppRafter Authors", year 2026. Each release converts
automatically to the MIT License two years after its publication date.

Plugins (`providers/`, `backstage-plugins/`, future
`InfrastructureProviderPlugin` / `ServiceProviderPlugin` SDKs and
community plugins) are licensed under the MIT License from day one.

## Consequences

Positive:

- Cloud vendors cannot ship the platform as their primary managed
  product within the FSL window.
- Every release becomes fully OSI-open within two years; the most
  recent release is at most two years away from MIT — a built-in
  safety net for the community even if the project is abandoned.
- Single license to communicate, easier than a tiered AGPL/MPL/Apache
  split.
- Plugin contributions are friction-free; community plugin authors do
  not have to negotiate FSL terms.

Negative:

- Some users will mistake FSL for non-OSI-open and pass on the
  platform during the FSL window. Documentation has to be explicit
  about the conversion model.
- We must track release dates carefully so that conversion timing
  is unambiguous.

## Alternatives considered

- **Apache-2.0 for everything.** Rejected: cloud vendors can rebrand
  the platform with zero contribution back.
- **AGPL on portal + MPL on core.** Rejected: tier-split licensing
  creates governance complexity and a confusing message to users.
- **BSL** (HashiCorp's choice in 2023). Rejected: BSL is not
  OSI-approved and its non-compete clauses scare enterprise
  procurement.
- **MIT for everything.** Rejected for the core: no protection
  against the cloud-vendor rebranding scenario.

## Risks

- Some procurement teams may flag FSL during the active window
  despite the conversion clause. Mitigation: the conversion model is
  documented in `NOTICE` and the docs site; the safety net of "release
  becomes MIT in 2 years" is the clearest counter-argument.
- Sentry's FSL is still a relatively young license (2023). If the
  ecosystem (package managers, scanners, distros) does not handle FSL
  cleanly, we may need to publish per-release MIT-converted artefacts
  separately.

## Owner

Project maintainers.

## Re-evaluation

Revisit if:

- The OSI ever approves a successor to FSL with broader recognition.
- Adoption is materially limited by license confusion (measurable
  signal: repeated procurement push-back across multiple deployments).

## References

- `spec.md` §7 ("License decision"), §8 ("Why FSL-1.1-MIT for the
  core (Sentry's model)").
- `LICENSE`, `LICENSE-MIT`, `NOTICE` in repo root.
- <https://fsl.software/>
- Sentry's FSL adoption announcement (2023).
