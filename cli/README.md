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

`platform-cli` currently prints `would …` stubs for every command.
Real provisioning, GitOps wiring, and OIDC login arrive in the
following plan.md phases (1.2 / 1.3 / 4.7 / …).
