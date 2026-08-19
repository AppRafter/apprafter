#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Local-first documentation gate, in four passes:
#
#   1. the strict mkdocs build (dead links, dead anchors, unlisted
#      pages, missing snippet includes);
#   2. what the two BUILD HOOKS were supposed to leave behind, read
#      back off the site that was just built: llms.txt, llms-full.txt
#      and one markdown twin per page, and every ```cue fence rendered
#      as highlighted code;
#   3. `docsgen check` — the byte-compare of the GENERATED CLI
#      reference under docs/reference/cli/ against the clap tree;
#   4. `docsgen gate` — every claim in the HAND-WRITTEN documentation
#      resolved against the product: `apprafter` invocations against
#      the clap tree, schema paths against the shipped CRDs, complete
#      CUE manifests through one batched `cue vet`.
#
# Pass 2 exists because BOTH hooks are invisible when they break, and
# in the same way. mkdocs does not know llms.txt was supposed to exist,
# so a hook that stopped writing it — or wrote an index that quietly
# lost a nav group, or twins holding forty characters each — leaves a
# green build and a published site with a broken machine-readable
# layer. And `pymdownx.highlight` swallows a failed lexer lookup and
# falls back to `text`, so an unregistered CUE lexer leaves a green
# build and every CUE block on the site rendered as plain prose. The
# lexer hook does self-check at `on_config`, but that assertion lives
# INSIDE the thing it guards: delete its one line from `hooks:` in
# mkdocs.yml and the self-check goes with it. This pass reads the
# ARTEFACT instead of the registry, so unregistering the hook, having a
# later hook overwrite the lexer, and turning pygments off site-wide
# are all one failure here.
#
# Every expectation it asserts is derived from the site that was just
# built, so it stays true as pages are added: nothing here restates a
# page list, a page count, or a language.
#
# Order matters throughout: the mkdocs build reports on what is
# committed, so it runs first and its failures (a dead link a
# contributor wrote) are not hidden behind a regeneration reminder;
# and `check` precedes `gate` because a stale generated reference is
# a one-command fix that would otherwise be buried under a work-list.
#
# `gate` exits 1 for findings and 2 when the gate itself broke (no
# `cue`, an unreadable page). `set -e` fails the script on either —
# the codes are for a reader of the log, not for control flow here.
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
    echo "still runs on the pull request." >&2
    exit 2
fi
cd "$(dirname "$0")/.."
repo_root="$PWD"
export repo_root
exec nix develop --command bash -c '
  set -euo pipefail
  out="$(mktemp -d)"
  trap '"'"'rm -rf "$out"'"'"' EXIT
  mkdocs build --strict --site-dir "$out/site"
  # Pass 2 lives in its own file: an apostrophe inside a heredoc in
  # this single-quoted `bash -c` string closes the string, and the
  # truncation is silent -- see the module docstring there.
  python3 "$repo_root/scripts/docs-artefacts-check.py" "$out/site"
  cd cli && cargo run --quiet -p docsgen -- check
  cd "$repo_root/cli" && cargo run --quiet -p docsgen -- gate
'
