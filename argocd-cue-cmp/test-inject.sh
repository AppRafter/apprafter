#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# Regression test for the APPRAFTER_APP_ENV injection in
# `argocd-cue-cmp/entrypoint.sh` (subphase 2.9, ADR 0044).
#
# Per-environment deploy makes the CUE CMP inject
# `spec.environment` AND the `apprafter.io/environment` label
# into every rendered Application manifest WHEN the env var
# `APPRAFTER_APP_ENV` is set (the CLI `app add --env` sets it
# via the Argo Application's spec.source.plugin.env). When the
# var is unset the entrypoint must emit the manifest unchanged
# (base-only). The entrypoint's stdout is the manifest stream
# Argo CD consumes, so it must stay PURE manifest YAML — every
# diagnostic goes to stderr.
#
# This test invokes entrypoint.sh from inside two fixture
# repos and asserts:
#
#   * Style A (single unwrapped manifest):
#       - with APPRAFTER_APP_ENV=dev → stdout carries
#         `environment: dev` AND `apprafter.io/environment: dev`.
#       - without the var → neither marker appears.
#   * Style B (named-wrapper, two manifests in one file): with
#     APPRAFTER_APP_ENV=dev the injection lands on BOTH emitted
#     documents (one `apprafter.io/environment: dev` label per
#     manifest), proving the per-key re-export loop injects
#     every doc, not just the first.
#
# Wired into `.github/workflows/argocd-cue-cmp-check.yml`
# alongside test-discover.sh; runs locally too via
# `bash test-inject.sh`.

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)
entrypoint="$script_dir/entrypoint.sh"
if [[ ! -f "$entrypoint" ]]; then
    echo "FAIL: $entrypoint not found" >&2
    exit 1
fi

# Resolve a usable `cue`: prefer one on PATH, fall back to the
# repo convention `nix run nixpkgs#cue --`. entrypoint.sh calls
# bare `cue`, so when only the nix form is available we shim a
# `cue` wrapper onto PATH for the duration of the test.
shim_dir=""
if ! command -v cue >/dev/null 2>&1; then
    if command -v nix >/dev/null 2>&1; then
        shim_dir=$(mktemp -d)
        cat > "$shim_dir/cue" <<'SHIM'
#!/usr/bin/env bash
exec nix run nixpkgs#cue -- "$@"
SHIM
        chmod +x "$shim_dir/cue"
        export PATH="$shim_dir:$PATH"
    else
        echo "FAIL: no 'cue' on PATH and no 'nix' to fall back to." >&2
        exit 1
    fi
fi

cleanup() { [[ -n "$shim_dir" ]] && rm -rf "$shim_dir"; return 0; }
trap cleanup EXIT

pass=0
fail=0

assert_contains() {
    local haystack=$1 needle=$2 label=$3
    if printf '%s\n' "$haystack" | grep -Fq -- "$needle"; then
        echo "PASS: $label"
        pass=$((pass + 1))
    else
        echo "FAIL: $label — expected to find '$needle'"
        echo "--- stdout was ---" >&2
        printf '%s\n' "$haystack" >&2
        fail=$((fail + 1))
    fi
}

assert_absent() {
    local haystack=$1 needle=$2 label=$3
    if printf '%s\n' "$haystack" | grep -Fq -- "$needle"; then
        echo "FAIL: $label — did not expect '$needle'"
        echo "--- stdout was ---" >&2
        printf '%s\n' "$haystack" >&2
        fail=$((fail + 1))
    else
        echo "PASS: $label"
        pass=$((pass + 1))
    fi
}

# Asserts SEQUENCE, not just presence: $2 must appear before $3.
# Both layers derive their workload order from the same exported JSON
# document, so a divergence here is a reader meeting one finding twice
# in two different sequences.
#
# Offsets in the WHOLE text, not line numbers — the refusal summary
# carries both workloads on its first line (that line is what Argo CD
# truncates onto the Application tile), so a line-wise comparison would
# read them as tied.
#
# awk, not `grep -n | head`: grep exits non-zero on no match and head's
# SIGPIPE would turn a match into a miss under `pipefail`. `index`
# returns 0 for absent, which fails the comparison below rather than
# passing vacuously.
assert_before() {
    local haystack=$1 first=$2 second=$3 label=$4
    local first_at second_at
    first_at=$(printf '%s\n' "$haystack" | awk -v n="$first" '{b = b $0 "\n"} END {print index(b, n)}')
    second_at=$(printf '%s\n' "$haystack" | awk -v n="$second" '{b = b $0 "\n"} END {print index(b, n)}')
    if [[ "$first_at" -gt 0 && "$second_at" -gt 0 && "$first_at" -lt "$second_at" ]]; then
        echo "PASS: $label (offset $first_at before offset $second_at)"
        pass=$((pass + 1))
    else
        echo "FAIL: $label — expected '$first' (at ${first_at}) before '$second' (at ${second_at}); 0 means absent"
        echo "--- stdout was ---" >&2
        printf '%s\n' "$haystack" >&2
        fail=$((fail + 1))
    fi
}

# `assert_before` over a whole ROSTER: every needle present, each one
# strictly after the one before it.
#
# Pairwise `assert_before` calls would cover the same ground, but a
# four-workload sequence is one rule, and reporting it as one PASS (or
# as the single pair that broke) is what makes a reordering readable.
# Same `awk index` offsets, for the same reason stated there.
assert_sequence() {
    local haystack=$1 label=$2
    shift 2
    local prev_at=0 prev="" needle at why=""
    for needle in "$@"; do
        at=$(printf '%s\n' "$haystack" | awk -v n="$needle" '{b = b $0 "\n"} END {print index(b, n)}')
        if [[ "$at" -eq 0 ]]; then
            why="'$needle' is absent"
            break
        fi
        if [[ "$at" -le "$prev_at" ]]; then
            why="'$needle' (at ${at}) does not follow '$prev' (at ${prev_at})"
            break
        fi
        prev_at=$at
        prev=$needle
    done
    if [[ -z "$why" ]]; then
        echo "PASS: $label ($# in sequence)"
        pass=$((pass + 1))
    else
        echo "FAIL: $label — $why"
        echo "--- stdout was ---" >&2
        printf '%s\n' "$haystack" >&2
        fail=$((fail + 1))
    fi
}

assert_count() {
    local haystack=$1 needle=$2 want=$3 label=$4
    local got
    got=$(printf '%s\n' "$haystack" | grep -Fc -- "$needle" || true)
    if [[ "$got" -eq "$want" ]]; then
        echo "PASS: $label (found $got)"
        pass=$((pass + 1))
    else
        echo "FAIL: $label — expected $want occurrences of '$needle', got $got"
        echo "--- stdout was ---" >&2
        printf '%s\n' "$haystack" >&2
        fail=$((fail + 1))
    fi
}

# entrypoint.sh cd's into ./apprafter itself, so run it from the
# fixture ROOT (the parent of apprafter/), matching how Argo CD's
# repo-server invokes it when source.path points at the repo root.
#
# $3 selects which env-var NAME carries the value:
#   "bare"   → APPRAFTER_APP_ENV (direct invocation; an operator
#              running entrypoint.sh by hand).
#   "argocd" → ARGOCD_ENV_APPRAFTER_APP_ENV — the ONLY form a real
#              Argo CD repo-server passes a `spec.source.plugin.env`
#              var under (CMP vars are exposed PREFIXED with
#              `ARGOCD_ENV_` since Argo CD v2.4). The entrypoint MUST
#              honour this form or the in-cluster injection is inert
#              (the e2e per-env walk caught exactly that regression).
# Default "bare" keeps existing call sites unchanged.
# $4 (optional) — APPRAFTER_SCHEMA_SRC for the entrypoint's 2.12f
# schema + `claim` binding injection. On a real cluster the schema is
# bundled at /opt/apprafter/schema/v1alpha1; off-cluster (here) we
# point it at the repo's schemas/v1alpha1 so the claim/secret path is
# exercised on the host too (the 2.9 lesson: don't let host tests mask
# the real injection path). Left empty for the legacy standalone
# fixtures, whose inject step then no-ops (no schema → early return).
run_entrypoint() {  # $1 fixture root, $2 env value ("" = unset), $3 var style (bare|argocd), $4 schema_src
    local root=$1 env_val=$2 style=${3:-bare} schema_src=${4:-}
    if [[ -z "$env_val" ]]; then
        ( cd "$root" && unset APPRAFTER_APP_ENV ARGOCD_ENV_APPRAFTER_APP_ENV \
            && APPRAFTER_SCHEMA_SRC="$schema_src" bash "$entrypoint" )
    elif [[ "$style" == "argocd" ]]; then
        ( cd "$root" && unset APPRAFTER_APP_ENV \
            && APPRAFTER_SCHEMA_SRC="$schema_src" ARGOCD_ENV_APPRAFTER_APP_ENV="$env_val" bash "$entrypoint" )
    else
        ( cd "$root" && unset ARGOCD_ENV_APPRAFTER_APP_ENV \
            && APPRAFTER_SCHEMA_SRC="$schema_src" APPRAFTER_APP_ENV="$env_val" bash "$entrypoint" )
    fi
}

style_a="$script_dir/testdata/inject-fixture"
style_b="$script_dir/testdata/inject-fixture-multi"

# ── Style A, var SET → injection present ───────────────────
out=$(run_entrypoint "$style_a" "dev")
assert_contains "$out" "environment: dev" "Style A: spec.environment injected when APPRAFTER_APP_ENV=dev"
assert_contains "$out" "apprafter.io/environment: dev" "Style A: apprafter.io/environment label injected when APPRAFTER_APP_ENV=dev"
# The base manifest must survive the round-trip intact.
assert_contains "$out" "name: inject-fixture" "Style A: base metadata.name preserved through injection"
assert_contains "$out" "image: nginxdemos/hello:plain-text" "Style A: base spec.base.image preserved through injection"

# ── Style A, Argo CD PREFIXED var SET → injection present ──
# Argo CD's repo-server passes a `spec.source.plugin.env` var named
# APPRAFTER_APP_ENV to the generate command as
# ARGOCD_ENV_APPRAFTER_APP_ENV (the v2.4+ CMP hardening). This is the
# form that ACTUALLY reaches the entrypoint in-cluster — the bare
# APPRAFTER_APP_ENV is never set by Argo. Reading only the bare name
# made the in-cluster injection silently inert; this case is the
# regression guard for that (e2e per-env walk found, 2.9).
out_argocd=$(run_entrypoint "$style_a" "dev" "argocd")
assert_contains "$out_argocd" "environment: dev" "Style A: spec.environment injected when ARGOCD_ENV_APPRAFTER_APP_ENV=dev (Argo CD prefixed form)"
assert_contains "$out_argocd" "apprafter.io/environment: dev" "Style A: env label injected when ARGOCD_ENV_APPRAFTER_APP_ENV=dev (Argo CD prefixed form)"

# ── Style A, var UNSET → manifest unchanged ────────────────
out_base=$(run_entrypoint "$style_a" "")
assert_absent "$out_base" "environment:" "Style A: no spec.environment when APPRAFTER_APP_ENV unset"
assert_absent "$out_base" "apprafter.io/environment" "Style A: no environment label when APPRAFTER_APP_ENV unset"
assert_contains "$out_base" "name: inject-fixture" "Style A: base manifest still emitted when var unset"

# ── Style B (multi-doc), var SET → injection on EVERY doc ──
out_multi=$(run_entrypoint "$style_b" "dev")
assert_count "$out_multi" "apprafter.io/environment: dev" 2 "Style B: label injected on BOTH emitted manifests"
assert_count "$out_multi" "environment: dev" 4 "Style B: environment marker present per doc (spec.environment + label, x2)"
assert_contains "$out_multi" "name: inject-multi-one" "Style B: first manifest still emitted"
assert_contains "$out_multi" "name: inject-multi-two" "Style B: second manifest still emitted"

# ── Style B, var UNSET → both docs unchanged ───────────────
out_multi_base=$(run_entrypoint "$style_b" "")
assert_absent "$out_multi_base" "apprafter.io/environment" "Style B: no environment label when var unset"
assert_count "$out_multi_base" "name: inject-multi-" 2 "Style B: both base manifests still emitted when var unset"

# ── 2.12f: claim + external-secret value references (ADR 0046) ──
#
# The claim fixture's manifest uses BARE `claim.pg.url` /
# `claim.pg.host` / `claim.pg.main.url` selectors and the braceless
# `secret: "stripe/api-key"` form, and vendors NEITHER the schema nor
# a `claim` binding. The entrypoint must lay down the bundled schema +
# generate the `claim` binding so these render to {claim: "..."} /
# {secret: "..."} markers. We point APPRAFTER_SCHEMA_SRC at the repo's
# schemas/v1alpha1 to mimic the image's /opt bundle off-cluster.
#
# CRITICAL (2.9 lesson re-applied): exercise the IN-CLUSTER var form.
# Argo CD passes plugin env PREFIXED as ARGOCD_ENV_APPRAFTER_APP_ENV,
# so the per-env assertion uses the "argocd" style — a host test that
# only set the bare name would mask an in-cluster injection bug.
schema_src="$(cd "$script_dir/../schemas/v1alpha1" && pwd)"
claim_fx="$script_dir/testdata/inject-fixture-claim"

# The injection writes a cue.mod/ + apprafter_claim_gen.cue into the
# fixture (ephemeral on a real cluster; here we must scrub them so the
# committed fixture stays clean and reruns are deterministic).
scrub_claim_fixture() {
    rm -rf "$claim_fx/apprafter/cue.mod" "$claim_fx/apprafter/apprafter_claim_gen.cue"
}
trap 'cleanup; scrub_claim_fixture' EXIT
scrub_claim_fixture

# Base-only (var unset): EVERY env's claim refs must still resolve
# (the binding unions base + all environments' needs), so the
# environments.dev.env.REDIS_URL=claim.redis.url marker renders even
# though redis is declared only in the dev env.
out_claim_base=$(run_entrypoint "$claim_fx" "" "bare" "$schema_src")
scrub_claim_fixture
assert_contains "$out_claim_base" "claim: pg.url"           "2.12f: claim.pg.url -> {claim: pg.url}"
assert_contains "$out_claim_base" "claim: pg.host"          "2.12f: claim.pg.host -> {claim: pg.host}"
assert_contains "$out_claim_base" "claim: pg.main.url"      "2.12f: named claim.pg.main.url -> {claim: pg.main.url}"
assert_contains "$out_claim_base" "secret: stripe/api-key"  "2.12f: secret: \"stripe/api-key\" -> {secret: stripe/api-key}"
assert_contains "$out_claim_base" "claim: redis.url"        "2.12f: per-env claim.redis.url resolves base-only (env-agnostic union)"
assert_contains "$out_claim_base" "LOG_LEVEL: info"         "2.12f: literal env value preserved"
# The generated `claim` binding must NOT leak into the manifest stream.
assert_absent  "$out_claim_base" "_apprafterClaimState"     "2.12f: generated claim state not emitted as a manifest"
assert_absent  "$out_claim_base" "_N:"                      "2.12f: pass-1 stub not emitted"

# In-cluster per-env form (ARGOCD_ENV_ prefix): same markers PLUS the
# env stamp from the 2.9 path — proving the two injections compose.
out_claim_dev=$(run_entrypoint "$claim_fx" "dev" "argocd" "$schema_src")
scrub_claim_fixture
assert_contains "$out_claim_dev" "claim: pg.url"               "2.12f (ARGOCD_ENV_): claim.pg.url resolves"
assert_contains "$out_claim_dev" "claim: redis.url"            "2.12f (ARGOCD_ENV_): per-env redis claim resolves"
assert_contains "$out_claim_dev" "secret: stripe/api-key"      "2.12f (ARGOCD_ENV_): secret ref resolves"
assert_contains "$out_claim_dev" "environment: dev"            "2.12f (ARGOCD_ENV_): 2.9 env stamp composes with claim injection"
assert_contains "$out_claim_dev" "apprafter.io/environment: dev" "2.12f (ARGOCD_ENV_): env label composes with claim injection"

# ── 2.12f Style-A (unwrapped) claim leak regression ────────
# For an UNWRAPPED manifest the injected top-level `claim` binding is a
# SIBLING of apiVersion/kind/spec, so without the Style-A strip it
# would render INTO the Application as `claim: {…}`. Guard: selectors
# resolve AND `claim:`/`_apprafterClaimState` never appear as a CR key.
claima_fx="$script_dir/testdata/inject-fixture-claim-styleA"
scrub_claima_fixture() {
    rm -rf "$claima_fx/apprafter/cue.mod" "$claima_fx/apprafter/apprafter_claim_gen.cue"
}
trap 'cleanup; scrub_claim_fixture; scrub_claima_fixture' EXIT
scrub_claima_fixture
out_claima=$(run_entrypoint "$claima_fx" "" "bare" "$schema_src")
scrub_claima_fixture
assert_contains "$out_claima" "claim: pg.url"          "2.12f Style-A: bare claim.pg.url resolves in unwrapped manifest"
assert_contains "$out_claima" "secret: stripe/api-key" "2.12f Style-A: secret ref resolves in unwrapped manifest"
assert_contains "$out_claima" "name: style-a-claim"    "2.12f Style-A: manifest still emitted"
# The leak guard: a top-level `claim:` CR key (2-space indent, value {})
# must NOT appear. We check the specific leak shape `claim: {}` plus the
# helper state field.
assert_absent  "$out_claima" "_apprafterClaimState"    "2.12f Style-A: claim state helper not leaked"
if printf '%s\n' "$out_claima" | grep -Eq '^claim:'; then
    echo "FAIL: 2.12f Style-A: top-level claim binding leaked into the Application CR"
    printf '%s\n' "$out_claima" >&2
    fail=$((fail + 1))
else
    echo "PASS: 2.12f Style-A: top-level claim binding stripped from the CR"
    pass=$((pass + 1))
fi

# ── ADR 0063 §Decision 3: a bundle may span directories ────
#
# `split-bundle/` holds TWO independent bundles, `services/api/apprafter/`
# and `services/web/apprafter/`. Registered one at a time each must
# render on its own; registered TOGETHER at the repository root they are
# an ambiguous path and the render must REFUSE.
#
# Before this change the single greedy `cd ./apprafter` at
# entrypoint.sh:84-88 saw no `./apprafter` at the repository root, left
# cwd there, and `cue export ./... -e api` evaluated against BOTH
# package instances → `reference "api" not found`. Non-zero, but the
# message names nothing the reader can act on.
split_fx="$script_dir/testdata/split-bundle"
# This fixture is rendered from THREE working directories (services/api,
# services/web, and the ambiguous root), so the injected artefacts can
# land at any of them — a pre-ADR-0063 entrypoint stayed at the root and
# injected there. Scrub by search rather than by a fixed list.
scrub_split_fixture() {
    find "$split_fx" \( -name cue.mod -type d -o -name apprafter_claim_gen.cue -type f \) \
        -prune -exec rm -rf {} +
}
trap 'cleanup; scrub_claim_fixture; scrub_claima_fixture; scrub_split_fixture' EXIT
scrub_split_fixture

# Capture stdout, stderr AND the exit code without aborting the harness
# (`set -e` would kill us on the expected non-zero run). stdout goes to a
# FILE, not a `$(…)` capture, so "empty" is measured in BYTES — `$(…)`
# strips trailing newlines and would read a stream of bare `---`
# separators as empty.
ep_rc=0; ep_out=""; ep_err=""; ep_out_bytes=0
run_entrypoint_capture() {  # $1 fixture root, $2 schema_src, $3 ARGOCD_APP_SOURCE_PATH ("" = unset)
    local root=$1 schema_src=${2:-} source_path=${3:-} out_file err_file
    out_file=$(mktemp)
    err_file=$(mktemp)
    set +e
    if [[ -n "$source_path" ]]; then
        ( cd "$root" \
            && unset APPRAFTER_APP_ENV ARGOCD_ENV_APPRAFTER_APP_ENV \
            && APPRAFTER_SCHEMA_SRC="$schema_src" ARGOCD_APP_SOURCE_PATH="$source_path" \
               bash "$entrypoint" ) \
            >"$out_file" 2>"$err_file"
    else
        ( cd "$root" \
            && unset APPRAFTER_APP_ENV ARGOCD_ENV_APPRAFTER_APP_ENV ARGOCD_APP_SOURCE_PATH \
            && APPRAFTER_SCHEMA_SRC="$schema_src" bash "$entrypoint" ) \
            >"$out_file" 2>"$err_file"
    fi
    ep_rc=$?
    set -e
    ep_out=$(cat "$out_file")
    ep_err=$(cat "$err_file")
    ep_out_bytes=$(wc -c < "$out_file" | tr -d '[:space:]')
    rm -f "$out_file" "$err_file"
}

assert_rc() {  # $1 got, $2 want-shape (zero|nonzero), $3 label
    local got=$1 want=$2 label=$3
    if [[ "$want" == "zero" && "$got" -eq 0 ]] || [[ "$want" == "nonzero" && "$got" -ne 0 ]]; then
        echo "PASS: $label (rc=$got)"
        pass=$((pass + 1))
    else
        echo "FAIL: $label — expected $want exit, got rc=$got"
        fail=$((fail + 1))
    fi
}

# Reads `ep_out_bytes` from the last run_entrypoint_capture. Measured in
# BYTES, not via `[[ -z "$ep_out" ]]`: a `$(…)` capture strips trailing
# newlines and would read a stream of bare `---` separators as empty.
assert_stdout_empty() {  # $1 label
    local label=$1
    if [[ "$ep_out_bytes" -eq 0 ]]; then
        echo "PASS: $label (0 bytes)"
        pass=$((pass + 1))
    else
        echo "FAIL: $label — wrote $ep_out_bytes bytes to stdout; a partial stream prunes"
        printf '%s\n' "$ep_out" >&2
        fail=$((fail + 1))
    fi
}

# ── ADR 0063: `services/api` registered alone renders ──────
run_entrypoint_capture "$split_fx/services/api" "$schema_src"
scrub_split_fixture
assert_rc "$ep_rc" zero "ADR 0063: services/api alone renders (exit 0)"
assert_count "$ep_out" "---" 1 "ADR 0063: services/api alone emits exactly ONE document"
assert_contains "$ep_out" "name: split-api" "ADR 0063: services/api alone emits the api manifest"
assert_absent "$ep_out" "split-web" "ADR 0063: services/api alone does not drag in the web sibling"

# ── ADR 0063: `services/web` registered alone renders ──────
run_entrypoint_capture "$split_fx/services/web" "$schema_src"
scrub_split_fixture
assert_rc "$ep_rc" zero "ADR 0063: services/web alone renders (exit 0)"
assert_count "$ep_out" "---" 1 "ADR 0063: services/web alone emits exactly ONE document"
assert_contains "$ep_out" "name: split-web" "ADR 0063: services/web alone emits the web manifest"

# ── ADR 0063: the repository root is an AMBIGUOUS path ─────
#
# All THREE assertions matter, though not for the reason the 0-byte one
# might suggest. Argo CD does NOT read stdout when generate exits
# non-zero (v2.13.1 `cmpserver/plugin/plugin.go` discards it on error),
# so a partial stream BEHIND a non-zero exit is not itself the pruning
# vector — rc=0 with an empty stream is, and the rc assertion covers
# that. What the 0-byte assertion pins is narrower and more durable:
# that the refusal happens BEFORE anything is emitted, so the guarantee
# holds on this side of the contract and does not depend on Argo CD's
# error handling staying as it is.
#
# Stated asymmetry, so the gap is on the record rather than implied: the
# per-key export failure path is NOT held to this and nothing asserts
# against it. It emits `---`, then every document that preceded the
# failure, then exits 1 — measured at 164 bytes on the
# inject-fixture-multi layout with the second key's export forced to
# fail. That is safe only because rc≠0, i.e. it leans on exactly the
# Argo CD behaviour the refusal path deliberately does not lean on.
run_entrypoint_capture "$split_fx" "$schema_src"
scrub_split_fixture
assert_rc "$ep_rc" nonzero "ADR 0063: repository root holding two bundles REFUSES"
if [[ "$ep_out_bytes" -eq 0 ]]; then
    echo "PASS: ADR 0063: ambiguous path writes NOTHING to stdout (0 bytes)"
    pass=$((pass + 1))
else
    echo "FAIL: ADR 0063: ambiguous path wrote $ep_out_bytes bytes to stdout — a partial stream prunes"
    printf '%s\n' "$ep_out" >&2
    fail=$((fail + 1))
fi
assert_contains "$ep_err" "manifest packages" "ADR 0063: the refusal names the ambiguity on stderr"
# Argo CD truncates stderr onto the Application tile at the first line, so
# the directory list on line 4 is off-tile. This is the refusal most likely
# to fire on upgrade — it has to be actionable from line 1 alone, the same
# contract the four bundle_refuse summaries hold to.
assert_contains "$(printf '%s\n' "$ep_err" | head -1)" "./services/api/apprafter" \
    "ADR 0063: the ambiguity refusal NAMES the packages on its first (tile) line"

# ── ADR 0063: a helper SUB-package is not a second bundle ──
#
# `helper-subpackage/apprafter/` holds the manifest and
# `helper-subpackage/apprafter/lib/` the shared CUE it imports. The
# helper imports the schema, so it carries the marker and the search
# returns BOTH directories — but `lib/` is nested inside the package
# directory, so it is part of that bundle's tree, not a sibling. ADR
# 0029 contemplates this layout for monorepos with shared CUE.
#
# Both registrations must render the SAME single manifest. Before the
# nesting rule the root registration refused outright, and `--path
# apprafter` failed with `reference "helperApp" not found` — that second
# one because `cue export ./...` spans the sub-package as a second
# instance and `-e` then evaluates against the wrong one, which is why
# the render targets `.` rather than `./...`.
helper_fx="$script_dir/testdata/helper-subpackage"
scrub_helper_fixture() {
    find "$helper_fx" \( -name pkg -type d -path '*/cue.mod/*' -o -name apprafter_claim_gen.cue -type f \) \
        -prune -exec rm -rf {} +
}
trap 'cleanup; scrub_claim_fixture; scrub_claima_fixture; scrub_split_fixture; scrub_helper_fixture' EXIT
scrub_helper_fixture

run_entrypoint_capture "$helper_fx" "$schema_src"
scrub_helper_fixture
assert_rc "$ep_rc" zero "ADR 0063: helper sub-package at the repo root renders (exit 0)"
assert_count "$ep_out" "---" 1 "ADR 0063: helper sub-package at the repo root emits exactly ONE document"
assert_contains "$ep_out" "name: helper-app" "ADR 0063: helper sub-package at the repo root emits the bundle manifest"
assert_contains "$ep_out" "namespace: helper-demo" "ADR 0063: the helper package's shared values resolved"

run_entrypoint_capture "$helper_fx/apprafter" "$schema_src" "helper-subpackage/apprafter"
scrub_helper_fixture
assert_rc "$ep_rc" zero "ADR 0063: helper sub-package at --path apprafter renders (exit 0)"
assert_count "$ep_out" "---" 1 "ADR 0063: helper sub-package at --path apprafter emits exactly ONE document"
assert_contains "$ep_out" "name: helper-app" "ADR 0063: helper sub-package at --path apprafter emits the bundle manifest"

# ── ADR 0063: a nested BUNDLE is folded, but never in silence ──
#
# The dual of the helper case, and identical to it on disk: a package
# directory with a marker-bearing directory inside it. `extra/` holds a
# second real Application rather than shared CUE, so folding it in means
# `inner-bundle` never deploys.
#
# Not a prune vector — every revision before this one failed outright on
# this layout, so no cluster can have had `inner-bundle` applied from
# this registration, and the surprise is "it never deployed" rather than
# "it was deleted". But ADR 0063 §Decision 3's own argument is that a
# silent partial render is the worse state, so one line on stderr is the
# difference between an operator finding the cause in the sync log and
# not finding it at all.
#
# The negative assertion on the helper layout below is the other half:
# the notice is gated on whether the folded directory would itself have
# RENDERED something, so the case we just fixed stays quiet.
nested_fx="$script_dir/testdata/nested-bundle"
scrub_nested_fixture() {
    find "$nested_fx" \( -name cue.mod -type d -o -name apprafter_claim_gen.cue -type f \) \
        -prune -exec rm -rf {} +
}
trap 'cleanup; scrub_claim_fixture; scrub_claima_fixture; scrub_split_fixture; scrub_helper_fixture; scrub_nested_fixture' EXIT
scrub_nested_fixture

run_entrypoint_capture "$nested_fx" "$schema_src"
scrub_nested_fixture
assert_rc "$ep_rc" zero "ADR 0063: nested bundle — the enclosing package still renders (exit 0)"
assert_count "$ep_out" "---" 1 "ADR 0063: nested bundle — exactly ONE document emitted"
assert_contains "$ep_out" "name: outer-bundle" "ADR 0063: nested bundle — the enclosing package's manifest is emitted"
assert_absent "$ep_out" "inner-bundle" "ADR 0063: nested bundle — the nested manifest is NOT emitted"
assert_contains "$ep_err" "folded into their enclosing package" "ADR 0063: nested bundle — the fold is announced on stderr"
assert_contains "$ep_err" "./apprafter/extra" "ADR 0063: nested bundle — stderr NAMES the directory that was folded away"

# The negative: the helper layout folds a directory too, but that
# directory renders nothing, so announcing it would be pure noise on the
# layout this change exists to support.
run_entrypoint_capture "$helper_fx" "$schema_src"
scrub_helper_fixture
assert_absent "$ep_err" "folded into their enclosing package" "ADR 0063: helper sub-package — folding shared CUE is SILENT (no notice)"
assert_absent "$ep_err" "./apprafter/lib" "ADR 0063: helper sub-package — the helper directory is not named on stderr"

# ── ADR 0063 §Decision 2: the ARGOCD_APP_SOURCE_PATH arm ───
#
# The part of the registered path AT or ABOVE the working directory is
# invisible to `find .`; ARGOCD_APP_SOURCE_PATH is the only signal for
# it, and argocd-repo-server supplies it to generate as well as to
# discover. `${PWD##*/}` covers only the case where the working
# directory ITSELF is named `apprafter`, so for `--path apprafter/api`
# — working directory basename `api` — the variable is the whole gate.
#
# The fixture makes the arm observable: a depth-1 `Application.cue`
# (matches `-name '*.cue'`, so ONLY the ancestor arm finds it) and a
# `helpers/apprafter-shared.cue` (matches `apprafter*.cue`, so only the
# other arm finds it), each emitting a differently-named manifest. The
# assertion names the manifest, so it can pass only if the arm fired.
#
# Mutation-tested: deleting the `case "/${ar_sp#/}/" in */apprafter/*)`
# line flips the SET case to `ancestor-fallback`.
anc_fx="$script_dir/testdata/ancestor-arm/apprafter/api"

run_entrypoint_capture "$anc_fx" "" "apprafter/api"
assert_rc "$ep_rc" zero "ADR 0063: ancestor arm — ARGOCD_APP_SOURCE_PATH set renders (exit 0)"
assert_contains "$ep_out" "name: ancestor-registered" "ADR 0063: ancestor arm — an apprafter component in ARGOCD_APP_SOURCE_PATH selects the registered directory"
assert_absent "$ep_out" "ancestor-fallback" "ADR 0063: ancestor arm — the registered directory short-circuits the deeper candidate"
# `helpers/` holds a real manifest that this registration does not
# render, so the fold notice must name it — the same guarantee the
# nested-bundle case asserts, reached through the depth-1 short-circuit
# rather than through a sibling directory.
assert_contains "$ep_err" "./helpers" "ADR 0063: ancestor arm — the deeper manifest directory is named on stderr, not dropped silently"

run_entrypoint_capture "$anc_fx" "" ""
assert_rc "$ep_rc" zero "ADR 0063: ancestor arm — ARGOCD_APP_SOURCE_PATH unset renders (exit 0)"
assert_contains "$ep_out" "name: ancestor-fallback" "ADR 0063: ancestor arm — without the variable the depth-1 manifest is NOT claimed"
assert_absent "$ep_out" "ancestor-registered" "ADR 0063: ancestor arm — the two arms select different directories (the arm is live)"

# ── ADR 0063 §Decision 5: intra-bundle consistency ─────────
#
# A manifest package is a BUNDLE (ADR 0062): one registration, one
# namespace, one environment, every workload added/synced/removed
# together. Four ways a bundle can contradict itself were undetected
# before this change, and each was silently destructive. Measured on
# these fixtures against the pre-guard entrypoint:
#
#   bundle-split-ns      rc=0, 2 documents — both rendered, and Argo CD's
#                        single destination.namespace plus the CLI's
#                        namespace picker each disagree with one of them.
#   bundle-dup-name      rc=0, 2 documents — but ONE apiserver identity;
#                        whichever is applied last silently wins.
#   bundle-mixed-style   rc=0, 1 document — the Style-A branch returns
#                        before the Style-B enumeration is reached, so
#                        `mixed-wrapped` rides out as a stray top-level
#                        key that the apiserver PRUNES without an error.
#                        The discarded manifest never becomes an API
#                        object, which is why no layer below this one
#                        could ever have caught it.
#   bundle-split-env     rc=0, 2 documents — and with a registered env,
#                        `inject_env` overwrites `.spec.environment` on
#                        both, erasing the divergence before anything
#                        downstream could see it.
#   bundle-env-partial   rc=0, 2 documents — the partial form: one
#                        workload declares an environment, its sibling
#                        deploys base-only.
#
# NONE of these is reachable from the admission webhook: it receives one
# object per AdmissionReview and has no kube client, so it has no sibling
# to compare against (ADR 0063 §Decision 5 records why giving it one is
# worse than the gap).
#
# Each case asserts ALL THREE of:
#   1. non-zero exit;
#   2. stdout EMPTY — the exit code alone would also pass for a run that
#      had already flushed a partial stream, which is the pruning shape;
#   3. the specific summary on stderr, since that first line is what Argo
#      CD shows on the Application tile.
bundlens_fx="$script_dir/testdata/bundle-split-ns"
bundledup_fx="$script_dir/testdata/bundle-dup-name"
bundlemixed_fx="$script_dir/testdata/bundle-mixed-style"
bundleenv_fx="$script_dir/testdata/bundle-split-env"
bundleenvp_fx="$script_dir/testdata/bundle-env-partial"
bundleforeign_fx="$script_dir/testdata/bundle-foreign-kind"
bundlemfile_fx="$script_dir/testdata/bundle-multi-file"
bundlemfilens_fx="$script_dir/testdata/bundle-multi-file-split-ns"
bundleorder_fx="$script_dir/testdata/bundle-key-order"
bundleorderns_fx="$script_dir/testdata/bundle-key-order-split-ns"

scrub_bundle_fixtures() {
    find "$bundlens_fx" "$bundledup_fx" "$bundlemixed_fx" "$bundleenv_fx" "$bundleenvp_fx" \
        "$bundleforeign_fx" "$bundlemfile_fx" "$bundlemfilens_fx" \
        "$bundleorder_fx" "$bundleorderns_fx" \
        \( -name cue.mod -type d -o -name apprafter_claim_gen.cue -type f \) \
        -prune -exec rm -rf {} +
}
trap 'cleanup; scrub_claim_fixture; scrub_claima_fixture; scrub_split_fixture; scrub_helper_fixture; scrub_nested_fixture; scrub_bundle_fixtures' EXIT
scrub_bundle_fixtures

# ── §5: two namespaces in one package ──────────────────────
run_entrypoint_capture "$bundlens_fx" "$schema_src"
scrub_bundle_fixtures
assert_rc "$ep_rc" nonzero "ADR 0063 §5: two namespaces in one package REFUSE"
assert_stdout_empty "ADR 0063 §5: two namespaces write NOTHING to stdout"
assert_contains "$ep_err" "bundle is inconsistent" "ADR 0063 §5: namespaces — the refusal marker is on stderr"
assert_contains "$ep_err" "2 different namespaces" "ADR 0063 §5: namespaces — the summary names the divergence"
assert_contains "$ep_err" 'nsOne -> "one"' "ADR 0063 §5: namespaces — the summary names the first workload AND its value"
assert_contains "$ep_err" 'nsTwo -> "two"' "ADR 0063 §5: namespaces — the summary names the second workload AND its value"
assert_absent "$ep_err" "spec.environment:" "ADR 0063 §5: namespaces — the environment check does not also fire"

# ── §5: duplicate (namespace, name) ────────────────────────
run_entrypoint_capture "$bundledup_fx" "$schema_src"
scrub_bundle_fixtures
assert_rc "$ep_rc" nonzero "ADR 0063 §5: duplicate (namespace, name) REFUSES"
assert_stdout_empty "ADR 0063 §5: duplicate identity writes NOTHING to stdout"
assert_contains "$ep_err" "share one (namespace, name)" "ADR 0063 §5: identity — the summary names the rule"
assert_contains "$ep_err" "dup-demo/dup-app" "ADR 0063 §5: identity — the summary names the colliding identity"
assert_contains "$ep_err" "dupOne, dupTwo" "ADR 0063 §5: identity — the summary names both workloads that declared it"

# ── §5: Style A mixed with Style B ─────────────────────────
run_entrypoint_capture "$bundlemixed_fx" "$schema_src"
scrub_bundle_fixtures
assert_rc "$ep_rc" nonzero "ADR 0063 §5: mixed Style A / Style B REFUSES"
assert_stdout_empty "ADR 0063 §5: mixed style writes NOTHING to stdout"
assert_contains "$ep_err" "package-scope manifest mixed with named wrapper" "ADR 0063 §5: mixed style — the summary names the finding"
assert_contains "$ep_err" "wrapped" "ADR 0063 §5: mixed style — the summary names the wrapper that would have been dropped"
# The whole point of this one: before the guard the wrapped manifest was
# silently discarded by the Style-A dispatch, so the package-scope
# document rendered ALONE with rc=0.
assert_absent "$ep_out" "mixed-package-scope" "ADR 0063 §5: mixed style — the package-scope manifest is NOT emitted either"

# ── §5: two environments in one package ────────────────────
run_entrypoint_capture "$bundleenv_fx" "$schema_src"
scrub_bundle_fixtures
assert_rc "$ep_rc" nonzero "ADR 0063 §5: two environments in one package REFUSE"
assert_stdout_empty "ADR 0063 §5: two environments write NOTHING to stdout"
assert_contains "$ep_err" "2 different environments" "ADR 0063 §5: environments — the summary names the divergence"
assert_contains "$ep_err" 'envOne -> "dev"' "ADR 0063 §5: environments — the summary names the first workload AND its value"
assert_contains "$ep_err" 'envTwo -> "prod"' "ADR 0063 §5: environments — the summary names the second workload AND its value"

# The guard must run BEFORE inject_env, which sets `.spec.environment`
# on every document unconditionally — after it, the divergence reads as
# agreement. Registering an env is exactly the case where that erasure
# happens, so the refusal has to survive it.
run_entrypoint_capture_env() {  # $1 fixture root, $2 schema_src, $3 env value
    local root=$1 schema_src=$2 env_val=$3 out_file err_file
    out_file=$(mktemp)
    err_file=$(mktemp)
    set +e
    ( cd "$root" && unset APPRAFTER_APP_ENV ARGOCD_APP_SOURCE_PATH \
        && APPRAFTER_SCHEMA_SRC="$schema_src" ARGOCD_ENV_APPRAFTER_APP_ENV="$env_val" \
           bash "$entrypoint" ) >"$out_file" 2>"$err_file"
    ep_rc=$?
    set -e
    ep_out=$(cat "$out_file")
    ep_err=$(cat "$err_file")
    ep_out_bytes=$(wc -c < "$out_file" | tr -d '[:space:]')
    rm -f "$out_file" "$err_file"
}
run_entrypoint_capture_env "$bundleenv_fx" "$schema_src" "staging"
scrub_bundle_fixtures
assert_rc "$ep_rc" nonzero "ADR 0063 §5: environments — the refusal survives a REGISTERED env (guard runs before inject_env)"
assert_stdout_empty "ADR 0063 §5: environments — nothing on stdout with a registered env either"
assert_contains "$ep_err" "2 different environments" "ADR 0063 §5: environments — inject_env has not erased the divergence"

# ── §5: one workload declares an environment, its sibling does not ──
#
# The decision the guard makes, tested rather than merely commented: an
# absent `spec.environment` is the BASE-ONLY deploy, a deployment
# semantic of its own and not a blank waiting for a registration-level
# default (there is no `destination.environment`). So "declared on one,
# absent on the other" counts as a divergence. All-absent is the normal
# case and is covered by the Style-B fixture at the top of this file,
# which must keep rendering both documents.
run_entrypoint_capture "$bundleenvp_fx" "$schema_src"
scrub_bundle_fixtures
assert_rc "$ep_rc" nonzero "ADR 0063 §5: declared-vs-absent environment REFUSES"
assert_stdout_empty "ADR 0063 §5: declared-vs-absent environment writes NOTHING to stdout"
assert_contains "$ep_err" "2 different environments" "ADR 0063 §5: declared-vs-absent — absent counts as its own value"
assert_contains "$ep_err" "(not declared)" "ADR 0063 §5: declared-vs-absent — the summary spells out the absent side"

# ── §5: `kind: Application` from a FOREIGN apiVersion is not a workload ──
#
# The cross-workload checks key on apiVersion AND kind, never kind alone.
# Argo CD's own CRD is `argoproj.io/v1alpha1, kind: Application` — the one
# foreign apiVersion that collides exactly with ours — and a package may
# legitimately ship one beside the workload it registers. It lives in the
# `argocd` namespace and carries no spec.environment, so a kind-only
# filter reads this package as two namespaces AND two environments and
# refuses a bundle that is not inconsistent.
#
# Measured on the kind-only filter this fixture was written against:
# `rc=1 … 2 different namespaces — web -> "apprafter", argoApp ->
# "argocd"`, where the revision before the guards rendered BOTH documents
# at rc=0 — i.e. a behaviour change, and broader than ADR 0063
# §Decision 5, whose table speaks of WORKLOADS. Safe in direction (a loud
# refusal applies nothing) but wrong.
#
# Mutation-tested: dropping the `startswith("apprafter.io/")` predicate
# flips exactly this case and leaves all five inconsistent fixtures red.
run_entrypoint_capture "$bundleforeign_fx" "$schema_src"
scrub_bundle_fixtures
assert_rc "$ep_rc" zero "ADR 0063 §5: a foreign Application-kind object does NOT make the bundle inconsistent (exit 0)"
assert_count "$ep_out" "---" 2 "ADR 0063 §5: foreign kind — BOTH documents are still emitted"
assert_contains "$ep_out" "name: foreign-web" "ADR 0063 §5: foreign kind — the AppRafter workload is emitted"
assert_contains "$ep_out" "name: foreign-argo" "ADR 0063 §5: foreign kind — the argoproj.io object is emitted"
assert_absent "$ep_err" "bundle is inconsistent" "ADR 0063 §5: foreign kind — no refusal (the apiVersion predicate is live)"

# ── §5: a bundle spread over TWO FILES of one package ──────
#
# Every other fixture above is a single `.cue` file, and that gap had a
# cost on the OTHER side of the pair. `apprafter app validate` — the
# local twin ADR 0063 §Decision 5 puts on the same row as this layer —
# resolved the FILE `apprafter/Application.cue` by default and dropped
# every sibling, so all four checks saw N=1 and none could fire. On a
# two-file bundle the twin therefore answered `✓ valid` for exactly what
# this script asserts is refused: the opposite verdict, on the default
# invocation of both commands.
#
# A single-file fixture cannot catch that — naming the file and naming
# the directory are the same package when there is only one file. These
# two can, and they are modelled on this repository's own
# `landing/web/apprafter/` (prod in `Application.cue`, preview in
# `Application-preview.cue`), which is the only multi-file bundle that
# exists today.
#
# The CLI half asserts the same two directories in
# `cli/platform-cli/src/commands/app_validate.rs`
# (`validate_reads_every_file_of_a_multi_file_bundle_by_default`,
# `validate_refuses_a_multi_file_bundle_that_contradicts_itself_by_default`,
# `validate_orders_workloads_the_way_the_sidecar_does`). Sharing the
# fixtures is what makes the two halves one gate rather than two.
run_entrypoint_capture "$bundlemfile_fx" "$schema_src"
scrub_bundle_fixtures
assert_rc "$ep_rc" zero "ADR 0063 §5: a CONSISTENT two-FILE bundle renders (exit 0)"
assert_count "$ep_out" "---" 2 "ADR 0063 §5: two files — BOTH documents are emitted"
assert_contains "$ep_out" "name: multi-file-web" "ADR 0063 §5: two files — the workload from Application.cue is emitted"
assert_contains "$ep_out" "name: multi-file-web-preview" "ADR 0063 §5: two files — the workload from Application-preview.cue is emitted"
assert_absent "$ep_err" "bundle is inconsistent" "ADR 0063 §5: two files — a consistent bundle triggers no refusal"
# ORDER, asserted on both sides of the pair. The emission order is
# `cue export . --out json`'s own key order, which for a MULTI-file
# package is not `cue def`'s (that one follows file order, and
# `Application-preview.cue` sorts before `Application.cue`). The CLI read
# `cue def` until 2.27b and so listed these two the other way round from
# the tile.
assert_before "$ep_out" "name: multi-file-web" "name: multi-file-web-preview" \
    "ADR 0063 §5: two files — the prod workload is emitted BEFORE the preview one"

run_entrypoint_capture "$bundlemfilens_fx" "$schema_src"
scrub_bundle_fixtures
assert_rc "$ep_rc" nonzero "ADR 0063 §5: two FILES declaring two namespaces REFUSE"
assert_stdout_empty "ADR 0063 §5: two files, two namespaces write NOTHING to stdout"
assert_contains "$ep_err" "2 different namespaces" "ADR 0063 §5: two files — the divergence is found across the file boundary"
assert_contains "$ep_err" 'splitFileWeb -> "prod"' "ADR 0063 §5: two files — the summary names the workload from Application.cue"
assert_contains "$ep_err" 'splitFileWebPreview -> "preview"' "ADR 0063 §5: two files — the summary names the workload from Application-preview.cue"
assert_before "$ep_err" 'splitFileWeb -> "prod"' 'splitFileWebPreview -> "preview"' \
    "ADR 0063 §5: two files — the refusal summary is sorted by key"

# ── §5: the order rule itself — sorted by TOP-LEVEL KEY ────
#
# The two assertions above pass under any rule that happens to put
# `splitFileWeb` before `splitFileWebPreview`, and the export's own key
# order is one such rule on some cue versions. That is how it came to be
# the rule: it was measured on a developer shell's cue v0.16.0, and the
# same two assertions went red in CI, which installs the v0.10.0 the
# sidecar image pins. Same script, same fixture, opposite verdict —
# because the export order is an evaluator detail, not a CUE contract.
#
# `bundle-key-order/` is the fixture that cannot be satisfied by
# accident: over its four keys the sorted sequence, cue v0.10.0's export
# and cue v0.16.0's export are three DIFFERENT sequences (measured in
# the fixture's own header), and `metadata.name` runs opposite to the
# key order, so sorting the rows by what is printed fails too.
#
# Mutation-tested: dropping `sort_by(.key)` from the entrypoint's key
# enumeration turns both assertions below red on either cue.
run_entrypoint_capture "$bundleorder_fx" "$schema_src"
scrub_bundle_fixtures
assert_rc "$ep_rc" zero "ADR 0063 §5: the four-workload order fixture renders (exit 0)"
assert_count "$ep_out" "---" 4 "ADR 0063 §5: order — all FOUR documents are emitted"
assert_sequence "$ep_out" "ADR 0063 §5: order — documents are emitted sorted by top-level key" \
    "name: keyorder-zulu" "name: keyorder-yankee" "name: keyorder-xray" "name: keyorder-whiskey"

run_entrypoint_capture "$bundleorderns_fx" "$schema_src"
scrub_bundle_fixtures
assert_rc "$ep_rc" nonzero "ADR 0063 §5: order — the split-namespace half REFUSES"
assert_stdout_empty "ADR 0063 §5: order — the refusing half writes NOTHING to stdout"
assert_sequence "$ep_err" "ADR 0063 §5: order — the refusal summary is sorted by top-level key" \
    'apiTier -> "prod"' 'cacheTier -> "preview"' 'jobsTier -> "preview"' 'webTier -> "prod"'

# ── §5: the consistent bundles must be UNAFFECTED ──────────
#
# The strongest non-regression signal available off-cluster: a package
# with TWO workloads that agree on everything the guards check must still
# render BOTH documents, silently. `inject-fixture-multi` is exactly that
# (one namespace — neither declares any — distinct names, no environment
# on either), so it is re-run here through the refusal-aware capture so
# rc and stderr are asserted, not just the stdout the cases above check.
run_entrypoint_capture "$style_b" ""
assert_rc "$ep_rc" zero "ADR 0063 §5: a CONSISTENT two-workload bundle still renders (exit 0)"
assert_count "$ep_out" "---" 2 "ADR 0063 §5: a consistent two-workload bundle still emits BOTH documents"
assert_absent "$ep_err" "bundle is inconsistent" "ADR 0063 §5: a consistent bundle triggers no refusal"

# A Style-A (unwrapped) package renders exactly ONE row into the guards'
# table, so all three cross-workload checks are no-ops on it. That is
# what keeps every single-manifest layout working; assert it rather than
# assume it.
run_entrypoint_capture "$style_a" ""
assert_rc "$ep_rc" zero "ADR 0063 §5: a Style-A package still renders (exit 0)"
assert_contains "$ep_out" "name: inject-fixture" "ADR 0063 §5: the Style-A manifest is still emitted"
assert_absent "$ep_err" "bundle is inconsistent" "ADR 0063 §5: a Style-A package triggers no refusal"

echo ""
echo "Summary: $pass passed, $fail failed"
if [[ "$fail" -gt 0 ]]; then exit 1; fi
