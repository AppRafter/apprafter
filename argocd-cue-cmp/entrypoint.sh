#!/bin/sh
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# CUE → YAML wrapper for the Argo CD Config Management Plugin
# sidecar (ADR 0029). Argo CD's repo-server invokes this
# script when an Application's source repository matches the
# discovery glob declared in `plugin.yaml`. The script runs
# `cue export` against the user's repo (already checked out
# into the current working directory by repo-server) and
# prints the rendered manifests to stdout, where Argo CD
# picks them up for the sync.
#
# Output contract
# ---------------
#
# Argo CD's CMP `generate.command` must produce one or more
# Kubernetes manifests on stdout, each a complete YAML
# document separated by `---`. The user's CUE source
# typically declares one named top-level value per manifest:
#
#   landingWeb: v1alpha1.#Application & { ... }
#   landingWebPreview: v1alpha1.#Application & { ... }
#
# `cue export ./... --out yaml` on its own would emit
# `landingWeb: ...` / `landingWebPreview: ...` as nested
# top-level keys inside a single YAML document — Argo CD
# would treat that as ONE invalid manifest (no `apiVersion`
# at the top level). To produce a valid YAML stream the
# entrypoint enumerates the top-level keys via
# `cue export --out json | jq` и re-exports each one
# individually via `cue export -e <key> --out yaml`. This is
# walk-fix #5 post-B.1.79a (cue-cmp v0.1.2).
#
# Filter: only top-level values that LOOK like k8s objects
# (`apiVersion` + `kind` present) are emitted. Helper values
# the user might declare at the top level (e.g. shared
# constants) are silently skipped — they're imported by the
# real manifests anyway, so не need to land on stdout.
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

# Use temp files so we can capture both the JSON body and
# any stderr without merging streams (Argo CD reads stdout
# for manifests; stderr is the diagnostic surface).
json_out=$(mktemp)
err_out=$(mktemp)
trap 'rm -f "$json_out" "$err_out"' EXIT

# `./...` recursively evaluates all CUE files under the
# current directory, including imports that resolve through
# `cue.mod/module.cue` at the repo root (CUE walks upward
# from cwd to find the module manifest, so a sub-directory
# source path works as long as the repo root carries
# cue.mod/).
#
# JSON intermediate (rather than YAML) keeps key extraction
# trivial via `jq`.
if ! cue export ./... --out json >"$json_out" 2>"$err_out"; then
    summary=$(awk 'NF { print; exit }' "$err_out")
    echo "::cue-cmp:: CUE compile failed: ${summary}" >&2
    echo "" >&2
    echo "--- full cue export stderr ---" >&2
    cat "$err_out" >&2
    exit 1
fi

# Enumerate top-level keys whose value is a k8s-shaped
# object (`apiVersion` + `kind` set). Unsorted iteration
# preserves CUE's declaration order, which matches operator
# expectations when scanning the rendered manifest stream.
#
# `--raw-output` strips the JSON quoting so each line is a
# bare key the `for` loop reads cleanly.
keys=$(jq --raw-output \
    'to_entries[]
     | select(.value | type == "object" and has("apiVersion") and has("kind"))
     | .key' "$json_out")

if [ -z "$keys" ]; then
    # No k8s manifests in the source. Argo CD treats empty
    # output as "no resources to sync" — which is the right
    # behaviour when the user's path doesn't carry any
    # AppRafter / Argo CD resources (e.g. they pointed `path`
    # at a directory that only has supporting CUE).
    exit 0
fi

# Re-export each manifest individually. `cue export -e <expr>`
# evaluates a top-level expression and emits its value
# unwrapped — exactly the YAML doc shape Argo CD expects.
# The leading `---` line is the YAML document separator;
# emit it before every doc so the stream is always
# well-formed even when only one manifest is present
# (operators reading the rendered output get a consistent
# shape).
first_doc=1
echo "$keys" | while IFS= read -r key; do
    [ -z "$key" ] && continue
    if [ "$first_doc" -eq 1 ]; then
        first_doc=0
    fi
    echo "---"
    if ! cue export ./... -e "$key" --out yaml 2>"$err_out"; then
        # Surface the per-key error к stderr so operators can
        # locate the failing manifest. Single-manifest failure
        # aborts the whole sync — keeping stricter "all or
        # nothing" semantics is safer than partial application.
        summary=$(awk 'NF { print; exit }' "$err_out")
        echo "::cue-cmp:: failed exporting '${key}': ${summary}" >&2
        echo "--- full cue export -e ${key} stderr ---" >&2
        cat "$err_out" >&2
        exit 1
    fi
done
