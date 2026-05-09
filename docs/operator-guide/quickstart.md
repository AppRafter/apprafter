# Operator quickstart

You are a cluster operator standing up AppRafter on Hetzner Cloud.
This walks the manual flow that `e2e/mvp.sh` automates.

For the developer perspective ("I want to scaffold a new app"),
see [`docs/dev-guide/quickstart.md`](../dev-guide/quickstart.md).

## What you'll build

| Component         | Tier-1 baseline                                                      |
| ----------------- | -------------------------------------------------------------------- |
| Substrate         | One Hetzner CX22 (2 vCPU / 4 GB / 40 GB) in `nbg1` (or your region). |
| Network           | Hetzner private network 10.0.0.0/16, subnet 10.0.0.0/24.             |
| Firewall          | TCP 22 (SSH) + 6443 (kube API) + 80/443 (HTTP/S) + UDP 51820 (WG).   |
| Kubernetes        | k3s single-node (traefik + servicelb disabled).                      |
| CNI               | Cilium 1.16.5 (kube-proxy replacement, IPAM kubernetes).             |
| Gateway           | Gateway API CRDs + Cilium gateway.                                   |
| GitOps            | Argo CD 7.7.7 (single replicas, Dex off).                            |
| TLS               | cert-manager 1.16.2 + self-signed `apprafter-selfsigned` issuer.     |
| Application CRD   | `apprafter.io/v1alpha1.Application` (admission-validated).            |

## Prerequisites

```sh
# In the AppRafter repo's nix dev shell:
export HCLOUD_TOKEN=...                    # Hetzner API token, Read+Write
export APPRAFTER_SSH_PUBLIC_KEY="$(cat ~/.ssh/id_ed25519.pub)"
```

The dev shell ships `cargo`, `kubectl`, `helm`, `cue`, and friends.
Outside Nix, install them via your package manager.

## 1. Init + provision

```sh
cd cli
cargo run --bin platform-cli -- init \
    --provider hetzner-cloud --tier solo --region nbg1
cargo run --bin platform-cli -- apply
```

`apply` provisions an SSH key, a private network with subnet, a
firewall (default-deny inbound + the AppRafter port whitelist),
a CX22 server attached to both, and a cloud-init `#cloud-config`
that installs fail2ban and k3s (Hetzner Cloud Firewall enforces the
port whitelist at the network edge).

The first `apply` takes ~30s on the Hetzner side; the cloud-init
phase that brings k3s up takes another 3-5 minutes.

## 2. Get kubeconfig + verify the node

```sh
cargo run --bin platform-cli -- kubeconfig | tee /tmp/kc
KUBECONFIG=/tmp/kc kubectl get nodes
# ↳ <hostname>   Ready   control-plane,master   <age>   v1.31.x+k3s
```

The kubeconfig is age-encrypted in `.apprafter/state.json` after
the first fetch; subsequent calls decrypt the cache. `--refresh`
forces a re-fetch over SSH.

## 3. cluster-bootstrap

```sh
cargo run --bin platform-cli -- cluster-bootstrap
```

This installs (in order):

1. Cilium via Helm.
2. Gateway API standard CRDs (HTTPRoute, Gateway, etc.).
3. The AppRafter `Application` CRD.
4. A default-deny `NetworkPolicy` on the `default` namespace.
5. Argo CD via Helm.
6. cert-manager via Helm.
7. The self-signed `apprafter-selfsigned` ClusterIssuer.

Optional opt-ins (set in your `Infrastructure.cue` manifest then
`export APPRAFTER_MANIFEST=path/to/manifest.cue`):

- `spec.argocd.domain` → Argo CD UI is exposed via Gateway +
  HTTPRoute + Certificate.
- `spec.argocd.bootstrapRepo` → Argo CD `Application` named
  `bootstrap` watches your Git repo (everything in there auto-
  syncs into the cluster).
- `spec.backstage.domain` + `spec.backstage.image` → Backstage
  deploys with an `app-config.yaml` ConfigMap mount.
- `spec.admissionWebhook.image` → admission-webhook stack
  (cert-manager Certificate + Service + Deployment +
  ValidatingWebhookConfiguration in `apprafter-system` namespace).

See [`cli/README.md`](../../cli/README.md) for the full step list
and how each opt-in fans out.

## 4. Smoke test (manual)

```sh
KUBECONFIG=/tmp/kc kubectl apply -f - <<'EOF'
apiVersion: apps/v1
kind: Deployment
metadata: { name: hello, namespace: default, labels: { apprafter: "true" } }
spec:
  replicas: 1
  selector: { matchLabels: { app: hello } }
  template:
    metadata: { labels: { app: hello, apprafter: "true" } }
    spec:
      containers:
        - name: hello
          image: nginxdemos/hello:plain-text
          ports: [{ containerPort: 80 }]
---
apiVersion: v1
kind: Service
metadata: { name: hello, namespace: default, labels: { apprafter: "true" } }
spec:
  type: ClusterIP
  selector: { app: hello }
  ports: [{ port: 80, targetPort: 80 }]
EOF

KUBECONFIG=/tmp/kc kubectl run -it --rm curl --image=curlimages/curl --restart=Never -- \
    curl -sSf http://hello.default.svc.cluster.local/
# ↳ Server address: 10.x.x.x:80
#   Server name: hello-…
#   ...
```

For the same flow scripted, see [`e2e/mvp.sh`](../../e2e/mvp.sh).

## 5. Use the Application CRD (operator pod required)

The cluster has the CRD installed but no operator pod by default.
To exercise the full Application flow you need to:

1. Build the operator image (see [`operator/README.md`](../../operator/README.md)).
2. Push it to a registry the cluster can pull from.
3. `helm install apprafter-operator operator/charts/apprafter-operator \
       --namespace apprafter-system --create-namespace \
       --set image.repository=<your-registry>/apprafter-operator \
       --set image.tag=<your-tag>`.
4. `kubectl apply -f manifests/tier-1/application/example-app.yaml` —
   the operator reconciles it into a Deployment + Service via SSA
   and writes status (`phase=Ready`, `endpointURL=...`).

## 6. Day-2 ops

| Task                          | Command                                                        |
| ----------------------------- | -------------------------------------------------------------- |
| List Applications             | `kubectl get applications.apprafter.io -A`                     |
| Watch operator metrics        | `kubectl -n apprafter-system port-forward svc/apprafter-operator 8080:8080` then `curl http://127.0.0.1:8080/metrics` |
| Get Argo CD admin password    | `cargo run --bin platform-cli -- argocd-password`              |
| Re-fetch kubeconfig           | `cargo run --bin platform-cli -- kubeconfig --refresh`         |
| Rebuild local state           | `cargo run --bin platform-cli -- import` (live → state.json)   |
| Tear down                     | `cargo run --bin platform-cli -- destroy --yes`                |

## Where to look next

- [`docs/dev-guide/quickstart.md`](../dev-guide/quickstart.md) —
  scaffold a new Application from the bun-http template.
- [`cli/README.md`](../../cli/README.md) — every `platform-cli`
  subcommand + state model.
- [`operator/README.md`](../../operator/README.md) — operator
  reconcile loop + leader-election + per-environment expansion.
- [`schemas/v1alpha1/`](../../schemas/v1alpha1/) — the CRD CUE
  schemas that admission validates against.
