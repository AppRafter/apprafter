# Changelog — Unreleased

All notable changes to AppRafter that have not yet shipped in a
tagged release land here. The format follows [Keep a Changelog]
v1.1.0. Pre-1.0 development is tracked as patch increments under
the `0.0.x` series; semver starts at 1.0.

## Phase 0 — Foundations (v0.0.1 → v0.0.8)

### Added

- **Repository scaffold** per `spec.md` Appendix A: `cli/`,
  `operator/`, `schemas/`, `providers/{pg-integrated, pg-aws,
  jetstream-integrated, clickhouse-integrated, redis-integrated,
  s3-integrated}/`, `backstage-plugins/`, `manifests/`, `examples/`,
  `docs/`.
- **`plan.md`** — actionable phase-by-phase development plan
  derived from the spec.
- **Licensing** — `LICENSE` (FSL-1.1-MIT, canonical text from
  fsl.software), `LICENSE-MIT`, `NOTICE` explaining the 2-year
  FSL → MIT conversion model, plugin-level MIT `LICENSE` files in
  `providers/` and `backstage-plugins/`, SPDX-header conventions
  in `docs/contributing/license-headers.md`.
- **12 ADRs** + Nygard-style template covering: FSL-1.1-MIT for
  core, codename "AppRafter", custom Rust operator vs Crossplane,
  CUE vs Pkl, kine+NATS vs etcd, OpenBao vs Vault, Tier-1
  SealedSecrets vs Tier-2+ OpenBao, HTTP-first notifications,
  platform-only templates, Dockerfile-first build, hybrid Rust SDK
  + OpenTofu shim providers, MigrationPlan as first-class.
- **CUE module** (`apprafter.io`) with v1alpha1 skeleton schemas
  for all nine CRDs (`Application`, `ServiceProvider`,
  `ResourceClaim`, `AccessGrant`, `MigrationPlan`,
  `ExternalSurface`, `Infrastructure`, `ServiceProviderPlugin`,
  `InfrastructureProviderPlugin`) and a vet-time fixture
  (`examples/applications/parser.cue`).
- **CI** — GitHub Actions workflows (`lint`, `test`,
  `license-check`, `conventional-commits`); GitHub meta files
  (`CODEOWNERS`, `PULL_REQUEST_TEMPLATE.md`, `ISSUE_TEMPLATE/`);
  `lefthook.yml` for local hooks; `scripts/check-spdx-headers.sh`
  and `scripts/check-commit-msg.sh`.
- **Dev environment** — three install paths (Nix flake, VS Code
  Dev Container, manual via `mise.toml`), unified `Justfile`
  (`bootstrap`, `lint`, `fmt`, `test`, `e2e-up`, `e2e-down`,
  `docs-serve`, `docs-build`, `stats`),
  `docs/contributing/setup.md`.
- **TechDocs skeleton** — mkdocs-material site with stub pages for
  Architecture, Concepts, Operator Guide, Developer Guide,
  Reference, plus Contributing and ADR sections; `CONTRIBUTING.md`,
  `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1), `SECURITY.md`,
  `GOVERNANCE.md` (lazy consensus + ADR process) at the repo root.

### Changed

- `spec.md` §6 (M0) — both remaining items flipped to `[x]`:
  "Repository structure defined" and "License chosen". The
  license-candidates note (MPL-2.0 / Apache-2.0) is replaced by
  the actual decision (FSL-1.1-MIT for core, MIT for plugins;
  see ADR 0001).

## Phase 1 — MVP single-node (in progress)

### Added

- **`platform-cli` workspace** — Cargo workspace under `cli/` with
  one binary crate (`platform-cli`) and three library crates
  (`cli-core`, `cli-state`, `cli-providers`).
- All six top-level subcommands (`init`, `plan`, `apply`, `status`,
  `login`, `upgrade-tier`) wired as no-op stubs that print
  structured "would-do" output and point at the future plan.md
  phase that fills each one in.
- `cli-core::cue::export` / `export_in` — subprocess wrappers
  around `cue export --out json`; `export_in(workdir, path)`
  invokes `cue` from the module-root directory because `cue`
  rejects absolute directory paths. Honours `CUE_BIN` env override;
  test skips gracefully when `cue` is absent.
- Local state at `.apprafter/state.json` (JSON in the skeleton
  phase) with `load_or_default` / `save` API and the expected
  error semantics.
- **`HetznerCloudProvider`** — first real built-in infrastructure
  provider. Blocking HTTP client (`ureq`) with handcrafted wire
  types; `apply` provisions a CX22 (idempotent via the
  `apprafter=true` label diff); new `destroy --yes` command tears
  it down. Mocked tests via `mockito`; one `#[ignore]`-tagged
  end-to-end test runs against a real Hetzner project when
  `APPRAFTER_HCLOUD_E2E=1` and `HCLOUD_TOKEN` are set.
- **`Provider` trait** — gained `destroy()` and a typed `Action`
  enum (`CreateServer`, `DestroyServer`, `Noop`); `Plan.changes`
  → `Plan.actions: Vec<Action>`.
- **`HetznerCloudState`** — `cli-state` carries `server_id` +
  `server_name` for the managed server (extended with
  `ssh_key_ids` in v0.1.3).
- **SSH-keys for Hetzner Cloud** (v0.1.3) — `HetznerCloudClient`
  list/create/delete ssh-keys; `Action::CreateSshKey/DestroySshKey`;
  `SshKeySpec`; `HetznerCloudProvider.ssh_keys` with ordered
  apply (ssh → server) and destroy (server → ssh); CLI `apply`
  reads `APPRAFTER_SSH_PUBLIC_KEY` from env; `HetznerCloudState`
  caches `ssh_key_ids`.

### Changed

- `platform-cli init` now persists state (provider/tier/region/
  cluster_name) instead of just printing.
- `platform-cli apply` is no longer a stub — it requires
  `HCLOUD_TOKEN` and a state with `provider: hetzner-cloud`.

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
