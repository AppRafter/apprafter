#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-MIT
#
# Provision tools that are not installed by the dev-container features.
# Idempotent — safe to re-run.

set -euo pipefail

INSTALL_BIN="${HOME}/.local/bin"
mkdir -p "$INSTALL_BIN"
export PATH="$INSTALL_BIN:$PATH"

CUE_VERSION="v0.10.0"
K3D_VERSION="v5.6.3"
COSIGN_VERSION="v2.2.4"

install_cue() {
  if command -v cue >/dev/null 2>&1; then return; fi
  curl -fsSL "https://github.com/cue-lang/cue/releases/download/${CUE_VERSION}/cue_${CUE_VERSION}_linux_amd64.tar.gz" \
    | tar -xzC "$INSTALL_BIN" cue
}

install_k3d() {
  if command -v k3d >/dev/null 2>&1; then return; fi
  curl -fsSL "https://github.com/k3d-io/k3d/releases/download/${K3D_VERSION}/k3d-linux-amd64" \
    -o "$INSTALL_BIN/k3d"
  chmod +x "$INSTALL_BIN/k3d"
}

install_just() {
  if command -v just >/dev/null 2>&1; then return; fi
  curl -fsSL https://just.systems/install.sh | bash -s -- --to "$INSTALL_BIN"
}

install_lefthook() {
  if command -v lefthook >/dev/null 2>&1; then return; fi
  go install github.com/evilmartians/lefthook@latest
}

install_cosign() {
  if command -v cosign >/dev/null 2>&1; then return; fi
  curl -fsSL "https://github.com/sigstore/cosign/releases/download/${COSIGN_VERSION}/cosign-linux-amd64" \
    -o "$INSTALL_BIN/cosign"
  chmod +x "$INSTALL_BIN/cosign"
}

install_cue
install_k3d
install_just
install_lefthook
install_cosign

echo
echo "Dev container ready."
echo "Try: just --list"
