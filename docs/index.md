---
description: "Where to start: the four hardware tiers, what each section of this site covers, and where the specification of record lives."
---

# AppRafter

**Opinionated, vertically-integrated Platform-as-a-Service on
Kubernetes — the same `Application` manifest from a €5 VDS to
confidential bare metal.**

This site documents the AppRafter platform. The architectural
specification of record lives in `spec.md` at the repository root;
this site presents the same material in a navigable form, plus
operator and developer guides.

## Tier model

| Tier              | Persona                    | Hardware                  | Cost         |
| ----------------- | -------------------------- | ------------------------- | ------------ |
| **1. Solo**       | Solo founder, side-project | 1× VDS (Hetzner CX22+)    | €5–20/mo     |
| **2. Small team** | 3–10 engineers             | 3× CCX or small dedicated | €50–200/mo   |
| **3. Production** | Established product        | 3–5× dedicated EPYC       | €500–2000/mo |
| **4. Regulated**  | Compliance / sovereignty   | TDX/SEV-SNP, confidential | $2000+/mo    |

The dev-facing API is identical across tiers. Migrating between
tiers is a platform operation, not a manifest rewrite.

## Sections

- **[Architecture](architecture/index.md)** — high-level structure
  (layers, control plane, substrate model, tier ladder).
- **[Concepts](concepts/index.md)** — the AppRafter object model:
  `Application`, `ServiceProvider`, `ResourceClaim`, `AccessGrant`,
  `MigrationPlan`, `ExternalSurface`, `Infrastructure`.
- **[Operator Guide](operator-guide/index.md)** — installing and
  running the platform.
- **[Developer Guide](dev-guide/index.md)** — building applications
  on the platform.
- **[Reference](reference/index.md)** — every CRD, every CLI command.
- **[ADRs](adr/README.md)** — architectural decisions.

## Status

Pre-MVP. Active development.
