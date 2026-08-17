<div align="center">
  <img src="docs/apprafter-logo.svg" alt="AppRafter" width="160" />

# AppRafter

**An opinionated, vertically integrated Platform-as-a-Service on Kubernetes.**

**One `Application` manifest, designed to run unchanged from a €5 VDS to multi-node production — an open-source core, with an optional managed cloud on top.**

[![License: FSL-1.1-Apache-2.0](https://img.shields.io/badge/license-FSL--1.1--Apache--2.0-blue.svg)](./LICENSE)
[![Plugins: MIT](https://img.shields.io/badge/plugins-MIT-green.svg)](./LICENSE-MIT)

</div>

---

> **Status:** pre-1.0, under active development. The single-node **Tier 1** path has shipped (MVP, the `v0.1.x` series); multi-node tiers, the platform services, and the managed cloud are in active development. `spec.md` (its `Revision` line) and `plan.md` are the source of truth — this README states what runs today versus what is on the roadmap.

## What it is

AppRafter fills the gap between a hosted PaaS and vanilla Kubernetes:

- **Hosted PaaS** — easy to start, but tied to one vendor, costly at scale, and not self-hostable.
- **Vanilla Kubernetes** — portable and scalable, but the cognitive load is high and the ecosystem is fragmented.
- **AppRafter** — one `Application` manifest, four hardware tiers, GitOps as the only control surface, and no vendor lock-in at any layer. Deploy via Argo CD and the `apprafter` CLI, with destructive operations gated by a `MigrationPlan` CRD rather than an unguarded `kubectl delete`. (An MCP interface — letting an AI agent drive deploys through the same gate — is on the roadmap.)

The platform is deliberately **opinionated** — one proven component per slot (e.g. Cilium for networking, Argo CD for GitOps) — so there is a single, well-trodden path instead of an assembly project.

## Two products, one platform

AppRafter is an open-source platform first, with a managed cloud as a second product built **on top of** the same core — not a premium fork of it.

### 1. Open-source core — self-host

Everything required to run the platform is open source and free to run on your own hardware or in your own cloud: the Rust operator and its CRDs, the `apprafter` CLI, the developer portal, the platform services, and observability. The managed cloud adds only a thin convenience layer on top — a self-hosted cluster is **fully functional without it**.

Shipped today is the single-node **Tier 1** control plane: `apprafter` provisions a Hetzner VDS, bootstraps the cluster (Cilium, upstream Gateway API, the AppRafter operator, Argo CD, cert-manager), and the operator reconciles your `Application`s through GitOps. Multi-node tiers and the `needs`-based platform services (Postgres, Redis, and more) are landing per `plan.md`.

Licensed under **FSL-1.1-Apache-2.0** (see [License](#license)).

### 2. Managed cloud — on top (in development)

For teams who would rather not run the ops themselves, a managed cloud is in development. Its defining design choice: **your cluster, control plane, operator, and all your data always stay on your own infrastructure.** The managed side hosts only a thin layer on top — the Backstage portal, an account UI, and a hosted MCP endpoint.

Two properties follow directly from that architecture:

- **Anti-vendor-lock by design.** Because the cluster will be a standalone open-source install on your own infrastructure, canceling the subscription leaves it running by design — you lose only the hosted convenience layer, with no migration required.
- **Minimal Data Exposure.** The managed services are designed to see only metadata — manifests applied, status and audit events — never the data in your databases or your secret values.

The managed cloud is planned at three levels of increasing scope — **Hosted Services** (the launch plan) → **Managed Operations** → **Turnkey Cloud** — billed per cluster. A waitlist opens at launch.

> A hardware **tier** (the compute substrate, T1–T4) and a managed **plan** (how much of the operations we run for you) are two independent axes — any tier can run self-hosted or under a managed plan.

## Tier model

The dev-facing API is identical across tiers; moving between tiers is a platform operation, not a manifest rewrite. Tier names denote the **compute substrate** only.

| Tier               | Substrate                                  | Typical use                    | Status              |
| ------------------ | ------------------------------------------ | ------------------------------ | ------------------- |
| **1 · Solo**       | Single VDS (Hetzner CX/CPX-class)          | Side-projects, solo founders   | **Implemented**     |
| **2 · Small team** | 3+ heterogeneous nodes (HA)                | Growing teams, production      | In development      |
| **3 · Production** | Bare metal (dedicated EPYC, Talos)         | Established products           | Roadmap (Phase 5+)  |
| **4 · Regulated**  | External hyperscalers (AWS / GCP / Azure)  | Regulatory / sovereignty needs | Roadmap (Phase 6+)  |

**Confidential containers** (Kata-CC on TDX / SEV-SNP hardware) are a planned **orthogonal opt-in capability** — available on any tier whose hardware supports it, not a tier of their own.

## Under the hood

Boring, proven components, plus a thin layer of our own code only where no ready solution exists.

- **Components the platform standardizes on (one per slot):** Talos Linux, k3s + Cilium (eBPF networking), Argo CD (GitOps), CloudNativePG, Dragonfly (Redis-compatible), ClickHouse, NATS JetStream, Backstage, cert-manager, external-dns, KEDA. Secrets use SealedSecrets on Tier 1 and OpenBao on Tier 2+; control-plane storage is kine (SQLite on Tier 1), with a NATS JetStream backend and a replayable audit log as the Tier 2+ target.
- **Written here, shipping today:** the Rust operator on kube-rs (reconciling `Application`, `MigrationPlan`, `PlatformStack`, and `SourceCredential`), a Rust admission webhook enforcing cross-field invariants the CRD schema can't express, the `apprafter` CLI (provisioning, bootstrap, lifecycle), and the `MigrationPlan` reconciler that gates destructive changes behind explicit approval. CUE is the design-time schema layer (`schemas/`), checked with `cue vet`.
- **Designed, landing per the roadmap:** the `ResourceClaim` / `ServiceProvider` primitives and their reconcilers (Phase 2), the `AccessGrant` access model (Phase 4), and the MCP server with its agentic-safety gate (managed track).

## Repository layout

| Directory                                    | Contents                                                       |
| -------------------------------------------- | -------------------------------------------------------------- |
| [`cli/`](./cli/)                             | `apprafter` — Rust CLI for provisioning, bootstrap, lifecycle  |
| [`operator/`](./operator/)                   | In-cluster Rust operator and admission webhook (kube-rs)       |
| [`schemas/`](./schemas/)                     | CUE schemas for every CRD                                      |
| [`providers/`](./providers/)                 | Built-in `ServiceProvider`s (Postgres, JetStream, ClickHouse, Redis, S3) |
| [`backstage-plugins/`](./backstage-plugins/) | TypeScript plugins for the developer portal                    |
| [`platform-stack/`](./platform-stack/)       | Platform Helm / CUE charts distributed to clusters             |
| [`argocd-cue-cmp/`](./argocd-cue-cmp/)       | Argo CD config-management plugin for CUE app repositories      |
| [`manifests/`](./manifests/)                 | Base platform manifests (Tier 1 today)                         |
| [`landing/`](./landing/)                     | Project website (Astro + Payload CMS)                          |
| [`docs/`](./docs/)                           | TechDocs sources, ADRs, visual assets                          |
| [`examples/`](./examples/)                   | Reference `Application`s and golden-path templates             |

## Quick start

```sh
git clone https://github.com/AppRafter/apprafter
cd apprafter
nix develop          # or open in the VS Code Dev Container
just bootstrap       # install local Git hooks
just lint            # CUE + SPDX + cargo + bun checks
just e2e-up          # local k3d cluster
```

Three setup paths (Nix flake, Dev Container, manual via `mise`) are documented in
[`docs/contributing/setup.md`](./docs/contributing/setup.md).

Full operator and developer documentation:

- [Operator quickstart](./docs/operator-guide/quickstart.md) — provision and bootstrap a Tier-1 cluster.
- [Platform management](./docs/operator-guide/platform-management.md) — PlatformStack lifecycle, channels, upgrade and freeze.
- [Migration plans](./docs/operator-guide/migration-plans.md) — approve and reject destructive-change gates.
- [GitOps walk](./docs/operator-guide/gitops-walk.md) — wiring Argo CD to your Git repositories.
- [Writing Application.cue](./docs/dev-guide/application-cue.md) — the CUE manifest format, CMP, and multi-environment patterns.

## License

AppRafter is **open core**:

- **Platform core** (`cli/`, `operator/`, `schemas/`, `manifests/`) — **FSL-1.1-Apache-2.0**: the Functional Source License, which auto-converts to **Apache 2.0** two years after each release. It permits any use — personal, internal business, and commercial workloads — **except offering AppRafter itself as a managed service to third parties**. Once a release reaches its two-year conversion date, that restriction lifts.
- **Plugins** (`providers/*`, `backstage-plugins/*`, community SDKs) — **MIT** from day one.
- **Documentation** (`docs/`) — **CC-BY-4.0** for prose, **Apache-2.0** for code samples in guides. See [`docs/license.md`](./docs/license.md).

See [`LICENSE`](./LICENSE), [`LICENSE-APACHE`](./LICENSE-APACHE), [`LICENSE-MIT`](./LICENSE-MIT), [`LICENSE-CC-BY-4.0`](./LICENSE-CC-BY-4.0), [`NOTICE`](./NOTICE), and ADRs [0001](./docs/adr/0001-license-fsl-1-1-mit.md) (original FSL choice) and [0032](./docs/adr/0032-license-fsl-1-1-apache-2-0.md) (current core license) for the rationale.
