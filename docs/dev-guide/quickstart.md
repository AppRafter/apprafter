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
    Bringing the cluster up creates a real **Hetzner Cloud** server of
    the type you pick in step 1, billed by Hetzner (hourly) for as long
    as it runs — this is not a free sandbox. There is no default server
    type: you choose one, and that choice is what you are billed for.
    When you are done, tear everything down with
    [`apprafter destroy --yes`](#clean-up) so you stop being billed. If
    you only want to evaluate AppRafter without the public domain,
    there is an [exit point](#checkpoint) after step 4.

## Install

Get the `apprafter` CLI onto your `PATH`. The release binary is the
recommended path; build-from-source is for contributors.

=== "Recommended — one-line install"

    ```sh
    curl -fsSL https://apprafter.dev/install.sh | sh
    ```

    It detects your platform, resolves the newest release, downloads the
    archive with its `.sha256`, and **verifies the checksum before
    installing**. A download it cannot verify is refused, not installed.
    To read it first — the better habit, and one line more:

    ```sh
    curl -fsSL https://apprafter.dev/install.sh -o install.sh
    sh install.sh
    ```

    `APPRAFTER_VERSION=v0.2.x` pins a release instead of resolving one;
    `APPRAFTER_INSTALL_DIR` chooses where the binary lands (default
    `/usr/local/bin`).

=== "By hand — release archive"

    [apprafter.dev/download](https://apprafter.dev/download/) lists each
    archive and its checksum, resolved to the current release. Verify
    before you install:

    ```sh
    shasum -a 256 -c apprafter-<version>-<target>.tar.gz.sha256
    tar xzf apprafter-<version>-<target>.tar.gz && sudo mv apprafter /usr/local/bin/
    ```

    Prebuilt targets are Linux `x86_64`, macOS `x86_64` (Intel), and
    macOS `aarch64` (Apple Silicon). **Linux `aarch64` (ARM) is not
    published yet** — build from source for ARM servers.

    Do not resolve the version through `/releases/latest`, and do not
    reach for `gh release download` without a tag. This is a monorepo
    with five release series, and both of those return the newest
    release across all of them — which is usually a chart, not the CLI,
    and the download URL built from it 404s.

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

`apprafter` is not a self-contained binary: it **shells out to
`kubectl` and `helm`** rather than reimplementing them. Every command
that talks to the cluster — `app list`, `app status`, `app logs`,
`secret seal`, `repo creds`, `target domain`, `backup`, `restore` —
spawns `kubectl`, and `cluster-bootstrap` additionally spawns `helm`.
Both must be on your `PATH` before you start; without `kubectl` you get

```text
× spawn kubectl: No such file or directory (os error 2)
```

`cue` is the genuinely optional one — it is needed only by
`apprafter app validate`, and the cluster validates every change
server-side regardless.

| Tool / credential | When you need it | Notes |
| ----------------- | ---------------- | ----- |
| `apprafter` CLI | **Always** | see [Install](#install). |
| `kubectl` ≥ 1.29 | **Always** | the CLI spawns it for every cluster-facing command; `apprafter doctor` checks it. |
| `helm` ≥ 3 | **Always** | spawned by `cluster-bootstrap` (step 1) to install the Argo CD loader; `apprafter doctor` checks it. |
| Hetzner Cloud API token | **Always (Tier 1)** | create one in the Hetzner Cloud console. |
| SSH public key | **Always (Tier 1)** | injected into the node for break-glass access. |
| Docker ≥ 24 | To ship **your own** app | builds and pushes the container image (step 3). |
| A container registry | To ship **your own** app | e.g. GHCR — where the image lives. See [private repos & registries](./private-repos-and-registries.md). |
| Domain + Cloudflare account | **Public HTTPS only** | step 5 — skip it if you only want to evaluate. |
| `cue` ≥ 0.10 | _Optional_ | only for `apprafter app validate` locally. |
| Bun ≥ 1.x | _Optional_ | only to run the OneBun starter on your machine. |

## 1. Register a target and bring the cluster up

If someone has already done this, skip to step 2 — you need nothing from it
except a cluster that exists.

Otherwise it is the operator guide's job, in full:
[Operator quickstart](../operator-guide/quickstart.md) takes a blank Hetzner
account to a self-managing cluster. Follow it and come back.

The short version, so you know what you are being sent to do: register a target
with `apprafter target add`, then bring the whole tier-1 stack up with
`apprafter up`. About three minutes.

You do not need `KUBECONFIG` for anything on this page — the `apprafter app`
commands resolve the cluster from the target store themselves.

??? note "If you want a shell against the cluster anyway"

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
> [The bun-http template](https://github.com/apprafter/apprafter/blob/master/examples/templates/bun-http/README.md)
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
[Image iteration](./image-iteration.md).

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

On a TTY `apprafter app add` opens a short wizard, pre-filling each
field from your git remote and the scaffolded manifest; pressing Enter
through it accepts the defaults below. It then confirms:

```text
✓ Application 'my-service' registered in AppProject 'apps'.
  Repo:        https://github.com/<your-org>/my-service.git
  Revision:    main
  Path:        /
  Destination: apprafter (created if missing)

Argo CD will sync the workload within a reconcile cycle. State:
  apprafter app status my-service
```

`Path: /` is the repo root — the CUE plugin walks the whole repository
for a manifest, so it does not need the `apprafter/` subdirectory
named. `Destination: apprafter` is the namespace the operator watches
and the one `apprafter app scaffold` wrote into your manifest's
`metadata.namespace`; `--path` and `--namespace` override both.

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
  path:          /
  destination:   apprafter
  environment:   (base)
  sync state:    Synced
  health:        Healthy
```

`environment: (base)` is what a deployment registered without `--env`
reports: no `spec.environments` overlay is applied, so `spec.base`
renders as written. Below this block `app status` also lists the
workload pods, services and any resource claims.

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

Two halves, and only the second one is yours.

**The cluster half is done once, by whoever operates the cluster**: lock the
origin firewall to Cloudflare, mint and import an Origin CA certificate, register
the zone, and point DNS at the node. It is a runbook with real DNS waits and a
firewall you can lock yourself out behind, so it lives where it belongs —
[Connect a domain](../operator-guide/connect-a-domain.md), with
[Cloudflare Origin CA certificate](../operator-guide/cloudflare-origin-cert.md)
for the certificate. If `apprafter target domain list` already shows your zone,
it is done.

**Your half is one field.** In `apprafter/Application.cue`, open up the existing
`expose` block. The `bun` skeleton scaffolds `port: 3000` with `network` and
`hostname` present but commented out — uncomment both, set the hostname, and keep
whatever port your process actually binds:

```cue
spec: base: expose: {
    port:     3000             // your service's listen port
    network:  "public"         // was commented out; default is "internal"
    hostname: "app.<zone>"     // the apex, or a subdomain of a registered zone
}
```

TLS is on by default for a public service. Commit and push — Argo CD re-syncs
and your Service picks up a route on the Gateway:

```sh
git commit -am "feat: expose my-service on app.<zone>"
git push
apprafter app status my-service  # watch it return to Synced + Healthy
```

Then check it serves:

```sh
curl -v https://app.<zone>/
```

!!! note "Going public is a gated change"
    `network: internal` → `public`, and adding a hostname to a public app, are
    both edits the platform pauses for approval rather than rolling out. See
    [When a change needs approval](when-a-change-needs-approval.md).

## What you just got

- A container image of your own, rolling out on every push of the tag
  it names (see [Image iteration](./image-iteration.md)). If you
  started from the `bun-http` example, that is a typed OneBun service
  (`@onebun/core` decorators + DI) that serves Prometheus `/metrics`
  and emits OpenTelemetry traces — the endpoints exist, but the Tier-1
  baseline ships no scraper or collector to consume them yet.
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
resource tagged `apprafter=true` **in the project the token belongs
to**, so it works even if your local state file is stale — and so it is
scoped to a project rather than to a cluster. That is the right thing
here, where the project holds only the cluster this guide built; if you
ever run two AppRafter clusters under one Hetzner token, read [the
destroy scope](../operator-guide/target-store.md#destroy-scope) first.
Drop the `--yes` to be prompted for confirmation first.

## Where to look next

- [Writing Application.cue](./application-cue.md) — the Application.cue
  manifest in depth: fields, `needs`, multi-environment patterns.
- [private repos & registries](./private-repos-and-registries.md) —
  credentials for private source repos and private image pulls, and
  the token-scope gotchas that differ between the two.
- [Image iteration](./image-iteration.md) — the build → push →
  auto-redeploy iteration loop.
- [Rolling back a bad deploy](./rollback.md) — undoing a bad deploy, and releasing
  the pin it leaves.
- [Troubleshooting](../operator-guide/troubleshooting.md) — diagnostic
  codes and common bring-up failures.
- [connect a domain](../operator-guide/connect-a-domain.md) and the
  [Cloudflare Origin CA cert](../operator-guide/cloudflare-origin-cert.md)
  guide — the full public-ingress runbook.
- [operator quickstart](../operator-guide/quickstart.md) — the full
  cluster lifecycle and day-2 operations.
- [`schemas/v1alpha1/application.cue`](https://github.com/apprafter/apprafter/blob/master/schemas/v1alpha1/application.cue) —
  the Application CRD shape your manifest is validated against.
