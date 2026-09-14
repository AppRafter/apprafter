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
# `cue export . --out yaml` on its own would emit
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

# ── Locate the CUE package directories ─────────────────────
#
# Argo CD sets the working directory to the Application's
# `spec.source.path` (repo-root-relative). An AppRafter manifest
# lives under a path carrying an `apprafter` DIRECTORY component
# or in a file named `apprafter*.cue`, and it CONTAINS an
# AppRafter manifest marker — the convention-AND-content gate
# that `plugin.yaml`'s discover snippet applies (ADR 0063
# §Decisions 1-3). The two must agree: discovery decides the
# repository is ours, this decides which directory inside it is
# the package. That difference in question means this block is
# the discover predicate PLUS two additions, both named at the
# search itself below.
#
# The render has to run from INSIDE the package directory, and
# `cue export` is then invoked as `.` — this cwd's package
# instance — never `./...`. From a PARENT the old recursive form
# reported "matched no packages" whenever the package carried its
# own `cue.mod/` (a MODULE BOUNDARY it will not cross), and where
# it did match it spanned TWO instances and failed
# `reference "<key>" not found`. The full argument for `.` is at
# the export call itself.
#
# Until ADR 0063 this was a single greedy `cd ./apprafter`.
# Measured: a repository holding both a root `apprafter/` and a
# `services/web/apprafter/` rendered ONE document of two, rc=0,
# no warning; and two unwrapped (Style A) package directories
# rendered an EMPTY stream with rc=0 — which under
# `syncPolicy.automated.prune` deletes the whole bundle.
#
# So: enumerate the package directories instead, and REFUSE
# loudly when one registered path holds more than one. Per ADR
# 0062 one registration is one package, so two packages under
# one path is a registration mistake, and the CMP is the only
# layer positioned to see it.
#
# After the per-directory cd (see the driver at the end of this
# file), `cue export .` resolves the module by walking UP
# from that cwd — so this works whether the `cue.mod/` is a
# vendored one inside the package directory (external scaffolded
# repo) OR a repo-root `cue.mod/` shared across many apps (the
# AppRafter monorepo's own landing manifests).

# Hoisted from the 2.12f injection block below so the package search and
# the claim-binding writer agree on one filename.
CLAIM_GEN="apprafter_claim_gen.cue"
AR_MARKER_RE='apprafter\.io/(schemas/)?v1alpha1'

# The `..` rejection, the ancestor arm and the two find predicates below
# are the discover snippet's, verbatim — same variable handling, same
# marker regex (ADR 0063 §Decisions 1-2). Two things are NOT mirrored,
# because discover answers "is this repository ours" and this answers
# "which directory in it is the package":
#
#   * the NESTING rule. Discover takes the first hit at any depth and
#     stops; this has to reduce every hit to a directory set, and a
#     candidate inside another candidate is part of its tree — which is
#     also how "a manifest at depth 1 means cwd IS the package" is
#     expressed, since `.` encloses everything.
#   * the `.` FALLBACK. Where discover finds nothing it prints nothing
#     and declines the repository; here cwd is rendered anyway, which is
#     what keeps the pre-ADR-0063 filename-prefix and bare-fixture
#     layouts working.
#
# Never glob the absolute $PWD: the CMP workdir base is set by
# ARGOCD_CMP_WORKDIR and TMPDIR.
ar_sp="${ARGOCD_APP_SOURCE_PATH:-}"
case "/$ar_sp/" in */../*) ar_sp="" ;; esac
ar_above=0
case "/${ar_sp#/}/" in */apprafter/*) ar_above=1 ;; esac
case "${PWD##*/}" in apprafter) ar_above=1 ;; esac
if [ "$ar_above" = 1 ]; then
    set -- -name '*.cue'
else
    set -- -name '*.cue' \( -path '*/apprafter/*' -o -name 'apprafter*.cue' \)
fi

# ONE search, at every depth, exactly as discover does it. "A manifest
# at depth 1 means cwd IS the package directory" is NOT a second search:
# a depth-1 hit puts `.` in the candidate list, `.` encloses every other
# candidate, and the nesting rule below therefore folds them all into it.
# The short-circuit falls out of the rule instead of duplicating it —
# and unlike a separate first pass, it still SEES what lies below, which
# is what lets a nested bundle be named rather than silently dropped.
#
# `set -e`-hardened TWICE over, deliberately. `find -exec grep -l … +`
# exits NON-ZERO when the last grep matched nothing (plugin.yaml carries
# the same note — it pays for it with an explicit `exit 0`), and that is
# reachable here: a `.cue` satisfying the name predicate but carrying no
# marker, e.g. an `apprafter-notes.cue` at depth 1. An assignment whose
# command substitution fails is itself a failing command, so under
# `set -e` the render would die with rc=1, no stdout and NO stderr —
# measured, and indistinguishable from a successful empty render, which
# prunes. Today the `| sort` already masks it (a pipeline's status is
# its LAST component's), so the `|| true` is the belt to the sort's
# braces: it keeps the hardening true of the assignment itself,
# independent of the pipeline's shape.
ar_hits=$(find . -type f ! -path '*/cue.mod/*' ! -name "$CLAIM_GEN" "$@" \
            -exec grep -lE "$AR_MARKER_RE" {} + 2>/dev/null | sort) || true
set --   # generate is invoked with no arguments; reset after use

ar_dirs=$(printf '%s\n' "$ar_hits" | sed -n 's|/[^/]*$||p' | sort -u)
[ -n "$ar_dirs" ] || ar_dirs="."

# Drop every candidate NESTED under another candidate. A directory
# inside a package directory belongs to that package's tree: a helper
# sub-package — `apprafter/lib/`, imported by `apprafter/Application.cue`
# and carrying the marker through its own schema import — is part of the
# bundle, not a second one. ADR 0029 contemplates exactly this layout for
# monorepos with shared CUE. Only genuine SIBLINGS are an ambiguous
# registration.
#
# Pairwise, not a single pass down the sorted list: lexicographic order
# does NOT put an ancestor next to its descendants. `./apprafter`,
# `./apprafter-x`, `./apprafter/lib` sort in that order because `-`
# (0x2D) precedes `/` (0x2F), so a running-root scan would adopt
# `./apprafter-x` as the root and then keep `./apprafter/lib` — and
# LC_COLLATE can reorder it further still. n is the number of manifest
# directories under ONE registration, so pairwise costs nothing.
#
# Neither loop is a pipe, so `ar_kept` accumulates in THIS shell; the
# inner here-doc redirects only the inner loop's stdin, leaving the
# outer loop reading its own.
# Folding is silent for a helper, LOUD for a nested bundle. The two are
# indistinguishable by the marker alone — both are `.cue` under the
# package directory carrying `apprafter.io/…v1alpha1` — so the notice is
# gated on a second, narrower pattern that asks whether the nested
# directory would itself have RENDERED something: a literal Style-A
# `apiVersion: "apprafter.io/v1alpha1"`, or an instantiation of
# `#Application`. `#Application([^A-Za-z]|$)` deliberately excludes
# `#ApplicationSpec`, which a helper constraining shared values against
# the schema will legitimately mention.
#
# The residual error is one-directional by construction: a helper that
# happens to reference `#Application` (a reusable base, say) draws one
# extra informational line, while a real manifest — which must declare
# apiVersion/kind or instantiate #Application to be one at all — cannot
# slip through unnamed. Noise over silence, because the operator reading
# this is looking for a workload that never appeared.
AR_BUNDLE_RE='apiVersion:[[:space:]]*"apprafter\.io/v1alpha1"|#Application([^A-Za-z]|$)'

ar_kept=""
ar_folded=""
while IFS= read -r ar_c; do
    [ -n "$ar_c" ] || continue
    ar_parent=""
    while IFS= read -r ar_p; do
        if [ -n "$ar_p" ] && [ "$ar_p" != "$ar_c" ]; then
            # "$ar_p" is QUOTED inside the pattern, so it matches
            # literally — a directory name may contain glob characters.
            case "$ar_c" in
                "$ar_p"/*)
                    # Keep the SHORTEST enclosing candidate. Nothing
                    # encloses it in turn — a shorter match would have
                    # been found here too — so it is guaranteed to be one
                    # of the KEPT directories, and the notice can never
                    # point at a directory that was itself folded away.
                    if [ -z "$ar_parent" ] || [ "${#ar_p}" -lt "${#ar_parent}" ]; then
                        ar_parent=$ar_p
                    fi
                    ;;
            esac
        fi
    done <<AR_NEST_EOF
$ar_dirs
AR_NEST_EOF
    if [ -n "$ar_parent" ]; then
        if find "$ar_c" -maxdepth 1 -type f -name '*.cue' ! -name "$CLAIM_GEN" \
             -exec grep -lE "$AR_BUNDLE_RE" {} + >/dev/null 2>&1; then
            ar_folded="${ar_folded}  ${ar_c}  (inside ${ar_parent})
"
        fi
    else
        ar_kept="${ar_kept}${ar_c}
"
    fi
done <<AR_CAND_EOF
$ar_dirs
AR_CAND_EOF
ar_dirs=$(printf '%s' "$ar_kept" | sed -n '/./p')
[ -n "$ar_dirs" ] || ar_dirs="."

# A successful render's stderr still reaches the Argo CD sync log, which
# is where an operator looks when a workload they expected is missing.
# Silence there was the one thing ADR 0063 §Decision 3 set out to stop:
# its whole argument is that a silent partial render is the worse state,
# and folding a nested bundle away without a word is exactly that shape.
# Not a prune vector — every revision before this one failed outright on
# the layout, so no cluster ever had those workloads applied from this
# registration — but "it never deployed" still deserves a sentence.
if [ -n "$ar_folded" ]; then
    echo "::cue-cmp:: nested manifest directories folded into their enclosing package:" >&2
    printf '%s' "$ar_folded" >&2
    echo "::cue-cmp:: rendering only the enclosing package — nothing from a nested directory was applied." >&2
    echo "::cue-cmp:: if a nested directory is its own bundle, register it with \`apprafter app add --path\`." >&2
fi

ar_ndirs=$(printf '%s\n' "$ar_dirs" | grep -c . || true)
if [ "${ar_ndirs:-0}" -gt 1 ]; then
    echo "::cue-cmp:: this path holds ${ar_ndirs} manifest packages; one registration is one package" >&2
    echo "" >&2
    echo "--- apprafter bundle check ---" >&2
    printf '%s\n' "$ar_dirs" | sed 's|^|  |' >&2
    echo "" >&2
    echo "Nothing from this path was applied; the resources already running" >&2
    echo "are untouched. Point spec.source.path at a single package, or" >&2
    echo "register each with its own \`apprafter app add --path\`." >&2
    exit 1
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
# earlier `cue export .` succeeded), so a failure here is
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
# (cwd), so `cue export .` below resolves the markers. Likewise we
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
#     ENV-AGNOSTIC on purpose — `cue export .` evaluates every env
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
# (CLAIM_GEN is set once, up in the package-location block — the search
# there must exclude exactly the file this writes.)
SCHEMA_SRC="${APPRAFTER_SCHEMA_SRC:-/opt/apprafter/schema/v1alpha1}"
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
    names=$(cue def . 2>/dev/null \
        | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\):.*/\1/p' \
        | grep -vxE '_N|claim|apiVersion|kind|metadata|spec' || true)
    for name in $names; do
        scopes="$scopes ${name}.spec"
    done

    # 4. Collect the per-type {unnamed, names[]} union across all
    #    manifests' base.needs AND **every** environments[*].needs.
    #
    #    Why ALL environments, not just the active one (APPRAFTER_APP_ENV)?
    #    `cue export .` evaluates the WHOLE manifest, including every
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
        base=$(cue export . -e "${scope}.base.needs" --out json 2>/dev/null || echo '{}')
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
        cue export . \
            -e "[for k, _ in ${scope}.environments {k}]" \
            --out json 2>/dev/null \
            | jq -r '.[]?' 2>/dev/null > "$envneeds_tmp" || true
        while IFS= read -r envkey; do
            [ -n "$envkey" ] || continue
            en=$(cue export . -e "${scope}.environments.${envkey}.needs" --out json 2>/dev/null || echo '{}')
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

# Use temp files so we can capture both the JSON body and
# any stderr without merging streams (Argo CD reads stdout
# for manifests; stderr is the diagnostic surface).
#
# Created ONCE, above render_package, and reused by every call: a
# `trap … EXIT` re-registered per invocation would leak one pair of
# temp files per package directory. `render_package` only ever
# truncates them with `>`, so sharing is safe.
json_out=$(mktemp)
err_out=$(mktemp)
trap 'rm -f "$json_out" "$err_out"' EXIT

# ── render_package — render ONE package directory ──────────
#
# Runs with cwd already set to the package directory by the driver at
# the foot of this file. Everything from the schema/claim injection to
# the per-key emit loop is per-directory work, so it all lives here.
#
# Control flow contract: this function never calls `exit` in its own
# shell — it `return`s, and the driver's BARE `( … )` lets `set -e` turn
# a non-zero return into the script's exit status. The ONE deliberate
# exception is documented at the per-key loop below.
render_package() {
    inject_schema_and_claim

    # `.` evaluates exactly ONE package instance: the one in the
    # current directory, which the driver has already cd'd into.
    # Imports resolve through the nearest `cue.mod/module.cue` CUE
    # finds walking upward — the vendored module inside `apprafter/`
    # for scaffolded repos, or a repo-root module for the monorepo.
    # `cue.mod/pkg/` is the dependency cache and is never evaluated,
    # so the vendored schema package is not emitted as a manifest.
    #
    # NOT `./...`, and that is load-bearing. `./...` matches every
    # package instance BELOW cwd as well, and a package directory may
    # legitimately hold a helper sub-package — `apprafter/lib/`,
    # imported by `apprafter/Application.cue`, the shared-CUE monorepo
    # layout ADR 0029 contemplates. Measured on exactly that layout:
    # `./...` emits TWO JSON documents and then fails
    # `cue export ./... -e <key>` with `reference "<key>" not found`,
    # because `-e` is evaluated against the wrong instance; and two
    # UNWRAPPED instances make the dispatch below read the two-line
    # string "yes\nyes", fall through to the Style-B branch and emit
    # nothing at all. A CUE package is one directory, so `.` is also
    # the semantically exact request — this is what ADR 0063 means by
    # rendering each directory in its own working directory.
    #
    # JSON intermediate (rather than YAML) keeps key extraction
    # trivial via `jq`.
    if ! cue export . --out json >"$json_out" 2>"$err_out"; then
        summary=$(awk 'NF { print; exit }' "$err_out")
        echo "::cue-cmp:: CUE compile failed: ${summary}" >&2
        echo "" >&2
        echo "--- full cue export stderr ---" >&2
        cat "$err_out" >&2
        return 1
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
        cue export . --out json \
          | jq 'del(.claim) | del(._apprafterClaimState)' \
          | cue export json: - --out yaml \
          | inject_env
        return 0
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
        return 0
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
        if ! doc=$(cue export . -e "$key" --out yaml 2>"$err_out"); then
            # Surface the per-key error to stderr so operators can
            # locate the failing manifest. Single-manifest failure
            # aborts the whole sync — keeping stricter "all or
            # nothing" semantics is safer than partial application.
            summary=$(awk 'NF { print; exit }' "$err_out")
            echo "::cue-cmp:: failed exporting '${key}': ${summary}" >&2
            echo "--- full cue export -e ${key} stderr ---" >&2
            cat "$err_out" >&2
            # `exit 1`, NOT `return 1` — and that is deliberate. The
            # right-hand side of a pipe is a SUBSHELL, so this loop body
            # is not running in render_package's shell; `return` here
            # would not return from render_package. `exit 1` leaves the
            # subshell with status 1, which becomes the pipeline's
            # status, which — the pipeline being render_package's last
            # command — becomes the function's return value. It also
            # abandons the remaining keys, which is the "all or nothing"
            # semantics the comment above promises.
            exit 1
        fi
        printf '%s\n' "$doc" | inject_env
    done
}

# ── Driver — render every located package directory ────────
#
# The ambiguity refusal in the package-location block above means
# `$ar_dirs` holds exactly ONE entry today, so this loop runs at most
# once. It is written as a loop anyway so the single-directory case is
# expressed as the degenerate case of the general one: relaxing the
# refusal (ADR 0063 leaves that door open — the enumeration is the point,
# the refusal is the current policy) needs no restructuring here.
#
# Two shapes below are load-bearing, both measured under bash AND under
# busybox ash 1.36.1, the sidecar's actual shell:
#
#   * The loop is fed by a HERE-DOCUMENT, not by `printf … | while`.
#     The right-hand side of a pipe is a subshell, so an `exit` inside
#     it exits only that subshell; it reaches the script's status solely
#     by being the pipeline's last component, and any command appended
#     after the loop later would silently swallow a render failure. A
#     here-doc keeps the loop in THIS shell, where `exit` means exit.
#
#   * `( cd … && render_package )` is run BARE, never as
#     `( … ) || exit 1`. Putting it on the left of `||` makes it a
#     condition context, and POSIX then ignores `set -e` for the whole
#     compound INCLUDING the function body — measured: an unchecked
#     failing command inside render_package stops aborting, execution
#     continues past it, and the script exits 0 having flushed a partial
#     manifest stream to stdout. Under `syncPolicy.automated.prune` that
#     is precisely the destructive outcome this change exists to remove.
#     Bare, `set -e` propagates the subshell's non-zero status and the
#     script dies with it.
#
# `</dev/null` so nothing inside the render can consume the directory
# list the loop is still reading.
ar_root=$PWD
while IFS= read -r ar_d; do
    [ -n "$ar_d" ] || continue
    ( cd "$ar_root/$ar_d" && render_package ) </dev/null
done <<AR_DIRS_EOF
$ar_dirs
AR_DIRS_EOF
