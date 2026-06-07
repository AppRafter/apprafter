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
# `cue export --out json | jq` and re-exports each one
# individually via `cue export -e <key> --out yaml`. This is
# walk-fix #5 post-B.1.79a (cue-cmp v0.1.2).
#
# Filter: only top-level values that LOOK like k8s objects
# (`apiVersion` + `kind` present) are emitted. Helper values
# the user might declare at the top level (e.g. shared
# constants) are silently skipped — they're imported by the
# real manifests anyway, so no need to land on stdout.
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

# ── Locate the CUE package directory ───────────────────────
#
# Argo CD sets the working directory to the Application's
# `spec.source.path` (repo-root-relative). The AppRafter
# manifest lives in an `apprafter/` directory that, for
# scaffolded user repos, carries its OWN `cue.mod/` (vendored
# schemas — `apprafter app scaffold` walk-fix post-Part-3b).
#
# A nested `cue.mod/` defines a MODULE BOUNDARY: `cue export
# ./...` run from a PARENT directory does not descend into the
# nested module, so it reports "matched no packages". The fix
# is to run `cue export` from INSIDE the package directory.
# Mirror the discovery convention from plugin.yaml:
#
#   * cwd basename `apprafter` → source.path already points at
#     the convention directory; run here (cue.mod, if any, is
#     in cwd or an ancestor).
#   * else if `./apprafter/` holds `.cue` files → cd into it.
#     Covers source.path pointing at the repo root / a parent.
#   * else → run in cwd (filename-prefix convention, or the
#     CI fixture that writes the manifest at cwd directly).
#
# After the cd, `cue export ./...` resolves the module by
# walking UP from the new cwd — so this works whether the
# `cue.mod/` is the vendored one inside `apprafter/` (external
# scaffolded repo) OR a repo-root `cue.mod/` shared across
# many apps (the AppRafter monorepo's own landing manifests).
if [ "$(basename "$PWD")" != "apprafter" ] \
   && [ -d ./apprafter ] \
   && find ./apprafter -maxdepth 1 -type f -name '*.cue' -print -quit | grep -q .; then
    cd ./apprafter
fi

# ── Per-environment injection (subphase 2.9, ADR 0044) ─────
#
# When the Argo Application's `spec.source.plugin.env` sets
# APPRAFTER_APP_ENV (the CLI `apprafter app add --env` does
# this), every rendered manifest is stamped with
# `spec.environment` plus an `apprafter.io/environment` label
# carrying that env name. The operator then unifies
# `spec.environments[<env>]` onto `spec.base` before rendering
# (see operator-rendering's APPRAFTER_ENV path). When the var
# is unset the manifest is emitted unchanged (base-only) — the
# pre-2.9 behaviour is byte-for-byte preserved.
#
# Mechanism: round-trip ONE rendered YAML document through
# `cue export yaml: - --out json | jq | cue export json: -
# --out yaml`. cue reads stdin when the input argument is `-`,
# and the `yaml:` / `json:` filetype prefixes pin the input
# encoding (bare `-` would otherwise be parsed as CUE). jq
# sets `.spec.environment` and merges the label into
# `.metadata.labels` (creating it if absent). All cue/jq
# stderr is suppressed here — on success it's silent, and the
# round-trip only runs on already-validated manifests (the
# earlier `cue export ./...` succeeded), so a failure here is
# a bug, not user error; we let the non-zero exit propagate
# under `set -e` rather than emit partial YAML to stdout.
#
# Applied identically on the Style-A single-manifest path and
# inside the Style-B per-key loop, so multi-manifest streams
# get every document stamped.
inject_env() {  # stdin: one rendered manifest; stdout: same, injected when APPRAFTER_APP_ENV set
    if [ -z "${APPRAFTER_APP_ENV:-}" ]; then
        cat
        return
    fi
    cue export yaml: - --out json 2>/dev/null \
      | jq --arg e "$APPRAFTER_APP_ENV" \
          '.spec.environment = $e
           | .metadata.labels = ((.metadata.labels // {}) + {"apprafter.io/environment": $e})' \
      | cue export json: - --out yaml
}

# Use temp files so we can capture both the JSON body and
# any stderr without merging streams (Argo CD reads stdout
# for manifests; stderr is the diagnostic surface).
json_out=$(mktemp)
err_out=$(mktemp)
trap 'rm -f "$json_out" "$err_out"' EXIT

# `./...` recursively evaluates all CUE files under the
# current directory (now the package dir after the cd above),
# resolving imports through the nearest `cue.mod/module.cue`
# CUE finds walking upward — the vendored module inside
# `apprafter/` for scaffolded repos, or a repo-root module
# for the monorepo. `cue.mod/pkg/` itself is the dependency
# cache and is excluded from `./...` evaluation, so the
# vendored schema package is never emitted as a manifest.
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

# Two source-layout conventions are accepted:
#
#   A) **Unwrapped**: the package's top-level fields ARE
#      the manifest — `apiVersion` + `kind` + `metadata` +
#      `spec` declared directly at package scope. Common
#      for single-resource files.
#
#      ```cue
#      package app
#      apiVersion: "apprafter.io/v1alpha1"
#      kind:       "Application"
#      metadata: name: "hello"
#      spec: image: "..."
#      ```
#
#   B) **Named wrapper(s)**: each top-level field is a
#      complete manifest under a readable name. Required
#      for multi-resource files (a single CUE file declares
#      `landingWeb: …`, `landingWebPreview: …` side-by-side).
#
#      ```cue
#      package apprafter
#      landingWeb: v1alpha1.#Application & { ... }
#      landingWebPreview: v1alpha1.#Application & { ... }
#      ```
#
# `cue export` emits style A as a bare `{apiVersion, kind,
# metadata, spec}` JSON object. Style B emits the same with
# the manifest nested under a field key. We dispatch on
# whether the top-level JSON itself carries `apiVersion`
# + `kind`.
is_top_level_manifest=$(jq -r '
    if (type == "object" and has("apiVersion") and has("kind"))
    then "yes" else "no" end' "$json_out")

if [ "$is_top_level_manifest" = "yes" ]; then
    # Style A — single manifest, emit verbatim. Re-run cue
    # export with `--out yaml` (instead of round-tripping the
    # captured JSON through yq) so the output matches what
    # operators get when they run `cue export ./...` locally;
    # consistent surface beats one fewer subprocess.
    echo "---"
    cue export ./... --out yaml | inject_env
    exit 0
fi

# Style B — enumerate top-level keys whose value is a k8s-
# shaped object (`apiVersion` + `kind` set). Unsorted
# iteration preserves CUE's declaration order, which
# matches operator expectations when scanning the rendered
# manifest stream.
#
# `--raw-output` strips JSON quoting so each line is a bare
# key the `for` loop reads cleanly.
keys=$(jq --raw-output \
    'to_entries[]
     | select(.value | type == "object" and has("apiVersion") and has("kind"))
     | .key' "$json_out")

if [ -z "$keys" ]; then
    # No k8s manifests in the source. Argo CD treats empty
    # output as "no resources to sync" — the right behaviour
    # when the user's path doesn't carry any AppRafter /
    # Argo CD resources (e.g. they pointed `path` at a
    # directory that only has supporting CUE).
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
echo "$keys" | while IFS= read -r key; do
    [ -z "$key" ] && continue
    echo "---"
    # Capture the rendered doc into a variable (rather than
    # piping `cue export` straight into `inject_env`) so the
    # export's exit code and stderr are still checked directly
    # — a pipe would mask cue's failure behind `inject_env`'s
    # exit. Only on success does the doc flow through the env
    # injection, so the "all or nothing" abort semantics below
    # are preserved.
    if ! doc=$(cue export ./... -e "$key" --out yaml 2>"$err_out"); then
        # Surface the per-key error to stderr so operators can
        # locate the failing manifest. Single-manifest failure
        # aborts the whole sync — keeping stricter "all or
        # nothing" semantics is safer than partial application.
        summary=$(awk 'NF { print; exit }' "$err_out")
        echo "::cue-cmp:: failed exporting '${key}': ${summary}" >&2
        echo "--- full cue export -e ${key} stderr ---" >&2
        cat "$err_out" >&2
        exit 1
    fi
    printf '%s\n' "$doc" | inject_env
done
