# Architecture

> **Status:** stub. Full TechDocs migration of the architecture
> chapter happens in phase 8.2.

This section will mirror `spec.md` §2 (Architecture Overview) and §4
(Layer Specifications), broken into navigable subpages:

- Compute substrate (per tier).
- Control-plane storage — kine + NATS JetStream.
- Networking — Cilium, Gateway API, mTLS, egress.
- Secrets — SealedSecrets at Tier 1, OpenBao at Tier 2+.
- Application operator (custom Rust on `kube-rs`).
- Platform services (Postgres, JetStream, ClickHouse, Redis, S3,
  Notifications).
- External surface (git host, registry, access plane, monitoring,
  backups).
- Build pipeline (Dockerfile-first with auto-analysis).
- Observability (OpenTelemetry → VictoriaMetrics + ClickHouse +
  Hubble).
- UX layer (Backstage + custom plugins).
- Infrastructure tooling (`apprafter`).

Until those pages exist, refer to `spec.md` §2 and §4 in the repo
root, and to the [ADRs](../adr/README.md) for the rationale behind
specific choices.
