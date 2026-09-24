#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Provision tools that are not installed by the dev-container features.
# Idempotent — safe to re-run.

set -euo pipefail

INSTALL_BIN="${HOME}/.local/bin"
mkdir -p "$INSTALL_BIN"
export PATH="$INSTALL_BIN:$PATH"

CUE_VERSION="v0.17.1"
K3D_VERSION="v5.6.3"
COSIGN_VERSION="v2.2.4"

# A MINOR, not a release: the newest patch of it is resolved at install time,
# the rule mise.toml states for these tools (`latest` gives up the minor, an
# exact pin gives up the security patches). Copies of mise.toml's `bun` and
# `just`; lefthook is the flake-locked nixpkgs' minor, which mise does not
# list (`nix eval --raw --inputs-from . nixpkgs#lefthook.version`).
# scripts/upstream-versions.py reads these three lines, and the *_VERSION ones
# above, by name (scripts/upstream-pins.json), so rename them there too.
BUN_MINOR="1.4"
JUST_MINOR="1.51"
LEFTHOOK_MINOR="v2.1"

# Newest release tag "<prefix>.<patch>" of a git repository, e.g. prefix
# "bun-v1.4" -> "bun-v1.4.2". Tags with anything after the patch number
# (release candidates, canaries) never match. Callers assign the result on its
# own line (`tag="$(newest_tag ...)"`, not `local tag="$(...)"`) so that a miss
# stops the script under `set -e`.
newest_tag() {
  local url="$1" prefix="$2" tag
  tag="$(git ls-remote --tags --refs "$url" \
    | sed -n "s|^.*refs/tags/\(${prefix//./\\.}\.[0-9][0-9]*\)\$|\1|p" \
    | sort -V | tail -n 1)"
  if [[ -z "$tag" ]]; then
    echo "post-create: no ${prefix}.<patch> tag in ${url}" >&2
    return 1
  fi
  printf '%s\n' "$tag"
}

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

install_bun() {
  if command -v bun >/dev/null 2>&1; then return; fi
  local tag target
  tag="$(newest_tag https://github.com/oven-sh/bun "bun-v${BUN_MINOR}")"
  # The AVX2 probe bun's own installer makes: without AVX2 the default build
  # dies on its first instruction, and the -baseline build is the fallback.
  target="linux-x64"
  grep -qw avx2 /proc/cpuinfo || target="linux-x64-baseline"
  curl -fsSL "https://github.com/oven-sh/bun/releases/download/${tag}/bun-${target}.zip" \
    -o /tmp/bun.zip
  unzip -oqj /tmp/bun.zip "bun-${target}/bun" -d "$INSTALL_BIN"
  rm -f /tmp/bun.zip
}

install_just() {
  if command -v just >/dev/null 2>&1; then return; fi
  local tag
  tag="$(newest_tag https://github.com/casey/just "${JUST_MINOR}")"
  curl -fsSL "https://github.com/casey/just/releases/download/${tag}/just-${tag}-x86_64-unknown-linux-musl.tar.gz" \
    | tar -xzC "$INSTALL_BIN" just
}

# lefthook 2.x lives at the `/v2` module path. The previous
# `go install github.com/evilmartians/lefthook@latest` could never reach it:
# `@latest` of the v1 path is the last 1.x (v1.13.6), while the flake runs 2.x.
# A version prefix is a Go module query, so `@v2.1` is the newest v2.1.x.
install_lefthook() {
  if command -v lefthook >/dev/null 2>&1; then return; fi
  go install "github.com/evilmartians/lefthook/v2@${LEFTHOOK_MINOR}"
}

install_cosign() {
  if command -v cosign >/dev/null 2>&1; then return; fi
  curl -fsSL "https://github.com/sigstore/cosign/releases/download/${COSIGN_VERSION}/cosign-linux-amd64" \
    -o "$INSTALL_BIN/cosign"
  chmod +x "$INSTALL_BIN/cosign"
}

install_cue
install_k3d
install_bun
install_just
install_lefthook
install_cosign

echo
echo "Dev container ready."
echo "Try: just --list"
