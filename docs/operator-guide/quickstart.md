# Operator quickstart

You are a cluster operator standing up AppRafter on Hetzner Cloud.
This page covers the **three-step CLI flow** that gets you from a
blank Hetzner account to a self-managing cluster in one session.

For the developer perspective ("I want to deploy my first app"),
see [`docs/dev-guide/quickstart.md`](../dev-guide/quickstart.md).

## What you will build

| Component         | Tier-1 baseline                                                        |
| ----------------- | ---------------------------------------------------------------------- |
| Substrate         | One Hetzner CPX22 (2 vCPU / 4 GB / 40 GB) in `nbg1` (or your region). |
| Network           | Hetzner private network 10.0.0.0/16, subnet 10.0.0.0/24.              |
| Firewall          | TCP 22 (SSH) + 6443 (kube API) + 80/443 (HTTP/S) + UDP 51820 (WG).    |
| Kubernetes        | k3s single-node (traefik + servicelb disabled).                        |
| CNI               | Cilium 1.16.5 (kube-proxy replacement, IPAM kubernetes).               |
| Gateway           | Gateway API CRDs + Cilium gateway.                                     |
| GitOps            | Argo CD 7.7.7 (single replicas, Dex off).                              |
| TLS               | cert-manager 1.16.2 + self-signed `apprafter-selfsigned` issuer.       |
| Application CRD   | `apprafter.io/v1alpha1.Application` (admission-validated).             |

**Important:** the platform does not stop at Argo CD installation. After
`bootstrap-all` completes, Argo CD adopts the platform stack itself — Cilium,
cert-manager, the AppRafter operator, the admission webhook — and reconciles it
from a versioned OCI chart. You do not install the operator by hand; the
platform installs and upgrades itself through GitOps.

## Prerequisites

```sh
# In the AppRafter repo's nix dev shell:
nix develop                              # ships cargo, kubectl, helm, cue, ...
cargo install --path cli/platform-cli    # puts `apprafter` on PATH
```

You will also need:

- A Hetzner Cloud API token with Read+Write access.
- An SSH key whose **public** half you will hand to the provider for
  the new node. The CLI never touches the private half.

The rest of this page assumes `apprafter` is on PATH. If it is not,
substitute `cargo run --bin apprafter -- <subcommand>` everywhere
and run the commands from `cli/`.

## Step 1 — Configure a target

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
the Hetzner API before saving.

The first target on a fresh store is auto-activated. To check:

```sh
apprafter target list           # (alias: apprafter t ls)
apprafter target show           # (alias: apprafter t info)
apprafter whoami                # one-line identity + active target
```

Credentials are stored in `~/.config/apprafter/targets/prod/` at
mode 0600. The CLI never echoes the token value in `show`/`whoami`
output. See [`target-store.md`](./target-store.md) for the full
file layout and the credential resolution chain (flag → env → store).

## Step 2 — Bring the cluster up

Run one command to provision and bootstrap the entire tier-1 stack:

```sh
apprafter bootstrap-all         # (alias: apprafter up)
```

This runs three phases under a unified progress display:

1. **`apply`** — provisions the SSH key, private network, firewall,
   the CPX22 server, and a `#cloud-config` user-data block that
   installs fail2ban + k3s. Around 30 s on the Hetzner side;
   cloud-init needs another 90–180 s after that.
2. **`k3s-ready` (poll)** — waits for cloud-init + k3s to finish
   on the new node, then retrieves the kubeconfig over SSH.
   The kubeconfig lands age-encrypted in `.apprafter/state.json`.
3. **`cluster-bootstrap`** — installs Argo CD (the bootstrap
   loader), then applies a root Argo CD `Application` that points
   at the platform-stack OCI chart. Argo CD reconciles all remaining
   platform components — Cilium, Gateway API CRDs, the AppRafter
   Application CRD, default-deny NetworkPolicy, cert-manager,
   self-signed ClusterIssuer, apprafter-operator, and the
   admission webhook — from that chart without further CLI
   intervention.

Preview before spending a Hetzner cent:

```sh
apprafter up --dry-run
```

The dry-run prints the resolved target name, every field from
`config.yaml`, and the three-phase plan. No provider calls.

Each phase also has its own subcommand for partial re-runs:

```sh
apprafter apply                 # Phase 1 alone
apprafter kubeconfig --refresh  # Phase 2 alone (force re-fetch over SSH)
apprafter cluster-bootstrap     # Phase 3 alone (re-runs the loader)
apprafter cb                    # alias for cluster-bootstrap
```

## Step 3 — Verify

```sh
apprafter doctor                # self-diagnostic, exits 1 on FAIL
```

`doctor` walks the active target's stored config, credentials, and
reachability checks plus the surrounding shell environment
(`kubectl`, `helm`, `ssh`, DNS). Each check reports PASS / WARN /
FAIL with a hint pointing at the right next command.

Export the kubeconfig and check the cluster:

```sh
apprafter kubeconfig | tee /tmp/kc
export KUBECONFIG=/tmp/kc

kubectl get nodes
# <hostname>   Ready   control-plane,master   <age>   v1.31.x+k3s

kubectl -n argocd get pods
# argocd-server-...   Running

kubectl get applications.apprafter.io -A
# (empty until you deploy your first Application)
```

### Open the Argo CD UI

```sh
apprafter open argocd
```

This command starts a local port-forward to Argo CD, prints the
admin username and password, and opens your default browser at
`https://localhost:8080`. No separate `kubectl port-forward` or
`apprafter argocd-password` dance is needed.

In the UI you will see the platform-stack Argo CD `Application`
and its child components. Each should be **Synced** and **Healthy**
within a few minutes of `cluster-bootstrap` completing.

### Smoke: apply your first Application CR

The AppRafter operator and admission webhook are installed by
`cluster-bootstrap` as part of the platform stack. Verify the
end-to-end path by applying an `Application` CR:

```sh
kubectl apply -f manifests/tier-1/application/example-app.yaml

kubectl get applications.apprafter.io parser -n default \
    -o jsonpath='{.status.phase}'
# → Ready

kubectl get deployment parser -n default
# parser   1/1   1   ...
```

The operator reconciles the `Application` CR into a Deployment and
Service via server-side apply. The status field `phase=Ready` and
`endpointURL` are written by the operator after a successful
reconcile. The admission webhook validates the create and update
payloads against the CRD schema and cross-field invariants.

To opt out of the operator or webhook in a custom `Infrastructure.cue`:

```cue
spec: {
    operator?:        { enabled: false }   // skip operator helm release
    admissionWebhook?: { enabled: false }  // skip admission-webhook
}
```

## Day-2 operations

| Task                          | Command                                                  |
| ----------------------------- | -------------------------------------------------------- |
| Open Argo CD UI               | `apprafter open argocd`                                  |
| List Applications             | `kubectl get applications.apprafter.io -A`               |
| Argo CD admin password only   | `apprafter argocd-password`                              |
| Re-fetch kubeconfig           | `apprafter kubeconfig --refresh` (alias: `apprafter kc --refresh`) |
| Rebuild local state           | `apprafter import` (live Hetzner → state.json)           |
| Switch active target          | `apprafter target use <name>` (alias: `apprafter t use`) |
| Rotate the Hetzner token      | `apprafter target add <name> --renew --token <new>`      |
| Inspect target config         | `apprafter target show` (alias: `apprafter t info`)      |
| Platform version / status     | `apprafter platform status`                              |
| Upgrade platform              | `apprafter platform upgrade --to <version>`              |
| Tear down                     | `apprafter destroy --yes`                                |

The credential resolution chain (flag → env → target store) means
all of the above work without an explicit `HCLOUD_TOKEN` export
once the target is configured. CI keeps the env-var path working
unchanged.

## When things go wrong

Each error renders with a stable `apprafter::<area>::<reason>`
diagnostic code and a multi-line `help:` block. Examples:

```text
Error: apprafter::target::not_found
  × target `ghost` not found (available: prod)
  help: Either the `--target` flag was given a name that is not
        in the store, or no target has been created yet. List
        existing targets with `apprafter target list`; create a
        new one with `apprafter target add <name> --provider
        hetzner-cloud ...`.
```

Set `NO_COLOR=1` for CI / pipe consumers. Output stays
byte-identical to the pre-colour baseline.

## Where to look next

- [`target-store.md`](./target-store.md) — target store layout +
  credential resolution chain reference.
- [`troubleshooting.md`](./troubleshooting.md) — diagnostic-code
  catalogue, common failures, recovery commands.
- [`gitops-walk.md`](./gitops-walk.md) — wiring Argo CD to a Git
  repository for GitOps deployment of your applications.
- [`platform-management.md`](./platform-management.md) — platform
  version lifecycle, release channels, upgrade and freeze.
- [`docs/reference/cli.md`](../reference/cli.md) — full subcommand
  reference with every flag + alias.
- [`docs/dev-guide/quickstart.md`](../dev-guide/quickstart.md) —
  scaffold and deploy a first Application.
- `schemas/v1alpha1/` — the CRD CUE schemas that admission validates
  against.
