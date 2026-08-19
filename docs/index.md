---
description: "Where to start: the four hardware tiers and what each section of this site covers."
---

# AppRafter

**Opinionated, vertically-integrated Platform-as-a-Service on
Kubernetes — the same `Application` manifest from a €5 VDS to
confidential bare metal.**

This site documents the AppRafter platform as it ships today:
guides for running a cluster, guides for shipping an application
onto one, and generated reference for every command and object.

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

- **[Operator Guide](operator-guide/index.md)** — installing and
  running the platform.
- **[Developer Guide](dev-guide/index.md)** — building applications
  on the platform.
- **[Reference](reference/index.md)** — the custom resources the
  platform installs, every CLI command, every environment variable.
- **[ADRs](adr/README.md)** — the reasoning behind each architectural
  choice, one decision per record.

## Status

Pre-MVP. Active development.
