#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-MIT
#
# Lint CUE schemas and examples. Used both locally and in CI
# (CI wiring lands in phase 0.5).
#
# Override the CUE binary by exporting CUE=<path>. If neither a
# `cue` binary nor `nix` is available, the script exits with a
# non-zero status and a hint.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if command -v cue >/dev/null 2>&1; then
    CUE_CMD=(cue)
elif command -v nix >/dev/null 2>&1; then
    CUE_CMD=(nix run nixpkgs#cue --)
else
    echo "ERROR: cue is not installed and nix is unavailable." >&2
    echo "Install CUE >= 0.10 from https://cuelang.org/docs/install/" >&2
    exit 2
fi

echo "==> cue fmt --check"
"${CUE_CMD[@]}" fmt --check ./schemas/... ./examples/...

echo "==> cue vet ./schemas/..."
"${CUE_CMD[@]}" vet ./schemas/...

echo "==> cue vet ./examples/..."
"${CUE_CMD[@]}" vet ./examples/...

echo "==> all CUE checks passed"
