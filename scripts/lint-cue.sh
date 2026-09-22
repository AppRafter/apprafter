#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Lint CUE schemas and examples. Used both locally and in CI
# (CI wiring lands in phase 0.5).
#
# The cue binary comes from `scripts/cue`, the single resolver for this repo's
# PINNED version. Export CUE=<path> to override it (e.g. to bisect a candidate
# release); that variable was documented here for a long time and only became
# real in 2026-09.
#
# Why it is not resolved inline: this script used to do
# `command -v cue || nix run nixpkgs#cue --`, and BOTH branches give whatever
# the machine or nixpkgs carries rather than the version flake.nix pins. Six
# other call sites had copied the same two lines.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

CUE_CMD=("$REPO_ROOT/scripts/cue")

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
