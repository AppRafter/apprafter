#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-cue-cmp-mirror.sh — fail if the cue-cmp plugin manifest and its
# chart-embedded mirror have diverged.
#
# ## Why this exists
#
# `platform-stack/cue/component_argocd.cue` embeds a copy of
# `argocd-cue-cmp/plugin.yaml` in the `cue-cmp-plugin-config` ConfigMap,
# and the repo-server sidecar mounts that ConfigMap OVER the copy baked
# into the image (`subPath: plugin.yaml`). The mirror is therefore what
# actually RUNS in a cluster — and until this script, nothing compared
# the two. Editing only `plugin.yaml` passes `test-discover.sh`, passes
# both drift guards after a version bump, publishes a new image, and
# ships the OLD snippet to every cluster. ADR 0063 §Risks.
#
# ## What it compares — PARSED DOCUMENTS, NOT BYTES
#
# This is deliberate; do NOT "simplify" it into a plain `diff` of the
# two files. The two documents legitimately carry DIFFERENT prose
# preambles — the file explains the plugin for a reader of
# `argocd-cue-cmp/`, the chart block explains the walk-fix for a reader
# of the chart — and the file has an interior comment above `generate:`
# that the mirror does not. A byte diff is ~45 lines red on a tree that
# is perfectly in sync. Making them byte-identical would mean editing
# chart source, which forces a platform-stack version bump and release.
#
# So both sides are pushed through the same `cue export --out yaml`
# normalizer, which drops YAML comments and canonicalizes YAML style
# (flow vs block sequences, quoting) while preserving field order and
# every scalar byte-for-byte.
#
# The consequences, precisely:
#
#   * YAML-level comments are OUT OF SCOPE. Appending `# note` to
#     either document passes. They are not data; Argo CD parses the
#     ConfigMap value and consumes only the parsed document, so a
#     comment cannot change what runs in a cluster.
#   * A `#` INSIDE the `discover` block scalar IS data — it is part of
#     the shell snippet string, not a YAML comment — and IS compared.
#     Inserting `# shell-level comment` above the `find` line fails.
#   * Every other difference fails: a changed flag in the shell
#     snippet, a new `generate.args` entry, a renamed key, reordered
#     fields.
#
# Nothing that reaches the CMP at runtime can differ without this
# diffing.
#
# Usage: check-cue-cmp-mirror.sh
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Resolved through scripts/cue, the single resolver for this repo's pinned
# cue. It used to be `command -v cue || nix run nixpkgs#cue`, where BOTH
# branches give whatever the machine or nixpkgs happens to have rather than
# the version flake.nix pins. See scripts/cue for the full reasoning.
CUE_CMD=("$(git rev-parse --show-toplevel)/scripts/cue")

FILE="argocd-cue-cmp/plugin.yaml"
CHART="platform-stack/cue/component_argocd.cue"

# Select the ConfigMap by NAME rather than by its index in
# `extraObjects` — a new entry inserted ahead of it must not silently
# re-point this check at some other object.
#
# CHAINED `if` clauses, not `&&`: CUE's `&&` does not short-circuit, so
# `o.kind == "ConfigMap" && o.metadata.name == ...` evaluates the second
# operand for EVERY entry and dies with `undefined field: metadata` on an
# extraObject that has none. Chained `if` does short-circuit, so a
# metadata-less entry that isn't a ConfigMap is simply skipped.
selector='for o in _components.argocd.values.extraObjects
    if o.kind == "ConfigMap" if o.metadata.name == "cue-cmp-plugin-config"'

count=$("${CUE_CMD[@]}" export ./platform-stack/cue/... \
    -e "len([${selector} {o}])" --out json)
if [[ "$count" != "1" ]]; then
    echo "::error::expected exactly 1 cue-cmp-plugin-config ConfigMap in" >&2
    echo "  ${CHART} extraObjects, found ${count}." >&2
    exit 2
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

"${CUE_CMD[@]}" export ./platform-stack/cue/... \
    -e "[${selector} {o.data.\"plugin.yaml\"}][0]" \
    --out text > "$work/mirror.yaml"

# `.yaml` suffixes matter: cue infers the input format from the
# extension.
"${CUE_CMD[@]}" export "$work/mirror.yaml" --out yaml > "$work/mirror.norm.yaml"
"${CUE_CMD[@]}" export "$FILE" --out yaml > "$work/file.norm.yaml"

if ! diff -u --label "$FILE" "$work/file.norm.yaml" \
              --label "${CHART} → data.\"plugin.yaml\"" "$work/mirror.norm.yaml"; then
    cat >&2 <<EOF

::error::${FILE} and its chart mirror in ${CHART} have diverged.

      The chart's ConfigMap is mounted OVER the image's copy, so the
      MIRROR is what runs in a cluster. Editing only plugin.yaml ships
      the old snippet.

      Copy the file's body into ${CHART}'s
      data."plugin.yaml" block, DOUBLING every backslash (a CUE """
      block treats \( as interpolation and rejects a bare \; with
      "unknown escape sequence").

      The prose comment preambles are allowed to differ — this compares
      the parsed documents, so only a real data change fails here.
EOF
    exit 1
fi

echo "OK: cue-cmp plugin.yaml mirror is in sync"
