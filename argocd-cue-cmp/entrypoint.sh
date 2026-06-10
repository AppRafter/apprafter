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
# Argo CD env-var prefix (CRITICAL): a CMP plugin does NOT see
# `spec.source.plugin.env` vars under their bare names. Since
# Argo CD v2.4 the repo-server's CMP server exposes every
# user-declared plugin env var to the generate command PREFIXED
# with `ARGOCD_ENV_` (a deliberate hardening so a repo cannot
# silently override a sidecar's own environment). So the var the
# CLI sets as `APPRAFTER_APP_ENV` arrives here as
# `ARGOCD_ENV_APPRAFTER_APP_ENV`. We resolve the prefixed form
# first and fall back to the bare name — the bare name still
# works for direct invocation (the host regression test
# `test-inject.sh`, or an operator running entrypoint.sh by
# hand). Reading ONLY the bare name made the in-cluster injection
# silently inert (the e2e per-env walk caught this).
APPRAFTER_APP_ENV="${ARGOCD_ENV_APPRAFTER_APP_ENV:-${APPRAFTER_APP_ENV:-}}"
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

# ── Schema + `claim` binding injection (subphase 2.12f, ADR 0046) ──
#
# Bare `claim.<type>.<field>` selectors in `env` (and the named
# `claim.<type>.<name>.<field>` form) are CUE LEXICAL references that
# resolve against a top-level `claim` field. The user does NOT vendor
# that binding — we generate it here, into the package directory
# (cwd), so `cue export ./...` below resolves the markers. Likewise we
# lay down the CURRENT AppRafter schema this image ships with, so a
# `import "apprafter.io/schemas/v1alpha1"` resolves against it
# (inject-wins: overwrite any stale vendored copy). See ADR 0046
# Decisions #2 + #7.
#
# CUE specifics this implementation depends on (all de-risked on real
# cue, 2.12f):
#   * Files whose name begins with `_` or `.` are IGNORED by cue — the
#     generated file is named `apprafter_claim_gen.cue` (no leading
#     underscore) or it would silently never load.
#   * A struct-comprehension that consumes a field supplied via
#     CROSS-PACKAGE unification does NOT re-evaluate. So we cannot emit
#     `claim: v1alpha1.#MkClaim & {_needs: …}` — the loop would run
#     over an empty `_needs` inside the schema package and yield `{}`.
#     The comprehension MUST run in the manifest's own package; only
#     the field-name table `#ClaimFieldsFor` is referenced across the
#     import boundary (definitions ARE cross-package-accessible).
#   * `claim` is a SINGLE top-level lexical binding shared by every
#     manifest in the package. We build it from the UNION of needs
#     (base AND every environments[*], across all top-level Application
#     manifests), collected as {type: {unnamed, names[]}}. It is
#     ENV-AGNOSTIC on purpose — `cue export ./...` evaluates every env
#     block, so a non-active env's claim ref must resolve too (see the
#     "Why ALL environments" note at the collection step).
#     Per-app strictness (rejecting a claim type/field not in THAT
#     app's needs) is the webhook's job — the cue layer's contract is
#     "resolve every declared (type[,name]) field; error on a type or
#     field absent from the union". A claim ref to a wholly undeclared
#     need type, a non-enum field, or an unknown named entry is still a
#     cue error (the field simply doesn't exist in `claim`).
#
# Extraction is two-pass because of the chicken-and-egg: the manifest
# won't evaluate until `claim` resolves, but `claim` is built from the
# manifest's needs. Pass 1 injects a permissive recursive stub
# (`_N: {claim: string} & {[!="claim"]: _N}`) under which any selector
# resolves, letting us read each manifest's `needs` via a TARGETED
# `cue export -e <name>.spec...needs` (a targeted export forces only
# that path concrete, ignoring the still-incomplete `claim` sibling).
# Pass 2 overwrites the stub with the real, concrete `claim`.
#
# `cue.mod/` is created if absent (scaffold no longer vendors it,
# Decision #7); the bundled schema is copied in inject-wins.
# SCHEMA_SRC defaults to the image's bundled path; the host
# regression test (test-inject.sh) overrides it via
# APPRAFTER_SCHEMA_SRC to point at the repo's schemas/v1alpha1 so the
# claim/secret injection path is exercised off-cluster too (the 2.9
# lesson: don't let host tests mask the real injection path).
SCHEMA_SRC="${APPRAFTER_SCHEMA_SRC:-/opt/apprafter/schema/v1alpha1}"
CLAIM_GEN="apprafter_claim_gen.cue"
inject_schema_and_claim() {
    # 0. Bundled schema present? (Absent only in ad-hoc local runs of
    #    entrypoint.sh without the image's /opt payload — skip silently
    #    so plain manifests still render; claim refs then won't resolve,
    #    which surfaces as a normal cue error below.)
    [ -d "$SCHEMA_SRC" ] || return 0

    # 1. cue.mod with the bundled schema (inject-wins). We anchor the
    #    module at the PACKAGE DIRECTORY (cwd) — NOT the nearest cue.mod
    #    walking up — for two reasons:
    #      * Decision #7: scaffolded repos no longer vendor cue.mod, so
    #        cwd usually has none; we own the render workspace.
    #      * If a PARENT carries an `apprafter.io` module (the monorepo
    #        root declares exactly that), injecting the schema into its
    #        `pkg/` makes `import "apprafter.io/schemas/v1alpha1"`
    #        AMBIGUOUS (resolvable via the parent module's own
    #        `schemas/` AND via the injected pkg). A cwd-local cue.mod
    #        establishes a fresh module boundary that shadows the parent,
    #        so the import resolves unambiguously through our bundle.
    #    If cwd already HAS its own cue.mod (a pre-2.12 vendored repo or
    #    one of our standalone fixtures), reuse it (inject-wins on pkg/).
    mod_dir="$PWD/cue.mod"
    if [ ! -d "$mod_dir" ]; then
        mkdir -p "$mod_dir"
        # Minimal module file; the module path is irrelevant to import
        # resolution of the bundled pkg, but cue requires it to exist.
        cat > "$mod_dir/module.cue" <<'MODEOF'
module: "apprafter.io/render-workspace"

language: {
	version: "v0.10.0"
}
MODEOF
    fi
    schema_dst="$mod_dir/pkg/apprafter.io/schemas/v1alpha1"
    mkdir -p "$schema_dst"
    # inject-wins: overwrite so a stale vendored schema can't break
    # #ClaimFieldsFor / the env-value union.
    cp -f "$SCHEMA_SRC"/*.cue "$schema_dst"/ 2>/dev/null || true

    # 2. PASS 1 — permissive stub so the manifest evaluates. The
    #    generated sibling MUST share the user manifest's package clause
    #    (else cue treats them as separate packages and the lexical
    #    `claim` binding is invisible). Detect it FIRST; if there is no
    #    detectable package (e.g. cwd has no readable .cue manifest),
    #    bail BEFORE writing any file — a stub with an unresolved package
    #    name would itself break the export.
    pkg=$(detect_package) || return 0
    cat > "$CLAIM_GEN" <<STUBEOF
package $pkg
// 2.12f pass-1 extraction stub (overwritten in pass 2). Any selector
// resolves: each non-\`claim\` key recurses, every node is a valid
// {claim: string} #EnvRef leaf.
_N: {claim: string} & {[!="claim"]: _N}
claim: _N
STUBEOF

    # 3. Build the list of manifest SCOPES whose `spec` carries needs.
    #    Two layouts (mirrored from the emit dispatch below):
    #      * Style A (unwrapped): apiVersion/kind/spec are package-scope
    #        fields → the spec scope is bare `spec`.
    #      * Style B (named wrappers): each manifest is `<name>: {spec:…}`
    #        → the spec scope is `<name>.spec`.
    #    We always include the bare `spec` scope (a no-op for Style B,
    #    where top-level `spec` doesn't exist → its targeted export just
    #    yields {}), plus every discovered top-level field name. `cue def`
    #    lists top-level fields even with the incomplete `claim` sibling;
    #    we strip our own helpers and the literal scalars apiVersion/kind.
    scopes="spec"
    names=$(cue def ./... 2>/dev/null \
        | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\):.*/\1/p' \
        | grep -vxE '_N|claim|apiVersion|kind|metadata|spec' || true)
    for name in $names; do
        scopes="$scopes ${name}.spec"
    done

    # 4. Collect the per-type {unnamed, names[]} union across all
    #    manifests' base.needs AND **every** environments[*].needs.
    #
    #    Why ALL environments, not just the active one (APPRAFTER_APP_ENV)?
    #    `cue export ./...` evaluates the WHOLE manifest, including every
    #    `environments[*].env` block — so a `claim.redis.url` that lives
    #    in a NON-active env must still resolve at cue time, or the
    #    render fails. The operator picks the active env later; the
    #    cue layer just needs every declared (type[,name]) to exist in
    #    `claim`. Per-env strictness (a ref to a type not in THAT env's
    #    effective needs) is the webhook's job — this matches the
    #    cross-manifest union rationale above. This also sidesteps the
    #    per-env binding limitation entirely: the binding is env-agnostic.
    #
    #    merge_prog: `norm1` normalises one `needs` object (scalar struct
    #    => unnamed; array of {name?} => unnamed flag + collected names)
    #    to {type:{unnamed,names[]}}; `union` folds it into the running
    #    `state`. Applied to each manifest's base.needs and every
    #    environment's needs.
    envneeds_tmp=$(mktemp)
    merge_prog='def norm1($n):($n|to_entries|map({key:.key,value:(if (.value|type)=="array" then {unnamed:([.value[]|select((has("name")|not))]|length>0),names:([.value[]|select(has("name"))|.name]|unique)} else {unnamed:true,names:[]} end)})|from_entries);
def union($a;$b):reduce ($b|to_entries[]) as $e ($a;(.[$e.key]//{unnamed:false,names:[]}) as $c|.[$e.key]={unnamed:($c.unnamed or $e.value.unnamed),names:(($c.names+$e.value.names)|unique)});
union($state; norm1($incoming))'
    state='{}'
    for scope in $scopes; do
        base=$(cue export ./... -e "${scope}.base.needs" --out json 2>/dev/null || echo '{}')
        [ -n "$base" ] || base='{}'
        state=$(jq -cn --argjson state "$state" --argjson incoming "$base" "$merge_prog" 2>/dev/null) || state="$state"
        # Every environment's needs. We must NOT export the whole
        # `environments` value — that drags in each env's `env` block,
        # whose `claim.*` refs are still the incomplete pass-1 stub and
        # would fail the export. Instead: (a) list env KEYS lazily via an
        # inline comprehension (a list expr does not force the env values
        # concrete), then (b) export each `<scope>.environments.<key>.needs`
        # TARGETED (needs has no claim refs, so it exports cleanly).
        # Env keys land one-per-line in the temp file so the inner read
        # loop runs in THIS shell (a piped while would subshell-lose the
        # accumulating `state`).
        cue export ./... \
            -e "[for k, _ in ${scope}.environments {k}]" \
            --out json 2>/dev/null \
            | jq -r '.[]?' 2>/dev/null > "$envneeds_tmp" || true
        while IFS= read -r envkey; do
            [ -n "$envkey" ] || continue
            en=$(cue export ./... -e "${scope}.environments.${envkey}.needs" --out json 2>/dev/null || echo '{}')
            [ -n "$en" ] || en='{}'
            state=$(jq -cn --argjson state "$state" --argjson incoming "$en" "$merge_prog" 2>/dev/null) || state="$state"
        done < "$envneeds_tmp"
    done
    rm -f "$envneeds_tmp"

    # 5. PASS 2 — emit the real, concrete `claim` binding. The
    #    comprehension runs HERE (manifest package), referencing only
    #    the cross-package #ClaimFieldsFor table. Emits unnamed default
    #    fields when any manifest used the scalar form, plus a sub-struct
    #    per named entry.
    cat > "$CLAIM_GEN" <<CLAIMEOF
package $pkg

import v1alpha1 "apprafter.io/schemas/v1alpha1"

// 2.12f generated claim binding (ADR 0046) — runtime artifact, never
// committed. _apprafterClaimState is the per-type {unnamed, names[]}
// union of effective needs across all manifests + the active env.
_apprafterClaimState: $state

claim: {
	for type, st in _apprafterClaimState if (v1alpha1.#ClaimFieldsFor[type] != _|_) {
		(type): {
			if st.unnamed {
				for f in v1alpha1.#ClaimFieldsFor[type] {(f): {claim: "\(type).\(f)"}}
			}
			for nm in st.names {
				(nm): {for f in v1alpha1.#ClaimFieldsFor[type] {(f): {claim: "\(type).\(nm).\(f)"}}}
			}
		}
	}
}
CLAIMEOF
}

# detect_package — read the `package <name>` clause from the first
# user `.cue` file in cwd (excluding our own generated file and the
# dot/underscore files cue ignores). The generated sibling must match
# it or cue treats them as different packages.
detect_package() {
    for f in ./*.cue; do
        [ -f "$f" ] || continue
        case "$(basename "$f")" in
            "$CLAIM_GEN") continue ;;
            _*|.*) continue ;;
        esac
        p=$(sed -n 's/^[[:space:]]*package[[:space:]]\{1,\}\([A-Za-z_][A-Za-z0-9_]*\).*/\1/p' "$f" | head -n1)
        if [ -n "$p" ]; then printf '%s' "$p"; return 0; fi
    done
    return 1
}

inject_schema_and_claim

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
    # Style A — single manifest at package scope. The 2.12f `claim`
    # binding (Decision #2) is injected as a top-level field too, so for
    # an UNWRAPPED manifest it lands as a SIBLING of apiVersion/kind/spec
    # — i.e. it would leak into the rendered Application as `claim: {…}`.
    # Strip it here (it is the render-time helper, never part of the CR).
    # We round-trip through jq's `del(.claim)`; `del` on an absent key is
    # a no-op, so a manifest with no claim injection is unaffected.
    # `_apprafterClaimState` is a hidden field and never exports, but we
    # drop it defensively too.
    echo "---"
    cue export ./... --out json \
      | jq 'del(.claim) | del(._apprafterClaimState)' \
      | cue export json: - --out yaml \
      | inject_env
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
