---
description: "Outline of the layers the platform is built from, and where to read each one until the full chapter is written."
---

# Architecture

> **Status:** stub. The chapters listed below are not written yet.

This section will cover the layers the platform is built from, one
navigable page each:

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

Until those pages exist, the [ADRs](../adr/README.md) carry the
rationale behind each specific choice, and §2 and §4 of
[the repository's architectural specification](https://github.com/apprafter/apprafter/blob/master/spec.md)
carry the layer-by-layer description. That specification is a
roadmap: it is not published on this site, and it names capabilities
that do not exist yet.
