# ADR 0005: kine + NATS JetStream as control-plane storage

## Status

`Accepted`. Date: 2026-05-06.

## Context

Kubernetes' default storage layer is etcd. The platform also runs
NATS JetStream as a first-class platform service (used for the
notifications transport, the audit log, and CDC). Running etcd and
NATS side by side doubles the operational burden for fundamentally
similar functionality (consistent log + watch).

`kine` is a shim that exposes the etcd v3 API on top of various
backends (SQLite, PostgreSQL, MySQL, NATS KV). With kine in front,
NATS JetStream can serve as Kubernetes' state store.

## Decision

We will use **kine + NATS JetStream** as the Kubernetes control-plane
storage. The same NATS cluster also serves the platform's
notifications transport, audit log, and CDC needs.

## Consequences

Positive:

- One distributed system to operate (NATS) rather than two
  (etcd + NATS).
- Every Kubernetes resource mutation is also a JetStream message:
  audit, CDC, time-travel come for free.
- External consumers (Backstage plugins, monitoring) can subscribe
  to the same streams without polling the API server.
- Cross-region replication uses JetStream mirroring rather than
  etcd's restricted Raft model.

Negative:

- kine implements a subset of etcd v3. Some Kubernetes features that
  rely on subtle etcd semantics need verification (admission
  webhooks, watch on CRDs, conditional updates).
- Performance ceiling vs etcd is not yet measured at very large scale
  (>1000 nodes, >100k objects).

## Alternatives considered

- **etcd.** Standard, but adds a second consensus system to operate.
- **kine + SQLite.** Simple but does not give us the event-log bonus,
  and clustering requires a separate replication story.
- **kine + PostgreSQL.** Useful in some single-node deployments but
  not aligned with the NATS-centric architecture.

## Risks

- A future Kubernetes feature relies on an etcd-only behaviour; we
  must either fix kine or fall back to etcd for that tier. We accept
  this risk and track upstream changes.
- NATS KV's conditional-write behaviour must remain correct under
  high CRD churn. kine ≥ 0.10 handles this; we will monitor.

## Owner

Platform-control-plane maintainers.

## Re-evaluation

Revisit if:

- We hit a hard scaling ceiling at production scale (Tier 3+).
- A regression in upstream Kubernetes makes kine + NATS infeasible.

## References

- `spec.md` §4.2 and §8 ("Why kine + NATS JetStream over etcd").
- <https://github.com/k3s-io/kine>
- <https://docs.nats.io/nats-concepts/jetstream>
