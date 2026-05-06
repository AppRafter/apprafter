# cli/

`platform-cli` — Rust binary for cluster bootstrap, lifecycle
management, and tier upgrades. Subcommands: `init`, `plan`,
`apply`, `status`, `login`, `upgrade-tier`.

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
10.0.0.0/24 cloud subnet in `eu-central`), one firewall (SSH 22 +
HTTPS 443 inbound), and one CX22 server attached to both. The
second `apply` is a no-op (idempotent). `destroy` requires
`--yes` and tears everything down (server → firewall → network →
SSH key).

Run the real-Hetzner integration test manually:

```sh
APPRAFTER_HCLOUD_E2E=1 \
HCLOUD_TOKEN=... \
cargo test -p cli-providers --test hetzner_cloud_test \
    e2e_real_hetzner_test -- --ignored
```
