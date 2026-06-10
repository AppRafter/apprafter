#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Local-first CRD drift gate (ADR 0047). Asserts that every committed
# operator-chart `crd-*.yaml` is byte-identical to what `crdgen` renders
# from the `schemas/v1alpha1` CUE schemas right now. Catches "edited the
# CUE, forgot `just gen-crds`" and "hand-edited a GENERATED file".
#
# Runs under `nix develop` so `cue` is the flake.lock-pinned version: the
# byte-compare needs ONE cue version across local + CI (ADR 0047 R4). The
# same flake shell also provides `cargo`. On a drift it prints the first
# differing line and exits non-zero — run `just gen-crds` to fix.
set -euo pipefail
cd "$(dirname "$0")/.."
exec nix develop --command bash -c 'cd operator && cargo run --quiet -p crdgen -- check'
