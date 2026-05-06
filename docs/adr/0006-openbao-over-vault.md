# ADR 0006: OpenBao as the secrets backend (Tier 2+)

## Status

`Accepted`. Date: 2026-05-06.

## Context

HashiCorp's BSL relicensing of Vault in August 2023 removed Vault
from the OSI-open-source category. The platform's "no vendor-lock at
any layer" principle (`spec.md` §1.6) rules out a non-OSS secrets
backend.

OpenBao is an MPL-2.0 fork of Vault 1.14.0, governed by the Linux
Foundation, with IBM among the principal contributors. It is API-
compatible with Vault and reached production-ready 2.5.0 in
February 2026.

## Decision

OpenBao is the secrets backend for **Tier 2 and above**. Vault SDK
clients work unchanged. **Tier 1 uses SealedSecrets** instead — see
ADR 0007 for that decision.

## Consequences

Positive:

- MPL-2.0 license aligns with the platform's no-vendor-lock stance.
- API compatibility with Vault means existing user code works
  unchanged.
- Linux Foundation governance reduces the risk of another sudden
  relicensing.
- Dynamic secrets, leases, and per-request audit are available.

Negative:

- OpenBao's 3-node HA + Raft footprint is too heavy for Tier 1,
  hence the SealedSecrets fallback.
- Some Vault Enterprise-only features (replication, namespaces) are
  not in scope for OpenBao yet; we do not depend on them.

## Alternatives considered

- **HashiCorp Vault.** Rejected because of the BSL relicensing.
- **Mozilla SOPS + KMS.** Viable for static secrets but lacks
  dynamic secrets, leases, and per-request audit.
- **External Secrets Operator.** Complementary, not a replacement
  for the secrets backend itself.

## Risks

- OpenBao's adoption is still building; an upstream maintenance gap
  is the largest risk. Mitigated by Linux Foundation governance and
  IBM's involvement.

## Owner

Secrets-platform maintainers.

## Re-evaluation

Revisit if OpenBao stalls in maintenance or if a successor with
broader adoption appears.

## References

- `spec.md` §4.4 and §8 ("Why OpenBao instead of Vault").
- <https://openbao.org/>
- ADR 0007 (Tier 1 secrets via SealedSecrets).
