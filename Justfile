# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Top-level command runner. Entry point for contributors.
# Run `just --list` for an overview.

set shell := ["bash", "-cu"]

# Default: show available targets.
default:
    @just --list

# Install local Git hooks via lefthook. Idempotent.
bootstrap:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v lefthook >/dev/null 2>&1; then
        lefthook install
        echo "==> lefthook hooks installed"
    else
        echo "lefthook not on PATH; skipping git-hooks install."
        echo "Install: nix profile install nixpkgs#lefthook"
    fi

# Lint everything: CUE, SPDX, conditionally Rust and TS.
lint:
    #!/usr/bin/env bash
    set -euo pipefail
    ./scripts/lint-cue.sh
    ./scripts/check-spdx-headers.sh
    ./scripts/check-no-cyrillic.sh
    ./scripts/docs-check.sh
    ./scripts/check-crd-structural.sh
    ./scripts/check-operator-version-bump.sh
    ./scripts/check-cli-version-bump.sh
    # cli/ and operator/ are SEPARATE Cargo workspaces (no top-level
    # Cargo.toml), so cargo must run from inside each — matching CI
    # (.github/workflows/lint.yml runs fmt+clippy per workspace). The
    # `-f` guard keeps this safe from a fresh root before subworkspaces exist.
    for ws in cli operator; do
        if [ -f "$ws/Cargo.toml" ]; then
            echo "==> rustfmt + clippy ($ws)"
            ( cd "$ws" && cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings )
        else
            echo "==> no $ws/Cargo.toml — skipping rustfmt/clippy"
        fi
    done
    # Per-package JS lint (landing/*, backstage-plugins/*) is owned by CI
    # (.github/workflows/lint.yml iterates each package with its own
    # bun install). `just lint` only runs a root lint script if one exists;
    # there is no root package.json, so this skips cleanly rather than
    # erroring with "Script not found lint" from the repo root.
    if [ -f package.json ] && grep -q '"lint"' package.json 2>/dev/null; then
        bun run lint
    else
        echo "==> no root package.json lint script — per-package bun lint runs in CI"
    fi

# Format all code (in-place).
fmt:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v cue >/dev/null 2>&1; then
        cue fmt ./schemas/... ./examples/...
    else
        nix run nixpkgs#cue -- fmt ./schemas/... ./examples/...
    fi
    # Per-workspace (no top-level Cargo.toml) — cargo must run from inside each.
    for ws in cli operator; do
        if [ -f "$ws/Cargo.toml" ]; then ( cd "$ws" && cargo fmt --all ); fi
    done

# Run all tests, conditional on workspace presence.
test:
    #!/usr/bin/env bash
    set -euo pipefail
    # Per-workspace (no top-level Cargo.toml) — cargo must run from inside each.
    for ws in cli operator; do
        if [ -f "$ws/Cargo.toml" ]; then
            echo "==> cargo test ($ws)"
            ( cd "$ws" && cargo test --all --all-features )
        fi
    done
    if find . -name package.json -not -path '*/node_modules/*' | head -1 | grep -q .; then
        bun test
    else
        echo "==> no package.json — skipping bun test"
    fi

# Generate the operator chart CRDs from the v1alpha1 CUE schemas (ADR 0047).
# CUE is the single source of truth; the crd-*.yaml files are GENERATED and
# committed. Runs under `nix develop` so the cue version is flake.lock-pinned
# (the drift gate compares byte-for-byte). Run after any schema change.
gen-crds:
    nix develop --command bash -c 'cd operator && cargo run -q -p crdgen -- generate'

# CRD drift gate (ADR 0047): assert the committed crd-*.yaml match what
# crdgen generates from CUE. Local-first — the lefthook pre-commit hook and
# the crd-check CI workflow run the same script.
crd-check:
    bash scripts/crd-check.sh

# Validate every CRD against a REAL apiserver (ephemeral kind cluster).
# `helm lint` does NOT catch CRD structural-schema errors — only the
# apiserver does — so run this before any CRD-changing release. Fast
# (~30s) focused CRD gate; `just e2e` is the comprehensive backstop.
# Requires kind + a working docker/podman.
crd-validate:
    bash scripts/validate-crds.sh

# Run the GitOps-walk k3d e2e (CMP → Argo CD → operator loop).
# Requires a running Docker daemon and k3d on PATH.
e2e-gitops:
    bash e2e/gitops-walk.sh

# Run the local k3d e2e gate (currently just gitops-walk).
# NOTE: e2e/mvp.sh (real Hetzner provision smoke, costs money) is
# intentionally excluded here — it runs via the nightly CI workflow
# only (.github/workflows/nightly.yml, 04:00 UTC).
#
# The platform-migration walk is NOT part of the k3d gate: the
# PlatformController's OCI client is HTTPS-only (oci-distribution
# ClientConfig::default()), while k3d's local registry is plain
# HTTP, so the controller cannot pull fixture compat-docs in-cluster.
# The MigrationPlan gate is covered by operator unit + integration
# tests; a real-infra (HTTPS-registry) migration walk is a tracked
# follow-up for the nightly Hetzner harness. See plan.md §1.81.
e2e: e2e-gitops

# Run the needs.pg ResourceClaim-chain k3d/kind walk (generate -> schedule
# -> provision -> resume + explicit DSN ref -> delete + RetainedClaim ->
# force-GC -> psql DROP proof). Since 2.12 (ADR 0046) the app binds its DSN by
# an explicit `env: {DATABASE_URL: {claim: "pg.url"}}` ref (the 2.4e auto-inject
# + composed connection-Secret key are removed), so while 2.12 is UNRELEASED
# this RUNS WITH APPRAFTER_E2E_LOCAL_OPERATOR=1: it builds + side-loads the
# working-tree operator + admission-webhook and applies the branch CRDs (the
# published image/CRD predate the `env` value node). Deliberately NOT a
# dependency of `e2e`: it boots a Postgres pod (heavier) and runs on its own
# nightly cadence via .github/workflows/e2e-pg-nightly.yml. Requires a container
# runtime (docker->k3d / podman->kind), cargo, kubectl.
e2e-pg:
    APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/needs-pg-walk.sh

# Run the needs.redis ResourceClaim-chain k3d walk (generate -> schedule
# -> provision -> resume + DSN -> $N-ACL isolation proof -> delete +
# RetainedClaim -> force-GC -> FLUSHDB/DELUSER proof). Like e2e-pg it is
# NOT a dependency of `e2e`: it boots a Dragonfly pod and runs on its own
# nightly cadence via .github/workflows/e2e-redis-nightly.yml. Requires
# Docker + k3d on PATH.
e2e-redis:
    bash e2e/needs-redis-walk.sh

# Run the needs.disk ResourceClaim-chain k3d/kind walk (generate ->
# schedule -> provision (unowned RWO PVC) -> resume + mount -> data
# durability -> delete + RetainedClaim + reattach -> force-GC -> PVC
# dropped). Runs the PUBLISHED operator image (v0.2.21+ ships the 2.6b disk
# feature, CRDs, the disk-local seed, and the PVC RBAC), exactly like
# e2e-pg / e2e-redis. Set APPRAFTER_E2E_LOCAL_OPERATOR=1 to build +
# side-load the working-tree operator instead (pre-release validation of
# disk-code changes). Like e2e-pg/e2e-redis it is NOT a dependency of
# `e2e`: it boots a CNPG pod + provisions PVCs and runs on its own cadence.
# Requires a container runtime (docker→k3d / podman→kind), cargo, kubectl.
e2e-disk:
    bash e2e/needs-disk-walk.sh

# Run the needs-env-refs k3d/kind walk (2.12, ADR 0046): the operator
# resolves an `env` map carrying ALL THREE sources (literal + claim ref
# {claim:"pg.url"} → decomposed connection-Secret keys url/user/pass + external
# secret ref {secret:"appsecret/token"}) into container EnvVar/secretKeyRef,
# the pod sees the resolved values, and deleting the external Secret flips the
# Application to Ready=False/EnvSecretMissing (then recovers on re-create).
# 2.12 is UNRELEASED, so this RUNS WITH APPRAFTER_E2E_LOCAL_OPERATOR=1: it
# builds + side-loads the working-tree operator + admission-webhook and applies
# the branch CRDs (the published image/CRD predate the `env` value node). It
# takes the DIRECT-CR marker path (the cue-cmp bare-selector rendering is
# covered by argocd-cue-cmp/test-inject.sh). Like e2e-pg/e2e-redis/e2e-disk it
# is NOT a dependency of `e2e`: it boots a CNPG pod and runs on its own nightly
# cadence via .github/workflows/e2e-env-refs-nightly.yml. Requires a container
# runtime (docker→k3d / podman→kind), cargo, kubectl.
e2e-env-refs:
    APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/needs-env-refs-walk.sh

# Run the per-environment GitOps-walk k3d/kind e2e (2.9, ADR 0044): the
# SAME repo deployed per env (app add --env dev|prod) -> two Argo
# Applications web-dev + web-prod -> the argocd-cue-cmp sidecar injects
# spec.environment -> the operator env-resolves base+environments -> two
# self-contained Deployments in two namespaces (replicas 1 vs 2, TIER dev
# vs prod) -> app status aggregates both -> app remove --env is surgical.
# 2.9 is UNRELEASED, so this RUNS WITH APPRAFTER_E2E_LOCAL_OPERATOR=1: it
# builds + side-loads the working-tree operator + admission-webhook AND the
# working-tree cue-cmp (Approach A — the per-env injection lives only in the
# working-tree entrypoint.sh), and applies the branch CRDs. Like
# e2e-pg/e2e-redis/e2e-disk it is NOT a dependency of `e2e`: it boots Argo CD
# + a git daemon and runs on its own nightly cadence via
# .github/workflows/e2e-per-env-nightly.yml. Requires a container runtime
# (docker→k3d / podman→kind), git, cargo, kubectl.
e2e-per-env:
    APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/gitops-walk-per-env.sh

# Run the per-environment `expose` DEEP-MERGE k3d/kind e2e (2.16c): the
# SAME repo deployed per env, where the dev override carries ONLY the diff
# `expose:{network:"internal"}` -> the operator effective_spec DEEP-MERGES it
# onto base.expose so the dev Deployment/Service INHERIT the base port (8080)
# and, being internal, emit NO public HTTPRoute (the inherited hostname is
# inert), while base/prod keeps the public HTTPRoute on x.example.com. Also
# asserts H1: the STORED dev CR's env-override expose stays partial
# ({network:internal} only, no defaulted keys). 2.16c is UNRELEASED, so the
# walk FORCES APPRAFTER_E2E_LOCAL_OPERATOR=1 internally (builds + side-loads
# the working-tree operator + admission-webhook + cue-cmp, applies branch
# CRDs). Uses the DEFAULT CNI (no Cilium → no sandbox-run microVM needed).
# Like the other walk targets it is NOT a dependency of `e2e`; requires a
# container runtime (docker→k3d / podman→kind), git, cargo, kubectl.
e2e-expose-deep-merge:
    bash e2e/expose-deep-merge-walk.sh

# Run the needs.networkpolicy egress-enforcement kind+Cilium walk (2.10, ADR
# 0045): the operator derives one per-Application egress CiliumNetworkPolicy
# from declared needs -> a needs.pg app CAN reach pg (Hubble FORWARDED) while
# a needs-less app CANNOT (Hubble DROPPED) -> `apprafter platform egress set
# internal|strict` tightens the baseline (external blocked, then same-namespace
# blocked). UNLIKE the other walks this one needs REAL Cilium: it brings kind
# up with the default CNI + kube-proxy disabled (kind_up_cilium) and bootstraps
# WITH Cilium (bootstrap_with_cilium), then asserts `cilium status --wait`. The
# walk forces the kind runtime + builds the working-tree operator + webhook
# (2.10 UNRELEASED), so it needs cargo, kubectl, a container runtime, AND the
# Cilium + Hubble CLIs (nix develop ships cilium-cli; the lib.sh wrappers fall
# back to `nix run nixpkgs#…`). NOTE: a rootless-podman host with a capped
# memlock ulimit (8 MB) cannot run the cilium-agent — the full enforcement run
# needs a rootful runtime (CI) or a host memlock raise; the nightly is
# .github/workflows/e2e-networkpolicy-nightly.yml. Like the other walk targets
# it is NOT a dependency of `e2e`.
e2e-networkpolicy:
    bash e2e/needs-networkpolicy-walk.sh

# Spin up a local k3d cluster suitable for end-to-end work.
e2e-up:
    k3d cluster create apprafter-dev \
        --servers 1 --agents 0 \
        --port "8080:80@loadbalancer" \
        --port "8443:443@loadbalancer" \
        --k3s-arg "--disable=traefik@server:0" \
        --k3s-arg "--disable=servicelb@server:0"
    @echo "==> cluster ready. kubectl context: k3d-apprafter-dev"

# Tear down the local k3d cluster.
e2e-down:
    k3d cluster delete apprafter-dev

# Live docs preview. Always goes through the flake so the mkdocs
# version, theme and plugins are the flake.lock-pinned ones — the
# old `command -v mkdocs || nix shell …` fallback could not work
# (two python envs; see flake.nix comment).
docs-serve:
    nix develop --command mkdocs serve

# Strict docs build. `--strict` turns every mkdocs warning into a
# build failure; `mkdocs.yml` decides which checks warn (dead links,
# dead anchors, pages missing from nav).
docs-build:
    nix develop --command mkdocs build --strict

# Documentation drift gate: strict build + generated-reference
# byte-compare. Same script the lefthook hook and the docs.yml
# workflow run.
docs-check:
    bash scripts/docs-check.sh

# Live landing content smoke (SYS-3 layer c). Probes the running
# production site — the CMS retags prod with no PR, so an image smoke
# would never witness a content-only regression. Defaults to
# apprafter.dev; pass a base URL to probe a preview/local host.
landing-smoke base='https://apprafter.dev':
    bash scripts/landing-site-smoke.sh {{base}}

# Landing fallback-JSON schema gate (SYS-3 layer a). The bun test that
# validates the fallback JSONs baked into the reproducible image against
# the site shapes and the phase registry.
landing-check:
    cd landing && bun test landing-content-gate.test.ts

# Regenerate `docs/reference/cli/**` from the clap definitions.
#
# MUST be run (and the result committed) after ANY change to the CLI
# surface — a new command, a renamed or removed flag, a changed
# default, an edited doc comment on anything under
# `cli/platform-cli/src/`. `docsgen check` byte-compares the committed
# tree against a fresh render, so forgetting this fails `just lint`,
# the pre-commit hook and the docs workflow.
#
# ALSO run it on a release commit: `commands.json` carries
# `cli_version`, so bumping `cli/Cargo.toml` changes a byte-compared
# artefact. That is deliberate — a consumer of the machine-readable
# surface needs to know which CLI it describes.
#
# Goes through the flake for the same pinned toolchain as the docs
# build; `cli/` is its own Cargo workspace, hence the `cd`.
docsgen-generate:
    nix develop --command bash -c 'cd cli && cargo run -q -p docsgen -- generate'

# Render the platform-stack umbrella chart from CUE source into
# `platform-stack/dist/platform-stack-<version>/`. Wrapper over
# `make -C platform-stack render`. Used by the publish workflow
# (`.github/workflows/platform-stack-publish.yml`) and by
# contributors editing `platform-stack/cue/`. `dist/` is gitignored.
platform-stack-render:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v make >/dev/null 2>&1; then
        make -C platform-stack render-only
    else
        nix shell nixpkgs#gnumake -c make -C platform-stack render-only
    fi

# Render + helm lint + per-tier helm template sanity check. The
# `make render` target inside `platform-stack/` already invokes
# helm lint; this just bundles the typical developer round-trip
# under one project-level entry point.
platform-stack-check:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v make >/dev/null 2>&1 && command -v helm >/dev/null 2>&1; then
        make -C platform-stack render
    else
        nix shell nixpkgs#gnumake nixpkgs#kubernetes-helm -c make -C platform-stack render
    fi

# Quick repo statistics — lines of code per language.
stats:
    @echo "Lines of code (tracked source files):"
    @git ls-files \
        '*.rs' '*.ts' '*.tsx' '*.cue' '*.sh' '*.nix' \
        2>/dev/null | xargs wc -l 2>/dev/null | tail -1 || true
