#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-cli-version-bump.sh — fail if CLI source changed since the monorepo tag
# matching `cli/Cargo.toml`'s version was PUBLISHED, without bumping it.
#
# ## Why
#
# The sibling `check-operator-version-bump.sh` has guarded the operator since
# the 0.2.40/0.2.41 double-yank. The CLI had no equivalent, and the gap is the
# same shape with a different consequence: `release-cli.yml` builds from a
# `v<version>` tag, so a CLI change riding an ALREADY-TAGGED version is not
# published by anything — `apprafter --version` then reports a release whose
# code it does not contain, and the fix simply never reaches a user.
#
# That happened: the D26 sequential-restore fix (a75f0dc) landed after v0.2.52
# was tagged and pushed, `cli/Cargo.toml` still read 0.2.52, and nothing
# complained. The chart has a drift guard that FORCES a bump; the operator has
# this check; the CLI had neither, so it was the one that slipped.
#
# ## What it checks
#
# If `v<version>` exists on the remote AND any CLI source changed since it, the
# version must move. Docs and the generated CLI reference are excluded: they are
# regenerated FROM the clap tree and cannot change the shipped binary.
#
# Usage: check-cli-version-bump.sh [remote]   (default: origin)

set -euo pipefail

MANIFEST="cli/Cargo.toml"
REMOTE="${1:-origin}"

# The workspace version — the `[workspace.package] version` line, which every
# crate inherits and which `release-cli.yml` builds under.
version="$(awk '/^\[workspace\.package\]/{f=1} f && /^version/{gsub(/[" ]/,"",$3); print $3; exit}' "$MANIFEST")"
if [[ -z "${version:-}" ]]; then
    echo "::error::could not read workspace.package version from $MANIFEST" >&2
    exit 2
fi
tag="v${version}"

# Source that changes the shipped binary. `cli/**/*.md` and the generated
# reference under docs/ are not it.
paths=(cli ":(exclude)cli/**/*.md")

if ! git ls-remote --tags --exit-code "$REMOTE" "refs/tags/${tag}" >/dev/null 2>&1; then
    echo "OK: ${tag} not yet on ${REMOTE} — version bump is in flight."
    exit 0
fi

if ! git rev-parse --verify --quiet "refs/tags/${tag}" >/dev/null; then
    git fetch --quiet "$REMOTE" "refs/tags/${tag}:refs/tags/${tag}" 2>/dev/null || {
        echo "::warning::could not fetch ${tag} for the diff — skipping the check." >&2
        exit 0
    }
fi

if git diff --quiet "refs/tags/${tag}..HEAD" -- "${paths[@]}"; then
    echo "OK: no CLI source change since ${tag}."
    exit 0
fi

changed="$(git diff --name-only "refs/tags/${tag}..HEAD" -- "${paths[@]}" | head -20)"
cat >&2 <<EOF
::error::CLI source changed since ${tag} was PUBLISHED, but cli/Cargo.toml still
reads ${version}. release-cli.yml builds from the tag, so this change ships
nowhere and \`apprafter --version\` reports a release it does not contain.

Bump [workspace.package] version in ${MANIFEST}, regenerate cli/Cargo.lock IN THE
SAME COMMIT (release-cli.yml runs --locked and fails if the lock lags), and tag
the release commit.

Changed since ${tag}:
${changed}
EOF
exit 1
