#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Local-first documentation gate: the strict mkdocs build (dead
# links, dead anchors, unlisted pages, missing snippet includes)
# followed by `docsgen check` — the byte-compare of the generated CLI
# reference under docs/reference/cli/ against the clap tree.
#
# Order matters: the mkdocs build reports on what is committed, so it
# runs first and its failures (a dead link a contributor wrote) are
# not hidden behind a regeneration reminder.
#
# Runs under `nix develop` so mkdocs, the theme and the plugins are
# the flake.lock-pinned versions: a byte-compare needs ONE toolchain
# across local + CI. Builds into a scratch dir so ./site is never
# clobbered by a check.
set -euo pipefail
if ! command -v nix >/dev/null 2>&1; then
    echo "ERROR: docs-check needs nix — mkdocs, the material theme and the" >&2
    echo "plugins must be the flake.lock-pinned versions, because a later" >&2
    echo "change byte-compares generated pages against them. There is no" >&2
    echo "cue-style fall back to a system binary here." >&2
    echo >&2
    echo "Install nix (flakes enabled) — see docs/contributing/setup.md — or" >&2
    echo "skip the local hook with 'git commit --no-verify'; the docs workflow" >&2
    echo "still gates the pull request." >&2
    exit 2
fi
cd "$(dirname "$0")/.."
exec nix develop --command bash -c '
  set -euo pipefail
  out="$(mktemp -d)"
  trap '"'"'rm -rf "$out"'"'"' EXIT
  mkdocs build --strict --site-dir "$out/site"
  cd cli && cargo run --quiet -p docsgen -- check
'
