# Quickstart

This walkthrough takes you from a blank Hetzner Cloud account to a
running AppRafter Application in about 10 minutes. The path uses
the `bun-http` golden-path template; substitute another template
once you've confirmed the basics work.

## Prerequisites

| Tool          | Version | Purpose                                    |
| ------------- | ------- | ------------------------------------------ |
| Bun           | ≥ 1.x   | runs the OneBun starter; ships in the dev shell. |
| Rust          | ≥ 1.83  | builds `apprafter`.                     |
| Cargo         | (paired with Rust) | workspace builds.                |
| Docker        | ≥ 24    | builds the container image.                |
| `cue`         | ≥ 0.10  | manifest validation (in the dev shell).    |
| `kubectl`     | ≥ 1.29  | reaches the apiserver after bootstrap.     |
| Hetzner Cloud token | n/a | API token from the Cloud console.          |

The repo's `nix develop` shell pre-installs Bun, Rust, cue,
kubectl, helm, and friends. Outside Nix, install via your package
manager.

## 1. Provision the cluster

```sh
export HCLOUD_TOKEN=...                          # Hetzner Cloud API token
export APPRAFTER_SSH_PUBLIC_KEY="$(cat ~/.ssh/id_ed25519.pub)"

cd cli
cargo run --bin apprafter -- init \
    --provider hetzner-cloud --tier solo --region nbg1
cargo run --bin apprafter -- apply
# ↳ provisions a CX22 with cloud-init k3s. ~3-5 min.

cargo run --bin apprafter -- kubeconfig | tee /tmp/kc
KUBECONFIG=/tmp/kc kubectl get nodes
# ↳ Ready

cargo run --bin apprafter -- cluster-bootstrap
# ↳ Cilium + Gateway API + default-deny NP + Application CRD +
#   Argo CD + cert-manager + self-signed ClusterIssuer.
```

The full flow + opt-ins (Backstage, Argo CD Gateway, admission
webhook) is documented in [`cli/README.md`](https://github.com/apprafter/apprafter/blob/main/cli/README.md).

## 2. Scaffold an Application

Two ways to scaffold:

**A. Via Backstage UI** (operators with Backstage already up):

1. Register the template in your Backstage `app-config.yaml`:
   ```yaml
   catalog:
     locations:
       - type: url
         target: https://github.com/apprafter/apprafter/blob/main/examples/templates/bun-http/template.yaml
         rules:
           - allow: [Template]
   ```
2. In Backstage UI → **Create** → pick **Bun HTTP service (OneBun)**.
3. Fill in the form (name, namespace, owner, image tag, repo URL).
4. The scaffolder publishes a new repo with the templated source.

**B. Via copy-paste** (operators without Backstage):

```sh
mkdir my-service && cd my-service
cp -r path/to/apprafter/examples/templates/bun-http/skeleton/. .
# Search-and-replace the ${{ values.* }} placeholders by hand:
#   - ${{ values.name }}        → my-service
#   - ${{ values.namespace }}   → default
#   - ${{ values.image }}       → ghcr.io/<your-org>/my-service:0.1.0
#   - ${{ values.description }} → My first AppRafter service
#   - ${{ values.owner }}       → user:default/<your-handle>
git init && git add . && git commit -m "feat: scaffold from apprafter bun-http"
```

## 3. Build + push the image

```sh
cd my-service
bun install
docker build -t ghcr.io/<your-org>/my-service:0.1.0 .
docker push ghcr.io/<your-org>/my-service:0.1.0
```

The Dockerfile is multi-stage — `oven/bun:1-debian` builds, the
runtime is `distroless/nodejs20-debian12:nonroot`. Final image is
~30 MB.

## 4. Wire Argo CD

Push the repo to the Git remote you set as
`Infrastructure.spec.argocd.bootstrapRepo` during cluster-bootstrap.
Argo CD picks the change up; the AppRafter operator reconciles
`apprafter/Application.cue` into a Deployment + Service.

```sh
KUBECONFIG=/tmp/kc kubectl get applications.apprafter.io
# ↳ my-service in default; phase = Ready

KUBECONFIG=/tmp/kc kubectl run -it --rm curl-test \
    --image=curlimages/curl --restart=Never -- \
    curl http://my-service.default.svc.cluster.local/api/health
# ↳ {"success":true,"result":{"status":"healthy", "timestamp":"..."}}
```

## What you just got

- A typed OneBun service (`@onebun/core` decorators + DI).
- Prometheus `/metrics` (the operator scrapes via the `Service`).
- OpenTelemetry tracing (configured in `src/index.ts`).
- A v1alpha1 `Application` manifest validated by the admission
  webhook on every change.
- Per-environment overrides via `spec.environments.<env>` — set
  `APPRAFTER_ENV` on the operator pod to switch.

## Where to look next

- [`examples/templates/bun-http/README.md`](https://github.com/apprafter/apprafter/blob/main/examples/templates/bun-http/README.md) —
  starter internals (controller / module / bootstrap).
- [`schemas/v1alpha1/application.cue`](https://github.com/apprafter/apprafter/blob/main/schemas/v1alpha1/application.cue) —
  the Application CRD shape your manifest gets validated against.
- [`operator/README.md`](https://github.com/apprafter/apprafter/blob/main/operator/README.md) — operator
  reconcile loop + per-environment expansion semantics.
- [`backstage-plugins/applications-frontend/README.md`](https://github.com/apprafter/apprafter/blob/main/backstage-plugins/applications-frontend/README.md) —
  Backstage page that lists Applications + their status.
