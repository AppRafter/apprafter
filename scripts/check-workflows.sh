#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Lint the GitHub Actions workflows with actionlint.
#
# THE DEFECT CLASS THIS EXISTS FOR. A workflow file that does not parse
# does not fail a check — it fails as a RUN WITH ZERO JOBS, which
# `gh pr checks` does not list at all. Every PR check can be green while
# a release workflow is broken, and nothing says so until the day it was
# supposed to run. That is how `release-cli.yml` broke on 2026-09-22: a
# shell COMMENT inside a `run:` block contained empty Actions expression
# delimiters. Actions expands those before the shell sees the script, so
# it does not know the line is a comment; an empty expression is
# unparseable and took the whole file down.
#
# actionlint catches that class and a good deal more (unknown `needs:`
# targets, bad `runs-on`, misspelled contexts) without a network call or
# a container.
#
# Override the binary by exporting ACTIONLINT=<path>. With neither
# actionlint nor nix available the script exits non-zero and says so,
# rather than passing silently — a gate that skips when its tool is
# missing is not a gate.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if [[ -n "${ACTIONLINT:-}" ]]; then
    ACTIONLINT_CMD=("$ACTIONLINT")
elif command -v actionlint >/dev/null 2>&1; then
    ACTIONLINT_CMD=(actionlint)
elif command -v nix >/dev/null 2>&1; then
    ACTIONLINT_CMD=(nix run nixpkgs#actionlint --)
else
    echo "ERROR: actionlint is not installed and nix is unavailable." >&2
    echo "Install it from https://github.com/rhysd/actionlint or run" >&2
    echo "'nix develop' first." >&2
    exit 2
fi

# Two shellcheck STYLE codes predate this gate and are not what it is for:
#   SC2129 — prefer `{ a; b; } >> file` over repeated redirects
#   SC2006 — prefer $(...) over legacy backticks
# They appear in argocd-cue-cmp-publish.yml and platform-stack-publish.yml.
# Ignoring them keeps the gate's output actionable instead of a wall a
# reader learns to skip; anything that can actually break a workflow —
# syntax, expressions, contexts, job graph — still fails here. Fixing the
# two is worth its own change, not a drive-by inside a release.
echo "==> actionlint .github/workflows/"
"${ACTIONLINT_CMD[@]}" \
    -ignore 'shellcheck reported issue in this script: SC2129' \
    -ignore 'shellcheck reported issue in this script: SC2006' \
    .github/workflows/*.yml

echo "OK: workflows parse and lint clean."
