# ADR 0008: HTTP-first notifications API

## Status

`Accepted`. Date: 2026-05-06.

## Context

Applications need to send notifications (email, Slack, Telegram,
custom webhooks) reliably with retries, DLQ, and audit. Two
transport options:

1. Expose NATS JetStream directly to applications and let them
   publish to subjects.
2. Wrap the transport in an HTTP service that applications call.

Option 1 is more efficient at scale and gives applications richer
primitives (streams, consumers). Option 2 is the universal lowest
common denominator.

## Decision

The notifications service exposes an **HTTP `POST /send` API**.
Internally, requests are persisted to NATS JetStream
(`notifications.<account>.outbox`); channel workers consume from the
stream and deliver. NATS is an implementation detail and is not
exposed to application code.

A thin SDK wrapper is provided for convenience, but applications can
integrate via raw HTTP without any platform-specific library.

## Consequences

Positive:

- Every language has an HTTP client built in; no NATS-client
  dependency or SDK lock-in for application code.
- Solo founders writing a small Bun service do not have to learn
  NATS protocols just to send an email.
- HTTP is debuggable with `curl`, browser DevTools, or Postman; NATS
  needs specialist tooling.
- We retain the option to expose direct NATS for power users in a
  future version if real demand emerges.

Negative:

- HTTP adds a small per-call overhead vs direct NATS publish. We
  accept this: notifications are not a hot path for application
  workloads.
- An additional service must be operated. Acceptable: it is one of
  the canonical platform services.

## Alternatives considered

- **Direct NATS to applications.** Rejected: SDK and specialist-
  tooling burden on the application side.
- **gRPC to applications.** Rejected for the same UX reasons as
  direct NATS.
- **Off-the-shelf SMTP relay only.** Insufficient: no retries, no
  DLQ, no per-channel routing, no audit.

## Risks

- Unexpected throughput requirements may force us to expose NATS
  directly. We accept this and have a documented escape hatch.

## Owner

Notifications maintainers.

## Re-evaluation

If application-side throughput patterns demand sub-millisecond
publish, revisit and consider exposing NATS as an opt-in.

## References

- `spec.md` §4.6 ("Notifications — detailed design"), §8 ("Why HTTP
  API for notifications (not direct NATS exposure)").
- ADR 0009 (Platform-only templates).
