# operator/

Custom Rust operator built on `kube-rs`. Implements controllers for
the `Application`, `ResourceClaim`, `AccessGrant`, and
`MigrationPlan` CRDs in a single reconcile loop (no Crossplane
composition layer).

## Layout

This is a Cargo workspace with the following crates:

| Crate                                  | Role                                                              |
| -------------------------------------- | ----------------------------------------------------------------- |
| `admission-webhook`                    | Validating admission webhook for v1alpha1 `Application` (binary).  |
| `operator-core`                        | Shared types — kube-rs `Application` CRD type (library).           |
| `operator-rendering`                   | Pure renderer: `Application` -> Vec of k8s resources (library).    |
| `operator-controllers/application`     | kube-rs Controller for `Application` (library).                    |

The `apprafter-operator` binary (which wires the controllers + a
metrics + health HTTP server) lands in v0.1.27. Leader election +
the Helm chart land in v0.1.28 and close plan.md sub-phase 1.8.
`ResourceClaim` / `AccessGrant` / `MigrationPlan` controllers come
in their own subphases under phase 2.x / 4.x.

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
