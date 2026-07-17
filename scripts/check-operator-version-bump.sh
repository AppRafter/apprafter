#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-operator-version-bump.sh — fail if the operator image/CRD source changed
# since the operator's currently-declared appVersion was PUBLISHED, without
# bumping appVersion.
#
# ## Why (the 0.2.40/0.2.41 double-yank class)
#
# `release-operator.yml` has a two-axis gate: if the `operator/v<appVersion>`
# git tag AND the images already exist on ghcr.io, it SKIPS the rebuild. So a
# functional operator change (operator-core, a controller, a CRD template, or a
# bundled `schemas/v1alpha1` edit that forces an operator rebuild) that rides an
# ALREADY-PUBLISHED appVersion silently never ships — the deployed operator
# stays the old image, and a platform-stack that pins it ships broken. This bit
# 2.6d-4 twice (op v0.2.32 was already published for the 2.4h imagePolicy fix →
# the spec.backup projection didn't ship → yank; then the runner RBAC → yank).
#
# This check makes that LOUD before publish: if the operator/CRD/schema source
# changed since `operator/v<appVersion>` was published and appVersion wasn't
# bumped, fail with an actionable message.
#
# ## Semantics
#
# - appVersion tag NOT yet on origin  → an in-flight bump; nothing to check (OK).
# - appVersion tag IS on origin, and no image-affecting path changed since it → OK.
# - appVersion tag IS on origin, and operator source / CRD templates /
#   schemas/v1alpha1 changed since it → FAIL (bump appVersion + the
#   platform-stack operatorVersion pin to a fresh version).
#
# Image-affecting paths (NOT docs): the operator/webhook binaries + their src,
# the operator-core + operator-controllers crates, the chart CRD/RBAC templates,
# and the bundled CUE schema. `*.md` under operator/ is excluded.
#
# Run locally: scripts/check-operator-version-bump.sh
# In CI: needs full history + tags (actions/checkout fetch-depth: 0).

set -euo pipefail

CHART="operator/charts/apprafter-operator/Chart.yaml"
REMOTE="${1:-origin}"

app_version="$(grep '^appVersion:' "$CHART" | awk '{print $2}' | tr -d '"')"
app_version="${app_version#v}"
if [[ -z "$app_version" ]]; then
    echo "::error::could not read appVersion from $CHART" >&2
    exit 2
fi
tag="operator/v${app_version}"

# The image-affecting pathspec (exclude markdown docs under operator/).
paths=(operator schemas/v1alpha1 ":(exclude)operator/**/*.md")

# Is the current appVersion already published?
if ! git ls-remote --tags --exit-code "$REMOTE" "refs/tags/${tag}" >/dev/null 2>&1; then
    echo "OK: ${tag} not yet on ${REMOTE} — appVersion bump is in flight."
    exit 0
fi

# The tag is published — make sure it's available locally for the diff.
if ! git rev-parse --verify --quiet "refs/tags/${tag}" >/dev/null; then
    git fetch --quiet "$REMOTE" "refs/tags/${tag}:refs/tags/${tag}" 2>/dev/null || {
        echo "::warning::could not fetch ${tag} for the diff — skipping the check." >&2
        exit 0
    }
fi

if git diff --quiet "refs/tags/${tag}..HEAD" -- "${paths[@]}"; then
    echo "OK: no image-affecting operator/schema change since ${tag}."
    exit 0
fi

changed="$(git diff --name-only "refs/tags/${tag}..HEAD" -- "${paths[@]}" | head -20)"
cat >&2 <<EOF
::error::Operator image/CRD/schema source changed since ${tag} was PUBLISHED, but
appVersion is still v${app_version}. release-operator's two-axis gate will SKIP the
rebuild (tag + image already exist), so this change would silently NOT ship — the
class of miss behind the 0.2.40/0.2.41 double-yank.

Bump the operator appVersion (both operator/charts/*/Chart.yaml) to a FRESH version
AND the platform-stack operatorVersion pin (component_apprafter-operator.cue +
component_admission-webhook.cue + the compatibility.cue entry), then re-render.

Changed since ${tag}:
${changed}
EOF
exit 1
