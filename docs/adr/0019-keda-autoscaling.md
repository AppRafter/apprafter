# ADR 0019: KEDA as official autoscaling backend in v1

## Status

Accepted (2026-05-12).

## Context

Spec.md §4.5 (rev.5) referenced KEDA implicitly: "render Deployment + Service + Gateway Route + ScaledObject (KEDA)". This was a de-facto decision without an explicit decision record.

The alternative was a custom autoscaler controller — a Rust-native reconcile loop that reads Application + reads metrics (Prometheus/VictoriaMetrics) + patches HPA on Deployment. This would avoid an external dependency but require building autoscaling logic from scratch.

## Decision

KEDA is the official horizontal workload autoscaling backend in v1.

The operator renders KEDA `ScaledObject` resources from `Application.autoscale` declarations. KEDA itself is installed as a platform-service during cluster bootstrap (T1) or via Helm during HA bootstrap (T2+).

Supported triggers in v1:
- `jetstream_lag` — NATS JetStream consumer lag.
- `cpu` — pod CPU utilisation.
- `memory` — pod memory utilisation.
- `http_rps` — HTTP requests per second (via Gateway metrics).

Additional triggers may be added in subsequent versions based on demand.

Custom autoscaler is **not** built in v1.

## Rationale

### KEDA is mature and proven

KEDA is a CNCF Graduated project (graduated August 2023). It has extensive trigger ecosystem (50+ scalers), production deployments across thousands of clusters, and well-developed mock infrastructure for testing.

### Custom autoscaler is build complexity without incremental value

Building a custom autoscaler requires:
- Reconcile loop with metrics polling.
- HPA patching logic.
- Trigger evaluation per scaler type.
- Test infrastructure for each trigger.

This is several months of work that KEDA already provides. The incremental value (Rust-native, tighter operator integration) does not justify the cost for v1.

### Manifest portability

Application's `autoscale.on:` field abstracts away the underlying mechanism. If KEDA proves insufficient later, the abstraction is preserved and a custom autoscaler can be swapped in as the backing without manifest changes.

## Consequences

**Positive:**
- Battle-tested autoscaling from day one.
- 50+ triggers available out of the box (most not enabled in v1, but available for opt-in).
- Operator code stays focused on AppRafter primitives, not autoscaling internals.

**Negative:**
- External dependency adds bootstrap step and resource overhead (~50MB Helm chart, 3 deployments).
- KEDA's poll cycle (default 30s) may be insufficient for latency-critical scaling decisions on business metrics.

**Trade-offs:**
- Build vs buy — chose buy for v1. Build is available later if KEDA proves insufficient.

## Risk

- KEDA fails on a specific scaler type AppRafter needs to support (e.g. custom metric source not in KEDA's scaler list). Mitigation: KEDA's scaler interface is extensible; AppRafter can ship a custom KEDA scaler if needed.
- KEDA upgrade breaks AppRafter's ScaledObject schema. Mitigation: pin KEDA version in platform Helm chart; test upgrades in CI.

## Owner

Core platform team; install + ScaledObject rendering in Phase 2 (subphase TBD per PLAN_CHANGES doc).

## Re-evaluation triggers

- KEDA fails to meet latency requirements for business-metric scaling.
- KEDA project pivots or loses maintenance support.
- Custom autoscaler becomes necessary for a specific customer requirement.

## References

- KEDA project: https://keda.sh
- spec.md §3.1 Application (autoscale field).
- spec.md §4.5 Application Operator (ScaledObject rendering).
- spec.md §5 Tech Stack (Workload autoscaling row).
