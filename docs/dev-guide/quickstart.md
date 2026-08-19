---
description: "The whole developer path in one session: register a target, ship an application, and serve it on a public domain through Cloudflare."
---

# Quickstart

This walkthrough is the full developer path on AppRafter: from
registering a deployment target, through scaffolding and shipping an
application, to serving it in production on a public domain through
Cloudflare. Everything runs through the `apprafter` CLI. Budget ~15
minutes of hands-on time, plus DNS-propagation wait at the end.

!!! warning "This guide provisions a paid server"
    Bringing the cluster up creates a real **Hetzner Cloud CPX22**
    server, billed by Hetzner (hourly) for as long as it runs — this
    is not a free sandbox. When you are done, tear everything down
    with [`apprafter destroy --yes`](#clean-up) so you stop being
    billed. If you only want to evaluate AppRafter without the public
    domain, there is an [exit point](#checkpoint) after step 4.

## Install

Get the `apprafter` CLI onto your `PATH`. The release binary is the
recommended path; build-from-source is for contributors.

=== "Recommended — release binary"

    Download the prebuilt binary for your platform from
    [GitHub Releases](https://github.com/AppRafter/apprafter/releases)
    and drop it on your `PATH`. The asset name carries the tag, so the
    first line resolves the newest release rather than naming one — a
    literal tag written here goes stale the next time we cut a release,
    which is what it had done. Substitute an explicit `v0.2.x` if you
    want to pin.

    ```sh
    VERSION=$(curl -fsSL https://api.github.com/repos/AppRafter/apprafter/releases/latest \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')
    TARGET=x86_64-unknown-linux-gnu              # or x86_64-apple-darwin / aarch64-apple-darwin
    curl -fsSL "https://github.com/AppRafter/apprafter/releases/download/${VERSION}/apprafter-${VERSION}-${TARGET}.tar.gz" | tar xz
    sudo mv apprafter /usr/local/bin/
    apprafter --version
    ```

    Or, with the GitHub CLI (resolves the latest release for you):

    ```sh
    gh release download --repo AppRafter/apprafter \
        --pattern 'apprafter-*-x86_64-unknown-linux-gnu.tar.gz'
    tar xzf apprafter-*-x86_64-unknown-linux-gnu.tar.gz && sudo mv apprafter /usr/local/bin/
    ```

    Each release ships a `.sha256` next to every tarball — verify with
    `shasum -a 256 -c apprafter-${VERSION}-${TARGET}.tar.gz.sha256`.
    Prebuilt targets are Linux `x86_64`, macOS `x86_64` (Intel), and
    macOS `aarch64` (Apple Silicon). **Linux `aarch64` (ARM) is not
    published yet** — build from source for ARM servers.

=== "Contributors — build from source"

    Requires a Rust toolchain (`mise.toml` pins `stable`). From a repo
    checkout:

    ```sh
    cargo install --path cli/platform-cli
    ```

    See [Contributing → Setup](../contributing/setup.md) for the full
    contributor toolchain.

=== "Local dev — Nix / devcontainer"

    The repo's `nix develop` shell pre-installs Bun, Rust, cue,
    kubectl, helm, and friends; the `.devcontainer/` mirrors it.
    Inside the shell, build the CLI once with `cargo install --path
    cli/platform-cli` so `apprafter` is on your `PATH`.

### Shell completion

The CLI can complete its own subcommands, flags, and the values of the
flags that take a fixed set. `apprafter completion <shell>` prints the
script to stdout and installs nothing — putting that output where your
shell reads completions from is the whole job, and where that is depends
on the shell:

=== "bash"

    Needs the `bash-completion` package, which most distributions ship;
    on macOS install it with `brew install bash-completion@2`.

    ```sh
    mkdir -p ~/.local/share/bash-completion/completions
    apprafter completion bash > ~/.local/share/bash-completion/completions/apprafter
    ```

    Open a new shell to pick it up.

=== "zsh"

    The script has to land in a directory on `fpath`:

    ```sh
    mkdir -p ~/.zfunc
    apprafter completion zsh > ~/.zfunc/_apprafter
    ```

    If `~/.zfunc` is not on `fpath` already, add both of these to
    `~/.zshrc`, in this order — `compinit` reads `fpath` as it runs, so
    a line added after it has no effect until the next shell:

    ```sh
    fpath=(~/.zfunc $fpath)
    autoload -Uz compinit && compinit
    ```

    Open a new shell to pick it up.

=== "fish"

    ```sh
    mkdir -p ~/.config/fish/completions
    apprafter completion fish > ~/.config/fish/completions/apprafter.fish
    ```

    fish reads that directory at the next prompt; no restart needed.

To check it worked, type a partial command and press Tab — the
subcommand list should complete.

Two things worth knowing. The script describes the binary that produced
it, so it goes stale when you upgrade: re-run the same command after
installing a new release. And the three shells above are the ones with a
published recipe, not the whole list — `apprafter completion --help`
names every value the argument accepts.

## Prerequisites

Only the CLI and a Hetzner token are needed to stand up a Tier 1
cluster. The rest depends on what you want to do — the happy path
drives everything through `apprafter app *` and server-side
validation, so `kubectl` and `cue` are **optional**.

| Tool / credential | When you need it | Notes |
| ----------------- | ---------------- | ----- |
| `apprafter` CLI | **Always** | the one hard requirement — see [Install](#install). |
| Hetzner Cloud API token | **Always (Tier 1)** | create one in the Hetzner Cloud console. |
| SSH public key | **Always (Tier 1)** | injected into the node for break-glass access. |
| Docker ≥ 24 | To ship **your own** app | builds and pushes the container image (step 3). |
| A container registry | To ship **your own** app | e.g. GHCR — where the image lives. See [private repos & registries](./private-repos-and-registries.md). |
| Domain + Cloudflare account | **Public HTTPS only** | step 5 — skip it if you only want to evaluate. |
| `kubectl` ≥ 1.29 | _Optional_ | only for raw cluster inspection; `apprafter app *` covers the happy path. |
| `cue` ≥ 0.10 | _Optional_ | only for `apprafter app validate` locally; the cluster validates every change server-side regardless. |
| Bun ≥ 1.x | _Optional_ | only to run the OneBun starter on your machine. |

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

`apprafter doctor` prints one line per check; output resembles:

```text
Checking target `prod`...
  ✓ credentials.yaml present
  ✓ Hetzner token format
  ✓ Hetzner API reachable (token authenticates)
  ✓ SSH public key readable

Checking environment...
  ✓ kubectl (Client Version: v1.29.x)
  ⚠ cue (not found — optional; needed only for local `app validate`)

N checks: … passed, 1 warning(s), 0 FAIL
```

A non-zero exit (any `✗ FAIL`) means something is broken — see
[Troubleshooting](../operator-guide/troubleshooting.md). Then point
`kubectl` at the new cluster (optional — only if you want to poke at
raw resources):

```sh
apprafter kubeconfig > /tmp/kc && export KUBECONFIG=/tmp/kc
kubectl get nodes                # ↳ Ready
```

## 2. Scaffold your application

Work from the root of your application repository (it needs a
`Dockerfile`). `apprafter app scaffold` writes a minimal
`apprafter/Application.cue` from a starter **skeleton** — it does
**not** inspect your `Dockerfile`, source, ports, or env. The
`--runtime` flag only picks which skeleton to start from (omit it and
the CLI guesses from files in the current directory; pass it to force
the choice). The one value auto-filled from your project is the
`image` ref, derived from your git `origin` (GitHub → `ghcr.io`,
GitLab → `registry.gitlab.com`, …) — review `image` / `port` / `env`
/ `needs` before you commit:

```sh
git init
git remote add origin https://github.com/<your-org>/my-service.git

apprafter app scaffold --runtime bun --name my-service
# ↳ writes apprafter/Application.cue from the `bun` skeleton. The
#   image ref comes from your git origin — edit `image:`, `port`, and
#   `env` to match your app. Add --needs pg|redis|disk to declare a
#   managed dependency.

apprafter app validate           # local cue render check (needs cue on PATH)
git add . && git commit -m "feat: scaffold apprafter manifest"
```

> **No app yet?** The `bun-http` example under
> [`examples/templates/bun-http/`](https://github.com/apprafter/apprafter/blob/master/examples/templates/bun-http/README.md)
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

A **private** image (or a private source repo) needs credentials
registered first — the token types and scopes differ between Git
read and registry pull, so see
[private repos & registries](./private-repos-and-registries.md)
before you continue.

## 4. Deploy and verify (internal)

Push the repo, then register it with one command. Argo CD tracks the
repo, the CUE CMP compiles `apprafter/Application.cue` on every sync,
and the AppRafter operator reconciles the result into a Deployment +
Service. By default `expose.network` is `internal`, so the app is
ClusterIP-only at this point:

```sh
git push -u origin main          # or your repo's default branch

apprafter app add                # auto-detects the git origin
# ↳ for a private repo, register credentials first with
#   `apprafter repo creds add`.
```

`apprafter app add` confirms the registration; output resembles:

```text
✓ Application 'my-service' registered in AppProject 'apps'.
  Repo:        https://github.com/<your-org>/my-service.git
  Revision:    main
  Path:        apprafter
  Destination: my-service (created if missing)

Argo CD will sync the workload within a reconcile cycle. State:
  apprafter app status my-service
```

Watch it converge with the simple `app` commands — no raw `kubectl`
needed (each takes the logical app name, `my-service`):

```sh
apprafter app status my-service  # sync state, health, source, history
apprafter app logs my-service -f # stream workload logs
apprafter app open my-service    # port-forward + open in a browser
```

`apprafter app status` reports the Argo CD + operator view; once it
has converged you'll see:

```text
Application argocd/my-service
  project:       apps
  repo:          https://github.com/<your-org>/my-service.git
  revision:      main
  path:          apprafter
  destination:   my-service
  environment:   prod
  sync state:    Synced
  health:        Healthy
```

`apprafter app list` shows every app you've registered; `apprafter
app rollback my-service` reverts to the previous revision.

!!! success "Checkpoint — you have a working cluster + app"
    <a id="checkpoint"></a>
    You now have a running Tier 1 cluster and an internally-reachable
    app. **Stop here if you only wanted to evaluate AppRafter** — jump
    to [Clean up](#clean-up) to tear the paid server down. Continue
    below only for the full public-HTTPS path on your own domain,
    which is the heaviest part of the guide.

## 5. Release to production on a Cloudflare domain

The production path serves your app over HTTPS through Cloudflare:
TLS terminates at the cluster Gateway on a Cloudflare Origin CA
certificate, and the node's `80`/`443` are firewalled so the only way
in is through Cloudflare. This summarizes the
[connect-a-domain runbook](../operator-guide/connect-a-domain.md) —
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
[Cloudflare Origin CA certificate](../operator-guide/cloudflare-origin-cert.md))
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
  deployment — `apprafter app status my-service` reports every
  environment of the app, one section each, while `--env staging`
  narrows `app logs`, `app rollback` and `app remove` to that one), and
  set the cluster's default environment with `apprafter platform env
  set`.

## Clean up

The Tier 1 server bills for as long as it runs. When you're done,
tear down the whole cluster and its infrastructure:

```sh
apprafter destroy --yes          # removes the Hetzner server + all tagged resources
```

`destroy` reads live state from the Hetzner API and removes every
resource tagged `apprafter=true`, so it works even if your local
state file is stale. Drop the `--yes` to be prompted for confirmation
first.

## Where to look next

- [`application-cue.md`](./application-cue.md) — the Application.cue
  manifest in depth: fields, `needs`, multi-environment patterns.
- [private repos & registries](./private-repos-and-registries.md) —
  credentials for private source repos and private image pulls, and
  the token-scope gotchas that differ between the two.
- [`image-iteration.md`](./image-iteration.md) — the build → push →
  auto-redeploy iteration loop.
- [Troubleshooting](../operator-guide/troubleshooting.md) — diagnostic
  codes and common bring-up failures.
- [connect a domain](../operator-guide/connect-a-domain.md) and the
  [Cloudflare Origin CA cert](../operator-guide/cloudflare-origin-cert.md)
  guide — the full public-ingress runbook.
- [operator quickstart](../operator-guide/quickstart.md) — the full
  cluster lifecycle and day-2 operations.
- [Connect a Git repository](../operator-guide/connect-a-git-repository.md)
  — wiring Argo CD to GitHub / GitLab, public and private.
- [`schemas/v1alpha1/application.cue`](https://github.com/apprafter/apprafter/blob/master/schemas/v1alpha1/application.cue) —
  the Application CRD shape your manifest is validated against.
