# ADR 0003: Custom Rust operator instead of Crossplane

## Status

`Accepted`. Date: 2026-05-06.

## Context

The platform's central control loop reconciles `Application`,
`ResourceClaim`, `AccessGrant`, and `MigrationPlan` CRDs into
Kubernetes resources and platform-service allocations. Two paths were
available:

1. Build a custom operator on top of `kube-rs` or `controller-runtime`.
2. Use Crossplane with XR / XRD / Composition / Functions to express
   the same logic.

## Decision

We will build a custom operator in **Rust** on top of **kube-rs**.
A single reconcile loop owns all platform CRDs. There is no
Crossplane composition layer in the platform.

## Consequences

Positive:

- One file → one resource, debuggable as ordinary Rust code with
  breakpoints, unit tests, and profiling.
- One reconcile loop instead of Crossplane's per-claim multi-loop
  pipeline; latency and memory footprint are predictable.
- Schema evolution uses standard mechanisms: `serde` + Kubernetes
  conversion webhooks. No Composition versioning matrix.
- Performance and memory characteristics suit running on a single
  €5 VDS (Tier 1) without dominating cluster overhead.

Negative:

- We give up Crossplane's heterogeneous cloud-resource catalogue
  and community provider list. We accept this: the platform exposes
  `ResourceClaim` against six canonical platform-service types, not
  arbitrary cloud resources.
- We assume the maintenance cost of a custom operator. Mitigated by
  Rust's language guarantees, kube-rs's maturity, and the small
  surface area of our CRDs.

## Alternatives considered

- **Crossplane.** Rejected: XR / XRD / Composition / Functions add
  four abstractions before reaching Kubernetes resources, and the
  composition layer is opaque to our debugging workflow.
- **Operator SDK / Go.** Viable, but the team has stronger Rust
  fluency and the operator's hot path benefits from Rust's lower
  memory overhead.
- **Pure Helm / Kustomize.** Rejected: the reconcile model requires
  real state management (claims, migrations, attestation).

## Risks

- `kube-rs` lags `controller-runtime` in some niche features
  (e.g., dynamic client). We accept this and contribute upstream
  where needed.
- The Rust talent pool for operator work is smaller than Go's.

## Owner

Operator maintainers.

## Re-evaluation

If our CRD set grows substantially and we find ourselves
re-implementing pieces of Crossplane Composition, revisit.

## References

- `spec.md` §4.5 and §8 ("Why custom Rust operator over Crossplane").
- <https://kube.rs/>
