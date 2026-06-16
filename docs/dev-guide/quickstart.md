# Quickstart

This walkthrough is the full developer path on AppRafter: from
registering a deployment target, through scaffolding and shipping an
application, to serving it in production on a public domain through
Cloudflare. Everything runs through the `apprafter` CLI. Budget ~15
minutes of hands-on time, plus DNS-propagation wait at the end.

## Prerequisites

| Tool          | Version | Purpose                                              |
| ------------- | ------- | ---------------------------------------------------- |
| `apprafter`   | latest  | the CLI; `cargo install --path cli/platform-cli` puts it on PATH. |
| Bun           | ≥ 1.x   | runs the OneBun starter; ships in the dev shell.     |
| Docker        | ≥ 24    | builds the container image.                          |
| `cue`         | ≥ 0.10  | local manifest validation (in the dev shell).        |
| `kubectl`     | ≥ 1.29  | reaches the apiserver after bootstrap.               |
| Hetzner Cloud token | n/a | API token from the Cloud console.                  |
| Cloudflare account + a domain | n/a | for the public-ingress step.             |

The repo's `nix develop` shell pre-installs Bun, Rust, cue, kubectl,
helm, and friends. Build the CLI once with `cargo install --path
cli/platform-cli` so `apprafter` is on your PATH; the rest of this
page assumes it is. (Outside Nix, install the tools via your package
manager.)

## 1. Register a target and bring the cluster up

A **target** is a named bundle of provider, region, and credentials.
Save one, then bring the whole tier-1 stack up with a single command:

```sh
apprafter target add prod \
    --provider hetzner-cloud \
    --token  "<your-hcloud-token>" \
    --region nbg1 \
    --tier   solo \
    --ssh-key ~/.ssh/id_ed25519.pub

apprafter up          # alias: apprafter bootstrap-all
# ↳ provisions a CPX22 with cloud-init k3s, waits for the node,
#   then bootstraps Cilium + Gateway API + Argo CD + the platform
#   stack (operator, admission webhook, cert-manager). ~3 min.

apprafter doctor                 # self-diagnostic; exits 1 on FAIL
```

`up` runs `apply` → kubeconfig poll → `cluster-bootstrap`
under one progress display. Preview it first with
`apprafter up --dry-run`, or run the phases individually with
`apprafter apply`, `apprafter kubeconfig --refresh`, and
`apprafter cluster-bootstrap` (alias `cb`). The full lifecycle and
day-2 commands are in the
[operator quickstart](../operator-guide/quickstart.md).

Point `kubectl` at the new cluster:

```sh
apprafter kubeconfig | tee /tmp/kc
export KUBECONFIG=/tmp/kc
kubectl get nodes                # ↳ Ready
```

## 2. Scaffold your application

Work from the root of your application repository — a `Dockerfile`
plus a supported runtime (Bun, Node, Python, Rust, or Go). Let the
CLI detect the runtime and generate an `apprafter/Application.cue`
manifest for you:

```sh
git init
git remote add origin https://github.com/<your-org>/my-service.git

apprafter app scaffold --runtime bun --name my-service
# ↳ detects bun.lock and writes apprafter/Application.cue. It
#   pre-fills the image ref from your git origin — edit `image:` if
#   your registry path differs. Add --needs pg|redis|disk to declare
#   a managed dependency.

apprafter app validate           # local cue render check (needs cue on PATH)
git add . && git commit -m "feat: scaffold apprafter manifest"
```

> **No app yet?** The `bun-http` example under
> [`examples/templates/bun-http/`](https://github.com/apprafter/apprafter/blob/main/examples/templates/bun-http/README.md)
> is a runnable OneBun service (Bun.js + Effect.ts) you can copy as a
> starting point.

## 3. Build and push the image

```sh
bun install
docker build -t ghcr.io/<your-org>/my-service:0.1.0 .
docker push ghcr.io/<your-org>/my-service:0.1.0
```

The bun-http Dockerfile is multi-stage — `oven/bun:1-debian` builds,
the runtime is `distroless/nodejs20-debian12:nonroot`. Final image is
~30 MB. Pushing a moved tag re-rolls the deployment automatically; the
build → push → redeploy iteration loop is covered in
[`image-iteration.md`](./image-iteration.md).

## 4. Deploy and verify (internal)

Push the repo, then register it with one command. Argo CD tracks the
repo, the CUE CMP compiles `apprafter/Application.cue` on every sync,
and the AppRafter operator reconciles the result into a Deployment +
Service. By default `expose.network` is `internal`, so the app is
ClusterIP-only at this point:

```sh
git push -u origin main

apprafter app add                # auto-detects the git origin
# ↳ for a private repo, register credentials first with
#   `apprafter repo creds add`.
```

Watch it converge with the simple `app` commands — no raw `kubectl`
needed (each takes the logical app name, `my-service`):

```sh
apprafter app status my-service  # sync state, health, source, history
apprafter app logs my-service -f # stream workload logs
apprafter app open my-service    # port-forward + open in a browser
```

`apprafter app list` shows every app you've registered; `apprafter
app rollback my-service` reverts to the previous revision.

## 5. Release to production on a Cloudflare domain

The production path serves your app over HTTPS through Cloudflare:
TLS terminates at the cluster Gateway on a Cloudflare Origin CA
certificate, and the node's `80`/`443` are firewalled so the only way
in is through Cloudflare. This summarizes the
[connect-a-domain runbook](../public-ingress/connect-a-domain.md) —
see it for the full DNS detail.

**5.1 — Lock the origin firewall to Cloudflare (once per cluster):**

```sh
apprafter target firewall cloudflare-origin enable
# ↳ restricts inbound 80/443 to Cloudflare's published IP ranges.
#   SSH, the Kubernetes API, and WireGuard keep their access.
```

**5.2 — Point the domain at Cloudflare and import the origin cert.**
In the Cloudflare dashboard add the site for `<zone>`, set your
registrar's nameservers to Cloudflare's, and set **SSL/TLS → Full
(strict)**. Mint a **Cloudflare Origin CA** certificate for `<zone>`
+ `*.<zone>` (full steps:
[Cloudflare Origin CA certificate](../public-ingress/cloudflare-origin-cert.md))
and import it:

```sh
apprafter target cert import cf-origin-cert-<sanitized-zone> \
    --cert ./origin.pem --key ./origin.key
# ↳ <sanitized-zone> = zone with dots as dashes, e.g.
#   cf-origin-cert-example-com for example.com.
```

**5.3 — Register the zone** (adds an apex + wildcard `:443` listener
pair on the Gateway, both terminating TLS from that cert):

```sh
apprafter target domain add <zone> --cert cf-origin-cert-<sanitized-zone>
```

**5.4 — DNS records.** Get the node's public IPs and add **Proxied**
(orange-cloud) records in Cloudflare's DNS for the zone:

```sh
apprafter target ip              # prints the A (IPv4) + AAAA (IPv6) values
```

| Type  | Name  | Value         |
|-------|-------|---------------|
| A     | `@`   | `<node-IPv4>` |
| AAAA  | `@`   | `<node-IPv6>` |
| CNAME | `www` | `<zone>`      |

**5.5 — Make the application public.** In
`apprafter/Application.cue`, flip the existing `expose` block to
`public` and set the hostname:

```cue
spec: base: expose: {
    port:     8080              // your service's listen port
    network:  "public"         // was "internal"
    hostname: "<zone>"         // or a subdomain, e.g. "app.<zone>"
}
```

TLS is on by default for public services (the route attaches to
`:443`). Commit and push — Argo CD re-syncs and the operator renders
an HTTPRoute binding the host to your Service:

```sh
git commit -am "feat: expose my-service on <zone>"
git push
apprafter app status my-service  # watch it return to Synced + Healthy
```

**5.6 — Verify** the app serves through Cloudflare, and that the
origin firewall blocks a direct-to-node bypass:

```sh
curl -v https://<zone>/                       # served by your app via Cloudflare
apprafter target domain list                  # registered zones + the apps using each
curl --resolve <zone>:443:<node-ip> https://<zone>/   # refused / times out
```

## What you just got

- A typed OneBun service (`@onebun/core` decorators + DI), with
  Prometheus `/metrics` (the operator scrapes it) and OpenTelemetry
  tracing.
- A v1alpha1 `Application` manifest validated by the admission webhook
  on every change, and by `apprafter app validate` locally.
- A public HTTPS endpoint on your domain through Cloudflare — TLS
  terminated at the Gateway on a Cloudflare Origin CA cert, with the
  node firewalled to Cloudflare's ranges only.
- Per-environment overrides via `spec.environments.<env>`: deploy a
  named environment with `apprafter app add --env staging` (a separate
  deployment you reach with `apprafter app status my-service --env
  staging`), and set the cluster's default environment with
  `apprafter platform env set`.

## Where to look next

- [`application-cue.md`](./application-cue.md) — the Application.cue
  manifest in depth: fields, `needs`, multi-environment patterns.
- [`image-iteration.md`](./image-iteration.md) — the build → push →
  auto-redeploy iteration loop.
- [connect a domain](../public-ingress/connect-a-domain.md) and the
  [Cloudflare Origin CA cert](../public-ingress/cloudflare-origin-cert.md)
  guide — the full public-ingress runbook.
- [operator quickstart](../operator-guide/quickstart.md) — the full
  cluster lifecycle and day-2 operations.
- [gitops-walk](../operator-guide/gitops-walk.md) — wiring Argo CD to
  GitHub / GitLab, public and private.
- [`schemas/v1alpha1/application.cue`](https://github.com/apprafter/apprafter/blob/main/schemas/v1alpha1/application.cue) —
  the Application CRD shape your manifest is validated against.
