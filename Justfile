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
    ./scripts/check-crd-structural.sh
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

# Validate every hand-rolled CRD against a REAL apiserver (ephemeral kind
# cluster). `helm lint` does NOT catch CRD structural-schema errors — only
# the apiserver does — so run this before any CRD-changing release. Fast
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

# Run the needs.pg ResourceClaim-chain k3d walk (generate -> schedule
# -> provision -> resume + DSN -> delete + RetainedClaim -> force-GC ->
# psql DROP proof). Deliberately NOT a dependency of `e2e`: it boots a
# Postgres pod (heavier) and runs on its own nightly cadence via
# .github/workflows/e2e-pg-nightly.yml. Requires Docker + k3d on PATH.
e2e-pg:
    bash e2e/needs-pg-walk.sh

# Run the needs.redis ResourceClaim-chain k3d walk (generate -> schedule
# -> provision -> resume + DSN -> $N-ACL isolation proof -> delete +
# RetainedClaim -> force-GC -> FLUSHDB/DELUSER proof). Like e2e-pg it is
# NOT a dependency of `e2e`: it boots a Dragonfly pod and runs on its own
# nightly cadence via .github/workflows/e2e-redis-nightly.yml. Requires
# Docker + k3d on PATH.
e2e-redis:
    bash e2e/needs-redis-walk.sh

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

# Serve the TechDocs site locally for preview.
docs-serve:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v mkdocs >/dev/null 2>&1; then
        mkdocs serve
    else
        echo "mkdocs not on PATH; running via Nix..."
        nix shell nixpkgs#python3Packages.mkdocs-material -c mkdocs serve
    fi

# Build the TechDocs site (output to ./site/).
docs-build:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v mkdocs >/dev/null 2>&1; then
        mkdocs build --strict
    else
        nix shell nixpkgs#python3Packages.mkdocs-material -c mkdocs build --strict
    fi

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
