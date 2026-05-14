<div align="center">
  <img src="docs/apprafter-logo.svg" alt="AppRafter" width="160" />

# AppRafter

**Opinionated, vertically-integrated Platform-as-a-Service on Kubernetes — the same `Application` manifest from a €5 VDS to confidential bare metal.**

</div>

---

## What it is

AppRafter fills the gap between PaaS (Fly.io, Railway, Render) and vanilla Kubernetes:

- **PaaS** — easy to start, but vendor-locked, expensive at scale, not self-hostable.
- **Vanilla k8s** — portable and scalable, but cognitive load is enormous and the ecosystem is fragmented.
- **AppRafter** — one `Application` manifest, four deployment tiers (Solo / Small team / Production / Regulated), GitOps as the only control surface, no vendor-lock at any layer.

## Tier model

| Tier             | Persona                    | Hardware                    | Cost        |
| ---------------- | -------------------------- | --------------------------- | ----------- |
| **1. Solo**      | Solo founder, side-project | 1× VDS (Hetzner CX22+)      | €5–20/mo    |
| **2. Small team**| 3–10 engineers             | 3× CCX or small dedicated   | €50–200/mo  |
| **3. Production**| Established product        | 3–5× dedicated EPYC         | €500–2000/mo|
| **4. Regulated** | Compliance / sovereignty   | TDX/SEV-SNP, confidential   | $2000+/mo   |

The dev-facing API is identical across tiers. Migrating between tiers is a platform operation, not a manifest rewrite.

## Repository layout

| Directory                                       | Contents                                                          |
| ----------------------------------------------- | ----------------------------------------------------------------- |
| [`cli/`](./cli/)                                | `apprafter` — Rust CLI for bootstrap and lifecycle             |
| [`operator/`](./operator/)                      | Custom Rust operator on kube-rs                                   |
| [`schemas/`](./schemas/)                        | CUE schemas for every CRD                                         |
| [`providers/`](./providers/)                    | Built-in `ServiceProvider`s (pg, jetstream, clickhouse, redis, s3)|
| [`backstage-plugins/`](./backstage-plugins/)    | TypeScript plugins for the developer portal                       |
| [`manifests/`](./manifests/)                    | Base platform manifests, per tier                                 |
| [`docs/`](./docs/)                              | TechDocs sources, ADRs, visual assets                             |
| [`examples/`](./examples/)                      | Reference `Application`s and golden-path templates                |

## Quick start

```sh
git clone <this repo>
cd apprafter
nix develop          # or open in VS Code Dev Container
just bootstrap       # install local Git hooks
just lint            # CUE + SPDX
just e2e-up          # local k3d cluster
```

Three install paths (Nix flake, Dev Container, manual via `mise`)
are documented in [`docs/contributing/setup.md`](./docs/contributing/setup.md).

## Status

Pre-MVP. Under active development.

## License

Platform core — **FSL-1.1-MIT** (Functional Source License → MIT after 2 years, modeled on Sentry). Plugins (`providers/*`, `backstage-plugins/*`, community SDKs) — **MIT** from day one. See [`LICENSE`](./LICENSE), [`LICENSE-MIT`](./LICENSE-MIT), [`NOTICE`](./NOTICE), and [ADR 0001](./docs/adr/0001-license-fsl-1-1-mit.md) for the rationale.
