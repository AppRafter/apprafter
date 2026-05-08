# operator/

Custom Rust operator built on `kube-rs`. Implements controllers for
the `Application`, `ResourceClaim`, `AccessGrant`, and
`MigrationPlan` CRDs in a single reconcile loop (no Crossplane
composition layer).

## Layout

This is a Cargo workspace with one crate so far:

| Crate                | Role                                                                  |
| -------------------- | --------------------------------------------------------------------- |
| `admission-webhook`  | Validating admission webhook for v1alpha1 `Application` (binary).      |

The `Application` reconcile controller, `ResourceClaim` /
`AccessGrant` / `MigrationPlan` controllers, and operator-rendering
helper crate land in plan.md phases 1.8 / 2.x / 4.x as separate
workspace members.

## Build

```sh
cd operator && cargo build --workspace
```

## Test

```sh
cd operator && cargo test --workspace
```

## Lint

```sh
cd operator && cargo clippy --workspace --all-features --all-targets -- -D warnings
cd operator && cargo fmt --all -- --check
```

## admission-webhook

Validates AdmissionReview requests for `apprafter.io/v1alpha1`
`Application` objects. Errors picked up beyond the CRD's OpenAPI v3
layer:

- `spec.base.image` (or every `spec.environments[*].image`) must be set.
- Environment names must be DNS-1123 labels.
- `env` keys must match `^[A-Z_][A-Z0-9_]*$`.

Run the binary locally (plain HTTP, port 8443 by default):

```sh
cd operator && cargo run --bin admission-webhook
```

Build the container image (multi-stage; final stage is
`distroless/static-debian12:nonroot`):

```sh
cd operator && docker build -t apprafter/admission-webhook:dev -f admission-webhook/Dockerfile .
```

The cert-manager `Certificate`, `Service`, `Deployment`, and
`ValidatingWebhookConfiguration` manifests, plus
`platform-cli cluster-bootstrap` wiring, land in v0.1.24 (closes
plan.md sub-phase 1.7).
