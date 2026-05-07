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
- **Network + Firewall for Hetzner Cloud** (v0.1.4) —
  `HetznerCloudClient` list/create/delete networks and firewalls;
  four new `Action` variants (`CreateNetwork`, `DestroyNetwork`,
  `CreateFirewall`, `DestroyFirewall`); `NetworkSpec`,
  `FirewallSpec`, `FirewallRuleSpec`. `HetznerCloudProvider`
  applies in order ssh → net → fw → server (with all attached via
  `ServerCreateRequest.networks` / `firewalls`) and destroys in
  reverse. CLI `apply` builds default specs (10.0.0.0/16 net +
  SSH 22 / HTTPS 443 firewall, both keyed off the cluster name).
  `HetznerCloudState` caches `network_id` and `firewall_id`.
- **CUE Infrastructure manifest parsing** (v0.1.5) — new
  `cli-core::manifest` module mirrors the v1alpha1 Infrastructure
  schema in typed Rust and exposes `parse_infrastructure`. The
  CUE schema now declares optional `region`, `network` (with
  `subnet`), `firewall.ingress`, `sshKeys`, and `osImage` fields.
  Setting `APPRAFTER_MANIFEST=<path>` causes `apply` to overlay
  manifest values onto the v0.1.4 defaults; without the env var
  the v0.1.4 behaviour is unchanged.
- **cert-manager + self-signed ClusterIssuer** (v0.1.15) —
  `platform-cli cluster-bootstrap` now ends with `helm repo add
  jetstack https://charts.jetstack.io` + `helm upgrade --install
  cert-manager jetstack/cert-manager --version v1.16.2 --namespace
  cert-manager --create-namespace --wait` against the tier-1
  values from the new `cli-providers::k8s::cert_manager_values`
  module (installCRDs: true, single replicas, Prometheus off);
  then `kubectl apply -f` for the self-signed `ClusterIssuer`
  named `apprafter-selfsigned` (new module
  `cli-providers::k8s::issuer`, `pub const`
  `APPRAFTER_SELFSIGNED_ISSUER` so future HTTPRoute / Certificate
  manifests reference it by name). Renamed FakeRunner test pins
  the now-3-helm-installs / 3-kubectl-applies sequence.
- **`platform-cli argocd-password`** (v0.1.14) — new subcommand
  that reads the Argo CD admin password from the cluster on first
  call (`kubectl get secret argocd-initial-admin-secret -n argocd
  -o jsonpath` → base64 decode), encrypts the plaintext with the
  same age identity used for kubeconfig, caches the armored
  ciphertext in `state.hetzner_cloud.argocd_admin_password_age`,
  and prints the plaintext on stdout. Subsequent calls decrypt the
  cache in O(1); `--refresh` forces a re-fetch.
  `KubectlRunner` trait gains `get_secret_value` (real impl pulls
  in `base64 = "0.22"` for the decode); `KubectlCli` argv-shape is
  pinned by a new unit test. The cluster-bootstrap FakeKubectl
  gets a no-op `unreachable!()` impl since that orchestrator
  doesn't read secrets.
- **Argo CD Helm install** (v0.1.13) — `platform-cli
  cluster-bootstrap` now ends with `helm repo add argo
  https://argoproj.github.io/argo-helm` + `helm upgrade --install
  argocd argo/argo-cd --version 7.7.7 --namespace argocd
  --create-namespace --wait` against the tier-1 values from the
  new `cli-providers::k8s::argocd_values` module (Dex off,
  Redis-HA off, ApplicationSet on, Notifications off, ClusterIP
  server, single replicas across every sub-chart). The HTTPRoute
  exposure path + admin password retrieval are explicitly deferred
  to v0.1.14 (admin password) and v0.1.15 (cert-manager +
  HTTPRoute + bootstrap-Application).
- **NetworkPolicy default-deny + cluster smoke** (v0.1.12) —
  `platform-cli cluster-bootstrap` now ends with a `kubectl apply`
  of a default-deny `NetworkPolicy` on the `default` namespace
  (kube-system exempt — Cilium and Gateway API system pods need
  free egress). New module
  `cli-providers::k8s::network_policy` exposes
  `default_deny_network_policy_yaml(namespace)`. The existing
  FakeKubectl test in `commands::cluster_bootstrap` was renamed
  and extended to assert both the Gateway API URL apply and the
  NetworkPolicy path apply happen in order. A new
  `#[ignore]`-tagged real-cluster smoke
  (`cli/platform-cli/tests/cluster_smoke_test.rs`) verifies
  `cilium status` + Gateway admission + default-deny presence;
  opt-in via `APPRAFTER_K8S_SMOKE=1`. Sub-phase 1.4 in plan.md
  flips from 🚧 partial to ✅ shipped.
- **`platform-cli cluster-bootstrap`** (v0.1.11) — new subcommand
  that, after `apply` + `kubeconfig` give us a working cluster,
  installs Cilium 1.16.5 via Helm (kube-proxy replacement, IPAM
  kubernetes, Hubble off, single operator replica) and applies the
  upstream Gateway API v1.2.1 standard-install CRDs. New module
  `cli-providers::k8s` exposes `HelmRunner` / `KubectlRunner`
  trait seams (real impls shell out to `helm` and `kubectl`,
  fakes drive the unit tests) plus `cilium_values_yaml()` and
  `gateway_api_crds_url()` pure builders. The cloud-init payload
  now adds `--disable-kube-proxy` to the k3s install command so the
  Cilium-side replacement actually takes effect. Default-deny
  NetworkPolicy + the live smoke verifier land in v0.1.12.
- **age-encrypted kubeconfig** (v0.1.10) — `platform-cli kubeconfig`
  now persists the cached cluster YAML in
  `state.hetzner_cloud.kubeconfig_age` (ASCII-armored) instead of
  plaintext. New module `cli_core::secrets` exposes
  `load_or_create_identity`, `encrypt_for_recipient`, and
  `decrypt_with_identity`; the on-disk identity defaults to
  `~/.config/apprafter/age.key` (mode 0600 on Unix) with
  `APPRAFTER_AGE_KEY` honoured as an override. The legacy
  `kubeconfig_yaml` plaintext slot is read as fallback for one
  cycle so state files written by v0.1.9 keep working; the next
  cold-fetch / `--refresh` migrates them forward. Sub-phase 1.3
  in plan.md flips from 🚧 partial to ✅ shipped.
- **`platform-cli kubeconfig`** (v0.1.9) — new subcommand that
  reads the k3s kubeconfig from a freshly provisioned cluster.
  First call: SSHes to the server's public IPv4 (private key
  resolved from `APPRAFTER_SSH_PRIVATE_KEY`, default
  `~/.ssh/id_ed25519`), reads `/etc/rancher/k3s/k3s.yaml`,
  rewrites the loopback `server:` URL to the public address, and
  caches the result in `state.hetzner_cloud.kubeconfig_yaml`.
  Subsequent calls print the cache in O(1); `--refresh` forces a
  re-fetch. New module
  `cli-providers::hetzner_cloud::kubeconfig` exposes
  `rewrite_server_url`, the `KubeconfigFetcher` trait, and
  `SshKubeconfigFetcher`. `Server` wire type now decodes
  `public_net.ipv4`. The cached YAML is plaintext for this cycle;
  age-encryption arrives in v0.1.10.
- **k3s cloud-init bootstrap** (v0.1.8) — every newly provisioned
  Hetzner server gets a `#cloud-config` `user_data` payload that
  installs k3s in single-node mode (with traefik + servicelb
  disabled, since Cilium + Gateway API replace them in phase 1.4),
  enables UFW with the AppRafter port whitelist, and turns on
  fail2ban for the SSH jail. New module
  `cli-providers::hetzner_cloud::user_data` exposes
  `K3sBootstrapOptions` + `build_k3s_user_data`. `ServerSpec` and
  `ServerCreateRequest` gain an optional `user_data: String`
  (serde-skipped when `None`, so existing apply paths that don't
  set it produce identical wire JSON). The default cloud-side
  firewall is broadened to mirror the in-VM ufw whitelist: 22 +
  6443 + 80 + 443 / tcp + 51820 / udp (ssh, kube API, HTTP, HTTPS,
  wireguard).
- **`platform-cli import`** (v0.1.7) — new read-only subcommand
  that rebuilds `.apprafter/state.json` from live Hetzner Cloud
  resources tagged `apprafter=true`. Picks the server whose name
  matches `state.cluster_name`; collects ssh-keys / network /
  firewall / floating-IP ids by label only. Refuses to overwrite an
  existing `state.hetzner_cloud` unless `--force` is passed; supports
  `--dry-run` for preview. Backed by a new `commands::hcloud`
  helper that reads `APPRAFTER_HCLOUD_BASE_URL` (test-only seam used
  by the new mockito-driven integration tests) with a fallback to
  `DEFAULT_BASE_URL`. Closes sub-phase 1.2 in plan.md.
- **Floating IPs for Hetzner Cloud** (v0.1.6) —
  `HetznerCloudClient` list/create/delete floating IPs (404
  idempotent on delete); two new `Action` variants
  (`CreateFloatingIp`, `DestroyFloatingIp`); `FloatingIpSpec`.
  `HetznerCloudProvider.floating_ips` applies after the server
  exists (so each IP is reserved with `server` already attached)
  and destroys first (so detach completes before the server is
  removed). `HetznerCloudState` caches `floating_ip_ids`. The
  `network.floatingIPs: [...string]` CUE field — reserved in
  v0.1.5 — is now wired end-to-end: each name is prefixed with
  the cluster name on the provider side, the IP type defaults to
  `ipv4`, and `home_location` follows the cluster region. The
  example fixture declares `floatingIPs: ["egress"]`.

### Changed

- `platform-cli init` now persists state (provider/tier/region/
  cluster_name) instead of just printing.
- `platform-cli apply` is no longer a stub — it requires
  `HCLOUD_TOKEN` and a state with `provider: hetzner-cloud`.

### Quality

- **CLI test coverage uplift (round 1)** — added 14 mockito
  error-path tests for `HetznerCloudClient` (every `list_*` /
  `create_*` / `delete_*` method now exercises both the happy path
  and at least one `Err::Status` mapping to `CliError::Hetzner`),
  plus three small fillers in `cli-core` (`Tier::level`,
  `Tier::from_str` unknown branch,
  `cli_core::manifest::parse_infrastructure` missing-document
  branch). `hetzner_cloud/client.rs` 45% → 95%; `cli-core/src/tier.rs`
  and `manifest.rs` reach 100%.
- **CLI test coverage uplift (round 2)** — moved most testable
  logic in `platform-cli` out of subprocess-only territory by adding
  `#[cfg(test)] mod tests` blocks inside the source modules:
  - `commands/apply.rs` — 12 unit tests covering every builder
    helper (`build_server_spec`, `build_ssh_specs`, `build_network_spec`,
    `build_firewall_spec`, `rule_from_manifest`,
    `default_ingress_rules`, `build_floating_ip_specs`) with both
    "manifest absent" and "manifest overrides defaults" paths.
    apply.rs jumps from 0% to 51%.
  - `commands/hcloud.rs` — env-var fallback covered. 0% → 100%.
  - `commands/import.rs` — 5 in-process tests of the private
    `build_snapshot` helper against a `mockito` server: matched
    server, no apprafter label, name mismatch, per-category
    label filter, and a smoke for `print_summary`. 0% → 57%.
  Workspace coverage 78% → **89.6%**. Remaining gaps in
  `platform-cli` (the orchestration body of `run` in apply / destroy /
  import / init plus the `would …` stub commands) are subprocess-tested
  by the `cli_smoke` and `import_test` integration suites — tarpaulin
  cannot see them but they ARE exercised. Numbers measured with
  cargo-tarpaulin 0.35.2, e2e test excluded.

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
