#!/bin/sh
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# CUE → YAML wrapper for the Argo CD Config Management Plugin
# sidecar (ADR 0029). Argo CD's repo-server invokes this
# script when an Application's source repository matches the
# `**/apprafter*.cue` glob declared in `plugin.yaml`. The
# script runs `cue export ./... --out yaml` against the
# user's repo (already checked out into the current working
# directory by repo-server) and prints the rendered manifests
# to stdout, where Argo CD picks them up for the sync.
#
# Error handling
# --------------
#
# CUE compile errors are verbose by design — multiple lines
# of unification failures and source locations. Argo CD's UI
# truncates long error strings to ~one screen of text and
# breaks badly on multi-line strings. We post-process to:
#
#   1. Print the **first** error line as a single-line summary
#      on stderr (Argo CD displays this as the sync error).
#   2. Print the **full** error block on stderr below the
#      summary (the full sync log captures it).
#
# Stdout is reserved for the rendered YAML on success.

set -eu

# Use a temp file so we can capture both the YAML body and
# any stderr without merging streams (Argo CD reads stdout
# for manifests; stderr is the diagnostic surface).
yaml_out=$(mktemp)
err_out=$(mktemp)
trap 'rm -f "$yaml_out" "$err_out"' EXIT

# `./...` recursively evaluates all CUE files under the
# current directory, including imports from other paths the
# Argo CD `argocd.argoproj.io/manifest-generate-paths`
# annotation may have included. `--out yaml` ensures Kubernetes-
# compatible output.
if cue export ./... --out yaml >"$yaml_out" 2>"$err_out"; then
    cat "$yaml_out"
    exit 0
fi

# Failure path. Extract the first non-empty error line as
# the summary (CUE's typical format: "field: error: detail").
summary=$(awk 'NF { print; exit }' "$err_out")

# Surface summary to stderr; Argo CD UI uses this as the
# sync-error one-liner.
echo "::cue-cmp:: CUE compile failed: ${summary}" >&2
echo "" >&2

# Then the full block, so the sync log keeps every detail.
echo "--- full cue export stderr ---" >&2
cat "$err_out" >&2

exit 1
