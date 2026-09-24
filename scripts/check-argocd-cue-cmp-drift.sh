#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-argocd-cue-cmp-drift.sh — fail if the cue-cmp sidecar image's source
# changed since its published version, without bumping `version.cue`.
#
# ## Why this exists as a script
#
# The check itself is not new: `argocd-cue-cmp-check.yml` ran it on every push
# since 2.12f, as inline shell INSIDE the workflow, which made it the one drift
# guard in this repo that could not run locally — `check-operator-version-bump.sh`,
# `check-cli-version-bump.sh`, `check-backup-runner-pin.sh` and
# `check-platform-stack-version.sh` are all scripts, all wired into `just lint`,
# all catchable before a push.
#
# So it caught a schema change with red CI instead of a red `just lint`, which
# is a worse place to learn it: the push has already fanned out to the publish
# workflows by then. Same rule, same pathspec, now runnable in advance.
#
# The inline copy was kept for a while on the argument that CI must not depend
# on a script the branch under test can edit. That argument does not hold for a
# `pull_request` workflow — the branch under test edits the workflow file just
# as easily — and the two copies had already diverged: this one compares the
# INDEX (see the comment on the diff below), the inline one compared HEAD.
# Since WI-373 this script IS the CI check, run by the `version-guards` job in
# lint.yml with the other version guards.
#
# ## What it checks
#
# The image COPYs `schemas/v1alpha1/*.cue` (ADR 0046), so a schema edit IS an
# image change: the entrypoint injects the schema it ships with, and a sidecar
# built from an older schema silently validates manifests against it.
#
# Usage: check-argocd-cue-cmp-drift.sh [remote]   (default: origin)

set -euo pipefail
# shellcheck source-path=SCRIPTDIR source=lib/published-tag.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib/published-tag.sh"

REMOTE="${1:-origin}"
SOURCE="argocd-cue-cmp/version.cue"

# The version, without needing `cue` on PATH: the file is a two-line package
# with one `version: "x.y.z"` field, and `just lint` must work in a fresh
# checkout with no dev shell.
version="$(sed -n 's/^version:[[:space:]]*"\([^"]*\)".*/\1/p' "$SOURCE" | head -1)"
if [[ -z "${version:-}" ]]; then
    echo "::error::could not read the version from $SOURCE" >&2
    exit 2
fi
tag="argocd-cue-cmp/v${version}"

# The files that end up in the image, plus the schemas it bundles.
# `version.cue` is deliberately NOT here: editing it IS the bump.
paths=(
    'argocd-cue-cmp/Dockerfile'
    'argocd-cue-cmp/plugin.yaml'
    'argocd-cue-cmp/entrypoint.sh'
    'schemas/v1alpha1'
)

# Published, in flight, or undecided — see scripts/lib/published-tag.sh for
# why "the remote could not be asked" is no longer read as "in flight".
resolve_published_tag "$REMOTE" "argocd-cue-cmp/v" "$version"
case "$TAG_STATE" in
    undecided) version_guard_undecided "$TAG_WHY" || exit 1; exit 0 ;;
    in-flight)
        echo "OK: ${tag} not yet on ${REMOTE} — version.cue bump is in flight."
        exit 0
        ;;
esac

if # Compare the tag against the INDEX, not HEAD. As a pre-commit hook this runs
# BEFORE the commit exists, so a `tag..HEAD` diff cannot see the very change
# being committed — the guard passes, and only fires on the NEXT commit, by
# which point the incomplete one has already landed. (Found 2026-09-22: an
# `argocd-cue-cmp/Dockerfile` edit committed with no `version.cue` bump, waved
# through by this hook and caught afterwards by `just lint`.) `--cached`
# compares the tag to the index, which in CI equals HEAD and at pre-commit time
# is exactly the tree about to become the commit.
git diff --cached --quiet "${tag}" -- "${paths[@]}"; then
    echo "OK: the cue-cmp image source is unchanged since ${tag}."
    exit 0
fi

changed="$(git diff --cached --name-only "${tag}" -- "${paths[@]}" | head -20)"
cat >&2 <<EOF
::error::The cue-cmp sidecar's image source changed since ${tag} was published,
but ${SOURCE} still says ${version}.

The image bundles schemas/v1alpha1 (ADR 0046), so a schema edit is an image
change: the entrypoint injects the schema it ships with, and a sidecar built
from the old one validates manifests against a schema this tree no longer has.

Bump ${SOURCE}, or revert the source change. The bump also moves the chart's
sidecar pin (component_argocd-cue-cmp.cue reads it), so re-render
platform-stack after bumping.

Changed since ${tag}:
${changed}
EOF
exit 1
