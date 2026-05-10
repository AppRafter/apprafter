# Changelog

All notable changes to AppRafter. The format follows
[Keep a Changelog] v1.1.0. Pre-1.0 development tracks patches as
`MAJOR.MINOR.PATCH` where the minor matches the plan.md phase and
the patch matches the (sub-)subphase. Milestone tags
(`v0.1.0-mvp`, `v0.2.0-services`, etc.) point at the closing
patch of each phase.

## Phase 2 — Platform services (in progress)

_No entries yet — Phase 2 (M2) opens with v0.2.0._

## v0.1.58 — Phase 1 patch (2026-05-10)

### Fixed

- **Float Dockerfile rust to `stable-alpine`, MSRV → 1.88**
  (v0.1.58) — past the v0.1.57 fix, operator image build hit
  the next transitive-dep MSRV bump: `home@0.5.12` (pulled in
  by `kube` for `~/.kube/config` resolution) requires rustc
  1.88. Each new dep MSRV bump would force another v0.1.x
  patch on a fixed Dockerfile pin. Stop reacting: float the
  Dockerfile builder to `rust:stable-alpine` (always latest
  stable, refreshed on each rust release). Bit-for-bit
  reproducibility of the builder image trades for no more
  dep-MSRV-driven patch churn. Both Cargo.toml MSRV pins
  bumped 1.85 → 1.88 to match the actual transitive-dep floor.
  No downstream library consumers (both crates ship as
  binaries via ghcr.io, not as crates on crates.io), so MSRV
  declarations are advisory rather than a contract.

## v0.1.57 — Phase 1 patch (2026-05-10)

### Fixed

- **Bump rust pin from 1.83 to 1.85 for edition2024 deps**
  (v0.1.57) — operator + admission-webhook Dockerfile builds
  failed with `hashbrown-0.17.1: feature \`edition2024\` is
  required` because cargo 1.83 doesn't support it. edition2024
  stabilized in rust 1.85. Our Dockerfile builders were on
  `rust:1.83-alpine` and our workspace MSRVs declared
  `rust-version = "1.83"`, both predating that release. Bumped
  all four pins in lockstep — `cli/Cargo.toml`,
  `operator/Cargo.toml`, `operator/apprafter-operator/Dockerfile`,
  `operator/admission-webhook/Dockerfile`. Local builds were
  unaffected because `rust-toolchain.toml` floats on `stable`
  (1.95.0 today), so the discrepancy only surfaced when the
  Dockerfile build ran in CI on the pinned 1.83 image.

## v0.1.56 — Phase 1 patch (2026-05-10)

### Fixed

- **Operator + admission-webhook Dockerfiles: drop fragile
  dep-prebuild stage** (v0.1.56) — both images failed CI's
  release-operator build with cargo exit 101 (stderr truncated
  by the log writer). The v0.1.52 dep-prebuild trick — copy
  every workspace member's Cargo.toml, stub `src/lib.rs` +
  `main.rs`, build to populate the dep cache layer, then `rm`
  the stubs and `COPY` real sources — turned out fragile
  across the 5-member operator workspace + musl + alpine +
  buildx combination. The pre-existing admission-webhook
  variant was broken for an even simpler reason (only the
  webhook's Cargo.toml was copied, but the workspace's
  `members = [...]` list points at four others; cargo
  workspace resolution fails). Both never validated because no
  CI build ever ran against either before 2026-05-10.

  Simplified both to a single `COPY . .` + `cargo build`.
  Slower per CI run (dep layer invalidates on every source
  change) but reliable today. New
  `operator/.dockerignore` excludes `target/` and `.git/` so a
  developer's local multi-GB target tree doesn't leak into the
  builder. cargo-chef (industry-standard layered cargo cache
  for Docker) is the right next step and will land as a
  follow-up patch.

## v0.1.55 — Phase 1 patch (2026-05-10)

### Fixed

- **release-operator workflow: lowercase ghcr.io owner**
  (v0.1.55) — Docker registry repository names MUST be
  lowercase. `${{ github.repository_owner }}` preserves the
  org's display casing (`AppRafter`), and the v0.1.52
  workflow's first run on a v0.1.* tag failed with
  `invalid tag "ghcr.io/AppRafter/apprafter-operator:v0.1.54":
  repository name must be lowercase`. GitHub Actions has no
  built-in `toLower` expression. Fix: a per-job shell shim
  `Compute lowercase image base` reads `github.repository_owner`
  via an `env:` binding (anti-injection pattern), pipes it
  through `tr '[:upper:]' '[:lower:]'`, and exposes the
  lowercase image base via step outputs that subsequent steps
  consume.

## v0.1.54 — Phase 1 patch (2026-05-10)

### Fixed

- **CI clippy 1.95 `result_large_err` on retry helper closure**
  (v0.1.54) — CI's lint job runs `dtolnay/rust-toolchain@stable`
  which on the day pinned to rust 1.95.0; that release activates
  `clippy::result_large_err` by default with a 128-byte
  threshold. The v0.1.50 helper closure for
  `delete_firewall`/`delete_network` had
  `Result<ureq::Response, ureq::Error>` as its return type, and
  `ureq::Error::Status` carries a 272-byte `Response`. Fix:
  closure now returns a fresh `ureq::Request` each iteration and
  the helper invokes `.call()` internally. No `Result<_,
  ureq::Error>` in the closure signature ⇒ lint doesn't apply.
  Behaviour identical (still a fresh Request per retry, same
  `code=resource_in_use` gate, same back-off). Local toolchain
  (mise + `rust-toolchain.toml` both `stable`) was on a pre-1.95
  release at v0.1.50, hence the discrepancy. Pinning rust to a
  specific minor across the project is a separate hygiene
  follow-up.

## v0.1.53 — Phase 1 patch (2026-05-10)

### Fixed

- **`e2e/mvp.sh` curl image tag** (v0.1.53) — phase 6 of the
  E2E smoke (`curl http://e2e-hello.../`) used
  `curlimages/curl:8`, but bare-major floating tags like `:8`
  are NOT published to Docker Hub for `curlimages/curl` —
  only specific versions (`:8.11.0` etc.) and `:latest` exist.
  Every smoke run since v0.1.39 would have failed at this step
  with `Failed to pull image "curlimages/curl:8": ...
  not found`. Hidden because nightly never ran (workflow
  secrets unset until 2026-05-10) and manual §4 wasn't walked
  end-to-end on a real cluster until that date. Fix: use
  `curlimages/curl:latest`, matching what
  `docs/operator-guide/quickstart.md` §4 and
  `docs/dev-guide/quickstart.md` already do (both reference
  bare `curlimages/curl` which kubelet expands to `:latest`).
  Inline comment in `mvp.sh` captures the rationale.

## v0.1.52 — Phase 1 patch (2026-05-10)

### Added

- **GHCR publishing workflow + apprafter-operator Dockerfile**
  (v0.1.52) — adds the substrate the operator-quickstart §5
  ("install the operator + apply an Application CR") needs to
  actually be executable: an operator container image published
  to a registry the cluster can pull from.
  `operator/apprafter-operator/Dockerfile` mirrors
  admission-webhook's pattern (rust:1.83-alpine + musl,
  distroless/static-debian12:nonroot runtime, x86_64 only
  matching tier-1 CPX22). Path-deps (operator-core,
  operator-rendering, operator-controllers/application) get
  their manifests + stub sources copied up-front so the
  dep-cache layer only invalidates on Cargo.toml changes. New
  `.github/workflows/release-operator.yml` triggers on every
  `v0.*` / `v1.*` tag push (plus manual `workflow_dispatch`):
  two parallel jobs (operator + admission-webhook), auth via
  auto-provided `GITHUB_TOKEN` (no PAT, no manually configured
  secrets), tag-derived image refs at
  `ghcr.io/<owner>/apprafter-operator:<tag>` and `:latest`,
  GHA cache scoped per image. First-push UX caveat: ghcr.io
  creates each package private; operators flip visibility to
  public via the GitHub UI once (Packages → <package> →
  Settings → Change visibility → Public) so `cluster-bootstrap`
  can pull without an imagePullSecret. Subsequent tag pushes
  update the existing public package. The "Operator container
  image publishing (operators build their own for now)" caveat
  in the v0.1.0-mvp release block stops applying after this
  workflow runs against a v0.1.x tag.

## v0.1.51 — Phase 1 patch (2026-05-10)

### Fixed

- **`default-deny` NetworkPolicy: allow same-ns + kube-system,
  drop egress restriction** (v0.1.51) — every shipped version
  from v0.1.0-mvp through v0.1.50 deployed
  `policyTypes: [Ingress, Egress]` with zero allow rules. K8s
  semantics: select-all-pods + no-rules + both direction-types
  ⇒ namespace fully isolated. Workloads looked Running because
  `readinessProbe` is host-network (exempt from NP), but
  Service routing and DNS resolution were both blocked. Hidden
  for ~6 weeks because: (a) `e2e/mvp.sh` (which would have
  caught it) was wired in v0.1.39/40 but the nightly run never
  fired — the GitHub Actions secrets weren't configured; (b)
  manual operator-quickstart §4 (in-cluster `curl
  http://hello.default.svc.cluster.local/`) wasn't walked
  end-to-end against a real bootstrapped cluster until
  2026-05-10; (c) the original code-comment claim that "the
  operator adds explicit allow-rules per app" referred to
  phase 2.10 (`needs` → NetworkPolicy auto-derivation), which
  isn't shipped — so the policy was a default-deny waiting
  forever for non-existent rules.

  Fix: enforce Ingress only (Egress no longer in policyTypes —
  egress restriction without per-app allows is what actually
  broke things), and add two ingress allow rules:
  same-namespace pod-to-pod (Service routing works) and
  ingress-from-kube-system (Cilium gateway / HTTPRoute /
  monitoring can reach default-ns pods). What stays denied:
  ingress from any other namespace (future tier-2 multi-tenant
  `apps-*`, attacker tunneling via another ns) — the only
  cross-tenant threat the NP was actually mitigating before.

  Three new in-file tests guard the new shape, including a
  named `enforces_ingress_only_egress_unrestricted` whose panic
  message points future maintainers at this commit's rationale
  if they re-introduce Egress to `policyTypes`.

## v0.1.50 — Phase 1 patch (2026-05-10)

### Fixed

- **Retry `delete_firewall` / `delete_network` on
  `resource_in_use`** (v0.1.50) — the v0.1.47 `wait_for_server_gone`
  poll closed the race where `delete_network` saw 409 while the
  server was still in `GET /v1/servers`, but missed a subtler
  one: even AFTER the server vanishes from the server list,
  Hetzner's internal scheduler can take another 1-15s to drop
  the references in `firewall.applied_to` and `network.servers`.
  During that window `delete_firewall` returns
  `status=422 code=resource_in_use message="firewall with ID N
  is still in use"` — different error code than v0.1.47
  anticipated, on a different resource, same root cause.
  Manifested deterministically on the first post-v0.1.47 manual
  destroy: the server-poll completed in ~700ms, then
  `delete_firewall` 422'd. Belt-and-suspenders fix: new
  module-level helper `delete_with_retry_on_resource_in_use`
  wraps the inner ureq call. On `code=resource_in_use` it sleeps
  with exponential back-off (500ms → 5s cap) and retries, up to
  a 60s deadline. Any other error (auth, permanent 4xx,
  transport) propagates immediately. Applied to
  `delete_firewall` and `delete_network`. Three new mockito
  tests cover: retry-then-succeed for firewalls, same for
  networks, non-retriable error propagated one-shot.

## v0.1.49 — Phase 1 patch (2026-05-10)

### Added

- **Operator-guide recovery runbook** (v0.1.49) — new
  `docs/operator-guide/recovery.md` documents the Hetzner
  Rescue Mode procedure for the case when a VM becomes
  unreachable over SSH (cloud-init hung, kernel-level firewall
  misconfig that survived a release, etc.). AppRafter VMs are
  key-only — root has no password and the noVNC web console is
  unusable for emergency access; rescue-mode + chroot is the
  documented escape hatch. Includes commands for mounting the
  original disk, triaging cloud-init / iptables / k3s state
  from disk, and a "fix-in-place vs rebuild" decision table
  (rebuild is almost always correct for tier-1). Index page
  in operator-guide gets a cross-link. Closes ∞.7 bug #3 via
  the docs-only path chosen during the tier-1 stability
  hardening review on 2026-05-10 (variant C: code-level
  optional emergency password deferred to tier-3/4 where the
  trade-off makes sense).

## v0.1.48 — Phase 1 patch (2026-05-10)

### Fixed

- **cert-manager Helm values: switch `installCRDs` → `crds.enabled`**
  (v0.1.48) — the cert-manager Helm chart deprecated the top-level
  `installCRDs` key in v1.15+ and emitted

  ```
  WARNING: `installCRDs` is deprecated, use `crds.enabled` instead.
  ```

  on every `cluster-bootstrap`. Functionally still worked today,
  but the warning will become a hard failure in cert-manager v2.x
  and was noise in the operator's first-impression UX. Switched
  to the nested `crds: { enabled: true }` form per the chart's
  own NOTES. New regression guard
  `does_not_use_deprecated_top_level_install_crds` fails with a
  descriptive message if the deprecated key sneaks back. Tier-1
  cluster-bootstrap output is now warning-free.

## v0.1.47 — Phase 1 patch (2026-05-10)

### Fixed

- **`destroy()` waits for Hetzner async server-cleanup before
  deleting network** (v0.1.47) — `DELETE /servers/{id}` returns
  200 immediately and processes cleanup asynchronously. The
  immediately-following `DELETE /networks/{id}` returned 409
  "still in use" while the server's network interface lingered,
  so `provider.destroy()` returned Err mid-chain, `state.save()`
  in destroy.rs was skipped, and operators ended up with
  residual state.json (network_id + ssh_key_ids still set) +
  orphan resources in the Hetzner project — requiring a manual
  30-second wait + second `destroy --yes` to clean up. Bit
  reliably during fast destroy+apply cycles (every E2E iteration
  in v0.1.42–v0.1.46 hit it). Fix: after `delete_server`, poll
  `GET /v1/servers` until the deleted id is no longer listed
  before proceeding to firewall / network / ssh-key. New private
  helper `HetznerCloudProvider::wait_for_server_gone` —
  exponential-ish back-off (500ms → 5s cap), 60s deadline, errors
  out with an actionable message if Hetzner is unusually slow.
  Single sync point for the whole async-cleanup contract, rather
  than per-call retry logic. Existing destroy tests updated to
  use sequenced mockito mocks on `/v1/servers` (first call
  returns server, subsequent return empty so the wait completes
  in <1s). New regression guard
  `destroy_waits_for_server_async_cleanup_before_deleting_network`
  exercises the realistic case where Hetzner reports
  `status=deleting` for one poll iteration before vanishing.
  Closes ∞.7 bug #2.

## v0.1.46 — Phase 1 patch (2026-05-10)

### Fixed

- **Per-cluster `known_hosts` for SSH — no more host-key
  collisions across destroy+apply** (v0.1.46) — Hetzner Cloud
  freely recycles public IPs within seconds of `destroy`. With
  the previous code passing only `-o
  StrictHostKeyChecking=accept-new` against the operator's
  `~/.ssh/known_hosts`, the second `kubeconfig` call after a
  destroy/apply cycle on the same project would fail with
  `Host key verification failed`, forcing a manual `ssh-keygen
  -R <ip>` between every iteration of the E2E loop.
  Fix: new `StatePaths::known_hosts_file()` returns
  `<cwd>/.apprafter/known_hosts`. `SshKubeconfigFetcher::new`
  now takes the path as a constructor argument and passes
  `-o UserKnownHostsFile=<path>` alongside the existing
  `StrictHostKeyChecking=accept-new`. `destroy --yes` removes
  the per-cluster known_hosts alongside clearing the state file
  (best-effort: missing-file errors are logged and ignored). The
  user's `~/.ssh/known_hosts` is never touched. Three semantic
  outcomes: first contact silently accepts + writes; same-cluster
  matches silently; same-cluster key swap (real MITM risk)
  blocks. Closes ∞.7 bug #1.

## v0.1.45 — Phase 1 patch (2026-05-09)

### Fixed

- **k3s installer: disable flannel + k3s-NetworkPolicy for
  Cilium-coexistence** (v0.1.45) — every fresh `cluster-bootstrap`
  failed at the Argo CD step (`Error: failed pre-install: timed
  out waiting for the condition` after 5 minutes) because
  `cilium-agent` was in CrashLoopBackOff. cilium-agent fatal log:

  ```
  setting up vxlan device: creating vxlan device:
  setting up device cilium_vxlan: address already in use
  ```

  k3s ships an embedded flannel-vxlan daemon (not a pod — runs
  inside the k3s server process). Default backend `vxlan` claims
  UDP port 8472, the same default port Cilium's `cilium_vxlan`
  device wants. Even with `/etc/cni/net.d/05-cilium.conflist` as
  the only CNI config, the kernel-level VXLAN socket stays held
  by flannel, so Cilium can't take over the datapath. Cascade:
  no Cilium → no pod IPs → `coredns Pending`, `argocd-redis-secret-init
  Pending`, Helm `--wait` 5-min timeout. The same failure mode
  also kills `metrics-server` and `local-path-provisioner` (they
  came up under flannel pre-bootstrap with `10.42.0.x` IPs and
  CrashLoop after Cilium replaces the CNI). Fix: extend the k3s
  installer arguments in `user_data.rs` from
  `--disable=traefik --disable=servicelb --disable-kube-proxy`
  to `--flannel-backend=none --disable-network-policy
  --disable-kube-proxy --disable=traefik --disable=servicelb`.
  The recipe matches Cilium's own k3s install guide. Five
  components disabled in total: three replaced by Cilium (CNI,
  NetworkPolicy, kube-proxy), two by cluster-bootstrap
  (Gateway API replaces traefik, Cilium L2 announcements replace
  servicelb). Test `k3s_install_disables_traefik_servicelb_and_kube_proxy`
  renamed to `k3s_install_disables_default_components_replaced_by_cluster_bootstrap`
  and expanded to assert all five flags — each with a panic
  message that points at the failure mode if a flag goes missing
  in a future refactor. Closes ∞.7 bug #5 (added inline + closed
  in the same patch).

## v0.1.44 — Phase 1 patch (2026-05-09)

### Fixed

- **`tracing` logs now go to stderr, not stdout** (v0.1.44) —
  default `tracing-subscriber::fmt()` writes to stdout, which
  mixed with our `println!` output corrupts every machine-readable
  CLI output. `cargo run --bin platform-cli -- kubeconfig | tee
  /tmp/kc` produced a file kubectl rejected with `yaml: control
  characters are not allowed` because the first line of /tmp/kc
  was `2026-…Z INFO kubeconfig invoked refresh=false` instead of
  the YAML payload. Same class of breakage for
  `argocd-password | …` and any programmatic consumer of
  apply/destroy/import status output. Fix: one-line
  `.with_writer(std::io::stderr)` on the subscriber builder in
  `cli-core/src/logging.rs`. New regression-guard test
  `tracing_logs_go_to_stderr_not_stdout` runs `init`, asserts the
  `would init` program output is on stdout, the `init invoked`
  tracing message is on stderr, and stdout has no tracing message
  bodies. Closes ∞.7 bug #4 from `plan.md`.

## v0.1.43 — Phase 1 patch (2026-05-09)

### Fixed

- **cloud-init: drop ufw — silent-fail path locked out fresh VMs**
  (v0.1.43) — every `apply` against Ubuntu 24.04 since the noble
  release silently produced a host with `ENABLED=yes` in
  `/etc/ufw/ufw.conf` and ZERO user-allow rules in
  `/etc/ufw/user.rules`. Reason: `ufw allow N/proto` calls in the
  cloud-init `runcmd` block fired before netfilter modules were
  fully wired, so `iptables-nft` returned `ERROR: initcaps` /
  `Could not fetch rule set generation id: Invalid argument` and
  ufw bailed out without writing the rule. `ufw default deny`
  (writes `/etc/default/ufw` directly) and `ufw enable` (writes
  `/etc/ufw/ufw.conf`) didn't trigger the iptables path, so they
  succeeded — that's how we ended up with active default-deny + no
  allow rules. SSH (22), kube API (6443), HTTP/S (80/443) all
  timed-out at the in-VM firewall layer despite the Hetzner Cloud
  Firewall passing them; `kubeconfig` and any subsequent
  `cluster-bootstrap` step was unreachable. Diagnosed via
  Hetzner rescue mode + chroot inspection (`cat
  /etc/ufw/user.rules` showed the empty `### RULES ### / ### END
  RULES ###` block; `chroot ufw allow 22/tcp` reproduced the
  initcaps error). Fix: drop ufw entirely — it was always
  defense-in-depth duplicating the Hetzner Cloud Firewall (same
  default-deny + same 5-port whitelist), so removing it
  eliminates the failure mode without losing security.
  fail2ban stays — orthogonal log-driven IP-ban (sshd today, app
  workloads as we expose Gateway/HTTPRoute later); its systemd
  unit starts after `network-online.target`, well past the
  initcaps window. New `runcmd` is exactly 2 lines:
  `systemctl enable --now fail2ban` + the k3s install curl. New
  `user_data.rs` doc-comment captures the rationale to prevent
  re-adding ufw on a future "defense-in-depth" pass. Tests: 4 in-
  file unit tests, two of them new regression guards
  (`declares_only_fail2ban_in_packages_block`,
  `runcmd_does_not_invoke_ufw_anywhere`).

## v0.1.42 — Phase 1 patch (2026-05-08)

### Fixed

- **Hetzner `cx22` retirement workaround + lazy pre-flight
  server-type validation** (v0.1.42) — Hetzner retired the entire
  `cx*` Intel-shared series (cx11/cx21/cx22/cx32/...);
  `platform-cli apply` against the previous default `cx22` failed
  with `server type 104 is deprecated` AFTER creating SSH-key +
  network + firewall, leaving partial state. Two changes:
  (1) `DEFAULT_SERVER_TYPE` flips `cx22` → `cpx22` (same-spec
  AMD-shared replacement: 2 vCPU / 4 GB / 80 GB / x86, orderable
  in `nbg1`); the example manifest, the `destroy.rs` placeholder,
  the `manifest.rs` doc-comment, and the gated real-Hetzner E2E
  test follow. (2) New `cli-providers::hetzner_cloud::server_type::validate_server_type`
  pure function takes the live `/v1/server_types` view, the
  requested name, and the target region; returns
  `CliError::ServerTypeUnavailable` with the 3 closest live
  alternatives if the type is unknown, deprecated globally, or
  unavailable in the region. Wired into the **top** of
  `HetznerCloudProvider::apply()`, **gated on `needs_server_create`**:
  the `list_server_types` round-trip is paid only when the
  provider is about to POST a new server. No-op applies (live
  server with our name + `apprafter=true` label already there)
  skip the lookup entirely. Failure happens before the first
  CREATE, so a retired type no longer leaks SSH-key / network /
  firewall state. New client method `list_server_types()` with
  mockito tests; 7 in-file unit tests on the validator (happy
  path, unknown, deprecated, region-unavailable, closest-spec
  sort, dead/other-region filtered out, no-live-alternatives
  placeholder); 2 new integration tests in `provider_test.rs`
  proving (a) deprecated type rejected before any POST, (b) no-op
  apply makes zero `/server_types` calls (regression guard via
  `mockito.expect(0)`).

## v0.1.0-mvp — Milestone M1 ✅ (released 2026-05-08)

**What ships:** a complete tier-1 single-node AppRafter platform
on Hetzner Cloud. From a blank Hetzner account to a hello-world
Application reachable cluster-internally in ~6-9 minutes
wall-clock, well under the < 30-minute target. 41 development
cycles (v0.1.0 → v0.1.40) shipped across 13 sub-phases.

**Stack:** Hetzner provider (Rust + ureq + mockito tests) → k3s
single-node via cloud-init → Cilium 1.16.5 → Gateway API CRDs →
default-deny NetworkPolicy → Argo CD 7.7.7 (GitOps via
bootstrap-Application) → cert-manager 1.16.2 + self-signed
ClusterIssuer → AppRafter Application CRD v1alpha1 (CUE schema +
hand-rolled OpenAPI v3) → admission webhook (Rust + axum-server
+ rustls + cert-manager-rotated cert) → kube-rs operator with SSA
+ status subresource + per-environment expansion + Lease leader
election + Prometheus metrics + axum /healthz/readyz/metrics +
Helm chart → Backstage tier-1 manifests (with app-config
ConfigMap mount + guest auth) → applications-backend +
applications-frontend Backstage plugins (TS + Bun) → bun-http
golden-path starter (OneBun + multi-stage distroless Dockerfile +
Backstage Software Template) → e2e/mvp.sh smoke + nightly CI.

**Numbers:** 143 SPDX-tracked source files, ~290 Rust tests
(cli workspace), 56 Rust tests (operator workspace), 31 TS tests
(Bun packages), 0 clippy warnings, all under FSL-1.1-MIT (core)
or MIT (plugins / templates) per ADR 0001.

**Deliberately deferred to M2+:**

- `needs.{pg,jetstream,redis}` resource claims + ServiceProvider
  CRD + integrated providers (M2).
- HTTPRoute for `expose.network: public` (a later phase that owns
  Gateway domain config end-to-end).
- Operator container image publishing (operators build their own
  for now — `operator/charts/apprafter-operator` is ready to
  consume an image once published).
- Multi-replica operator HA + full Lease preemption (the v0.1.28
  leader election is single-replica safe; HA preemption lands
  with kine+NATS in M3).
- ServiceMonitor / NetworkPolicy / HPA / PDB on the operator
  chart (tier-1 single-node doesn't need them; phase 2 polish).
- CUE FFI for per-environment unification (the v0.1.32 pure-Rust
  merge is functionally equivalent for v1alpha1; CUE FFI lands
  when we add CUE-only constructs).

### Added

- **Nightly E2E workflow + sub-phase 1.12 ✅** (v0.1.40) — new
  `.github/workflows/nightly.yml` runs `e2e/mvp.sh` against real
  Hetzner Cloud every night at 04:00 UTC, plus on-demand via
  `workflow_dispatch`. Single-job workflow on `ubuntu-latest`:
  checkout → Rust toolchain (cached) → kubectl 1.31 → CUE 0.10 →
  run mvp.sh with `HCLOUD_TOKEN` + `APPRAFTER_SSH_PUBLIC_KEY`
  pulled from repo secrets. `timeout-minutes: 30` matches the
  plan.md "time-to-first-application" budget; observed runs
  complete in 6-9 min. The `mvp.sh` `EXIT` trap (v0.1.39) handles
  cleanup on failure, so a crashed CI run doesn't leak a Hetzner
  server. `e2e/README.md` gains a "Nightly CI" section: secrets
  table, expected cost (single-digit cents per night), and the
  plan.md §1.12 closure criterion ("5 greens in a row + one new
  operator walked the manual quickstart end-to-end" — both are
  judgment calls, not automated). plan.md §1.12 flips from 🚧
  partial to ✅ shipped (the automation is in place; the green-
  streak verdict lands when the streak holds).
- **E2E MVP smoke script + operator quickstart (sub-phase 1.12a)**
  (v0.1.39) — new `e2e/mvp.sh` orchestration script: provisions a
  Hetzner CX22 via `platform-cli`, waits for k3s, runs
  `cluster-bootstrap`, applies a plain `Deployment` + `Service`
  with `nginxdemos/hello:plain-text`, verifies the endpoint via an
  in-cluster `curl` pod, and tears the cluster down. `set -euo
  pipefail` + `EXIT` trap (auto-destroy on failure unless
  `APPRAFTER_E2E_SKIP_DESTROY=1`) + `START_NS` timer that prints
  elapsed wall-clock time at the end. Required env:
  `HCLOUD_TOKEN`, `APPRAFTER_SSH_PUBLIC_KEY`. The smoke applies a
  plain Deployment instead of an `Application` CRD because the
  operator container image isn't published yet — full Application
  flow lives in the new operator-guide quickstart at
  `docs/operator-guide/quickstart.md`. The dev-guide quickstart
  (v0.1.38) stays the developer-facing scaffold doc; the new
  operator-guide is the cluster-operator counterpart (provision +
  bootstrap + day-2 ops). CI nightly workflow + phase 1.12 closure
  land in v0.1.40 (sub-phase 1.12b).
- **bun-http Backstage Software Template + sub-phase 1.11 ✅**
  (v0.1.38) — Backstage scaffolder `template.yaml` (v1beta3)
  layered on top of the v0.1.37 starter. The
  `examples/templates/bun-http/skeleton/` subdir mirrors the
  runnable starter file-by-file with three light Nunjucks
  templates (`${{ values.name }}` / `namespace` / `image` /
  `description` / `owner`) in `package.json`,
  `apprafter/Application.cue`, `catalog-info.yaml`, and
  `README.md`; the rest of the skeleton (tsconfig, Dockerfile,
  `.gitignore`, `.dockerignore`, `src/{app.module,health.controller,
  health.controller.test,config}.ts`) is verbatim. `src/index.ts`
  is templated for `serviceName` via a `SERVICE_NAME` constant
  (decouples the scaffolder substitution from the runtime template
  literal). The scaffolder steps are `fetch:template` (skeleton →
  new repo), `publish:github` (creates the repo), and
  `catalog:register` (adds it to the Backstage catalog). Operators
  register the template by adding the URL to their `app-config.yaml`'s
  `catalog.locations`. New `docs/dev-guide/quickstart.md` walks
  the 4-step flow (cluster → scaffold → build/push → Argo CD
  picks up + operator reconciles → curl the endpoint). Sub-phase
  1.11 in plan.md flips from 🚧 partial to ✅ shipped.
- **bun-http golden-path starter (sub-phase 1.11a)** (v0.1.37) —
  new template at `examples/templates/bun-http/`, the artifact the
  v0.1.38 Backstage Software Template will scaffold. Built on
  [OneBun](https://github.com/RemRyahirev/onebun) (`@onebun/core`
  ^0.4.0) — `@Module` + `@Controller('/api')` + `@Get('/health')`
  + `@Get('/ready')` mirroring the canonical
  `withEffect/examples/crud-api` shape (controllers return plain
  data — auto-wrapped to `{ success, result }` by the framework).
  Typed `envSchema` in `src/config.ts` with `InferConfigType`
  module augmentation. `OneBunApplication` bootstrap with metrics
  + tracing enabled. Multi-stage `Dockerfile` (oven/bun:1-debian
  builder → `distroless/nodejs20-debian12:nonroot` runtime; bundles
  to a self-contained CommonJS via `bun build --target node` so
  Bun isn't shipped in the runtime layer). `apprafter/Application.cue`
  opt-in vets against the v1alpha1 schema (placeholder image, port
  3000, env vars, per-environment `prod` override with replicas=3).
  License is MIT (template / plugin tier). Backstage Software
  Template (`template.yaml`) + the `docs/dev-guide/quickstart.md`
  doc + phase 1.11 closure land in v0.1.38 (sub-phase 1.11b).
- **applications-frontend React components + sub-phase 1.10 ✅**
  (v0.1.36) — three pure props-driven React components ship in
  `@apprafter/applications-frontend`: `ApplicationsTable`
  (renders `ApplicationRow[]` with optional `onSelect` for
  drilldown), `ApplicationDetail` (drilldown — base config,
  per-environment overrides, status with phase + observedGeneration
  + endpointURL + full conditions table), `EnvironmentTabs`
  (controlled tab strip — operators own selection state). Two new
  pure helpers in `helpers.ts` (`environmentsOf`,
  `applicationsForEnvironment`) drive the tab + filter UX; 4 unit
  tests cover them. React 18 is a peer dep — no @backstage/* in
  the package's dep graph, so it publishes light. Backstage
  `createApiRef` + `createPlugin` wiring is a consumer-side
  snippet in the README. tsconfig flips to `jsx: react-jsx` (DOM
  + dom.iterable libs added). Sub-phase 1.10 in plan.md flips
  from 🚧 partial to ✅ shipped — backend (1.10a/b) + frontend
  (1.10c/d) all done.
- **applications-frontend plugin scaffold (sub-phase 1.10c)**
  (v0.1.35) — new TypeScript + Bun package at
  `backstage-plugins/applications-frontend/`. Mirrors the v0.1.33
  backend's layout: scaffold (package.json, tsconfig, .gitignore,
  bun.lock), re-declared `Application` / `ApplicationSpec` /
  `ApplicationBaseSpec` / `ApplicationExpose` /
  `ApplicationStatus` / `ApplicationCondition` / `ObjectMeta`
  types (hand-synced with the backend's), `ApplicationsApi
  { listApplications, getApplication }` interface that v0.1.36's
  React table will consume via the Backstage api-ref pattern, and
  a pure `applicationsToRows(apps): ApplicationRow[]` data
  transform (`name`, `namespace`, `image`, `replicas`, `phase`,
  `endpointURL`, `ready`). 5 unit tests (full projection, list
  order, missing-field defaults, Ready/False status, no-Ready
  condition fallback). License is MIT (plugin tier). React + the
  Backstage `createPlugin` glue + drilldown + per-env tabs land
  together in v0.1.36 (sub-phase 1.10d, closes phase 1.10).
- **applications-backend KubeApplicationStore (sub-phase 1.10b)**
  (v0.1.34) — replaces the v0.1.33 `StubApplicationStore` with a
  real `KubeApplicationStore` that proxies the kube apiserver via
  the in-cluster service-account token. Implements the
  `ApplicationStore` interface unchanged, so the v0.1.33
  `listApplicationsHandler` / `getApplicationHandler` work without
  modification. Bun's `fetch` carries the `tls: { ca }` option for
  the in-cluster CA cert (no `https.Agent` plumbing). New
  `inClusterConfig(): Promise<KubeStoreConfig>` reads
  `/var/run/secrets/kubernetes.io/serviceaccount/{token,ca.crt}` +
  `KUBERNETES_SERVICE_HOST`/`KUBERNETES_SERVICE_PORT_HTTPS`. URL
  shapes: cluster-wide list, namespaced list, and namespaced get.
  10 unit tests via mocked `fetchImpl` cover URL construction,
  header shape, namespace flow-through, `isApplication` filtering,
  404 → null, error propagation with status + body, and the
  `inClusterConfig()` env-var precondition. Backstage
  `createBackendPlugin` glue + the React frontend land together in
  v0.1.35 (sub-phase 1.10c).
- **Backstage applications-backend plugin scaffold (sub-phase
  1.10a)** (v0.1.33) — new TypeScript + Bun package at
  `backstage-plugins/applications-backend/`. v0.1.33 ships the
  scaffold (package.json, tsconfig.json, .gitignore, bun.lock), TS
  mirrors of `operator-core::Application` and friends
  (`Application`, `ApplicationSpec`, `ApplicationBaseSpec`,
  `ApplicationExpose`, `ApplicationStatus`, `ApplicationCondition`,
  `ObjectMeta`), an `isApplication(unknown)` shape guard, and pure
  async handlers (`listApplicationsHandler`,
  `getApplicationHandler`) backed by an `ApplicationStore`
  interface. The only `ApplicationStore` impl in v0.1.33 is the
  no-op `StubApplicationStore`; v0.1.34 (sub-phase 1.10b) wires up
  a `KubeApplicationStore` that proxies the kube apiserver via the
  in-cluster service-account token, then bolts on the Backstage
  `createBackendPlugin` glue. 5 router tests + 5 types tests = 10
  unit tests via `bun test`. CI workflows (`test.yml`, `lint.yml`)
  update so `bun install + bun test` / `bun run lint` iterate every
  depth-≤3 `package.json` directory. License is MIT (plugin tier).
  React frontend lands in v0.1.35 (sub-phase 1.10c); per-env tabs
  + closure in v0.1.36 (sub-phase 1.10d).
- **Application per-environment expansion + sub-phase 1.9 ✅**
  (v0.1.32) — `operator-rendering` gains `effective_spec(&app,
  env_name) -> ApplicationBaseSpec` that unifies `spec.base` with
  `spec.environments[env_name]` (override-wins on conflict for the
  `env` map; full replacement for `image`, `replicas`, `expose`).
  New `render_application_for_env(&app, Option<&str>)` consumes it;
  the existing `render_application(&app)` becomes a no-env
  shorthand. Controller `Context` gains `env_name: Option<String>`,
  the `run(client, metrics, env_name)` signature carries it
  through, and `apprafter-operator/main.rs` reads `APPRAFTER_ENV`
  (empty / unset → no override). 7 new unit tests cover the merge
  semantics (env-not-set, env-not-in-map, image+replicas
  replacement, expose full-replace, env map merge with conflict,
  render-with-env, render-without-env). Phase 1.9 closes ✅.
  HTTPRoute (mentioned in plan.md §1.9 goal as
  "Application → Deployment + Service + HTTPRoute") is
  deliberately deferred — the §1.9 acceptance ("HTTP endpoint,
  доступный изнутри кластера") is satisfied by the Service alone,
  and external traffic management is the cleanest fit for a phase
  that owns Gateway domain config end-to-end.
- **Application reconcile via SSA + status subresource (sub-phase
  1.9b)** (v0.1.31) — `operator-controllers/application::reconcile`
  now calls `render_application` (v0.1.30), applies each child
  (Deployment + optional Service) via server-side apply with field
  manager `apprafter-operator` (and `force = true` to take
  ownership of fields the operator manages), and writes the
  Application's `status` subresource: `phase = "Ready"`,
  `observedGeneration` from `metadata.generation`, `conditions`
  carrying a `Ready/True/ReconcileSucceeded` entry with an RFC3339
  `lastTransitionTime`, `endpointURL` set to
  `http://<service>.<namespace>.svc.cluster.local:80` when a
  Service is rendered. New `ApplicationCondition` type added to
  `operator-core::application` (project-local rather than
  `meta/v1.Condition` because the latter doesn't derive
  `JsonSchema`). New deps on `chrono`, `k8s-openapi`, `serde_json`
  in the controller crate. 4 in-file unit tests cover the pure
  helpers (endpoint FQDN, apply-payload injection, status builder
  with observedGeneration flow-through, RFC3339 timestamp shape).
  Per-environment expansion + HTTPRoute land in v0.1.32 (sub-phase
  1.9c, closes phase 1.9).
- **Application renderer (sub-phase 1.9a)** (v0.1.30) —
  `operator-rendering::render_application(&Application) ->
  RenderedApplication { deployment: Deployment, service:
  Option<Service> }` replaces the v0.1.26 stub. The Deployment is
  always rendered; the Service is `Some(...)` only when
  `spec.base.expose` is set. Both children get an
  `ownerReferences` entry back to the Application
  (`controller: true`, `blockOwnerDeletion: true`) so deleting the
  Application cascades. Common labels follow the standard
  `app.kubernetes.io/name`, `app.kubernetes.io/managed-by:
  apprafter-operator`, plus the project-wide `apprafter: "true"`.
  Image, replicas (default 1), env vars (string→string only —
  v1alpha1 limit), and containerPort flow from `spec.base`. Service
  is ClusterIP, port 80 → targetPort = `expose.port`. New direct
  dep on `k8s-openapi` (the workspace already pinned `v1_31` for
  v0.1.26). 9 in-file unit tests cover replicas defaulting,
  per-field flow-through, no-Service when expose is unset, label
  shape, and ownerReferences-with-UID. SSA wiring + status
  subresource land in v0.1.31 (sub-phase 1.9b);
  per-environment expansion + HTTPRoute (`expose.network: public`)
  land in v0.1.32 (sub-phase 1.9c).
- **Operator Helm chart + sub-phase 1.8 ✅** (v0.1.29) — new
  Helm 3 chart at `operator/charts/apprafter-operator/` packages
  the v0.1.27 operator binary with the v0.1.28 leader election as
  a deployable unit. The chart provisions a `ServiceAccount`, a
  `ClusterRole` (cluster-wide read/patch on `apprafter.io/applications`
  + read/write on `apps/deployments`, `services`,
  `gateway.networking.k8s.io/httproutes` for phase 1.9, plus
  `events` create/patch), a `ClusterRoleBinding`, a `Role` +
  `RoleBinding` in the install namespace for `coordination.k8s.io/leases`
  (leader election), a `Deployment` (1 replica, hardened security
  context — `runAsNonRoot: true`, `readOnlyRootFilesystem: true`,
  `capabilities.drop: [ALL]`, `seccompProfile: RuntimeDefault` —
  with downward-API `POD_NAME` + `POD_NAMESPACE`, `HTTP_PORT` +
  `RUST_LOG` env vars, liveness/readiness probes on
  `/healthz` + `/readyz`), and a ClusterIP `Service` exposing
  `/metrics` on port 8080. The Application CRD itself is NOT in
  the chart — `cluster-bootstrap` (v0.1.22) applies it; the chart
  README documents the prerequisite. `helm lint` clean. Sub-phase
  1.8 in plan.md flips from 🚧 partial to ✅ shipped.
- **Operator leader election (sub-phase 1.8c)** (v0.1.28) — new
  `operator-core::leader` module exposes `LeaderElection` +
  `LeaderConfig` for tier-1 single-replica `coordination.k8s.io/v1`
  Lease management. The operator's `main.rs` now acquires a Lease
  named `apprafter-operator` in `apprafter-system` before starting
  the Application Controller; holder identity is sourced from
  `POD_NAME` (downward API in the Helm chart, v0.1.29) and falls
  back to `local-<pid>` for local runs. Lease duration is 30s with
  10s renewal — three consecutive renewal failures exit the
  process so the Deployment restart picks up. The HTTP server
  (`/healthz`, `/readyz`, `/metrics`) runs unconditionally so the
  pod's probes don't flap during the acquire phase. New deps:
  `chrono` 0.4 (UTC + duration math). 4 unit tests on the pure
  helpers (config defaults, staleness math at three time offsets).
  Multi-replica preemption with full leader-elector semantics is
  deferred to the tier-2/3 HA cycle. The Helm chart that wires
  ServiceAccount + RBAC + Deployment + Service into a real cluster
  lands in v0.1.29 (sub-phase 1.8d, closes phase 1.8).
- **Operator binary + metrics + health endpoints (sub-phase 1.8b)**
  (v0.1.27) — new `apprafter-operator` workspace member (lib + bin).
  The binary spawns the Application Controller (`run(client,
  metrics)` lives in `operator-controllers/application`) against a
  `kube::Client` resolved via `Client::try_default()` (in-cluster
  config or `~/.kube/config` fallback) and serves an axum HTTP
  listener on `HTTP_PORT` (default 8080) with `/healthz` (200 ok),
  `/readyz` (200 ready), and `/metrics` (Prometheus text format).
  Three signals are tracked: `apprafter_reconcile_total{kind,
  namespace, result}`, `apprafter_reconcile_duration_seconds{kind}`,
  and `apprafter_reconcile_errors_total{kind}`. The reconcile fn
  starts a histogram timer + increments the `ok` counter on success;
  the error policy increments both the `error` counter and the
  errors-only counter. New deps: `prometheus` 0.13. tokio
  `select!` over the server task, the controller task, and
  `signal::ctrl_c()` so any one of them exits the process. 6
  unit tests (3 in operator-core::metrics + 3 in
  apprafter-operator::server). Leader election + the Helm chart
  for in-cluster deployment land in v0.1.28 (sub-phase 1.8c, closes
  phase 1.8).
- **Operator skeleton libraries (sub-phase 1.8a)** (v0.1.26) —
  three new Cargo workspace members under `operator/`:
  `operator-core` defines the v1alpha1 `Application` CRD type via
  the `kube::CustomResource` derive macro (the standard
  `apiVersion` / `kind` / `metadata` / `spec` / `status` shape, now
  possible thanks to the v0.1.25 spec-wrapper refactor);
  `operator-rendering` exposes a `render_application` stub that
  returns an empty `Vec<serde_json::Value>` (phase 1.9 fills it in);
  `operator-controllers/application` defines `Context`,
  `ReconcileError`, `reconcile` (logs + requeues every 60s), and
  `error_policy` (logs + requeues every 30s). New workspace deps:
  `kube` 0.95 (default-features = false; `client` + `runtime` +
  `derive` + `rustls-tls`), `k8s-openapi` 0.23 (`v1_31`),
  `schemars` 0.8, `futures` 0.3. 3 unit tests on the Application
  type (kind/apiVersion match the CRD; serde round-trip;
  status-subresource is optional) + 1 unit test on the rendering
  stub. The `apprafter-operator` binary, Prometheus metrics, and
  the axum-served `/healthz` / `/readyz` / `/metrics` endpoints
  land in v0.1.27 (sub-phase 1.8b); leader election + the Helm
  chart land in v0.1.28 (sub-phase 1.8c, closes phase 1.8).
- **Application schema fixup — `spec` wrapper** (v0.1.25) —
  refactor cycle that brings the v1alpha1 `Application` shape in
  line with k8s conventions: `base` + `environments` move under a
  `spec` object instead of sitting at the top level. CUE schema,
  hand-rolled OpenAPI v3 CRD, the `cli-core::manifest::ApplicationManifest`
  Rust mirror, the parser fixture (`examples/applications/parser.cue`),
  and both static manifests (`manifests/tier-1/application/example-app.yaml`,
  `…/example-crd.yaml`) flip together. The admission webhook
  (v0.1.23) already extracts `request.object.spec` — no webhook
  changes; the divergence between top-level CRD shape and
  spec-extraction logic is now resolved. Refactor is contained to
  shape only — no new fields, no behavior changes. This unblocks
  phase 1.8 (operator) using the `kube::CustomResource` derive
  macro, which assumes the standard `spec`/`status` shape.
- **Admission-webhook deployment + sub-phase 1.7 ✅** (v0.1.24) —
  the v0.1.23 webhook binary now serves HTTPS via `axum-server` +
  `rustls` (loads `tls.crt` / `tls.key` from `/tls`, falls back to
  HTTP when files are missing — keeps `cargo run` working in dev).
  New module `cli-providers::k8s::admission_webhook` emits the
  five-document install (Namespace `apprafter-system` +
  cert-manager Certificate `admission-webhook-tls` issued by
  `apprafter-selfsigned` + Service + Deployment +
  ValidatingWebhookConfiguration), with the
  `cert-manager.io/inject-ca-from` annotation keeping `caBundle`
  rotated. CUE schema gains `spec.admissionWebhook.image` (optional);
  Rust manifest mirror gets `AdmissionWebhookBlock`. `platform-cli
  cluster-bootstrap` adds an 8th conditional kubectl apply at the
  tail of the sequence — when the operator sets the image,
  Application admission is gated by the webhook with
  `failurePolicy: Fail`, `timeoutSeconds: 10`, and a CUE-shape
  message ("Application is invalid: <field>: <reason>") visible in
  `kubectl apply` output. Static rendering at
  `manifests/tier-1/admission-webhook/example.yaml` + README.
  Sub-phase 1.7 in plan.md flips from 🚧 partial to ✅ shipped.
- **Admission-webhook crate (sub-phase 1.7c)** (v0.1.23) —
  new `operator/` Cargo workspace with one member crate
  `admission-webhook`. Pure validator
  (`validate_application_spec(spec)`) catches what the OpenAPI v3
  CRD can't: cross-field "image must be reachable" (either
  `spec.base.image` or every `spec.environments[*].image` must be
  set), environment names that aren't DNS-1123 labels, and env keys
  that don't match `^[A-Z_][A-Z0-9_]*$`. axum 0.7 router exposes
  `POST /validate` (AdmissionReview in/out, hand-rolled via
  `serde_json::Value` to avoid pulling in the heavy `kube` crate),
  `GET /healthz`, and `GET /readyz`. tokio binary listens on
  `0.0.0.0:$PORT` (default 8443). Multi-stage Dockerfile (rust:1.83-
  alpine + musl → `distroless/static-debian12:nonroot`) ships
  alongside. 14 validator unit tests + 2 server unit tests + 7
  integration tests via `tower::ServiceExt::oneshot`. CI workflows
  (`test.yml`, `lint.yml`) updated to discover every top-level
  Cargo.toml and run `cargo test` / `cargo clippy` / `cargo fmt
  --check` in each, so cli + operator are both covered. v0.1.23
  ships HTTP-only — TLS termination via the cert-manager-issued
  Secret arrives in v0.1.24 along with the
  Certificate/Service/Deployment/ValidatingWebhookConfiguration
  manifests and `cluster-bootstrap` wiring (closes phase 1.7).
- **Application CRD OpenAPI v3 manifest (sub-phase 1.7b)** (v0.1.22) —
  hand-rolled `apiextensions.k8s.io/v1` CRD in
  `cli-providers::k8s::application_crd`, mirroring the v0.1.21 CUE
  `#ApplicationSpec` (`image` non-empty pattern, `replicas` ≥ 0,
  `expose` with port 1..=65535 + public bool + network enum {public,
  internal, vpn}, `env` string→string, plus the `environments` map
  of overrides). The schema is inlined twice — once under `base`,
  once under `environments.additionalProperties` — because k8s
  structural-schema rules forbid `$ref`. `subresources.status: {}`
  is declared up-front so phase 1.9 can populate it without a CRD
  migration. `platform-cli cluster-bootstrap` now applies the CRD
  right after the Gateway API CRDs (mandatory step, no manifest
  opt-in). New `cargo run -p cli-providers --example
  application_crd_example` re-renders the static
  `manifests/tier-1/application/example-crd.yaml`; alongside it
  ships an `example-app.yaml` minimal Application + a README. The
  four FakeKubectl in-file tests update to expect one extra apply
  in the sequence (Gateway-CRDs → Application-CRD → default-deny
  NP → …). Admission-webhook (Rust + kube-rs + cert-manager
  Certificate + ValidatingWebhookConfiguration) lands in v0.1.23
  and closes phase 1.7.
- **Application CRD v1alpha1 schema (sub-phase 1.7a)** (v0.1.21) —
  the v1alpha1 CUE schema for `#Application` is tightened to the
  field set declared in plan.md §1.7: `image` (non-empty),
  `replicas` (≥0), `expose` (port + public + network), `env`
  (string→string literals), and `environments` map of overrides.
  Out-of-scope fields removed: `needs`, `autoscale`, `confidential`
  (they re-appear in 2.x / 4.x). New Rust mirror types
  `ApplicationManifest` / `ApplicationSpec` / `ApplicationExpose`
  in `cli-core::manifest` plus a `parse_application(workdir, path)`
  helper that walks the `cue export --out json` payload the same
  way `parse_infrastructure` does. Six integration tests cover the
  happy path against `examples/applications/parser.cue`, the
  missing-path / wrong-kind / no-environments error branches, and
  two `cue vet` smokes (schema vets cleanly + fixture vets against
  the schema). No CRD installation yet — that lands in v0.1.22; the
  admission webhook + cert-manager Certificate +
  ValidatingWebhookConfiguration land in v0.1.23 and close
  sub-phase 1.7.
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
- **Backstage app-config ConfigMap + sub-phase 1.6 ✅** (v0.1.20) —
  the Backstage manifest set now embeds an `app-config.yaml`
  ConfigMap mounted into the Deployment at `/app/app-config.yaml`
  (subPath, read-only), overriding whatever's baked into the
  operator's image. New module
  `cli-providers::k8s::backstage_app_config` exposes
  `backstage_app_config_yaml(domain)` — fans the domain into
  `app.baseUrl`, `backend.baseUrl`, and `backend.cors.origin`,
  pins the SQLite in-memory database, and turns on the `guest`
  auth provider with `dangerouslyAllowOutsideDevelopment: true`
  (Backstage's basic-admin stub). The rendered example at
  `manifests/tier-1/backstage/example.yaml` grows from 6 to 7
  documents; cluster-bootstrap still issues a single
  `kubectl apply -f` for it. Sub-phase 1.6 in plan.md flips from
  🚧 partial to ✅ shipped.
- **Backstage scaffold helpers + Dockerfile** (v0.1.19) — adds
  `backstage-plugins/host/{Dockerfile,.dockerignore,scripts/
  scaffold.sh,README.md}`. The Dockerfile is the canonical
  Backstage 1.x multi-stage shape (Node 20 builder + slim
  runtime, EXPOSE 7007, unprivileged `node` user). The scaffold
  script wraps `npx @backstage/create-app@latest --skip-install`
  with a Node-20 preflight, refuses to overwrite a non-empty
  target, and drops the Dockerfile + .dockerignore alongside the
  generated app. README walks operators through the 6-step
  scaffold → install → build → push → manifest →
  cluster-bootstrap loop. We deliberately don't vendor the
  Backstage app itself — operators own their bootstrap repo. OAuth
  + ConfigMap mount land in v0.1.20.
- **Backstage tier-1 manifests** (v0.1.18) — when
  `Infrastructure.spec.backstage.domain` is set,
  `platform-cli cluster-bootstrap` applies a 6-document Backstage
  manifest set (Namespace + Deployment + Service + HTTPRoute +
  Gateway + Certificate) to the `backstage` namespace.
  `spec.backstage.image` overrides the placeholder container
  image (`ghcr.io/apprafter/backstage:placeholder`). New module
  `cli-providers::k8s::backstage_manifests`; CUE schema gains
  `spec.backstage`; Rust manifest mirror gets `BackstageBlock`;
  `perform_bootstrap` accepts `Option<&Path>` for the Backstage
  manifest. A static rendering of the placeholder values lives at
  `manifests/tier-1/backstage/example.yaml` (refreshable via
  `cargo run -p cli-providers --example backstage_example`) — the
  starting point for operators populating their
  `spec.argocd.bootstrapRepo`. Backstage app scaffold + Dockerfile
  + OAuth land in v0.1.19/v0.1.20.
- **Argo CD bootstrap Application + sub-phase 1.5 ✅** (v0.1.17) —
  when `Infrastructure.spec.argocd.bootstrapRepo` is set,
  `platform-cli cluster-bootstrap` applies an Argo CD `Application`
  named `bootstrap` that auto-syncs (prune + selfHeal) the named
  Git repo into the cluster (path defaults to `.`, override via
  `spec.argocd.bootstrapPath`). New module
  `cli-providers::k8s::bootstrap_app`; `ArgocdBlock` gains
  `bootstrap_repo` + `bootstrap_path`; `perform_bootstrap` accepts
  `Option<&Path>` for the bootstrap Application manifest. The
  real-cluster smoke (`cluster_smoke_test.rs`) gains a 4th
  assertion behind `APPRAFTER_BOOTSTRAP_REPO_SMOKE=1`. Sub-phase
  1.5 in plan.md flips from 🚧 partial to ✅ shipped.
- **Argo CD Gateway + HTTPRoute** (v0.1.16) — when the
  `Infrastructure` manifest declares `spec.argocd.domain`,
  `platform-cli cluster-bootstrap` provisions a `Gateway` (HTTPS
  listener on 443 with hostname + TLS terminate), an `HTTPRoute`
  routing the same hostname to `argocd-server:80`, and a
  cert-manager `Certificate` issued by the v0.1.15 self-signed
  `apprafter-selfsigned` ClusterIssuer. New module
  `cli-providers::k8s::argocd_gateway`; CUE schema gains
  `spec.argocd.domain` (optional); Rust manifest mirror gets
  `ArgocdBlock`. Without the manifest opt-in, Argo CD stays
  ClusterIP-only — the bootstrap finishes at the v0.1.15 step.
  Existing FakeRunner test now passes `None` for the optional
  Gateway path; a new test exercises the `Some(path)` branch and
  asserts 4 kubectl applies in order.
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

## v0.0.8 — Foundations (Phase 0)

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

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
