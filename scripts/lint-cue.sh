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
"${CUE_CMD[@]}" fmt --check ./schemas/... ./examples/... ./platform-stack/cue/...

echo "==> cue vet ./schemas/..."
"${CUE_CMD[@]}" vet ./schemas/...

echo "==> cue vet ./platform-stack/cue/..."
"${CUE_CMD[@]}" vet ./platform-stack/cue/...

echo "==> cue vet ./examples/..."
# Skip Backstage scaffolder skeletons — they contain template placeholders
# (e.g. "${{ values.name }}") that intentionally violate v1alpha1 schemas.
example_dirs=()
while IFS= read -r f; do
    example_dirs+=("$(dirname "$f")")
done < <(find ./examples -type d -name skeleton -prune -o -name '*.cue' -print)
if [[ ${#example_dirs[@]} -gt 0 ]]; then
    mapfile -t example_dirs < <(printf '%s\n' "${example_dirs[@]}" | sort -u)
    "${CUE_CMD[@]}" vet "${example_dirs[@]}"
fi

echo "==> all CUE checks passed"
