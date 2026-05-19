# Changelog

All notable changes to AppRafter. The format follows
[Keep a Changelog] v1.1.0. Pre-1.0 development tracks patches as
`MAJOR.MINOR.PATCH` where the minor matches the plan.md phase and
the patch matches the (sub-)subphase. Milestone tags
(`v0.1.0-mvp`, `v0.2.0-services`, etc.) point at the closing
patch of each phase.

## Phase 2 — Platform services (in progress)

_No entries yet — Phase 2 (M2) opens with v0.2.0._

## Phase 1.5 — Self-managing platform rethink (in progress)

## v0.1.98 — walk-fix: redis-ha pre-install timeout on single-node (2026-05-19)

Real-Hetzner walk of v0.1.97 surfaced a regression. The new
GitOps loader's `helm install argocd argo/argo-cd 7.7.7`
failed on single-node k3s with:

```
Error: failed pre-install: 1 error occurred:
        * timed out waiting for the condition
```

Root cause: the upstream `argo-cd` 7.7.7 chart defaults
`redis-ha.enabled: true`, which schedules **3 redis-ha pods**
plus an haproxy with `requiredDuringSchedulingIgnoredDuringExecution`
`podAntiAffinity`. On a single-node cluster those pods can
never become Ready. The chart's pre-install hook waits on them
and times out.

v0.1.x's in-tree imperative install explicitly disabled
redis-ha (`cli-providers::k8s::argocd_values_yaml`). The
v0.1.97 GitOps rewrite shrunk the loader values and
accidentally dropped that flag.

### Changed

- **`commands/cluster_bootstrap.rs::argocd_loader_values_yaml`**
  restores three values from the v0.1.x baseline:
  - `redis-ha.enabled: false` — primary fix for the timeout.
  - `notifications.enabled: false` — saves one more
    deployment on tier-1 cpx22 RAM (was `replicas: 1`).
  - `server.service.type: ClusterIP` — keeps the loader from
    exposing anything before the chart's
    `component_argocd.cue` wires Gateway/HTTPRoute.

- **`platform-stack/cue/component_argocd.cue`** mirrors the
  same tier-1 defaults so the chart's self-reconcile doesn't
  re-enable redis-ha when it adopts the loader's release.
  Without this mirror the loader installs fine, but the first
  Argo CD reconcile of its own Application would flip redis-ha
  back on and break.

- **Chart `currentVersion: 0.1.2 → 0.1.3`** plus the matching
  `compatibility.cue` entry. Change class `safe` — pure tier-1
  default refinement, no new components.

- **CLI `RELEASED_PLATFORM_STACK_VERSION`: `"0.1.2" → "0.1.3"`**
  so `apprafter bootstrap-all` pulls the fixed chart.

### Tests (2 new)

- `argocd_loader_values_disables_redis_ha_for_single_node_k3s`
  pins the `redis-ha.enabled: false` flag in the loader
  values so future refactors can't silently drop it again.
- `argocd_loader_values_keep_server_at_cluster_ip_until_chart_exposes_it`
  pins the `server.service.type: ClusterIP` value for the
  loader. Total: 551 passed.

- The pre-existing `argocd_loader_values_keeps_replicas_at_one_for_initial_install`
  test was tightened to assert `notifications.enabled: false`
  instead of `replicas: 1`.

### Recovery for operators stuck mid-install

If you ran `apprafter bootstrap-all` on v0.1.97 and saw the
timeout, the cluster has:

- A working VM + k3s.
- A failed (`pending-install`) Argo CD helm release.

`helm upgrade --install` against the fixed values usually
adopts the failed release cleanly. If it doesn't:

```sh
KUBECONFIG=$(apprafter kubeconfig --refresh) \
  helm delete argocd -n argocd || true
KUBECONFIG=$(apprafter kubeconfig) kubectl delete ns argocd || true
apprafter bootstrap-all
```

The `apply` + `k3s-ready` phases stay idempotent — they'll
no-op on the existing VM. The `bootstrap` phase reruns clean.

### Note on chart 0.1.2

0.1.2's `compatibility.cue` entry now carries a "known issue"
note pointing operators at 0.1.3. The 0.1.2 OCI artifact stays
on the registry as a historical record; future installs default
to 0.1.3 via the CLI's `RELEASED_PLATFORM_STACK_VERSION` bump.

## v0.1.97 — M1.5 Track B.1.70 — minimal cluster-bootstrap rewrite (2026-05-19)

ADR 0025 lands. The CLI's `cluster-bootstrap` step shrinks
from ~1250 lines of imperative `helm install` / `kubectl
apply` of seven components into a four-step GitOps loader
that hands the platform layer off to Argo CD. The
platform-stack chart published in 1.66–1.69 is now the actual
installer; this CLI just bootstraps Argo CD enough to reach
the chart.

### Changed

- **`commands/cluster_bootstrap.rs` rewritten end to end.**
  Four steps:
  1. `helm upgrade --install argocd argo/argo-cd` with
     loader-only values (single replicas, dex off). The
     platform-stack chart's `component_argocd.cue` overlay
     adopts this release on first reconcile.
  2. `kubectl wait --for=condition=Available
     deployment/argocd-server` — gates the next step (root
     Application apply fails with "no matches for kind"
     before the CRD is installed).
  3. `kubectl apply -f <root-application.yaml>`. The root
     Application points at `oci://ghcr.io/<owner>/platform-stack`
     chart `platform-stack` at the
     `RELEASED_PLATFORM_STACK_VERSION` pinned in the CLI
     binary (currently `0.1.2`).
  4. `kubectl wait
     --for=jsonpath='{.status.health.status}'=Healthy
     application/platform` — once Healthy, child Applications
     (cilium, cert-manager, argocd self-managing,
     apprafter-operator, admission-webhook, network-policies,
     conditionally Backstage) are reconciling under Argo CD.

- **Imperative install code deleted.** Cilium, cert-manager,
  Application CRD, default-deny NetworkPolicy, ClusterIssuer,
  the operator Helm release, the admission-webhook manifest,
  the bootstrap Application, the Argo CD repo-creds Secret,
  the Argo CD Gateway+HTTPRoute+Certificate triad, and the
  Backstage manifest set no longer appear in the CLI. Their
  renderers in `cli-providers::k8s::*_yaml()` stay around as
  the chart's parallel source of truth until sub-phase 1.71
  cuts the duplication.

- **`KubectlRunner` trait gained `wait_for_condition`.**
  Supports both `--for=condition=<X>` (deployment readiness)
  and `--for=jsonpath=...=<value>` (Argo CD Application
  health). Resource refs that need a label selector split on
  whitespace inside the wrapper so `Command::args` doesn't
  pass them as one quoted token.

- **Three new constants in `cli-providers::k8s`**:
  `RELEASED_PLATFORM_STACK_VERSION = "0.1.2"`,
  `APPRAFTER_PLATFORM_STACK_DEFAULT_REPO = "oci://ghcr.io/apprafter"`,
  `APPRAFTER_PLATFORM_STACK_CHART_NAME = "platform-stack"`.
  Bump `RELEASED_PLATFORM_STACK_VERSION` in lockstep with
  each `platform-stack/v*` publish.

### Tests

The 13 tests that pinned the old `perform_bootstrap` shape
(component install ordering, optional component handling,
SSA secret apply, etc.) are deleted along with the code they
pinned. Five new tests cover the GitOps loader:

- `perform_bootstrap_installs_argocd_then_applies_root_then_waits_for_healthy`
  pins the full call sequence: 1 helm repo_add, 1 helm
  install (argocd only), 1 client-side apply (root
  Application), 0 server-side applies, 2 waits in the right
  order.
- `render_root_application_includes_repo_url_and_chart_version`
  + `render_root_application_uses_argocd_namespace_destination`
  pin the rendered YAML structure.
- `argocd_loader_values_keeps_replicas_at_one_for_initial_install`
  pins the loader-only values shape.
- Existing two `decrypt_cached_kubeconfig_*` helper tests
  preserved (unchanged behaviour).

Test totals: 549 passed (down from 565 — net of −13 + 5).
fmt + clippy + spdx clean.

### Acceptance (real-cluster verification deferred)

The functional acceptance — fresh Hetzner →
`apprafter init && apprafter bootstrap-all` → tier-1 cluster
with 6+ child Applications all Healthy + Synced — can only
be verified against a live cluster. Pending:

- First real-Hetzner walk after `platform-stack/v0.1.2` lands
  in OCI.
- `kubectl get applications.argoproj.io -A` should list
  `platform` + 6 children.
- `kubectl edit application cilium -n argocd` should
  reconcile back.
- Re-run idempotency.

### Out of scope (deferred)

- Per-component progress sub-bars in `bootstrap-all` Phase 3.
- `apprafter cluster-bootstrap --manifest <path>` flag — the
  `APPRAFTER_MANIFEST` env var still works.
- Partial-state recovery on wait timeout.
- `e2e/mvp.sh` rewrite — drives the imperative install today.

### Migration note for operators

An existing v0.1.x cluster bootstrapped via the imperative
path: this CLI does not touch your existing Argo CD release
(`helm upgrade --install` adopts it). But applying the root
Application will replace the imperative Cilium / cert-manager
/ etc. helm releases with Argo CD-managed Applications.
Expect a single reconcile cycle where existing resources
transition from "client-side managed fields" to "Argo CD
field manager". Plan a maintenance window for the first
upgrade.

## v0.1.96 — chart-versioning policy: first published is 0.1.0 (2026-05-15)

Policy fix discovered before pushing the first chart release.
Previous plan was to ship `platform-stack/v0.2.0` aligned with
the upcoming `v0.2.0-services` AppRafter milestone. Walk
revealed a cleaner mental model: **chart MINOR tracks the
monorepo's PHASE number**, not the milestone target. We're in
Phase 1.5 → chart 0.1.x. Phase 2 services land → chart MINOR
bumps to 0.2.0 alongside `v0.2.0-services`. Chart patch
versions and monorepo patch versions remain independent;
the two share only MINOR/MAJOR semantics.

### Changed

- **`platform-stack/cue/tier_solo.cue`** + **`tier_team.cue`** —
  `version: "0.2.0"` → `"0.1.0"`.
- **`platform-stack/cue/compatibility.cue`** — entry key
  renamed `"0.2.0"` → `"0.1.0"`; the surrounding doc-comment
  now explains the phase-tracking rule instead of the
  milestone-alignment one.
- **`platform-stack/cue/platform.cue`** — `#Version` doc-block
  updated.
- **4 component doc-comments** (cilium / cert-manager /
  argocd-cue-cmp / apprafter-operator / admission-webhook)
  flipped their `// platform-stack 0.2.0 …` references to
  `// platform-stack 0.1.0 …`.
- **`platform-stack/CHANGELOG.md`** — `0.2.0 (planned)` →
  `0.1.0 (planned — first published chart release)` with the
  phase-tracking rationale.
- **`platform-stack/RELEASE.md`** — versioning rules section
  rewritten; tagging examples now show `platform-stack/v0.1.0`
  / `v0.1.0-rc1` instead of `v0.2.0` / `v0.2.0-rc1`.

### Verified

- `make -C platform-stack render-only` produces
  `dist/platform-stack-0.1.0/` (no `-0.2.0/` artifact left
  behind — the renderer keys off `tier1.version`).
- `helm lint` clean.
- `bash scripts/check-platform-stack-version.sh 0.1.0` →
  success, prints the YAML record.
- `bash scripts/check-platform-stack-version.sh 0.2.0` →
  exit 1 (no longer a recognised version — exactly the
  fail-fast behaviour the workflow's compatibility gate
  needs).
- `bash scripts/lint-cue.sh` clean.
- SPDX gate stays at 169 (no new files).
- `cargo test --workspace` stays at 565 passed (no Rust
  changes).

### What this unblocks

- Push `platform-stack/v0.1.0-rc1` as the first real-tag
  smoke test of the publish workflow (after the next
  `git push origin master`). The workflow's compatibility
  gate now accepts the version, the chart renders to a
  sensibly-named path, and the first OCI artifact lands at
  `ghcr.io/<owner>/platform-stack:0.1.0-rc1` instead of the
  semantically-confused `0.2.0-rc1` ("we shipped 0.2.0 before
  the M2 services milestone? what?").

### Note on ADR 0028

ADR 0028 is marked **Draft** and contains examples using
`0.2.0`. The ADR captures the design decisions
(OCI distribution, CUE source, dual-channel publishing) —
specific version numbers in its examples are illustrative,
not normative. No ADR rewrite needed; the version policy
lives in `RELEASE.md` and `compatibility.cue` (the
authoritative places).

## v0.1.95 — hotfix: workflow syntax error after v0.1.94 push (2026-05-15)

GitHub Actions rejected `platform-stack-publish.yml` on push
with `(Line: 232, Col: 14): Unexpected symbol: '…'`. Root cause:
one comment line inside a `run: |` block contained the literal
sequence `${{ … }}` (with a Unicode ellipsis between the
expression braces). GHA evaluates `${{ }}` expressions in `run:`
scalar bodies BEFORE the shell sees the script, including in
shell comments — the parser tried to evaluate `…` as an
expression and bailed.

### Changed

- **`.github/workflows/platform-stack-publish.yml`** — rewrote
  the offending comment without the `${{ }}` symbol. Net
  effect on functionality: zero (it was a security-rationale
  comment). Net effect on YAML validity: workflow now parses
  cleanly.

### Why this slipped past local validation

`yamllint` validates YAML structure (indentation, quoting, key
ordering) but doesn't run the GitHub Actions expression
evaluator. The only way to catch this kind of issue without
the real runner is `actionlint` (or `act` for local
simulation). Adding `actionlint` to the pre-commit chain is a
candidate for a future hardening pass; today the cost is the
~3-minute round trip via real GitHub Actions on push.

### Workflow-author rule of thumb

Inside any `run: |` block, never use `${{ }}` even in a
comment — neither bash nor GHA expression evaluator treats
`#` as a comment marker for THEIR parsing pass. Use plain
text descriptions referencing the syntax by name.

`bash scripts/lint-cue.sh` clean. `cargo test --workspace`
stays at 565 passed (no Rust changes).

## v0.1.94 — M1.5 Track B.1.68 — publish workflow + cosign signing (2026-05-15)

Third Track B slice. 1.66 landed CUE source, 1.67 turned it into
a rendered chart, this slice publishes that chart on tag with a
cosign signature attached. Maintainer flow after this lands:

```sh
# 1. Add compatibility entry for the new version.
# 2. Tag and push.
git tag platform-stack/v0.2.0-rc1
git push origin platform-stack/v0.2.0-rc1
# 3. Workflow runs end-to-end: render → lint → sign → push.
# 4. Verify the published artifact.
cosign verify ghcr.io/<owner>/platform-stack@<digest> \
    --certificate-identity-regexp 'https://github.com/<owner>/<repo>/' \
    --certificate-oidc-issuer 'https://token.actions.githubusercontent.com'
```

### Added

- **`.github/workflows/platform-stack-publish.yml`** — single
  job, 15 steps:
  1. Checkout.
  2. Resolve version from tag (strips `platform-stack/v`
     prefix) or workflow-dispatch input. Detects pre-release
     via `-` in version (controls `:latest` retag + GitHub
     Release `prerelease:` flag).
  3. Compute lowercase owner (ghcr requires lowercase) — same
     shim pattern as `release-operator.yml`.
  4. Install tooling: `cue-lang/setup-cue@v1` + `azure/setup-helm@v4`
     + `sigstore/cosign-installer@v3`.
  5. **Compatibility gate** — `bash scripts/check-platform-stack-version.sh "$VERSION"`
     fails fast with a pointer-to-fix when `compatibility.cue`
     has no entry for the tagged version. This is the contract
     `PlatformController` (Phase 2+) relies on; a missing entry
     would surface as a stuck reconcile in production.
  6. `make -C platform-stack render-only` (the 1.67 path).
  7. `helm lint`.
  8. `helm template` smoke for **both** tiers: assert 6 tier-1
     Applications present + Backstage on tier-2.
  9. `helm package` → `.tgz`.
  10. Log in to ghcr.io via the auto-provided `GITHUB_TOKEN`.
  11. `helm push` to `oci://ghcr.io/<owner>` (Helm 3.8+ native
      OCI). Resolve the immutable digest via `helm show chart`
      — cosign signs digests, not mutable tags (Sigstore best
      practice).
  12. `cosign sign --yes "${IMAGE}@${DIGEST}"` — keyless via
      Sigstore OIDC + GitHub Actions ambient identity
      (`id-token: write` permission, no managed keys).
  13. `cosign sign-blob` for the `.tgz` → `.tgz.sig` +
      `.tgz.pem` detached signature pair, for users who consume
      the GitHub Release attachment via plain Helm.
  14. `oras tag :latest` on stable releases (graceful warning
      if the runner image drops the `oras` CLI).
  15. `gh release create` with heredoc-built install/verify
      notes (written to a temp file and consumed via
      `--notes-file` — never passed as a multi-line bash arg),
      attaching `.tgz` + `.sig` + `.pem`, `--prerelease` flag
      on pre-release tags.

- **`scripts/check-platform-stack-version.sh`** — standalone
  helper, callable both from CI and from a maintainer's local
  shell as the pre-tag sanity check. Resolves the `cue` binary
  the same way `lint-cue.sh` does (local install →
  `nix run nixpkgs#cue --` fallback). Tested on:
  - `0.2.0` → success, prints the matching YAML record.
  - `99.99.99` → exit 1 with a human-readable instruction
    block telling the maintainer exactly what to add to
    `compatibility.cue`.

- **`platform-stack/RELEASE.md`** — full maintainer procedure:
  semver rules + the "0.2.0 is the first published version"
  explanation, pre-release checklist (compatibility entry +
  accurate change class + operator version + CHANGELOG entry +
  local render passes), tagging instructions for both
  pre-release and stable paths, after-publish actions
  (verification in a clean env, `RELEASED_OPERATOR_VERSION`
  bump if paired, `UNRELEASED.md` pointer), and failure-mode
  recovery (tag delete + re-push).

### Security hardening

- Every dynamic value (`github.ref_name`,
  `github.repository_owner`, `github.event.inputs.version`,
  `github.repository`) is routed through an `env:` binding
  rather than direct `${{ }}` interpolation into `run:`
  bodies. This is the documented anti-workflow-injection
  pattern, the same one `release-operator.yml` already
  follows.
- The cosign signature targets the **digest**, never the
  mutable tag — a follow-up push that overwrote the tag would
  not invalidate the signed digest, so consumers can pin to
  a known-good digest regardless of how the tag drifts.
- `gh release create` consumes the notes body via a temp
  file (`mktemp` + `--notes-file`), not as a multi-line shell
  arg, so heredoc content never reaches command-substitution
  parsing.

### Tests

CI-side acceptance — the workflow's end-to-end behaviour can
only be verified by pushing a real `platform-stack/v0.2.0-rc1`
tag, which is a one-shot maintainer action outside the scope
of this commit. Locally I validated:

- `bash scripts/check-platform-stack-version.sh 0.2.0` →
  exit 0, prints the YAML record.
- `bash scripts/check-platform-stack-version.sh 99.99.99` →
  exit 1, prints the fix-it-up instruction block.
- `yamllint -d relaxed .github/workflows/platform-stack-publish.yml`
  → clean.
- `bash scripts/lint-cue.sh` → clean.
- SPDX gate: 167 tracked source files → 170 after staging
  (`+ .yml + .sh + RELEASE.md`).
- `cargo test --workspace` → 565 passed (no Rust changes).

### Out of scope

- Smoke install into a real `kind` cluster within the
  workflow. `helm template` smoke + `helm lint` cover chart
  shape and template-time errors; kind install would extend
  workflow runtime ~3 minutes for marginal coverage. Promote
  when the first real-world bug slips past template-time
  validation.
- SLSA Level 3 build provenance attestation. cosign already
  provides keyless artifact provenance; SLSA Level 3 demands
  hermetic builds via `slsa-github-generator`. Defer to the
  M3 compliance pass.
- Multi-architecture OCI manifest list. The chart is a Helm
  artifact (architecture-neutral); sub-charts pulled at
  install time select arch on the user's cluster.

## v0.1.93 — M1.5 Track B.1.67 — chart renderer pipeline (2026-05-15)

Second Track B slice. The 1.66 scaffold provided the CUE
source; this slice turns it into a fully-lintable Helm umbrella
chart on disk. End-to-end loop:

```sh
cd platform-stack && make render
# ⇒ dist/platform-stack-0.2.0/
#   Chart.yaml + values.yaml + values.schema.json
#   templates/applications.yaml
#   compatibility.yaml + README.md
#   examples/values.solo.yaml + examples/values.team.yaml
helm lint dist/platform-stack-0.2.0          # → 0 errors, 1 INFO
helm template platform dist/platform-stack-0.2.0
#   → 6 Argo CD Applications (tier-1 default)
```

No CUE source ever leaves `platform-stack/cue/`; the rendered
chart is purely a CI / publish artifact, gitignored.

### Added

- **`platform-stack/cue/render_tool.cue`** — CUE `command:
  render: { ... }` declared using the `tool/file` package.
  Nine tasks chained via `$dep`:
  - `mkdist` / `mktemplates` / `mkexamples` — `file.Mkdir` with
    `createParents`.
  - `chartYaml` — Chart.yaml v2 with `apprafter.io/change-class`
    and `apprafter.io/operator-version` annotations pulled from
    `compatibility.cue`.
  - `valuesYaml` — defaults to `tier1` (solo). Operators who
    `helm install platform-stack` without `--values` get the
    same baseline the in-tree v0.1.x cluster-bootstrap installs.
  - `valuesSchemaJson` — hand-rolled Helm-native JSON Schema
    (draft-2020-12). CUE's auto-export targets draft-07, which
    Helm refuses. Required: `version` / `tier` / `channel` /
    `components`; `tier` enum `[1, 2, 3, 4]`; `channel` enum
    `["stable", "edge"]`; per-component pattern constraint on
    the map keys; `additionalProperties: false`.
  - `appsTemplate` — the umbrella chart's **single** Go
    template iterating over `.Values.components`. Emits one
    Argo CD `Application` per enabled entry. Labels
    `apprafter.io/{component,tier,channel}`. Conditional
    `helm.valuesObject` block only when `source.chart` is set
    (Git-source components like `network-policies` and
    `backstage` skip it). SSA + auto-create-namespace flow
    through from CUE base.
  - `compatibilityYaml` / `soloExample` / `teamExample` /
    `readme` — per-tier example values + an in-chart README
    pointing back at the canonical AppRafter monorepo CUE
    source.

- **`platform-stack/Makefile`** — `make render` /
  `render-only` / `lint` / `clean` / `help`. Auto-detects
  `cue` and `helm` on `PATH`, falls back to
  `nix run nixpkgs#cue --` / `nix run nixpkgs#kubernetes-helm --`
  so anyone with Nix available picks them up automatically
  even outside the dev shell. The chart version reads from
  the CUE source itself via `cue export ... -e tier1.version`,
  so the Makefile never hard-codes a version path.

- **`Justfile`** — `just platform-stack-render` and
  `just platform-stack-check` wrappers around the Makefile
  for project-level convenience.

- **`platform-stack/README.md`** Local-development section
  rewritten with the real `make render` / `helm template` /
  `--set tier=99` schema-gate examples, replacing the
  scaffold-era "lands in 1.67" placeholders.

### Changed

- Nothing in the existing CUE production package files
  (`platform.cue`, `component_*.cue`, `tier_*.cue`,
  `compatibility.cue`). The renderer reads them via the
  `package platformstack` import; they don't depend on
  `tool/*` and remain `cue vet`-clean without it.

### Verified

- `make -C platform-stack render-only` emits 8 files in
  `dist/platform-stack-0.2.0/`.
- `helm lint dist/platform-stack-0.2.0` → 0 errors, 0
  warnings, 1 INFO (chart icon recommendation — cosmetic).
- `helm template platform dist/platform-stack-0.2.0` (default
  / tier-1 values) → 6 Argo CD Applications:
  `admission-webhook` + `apprafter-operator` + `argocd` +
  `cert-manager` + `cilium` + `network-policies`. Backstage
  + argocd-cue-cmp correctly disabled.
- `helm template platform dist/platform-stack-0.2.0 --values
  dist/platform-stack-0.2.0/examples/values.team.yaml` → 7
  Applications (Backstage enabled, Hubble relay+UI in cilium
  values). argocd-cue-cmp still disabled until the 1.69
  sidecar wiring.
- `helm template platform dist/platform-stack-0.2.0 --set
  tier=99` → schema rejects: `value must be one of 1, 2, 3,
  4`. Validates the `values.schema.json` gate before Argo CD
  ever sees the input.
- `bash scripts/lint-cue.sh` clean; SPDX gate covers the
  new files (167 → 169 tracked source files after staging).
- `cargo test --workspace` stays at 565 passed (no Rust
  changes).

### Design notes

- **No `helm push` here.** Publishing to OCI + cosign
  signing is sub-phase 1.68's territory.
- **No in-tree manifest migration here.** The v0.1.x
  `cli-providers::k8s::cilium_values_yaml` etc. continue to
  produce the chart values they always did; `platform-stack/`
  is a parallel source of truth until 1.71 cuts the CLI
  over to consuming the rendered chart.
- **Single template by design.** Adding a component =
  adding a CUE declaration. Editing the template would mean
  the chart is no longer "data-driven", which trades the
  ADR 0028 contribution model for ad-hoc plumbing. Resist
  the urge.
- **JSON Schema is hand-rolled.** Auto-deriving from
  `#PlatformValues` via `cue def --out=jsonschema` was
  considered but rejected — output is draft-07, Helm
  requires draft-2020-12, and the manual schema is only
  ~35 lines that change once a year.

## v0.1.92 — M1.5 Track B.1.66 — platform-stack scaffold (2026-05-15)

First Track B slice. Lays down the CUE-only source tree the
upcoming chart renderer (1.67) and publish workflow (1.68) will
consume. No runtime code changes; only new files under
`platform-stack/` plus lint-config touch-ups.

The deliverable matches ADR 0028's distribution model:
**CUE source in this repository; rendered Helm chart in OCI on
tag.** This commit lands the CUE side.

### Added

- **`platform-stack/`** new top-level subdirectory, flat
  `cue/` tree (no subdirs — CUE treats subdirectories as
  separate package instances even when `package` declaration
  matches; that broke cross-file `_components` merging on the
  design walk).

  - **`cue/platform.cue`** — umbrella schema. Types:
    `#Version` (semver regex), `#Channel` (`stable | edge`),
    `#Tier` (1..4), `#ComponentSource`, `#Component`,
    `#ComponentSet`, `#PlatformValues`. Plus the hidden
    package-level `_components: {}` base.

  - **8 component declarations** (one file each — filename
    prefix `component_`):
    `cilium` (1.16.5, kube-proxy replacement + IPAM kubernetes
    + Hubble off by default), `cert-manager` (v1.16.2 +
    `crds.enabled`), `argocd` (7.7.7, self-managing with
    `prune: false`), `apprafter-operator` (v0.1.91 from
    `oci://ghcr.io/apprafter`), `admission-webhook` (same image
    family), `backstage` (Git source, default off), `network-
    policies` (default-deny + DNS + Argo-CD egress allowance),
    `argocd-cue-cmp` (declared but disabled until 1.69 wiring).

  - **2 tier overlays** (filename prefix `tier_`):
    - `solo.cue` — tier 1, Hubble off, Backstage off,
      argocd-cue-cmp off, single-replica everything (cpx22
      RAM budget).
    - `team.cue` — tier 2, Hubble on (relay + UI), Backstage
      on, admission-webhook + cert-manager + operator at 2
      replicas.

  - **`compatibility.cue`** — `#ChangeClass`
    (`safe | caution | breaking`) + `#VersionRecord` schema +
    initial `0.2.0` entry classifying the first published
    version as `safe` (no operator-facing behaviour change vs
    the in-tree v0.1.x bootstrap).

- **`platform-stack/Chart.yaml.tmpl`** — template the
  upcoming `cue cmd render` (lands 1.67) substitutes
  `{{ .Version }}` into. Operators never see this file
  directly; it lives here as the source of truth for chart
  metadata while everything else (rendered values, schema,
  templates) is generated from CUE.

- **`platform-stack/README.md`** — full layout reference,
  contribution model (adding a component / tightening a
  default / major version bump / adding a tier), local
  development workflow (`just lint` / future `make render`),
  forking story (`apprafter platform fork` lands in 1.74),
  design-walk decision rationale (why flat layout, why
  `[string]: #Component` without autobinding).

- **`platform-stack/CHANGELOG.md`** — operator-facing release
  notes for the platform-stack chart itself, distinct from
  the AppRafter monorepo's `docs/changelog/UNRELEASED.md`.
  Initial entry placeholder for the planned 0.2.0 publish.

### Changed

- **`scripts/lint-cue.sh`** — `cue fmt --check` and
  `cue vet` now cover `./platform-stack/cue/...` alongside
  `schemas/` and `examples/`.

- **`scripts/check-spdx-headers.sh`** — patterns extended to
  cover `platform-stack/cue/**/*.cue` and
  `platform-stack/Chart.yaml.tmpl`. The new files all carry
  the SPDX-License-Identifier in the first 5 lines, so the
  hook continues to pass (166 tracked source files).

### Design-walk gotchas (recorded so future contributors don't re-discover)

1. **Subdirectory split.** Even with identical `package
   platformstack` declarations, CUE treats `cue/components/`
   and `cue/tiers/` as separate package instances. `_components`
   defined in `cue/components/cilium.cue` does **not** flow
   into `cue/tiers/solo.cue`. Fix: single flat `cue/` directory
   with filename prefixes (`component_`, `tier_`) replacing
   the would-be subdirectory groupings.

2. **`#ComponentSet` autobinding.** The natural-looking
   `#ComponentSet: [NAME=string]: #Component & { name: NAME }`
   form (which auto-fills `name` from the map key) re-applies
   `#Component` to every map entry on each unification —
   including the per-tier `components: _components & {
   overlay }`. That re-application strips concrete fields
   contributed by `_components` (`namespace`, `version`, …)
   and `cue vet -c` reports them as incomplete on each tier
   instance. Fix: drop the autobinding, set `name:` explicitly
   in each component declaration, keep `#ComponentSet` as
   plain `[string]: #Component`.

3. **Typed `_components`.** Same problem as #2:
   `_components: #ComponentSet` forces #Component
   re-application on every per-tier overlay unification.
   Solution: declare `_components: {}` (untyped) at package
   level; each component's `#Component & { … }` conformance
   is enforced locally at its declaration site, which is
   strictly stronger than a package-level type pin.

4. **Default-marked values are not concrete to `vet -c`.**
   CUE 0.10+'s `vet -c` flags `bool | *false` as incomplete
   even though the default applies. Pin the value explicitly
   in any tier that doesn't override it (current case:
   `argocd-cue-cmp.enabled` in `tier_team.cue`).

### Tests

Pure scaffold release. Two layers verified locally:

- `bash scripts/lint-cue.sh` — `cue fmt --check` + `cue vet`
  pass for all three trees (`schemas`, `platform-stack/cue`,
  `examples`).
- `cue vet -c ./platform-stack/cue/...` — strict concreteness
  check passes. Every tier instance produces a fully-concrete
  components map with no leftover regex constraints / typed
  placeholders.
- `cue eval -e tier1 ./platform-stack/cue/...` — renders the
  tier-1 values document the renderer will hand off to
  `cue cmd render` in the next sub-phase.

Rust tests stay green (565 passed, unchanged from v0.1.91).
SPDX gate: 166 tracked files (now including
`platform-stack/cue/*.cue` + `platform-stack/Chart.yaml.tmpl`).
fmt + clippy clean.

### What's next

- **1.67** — `cue cmd render` command + `templates/applications.yaml`
  template + `make render` target. Produces the
  `dist/platform-stack-<version>/` tree from CUE.
- **1.68** — `.github/workflows/platform-stack-publish.yml`:
  on `platform-stack/v*` tags, render → helm lint → cosign
  sign → `oras push ghcr.io/apprafter/platform-stack:<v>` +
  GitHub Release `.tgz` attachment.
- **1.69** — Argo CD CMP sidecar wiring per ADR 0029.
- **1.70** — `apprafter cluster-bootstrap` rewrite to consume
  the published OCI artifact instead of direct
  `helm upgrade --install` (the work this whole Track B was
  named after).

## v0.1.91 — M1.5 Track A.9c — Phase 2 polish (backlog closure) (2026-05-15)

Track A backlog cleanup. The v0.1.85 walk surfaced two related
issues that v0.1.85's UX rework deliberately deferred to a
follow-up: Phase 2 of `bootstrap-all` always stabilising at
~60 s and the misleading `[2/3] kubeconfig` label. Both
addressed here.

### Changed

- **`SshKubeconfigFetcher::build_command` adds
  `-o ConnectTimeout=5`.** The first kubeconfig-poll attempt
  after `apply` used to block ~30 s on the kernel's default TCP
  connect retry while cloud-init was still bringing sshd up on
  the new cpx22. Capping ConnectTimeout at 5 s lets the retry
  loop's 10-second sleep do the waiting instead — typical
  Phase 2 on Hetzner cpx22 + Ubuntu 24.04 drops from ~60 s to
  ~20–40 s, the attempt counter ticks up evenly, and the
  spinner moves within the first 5 seconds instead of "freezing"
  for half a minute.

- **Phase 2 label renamed `[2/3] kubeconfig` →
  `[2/3] k3s-ready`** consistently across:
  - the spinner template / prefix;
  - the `✓` success line ("done in Ns");
  - the `✗` failure marker ("FAILED after Ns");
  - the dry-run plan;
  - the final `bootstrap-all complete in T (apply X +
    k3s-ready Y + bootstrap Z)` breakdown.

  The old label suggested the wait was about fetching the
  kubeconfig; in reality the ~60 s on a fresh cluster was
  cloud-init + k3s startup, and the actual SCP at the tail
  takes a second. The new label is what the operator should
  read: "I'm waiting for k3s to be ready", with the kubeconfig
  copy as the trailing side-effect.

- **Spinner message rephrased** to "waiting for cloud-init +
  k3s on the new node…" so even the in-progress text honestly
  names what's happening between the attempts.

- **Dry-run plan line** rewritten:
  ```
  [2/3] k3s-ready (poll)   — wait for cloud-init + k3s on the new node
                              (SCP /etc/rancher/k3s/k3s.yaml every 10s, up to 300s;
                               cache age-encrypted in state)
  ```

- **Docs** synced: `docs/operator-guide/quickstart.md` (Phase 2
  description + duration expectation), `docs/operator-guide/troubleshooting.md`
  (full rewrite of the "Phase 2 of bootstrap-all takes too long"
  section — now references v0.1.91's `ConnectTimeout=5`, names
  the new label, lists three actionable diagnostics for
  remaining slowdowns), `docs/reference/cli.md` (`bootstrap-all`
  blurb references `k3s-ready` instead of `kubeconfig`).

### Tests

- **+1 unit** in `cli_providers::hetzner_cloud::kubeconfig::tests`:
  `ssh_fetcher_caps_connect_timeout_at_five_seconds` pins the
  v0.1.91 SSH-arg change so a future refactor can't silently
  drop the timeout flag.

- **Existing integration test rewired**:
  `bootstrap_all_dry_run_prints_three_phase_plan_without_provider_calls`
  in `tests/bootstrap_all_test.rs` updated its assertion from
  `[2/3] kubeconfig` to `[2/3] k3s-ready`.

- Total: 565 (up from 564, +1 new).

### Backwards compatibility

- `apprafter kubeconfig` the SUBCOMMAND (and its `kc` alias) is
  unchanged — the rename was for the bootstrap-all Phase 2
  *label*, not the subcommand name. Operators scripting
  `apprafter kubeconfig --refresh` keep working unchanged.
- Diagnostic codes unchanged. The 11 stable
  `apprafter::<area>::<reason>` codes catalogued in v0.1.86 are
  unaffected.
- `NO_COLOR=1` / piped output produce byte-identical output
  modulo the literal `k3s-ready` substring replacing
  `kubeconfig` in the affected lines.

### Operator note

A real-Hetzner Phase 2 trace after v0.1.91 typically looks like:

```
✓ [1/3] apply        done in 4s
⠙ [2/3] k3s-ready    attempt 1 — fetching /etc/rancher/k3s/k3s.yaml over SSH…
⠴ [2/3] k3s-ready    attempt 2 — k3s not ready yet (Connection refused); next retry in 10s
⠧ [2/3] k3s-ready    attempt 3 — k3s not ready yet (No such file or directory); next retry in 10s
✓ [2/3] k3s-ready    done in 30s
```

Three attempts at 5 + 10 + 5 + 10 + ~1 s ≈ 30 s — the attempts
fail FAST now, the 10-second sleep is the dominant cost, and
attempt 3 lands once `/etc/rancher/k3s/k3s.yaml` exists. Without
v0.1.91 the same trace stretched to ~60 s because attempt 1
alone spent ~30 s blocked on the TCP timeout.

### Track A closure

With v0.1.91 the entire Track A backlog is empty. Track A is
fully closed — 13 sub-versions across 12 sub-phases plus the
backlog cleanup:

| Sub-phase  | Version  | Slice                                            |
| ---------- | -------- | ------------------------------------------------ |
| 1.66A.1    | v0.1.69  | Rename `platform-cli` → `apprafter` + shim       |
| 1.66A.2    | v0.1.72  | Target file structure + IO module                |
| 1.66A.3    | v0.1.73  | `target add` non-interactive                     |
| 1.66A.4    | v0.1.74  | Provider validator framework + Hetzner ping      |
| 1.66A.4b   | v0.1.76  | Interactive wizard via inquire                   |
| 1.66A.5    | v0.1.79  | Target CRUD                                      |
| 1.66A.6    | v0.1.80  | `whoami` + `auth` stubs                          |
| 1.66A.7    | v0.1.81  | `doctor`                                         |
| 1.66A.8    | v0.1.82–v0.1.83 | Wire `apply`/`destroy`/`import` to chain  |
| 1.66A.9    | v0.1.84–v0.1.85 | `bootstrap-all` orchestrator              |
| 1.66A.10   | v0.1.86–v0.1.87 | miette diagnostic refinement              |
| 1.66A.11   | v0.1.88–v0.1.89 | Semantic colours + aliases                |
| 1.66A.12   | v0.1.90  | Docs + ADR (Track A closure documentation)       |
| **1.66A.9c** | **v0.1.91** | **Phase 2 polish — backlog cleanup**            |

Next iteration opens **Track B (M1.5 sub-phase 1.66 platform-
stack rethink)** — the architectural work this M1.5 milestone
was named after.

## v0.1.90 — M1.5 Track A.12 — docs + ADR (Track A closure) (2026-05-15)

Final Track A slice. After 11 sub-versions of CLI rework
(`v0.1.69`–`v0.1.89`), the operator-facing surface is what it
is — this slice writes it down. Four new doc pages, one new
ADR, plus surface updates to the operator-guide and reference
index. Code changes: zero. Version bump tracks the closure tag.

### Added

- **`docs/adr/0030-cli-target-store-and-credential-chain.md`** —
  the Track A closure ADR. Codifies four design decisions
  (target store, three-step credential resolution chain,
  miette diagnostics, aliases + semantic colours), six
  alternatives considered, four risks with mitigations, and
  re-evaluation triggers (AWS provider landing, Phase 2 opening,
  credential leak surface).

- **`docs/operator-guide/quickstart.md`** — full rewrite. The
  old flow assumed `export HCLOUD_TOKEN` + `cargo run --bin
  apprafter -- init`; the new flow assumes `apprafter` on PATH
  + `apprafter target add` + `apprafter bootstrap-all`. Sections:
  prerequisites, target configuration, one-command provisioning
  (with dry-run preview + per-phase recovery), verification via
  `apprafter doctor` + `kubectl`, day-2 ops table, Application
  CRD usage, troubleshooting pointer.

- **`docs/operator-guide/target-store.md`** — new reference.
  File layout (`config.yaml`, `targets/<name>/`, `auth/`,
  `state/`), field reference for `TargetConfig`, the three-step
  credential resolution chain explained, four common patterns
  (single-cluster, multi-cluster, CI env-var-only, token
  rotation), three anti-patterns.

- **`docs/operator-guide/troubleshooting.md`** — new reference.
  Diagnostic-code catalogue: every one of the 11 stable
  `apprafter::<area>::<reason>` codes shipped through v0.1.86
  + v0.1.87 gets a 2–3 paragraph entry with the exact
  next-step CLI command. Plus a "walk-found common failures"
  section and a worked example reading the layered cause
  chain output (token_rejected wrapping hetzner_api_error).

- **`docs/reference/cli.md`** — full CLI reference. Top-level
  surface, global env vars table (11 entries:
  `APPRAFTER_CONFIG_DIR`, `HCLOUD_TOKEN`, `APPRAFTER_SSH_*`,
  `APPRAFTER_AGE_KEY`, `APPRAFTER_HCLOUD_BASE_URL`,
  `APPRAFTER_MANIFEST`, `APPRAFTER_NO_PING`, `NO_COLOR`,
  `RUST_BACKTRACE`), every subcommand with its flags, aliases
  reference table. Cross-references quickstart + target-store
  + troubleshooting.

### Changed

- **`docs/operator-guide/index.md`** — links to the new pages,
  status note flipped from "stub" to "Track A closed", canonical
  references list refreshed.

- **`docs/reference/index.md`** — CLI reference promoted from
  stub to first-class entry, diagnostic-code reference
  cross-linked into troubleshooting.

- **`mkdocs.yml`** nav — Operator Guide + Reference are now
  nested entries surfacing the new pages (quickstart, target
  store, gitops walk, troubleshooting, recovery for operator;
  index + CLI for reference).

### Tests

Docs-only release. SPDX gate: 166 tracked source files pass
(unchanged — new files are .md docs / ADR, not source code).
`cargo fmt --check`, `cargo clippy --workspace -- -D warnings`,
`cargo test --workspace` (564 passed): all clean. `mkdocs
build --strict` not run locally due to a `nix shell` env quirk
(mkdocs binary doesn't see the mkdocs-material theme through
the ad-hoc shell); the CI workflow at `.github/workflows/docs.yml`
remains the canonical validator.

### Track A closure

This is the last Track A slice. With v0.1.90 tagged, Track A
(CLI DX rework, 12 sub-phases) is closed:

| Sub-phase  | Version  | Slice                                          |
| ---------- | -------- | ---------------------------------------------- |
| 1.66A.1    | v0.1.69  | Rename `platform-cli` → `apprafter` + shim     |
| 1.66A.2    | v0.1.72  | Target file structure + IO module              |
| 1.66A.3    | v0.1.73  | `target add` non-interactive                   |
| 1.66A.4    | v0.1.74  | Provider validator framework + Hetzner ping    |
| 1.66A.4b   | v0.1.76  | Interactive wizard via inquire                 |
| 1.66A.5    | v0.1.79  | Target CRUD                                    |
| 1.66A.6    | v0.1.80  | `whoami` + `auth` stubs                        |
| 1.66A.7    | v0.1.81  | `doctor`                                       |
| 1.66A.8    | v0.1.82–v0.1.83 | Wire `apply`/`destroy`/`import` to chain |
| 1.66A.9    | v0.1.84–v0.1.85 | `bootstrap-all` orchestrator             |
| 1.66A.10   | v0.1.86–v0.1.87 | miette diagnostic refinement             |
| 1.66A.11   | v0.1.88–v0.1.89 | Semantic colours + aliases               |
| **1.66A.12** | **v0.1.90** | **Docs + ADR (this slice — closure)**         |

Track A backlog moved to explicit follow-up (A.9c):

- SSH `ConnectTimeout=5` in `SshKubeconfigFetcher` + Phase 2
  label rename `[2/3] kubeconfig` → `[2/3] k3s-ready` (current
  label is misleading; the ~60s isn't the fetch, it's
  cloud-init + k3s startup).

After v0.1.90 the next iteration opens **Track B (M1.5
sub-phase 1.66 platform-stack rethink)** — the architectural
work this M1.5 milestone was named after. Track A's CLI surface
becomes the load-bearing user contract Track B can rely on.

## v0.1.89 — hotfix: color bootstrap-all dry-run plan (2026-05-14)

Walk-found gap in v0.1.88. The Track A.11 colour pass touched
the wet path (`→`, `✓`, `✗` phase markers) and `doctor`
glyphs, but the much more common `apprafter up --dry-run`
output stayed monochrome — the only green visible was the
`INFO` label from `tracing-subscriber`, which is unrelated
to the new `style` module.

v0.1.89 closes the gap with subtle, semantically-meaningful
colouring of the dry-run plan, designed so the structurally
important parts (target name, phase numbers, real config
values) read at full brightness while the supporting labels
and `<unset — apply uses …>` defaults fade into `style::dim`.

### Changed

- **`bootstrap-all — DRY RUN`** heading: `style::bold` on the
  command name.
- **`Target:` label** + **active target name**: `style::bold`.
  Reads as the answer to "which cluster am I about to touch?",
  which is the single most important line in the plan.
- **`(active)` / `(via --target override)`**: `style::info`
  cyan. Tag, not content — quick visual confirmation that the
  resolution chain worked.
- **`Phases:` heading**: `style::bold`.
- **`[1/3]` / `[2/3]` / `[3/3]` phase numbers**: `style::info`
  cyan. Echoes the cyan `→` marker the wet path uses; the
  parallel visual cue helps operators map dry-run phases to
  the runtime spinner output.
- **Field labels `Provider:` / `Region:` / `Tier:` /
  `Cluster:` / `SSH key:`**: `style::dim`. The label is
  scaffolding; the operator's eye should land on the value to
  its right.
- **`<unset — apply uses …>` placeholders**: `style::dim`.
  Stays readable, but fades against real values.
- **`(server, network, firewall, …)`** sub-line under each
  phase: `style::dim`. Supplementary detail, not the headline.
- **Bottom hint** `Run \`apprafter bootstrap-all\` …`:
  `style::dim`.

The `→` / `✓` / `✗` markers + spinner colour in the wet path
keep the v0.1.88 styling; only the dry-run side of the file
changes here.

### Backwards compatibility

- `NO_COLOR=1` and non-TTY pipes still produce byte-identical
  output to v0.1.85 (zero ANSI sequences). Verified by
  `cargo test`'s existing `cli_core::style::tests` running
  under captured-stdout.
- All `bootstrap_all` integration tests stay green; assertions
  match the substring content of the `style::*` helpers'
  output, which under non-TTY equals the original text.

### Operator note

Running in a real terminal you now see:
- the target name bold (so you can't miss which cluster);
- `(active)` cyan (resolution-chain confirmation);
- phase numbers `[1/3]` … cyan (mapping to the cyan `→` in
  the wet path);
- everything else either full-brightness real value or
  dimmed scaffolding.

In CI / piped output the file is byte-for-byte the same as
v0.1.88.

## v0.1.88 — M1.5 Track A.11 — semantic colors + subcommand aliases (2026-05-14)

Eleventh Track A slice. Two ergonomic additions: semantic colour
on result lines (so success-vs-warning-vs-failure parses
visually, not character-by-character), and short aliases for the
most-typed subcommands (`apprafter ls`, `apprafter up`, etc.).
Both honour the standard `NO_COLOR` / non-TTY contracts so
nothing changes for CI / piped output.

### Added

- **`cli_core::style` module** — semantic colour helpers wrapping
  `owo-colors` 4.x with the `supports-colors` feature. Each
  helper checks the relevant stream (stdout / stderr) for TTY +
  `NO_COLOR` lazily on every format call and returns plain text
  when colour isn't supported:
  - `ok(t)` — green, for `✓` markers, PASS rows, verified labels.
  - `warn(t)` — yellow, for WARN rows and soft failures.
  - `fail(t)` — red (uses stderr stream — callsites that consume
    `fail()` write to stderr).
  - `info(t)` — cyan, for neutral phase markers (`→`), column
    headers, and `(active)` tags.
  - `dim(t)` — dimmed, for tertiary annotations like
    `(unset — apply uses platform-1)` in dry-run plans.
  - `bold(t)` — bold emphasis; combine with the others by
    nesting: `info(&bold("dev"))`.

  All helpers return `String` (not the `owo-colors` reference-
  flavoured wrapper) — the generic type returned by
  `if_supports_color` is hard to name in a function signature,
  and `format!()` overhead is negligible against the cost of
  emitting the line at all.

- **6 new subcommand aliases**:
  - `apprafter kc` → `apprafter kubeconfig`
  - `apprafter cb` → `apprafter cluster-bootstrap`
  - `apprafter up` → `apprafter bootstrap-all`
  - `apprafter t ls` → `apprafter target list` (chains with the
    pre-existing `t` → `target` alias)
  - `apprafter t info` → `apprafter target show`
  - `apprafter t rm` → `apprafter target remove`

  All clap aliases — `clap` shows them inline in `--help`. The
  canonical names stay primary; aliases exist for speed.

### Changed

- **`commands/bootstrap_all.rs`** wraps the `→`, `✓`, `✗`
  glyphs with `style::info`, `style::ok`, `style::fail`. Spinner
  finish-success line also uses `style::ok` for its `✓`.
- **`commands/doctor.rs`** gains a `CheckStatus::coloured_glyph`
  helper alongside the existing `glyph()`. PASS rows render
  green ✓, WARN rows yellow ⚠, FAIL rows red ✗. The non-glyph
  text (check name, detail, hint) stays uncoloured so the
  attention is on the verdict.

### Tests (2 unit + 7 integration)

- **Unit** (`cli_core::style::tests`):
  - `ok_returns_ansi_free_text_when_stream_is_not_a_tty` — pins
    the no-TTY contract (cargo test's captured stdout): each
    helper returns the literal input with zero ANSI bytes.
  - `warn_fail_info_dim_bold_all_round_trip_text_under_no_tty`
    — same contract across all 5 remaining helpers.

- **Integration** (`tests/aliases_test.rs`):
  - `target_ls_alias_routes_to_target_list` — subprocess `target
    list` and `target ls` produce byte-identical stdout. The
    strongest possible "same handler" assertion.
  - `target_rm_alias_routes_to_target_remove` — alias surfaces
    the same `apprafter::target::not_found` diagnostic on a
    missing target.
  - `target_info_alias_routes_to_target_show` — same.
  - `kc_alias_routes_to_kubeconfig` — surfaces the "no
    hetzner_cloud state" hint identically.
  - `cb_alias_routes_to_cluster_bootstrap` — same.
  - `up_alias_routes_to_bootstrap_all_dry_run` — `up --dry-run`
    prints the same `DRY RUN` plan as `bootstrap-all --dry-run`.
  - `t_alias_for_target_still_works_alongside_new_alias_chain`
    — `apprafter t ls` chains the pre-existing `t` alias with
    the new `ls` alias. Pins muscle-memory kubectl-style.

### Backwards compatibility

- All canonical command names still work. Aliases are purely
  additive.
- `NO_COLOR=1` / piped output produce byte-identical output to
  v0.1.87 (zero ANSI sequences).
- The `style` module is `pub mod` in `cli-core`, available to
  any future workspace member that wants to follow the same
  semantic palette.
- Miette's own renderer keeps using its own palette (already
  honours `NO_COLOR`); the new module is for our `println!`-
  based output, not miette diagnostics.

### Out of scope (deferred)

- Coloured cells in `apprafter target list` (active row
  highlighting via `tabled`). Feasible via custom cell renderer;
  current monospace table reads well enough that this can wait
  for walk feedback.
- Coloured identity / cluster names in `whoami` (bold-cyan
  emphasis). Foundation is there via `style::bold` +
  `style::info`; next iterative refinement.
- Background-colour `style::ok_strong` / `style::fail_strong`
  variants for critical-path emphasis. Add when needed.

### Operator note

Type-saving tour for the new shortcuts:

```
$ apprafter t ls          # → target list
$ apprafter t info dev    # → target show dev
$ apprafter t rm bad      # → target remove bad
$ apprafter kc            # → kubeconfig
$ apprafter cb            # → cluster-bootstrap
$ apprafter up            # → bootstrap-all
$ apprafter up --dry-run  # → bootstrap-all --dry-run
```

For CI / scripts where colour interferes, set `NO_COLOR=1` (or
pipe stdout into a file — the helpers auto-detect both).

## v0.1.87 — hotfix: typed errors on target-add token validation (2026-05-14)

Walk-found issue in v0.1.86. The `target add` flow with an
invalid token surfaced as the catch-all `apprafter::cli::other`
diagnostic, because both ping call sites (`commands/target.rs`
non-interactive path and `commands/target_wizard.rs` interactive
path) explicitly wrapped the typed `CliError::Hetzner { status:
401, .. }` into `CliError::Other(format!(...))`. This dropped
the diagnostic code AND replaced the variant-specific rotation
help with the generic "please file an issue" help line.

v0.1.87 promotes both ping outcomes to typed variants, keeps the
original Hetzner error as a `#[diagnostic_source]` cause chain,
and lets miette render both layers.

### Added

- **`CliError::ProviderTokenRejected { provider, cause }`** —
  code `apprafter::target::token_rejected`. Triggers for 401
  responses from the credential validation ping. Help lines
  guide the operator through:
  - Verifying the token at the provider's console UI.
  - The "trailing newline from clipboard" trap (common for
    Hetzner's 64-char tokens).
  - Using `apprafter target add <name> --renew` for rotation
    instead of recreating the target.
  - Falling back to `--no-ping` for offline / CI seeding.

- **`CliError::ProviderApiUnreachable { provider, cause }`** —
  code `apprafter::target::provider_unreachable`. Triggers for
  every non-401 ping failure (5xx, 429, transport errors). Help
  lines target reachability checks, NOT rotation:
  - `apprafter doctor` to confirm DNS + reachability.
  - Provider status page link.
  - VPN / corporate proxy check.
  - `--no-ping` to save the target offline and verify later.

  Crucially, the help does NOT mention rotation — that would
  misdirect operators when the token is actually fine and only
  the API is down.

Both variants carry the original `CliError::Hetzner` as a
`#[source] #[diagnostic_source] cause: Box<dyn
miette::Diagnostic + Send + Sync + 'static>` field. Miette walks
the chain and renders both layers:

```
Error: apprafter::target::token_rejected
  × provider `hetzner-cloud` rejected the supplied token
  ╰─▶ apprafter::provider::hetzner_api_error
        × hetzner-cloud GET https://api.hetzner.cloud/v1/locations failed
        │ (status 401): unauthorized: the token you have provided is invalid
        help: The Hetzner Cloud API returned a non-2xx response. Common causes:
              • 401 unauthorized — the stored API token was rotated or revoked. …
              • 403 forbidden — …
              • 429 rate limit — …
              • 5xx — …
  help: The provider's read-only credential check returned 401 / unauthorized.
        Either the token was mistyped, never had the right scopes, or has been
        rotated / revoked since you copied it.
        • Verify the token at https://console.hetzner.cloud/projects → …
        • Copy the token again (it's 64 ASCII chars, no prefix) — …
        • If you're rotating, run `apprafter target add <name> --renew …
        • Pass `--no-ping` to skip the check …
```

### Changed

- **`commands/target.rs::ping_provider`** stops downgrading
  typed errors. The previous `match err { CliError::Hetzner {
  status: 401, .. } => CliError::Other(format!(…)), … }` chain
  is replaced with a single call to
  `classify_ping_error(provider, err)`. The helper returns the
  new typed variants with the original error chained underneath.

- **`commands/target_wizard.rs::prompt_token`** stops wrapping
  the wizard's ping error as
  `CliError::Other(format!("token rejected by provider: {e}"))`.
  Same `classify_ping_error` helper, identical diagnostic codes
  across both flows.

- **`classify_ping_error(provider, err)`** lives in both
  modules. Kept duplicated rather than pulled into cli-core
  because the function is provider-specific (only the Hetzner
  `status: 401` predicate today); when the second provider
  lands, the dispatch will look different per flow. Trivial
  duplication is cheaper than designing for hypothetical reuse.

### Tests (2 new unit + 2 prior integration reworked)

- **`provider_token_rejected_carries_rotation_hint_and_chains_cause`**
  (cli_core) — pins the rotation hint substrings AND walks the
  `Diagnostic::diagnostic_source()` chain to confirm the inner
  `Hetzner` variant is reachable (so miette will render it).

- **`provider_api_unreachable_targets_outage_path_not_rotation`**
  (cli_core) — pins the outage hint substrings AND asserts the
  help does NOT contain `"rotated / revoked"`, so accidental
  copy-paste of the token_rejected help text into the outage
  variant would fail the test.

- **`target_add_surfaces_typed_error_on_hetzner_401`**
  (target_test) — assertions migrated from the old wrap text
  (`"Hetzner Cloud rejected the token (HTTP 401)"`) to
  diagnostic codes (`apprafter::target::token_rejected` AND
  `apprafter::provider::hetzner_api_error`). The chained-cause
  rendering produces line-wrapped output where the literal
  `(status 401)` substring may straddle a newline, but the
  diagnostic codes themselves never wrap.

- **`target_add_surfaces_helpful_error_when_api_is_unreachable`**
  (target_test) — same migration. Pinned codes:
  `apprafter::target::provider_unreachable`. Help substrings
  still pinned: `--no-ping`, `apprafter doctor`.

### Backwards compatibility

- The shell exit code on failure is unchanged.
- Existing log-grep workflows keyed on the original Hetzner
  status / message text still match — the chained-cause
  rendering preserves them inside the second layer.
- New diagnostic codes are *additive*; consumers that grouped
  failures by `apprafter::cli::other` will see the same
  failures move to `apprafter::target::token_rejected` /
  `apprafter::target::provider_unreachable` and can adjust
  filters.

### Operator note

A full repro of the walk's original symptom:

```
$ apprafter target add bad --provider hetzner-cloud \
    --token "$(python -c 'print("a"*64)')" \
    --region nbg1 --tier solo --ssh-key ~/.ssh/id_ed25519.pub
…
Error: apprafter::target::token_rejected
  × provider `hetzner-cloud` rejected the supplied token
  ╰─▶ apprafter::provider::hetzner_api_error
        × hetzner-cloud GET https://api.hetzner.cloud/v1/locations failed
        │ (status 401): unauthorized: the token you have provided is invalid
        help: …  (Hetzner-specific 401/403/429/5xx breakdown)
  help: …  (rotation-specific: console URL, --renew, --no-ping)
```

If you previously grep'd for `apprafter::cli::other` to spot
auth failures in CI, switch to `apprafter::target::` — both
the rejected-token and the unreachable-provider cases now live
under that namespace.

## v0.1.86 — M1.5 Track A.10 — miette diagnostic refinement (2026-05-14)

Tenth Track A slice. Errors stop being opaque `Debug` blobs and
start carrying a stable code + multi-line help text. The binary
switches from `color-eyre` to `miette`'s `fancy` reporter, so
unhandled `CliError`s render with the rustc-style three-line
block:

```
Error: apprafter::target::not_found

  × target `ghost` not found (available: )
  help: Either the `--target` flag was given a name that's not in the store,
        or no target has been created yet. List existing targets with
        `apprafter target list`; create a new one with `apprafter target add
        <name> --provider hetzner-cloud …`. If the store is empty (`available:
        ` shows nothing), this is your first run — start with `apprafter
        target add`.
```

The same `CliError` value used to render as
`Error: TargetNotFound { name: "ghost", available: "" }`, which
is fine for grepping in CI logs and useless for operators.

### Added

- **`cli_core::CliError` now derives `miette::Diagnostic`.** Every
  user-facing variant ships:
  - A stable `code(apprafter::*)` matching `^apprafter::[a-z_:]+$`.
    Codes are part of the public surface — log-analytics
    pipelines can group by them across releases.
  - A multi-line `help(...)` line describing root causes and the
    next-step CLI command to fix it.

  Codes added in this slice:
  - `apprafter::env::cue_not_found` — `nix develop` hint.
  - `apprafter::env::cue_export_failed` — `cue vet` reproduce
    hint.
  - `apprafter::provider::hetzner_api_error` — enumerates 401 /
    403 / 429 / 5xx common causes, points at `apprafter target
    add --renew` for the 401 path and `apprafter doctor` for the
    rest.
  - `apprafter::provider::server_type_unavailable` — explains
    the cx22 → cpx22 retirement story and points at the
    Infrastructure manifest's `nodes[0].kind` field.
  - `apprafter::state::corrupt` — recommends `apprafter import`
    as a safe recovery path (rebuilds state from live Hetzner
    labels).
  - `apprafter::target::invalid_config` — walks the operator
    through both hand-fix and per-target re-creation paths
    under `$XDG_CONFIG_HOME/apprafter/targets/<name>/`.
  - `apprafter::target::not_found` — lists `target list` +
    `target add` next steps, calls out the empty-store
    first-run case.
  - `apprafter::io::error` / `apprafter::io::json` /
    `apprafter::io::yaml` — variant-specific guidance for each
    decode flavour.
  - `apprafter::cli::other` — the catch-all. Kept stable so
    grepping logs for recurring messages produces useful
    candidates for promotion to a typed variant.

- **`miette::set_hook` installed in `main`** with the `fancy`
  handler (terminal links, Unicode glyphs, 2 lines of context,
  cause-chain rendering). Backtraces stay opt-in via
  `RUST_BACKTRACE=1`; the default render is help-line first,
  not stack-trace first.

- **Pure `dispatch(args: Cli) -> cli_core::Result<()>` helper**
  in `main.rs`. Subcommand handlers keep their original
  `cli_core::Result` ergonomics; the typed-error →
  `miette::Report` conversion happens exactly once at the
  binary boundary. Easy to drive from tests if needed.

### Changed

- **`color-eyre` dropped** from the workspace and platform-cli
  `Cargo.toml`. Nothing else in the workspace used it; one fewer
  TUI-error crate to track.

- **`cli-core` adds `miette` as a direct dep** because
  `CliError` lives there. Workspace dep added as
  `miette = { version = "7", features = ["fancy"] }`.

- **File-scope `#![allow(unused_assignments)]`** in
  `cli_core/src/error.rs`. `miette-derive` 7.6.0 generates
  diagnostic-plumbing helper bindings that reassign named-field
  values; the lint fires on generated code that isn't ours to
  fix. A local enum-level `#[allow]` doesn't propagate through
  the derive macro, so the suppression has to live at file
  scope. Documented at the top of the file with the rationale.

### Tests (8 unit + 3 integration)

- **Unit** (`cli_core::error::tests`) — 8 tests pin every
  user-facing variant's `.code()` and `.help()` text against the
  documented strings. Catches accidental code renames and help
  drift across future edits. Examples:
  - `hetzner_diagnostic_help_enumerates_401_403_429_5xx` —
    asserts each of the four error families is mentioned
    explicitly so operators don't have to guess which case
    applies.
  - `server_type_unavailable_diagnostic_explains_retirement_path`
    — both `cx22` and `cpx22` substrings must appear so the
    most common cause is self-explained.

- **Integration** (`tests/miette_render_test.rs`) — 3
  subprocess-based tests verifying the END-TO-END render:
  - `unhandled_error_renders_with_miette_help_line` — `apply`
    with no creds → stderr contains `help:` + a real
    `apprafter::…` code (proof we're going through the fancy
    reporter, not `eyre` or `Debug`).
  - `typed_target_not_found_renders_with_dedicated_code_and_help`
    — `target show ghost` → stderr contains the dedicated
    `apprafter::target::not_found` code AND the help-text
    substrings (`apprafter target list`, `apprafter target
    add`). Pins the rendering contract for typed variants.
  - `no_color_env_yields_ansi_free_stderr` — `NO_COLOR=1` →
    no `\x1b` bytes in stderr, but the same `help:` + code
    substrings stay present. Pipe-friendliness contract.

### Backwards compatibility

- All existing stderr-substring assertions in
  `cli_smoke.rs` / `target_test.rs` / `doctor_test.rs` /
  `kubeconfig_test.rs` / `import_test.rs` continue to pass —
  miette's render preserves the original `Display` text inside
  the boxed error message, and the help-text substrings only
  add to stderr (never remove).
- The shell-level exit code on error is unchanged. The binary
  returns `Err(miette::Report)` from `main`, miette renders to
  stderr, the process exits with code 1.
- Existing log-grep workflows keyed on the `Display` text keep
  working. The diagnostic codes are *additional* signal; they
  don't replace the human-readable message.

### Out of scope (deferred)

- `#[source_code]` + `#[label]` span highlighting per variant
  (e.g. highlighting the offending Hetzner token prefix). The
  miette feature exists; it requires threading source text
  through the error chain, which is more useful once CUE
  manifest parse errors also feed through this surface. Will
  revisit when those get the same treatment.
- Mass-promoting `CliError::Other(format!(...))` call sites
  into typed variants. `Other` stays the catch-all with a
  stable code; recurring messages get promoted when they
  become recurring. Today's catch-all-with-code is the
  signalling for future promotions.
- The Phase 2 SSH `ConnectTimeout` + `[2/3] kubeconfig` rename
  backlog (Track A.9 follow-up) is still untouched here.

### Operator note

The most operator-visible change is the help line. Two examples
from a fresh checkout:

```
$ apprafter target show ghost
…
Error: apprafter::target::not_found
  × target `ghost` not found (available: )
  help: …
```

```
$ apprafter apply
…
Error: apprafter::cli::other
  × no provider configured. Run `apprafter target add <name> --provider
  │ hetzner-cloud …` (recommended) or the legacy `apprafter init …`.
  help: …
```

Set `NO_COLOR=1` for ANSI-free output (CI / pipe).

## v0.1.85 — hotfix: bootstrap-all UX rework after walk feedback (2026-05-14)

Walk-found issues in v0.1.84. Three problems made the new
wrapper feel noisier than it should:

1. **Duplicated spinner lines**: `MultiProgress` keeps every
   bar (finished or not) in its render set. On every helm /
   kubectl write — and there are MANY — the bars re-rendered,
   producing a cascade of stale `[1/3] apply  done in 4s` /
   `[2/3] kubeconfig  ready in 1m00s` lines scrolling past
   the actual phase 3 output. Easy to miss the real work in
   the noise.

2. **`dry-run` was opaque**: showed the literal subcommand
   names but the `<active target>` placeholder was never
   resolved to a real name, so operators couldn't tell which
   target the wrapper would actually use. And the phase
   descriptions were just CLI lines, not "what this phase
   does".

3. **Spinner fought with `tracing` logs**: the steady tick
   redraws on every 120ms, the apply/cluster-bootstrap
   tracing-on-stderr writes interleaved with it, the row got
   visually torn.

v0.1.85 reworks the UX:

### Changed

- **Phases 1 and 3 stop using spinners.** Replaced with
  static start / end markers:
  ```
  → [1/3] apply        provisioning Hetzner Cloud resources…
  …helm / kubectl / tracing output flows normally here…
  ✓ [1/3] apply        done in 4s
  ```
  No animation to fight the inner subcommand's stderr writes;
  no `MultiProgress` to redraw stale frames; no duplicate
  spinner lines.

- **Phase 2 keeps its spinner** (and uses a single
  `ProgressBar`, not `MultiProgress`). The kubeconfig poll
  loop owns all output between Phase 1's end and Phase 3's
  start, so the spinner has nothing to compete with. On
  success: `finish_and_clear()` + static `✓ [2/3]
  kubeconfig   ready in Ns` line. On failure: same `✗`
  marker + bubbled error.

- **Failure marker added** for all phases:
  `✗ [N/3] phase   FAILED after Ns` on stderr, then the
  original `CliError` is propagated unchanged so the
  shell-level exit code + error chain still work.

- **Final summary now breaks down per phase**:
  ```
  bootstrap-all complete in 3m21s (apply 4s + kubeconfig 1m00s + bootstrap 2m16s)
  ```
  Quick visual sanity check on which phase ate the most
  time.

- **`--dry-run` rebuilt** as a real plan, not a subcommand
  listing:
  ```
  bootstrap-all — DRY RUN (no provider or cluster mutation)

  Target: dev2 (active)
    Provider:    hetzner-cloud
    Region:      nbg1
    Tier:        solo
    Cluster:     <unset — apply uses platform-1>
    SSH key:     /home/rem/.ssh/id_ed25519_gmail.pub

  Phases:
    [1/3] apply              — provision Hetzner Cloud resources
                                (server, network, firewall, SSH key, optional floating IPs)
    [2/3] kubeconfig (poll)  — SSH-fetch /etc/rancher/k3s/k3s.yaml every 10s, up to 300s
                                (cache age-encrypted in state)
    [3/3] cluster-bootstrap  — install Cilium + Gateway API CRDs + Application CRD
                                + default-deny NetworkPolicy + Argo CD + cert-manager
                                + self-signed ClusterIssuer + apprafter-operator
                                + admission-webhook

  Run `apprafter bootstrap-all` (without --dry-run) to execute.
  ```
  The target block resolves the *real* active target name
  (or echoes the `--target` override) and reads `config.yaml`
  so operators can see exactly which provider / region /
  tier the wrapper would use before committing. Each phase
  line describes what it does, not the CLI string it would
  call.

### Tests (2 new integration)

- `bootstrap_all_dry_run_with_empty_store_prints_onboarding_hint`
  — empty `APPRAFTER_CONFIG_DIR` → `no active target` +
  `apprafter target add` hint surfaces in stdout. Pins the
  onboarding UX so the empty-store path doesn't silently
  print a half-empty plan.

- `bootstrap_all_dry_run_with_active_target_resolves_name_and_config`
  — seeds a real target via `apprafter target add` with
  provider + region + tier, then runs `bootstrap-all
  --dry-run` against the same `APPRAFTER_CONFIG_DIR` and
  asserts the plan reads `Target: myprod (active)` with
  provider/region/tier echoed back. Locks down the resolution
  + display path so future refactors can't accidentally drop
  it back to a placeholder.

Existing 4 unit + 4 integration tests from v0.1.84 untouched
and still pass.

### Color theming

Not in scope here. Plan Track A.11 owns CLI colour /
`NO_COLOR` support along with alias support
(`.bootstrap`, `.k`, etc.). v0.1.85 keeps everything
monochrome.

### Operator note

The v0.1.84 walk surfaced this in two real-Hetzner runs.
The fresh-cluster trace now reads (1 main spinner active at
a time, no cascade):

```
→ [1/3] apply        provisioning Hetzner Cloud resources…
…
apply complete: 4 action(s)
✓ [1/3] apply        done in 4s
⠋ [2/3] kubeconfig   attempt 3 — k3s not ready yet (cat: …); next retry in 10s
✓ [2/3] kubeconfig   ready in 1m00s
→ [3/3] bootstrap    installing Cilium + Argo CD + cert-manager + operator…
…
cluster-bootstrap complete: cilium 1.16.5 + …
✓ [3/3] bootstrap    done in 2m16s
bootstrap-all complete in 3m21s (apply 4s + kubeconfig 1m00s + bootstrap 2m16s)
```

## v0.1.84 — M1.5 Track A.9 — apprafter bootstrap-all (2026-05-14)

Ninth Track A slice. The "happy path" first-run experience
collapses from three manual subcommands into one:

```
$ apprafter target add dev --provider hetzner-cloud --token "$T" \
    --region nbg1 --tier solo
$ apprafter bootstrap-all          # ← apply → wait → cluster-bootstrap
```

The wrapper still calls the three existing subcommands under the
hood; nothing about their solo invocations changes. The single
new piece is a `indicatif::MultiProgress` UX that makes the
multi-minute Phase 2 (Hetzner cloud-init + k3s startup) visible
instead of a black-box `ssh` retry loop.

### Added

- **`apprafter bootstrap-all`** (top-level command, no
  alias yet — Track A.11 wires `.bootstrap` shorthand).
  - **Phase 1/3 (`apply`)** — calls `commands::apply::run` with
    the same target resolution chain (`--target` override
    → `HCLOUD_TOKEN` env → active target). Spinner reads
    "provisioning Hetzner Cloud resources…".
  - **Phase 2/3 (`kubeconfig` poll)** — retry loop hitting
    `commands::kubeconfig::fetch_and_cache(refresh=true, ...)`
    every 10s up to a 5-minute budget (`KUBECONFIG_POLL_TIMEOUT
    = 300s`, `KUBECONFIG_POLL_INTERVAL = 10s`). The spinner
    message rotates with the attempt counter and the truncated
    last error so the operator can see WHY it's still spinning
    (typically `kex_exchange_identification` while k3s systemd
    unit is still coming up). On timeout: typed error with a
    hint to run `apprafter kubeconfig --refresh` once the
    cluster reports ready.
  - **Phase 3/3 (`cluster-bootstrap`)** — calls
    `commands::cluster_bootstrap::run()` unchanged. Spinner
    reads "installing Cilium + Argo CD + cert-manager +
    operator…".
  - **`--dry-run`** prints a 3-phase plan with the resolved
    target label and poll budget, then exits 0. No provider /
    cluster / kubeconfig calls. Safe to run in any directory
    without a state file or token configured — useful for
    inspecting what the wrapper would invoke before committing.
  - **`--target <name>`** overrides the active target for the
    entire run; same flag semantics as `apprafter apply
    --target`. Threaded into Phase 2 so the kubeconfig poll
    uses the same credentials as Phase 1.
  - **Total elapsed time** printed at the end:
    `bootstrap-all complete in Nm00s`. Per-phase elapsed
    summaries on each spinner's finish line.

- **`commands::kubeconfig::fetch_and_cache(refresh, target_override)`**
  extracted from `run`. Returns the YAML string and writes the
  encrypted state cache; never prints. `run` is now a thin
  wrapper around it (`fetch_and_cache(…)?` → `print!`). The
  reason it exists: Phase 2's retry loop needs to invoke the
  kubeconfig path in-process and discard the YAML on every
  attempt — spawning a child `apprafter kubeconfig --refresh`
  each cycle would have to parse stdout to detect success vs
  ssh transport error, which the in-process call gives us via
  `Result<String>` directly.

### Changed

- **`commands::kubeconfig`** — replaced direct
  `std::env::var("HCLOUD_TOKEN")` lookup with
  `resolve_hetzner_token(None, &target_store, target_override)`.
  `apprafter kubeconfig --refresh` now honours the same
  credential chain as the rest of the operational commands
  (`--target` flag → env → active target). Cold-fetch path
  (no cached kubeconfig in state) was the only branch that
  used HCLOUD_TOKEN, so this is the only path affected.

- **`Commands::Kubeconfig`** gains a `--target <name>` flag
  for symmetry with `apply`/`destroy`/`import`. Useful when
  scripting against multiple targets without
  `apprafter target use`.

- **`cli/Cargo.toml`** workspace deps: `indicatif = "0.17"`.
  `platform-cli/Cargo.toml` adds it as a direct dep.

### Tests (4 unit + 4 integration)

- **Unit** (`commands::bootstrap_all::tests`):
  - `format_elapsed_uses_seconds_under_one_minute` /
    `format_elapsed_switches_to_minutes_at_sixty_seconds` —
    guard the `0s` → `Nm00s` boundary in the elapsed-time
    formatter.
  - `short_error_keeps_first_line_only` — multi-line SSH error
    chains get trimmed to one line for the spinner.
  - `short_error_truncates_long_first_line_with_ellipsis` —
    80-char clamp, so the spinner glyph doesn't get shoved
    off-screen on wide terminals.

- **Integration** (`tests/bootstrap_all_test.rs`):
  - `bootstrap_all_dry_run_prints_three_phase_plan_without_provider_calls`
    — no state file, no HCLOUD_TOKEN, no
    APPRAFTER_HCLOUD_BASE_URL → success + all three phase
    labels in stdout. Pins the dry-run contract: it MUST be
    safe in any environment.
  - `bootstrap_all_dry_run_echoes_target_override` — `--target
    work` round-trips into the printed plan.
  - `bootstrap_all_help_documents_dry_run_and_target_flags` —
    surface-level guard so `--help` doesn't accidentally drop
    these flags during future refactors.
  - `bootstrap_all_rejects_unknown_flag` — clap surface
    contract.

### Backwards compatibility

- All three constituent subcommands (`apply`, `kubeconfig`,
  `cluster-bootstrap`) keep their pre-v0.1.84 signatures and
  exit behaviour. Operators who already wired
  `kubeconfig` / `cluster-bootstrap` into scripts can keep
  doing that; `bootstrap-all` is purely additive.
- `apprafter kubeconfig` without `--target` keeps reading
  `HCLOUD_TOKEN` from env first (step 2 in the resolution
  chain). The only behavioural change is that the cold-fetch
  path NOW also falls back to the active target's token when
  the env var is unset — same chain as `apply`.

### Out of scope (deferred)

- Idempotent re-run / "skip-already-installed" semantics —
  Argo CD already handles this for Phase 3 via `helm upgrade
  --install`; Phase 1 dedupes through Hetzner labels; Phase 2
  `--refresh` always re-fetches (cheap). No additional logic
  needed for now.
- miette-styled error rendering on Phase 2 timeout — Track
  A.10.
- `pb.suspend(|| println!(...))` wrapping for `apply` /
  `cluster_bootstrap` stdout — current `println!` calls go to
  stdout, indicatif spinner is on stderr; they don't visually
  collide. Will revisit if the e2e walk shows interleaving
  issues.

### Operator note

The e2e walk for v0.1.84 demonstrates:

```
$ apprafter target add dev --provider hetzner-cloud --token "$T" \
    --region nbg1 --tier solo
$ apprafter bootstrap-all
⠋ [1/3] apply          provisioning Hetzner Cloud resources…
✔ [1/3] apply          done in 22s
⠋ [2/3] kubeconfig     attempt 9 — k3s not ready yet (…); next retry in 10s
✔ [2/3] kubeconfig     ready in 1m32s
⠋ [3/3] bootstrap      installing Cilium + Argo CD + cert-manager + operator…
✔ [3/3] bootstrap      done in 3m12s
bootstrap-all complete in 5m06s
```

If a phase fails, the corresponding subcommand can be re-run
solo: `apprafter cluster-bootstrap` re-runs Phase 3 against
the already-cached kubeconfig, etc.

For a no-touch preview of what the wrapper would do without
spending a single Hetzner cent: `apprafter bootstrap-all
--dry-run`.

## v0.1.83 — hotfix: apply/import fall back to target config (2026-05-14)

Walk-found bug in v0.1.82. The Track A.8 wiring resolved the
Hetzner *token* from the active target — but `provider` /
`region` / `cluster_name` still lived only in the legacy
`<cwd>/.apprafter/state.json`, written by `apprafter init`. So
the operator's natural flow

```
$ apprafter target show
Target: dev (active)
  Provider: hetzner-cloud
  Region:   nbg1
  Default tier: solo
...
$ apprafter apply
Error: state has no provider — run `apprafter init …` first
```

surfaced as a confusing mismatch — target store says everything
is configured, apply demands `init` anyway.

v0.1.83 finishes the wiring: `apply` and `import` now read
`provider`, `region`, and `cluster_name` from the active target
as fallback when `state.json` is empty. `init` stays as a
command for operators who prefer the explicit setup, but is no
longer mandatory after `target add`.

### Changed

- **`cli_core::target` gains two helpers** (`pub fn`,
  re-exported through the crate root):
  - `resolve_active_target_name(paths, target_override)
    -> Result<Option<String>>` — mirrors the precedence the
    credential resolver uses (override → active pointer →
    None). Used by both apply/import to look at the *same*
    target for both config and credentials within one
    invocation.
  - `load_active_target_config(paths, target_override)
    -> Option<TargetConfig>` — best-effort: returns `None`
    on any IO / parse failure rather than `Err`. Operational
    commands treat the target store strictly as a fallback;
    a broken target file shouldn't take them down when they
    could otherwise resolve from env or state.json. Real
    errors stay with the credential resolver.

- **`commands::apply`** now resolves the provider chain
  `state.provider → target_config.provider → typed error`.
  Same for `cluster_name` (`state → target_config → "platform-1"`)
  and `region` (`manifest → state → target_config → "nbg1"`).
  First-run convenience: after the resolved values are in
  hand, missing `state.json` fields are seeded so subsequent
  destroy/import/apply round-trips in this directory
  short-circuit through state.

- **`commands::import`** gets the same fallback wiring for
  `provider` and `cluster_name`. `region` is seeded from the
  target when state was empty.

- **Error message refresh** on the "no provider" gate now
  enumerates both paths:
  > no provider configured. Run `apprafter target add <name>
  > --provider hetzner-cloud …` (recommended) or the legacy
  > `apprafter init --provider hetzner-cloud --tier solo
  > --region nbg1`.

### Tests

- **New regression-guard** in `tests/cli_smoke.rs`:
  `apply_without_init_uses_active_target_config_for_provider`
  — seeds a target with full config in an isolated
  `APPRAFTER_CONFIG_DIR`, then runs `apply` from a CWD that
  has no `state.json`. Points the Hetzner client at a closed
  port so apply fails fast (transport error) rather than
  hitting the real API; the assertion is that apply got PAST
  the provider gate (stderr must NOT contain "state has no
  provider" / "run init first"). Pins the v0.1.83 wiring
  against the exact failure mode the walk surfaced.
- **Reworked** `import_without_provider_in_state_errors_clearly`:
  added `APPRAFTER_CONFIG_DIR` isolation so the test doesn't
  accidentally fall back to the developer's real
  `~/.config/apprafter/` target store. Updated assertion to
  match the enriched two-path error message.

### Backwards compatibility

- `apprafter init` continues to work unchanged for operators
  who prefer the explicit two-step setup.
- `HCLOUD_TOKEN=… apprafter apply` still works without a
  target store configured — v0.1.82 chain still terminates
  on the env var.
- Existing `state.json` files keep being authoritative when
  populated. The target-store fallback only fills GAPS.

### Operator note (the walk that surfaced this)

```
$ apprafter target add dev --provider hetzner-cloud --token "$T" \
    --region nbg1 --tier solo
$ apprafter target use dev    # automatic on first add
$ apprafter apply             # ← v0.1.83 makes this work; v0.1.82 errored
```

If you previously ran `apprafter init` to work around the
v0.1.82 gate, that's now redundant but harmless — `init` just
writes the same values to `state.json` that the resolver would
have pulled from the target.

## v0.1.82 — M1.5 Track A.8 — credential resolution chain (2026-05-14)

Eighth Track A slice. The day-to-day goal of the M1.5 CLI work
lands: `apprafter target use prod && apprafter apply` works
without exporting `HCLOUD_TOKEN` first. Operational commands
(`apply`, `destroy`, `import`) now thread their Hetzner Cloud
credentials through the v0.1.73-+ target store as a fall-back
under the existing env-var path. CI scripts that already export
`HCLOUD_TOKEN` keep working unchanged — env stays step 2 in the
chain.

### Added

- **`cli_core::credentials` module** with the
  `cli-dx-task.md §7` resolution chain:
    - `resolve_hetzner_token(cli_flag, paths, target_override)
      -> Result<String>`
        1. `cli_flag` — highest, for future `--token` flags.
        2. `HCLOUD_TOKEN` env — pre-target-store CI flow.
        3. Active target's `credentials.yaml` (or
           `--target <name>` override).
        4. Typed `CliError::Other` enumerating all three paths
           when nothing is configured — no silent placeholder.
    - `resolve_hetzner_ssh_public_key(paths, target_override)
      -> Result<Option<String>>` — same shape but for the SSH
      public key BODY (`APPRAFTER_SSH_PUBLIC_KEY` env > target
      `ssh_key_path` → read file). Returns `Ok(None)` when no
      key is configured anywhere; apply proceeds without one
      (Hetzner falls back to a root password).
    - `read_ssh_public_key_body(path)` pure helper exposed for
      callers (e.g. `apprafter doctor`) that want the parse
      without the full chain.
    - Constants `HCLOUD_TOKEN_ENV` / `SSH_PUBLIC_KEY_ENV` so
      every consumer references the same env-var name as
      clap's `#[arg(env)]` annotations.

- **`--target <name>` flag** on `apprafter apply`, `destroy`,
  `import`. Per-invocation override of the active target —
  useful for scripts that touch multiple targets without
  toggling `target use`. When the override names a missing
  target, the resolver surfaces the canonical
  "target `X` not found (available: …)" hint.

- **`cli_core::TEST_ENV_MUTEX`** (cfg(test)-gated `pub(crate)`
  static `Mutex<()>`). Cargo runs unit tests in parallel and
  the process-wide env-var space is a single resource — without
  a shared lock, two tests in `target::tests` and
  `credentials::tests` that flip `HCLOUD_TOKEN` /
  `APPRAFTER_CONFIG_DIR` would race. The mutex serialises every
  env-touching test in cli-core through one global gate.

### Changed

- **`commands::apply::run(target_override)`** drops direct
  `env::var("HCLOUD_TOKEN")` reads in favour of the resolver.
  `build_ssh_specs` now threads the target store + override
  through and calls `resolve_hetzner_ssh_public_key`. The
  manifest `sshKeys` block still wins on the precedence ladder
  (operator hand-edited the manifest, they meant it).

- **`commands::destroy::run(yes, target_override)`** —
  analogous wiring. The empty-state early exit fires BEFORE
  credential resolution so `destroy --yes` in a directory with
  no Hetzner state reports "nothing to destroy" instead of a
  credentials error.

- **`commands::import::run(force, dry_run, target_override)`**
  — analogous wiring.

### Tests (17 new + 3 reworked)

- 11 new unit tests in `cli_core::credentials::tests` covering
  the full resolution matrix:
    - flag wins / env wins / store falls back / `--target`
      picks named-not-active / 3-paths error / no-token-stored
      / missing-override / SSH env wins / SSH path read / SSH
      none / SSH unreadable.
  Each test uses the new shared `TEST_ENV_MUTEX` to keep env
  changes serialised.
- 1 new integration test in `tests/cli_smoke.rs`:
  `apply_target_flag_routes_resolution_at_named_target_and_surfaces_not_found`
  — seeds the target store, runs `apply --target ghost`, pins
  the error message contains `ghost`, `not found`, and the
  seeded target name as the "available" hint.
- 3 prior `apply_without_token_*` / `import_without_token_*`
  integration tests reworked: added `APPRAFTER_CONFIG_DIR`
  isolation tempdir (otherwise the resolver successfully
  falls back to the developer's real `~/.config/apprafter/`
  target store and the test asserts the wrong thing), and
  assertions now match the enriched error message that
  enumerates all three credential paths.

### Backwards compatibility

- `HCLOUD_TOKEN=… apprafter apply` keeps working without any
  changes (env is step 2 in the chain, above the target store).
- `APPRAFTER_SSH_PUBLIC_KEY=…` env keeps working likewise.
- Existing `init` + `state.json` flow is unaffected — the
  resolution chain only intervenes at credential read time;
  state location stays per-CWD until a future iteration
  unifies state with the target store.

### Out of scope (deferred)

- `--token <X>` flag on `apply` / `destroy` / `import`. Secrets
  on the shell command line land in history; we'll layer a
  `--token-stdin` style alternative as part of Track A.10
  (miette + UX hardening).
- Migration of `<cwd>/.apprafter/state.json` into the target
  store's `state/<name>/` tree — touched by Track A.9
  (`bootstrap-all`) alongside the orchestration pass.
- `kubeconfig` / `argocd-password` / `cluster-bootstrap` don't
  consume the Hetzner token directly (they work against the
  cluster's kubeconfig that was already cached during `apply`).
  No resolver change needed.
- `init` doesn't talk to any API; just writes state.json.

### Operator note

The new resolution lets you stop pasting `HCLOUD_TOKEN=… ` in
front of every operational command:
```
apprafter target add prod --provider hetzner-cloud --token "$HCLOUD_TOKEN"
apprafter target use prod
apprafter init --provider hetzner-cloud --tier solo --region nbg1
apprafter apply              # ← reads token from the target store
apprafter destroy --yes      # ← same
apprafter apply --target ci  # ← per-invocation override
```
The env var path keeps working unchanged for CI / scripts.

## v0.1.81 — M1.5 Track A.7 — apprafter doctor (2026-05-14)

Seventh Track A slice. `apprafter doctor` walks the active
target's stored state + reachability + the surrounding shell
environment and prints a PASS / WARN / FAIL line per check.
Three audiences per `cli-dx-task.md` §5.9: new-user
troubleshooting, CI smoke gates wiring `doctor` in as a
quality precondition, and bug-report templates that paste
the output verbatim.

### Added

- **`apprafter doctor`** top-level command with two flags:
  `--target <name>` to inspect a non-active target, and
  `--no-ping` (+ `APPRAFTER_NO_PING` env binding) to skip the
  Hetzner API round-trip. Output layout per spec:
  ```
  Checking target `default`...
    ✓ Config file readable (.../config.yaml)
    ✓ Credentials file present (mode 0600) (.../credentials.yaml)
    ✓ Provider `hetzner-cloud` supported
    ✓ Token format valid (64 chars, alphanumeric)
    ✓ Token verified against provider API (Hetzner Cloud /v1/locations, 142 ms)
    ✓ SSH key readable (~/.ssh/id_ed25519.pub (ssh-ed25519))

  Checking environment...
    ✓ `kubectl` on PATH (Client Version: ...)
    ✓ `helm` on PATH (v3.16.0+gXXXX)
    ✓ `ssh` on PATH (OpenSSH_9.6p1, ...)
    ✓ DNS resolves `api.hetzner.cloud` (443/tcp)

  10 checks for target `default`: 10 passed. All good — ready
  for `apprafter init` / `apprafter apply`.
  ```
  Failed checks land their hint on a second indented line so
  the operator gets the specific next step without having to
  copy-paste into a search engine. Exit code is 0 on
  PASS-only or WARN-only runs, 1 on any FAIL — wires straight
  into CI gates.

- **`Check { name, status, detail, hint }` + `DoctorReport`
  data layer**, kept separate from the side-effecting
  collection functions so rendering can be unit-tested
  without invoking real APIs. `CheckStatus::{Pass, Warn, Fail}`
  with corresponding glyphs `✓ / ⚠ / ✗`.

- **Target checks** (when a target is resolved):
  - Config file readable — exercised by `load_target`, fails
    fast with an "available targets: ..." hint when the name
    doesn't exist.
  - Credentials file present (mode 0600 on Unix; existence
    only on Windows) — WARN if mode drifted from 0600 with a
    `chmod 600` hint.
  - Provider `X` supported — whitelist match (`hetzner-cloud`
    today).
  - Token format valid — through `validate_hetzner_token_format`.
  - Token verified against provider API — runs
    `HetznerCloudValidator::validate_credentials()` with
    `Instant::now()` timing. 401 → FAIL with a `--renew`
    hint; other HTTP / transport → FAIL with a status-page /
    network hint; `--no-ping` or missing token → WARN.
  - SSH key readable — reads + parses the OpenSSH first
    line; surfaces the algo (`ssh-ed25519`, `ssh-rsa`, ...);
    FAIL when the configured path is missing on disk (stale
    target config); WARN when no path is set at all.

- **Env checks** (always):
  - `kubectl` / `helm` / `ssh` on PATH — runs the tool's
    `--version`-style invocation, captures stdout AND stderr
    (ssh writes to stderr), reports the first non-empty line
    as detail. Missing tool downgrades to WARN with an
    install hint — doctor is a diagnostic, not an installer.
  - DNS resolves `api.hetzner.cloud` — via
    `ToSocketAddrs::to_socket_addrs("api.hetzner.cloud:443")`.
    PASS with `443/tcp` detail; FAIL with resolver-error
    hint on the rare unreachable-DNS case.

### Tests (17 new)

- 11 unit in `commands::doctor::tests` covering pure helpers:
  - Glyph rendering.
  - `DoctorReport::{passed, warned, failed, has_failures}`.
  - `check_dns_resolves` happy + `.invalid` TLD (RFC 6761
    reserved as never-resolvable).
  - `check_tool` WARN branch on missing binary.
  - `check_provider_known` known/unknown.
  - `check_token_format` valid/missing.
  - `check_token_ping` `--no-ping` branch.
- 6 integration tests in `tests/doctor_test.rs`:
  - `doctor_on_empty_store_errors_with_onboarding_hint`
  - `doctor_renders_target_and_env_checks_with_summary` —
    pins the section structure + summary phrasing + that the
    `--no-ping` WARN doesn't fail the run.
  - `doctor_target_flag_inspects_non_active_target` — pins
    the `--target <name>` override.
  - `doctor_ssh_key_missing_path_fails_the_run_with_exit_1`
    — stale-config detection (path stored, file deleted).
  - `doctor_target_not_found_fails_with_available_hint` —
    "did you mean..." surface from the canonical
    TargetNotFound shape.
  - `doctor_summary_line_phrases_outcomes_clearly` — pins
    the "warning(s)" verbiage when WARN-only.

### Out of scope (deferred)

- "Region in known list" check — would need a hardcoded list
  (brittle as Hetzner adds DCs) or an API call (already part
  of the token ping). Implicit pass: if the ping succeeds with
  this region configured, the region is valid for the provider.
- "No active cluster" check — needs a per-target state cache
  to query (`state/<name>/state.json`). Lands with Track A.8
  (resolution chain plumbing).
- Color output for PASS/WARN/FAIL — Track A.11
  (color / NO_COLOR / --color flag pass).
- miette-style diagnostics — Track A.10 (error UX refinement).

### Operator note

`apprafter doctor` is wire-it-in-CI friendly:
```yaml
- run: apprafter doctor --target ci --no-ping
```
A non-zero exit means a FAIL fired; the report itself goes to
stdout so the CI log shows exactly which check tripped. For
shell-prompt or banner use the `--no-ping` flag (or the
`APPRAFTER_NO_PING=1` env var) to keep the run offline.

## v0.1.80 — M1.5 Track A.6 — whoami + auth stubs (2026-05-14)

Sixth Track A slice. `apprafter whoami` is the new "what shell am
I in?" one-shot command — identity + active target + verified
status of the active target's token, all in seven lines of
human-scannable output. `apprafter auth login/logout/status` are
hidden stubs reserving the namespace for AppRafter Cloud
(Managed) without crowding the new-user discovery surface.

### Added

- **`apprafter whoami`** top-level command, single flag
  `--no-ping` (+ env `APPRAFTER_NO_PING`). Renders:
    ```
    Identity:     anonymous (self-hosted mode)
    Target:       <name> (active)
    Provider:     hetzner-cloud (verified ✓)
    Region:       <region or "not set">
    Default tier: <tier or "not set">
    Cluster name: <name or "not set">
    SSH key:      ~/.ssh/<key>.pub (loaded | missing! | not set)
    ```
  Verified status comes from `HetznerCloudValidator::
  validate_credentials()` — same probe as `target add`'s ping.
  Failure modes degrade gracefully (no exit-1 on a flaky network):
    - 401 → `verification failed ✗ — token rejected (HTTP 401).
      Run \`apprafter target add <name> --renew\` to rotate.`
    - Other HTTP → `verification failed ✗ — HTTP <N> from
      provider API`
    - Transport → `verification failed ✗ — provider unreachable
      (network?)`
  Empty store path prints an onboarding hint pointing at
  `apprafter target add`.

- **`apprafter auth login / logout / status`** hidden stubs.
  `Commands::Auth` carries `#[command(hide = true)]` so it
  doesn't appear in `apprafter --help`, but `apprafter auth
  --help` still works for the intentionally-curious. Each
  subcommand prints a friendly redirect:
    - `login` / `logout`: "AppRafter Cloud is not yet available
      ... For self-hosted use, configure a deployment target
      instead: `apprafter target add`. Track managed
      availability at: https://apprafter.dev"
    - `status`: "AppRafter Cloud authentication: not available
      yet. Self-hosted mode active. Use `apprafter whoami`...".

  `AuthCommand` is a real `Subcommand` enum (not stub strings)
  so the future Managed implementation can fill in each handler
  without reshaping the CLI surface.

### Tests

- 5 unit tests in `commands::whoami`:
    - `verified_status_honours_no_ping_flag_without_hitting_network`
    - `verified_status_reports_no_token_path_when_credentials_empty`
    - `ssh_key_status_shows_loaded_for_existing_path`
    - `ssh_key_status_flags_missing_path_loudly`
    - `ssh_key_status_returns_not_set_for_none`
- 10 integration tests in `whoami_auth_test.rs`:
    - `whoami_on_empty_store_prints_onboarding_hint`
    - `whoami_with_active_target_renders_summary_and_honours_no_ping`
      — also pins that the synthetic token never appears in
      stdout (regression-guard on leakage).
    - `whoami_with_real_ping_reports_verified_on_mockito_200`
    - `whoami_with_real_ping_reports_failure_hint_on_mockito_401`
      — 401 path; pin that whoami exit code stays 0 + the
      error message includes `--renew` hint.
    - `whoami_with_real_ping_reports_failure_when_provider_unreachable`
      — closed-port branch.
    - `auth_login_prints_friendly_redirect_to_target_add`
    - `auth_logout_prints_friendly_redirect_with_nothing_to_logout_phrasing`
    - `auth_status_explains_self_hosted_mode_and_points_at_whoami`
    - `auth_group_is_hidden_from_top_level_help` — pins
      `apprafter --help` does NOT contain `auth`.
    - `auth_subcommand_help_is_still_reachable` — pins
      `apprafter auth --help` lists `login`/`logout`/`status`
      (hide ≠ remove).

### Out of scope (deferred)

- "Account: name@example.com / project: default" — Hetzner
  Cloud doesn't expose an account-info endpoint publicly. Skip
  until upstream changes.
- "Last used: N hours ago" — needs telemetry from operational
  commands (apply / cluster-bootstrap / kubeconfig) writing to
  the target's state cache. Lands with Track A.8 (resolution
  chain plumbing).
- "Cluster: provisioned (server_id …)" — needs the
  per-target state cache wired into `cli-state`. Also A.8.
- Real AppRafter Cloud auth handlers — Managed offering,
  far beyond M1.5.
- ADR `docs/adr/0014-cli-command-structure.md` documenting the
  `auth` namespace reservation + resource-first grouping
  decision — Track A.12 final docs + ADR pass.

### Operator note

`apprafter whoami` is a useful prompt-prefix command — drop it
in your shell `precmd` or a script's startup banner to confirm
which target's credentials your next command will use. The
`--no-ping` opt-out + the `APPRAFTER_NO_PING` env var make it
safe in tight shell loops or air-gapped contexts.

## v0.1.79 — M1.5 Track A.5 — target CRUD commands (2026-05-14)

Fifth Track A slice. Five new subcommands on top of the existing
target store from A.2/A.3, closing the day-to-day "manage multiple
targets" workflow per `cli-dx-task.md` §5.2–§5.6. Each command runs
against the same `~/.config/apprafter/` store the wizard writes to,
so a target created via `apprafter target add` is immediately
listable / switchable / showable / renameable / removable.

### Added

- **`apprafter target list`** — renders configured targets as a
  `tabled`-derived sharp-line table with columns Active / Name /
  Provider / Region / Tier. The `Active` column is a single `*`
  for the active target and blank otherwise so the marker scans
  visually. Trailing summary line: `N targets configured. Active:
  '<name>'.`. Empty-store path prints an onboarding hint pointing
  at `apprafter target add`. Unreadable target dirs are skipped
  with a `tracing::warn!` rather than erroring out the whole
  listing — bad single targets don't tank the table.
- **`apprafter target use <name>`** — swaps
  `GlobalConfig.active_target`. `load_target` is the existence
  probe so the canonical `TargetNotFound { available: ... }`
  error fires when the name is wrong. Polite no-op (`target
  <name> was already the active target`) when the name matches
  the current active pointer.
- **`apprafter target show [<name>]`** — prints Provider /
  Region / Default tier / Cluster name / SSH key / Hetzner
  token + the on-disk paths of `config.yaml` and `credentials.
  yaml`. Token render uses the new `token_summary` helper:
  `set (N chars; read credentials.yaml for the raw value)` or
  `not set`. No bytes leak into output — `target show` is safe
  to share in bug reports. `name` defaults to the active target;
  empty-store invocation errors with an onboarding hint.
- **`apprafter target rename <from> <to>`** — destination name
  validated through `check_target_name`; self-rename refused;
  `cli_core::target::rename_target` (new) atomically moves the
  target directory + best-effort moves the per-target state
  cache (`state/<from>/`). When `active_target == from` the
  global config is updated in the same operation, with the
  message `target renamed: \`from\` → \`to\` (active pointer
  updated)` so the operator sees what changed.
- **`apprafter target remove <name>`** — interactive Confirm
  prompt (default `false`) when on a TTY; non-TTY invocations
  require `--yes` explicitly with `non-interactive invocation:
  pass --yes to confirm removing target ...` (no silent
  destruction). Removing the active target re-points active to
  the alphabetically-next remaining target. Removing the last
  target deletes `config.yaml` entirely so the next
  `apprafter target add` greets the operator with the fresh-
  store path again.

- **`cli_core::target::rename_target(paths, from, to) ->
  Result<()>`** — atomic `fs::rename` of `targets/<from>/` to
  `targets/<to>/` with a best-effort move of `state/<from>/`
  when present. Returns `TargetNotFound` (with the canonical
  `available: ...` hint) when source is missing, typed `Other`
  when destination already exists. Re-exported through
  `cli_core` crate root.

### Tests

- 4 new `cli_core::target` unit tests covering `rename_target`:
  happy path with state cache, missing source, destination
  collision, no-state-cache path.
- 16 new integration tests in `target_test.rs`:
  - `target_list_on_empty_store_prints_onboarding_hint`
  - `target_list_renders_table_with_active_marker_and_columns`
  - `target_use_switches_active_pointer_and_reports_the_swap`
  - `target_use_on_already_active_is_a_polite_noop`
  - `target_use_on_missing_target_surfaces_available_hint`
  - `target_show_with_no_args_renders_active_target_with_masked_token`
    — pins that the synthetic token NEVER appears in stdout.
  - `target_show_with_explicit_name_renders_named_target_without_active_marker`
  - `target_show_on_empty_store_errors_with_onboarding_hint`
  - `target_rename_moves_files_and_updates_active_pointer`
  - `target_rename_non_active_target_leaves_active_pointer_alone`
  - `target_rename_refuses_when_destination_exists` — pins
    that BOTH targets survive the rejection (no half-rename
    damage).
  - `target_rename_rejects_invalid_destination_name`
  - `target_rename_refuses_identical_source_and_destination`
  - `target_remove_with_yes_flag_deletes_and_reassigns_active_alphabetically`
  - `target_remove_last_target_clears_active_pointer` — pins
    `config.yaml` disappears so the fresh-store path returns.
  - `target_remove_non_active_target_keeps_active_pointer_intact`
  - `target_remove_non_interactive_without_yes_refuses` — pins
    the "no silent destruction" guarantee.
  - `target_remove_on_missing_target_surfaces_available_hint`
- 1 new unit test `token_summary_renders_set_or_not_set_without_leaking_bytes`
  pinning that the show helper never echoes token bytes.
- `apprafter t list/use/show/rename/remove` alias coverage is
  inherited from the existing `target_alias_t_subcommand_resolves_to_target`
  test which exercises `#[command(alias = "t")]` on the Target
  group — no new alias-specific test needed.

### Out of scope (deferred)

- `Last used` / `Account` / `Cluster status` columns in
  `target list` + `target show` — needs telemetry wired through
  operational commands (A.8) and/or a Hetzner `/v1/me`-style
  endpoint that Hetzner doesn't publicly expose. Plan-md A.5
  closure note marks the gap.
- ADR `docs/adr/0014-cli-command-structure.md` (resource-first
  grouping + auth namespace) — Track A.12 (docs + ADR final pass).
- `apprafter whoami` / `apprafter auth …` stubs — Track A.6.

### Operator note

Existing operational commands (`apply`, `cluster-bootstrap`,
etc.) still read `HCLOUD_TOKEN` from the env. `target use`
flipping the active pointer does NOT affect them yet — Track
A.8 plumbs the resolution chain so operational commands consume
the active target's credentials when no env var is set.

## v0.1.78 — wizard UX polish #2 (2026-05-14)

Follow-up to v0.1.77 driven by the second walk-through. Three
behaviour fixes:

### Changed

- **`prompt_name` skips silently on prefill.** v0.1.76 always
  asked, with the supplied name as the default — minor friction
  but inconsistent with how the other prefilled fields behave.
  Now the prefilled `name` flows through silently with the same
  `ℹ Target name: <name> (from <source>)` announcement other
  fields use.

- **Origin labels for every prefilled wizard field.** v0.1.77
  already printed `ℹ Using token from HCLOUD_TOKEN env var ...`
  but the other prefilled fields just disappeared into "wizard
  didn't ask". Now each prefilled field prints
  `  ℹ <Field>: <value> (from <source>)` so the user can audit
  what's coming from where without re-reading the command line.
  Sources:
  - Name: `positional argument` (`apprafter target add <name>`).
  - Provider: `--provider flag`.
  - SSH key: `--ssh-key flag` or
    `APPRAFTER_SSH_PUBLIC_KEY_PATH env var` (clap's
    `#[arg(env)]` blends both into one `Option<PathBuf>`; the
    new `classify_ssh_key_source_with(prefill, env_value)` pure
    helper disambiguates the same way `classify_token_source`
    does for the token field).
  - Region: `--region flag`.
  - Default tier: `--tier flag`.

- **`run_renew` rejects identical-token "rotations".** The
  second walk surfaced that the renew flow silently accepted
  whatever the wizard or CLI handed it, including the existing
  token re-pasted by muscle memory — green "credentials
  rotated" message, zero actual change in Hetzner. v0.1.78
  compares the new token to `existing.credentials.hetzner_token`
  byte-for-byte after `require_token` validates it; on match
  the command fails with:
  ```
  `--renew` requires a NEW token, but the value provided is
  identical to the one already saved for target `<name>`.
  Generate a fresh token in the Hetzner Cloud Console →
  Security → API Tokens, then re-run `apprafter target add
  <name> --renew` with the new value.
  ```
  No `--force` override — re-saving the same token under the
  banner of "rotation" is always the wrong outcome; an operator
  who really wants to overwrite the credentials half can pass
  `apprafter target add <name> --force --token <X>` instead
  (deletes the renew safety net by going through the create
  path).

### Tests

- `classify_ssh_key_source_prefers_env_label_when_path_matches_env_value`
  — four-way table (env match / env diff / no env / no
  prefill) pinning the source resolution.
- `target_add_renew_rejects_identical_token_with_rotation_hint`
  — integration test: create target with token A, attempt
  `--renew` with same token A → command fails, error mentions
  "requires a NEW token" + "Hetzner Cloud Console", and the
  on-disk credentials file still contains the original token
  (no corruption from the failed renew).
- `target_add_renew_accepts_genuinely_new_token` — regression
  guard that the identical-token check only fires on identity,
  not on the happy rotation path.

### Operator note

The renew check is purely defensive — flagging an outcome the
operator clearly didn't intend. If you legitimately want to
overwrite a target's config without rotating credentials, use
`apprafter target add <name> --force --token <same>` (the
create flow, not renew).

## v0.1.77 — wizard UX polish + workspace version-bump policy (2026-05-14)

Follow-up to v0.1.76 driven by the first real walk-through of the
wizard. Five concrete issues operator reported, all addressed in
one patch:

### Changed

- **Workspace `Cargo.toml` version is now bumped per release.**
  The field drifted at `0.1.2` from the original v0.1.2 commit
  all the way through v0.1.76 — `apprafter --version` always
  printed `0.1.2` regardless of what was actually installed.
  CLAUDE.md "Versioning" rule amended to mandate the bump in
  every release commit; this commit lands `version = "0.1.77"`
  in `cli/Cargo.toml`, and the rule is honoured going forward.
- **Wizard now fires on every TTY** (unless `--no-interactive`).
  v0.1.76's `should_use_wizard` had a "skip when all required
  flags are supplied" short-circuit that turned out to be the
  wrong default: a user running `apprafter target add work
  --provider hetzner-cloud --region nbg1` on a TTY was getting
  no wizard at all because clap's `#[arg(env = "HCLOUD_TOKEN")]`
  silently filled the token from the env var — so optional
  fields like `--ssh-key` and `--tier` never got a chance to be
  prompted for. v0.1.77 drops the short-circuit; per-prompt
  prefill checks keep already-supplied fields silent while the
  optional ones still get a `Select`. Explicit `--no-interactive`
  remains the way to force pure flag mode.
- **Wizard's region picker now sorts by measured TCP latency.**
  After the token verifies, the wizard fires a parallel probe
  (`<region>-speed.hetzner.com:443`, bounded by 2 s overall) and
  presents the `Select` sorted ascending by ms. Unreachable
  probes fall to the bottom with `(  n/a   )` so they're still
  pickable. Hetzner's per-DC speedtest endpoints follow the
  `<region>-speed.hetzner.com` pattern (`nbg1-speed`,
  `fsn1-speed`, `hel1-speed`, `ash-speed`, `hil-speed`,
  `sin-speed`); when DNS doesn't resolve we degrade gracefully.

### Added

- **Token-from-env notification.** When the wizard receives the
  token via clap's `#[arg(env = "HCLOUD_TOKEN")]` rather than an
  explicit `--token` flag, it now prints
  `  ℹ Using token from HCLOUD_TOKEN env var (length N chars)`
  before the format/ping check. The `--token` flag case prints
  `  ℹ Using token from --token flag` for symmetry. The new
  `TokenSource` enum + `classify_token_source_with(prefill,
  env_value)` pure helper drive this — testable without touching
  real env.
- **SSH-key `Select` picker.** The wizard now scans
  `~/.ssh/*.pub`, presents matching files as a `Select`, and
  appends two extra entries — `Other (type a path)` and `Skip
  (don't attach an SSH key now)`. Each `*.pub` entry shows the
  abbreviated path (`~/.ssh/...`) plus the parsed OpenSSH algo
  and comment (`(ssh-ed25519, me@laptop)`), so users with
  multiple keys can pick the right one without recalling
  filenames. Empty `~/.ssh/` falls back to the v0.1.76 Text
  input with `~/` tilde expansion intact.
- **`RegionInfo` carries latency in the picker** via the new
  `RegionWithLatency` wrapper. `Display` impl renders
  `nbg1 — Nuremberg DC Park 1  (  24 ms)` so the picker label
  doubles as a snapshot of connectivity.

### Tests

- **10 new unit tests** in `commands::target_wizard`:
  - `should_use_wizard_fires_on_tty_unless_no_interactive` —
    replaces the old four-arg test; pins the simplified rule.
  - `classify_token_source_distinguishes_env_flag_and_none` —
    four-way table (env matches, env differs, env unset, no
    prefill).
  - `scan_ssh_pub_keys_in_returns_empty_for_missing_or_empty_dirs`
    — `None` / non-existent / empty.
  - `scan_ssh_pub_keys_in_returns_only_pub_files_sorted_alphabetically`
    — mixes private keys, configs, subdirs, and pin that only
    `*.pub` regular files in the top-level dir are returned.
  - `ssh_key_label_emits_path_algo_and_comment_when_present`.
  - `ssh_key_label_falls_back_to_path_when_file_is_unreadable`.
  - `abbreviate_home_path_collapses_home_to_tilde` — happy +
    out-of-home + no-home cases.
  - `measure_region_latencies_sorts_unreachable_last_and_preserves_known`
    — uses RFC-6761 reserved `.invalid` TLD so probes
    deterministically fail; pins that every input region gets
    exactly one output entry.
  - `region_with_latency_display_marks_unreachable_distinctly`.

### Operator note

If you reinstalled `apprafter` from this commit, `apprafter
--version` will now print `apprafter 0.1.77`. Anyone who built
from a tag between v0.1.3 and v0.1.76 sees `apprafter 0.1.2`
permanently in those binaries — the in-place version was never
updated; only the git tag moved.

## v0.1.76 — M1.5 Track A.4b — interactive wizard via inquire (2026-05-14)

Second half of M1.5 Track A.4. `apprafter target add` now opens an
interactive wizard when stdin + stdout are both TTYs and
`--no-interactive` is not set, prompting only for the inputs the
user didn't already supply via flags. Closes the
"`apprafter target add` on a fresh terminal walks me through it"
UX from `cli-dx-task.md` §5.1.

### Added

- **`commands::target_wizard` module** with `inquire`-driven
  prompts following the six-step sequence from `cli-dx-task.md`
  §5.1:
  1. Target name (`Text`, default `default`, validates against
     `check_target_name`).
  2. Provider (`Select`, single entry `hetzner-cloud` today;
     kept as a Select so adding AWS/Managed later is a one-line
     surface change).
  3. Provider token (`Password`, masked). The validator runs
     inline on submit — format check via
     `validate_hetzner_token_format`, then API ping via
     `HetznerCloudValidator::validate_credentials()` unless
     `--no-ping` was passed. Failure → `Validation::Invalid`
     so the user gets to retry without re-running the entire
     wizard.
  4. SSH public key (`Text`, default
     `<home>/.ssh/id_ed25519.pub` resolved via `dirs::home_dir`;
     empty answer = skip). Validator checks the path exists.
     Tilde expansion (`~/...`) via the new `expand_tilde`
     helper.
  5. Default region (`Select`, populated by
     `validator.list_regions()`). With `--no-ping` falls back
     to a `Text` with default `nbg1` because we can't query the
     API.
  6. Default tier (`Select` with one-of `solo / team / prod /
     regulated` and human-readable labels lifted from
     `spec.md`).
- **`run_renew_wizard(provider, no_ping)`** — slim version
  that only prompts for the new token; the existing target's
  config (provider, region, tier, ...) is preserved.
- **`should_use_wizard(no_interactive, stdin_tty, stdout_tty,
  has_all_required_flags)`** pure decision function.  Pulled
  out so the wizard-fire condition is unit-testable without
  faking a PTY; the caller side resolves `stdin_tty` /
  `stdout_tty` via `std::io::IsTerminal`. Wizard fires only
  when both consoles are TTYs AND `--no-interactive` is unset
  AND at least one required input is missing — power users who
  pass full flags don't get a surprise prompt.
- **`ProviderValidator::list_regions() -> Result<Vec<RegionInfo>>`**
  on the trait. `RegionInfo { name, description }` with `Display`
  rendering `<name> — <description>` so `inquire::Select` labels
  read naturally. `HetznerCloudValidator::list_regions()` maps
  `client.list_locations()` into a sorted-by-name list.
- **`cli-core::target::check_target_name`** pure helper
  exported for the wizard. Returns `Result<(), String>` so the
  same validation message renders in both the CLI surface
  (`CliError::Other`) and the wizard prompt
  (`inquire::Validation::Invalid`). `validate_target_name` is
  now a thin `CliError`-wrapping adapter.
- **Workspace deps**: `inquire = "0.7"`; `dirs` promoted to a
  direct dep of `platform-cli` (used for SSH-key default + tilde
  expansion).
- **`AddArgs.no_interactive` field** now carries the clap flag
  value through to the orchestrator instead of being discarded
  in the destructure — the wizard-decision function consumes it.

### Changed

- **`TargetCommand::Add.name`** is now `Option<String>` (was
  `String`). The wizard prompts for it on a TTY; in non-TTY /
  `--no-interactive` mode the existing-error path fires with
  "target name required — pass it as a positional argument
  (`apprafter target add <name>`) or run on a TTY to enter the
  wizard." Backwards-compatible for all v0.1.73-style positional
  invocations.
- **`run_add` orchestration** restructured around the wizard
  decision: parse flags → maybe-wizard fills gaps →
  validate-name → save. `run_renew` takes the resolved name as
  an explicit parameter to match.

### Tests

- **5 new unit tests** in `commands::target_wizard`:
  - `should_use_wizard_only_when_tty_and_no_flag_and_missing_required`
    — pins the four-way decision matrix.
  - `expand_tilde_replaces_leading_tilde_slash_only` — abs path
    untouched, `~user/foo` NOT expanded (predictable behaviour).
  - `inline_ping_error_summarises_401_separately_from_other_http_errors`
    — pins the wizard's one-line error renderer that feeds
    `inquire::Validation::Invalid`.
  - `validate_for_provider_accepts_hetzner_64_char_token_and_rejects_others`
    — happy / wrong-length / unknown-provider.
  - `tier_choice_display_includes_both_key_and_label` — pins
    Display impl that drives the Select labels.
- **2 new mockito-based tests** in `cli_providers::validators`:
  - `list_regions_returns_sorted_region_info_from_locations_response`
    — pins both the wire-shape mapping (Location → RegionInfo)
    and the alphabetic sort.
  - `region_info_display_falls_back_to_name_when_description_empty`
    — pins the Display fallback so a stripped Hetzner response
    doesn't render `nbg1 — ` with a trailing em-dash.
- **22 prior integration tests** in `target_test.rs` continue
  to pass unmodified — assert_cmd pipes stdin/stdout so
  `IsTerminal` returns false and the wizard is correctly
  skipped. The non-TTY path is exactly the v0.1.75 behaviour.

### Out of scope (deferred)

- E2E wizard test via PTY harness — overkill for the v1
  shape; manual walks cover prompt UX. If we add a PTY harness
  later, it would also unlock testing of progress bars in
  `bootstrap-all` (Track A.9).
- Hetzner account/project details in the "✓ Token verified"
  line per `cli-dx-task.md` §5.1 — Hetzner's `/v1/locations`
  doesn't return that info; would need an account-scoped
  endpoint Hetzner doesn't currently expose. Current "✓ Token
  verified" string is the achievable signal.
- `apprafter target list / use / show / rename / remove` —
  Track 1.66A.5.
- `secrecy::Secret<String>` wrapper for in-memory tokens —
  Track A.10/A.11 hardening pass.

### Operator note

Existing CI / script invocations are unaffected — wizard fires
only on real terminals + when required inputs are missing.
`HCLOUD_TOKEN=… APPRAFTER_NO_PING=1 apprafter target add work
--provider hetzner-cloud` continues to work non-interactively
out of the box. To force non-interactive mode in a TTY (e.g.,
when piping prompts is too clever), pass `--no-interactive` or
make sure every required input is on the command line.

## v0.1.75 — M1.5 Track A.4a — provider validator + Hetzner API ping (2026-05-14)

First half of M1.5 Track A.4 (`cli-dx-task.md` §11 validation
framework + §5.1 "token verified" UX). `apprafter target add` now
actually confirms the token authenticates with Hetzner Cloud
before saving — no more half-state on disk after a bad-token
accident. The interactive wizard half of A.4 follows as A.4b
(v0.1.76); splitting into two iterations keeps each set of tests
focused.

### Added

- **`cli_providers::validators` module** with a minimal
  `ProviderValidator` trait:
  ```rust
  pub trait ProviderValidator {
      fn validate_credentials(&self) -> Result<()>;
  }
  ```
  Region / type lookups intentionally **not** in the trait yet —
  they're wizard-side concerns and arrive with A.4b so the trait
  doesn't grow speculative surface.
- **`HetznerCloudValidator`** implementation. Owns a
  `HetznerCloudClient`; `validate_credentials()` calls
  `client.list_locations()` and discards the payload.
  `GET /v1/locations` is Hetzner's own recommendation for cheap
  auth probes — any valid token can read it, no quota spent, no
  resources touched. Mockito-based unit tests cover 200 OK,
  401 typed-error, and transport-error (closed port).
- **`cli_providers::hetzner_cloud::types::{Location,
  LocationListResponse}`** wire-types — re-exported through the
  crate root. Wizard's region picker in A.4b consumes the same
  shape.
- **`cli_providers::hetzner_cloud::client::list_locations()`** —
  drop-in alongside the other `list_X` methods; same error
  mapping as the rest (2xx → parse, 4xx/5xx → `CliError::Hetzner`,
  transport-fail → `CliError::Other`).
- **`apprafter target add --no-ping`** flag (clap) plus
  **`APPRAFTER_NO_PING`** env-var binding through
  `BoolishValueParser`. The parser accepts `1 / 0 / yes / no /
  true / false / on / off` so shell scripts can write the
  natural `APPRAFTER_NO_PING=1` without clap's strict
  `true`/`false` rejecting it. Skips the round-trip for CI sand­
  boxes and offline pre-seeding flows.
- **`ping_provider(provider, token)` orchestrator** in
  `commands::target`. For hetzner-cloud it instantiates the
  validator against `hcloud_base_url()` (honours
  `APPRAFTER_HCLOUD_BASE_URL` for mockito-driven integration
  tests, falls back to the upstream URL otherwise) and maps the
  raw error into a human-readable surface:
  - 401 → "Hetzner Cloud rejected the token (HTTP 401):
    {message}. Double-check that the token has not been revoked
    and was copied complete (64 chars)."
  - Other HTTP → "Hetzner Cloud API ping failed (HTTP {status}):
    {message}. The target was NOT saved; rerun once the API is
    reachable, or pass `--no-ping` to skip the check."
  - Transport → "could not reach Hetzner Cloud at {base}: {msg}.
    Pass `--no-ping` to skip the network round-trip (offline /
    CI setups)."
- **Success-message extension** announces verification status:
  `… (token verified against Hetzner Cloud)` when the ping ran
  and `… (token NOT verified — \`--no-ping\` was passed)` when
  it didn't. Closes the "✓ Token verified" promise from
  `cli-dx-task.md` §5.1 on the non-interactive flow; the
  interactive wizard in A.4b will reuse the same string for
  consistency.

### Changed

- **`run_add`** now does the API ping after format / ssh-key
  checks but **before** `save_target`, so a 401 leaves no
  half-state on disk. Same ordering applied to `run_renew` for
  the credential-rotation path.

### Tests

- **3 mockito-based unit tests** in `cli_providers::validators`:
  200 OK happy path, 401 typed `CliError::Hetzner` surface,
  closed-port transport error.
- **5 new integration tests** in `tests/target_test.rs`:
  - `target_add_pings_provider_by_default_and_announces_verified_status`
    — `mockito::Mock::expect(1)` pins that the ping really
    happened; success message shows "verified".
  - `target_add_surfaces_typed_error_on_hetzner_401` — 401 path;
    assertion that the target dir does NOT exist after the
    failure (no half-state).
  - `target_add_surfaces_helpful_error_when_api_is_unreachable`
    — closed port; the error message must include either
    "could not reach" (Unix transport) or "API ping failed"
    (some sandboxes synthesise a 5xx), plus the `--no-ping`
    hint regardless.
  - `target_add_no_ping_flag_skips_validator_and_announces_unverified`
    — `--no-ping` short-circuits even when the base URL points
    at a closed port.
  - `target_add_no_ping_env_var_also_skips_validator` —
    `APPRAFTER_NO_PING=1` env binding equivalent to the flag.
- **17 pre-existing target_test cases updated** with
  `APPRAFTER_NO_PING=1` injected next to `APPRAFTER_CONFIG_DIR`
  in their `.env()` setup. Those tests are about file-store /
  flag parsing, not the API — the new ping-specific tests cover
  the validation path exclusively, so the focus split is clean.

### Out of scope (deferred to A.4b and beyond)

- Interactive wizard via `inquire` (default flow when TTY
  detected + no `--no-interactive`) — Track 1.66A.4b /
  v0.1.76.
- Region validator + region-picker (`list_regions()` on the
  trait, autocomplete in the wizard) — A.4b ships these
  together since the picker is the consumer.
- `secrecy::Secret<String>` wrapper for in-memory tokens — A.10
  / A.11 hardening pass.
- Resolution chain plumbing into `init` / `apply` /
  `cluster-bootstrap` so they consume the active target's
  credentials — A.8.

### Operator note

If you previously got into the habit of running `apprafter
target add` without internet access (e.g. air-gapped lab), pass
`--no-ping` or set `APPRAFTER_NO_PING=1` once in your shell
profile. The flag is documented in `apprafter target add
--help`; the env var name is stable contract.

## v0.1.74 — hotfix: token validator rejected real Hetzner Cloud tokens (2026-05-14)

v0.1.73's `validate_hetzner_token_format` required an `hcloud_`
prefix on every Hetzner Cloud API token — a constraint borrowed
from `cli-dx-task.md` §11 that turned out to be wrong. Real
Hetzner Cloud tokens (the ones the Cloud Console → Security → API
Tokens panel emits when you click "Create API token") are **64
ASCII alphanumeric characters with no prefix**. The `HCLOUD_TOKEN`
env-var name is convention, the value inside it is just the bare
64 chars. v0.1.73 rejected every real token with the surface
error `invalid Hetzner Cloud token: Hetzner Cloud tokens must
start with \`hcloud_\``.

### Fixed

- **`validate_hetzner_token_format`** rewritten to require exactly
  64 ASCII alphanumeric characters, no prefix:
  ```rust
  if token.len() != 64 { return Err(…); }
  if !token.chars().all(|c| c.is_ascii_alphanumeric()) { return Err(…); }
  ```
  The previous prefix-stripping logic is gone — Hetzner doesn't
  ship prefixed tokens, so the strict alphanumeric check matches
  the actual format. If Hetzner ever introduces prefixed tokens
  (secret-scanning style), we revisit the validator at that point.
- **`cli-dx-task.md` §11** amended: the canonical Hetzner Cloud
  token format is exactly 64 ASCII alphanumeric chars with no
  prefix, in both the "validation steps" enumeration and the
  per-provider validation table.

### Tests

Replaced the v0.1.73 prefix-centric tests with strict-format
ones:

- `validate_hetzner_token_format_accepts_canonical_64_char_token`
  — happy path on the real shape.
- `validate_hetzner_token_format_rejects_wrong_length` — strict
  equality covering 5 / 63 / 65 / 200 chars in one parameterised
  test.
- `validate_hetzner_token_format_rejects_non_alphanumeric_at_correct_length`
  — 64-char token with a dash; length passes, alphanumeric branch
  fires.
- `validate_hetzner_token_format_rejects_underscore_at_correct_length`
  — explicit regression-guard pinning the strict alphanumeric
  rule against any future "let's accept `hcloud_` prefix" change.
  64 chars total (`hcloud_` + 57 `a`s) so length passes and the
  alphanumeric check is what rejects.

Tests in `commands/target.rs` and `tests/target_test.rs` updated
to use the canonical 64-char alphanumeric synthetic shape
(`"a".repeat(64)` instead of `format!("hcloud_{a×60}")`).
`synthetic_hcloud_token()` helper renamed to
`synthetic_hetzner_token()` to reflect the actual format.

### Operator note

Anyone who ran `apprafter target add` after upgrading to v0.1.73
and got the cryptic "must start with `hcloud_`" error can just
retry the same command on v0.1.74 — your real token now passes
validation. No state migration required; v0.1.73 didn't save
anything when it rejected the token. If you already created a
target via the env-var-only flow before v0.1.73 (e.g., HCLOUD_TOKEN=…
apprafter apply) nothing changed for you — that flow doesn't go
through the validator at all and continues to work unchanged
until Track A.8 wires the target store in.

## v0.1.73 — M1.5 Track A.3 — `apprafter target add` non-interactive (2026-05-14)

Third slice of M1.5 Track A (CLI DX rework per `cli-dx-task.md`
§5.1, §11, §17 row 3). First user-visible piece of the target
store — `apprafter target add <name>` saves a deployment target
to the on-disk store from v0.1.72 with pure flag-driven UX.
**No interactive wizard yet** — Track A.4 (v0.1.74) layers
`inquire`-based prompts on top of this exact handler.

### Added

- **`apprafter target` subcommand group** in `cli/platform-cli/
  src/cli.rs` with `#[command(alias = "t")]`, so `apprafter t add
  …` works identically to `apprafter target add …`. Subcommand
  enum `TargetCommand` is room-to-grow (Track A.5 adds `List`,
  `Use`, `Show`, `Rename`, `Remove`).
- **`apprafter target add <name>`** with flags per `cli-dx-task.md`
  §5.1:
  - `--provider hetzner-cloud` (only provider in v0.1.73; AWS /
    Managed arrive later).
  - `--token <hcloud_…>` — Hetzner Cloud API token. Reads
    `HCLOUD_TOKEN` env var via clap's `#[arg(env)]` for CI
    ergonomics so existing scripts work without retyping the
    flag.
  - `--ssh-key <path>` — optional SSH public key path. Reads
    `APPRAFTER_SSH_PUBLIC_KEY_PATH` env. Stored as a path
    (not the key body) so the user's `~/.ssh/` remains the
    source of truth.
  - `--region nbg1 / --tier solo / --cluster-name platform-1`
    — defaults for downstream commands.
  - `--force` — overwrite an existing target. Without it, the
    command refuses (error message mentions both `--force` and
    `--renew` so the user picks the right path).
  - `--renew` — rotate only credentials of an existing target.
    Errors when target doesn't exist; refuses config flags
    (provider/region/tier/cluster-name); accepts new
    `--token` and optionally new `--ssh-key`. Mutually
    exclusive with `--force` (clap-level `conflicts_with`).
  - `--no-interactive` — placeholder for Track A.4. v0.1.73 is
    already non-interactive regardless; accepting the flag now
    keeps shell aliases stable across the upgrade.
- **`commands::target` handler module** orchestrating create /
  overwrite / renew semantics:
  - `validate_target_name(name)` — non-empty, ≤ 64 chars,
    `[A-Za-z0-9-]+`, no leading/trailing `-`. Filesystem safety
    + kubectl-style legibility.
  - `require_known_provider(opt)` — non-None + whitelist
    (`["hetzner-cloud"]`). Surfaces a typed error so users
    don't save a half-working config for a provider that's
    not wired.
  - `require_token(provider, opt)` — non-None + per-provider
    format validation. For hetzner-cloud invokes
    `cli_core::target::validate_hetzner_token_format`.
  - `verify_ssh_key_readable(path)` — `path.exists()` +
    `read_to_string` success (catches unreadable/typo paths).
  - `run_renew(paths, args)` — loads existing target, swaps
    credentials, preserves config.
  - `ensure_active_target(paths, name)` — promotes the
    just-saved target to active **only** when no `GlobalConfig`
    exists yet (first-run case). Subsequent target saves leave
    the active pointer alone; users switch explicitly via
    `apprafter target use <name>` (Track A.5).
- **`cli_core::target::CONFIG_DIR_ENV = "APPRAFTER_CONFIG_DIR"`**
  — env override that points `default_config_root()` at an
  alternate root. Primary use is integration tests (every test
  in `target_test.rs` runs against a fresh `tempfile::TempDir`),
  but power users get the same redirect for compartmentalised
  experiments. Used verbatim — no `apprafter/` suffix appended,
  so tests can point straight at their tempdir. Empty string is
  treated as unset to avoid `APPRAFTER_CONFIG_DIR= apprafter
  target list` accidentally pointing at the CWD.
- **`cli_core::target::validate_hetzner_token_format(token) ->
  Result<(), String>`** — pre-flight check for the
  `^hcloud_[a-zA-Z0-9]{60,}$` shape per `cli-dx-task.md` §11.
  Implemented with `str` methods instead of pulling in the
  `regex` crate — the pattern is simple enough that the manual
  check is clearer and dep-lighter. Real `GET /v1/locations`
  API ping lives in Track A.4 (validator framework).

### Tests

- **17 integration tests** in `cli/platform-cli/tests/
  target_test.rs`:
  - Happy path (saves both files, sets active, creates
    `auth/.keep` placeholder).
  - Mode 0600 on `credentials.yaml` (Unix-only `#[cfg(unix)]`).
  - `HCLOUD_TOKEN` env-var fallback works (clap `#[arg(env)]`).
  - Missing `--token` errors with hint about both flag + env.
  - Unknown provider, malformed token, invalid name all error
    with typed messages.
  - Existing target without `--force` → refuses; error mentions
    both `--force` and `--renew` paths.
  - `--force` overwrites; second-save does NOT change active
    pointer.
  - `--renew` rotates credentials; preserves region/cluster_name
    in the on-disk config.
  - `--renew` on missing target → error hinting "drop --renew".
  - `--renew` + config flag (`--region`) → error refusing the
    combination.
  - `--force --renew` together → clap-level conflict reject.
  - `--ssh-key` happy path saves the path verbatim into config.
  - `--ssh-key` pointing at non-existent file → error.
  - Second target save keeps the first as active (deferred
    switching to Track A.5).
  - `apprafter t add …` alias resolves identically.
- **10 unit tests** inline in `commands/target.rs` covering the
  pure validators (`validate_target_name` × 5,
  `require_known_provider` × 2, `require_token` × 2,
  `verify_ssh_key_readable` × 2).
- **6 unit tests** in `cli_core::target` covering the env
  override + format validator
  (`default_config_root_honours_apprafter_config_dir_env_override`,
  `_ignores_empty_env_override`,
  `validate_hetzner_token_format` × 4 — happy / missing prefix /
  too short / non-alphanumeric).

### Out of scope (deferred to subsequent Track A slots)

- Interactive wizard via `inquire` (default flow when TTY
  detected, no `--no-interactive`) — Track A.4 / v0.1.74.
- `apprafter target list / use / show / rename / remove` — Track
  A.5.
- Real Hetzner API ping (`GET /v1/locations`) as part of
  `target add` validation — Track A.4 validator framework.
- `apprafter whoami` aggregator (active target + verified
  status) — Track A.6.
- Resolution chain plumbed into `init / apply /
  cluster-bootstrap` (active target's token used when no env
  var or `--token` flag) — Track A.8.

### Operator note

The new command does **not** affect existing workflows.
`HCLOUD_TOKEN` + `APPRAFTER_SSH_PUBLIC_KEY` env-var-based
`apprafter apply` keeps working unchanged — Track A.8 wires the
target store into operational commands as a *fallback* below the
env-var override, per the resolution chain documented in
`cli-dx-task.md` §7. Until A.8 lands, `apprafter target add` is
informational only (saves config + credentials, but
`apply / cluster-bootstrap` still read env vars).

## v0.1.72 — M1.5 Track A.2 — target file structure + IO module (2026-05-14)

Second slice of M1.5 Track A (CLI DX rework per `cli-dx-task.md` §17
row 2). Foundation for the persistent target store that lets users
configure multiple deployment targets, pick one active at a time,
and drop the per-shell `HCLOUD_TOKEN=… APPRAFTER_SSH_PUBLIC_KEY=…`
incantation. **No CLI commands wired in this release** — pure IO
layer that subsequent Track A iterations build on (A.3 non-interactive
`target add`, A.4 interactive wizard, A.5 `list/use/show/rename/remove`,
A.8 plumb resolution chain into existing operational commands).

### Added

- **`cli-core::target` module** implementing the file layout from
  `cli-dx-task.md` §4 (`$XDG_CONFIG_HOME/apprafter/{config.yaml,
  targets/<name>/{config,credentials}.yaml, auth/.keep, state/<name>/}`):
  - `default_config_root() -> Result<PathBuf>` — cross-platform XDG
    resolution via `dirs::config_dir()`.
  - `TargetStorePaths` — testable locator (carries `root`) with
    methods for every file/dir in the spec.
  - `GlobalConfig { active_target, version }` + `TARGET_STORE_VERSION
    = 1` forward-compat marker.
  - `TargetConfig { provider, region, default_tier, cluster_name,
    ssh_key_path }` non-secret half.
  - `TargetCredentials { hetzner_token: Option<String> }` secret
    half with **manual `Debug` impl** that emits `<redacted>` —
    deriving `Debug` would leak the token in any `println!("{:?}", …)`
    or `tracing::debug!(creds = ?creds, …)` call. Catches stray
    log statements at compile time of the call site, not after a
    secrets leak in the wild.
  - `Target { name, config, credentials }` — in-memory composition
    of both halves.
  - `load_global_config / save_global_config / load_target /
    save_target / list_target_names / remove_target` IO surface.
  - Internal `atomic_write(path, bytes, secret)`: tempfile in the
    target dir → `write_all` + `sync_all` → `set_permissions` (0600
    for `secret = true`, 0644 otherwise) → `persist()` atomic rename
    over the final path. No window where the credentials file is
    briefly world-readable; no half-written file readable by an
    interrupted load.
- **`auth/.keep` placeholder** auto-created on first save so the
  reserved `apprafter auth` namespace directory exists from the
  beginning. Track A.6 fills it when Managed AppRafter Cloud login
  lands.
- **New `CliError` variants**:
  - `InvalidTargetConfig { path, message }` — surface for corrupt
    YAML so error UX can suggest `apprafter target add <name>
    --renew` instead of `apprafter init`.
  - `TargetNotFound { name, available }` — `available` is the
    comma-joined list of configured names so error messages can
    show "did you mean..." without an extra round-trip.
  - `Yaml(serde_yaml::Error)` via `#[from]` for ergonomic `?`
    propagation.
- **Workspace dependencies**:
  - `serde_yaml = "0.9"` — wire format (more human-friendly than
    JSON for hand-edited config; cli-dx-task.md §8 mandates it).
  - `dirs = "5"` — cross-platform XDG path resolution.
  - `tempfile` promoted from `dev-dependencies` to regular
    `dependencies` of `cli-core` since `atomic_write` uses it in
    prod code, not just tests.

### Tests

16 new regression-guards (inline in `target.rs`) covering every
contract worth pinning:

- `default_config_root_points_at_user_config_dir_under_apprafter`
  — leaf path is `apprafter/` regardless of host XDG.
- `paths_compose_per_spec_directory_layout` — pins each
  `TargetStorePaths` method against the spec layout so renaming
  a constant fails loudly.
- `load_global_config_returns_none_on_fresh_store` — first-run
  doesn't surface as an error.
- `save_then_load_global_round_trips_active_target` — global YAML
  round-trip.
- `save_global_creates_auth_placeholder_directory` — `auth/.keep`
  invariant.
- `load_global_config_returns_invalid_target_config_on_corrupt_yaml`
  — typed error with file path.
- `save_then_load_target_round_trips_both_halves` — per-target
  config + credentials round-trip including the Hetzner token.
- `load_target_returns_target_not_found_with_available_list` —
  error message contains comma-joined available names.
- `load_target_tolerates_missing_credentials_file` — dotfiles-only
  scenario (user pulled config.yaml from git, hasn't run `target
  add --renew` yet).
- `credentials_file_lands_at_mode_0600` (Unix-only) — credentials
  YAML is 0600; sibling `config.yaml` is 0644 (mode pin per spec).
- `list_target_names_returns_empty_on_fresh_store` +
  `list_target_names_returns_sorted_names_skipping_dot_dirs` —
  lexicographic sort, dot-prefixed dirs hidden (so the atomic-write
  tempfile leftover prefix `.apprafter-tgt-*` never leaks into
  user-facing `target list` output).
- `remove_target_deletes_both_files_and_state_dir` — cascading
  delete also clears `state/<name>/`.
- `remove_target_returns_target_not_found_when_missing` — caller
  pattern-matches for idempotent delete.
- `credentials_debug_redacts_token` — `<redacted>` marker present
  in Debug format; raw token absent.
- `atomic_write_leaves_no_temp_files_on_success` — no
  `.apprafter-tgt-*.tmp` files in `<root>/` after a clean save.

### Out of scope (deferred to subsequent Track A slots)

- CLI commands — `apprafter target add/list/use/show/rename/remove`
  arrive in Track A.3 (non-interactive flag-driven) → A.4
  (interactive wizard via `inquire`) → A.5 (the read/rename/remove
  set).
- Provider validator framework (token regex + API ping) — Track
  A.4.
- Resolution chain (CLI flag > env var > target store) plumbed
  into existing `init / apply / cluster-bootstrap` commands —
  Track A.8.
- Migration of `<cwd>/.apprafter/state.json` to per-target
  `<root>/state/<name>/` — Track A.8 (gradual; env-var flow stays
  the highest-priority override per `cli-dx-task.md` §7 so the
  migration is purely additive).
- `secrecy::Secret<String>` in-memory wrapper — defer to Track A.3
  where credentials hit hot in-memory paths (CLI flag parsing →
  YAML write). The manual `Debug` redact + 0600 mode already cover
  the dominant risk vector (accidental logging).

## v0.1.71 — 1.4 AUDIT — Cilium Helm values dual-stack (2026-05-14)

Second slice of the Phase 1 audit backlog. v0.1.70 made the substrate
(Hetzner provider + k3s) dual-stack-aware; v0.1.71 closes the CNI
half — Cilium now actually allocates IPv6 addresses to pods instead
of silently dropping the v6 podCIDR k3s offers.

### Changed

- **`cli-providers::k8s::cilium_values::cilium_values_yaml()`** now
  emits explicit `ipv4: { enabled: true }` + `ipv6: { enabled: true }`
  blocks. Cilium chart v1.16.x defaults to `ipv4.enabled: true` but
  `ipv6.enabled: false`. Without `ipv6.enabled` declared, the Cilium
  datapath ignores the v6 podCIDR even when k3s advertises one in
  `node.spec.podCIDRs`. After this release, pods on Tier 1 clusters
  receive both v4 (from `10.42.0.0/16`) and v6 (from `fd00:42::/64`)
  IPs on creation. Doc comment on the builder updated to point at
  ADR 0017 §Pod network.

### Added

- **`e2e/mvp.sh` Phase 6.4 — dual-stack podIPs assertion.** After
  the existing Phase 6 endpoint curl passes, the script reads
  `kubectl get pod -l app=e2e-hello -o jsonpath='{.items[0].status.
  podIPs[*].ip}'` and asserts the pod has BOTH a v4 IP matching
  `10.42.*` (k3s default pod CIDR) AND a v6 IP matching `fd00:42:`
  (k3s dual-stack pod CIDR from v0.1.70). Failure mode separates
  the two causes cleanly: missing v4 → "k3s --cluster-cidr likely
  missing" (1.2 AUDIT regressed); missing v6 → "Cilium ipv6.enabled
  likely false" (1.4 AUDIT regressed). Closes the 5th deferred
  checkbox from 1.2 AUDIT (pod-level dual-stack reachability).

### Tests

- `cli_providers::k8s::cilium_values::tests::dual_stack_enabled_per_adr_0017`
  — pins both `ipv4:` and `ipv6:` blocks present in the rendered
  values; asserts at least 2 `enabled: true` lines so flipping one
  back to `false` would fail. Doesn't pin order so values can be
  reorganised for readability later.

### Deferred to Phase 3.x

- **In-pod IPv6 outbound curl** (e.g., `curl -6 https://ipv6.google.com/`
  from a workload pod). The current Phase 6 pod image
  `nginxdemos/hello:plain-text` doesn't include curl; standing up a
  separate `curlimages/curl:latest --ipv6` pod just for the assertion
  duplicates work that fits naturally inside Hubble/observability
  work in 3.7a (network metrics with v6 visibility). The pod-IP
  assertion in Phase 6.4 already proves Cilium allocates the v6
  interface; outbound v6 verification reuses the same datapath.

### Operator note

If you're upgrading a Tier 1 cluster that was bootstrapped with
v0.1.68 or earlier, re-running `apprafter cluster-bootstrap` will
re-apply the Cilium Helm release with the new values. `helm` patches
the `cilium-config` ConfigMap, but **the cilium DaemonSet pod
template does not change** (Cilium chart v1.16.x has no
`checksum/config` annotation that would tie ConfigMap changes to a
pod rotation). Result: cilium-agent pods keep running with the old
v4-only IPAM until explicitly rolled. Two-step manual fix:

```
kubectl rollout restart daemonset cilium -n kube-system
# wait for new agents to be Ready, then recreate any existing pods:
kubectl rollout restart deployment <name> -n <ns>
# or for plain pods: kubectl delete pod <name> && kubectl run <name> ...
```

Fresh installs are unaffected — agents come up with `ipv6.enabled:
true` from the start, and the first pod scheduled receives both
v4 and v6.

**Why we ship it manual for now:** the cleanest fix is to add
`kubectl rollout restart daemonset cilium` inside `cluster-bootstrap`
after `helm upgrade cilium`, but that adds ~30s of overhead to every
re-bootstrap run. M1.5 Track B 1.70 (`cluster-bootstrap` rewrite to
Argo CD-managed platform stack) replaces the imperative helm flow
entirely — Argo CD's resource hooks handle ConfigMap-triggered
restarts natively, so the wart self-resolves at that point without
us shipping disposable code. Tracked in plan.md 1.4 AUDIT closure
section under "Known wart".

## v0.1.70 — 1.2 AUDIT — Hetzner Cloud provider dual-stack IPv6 (2026-05-14)

First slice of the Phase 1 AUDIT backlog (gap-fill items inserted
into closed sub-phases as the project audited each layer against
ADR 0017 "dual-stack everywhere"). 1.2 AUDIT covers the Hetzner
provider half (this release); 1.4 AUDIT covers the Cilium values
half in the follow-up release.

### Added

- **`PublicIpv6` wire-type + `PublicNet.ipv6` field** in
  `cli-providers::hetzner_cloud::types`. Hetzner delegates a /64
  IPv6 prefix per cloud server (free); the API returns it as
  `public_net.ipv6.ip = "<prefix>::/64"`. The wire-type now parses
  it instead of dropping it silently. Re-exported through
  `cli_providers::hetzner_cloud::PublicIpv6`.
- **`K3sBootstrapOptions.dual_stack: bool` + dual-stack CIDR
  constants** in `cli-providers::hetzner_cloud::user_data`.
  `build_k3s_user_data` now appends
  `--cluster-cidr=10.42.0.0/16,fd00:42::/64
  --service-cidr=10.43.0.0/16,fd00:43::/112` to the k3s install
  line by default — ADR 0017 §Pod network requires both family
  declarations to be present at install time (k3s pins single-stack
  on first boot if the flags are absent and the conversion is
  destructive). `dual_stack: false` opt-out is plumbed through for
  the eventual `Infrastructure.network.ipFamilies` knob; default
  stays `true` to honour the platform-wide dual-stack posture.
  Constants `CLUSTER_CIDR_DUAL_STACK` / `SERVICE_CIDR_DUAL_STACK`
  are exported for callers that need to reference them
  (cluster-bootstrap diagnostics, manifest validators).
- **ICMP allow-rule in default Hetzner Firewall ingress**
  (`commands::apply::default_ingress_rules`). `direction: in,
  protocol: "icmp", port: None, source_ips: ["0.0.0.0/0", "::/0"]`.
  Hetzner Cloud Firewall does NOT distinguish ICMPv4 from ICMPv6 —
  a single `icmp` protocol rule covers both. Required by ADR 0017
  §Per-tier for Path MTU Discovery (ICMPv4 "fragmentation needed"
  + ICMPv6 "Packet Too Big") and IPv6 Neighbour Discovery (NDP/RA);
  without it dual-stack workloads break in subtle ways (silent
  packet drops at the MTU boundary).

### Tests

- `cli_providers::hetzner_cloud::types_test::server_decodes_dual_stack_public_net_with_ipv6_prefix`
  — pins serde shape for combined v4 + v6 `public_net`.
- `cli_providers::hetzner_cloud::types_test::server_decodes_public_net_with_only_ipv6_when_ipv4_absent`
  — forward-compat guard for hypothetical IPv6-only server types.
- `cli_providers::hetzner_cloud::user_data::tests::default_options_install_dual_stack_per_adr_0017`
  — default k3s install line carries both CIDR flags.
- `cli_providers::hetzner_cloud::user_data::tests::single_stack_flag_drops_dual_stack_cidr_args`
  — `dual_stack: false` opt-out drops both CIDRs while keeping the
  five disable-flags intact.
- `apprafter::commands::apply::tests::default_ingress_rules_emits_one_rule_per_default_port_plus_icmp`
  — rule count = TCP ports + UDP ports + 1 ICMP.
- `apprafter::commands::apply::tests::default_ingress_rules_include_icmp_for_pmtu_and_ndp`
  — ICMP rule shape: `direction=in`, `port=None`,
  `source_ips=["0.0.0.0/0", "::/0"]`.

### Deferred to dependent sub-phases

- **`--node-ip` dual-binding** stays out of the install line for
  v0.1.70. Adding it requires runtime-detected host IPv4 + IPv6
  substitution at cloud-init time (multi-line bash in `runcmd`),
  and in Tier 1 single-node k3s auto-detects acceptably. Will land
  alongside 3.1 (HA bootstrap) where heterogeneous-nodes
  scenarios make explicit node-IP selection critical.
- **Pod-level dual-stack reachability e2e** is wired to 1.4 AUDIT
  (Cilium Helm values dual-stack). Without `ipv4.enabled` +
  `ipv6.enabled` on Cilium, pods won't get a v6 interface even if
  k3s has the dual-stack CIDRs. After 1.4 AUDIT we extend
  `e2e/mvp.sh` with a `kubectl exec` curl asserting v6 outbound
  to a dual-stack endpoint.

## v0.1.69 — M1.5 Track A.1 — rename `platform-cli` → `apprafter` + shim (2026-05-14)

First slice of M1.5 Track A (CLI DX rework per `cli-dx-task.md` §17 row 1).
Renames the user-facing CLI binary from the legacy `platform-cli` to the
canonical `apprafter`, with a one-cycle deprecation shim so existing
invocations don't break overnight. Foundation for the upcoming target
store + `bootstrap-all` orchestrator in subsequent Track A slices.

### Changed

- **Cargo package + binary renamed** —
  `cli/platform-cli/Cargo.toml` now declares package `apprafter` with
  `[[bin]] name = "apprafter"` as the canonical entry point. Workspace
  layout, dir names, and existing `cli-core` / `cli-providers` /
  `cli-state` crates are untouched — only the user-facing binary name
  flips. `cargo install --path cli/platform-cli` installs `apprafter`
  on `$PATH` instead of `platform-cli`.
- **clap App name flipped** — `apprafter --help` now shows `apprafter`
  in the usage line instead of `platform-cli`, matching the new binary
  name. Subcommands and flags are unchanged.
- **Default `tracing` filter retargeted** — `cli-core::logging::init`
  default `EnvFilter` now lists `apprafter=info` instead of
  `platform_cli=info`. Crate-renamed log levels keep firing at
  `INFO` without users needing to set `RUST_LOG`.
- **User-facing error hints retargeted** — strings like
  `state has no provider — run \`platform-cli init …\` first` now
  reference `apprafter init …`. Internal `cli-core` / `cli-providers`
  / `cli-state` doc-comments also retargeted to the new binary name
  to keep grep-discoverability consistent.

### Added

- **`platform-cli` deprecation shim** —
  `cli/platform-cli/src/bin/platform-cli.rs` is a second binary
  built from the same package. It prints a 3-line warning to stderr
  ("`platform-cli` has been renamed to `apprafter`. Run `apprafter
  <command>` instead. This shim will be removed in v0.2.0.") and
  then spawns the `apprafter` binary located in the same directory
  with the same argv, forwarding stdin/stdout/stderr and the exit
  code. Cross-platform (Unix + Windows `.exe` resolution).
- **Shim regression-guard test** —
  `cli_smoke::platform_cli_shim_warns_and_forwards` runs the shim
  via `assert_cmd::cargo_bin("platform-cli")`, asserts the deprecation
  banner reaches stderr, the forwarded `plan` output reaches stdout
  untouched, and the shim's exit code matches `apprafter plan`. Pins
  both halves of the deprecation contract.

### Docs

- **User-visible CLI references swept** across `README.md`,
  `cli/README.md`, `e2e/README.md`, `e2e/mvp.sh`,
  `operator/README.md`, `operator/charts/apprafter-operator/README.md`,
  `backstage-plugins/host/{README.md,scripts/scaffold.sh}`,
  `manifests/README.md`, `manifests/tier-1/{admission-webhook,application,backstage}/README.md`,
  `examples/templates/bun-http/{README.md,skeleton/README.md,skeleton/apprafter/Application.cue}`,
  `schemas/v1alpha1/{infrastructure,infrastructureproviderplugin}.cue`,
  `docs/architecture/`, `docs/dev-guide/`, `docs/operator-guide/`,
  `docs/reference/`, `.github/ISSUE_TEMPLATE/bug.yml`, `SECURITY.md`,
  `.gitignore`, `plan.md`, `spec.md`. The deprecated `platform-cli`
  string survives only in (1) `cli/platform-cli/Cargo.toml`'s second
  `[[bin]]` entry, (2) the shim source + its test, (3) ADRs (`docs/adr/`),
  (4) historical TDD plans (`docs/superpowers/plans/`), and (5) past
  changelog entries (v0.1.x ≤ 68) — each describing system state at
  the time of the decision.
- **`cli-dx-task.md` §17 row 1 marked actionable**; subsequent rows
  unlock Track A.2 (`target` file structure) onward.

### Backwards compatibility

- Existing scripts that invoke `platform-cli <command>` continue to
  work via the shim. The shim prints a deprecation warning on every
  call. Removal is scheduled for `v0.2.0-self-managing` (M1.5
  closure) — script owners have the full Track A + Track B cycle
  to migrate.
- `HCLOUD_TOKEN` + `APPRAFTER_SSH_PUBLIC_KEY` env-var workflow
  unchanged; Track A.2+ adds the persistent target store as an
  additive layer (resolution chain stays env-friendly).

## v0.1.68 — Phase 1 patch — repo-creds Secret SSA (security) (2026-05-13)

Walk-found security bug from §1.15 Q3 — exposed by manually inspecting
`kubectl get secret apprafter-bootstrap-repo-creds -n argocd -o yaml`:
the `kubectl apply` we used for the Secret stored the raw `stringData`
(including the `password` PAT) in the
`kubectl.kubernetes.io/last-applied-configuration` annotation as plain
text. Anyone with read access to the Secret (or to etcd backups, or to
Argo CD's "Manifest" tab in the UI) could recover the operator's GitHub
or GitLab PAT without even base64-decoding the `data` field.

### Fixed

- **Repo-creds Secret apply switched to server-side apply (SSA)** —
  new `KubectlRunner::apply_manifest_server_side(source, kubeconfig,
  field_manager)` trait method shells out to
  `kubectl apply --server-side --field-manager=apprafter-cli
  --force-conflicts -f …`. SSA tracks ownership in
  `metadata.managedFields` instead of writing the entire manifest
  body to `last-applied-configuration`, so the PAT no longer leaks
  into annotations. `--force-conflicts` lets the migration from
  client-side ownership succeed cleanly on existing clusters
  (operator re-runs `cluster-bootstrap` after upgrading).
  `APPRAFTER_CLI_FIELD_MANAGER = "apprafter-cli"` mirrors the
  operator's `apprafter-operator` field-manager identity so
  cluster-side ownership is legible from `managedFields`.

### Tests

- `k8s::kubectl::tests::apply_server_side_command_passes_field_manager_and_force_conflicts`
  — the build helper emits `--server-side`,
  `--field-manager=apprafter-cli`, `--force-conflicts`, and the
  manifest path; defensive assertion that no `--client-side` flag is
  present.
- `k8s::kubectl::tests::apply_server_side_command_with_url_source_still_emits_ssa_flags`
  — URL-sourced manifests still get full SSA flag set.
- `bootstrap_repo_with_token_creates_repo_secret_before_bootstrap_app`
  rewritten — repo-secret now lands in `ssa_applies` (with
  field-manager `apprafter-cli`), the bootstrap App stays in
  client-side `applies`. Assertion structure proves the SSA call
  ran AND the field-manager is correct.
- `bootstrap_repo_without_token_skips_secret_but_keeps_bootstrap_app`
  + `no_bootstrap_repo_skips_both_secret_and_app` updated — both
  assert `ssa_applies.is_empty()` for paths where no Secret is
  created.

### Backlog (post-§1.15 walks)

- **`helm upgrade --install` bumps REVISION on no-op re-bootstrap**
  (#fix-walk-4, not in v0.1.68). Every `cluster-bootstrap` re-run
  rev-bumps cilium / argocd / cert-manager / apprafter-operator
  even when values are byte-identical, polluting helm history and
  potentially triggering pod rotations on otherwise idempotent
  re-runs. Helm 3 has no native skip-on-empty-diff; fix requires
  either bundling the helm-diff plugin or writing our own pre-call
  values-comparison. Deferred.

## v0.1.67 — Phase 1 patch — §1.15 walks destroy follow-up (2026-05-13)

Two follow-up bugs from v0.1.66's destroy reorder, both surfaced
when the fix ran against production Hetzner (mockito didn't model
the exact upstream behaviour):

### Fixed

- **Idempotent unassign matched the wrong error code**
  (#fix-walk-2b). v0.1.66 treated `422 code=floating_ip_not_assigned`
  as "already detached, proceed to DELETE". Production Hetzner
  actually returns `422 code=service_error` + message "Floating IP
  with ID X is not assigned to any resource" for the same condition
  — the documented code never appears in practice. Fix:
  `unassign_floating_ip` now ALSO accepts `status=422` + message
  containing `is not assigned` as idempotent success. The docs-
  consistent `floating_ip_not_assigned` code stays in the matcher
  for forward-compat.

- **`DELETE /floating_ips/{id}` raced with Hetzner's async detach**
  (#fix-walk-2c). After `wait_for_server_gone` returns,
  Hetzner's scheduler keeps the FIP `423 locked` for a few more
  seconds while it tears down the server→FIP association. v0.1.66
  did a single `DELETE` and surfaced the lock as a destroy
  failure. Fix: generalised the existing retry helper
  `delete_with_retry_on_resource_in_use` →
  `delete_with_retry_on_transient_lock`, now retrying on EITHER
  `code=resource_in_use` (firewall/network — pre-existing) OR
  `status=423` (floating IP — new). 60 s deadline, 500 ms → 5 s
  exponential back-off (unchanged). `delete_floating_ip` switched
  to the helper.

### Tests

- `destroy_treats_service_error_is_not_assigned_as_idempotent_success`
  — exact production unassign-422 payload.
- `destroy_floating_ip_retries_on_423_locked_until_cleared` —
  sequenced mockito: 423 first call, 204 second, retry kicks in.

## v0.1.66 — Phase 1 patch — §1.15 walks bug fixes (2026-05-13)

Bugs surfaced during the §1.15 manual 4-quadrant walks (per
`feedback_phase_closure_validation` discipline — every walk-found
bug ships as a patch with a regression-guard test before the
next phase opens).

### Fixed

- **`examples/infrastructure/tier-1-hetzner.cue` — k3s apiserver
  port 6443 was missing from `firewall.ingress`** (#fix-walk-1).
  Hetzner Cloud Firewall default-deny dropped TCP packets from
  the operator's workstation to `:6443`, causing the first
  `helm upgrade --install cilium` of `cluster-bootstrap` to fail
  with `Kubernetes cluster unreachable ... i/o timeout`. Hidden
  during §1.14 walks because the rule was added manually in the
  Hetzner web UI for debug and never committed. Fix: the example
  manifest now opens `:6443` to `0.0.0.0/0` + `::/0` alongside
  `:22` and `:443`, with an inline comment that tier-1 keeps the
  apiserver public and higher tiers will restrict.
- **`destroy` rejected with `422 must_be_unassigned` when a
  Floating IP was still attached to the server** (#fix-walk-2).
  The pre-fix destroy order deleted floating IPs FIRST, but
  Hetzner's `DELETE /floating_ips/{id}` rejects assigned IPs —
  so the first attached-FIP destroy aborted the whole flow,
  leaving the cluster + firewall + network behind. Fix is
  two-layer:
  - **Reorder**: `destroy()` now deletes the server first
    (auto-detaches its FIPs), waits via `wait_for_server_gone`,
    then deletes the now-unassigned FIPs.
  - **Defensive `unassign_floating_ip` step**: `delete_floating_ip`
    calls a new client helper before `DELETE`, treating both
    `422 floating_ip_not_assigned` and `404` as idempotent
    success. Belt-and-suspenders against the race between
    `wait_for_server_gone` returning and Hetzner's internal
    scheduler fully detaching the FIP.

### Tests

- `destroy_removes_server_before_floating_ip_and_unassigns_first`
  (renamed + rewritten — pre-fix
  `destroy_removes_floating_ip_first_then_others` baked in the
  buggy order and passed by coincidence: mockito's `expect(1)`
  on each `DELETE` mock didn't enforce cross-endpoint order).
- `destroy_treats_floating_ip_not_assigned_as_idempotent_success`
  (new) — exercises the Hetzner `422 floating_ip_not_assigned`
  response on `POST /actions/unassign` and asserts destroy
  proceeds to `DELETE`.

### Backlog (post-§1.15 walks)

- **`apply` does not reconcile firewall rules on existing
  firewall** (#fix-walk-3, not in v0.1.66). After editing
  `Infrastructure.cue` to add a new ingress rule and re-running
  `apply`, the command returns `0 actions` because state already
  has a `firewall_id`. Workaround for §1.15: `destroy` + fresh
  `apply` (clean-path discipline). Real fix lands separately.

## v0.1.65 — Phase 1 patch — §1.15 Level C GitOps integration (2026-05-11)

### Added

- **`APPRAFTER_ARGOCD_REPO_TOKEN` env-var → auto-provisioned Argo
  CD repo-credentials Secret** (§1.15) — when the env-var is set
  alongside `spec.argocd.bootstrapRepo`, `cluster-bootstrap` creates
  the `apprafter-bootstrap-repo-creds` Secret in the `argocd`
  namespace with the `argocd.argoproj.io/secret-type: repository`
  label. Argo CD discovers it automatically and scopes HTTPS basic-
  auth to the configured `bootstrapRepo`. Closes the gap where
  private GitHub/GitLab repos required a `kubectl apply` of a Secret
  by hand. The companion env-var `APPRAFTER_ARGOCD_REPO_USERNAME`
  overrides the default username (`apprafter`).
- **`cli-providers::k8s::argocd_repo_secret` builder** — pure
  `argocd_repo_secret_yaml(repo_url, username, token) -> String`
  with constants `APPRAFTER_BOOTSTRAP_REPO_CREDS_SECRET` +
  `ARGOCD_REPO_USERNAME_DEFAULT`. 4 unit tests cover label set
  (`apprafter=true` + `argocd.argoproj.io/secret-type: repository`),
  field propagation (url + username + password), and `type: Opaque`.
- **`docs/operator-guide/gitops-walk.md`** — 4-quadrant manual
  runbook (GitHub × GitLab × public × private) with prerequisites,
  per-quadrant step-by-step instructions, DoD checklists, and
  troubleshooting tables. Includes token-rotation + revoke sections.
- **`cluster_bootstrap.rs::read_argocd_repo_creds`** — testable
  helper that reads the two env-vars through an injected closure
  (no `std::env::set_var` in tests). 4 unit tests.

### Changed

- **`perform_bootstrap` signature grows one parameter** —
  `argocd_repo_secret_path: Option<&Path>` inserted between
  `admission_webhook_path` and `argocd_gateway_path` (bootstrap-
  order step 9.5, between webhook and Argo CD HTTPRoute). The
  Secret is applied before the bootstrap `Application` so Argo
  CD's first reconcile sees credentials. All 8 pre-existing
  orchestration tests updated for the new arg position.
- **`ClusterSettings` gains `argocd_repo_creds: Option<(String, String)>`**
  — resolved at `cluster-bootstrap` time from env-vars. `None`
  means public-repo path (no Secret created).

### Tests

- `argocd_repo_secret::tests` — 4 unit tests for the YAML builder.
- `cluster_bootstrap::tests` — 4 new helper unit tests
  (`read_argocd_repo_creds_*`) + 3 new orchestration tests
  (`bootstrap_repo_with_token_creates_repo_secret_before_bootstrap_app`,
  `bootstrap_repo_without_token_skips_secret_but_keeps_bootstrap_app`,
  `no_bootstrap_repo_skips_both_secret_and_app`).

### Docs

- `quickstart.md` §3 gains the new env-var bullet pointing at the
  walk runbook.
- `plan.md` §1.15 entry with 10 deliverable checkboxes.

## v0.1.64 — Phase 1 patch — §1.14 Level B integration (2026-05-11)

### Added

- **`spec.operator` block + default-on operator install** (§1.14)
  — `platform-cli cluster-bootstrap` now installs the AppRafter
  operator helm release in `apprafter-system` by default, using
  the `ghcr.io/apprafter/apprafter-operator:v0.1.64` image
  published by `release-operator.yml`. Manifest opt-out via
  `spec.operator.enabled: false`; fork builds override via
  `spec.operator.image` and/or `spec.operator.tag`. The
  `RELEASED_OPERATOR_VERSION = "v0.1.64"` constant in
  `cli-providers::k8s::image_ref` is the single source of truth
  for both operator and admission-webhook image tags (paired
  release from the same workflow run).
- **`resolve_image_ref` helper** with variant-C override semantics
  — `image` with `:` is a full ref (tag-field ignored); without
  `:` it's a bare repo composed with the explicit-or-default tag.
  Six unit tests cover every row of the design's override-table.
- **Embedded operator helm chart** — the `include_dir!`-bundled
  `operator/charts/apprafter-operator/` extracts to a tempdir at
  `cluster-bootstrap` time, then `helm upgrade --install` runs
  against the local-path chart. Keeps `platform-cli` installable
  as a single binary (no out-of-repo dependency).
- **e2e/mvp.sh Phase 6.5** — between the endpoint-verify and
  destroy phases the harness now applies
  `manifests/tier-1/application/example-app.yaml`, polls
  `.status.phase` until `Ready` (60s deadline), and asserts the
  operator-rendered child Deployment is `Available`. Nightly CI
  thus exercises the operator reconcile path end-to-end.

### Changed

- **`spec.admissionWebhook` semantics flip — default-on**
  (one-time backward-incompatible change). In v0.1.63 and earlier
  the absence of `spec.admissionWebhook.image` meant "do not
  install"; from v0.1.64 onwards the absence of the whole block
  or any of its fields means "install with the released image".
  The block gains `enabled?: bool` (opt-out) and `tag?: string`
  (override only the tag) to mirror the `spec.operator` shape.
  The pre-v0.1.64 `image: "..."` shape continues to parse without
  errors. Documented in the `## v0.1.64` block above and in
  `docs/operator-guide/quickstart.md`.
- **`HelmUpgradeArgs.version: String` → `Option<String>`** — lets
  the new operator install reference the embedded chart by local
  path (version comes from `Chart.yaml`, not `--version`). Cilium
  / Argo CD / cert-manager continue to pass `Some(<pinned>)`.

### Tests

- `image_ref::tests` — 6 unit tests covering every row of the
  override-semantics table.
- `operator_values::tests` — 3 unit tests asserting the values
  YAML overrides only image fields (everything else stays at
  chart defaults).
- `operator_chart::tests::embedded_chart_contains_chart_yaml_and_templates`
  — runtime extraction smoke test verifying the embedded chart is
  intact.
- `cli-core::manifest::tests` — 4 new tests covering
  `OperatorBlock` + extended `AdmissionWebhookBlock` parsing
  (forward + backward shapes).
- `cluster_bootstrap::tests` — 5 new tests (3 from §1.14 task 7,
  2 from task 8) covering default install order, opt-out
  semantics, and image-override propagation.
- `cli-providers::k8s::helm::tests::upgrade_install_command_omits_version_flag_when_none`
  — regression-guard for the `Option<String>` refactor.

## v0.1.63 — Phase 1 patch (2026-05-11)

### Fixed

- **Operator hot-reconcile loop: preserve `lastTransitionTime` on
  same-status updates** (v0.1.63) — after the v0.1.62 CRD fix
  let the operator reach `phase: Ready`, controller logs showed
  ~10 reconciles per 100ms indefinitely. Each reconcile set
  `last_transition_time: Utc::now()` regardless of whether the
  Ready condition's `status` had actually changed, so the
  status-subresource patch produced a diff on every cycle, the
  apiserver fired a watch event on the operator's own write,
  and the controller re-reconciled — closing the loop.
  Per `meta/v1.Condition` semantics, `lastTransitionTime` moves
  ONLY when `status` transitions (False ↔ True). Fix:
  `ready_condition` now takes the previous condition slice; if
  a Ready condition with the same `status` already exists, its
  timestamp is reused. An idle Ready Application now produces
  identical status objects on the 60s safety-resync → SSA patch
  is a no-op → no watch event → no re-reconcile. Two new
  regression-guard tests cover both directions (preserve on
  same status, bump on actual transition). The 60s requeue at
  reconcile-bottom stays — that is the defensive resync floor,
  not the primary trigger; watch-event-driven reconcile remains
  the primary path.

## v0.1.62 — Phase 1 patch (2026-05-11)

### Fixed

- **CRD declares `status` subschema (was empty)** (v0.1.62) —
  operator's first reconcile against a real cluster failed on
  every status-subresource PATCH:

  ```
  failed to create typed patch object (apprafter.io/v1alpha1,
  Kind=Application): .status: field not declared in schema
  ```

  The CRD had `subresources.status: {}` (enables the /status
  endpoint) but `openAPIV3Schema.properties` declared only
  `apiVersion / kind / metadata / spec` — no `status` block at
  all. Apiserver's structural-schema enforcement rejects any
  `.status.*` PATCH because no schema describes what's allowed
  there. The operator's child Deployment + Service WERE created
  successfully (SSA on those paths doesn't traverse the CRD's
  structural validation for the application itself), but every
  `Application.status.phase = Ready` write died with 500.
  Hidden through v0.1.22 → v0.1.61 because the CRD schema
  (sub-phase 1.7b) and the operator's status-write path
  (sub-phase 1.9b) were developed independently and never
  exercised together end-to-end until the operator pod finally
  reached `leader acquired — starting Application controller`
  on 2026-05-11. Fix: add a `status` property to the
  `openAPIV3Schema` mirroring `operator_core::ApplicationStatus`
  (`phase` / `observedGeneration` / `endpointURL` / `conditions`
  array of `ApplicationCondition`). Static rendering
  `manifests/tier-1/application/example-crd.yaml` regenerated.
  New regression-guard test
  `status_subschema_declares_every_field_the_operator_writes`
  fails if `operator-core` grows an ApplicationStatus field that
  the CRD schema doesn't mirror — drift caught locally before CI
  next time.

## v0.1.61 — Phase 1 patch (2026-05-11)

### Fixed

- **Operator pod CrashLoopBackOff: install rustls CryptoProvider
  before TLS** (v0.1.61) — first ever `helm install
  apprafter-operator` produced CrashLoopBackOff. Logs:

  ```
  thread 'main' panicked at
    rustls-0.23.40/src/crypto/mod.rs:249:14:
    Could not automatically determine the process-level
    CryptoProvider from Rustls crate features.
  ```

  rustls 0.23+ removed its auto-default provider; kube 0.95's
  `rustls-tls` feature pulls `aws-lc-rs` into the dep graph but
  does not install it. `Client::try_default()` deep inside kube
  panics if no provider is set at process start. Hidden through
  v0.1.26 → v0.1.60 because nobody ran the operator binary
  against a real cluster (in-file unit tests use no kube
  client; axum tests use `tower::ServiceExt::oneshot`;
  `Client::try_default` only fires when there is a real
  apiserver to talk to). Fix: new
  `apprafter_operator::install_rustls_crypto_provider` helper
  — idempotent, called from `main()` BEFORE
  `Client::try_default`. Direct rustls dep with `aws-lc-rs`
  feature added to `apprafter-operator/Cargo.toml`. Two
  regression-guard unit tests assert: (a) after the helper
  runs `CryptoProvider::get_default()` returns `Some`; (b) the
  helper is idempotent (second call is a no-op, not a panic)
  so test processes that exercise `main()` more than once don't
  crash. `operator/Cargo.lock` now committed (previously
  untracked; standard practice for binaries — same as
  `cli/Cargo.lock`).

## v0.1.60 — Phase 1 patch (2026-05-11)

### Fixed

- **Helm template whitespace-trim ate `apiVersion:` line**
  (v0.1.60) — `helm install apprafter-operator` failed
  immediately with

  ```
  Error: INSTALLATION FAILED: unable to build kubernetes
  objects from release manifest: error validating data:
  apiVersion not set
  ```

  Because `serviceaccount.yaml` and `rbac.yaml` opened with
  `# SPDX-License-Identifier: MIT\n{{- if X.create -}}\napiVersion:
  …`, the trailing `-}}` stripped the newline before
  `apiVersion:`, rendering as
  `# SPDX-License-Identifier: MITapiVersion: v1` — YAML
  swallowed the apiVersion into the `#` comment, doc parsed
  with no apiVersion. Hidden through v0.1.29 → v0.1.59 because
  `helm install` against this chart was never walked
  end-to-end: the operator container image wasn't publishable
  until v0.1.52, and §5 of operator-quickstart wasn't run on a
  real cluster until today. Fix: drop the trailing `-` from
  `{{- if X.create -}}` → `{{- if X.create }}` in both
  affected templates. Leading-trim still eats the SPDX-comment's
  trailing newline; the trailing newline before the body's
  `apiVersion:` is preserved. `helm template` output now has
  correctly-separated comment + apiVersion lines; `helm lint`
  clean (only the cosmetic `icon is recommended` info).

## v0.1.59 — Phase 1 patch (2026-05-10)

### Fixed

- **`rust:alpine`, not `rust:stable-alpine` (the latter doesn't
  exist)** (v0.1.59) — the v0.1.58 floating Dockerfile pin was
  wrong: Docker Hub does not publish a `rust:stable-alpine` tag.
  Build failed with `docker.io/library/rust:stable-alpine: not
  found`. The `stable-` prefix is a node.js convention
  (`node:lts-alpine` etc.), not rust's. Fix: use `rust:alpine`,
  the canonical Docker Hub tag for "latest stable on alpine".
  Same forward-compat as v0.1.58 intended — image auto-refreshes
  with each rust release.

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
or MIT (plugins / templates) per ADR 0001. (M1 shipped before
the ADR 0032 base-license migration; subsequent releases use
FSL-1.1-Apache-2.0 — see ADR 0032.)

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
- **Licensing** — `LICENSE` (FSL-1.1-MIT at M0; canonical text from
  fsl.software; later migrated to FSL-1.1-Apache-2.0 per ADR 0032),
  `LICENSE-MIT`, `NOTICE` explaining the 2-year FSL conversion model,
  plugin-level MIT `LICENSE` files in `providers/` and
  `backstage-plugins/`, SPDX-header conventions in
  `docs/contributing/license-headers.md`.
- **12 ADRs** + Nygard-style template covering: FSL-1.1-MIT for
  core (the base-license choice was later migrated to
  FSL-1.1-Apache-2.0 via ADR 0032), codename "AppRafter", custom
  Rust operator vs Crossplane,
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
  see ADR 0001 — subsequently migrated to FSL-1.1-Apache-2.0 base
  via ADR 0032).

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
