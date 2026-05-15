# SPDX-License-Identifier: FSL-1.1-MIT
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
    if find . -name Cargo.toml -not -path './target/*' -not -path '*/node_modules/*' | head -1 | grep -q .; then
        cargo fmt --all -- --check
        cargo clippy --all-targets --all-features -- -D warnings
    else
        echo "==> no Cargo.toml — skipping rustfmt/clippy"
    fi
    if find . -name package.json -not -path '*/node_modules/*' | head -1 | grep -q .; then
        bun run lint
    else
        echo "==> no package.json — skipping bun lint"
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
    if find . -name Cargo.toml -not -path './target/*' -not -path '*/node_modules/*' | head -1 | grep -q .; then
        cargo fmt --all
    fi

# Run all tests, conditional on workspace presence.
test:
    #!/usr/bin/env bash
    set -euo pipefail
    if find . -name Cargo.toml -not -path './target/*' -not -path '*/node_modules/*' | head -1 | grep -q .; then
        cargo test --all --all-features
    else
        echo "==> no Cargo.toml — skipping cargo test"
    fi
    if find . -name package.json -not -path '*/node_modules/*' | head -1 | grep -q .; then
        bun test
    else
        echo "==> no package.json — skipping bun test"
    fi

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
