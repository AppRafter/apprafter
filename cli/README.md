# cli/

`apprafter` — Rust binary for cluster bootstrap, lifecycle
management, and tier upgrades. Subcommands: `init`, `plan`,
`apply`, `import`, `destroy`, `status`, `login`, `upgrade-tier`.

## Layout

This is a Cargo workspace with one binary crate and three library
crates:

| Crate           | Role                                                          |
| --------------- | ------------------------------------------------------------- |
| `apprafter`  | Binary: clap parsing, subcommand dispatch, `color-eyre` wiring|
| `cli-core`      | Errors, `Tier` enum, structured logging, CUE subprocess wrapper |
| `cli-state`     | `.apprafter/state.json` load/save                             |
| `cli-providers` | `Provider` trait + `DryRunProvider` (real providers in 1.2)   |

## Build

```sh
cd cli && cargo build --workspace
```

## Test

```sh
cd cli && cargo test --workspace
```

The CUE-wrapper test skips gracefully when `cue` is absent from
`PATH`. To exercise it, enter `nix develop` first.

## Run

```sh
cd cli && cargo run -- --help
cd cli && cargo run -- plan
```

Most commands still print `would …` stubs; only `apply` and
`destroy` against the Hetzner Cloud provider perform real work.

### Hetzner Cloud (real apply / destroy)

```sh
export HCLOUD_TOKEN=...   # https://docs.hetzner.cloud/#getting-started

# Optional: SSH-key boot (server skips the random root password).
export APPRAFTER_SSH_PUBLIC_KEY="$(cat ~/.ssh/id_ed25519.pub)"

# Optional: read network / firewall / server-type / image from a
# CUE Infrastructure manifest. Without this, hardcoded defaults
# are used (10.0.0.0/16 net, SSH 22 + HTTPS 443 firewall, cpx22,
# ubuntu-24.04).
export APPRAFTER_MANIFEST=examples/infrastructure/tier-1-hetzner.cue

cd cli
cargo run --bin apprafter -- init --provider hetzner-cloud --tier solo --region nbg1
cargo run --bin apprafter -- apply
cargo run --bin apprafter -- destroy --yes
```

`apply` provisions one private network (10.0.0.0/16 with a
10.0.0.0/24 cloud subnet in `eu-central`), one cloud-side firewall
(whitelisting 22 + 6443 + 80 + 443 / tcp + 51820 / udp inbound —
ssh, kube API, HTTP, HTTPS, wireguard), and one CPX22 server
attached to both. The server is provisioned with a cloud-init
`#cloud-config` payload that:

- installs `fail2ban` (log-driven IP-ban, watches sshd today, workload
  logs as we expose Gateway/HTTPRoute later);
- runs the canonical `get.k3s.io` installer with
  `--disable=traefik --disable=servicelb` (Cilium + Gateway API
  replace them in phase 1.4).

The host-level allow-list (22 + 6443 + 80 + 443 / tcp + 51820 / udp,
default-deny everything else) is enforced by the Hetzner Cloud Firewall
at the network edge — see `build_firewall_spec` in
`cli/apprafter/src/commands/apply.rs`. Earlier versions also installed
ufw inside the VM as defense-in-depth, but it was removed in v0.1.43
(silent failure on Ubuntu 24.04 cloud-init runcmd; see changelog).

The second `apply` is a no-op (idempotent — server name + apprafter
label match). `destroy` requires `--yes` and tears everything down
(floating IPs → server → firewall → network → SSH key).

If the manifest declares `network.floatingIPs: [...string]`, each
name is provisioned as an `ipv4` Hetzner Floating IP attached to
the cluster server (so egress traffic exits with that fixed
address). The reserved IPs are also tagged `apprafter=true`, which
keeps `apply` idempotent across re-runs and makes them visible to
`destroy`.

### Recovering state with `import`

If `.apprafter/state.json` is lost (or you cloned the repo on a new
machine), `apprafter import` rebuilds the Hetzner section by
scanning the live API for resources tagged with `apprafter=true`:

```sh
export HCLOUD_TOKEN=...
cargo run --bin apprafter -- import --dry-run   # preview only
cargo run --bin apprafter -- import             # write state
cargo run --bin apprafter -- import --force     # overwrite an
                                                   # existing snapshot
```

`import` is read-only (no `create_*` / `delete_*` calls) and refuses
to overwrite an already-populated `state.hetzner_cloud` unless you
pass `--force`. It picks the server whose name matches
`state.cluster_name` (default `platform-1`); if no labelled server
matches, it prints a friendly message and writes nothing.

`import` also fills the `HetznerCloudState.server_type` fact field
from the live server — so a state file rebuilt with `import` carries
the correct type for drift detection and reproduction. However,
**`import` is not the backfill path** for an old state file that
merely lacks the field: `--dry-run` only prints and without `--force`
it won't overwrite an existing `state.hetzner_cloud`. The normal
backfill for an existing target is a plain `apprafter apply` — on the
first `apply` after upgrading to a CLI version that tracks server type,
the reconcile path reads the live server and fills the field in place,
best-effort (a read-only config dir warns and continues).

### Reading kubeconfig from the cluster

After `apply` finishes (the cloud-init bootstrap on the new server
takes ~3-5 minutes — be patient on the first call), retrieve the
kubeconfig:

```sh
export HCLOUD_TOKEN=...
export APPRAFTER_SSH_PRIVATE_KEY="$HOME/.ssh/id_ed25519"  # optional, default
export APPRAFTER_AGE_KEY="$HOME/.config/apprafter/age.key" # optional, default; auto-created on first run, mode 0600

cargo run --bin apprafter -- kubeconfig | KUBECONFIG=/dev/stdin kubectl get nodes
```

The command shells out to `ssh root@<public-ip> cat
/etc/rancher/k3s/k3s.yaml`, rewrites the `server:` URL from the
loopback to the server's public IPv4, encrypts the result with age
under the on-disk identity, caches the armored ciphertext in
`.apprafter/state.json` (`hetzner_cloud.kubeconfig_age`), and
prints the plaintext on stdout. Subsequent calls decrypt the cache
in O(1); pass `--refresh` to force a re-fetch.

State files written by v0.1.9 (plaintext `kubeconfig_yaml` field)
are still readable: the v0.1.9 entry is treated as a one-cycle
legacy fallback. The next `--refresh` migrates the entry forward
to the age field and clears the plaintext slot.

### Reading the Argo CD admin password

After `cluster-bootstrap` finishes (Argo CD pods need a moment to
generate their initial admin secret), retrieve the password:

```sh
cargo run --bin apprafter -- argocd-password
```

First call: decrypts the cached kubeconfig from state, runs
`kubectl get secret argocd-initial-admin-secret -n argocd -o
jsonpath='{.data.password}'`, base64-decodes the value, encrypts
the plaintext with the same age identity used for kubeconfig, and
caches the armored ciphertext in
`state.hetzner_cloud.argocd_admin_password_age`. Subsequent calls
decrypt the cache instantly. Pass `--refresh` to force a re-fetch
(useful after a manual `kubectl patch secret …` rotation).

The cluster-bootstrap step uses Argo CD's default randomly-generated
admin password. Best practice on first login: change it via the UI
and re-run `argocd-password --refresh`.

### Bootstrapping the cluster

After `kubeconfig` works (so the cluster is reachable), install the
in-cluster components:

```sh
cargo run --bin apprafter -- cluster-bootstrap
```

Under the hood the command:

1. Decrypts the cached kubeconfig (age) from state and writes it to
   a tempfile.
2. Adds the upstream Cilium Helm repo and runs
   `helm upgrade --install cilium cilium/cilium --version <pinned>
   --namespace kube-system --create-namespace --wait` against tier-1
   values (kube-proxy replacement, IPAM kubernetes mode, Hubble off,
   single operator replica).
3. Applies the upstream Gateway API `standard-install.yaml` for the
   pinned version (`v1.2.1`) so `Gateway` / `HTTPRoute` /
   `GRPCRoute` / `ReferenceGrant` resources pass admission.
4. Applies the AppRafter `Application` CRD (group
   `apprafter.io`, version `v1alpha1`, scope namespaced). The CRD
   carries an OpenAPI v3 schema mirroring the v0.1.21 CUE
   `#Application`: `image` non-empty, `replicas` ≥ 0, `expose.port`
   1..=65535, `expose.network` enum {public,internal,vpn}, `env`
   string→string, plus the `environments` map of overrides.
   Stronger CUE-shaped admission lands with the v0.1.23 webhook.
5. Applies a default-deny `NetworkPolicy` to the `default`
   namespace (kube-system is intentionally exempt — Cilium and
   Gateway API system pods need free egress).
6. Adds the upstream Argo Helm repo and runs
   `helm upgrade --install argocd argo/argo-cd --version <pinned>
   --namespace argocd --create-namespace --wait` against tier-1
   values (single replicas across all sub-charts, Dex off, Redis-HA
   off, notifications off). The Argo CD server runs as ClusterIP —
   the HTTPRoute exposure path lands in v0.1.16.
7. Adds the upstream Jetstack Helm repo and runs
   `helm upgrade --install cert-manager jetstack/cert-manager
   --version <pinned> --namespace cert-manager --create-namespace
   --wait` against tier-1 values (installCRDs: true, single
   replicas across controller / webhook / cainjector, Prometheus
   off).
8. Applies a self-signed `ClusterIssuer` named
   `apprafter-selfsigned`. Future cycles' `Gateway` HTTPRoutes
   reference this issuer by name to mint TLS certs (no DNS
   validation needed for tier-1).
9. **(Optional)** When the `Infrastructure` manifest declares
   `spec.argocd.domain`, applies a `Gateway` (HTTPS on 443 +
   hostname + TLS terminate), an `HTTPRoute` to `argocd-server`,
   and a cert-manager `Certificate` issued by
   `apprafter-selfsigned`. Without the manifest opt-in, Argo CD
   stays ClusterIP-only — the bootstrap finishes after step 8.
10. **(Optional)** When the manifest also declares
    `spec.argocd.bootstrapRepo`, applies an Argo CD `Application`
    named `bootstrap` that watches the Git repo at
    `bootstrapRepo` (path defaults to `.`, override with
    `bootstrapPath`). Auto-prune + self-heal are on, so committing
    to the repo continuously syncs the platform manifests into the
    cluster.
11. **(Optional)** When the manifest declares
    `spec.backstage.domain`, applies the tier-1 Backstage manifest
    set (Namespace + ConfigMap + Deployment + Service + HTTPRoute +
    Gateway + cert-manager Certificate) to the `backstage`
    namespace. The ConfigMap carries an `app-config.yaml` mounted
    at `/app/app-config.yaml` (subPath, read-only) — it overrides
    whatever the operator baked into their image and pins the
    `guest` auth provider with `dangerouslyAllowOutsideDevelopment:
    true` (Backstage's basic-admin stub). `spec.backstage.image`
    overrides the placeholder container image. Without the
    manifest opt-in, Backstage is skipped — the bootstrap finishes
    after step 10.
12. **(Optional)** When the manifest declares
    `spec.admissionWebhook.image`, applies the admission-webhook
    stack (Namespace + cert-manager Certificate + Service +
    Deployment + ValidatingWebhookConfiguration) to the
    `apprafter-system` namespace. The Deployment runs the operator-
    built image; cert-manager issues a TLS Secret named
    `admission-webhook-tls`, the ValidatingWebhookConfiguration's
    `cert-manager.io/inject-ca-from` annotation keeps `caBundle` in
    sync, and the webhook validates `apprafter.io/v1alpha1`
    Application objects on CREATE+UPDATE beyond the OpenAPI v3
    layer. Without the manifest opt-in, the webhook is skipped —
    Application validation falls back to the CRD's OpenAPI v3
    schema alone.

> The container image referenced by `spec.backstage.image` is
> built outside this CLI — see
> [`backstage-plugins/host/README.md`](../backstage-plugins/host/README.md)
> for the scaffold + Dockerfile + push workflow.

> The container image referenced by `spec.admissionWebhook.image`
> is built from the operator workspace — see
> [`operator/README.md`](../operator/README.md) and
> [`manifests/tier-1/admission-webhook/README.md`](../manifests/tier-1/admission-webhook/README.md).

Both shell-outs require `helm` and `kubectl` on `$PATH`.

To verify the install end-to-end against a live cluster:

```sh
APPRAFTER_K8S_SMOKE=1 \
  cargo test -p apprafter --test cluster_smoke_test smoke -- --ignored
```

The smoke test asserts `cilium status` is green, a minimal Gateway
passes server-side admission, and the default-deny NetworkPolicy is
present. It expects `KUBECONFIG` to be exported (e.g.
`KUBECONFIG=$(apprafter kubeconfig | tee /tmp/kc; echo /tmp/kc)`).

Run the real-Hetzner integration test manually:

```sh
APPRAFTER_HCLOUD_E2E=1 \
HCLOUD_TOKEN=... \
cargo test -p cli-providers --test hetzner_cloud_test \
    e2e_real_hetzner_test -- --ignored
```
