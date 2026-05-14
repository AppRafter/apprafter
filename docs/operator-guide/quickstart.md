# Operator quickstart

You are a cluster operator standing up AppRafter on Hetzner Cloud.
This walks the manual flow that `e2e/mvp.sh` automates.

For the developer perspective ("I want to scaffold a new app"),
see [`docs/dev-guide/quickstart.md`](../dev-guide/quickstart.md).

## What you'll build

| Component         | Tier-1 baseline                                                      |
| ----------------- | -------------------------------------------------------------------- |
| Substrate         | One Hetzner CPX22 (2 vCPU / 4 GB / 40 GB) in `nbg1` (or your region).|
| Network           | Hetzner private network 10.0.0.0/16, subnet 10.0.0.0/24.             |
| Firewall          | TCP 22 (SSH) + 6443 (kube API) + 80/443 (HTTP/S) + UDP 51820 (WG).   |
| Kubernetes        | k3s single-node (traefik + servicelb disabled).                      |
| CNI               | Cilium 1.16.5 (kube-proxy replacement, IPAM kubernetes).             |
| Gateway           | Gateway API CRDs + Cilium gateway.                                   |
| GitOps            | Argo CD 7.7.7 (single replicas, Dex off).                            |
| TLS               | cert-manager 1.16.2 + self-signed `apprafter-selfsigned` issuer.     |
| Application CRD   | `apprafter.io/v1alpha1.Application` (admission-validated).           |

## Prerequisites

```sh
# In the AppRafter repo's nix dev shell:
nix develop                              # ships cargo, kubectl, helm, cue, ...
cargo install --path cli/platform-cli    # puts `apprafter` on PATH
```

You'll also need:

- A Hetzner Cloud API token with Read+Write access.
- An SSH key whose **public** half you'll hand to the provider for
  the new node. The CLI never touches the private half.

The rest of this page assumes `apprafter` is on PATH. If it's not,
substitute `cargo run --bin apprafter -- <subcommand>` everywhere
and run the commands from `cli/`.

## 1. Configure a target

A **target** bundles `(provider, region, credentials, defaults)`
under a name. One command saves it; future commands reuse it.

```sh
apprafter target add prod \
    --provider hetzner-cloud \
    --token  "<your-hcloud-token>" \
    --region nbg1 \
    --tier   solo \
    --ssh-key ~/.ssh/id_ed25519.pub
```

The wizard auto-fills any flag you skip. On a TTY without
`--no-interactive` you can run `apprafter target add prod` alone
and answer the prompts; the wizard validates the token against
`GET /v1/locations` before saving.

First target on a fresh store is auto-activated. To check:

```sh
apprafter target list           # or `apprafter t ls`
apprafter target show           # or `apprafter t info`
apprafter whoami                # one-line identity + active target
```

Stored under `~/.config/apprafter/targets/prod/`:

- `config.yaml` — provider, region, tier, cluster_name, ssh_key_path.
- `credentials.yaml` — the API token. **Mode 0600**. The CLI never
  echoes the value in `show`/`whoami` output; read the file
  directly if you need the raw bytes.

See [`target-store.md`](./target-store.md) for the full file
layout and the credential resolution chain (flag → env → store).

## 2. One-command provisioning

Bring the entire tier-1 stack up in one shot:

```sh
apprafter bootstrap-all         # or `apprafter up`
```

This runs three phases under a unified progress UX:

1. **`apply`** — provisions the SSH key, private network, firewall,
   the CPX22 server, and a `#cloud-config` user-data block that
   installs fail2ban + k3s. ~30 s on the Hetzner side; cloud-init
   needs another 90–180 s after that.
2. **`k3s-ready` (poll)** — waits for cloud-init + k3s to finish
   on the new node, then retrieves the kubeconfig.
   Implementation: `ssh root@<node> cat /etc/rancher/k3s/k3s.yaml`
   with `ConnectTimeout=5`, retried every 10 s for up to 5
   minutes; the YAML lands age-encrypted in
   `.apprafter/state.json`. Typical Phase-2 duration on Hetzner
   `cpx22` + Ubuntu 24.04 is 20–40 s — most of it is the cluster
   booting, not the kubeconfig copy.
3. **`cluster-bootstrap`** — installs Cilium + Gateway API CRDs +
   the AppRafter Application CRD + default-deny NetworkPolicy +
   Argo CD + cert-manager + the self-signed ClusterIssuer +
   apprafter-operator + admission-webhook.

Preview before spending a Hetzner cent:

```sh
apprafter up --dry-run
```

The dry-run prints the resolved target name, every field from
`config.yaml`, and the three-phase plan with budgets. No provider
calls, safe in any directory.

Each phase still has its own subcommand for partial re-runs:

```sh
apprafter apply                 # Phase 1 alone
apprafter kubeconfig --refresh  # Phase 2 alone (force re-fetch)
apprafter cluster-bootstrap     # Phase 3 alone (re-runs `helm upgrade --install`)
apprafter cb                    # alias for cluster-bootstrap
```

## 3. Verify

```sh
apprafter doctor                # self-diagnostic, exits 1 on FAIL

apprafter kubeconfig | tee /tmp/kc
KUBECONFIG=/tmp/kc kubectl get nodes
# ↳ <hostname>   Ready   control-plane,master   <age>   v1.31.x+k3s

KUBECONFIG=/tmp/kc kubectl get applications.apprafter.io -A
KUBECONFIG=/tmp/kc kubectl -n argocd get pods
```

`doctor` walks the active target's stored config, credentials, and
reachability checks plus the surrounding shell environment
(`kubectl`, `helm`, `ssh`, DNS). Each check reports PASS / WARN /
FAIL with a hint pointing at the right next command.

## 4. Day-2 ops

| Task                          | Command                                                |
| ----------------------------- | ------------------------------------------------------ |
| List Applications             | `kubectl get applications.apprafter.io -A`             |
| Argo CD admin password        | `apprafter argocd-password`                            |
| Re-fetch kubeconfig over SSH  | `apprafter kubeconfig --refresh`  (alias: `apprafter kc --refresh`) |
| Rebuild local state           | `apprafter import`  (live Hetzner → state.json)        |
| Switch active target          | `apprafter target use <name>`  (alias: `apprafter t use`) |
| Rotate the Hetzner token      | `apprafter target add <name> --renew --token <new>`    |
| Inspect target config         | `apprafter target show`  (alias: `apprafter t info`)   |
| Tear down                     | `apprafter destroy --yes`                              |

The credential resolution chain (flag → env → target store) means
all of the above work without an explicit `HCLOUD_TOKEN` export
once the target is configured. CI keeps the env-var path working
unchanged.

## 5. Use the Application CRD

From v0.1.64 onwards the AppRafter operator and admission-webhook
are installed by `apprafter cluster-bootstrap` (and therefore
`apprafter up`) by default. Apply an Application CR and the
operator reconciles it into a Deployment + Service via SSA, writes
status (`phase=Ready`, `endpointURL=...`), and the
admission-webhook gates the create/update payload.

```sh
KUBECONFIG=/tmp/kc kubectl apply -f manifests/tier-1/application/example-app.yaml
KUBECONFIG=/tmp/kc kubectl get applications.apprafter.io parser -n default \
    -o jsonpath='{.status.phase}'   # → Ready
KUBECONFIG=/tmp/kc kubectl get deployment parser -n default
```

To skip either component, opt out in your `Infrastructure.cue`
manifest:

```cue
spec: {
    operator?:        { enabled: false }   // skip operator helm release
    admissionWebhook?: { enabled: false }  // skip admission-webhook
}
```

For fork / dev builds, override the images:

```cue
spec: {
    operator?: {
        image: "ghcr.io/my-fork/apprafter-operator"
        tag:   "dev"
    }
}
```

See [`schemas/v1alpha1/infrastructure.cue`](../../schemas/v1alpha1/infrastructure.cue)
for the full block reference and
[`gitops-walk.md`](./gitops-walk.md) for the Argo CD + repo-creds
walk.

## 6. When things go wrong

Each error renders with a stable `apprafter::<area>::<reason>`
diagnostic code and a multi-line `help:` block pointing at the
next-step command. Examples:

```text
Error: apprafter::target::not_found
  × target `ghost` not found (available: prod)
  help: Either the `--target` flag was given a name that's not
        in the store, or no target has been created yet. List
        existing targets with `apprafter target list`; create a
        new one with `apprafter target add <name> --provider
        hetzner-cloud …`.
```

```text
Error: apprafter::target::token_rejected
  × provider `hetzner-cloud` rejected the supplied token
  ╰─▶ apprafter::provider::hetzner_api_error
        × hetzner-cloud GET /v1/locations failed (status 401): …
        help: …  (401 / 403 / 429 / 5xx breakdown)
  help: …  (rotation flow: --renew, --no-ping)
```

See [`troubleshooting.md`](./troubleshooting.md) for the full
diagnostic-code catalogue.

For `NO_COLOR` / CI consumers: set `NO_COLOR=1`. Output stays
byte-identical to v0.1.85's pre-colour baseline.

## Where to look next

- [`target-store.md`](./target-store.md) — target store layout +
  credential resolution chain reference.
- [`troubleshooting.md`](./troubleshooting.md) — diagnostic-code
  catalogue, common failures, recovery commands.
- [`docs/reference/cli.md`](../reference/cli.md) — full subcommand
  reference with every flag + alias.
- [`docs/dev-guide/quickstart.md`](../dev-guide/quickstart.md) —
  scaffold a new Application from the bun-http template.
- [`operator/README.md`](../../operator/README.md) — operator
  reconcile loop + leader-election + per-environment expansion.
- [`schemas/v1alpha1/`](../../schemas/v1alpha1/) — the CRD CUE
  schemas that admission validates against.
