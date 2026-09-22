#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# cargo-deny.sh — advisory, licence, source and duplicate checks for both
# Cargo workspaces.
#
# ## Why
#
# `scripts/upstream-versions.py` asks "are we behind the newest release?".
# That question is blind to the two states that actually hurt:
#
#   * ABANDONED — a dead crate sits on its own final release forever, so it is
#     never "behind". `oci-distribution` read as perfectly current for eighteen
#     months while the project had been renamed to `oci-client`.
#   * VULNERABLE-AND-UNFIXABLE — an advisory with no patched version is not a
#     version gap either.
#
# The first run of this script (2026-09-22) found five findings the watcher had
# never reported: one vulnerability (rsa/Marvin) and four unmaintained crates
# (rustls-pemfile, backoff, derivative, instant), every one of the four
# arriving through a single dependency, `kube` 0.95.
#
# ## Why one config for two workspaces
#
# `cli/` and `operator/` are SEPARATE Cargo workspaces — there is no top-level
# Cargo.toml — so cargo-deny has to run once per workspace. Both point at the
# repo-root `deny.toml` so an advisory decision is made once, in one place,
# rather than drifting between two copies.
#
# Usage:  bash scripts/cargo-deny.sh
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
CONFIG="$ROOT/deny.toml"

# Prefer a cargo-deny on PATH; fall back to nix, matching how the other
# CUE/lua-using scripts in this directory resolve their tools so a fresh
# checkout works without a dev shell.
if command -v cargo-deny >/dev/null 2>&1; then
    _deny() { cargo-deny "$@"; }
else
    _deny() { nix run nixpkgs#cargo-deny -- "$@"; }
fi

status=0
for ws in cli operator; do
    [ -f "$ROOT/$ws/Cargo.toml" ] || { echo "==> no $ws/Cargo.toml — skipping"; continue; }
    echo "==> cargo-deny ($ws)"
    # `--all-features` is a TOP-LEVEL flag, before the subcommand — passing it
    # after `check` is rejected. It matters: a dependency reachable only behind
    # a feature flag still ships when that feature is on, and CI builds both
    # workspaces with --all-features.
    if ! ( cd "$ROOT/$ws" && _deny --all-features check --config "$CONFIG" ); then
        status=1
    fi
done

if [ "$status" -ne 0 ]; then
    cat >&2 <<'EOF'

cargo-deny failed. Before reaching for `deny.toml`'s ignore list:

  * a NEW advisory on a crate we can move is a bump, not an ignore;
  * an UNMAINTAINED crate usually has a successor, sometimes under a new name
    (oci-distribution -> oci-client) — check before assuming it is stuck;
  * only ignore what is genuinely unfixable or unreachable, and write the
    reason inline. An ignore with no reason is indistinguishable from the
    silence this gate exists to end.
EOF
    exit 1
fi

echo "cargo-deny OK: advisories, bans, licences and sources clean in both workspaces."
