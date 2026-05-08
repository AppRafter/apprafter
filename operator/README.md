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
| `apprafter-operator`                   | Operator binary — wires controllers + metrics + health endpoints.  |

Leader election + the Helm chart land in v0.1.28 and close plan.md
sub-phase 1.8.
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

## apprafter-operator

Runs the operator. The binary builds a `kube::Client` via
`Client::try_default()` (in-cluster config inside a pod, or the
`~/.kube/config` fallback when run locally), spawns the Application
Controller, and serves three HTTP routes:

| Route       | Purpose                                          |
| ----------- | ------------------------------------------------ |
| `/healthz`  | Liveness probe — returns 200 OK with body `ok`.   |
| `/readyz`   | Readiness probe — returns 200 OK with body `ready`. |
| `/metrics`  | Prometheus text format — three `apprafter_reconcile_*` metrics. |

Run locally against your current kubeconfig:

```sh
cd operator && cargo run --bin apprafter-operator
# → in another shell:
curl -s http://127.0.0.1:8080/healthz
curl -s http://127.0.0.1:8080/metrics
```

`HTTP_PORT` (default 8080) overrides the listener port. Tracing
filter follows `RUST_LOG` (e.g. `RUST_LOG=apprafter_operator=debug`).

### Leader election

The binary acquires a `coordination.k8s.io/v1` `Lease` named
`apprafter-operator` in the `apprafter-system` namespace before
starting the Application Controller. Holder identity is read from
`POD_NAME` (set by the Kubernetes downward API in the Helm chart
shipping in v0.1.29) and falls back to `local-<pid>` for local
runs. The Lease is renewed every 10 seconds with a 30-second
expiry. Three consecutive renewal failures exit the process so the
Deployment restart picks up.

The HTTP server runs unconditionally — `/healthz` and `/readyz`
return 200 even before leadership is acquired so the pod's probes
don't flap during the (typically sub-second) acquire phase.

### What's still pending

The Helm chart for in-cluster deployment (ServiceAccount + RBAC +
Deployment + Service for `/metrics`) lands in v0.1.29 and closes
plan.md sub-phase 1.8.
