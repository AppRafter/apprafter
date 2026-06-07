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
    if printf '%s\n' "$haystack" | grep -Fq "$needle"; then
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
    if printf '%s\n' "$haystack" | grep -Fq "$needle"; then
        echo "FAIL: $label — did not expect '$needle'"
        echo "--- stdout was ---" >&2
        printf '%s\n' "$haystack" >&2
        fail=$((fail + 1))
    else
        echo "PASS: $label"
        pass=$((pass + 1))
    fi
}

assert_count() {
    local haystack=$1 needle=$2 want=$3 label=$4
    local got
    got=$(printf '%s\n' "$haystack" | grep -Fc "$needle" || true)
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
run_entrypoint() {  # $1 fixture root, $2 value for APPRAFTER_APP_ENV ("" = unset)
    local root=$1 env_val=$2
    if [[ -z "$env_val" ]]; then
        ( cd "$root" && unset APPRAFTER_APP_ENV && bash "$entrypoint" )
    else
        ( cd "$root" && APPRAFTER_APP_ENV="$env_val" bash "$entrypoint" )
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

echo ""
echo "Summary: $pass passed, $fail failed"
if [[ "$fail" -gt 0 ]]; then exit 1; fi
