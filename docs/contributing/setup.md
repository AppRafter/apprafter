---
description: "Three ways to get a working toolchain — Nix flake, dev container, manual install — and the bootstrap and local-cluster steps that follow."
---

# Local development setup

> **TL;DR.** Pick **one** of the three install paths below, then run
> `just bootstrap && just e2e-up`.

There is no single "blessed" install path — Nix flake, dev container,
and manual install all work. Pick the one that matches your machine.

## Path A — Nix flake (recommended)

If you have [Nix](https://nixos.org/download.html) with flakes
enabled, a single command gives you a fully pinned shell:

```sh
nix develop
```

You get Rust + Bun + CUE + kubectl + k9s + helm + argocd + talosctl
+ k3d + cosign + syft + trivy + grype + just + lefthook + age + sops
+ jq + git, all from the same `nixpkgs` revision recorded in
`flake.lock`.

## Path B — VS Code Dev Container

If you use VS Code, install the **Dev Containers** extension, open
the repo, and pick "Reopen in Container". The container layout lives
in `.devcontainer/devcontainer.json` and the post-create script
fetches the rest (CUE, k3d, just, lefthook, cosign).

## Path C — Manual install

`mise.toml` covers Rust, Bun, Go, Node, and just. Install the rest
from upstream:

| Tool       | Where                                                 |
| ---------- | ----------------------------------------------------- |
| CUE ≥ 0.10 | <https://cuelang.org/docs/install/>                   |
| kubectl    | <https://kubernetes.io/docs/tasks/tools/>             |
| helm       | <https://helm.sh/docs/intro/install/>                 |
| k3d        | <https://k3d.io/v5.x/#installation>                   |
| just       | <https://just.systems/>                               |
| lefthook   | <https://lefthook.dev/>                               |
| cosign     | <https://docs.sigstore.dev/cosign/installation/>      |
| age, sops  | distro packages                                       |
| (optional) | k9s, argocd CLI, talosctl, syft, trivy, grype         |

Then:

```sh
mise install
```

## Bootstrap

After dependencies are present:

```sh
just bootstrap     # installs lefthook git hooks
just lint          # CUE + SPDX + docs + conditional Rust/TS
just test          # conditional Rust/TS
```

## Local cluster

`just e2e-up` provisions a local k3d cluster ready for the platform
manifests:

```sh
just e2e-up
kubectl get nodes
# ... iterate ...
just e2e-down
```

The cluster is created **without** traefik and servicelb; Cilium and
Gateway API are installed later by the platform bootstrap (phase 1.4
in `plan.md`).

## Common issues

- **`cue: command not found`** after `nix develop`: the tool is in
  `$PATH` only inside the dev shell. Re-enter via `nix develop`.
- **Docker socket unavailable in container**: the Dev Container uses
  `docker-in-docker`; on macOS, allow VS Code to access Docker
  Desktop's daemon in its settings.
- **Lefthook hooks not running**: run `just bootstrap` once after
  installing lefthook; the hooks live in `.git/hooks/`.
- **`just lint` fails with "docs-check needs nix"** on Path B/C: the
  docs gate needs the flake.lock-pinned mkdocs (no system fallback),
  even from a non-Nix install path. Either install Nix (Path A), or
  skip it locally and rely on the `docs` workflow to gate the PR.
