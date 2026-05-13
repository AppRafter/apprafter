# ADR 0011: Hybrid native-SDK + OpenTofu-shim infrastructure providers

## Status

`Superseded by ADR 0016`. Originally accepted 2026-05-06; superseded 2026-05-12.

## Context

`platform-cli` provisions infrastructure on multiple providers
(starting with Hetzner Cloud and AWS, with OVH, Scaleway, GCP, Azure,
DigitalOcean, vSphere, Proxmox to follow via the community). Two
extreme strategies:

1. Native Rust SDKs for every provider — best UX, infinite
   maintenance burden.
2. A single `kind: Infrastructure` manifest translated to OpenTofu
   for every provider — minimal maintenance, looser integration.

## Decision

We adopt a **hybrid** model:

- **Built-in providers** (Hetzner Cloud, Hetzner Robot, AWS) are
  implemented as native Rust SDK calls inside `platform-cli`.
- **Community providers** (OVH, Scaleway, GCP, Azure, DigitalOcean,
  vSphere, Proxmox, etc.) are implemented as
  `InfrastructureProviderPlugin`s that wrap an OpenTofu module. The
  plugin contains a CUE-to-OpenTofu translator and a state-importer.

`platform-cli` invokes OpenTofu under the hood for community
providers. Users see only CUE manifests and `platform-cli plan/apply`.
State remains in Git, encrypted via age/sops.

## Consequences

Positive:

- The two main providers we ship get the tightest possible
  integration (native SDKs, no subprocess overhead).
- Every cloud / virtualization platform with an OpenTofu provider
  becomes accessible without us writing a Rust SDK for each.
- OpenTofu is MPL-2.0, Linux Foundation, aligned with our
  no-vendor-lock principle.
- Users never see Terraform / OpenTofu directly; it is an
  implementation detail.

Negative:

- Two implementation paths inside `platform-cli`. Mitigated by a
  uniform plugin contract (CUE → OpenTofu translator interface).
- OpenTofu's subprocess invocation adds startup cost. Acceptable for
  infrastructure operations (which are not hot-path).

## Alternatives considered

- **Native SDK everywhere.** Rejected: years of engineering for
  marginal value beyond the top two providers.
- **OpenTofu everywhere.** Rejected: Hetzner and AWS UX matters
  enough to justify native SDKs.

## Risks

- A future OpenTofu fork or governance change could affect us.
  Mitigated by the MPL-2.0 license and the mature provider ecosystem.

## Owner

`platform-cli` maintainers.

## Re-evaluation

Revisit if OpenTofu's governance changes materially or if a
community emerges around a non-Tofu provider standard.

## References

- `spec.md` §3.7, §4.12, §8 ("Why hybrid native-SDK + OpenTofu-shim
  for infrastructure providers", "Why infrastructure tooling in Rust
  + CUE, not pure Terraform/Ansible").
