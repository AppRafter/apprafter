# cli/

`platform-cli` — Rust binary for cluster bootstrap, lifecycle
management, and tier upgrades. Subcommands: `init`, `plan`,
`apply`, `import`, `destroy`, `status`, `login`, `upgrade-tier`.

## Layout

This is a Cargo workspace with one binary crate and three library
crates:

| Crate           | Role                                                          |
| --------------- | ------------------------------------------------------------- |
| `platform-cli`  | Binary: clap parsing, subcommand dispatch, `color-eyre` wiring|
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
# are used (10.0.0.0/16 net, SSH 22 + HTTPS 443 firewall, cx22,
# ubuntu-24.04).
export APPRAFTER_MANIFEST=examples/infrastructure/tier-1-hetzner.cue

cd cli
cargo run --bin platform-cli -- init --provider hetzner-cloud --tier solo --region nbg1
cargo run --bin platform-cli -- apply
cargo run --bin platform-cli -- destroy --yes
```

`apply` provisions one private network (10.0.0.0/16 with a
10.0.0.0/24 cloud subnet in `eu-central`), one cloud-side firewall
(whitelisting 22 + 6443 + 80 + 443 / tcp + 51820 / udp inbound —
ssh, kube API, HTTP, HTTPS, wireguard), and one CX22 server
attached to both. The server is provisioned with a cloud-init
`#cloud-config` payload that:

- updates apt and installs `ufw` + `fail2ban`;
- enables UFW with the same port whitelist (default-deny inbound);
- enables fail2ban for the SSH jail;
- runs the canonical `get.k3s.io` installer with
  `--disable=traefik --disable=servicelb` (Cilium + Gateway API
  replace them in phase 1.4).

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
machine), `platform-cli import` rebuilds the Hetzner section by
scanning the live API for resources tagged with `apprafter=true`:

```sh
export HCLOUD_TOKEN=...
cargo run --bin platform-cli -- import --dry-run   # preview only
cargo run --bin platform-cli -- import             # write state
cargo run --bin platform-cli -- import --force     # overwrite an
                                                   # existing snapshot
```

`import` is read-only (no `create_*` / `delete_*` calls) and refuses
to overwrite an already-populated `state.hetzner_cloud` unless you
pass `--force`. It picks the server whose name matches
`state.cluster_name` (default `platform-1`); if no labelled server
matches, it prints a friendly message and writes nothing.

Run the real-Hetzner integration test manually:

```sh
APPRAFTER_HCLOUD_E2E=1 \
HCLOUD_TOKEN=... \
cargo test -p cli-providers --test hetzner_cloud_test \
    e2e_real_hetzner_test -- --ignored
```
