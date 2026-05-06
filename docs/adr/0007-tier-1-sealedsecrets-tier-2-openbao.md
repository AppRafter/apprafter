# ADR 0007: SealedSecrets at Tier 1, OpenBao at Tier 2+

## Status

`Accepted`. Date: 2026-05-06.

## Context

Tier 1 of the platform is a single Hetzner CX22 (or equivalent) at
roughly €5 / month. There is no KMS available for OpenBao
auto-unsealing, and a manual Shamir unseal at every restart is
unacceptable UX for a solo founder. OpenBao's 3-node HA + Raft
footprint is also inappropriate for a single-node, single-app
environment.

At the same time, principle 1.8 of the spec ("Enterprise practices
must not block solo-tier adoption") requires that we ship a secret-
management story that works on Tier 1 and migrates cleanly to Tier 2.

## Decision

Tier 1 uses **Bitnami SealedSecrets** as the default secrets backend.
Developers commit `kind: SealedSecret` manifests; the public
encryption key lives in Git and the in-cluster controller decrypts
them at apply time into Kubernetes Secrets. From Tier 2 upwards,
OpenBao is the default (see ADR 0006).

`platform-cli upgrade-tier` (1 → 2) includes a step that imports
existing SealedSecrets into OpenBao kv-v2 and rewrites Application
manifests to use OpenBao paths. The migration is non-destructive and
one-time.

## Consequences

Positive:

- Tier 1 has zero unseal operations and no extra daemon footprint.
- ~80% of OpenBao's value (encrypted secrets in Git) at ~5% of the
  operational cost.
- Migration to OpenBao on tier upgrade is mechanical.
- Application authors see a single `secret(...)` API regardless of
  which backend resolves it.

Negative:

- SealedSecrets does not provide dynamic credentials, automatic
  rotation, or fine-grained ACL. The Backstage UI carries a visible
  banner explaining this and prompting the migration as the user
  approaches Tier 2.
- Two backends increase documentation surface. Acceptable: the
  difference is invisible to Application authors.

## Alternatives considered

- **OpenBao at Tier 1.** Rejected because of unseal UX and resource
  footprint.
- **Plain Kubernetes Secrets in Git.** Rejected because of plaintext
  exposure.
- **Mozilla SOPS at Tier 1.** Viable but more moving parts than
  SealedSecrets for the same outcome on a single node.

## Risks

- Migration during `upgrade-tier` is the riskiest step. Mitigated by
  pre-migration backup and post-migration content/hash verification.

## Owner

Secrets-platform maintainers.

## Re-evaluation

Revisit if a lightweight OpenBao deployment mode emerges that is
practical on a single VDS, or if SealedSecrets upstream stalls.

## References

- `spec.md` §1.8, §4.4, §8 ("Why SealedSecrets at Tier 1, not
  OpenBao").
- ADR 0006 (OpenBao at Tier 2+).
- <https://github.com/bitnami-labs/sealed-secrets>
