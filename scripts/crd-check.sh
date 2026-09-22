#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Local-first CRD drift gate (ADR 0047). Asserts that every committed
# operator-chart `crd-*.yaml` is byte-identical to what `crdgen` renders
# from the `schemas/v1alpha1` CUE schemas right now. Catches "edited the
# CUE, forgot `just gen-crds`" and "hand-edited a GENERATED file".
#
# Runs under `nix develop` so `cue` is the pinned version: the byte-compare
# needs ONE cue version across local + CI (ADR 0047 R4). The same flake shell
# also provides `cargo`. On a drift it prints the first differing line and
# exits non-zero — run `just gen-crds` to fix.
#
# That "ONE cue version" was ASPIRATIONAL until 2026-09 and is worth spelling
# out, because the failure was invisible: the flake shipped whatever cue
# nixpkgs happened to carry (0.16.1) while CI's `cue-lang/setup-cue` installed
# 0.10.0 — six minors apart, and this gate was comparing bytes produced by two
# different evaluators. flake.nix now pins cue to an exact upstream release
# rather than taking nixpkgs', so the sentence above is true by construction.
# A cue bump therefore edits four places together: flake.nix's cueVersion +
# hash, the setup-cue inputs in .github/workflows, argocd-cue-cmp/Dockerfile's
# ARG, and .devcontainer/post-create.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
exec nix develop --command bash -c 'cd operator && cargo run --quiet -p crdgen -- check'
