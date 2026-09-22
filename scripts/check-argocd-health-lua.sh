#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-argocd-health-lua.sh — execute the custom resource-health Lua that
# `platform-stack/cue/component_argocd.cue` ships into `argocd-cm`.
#
# ## Why
#
# These scripts decide what an operator sees on an Argo CD tile, and nothing
# else in this repository runs them. They fail quietly in both directions: a
# syntax error makes Argo CD log it and fall back to no assessment, and a
# wrong branch just reports the wrong colour. `helm lint`, `cue vet` and
# `helm template` all see an opaque string.
#
# Twice now that has cost a release. `argoproj.io_Application` was dropped as
# dead in chart 0.2.30 and every sync wave in the chart silently stopped
# ordering anything for 45 versions, until 0.2.75. And
# `apprafter.io_Application` reported three DELIBERATELY held phases —
# `AwaitingResourceClaim`, `EnvSecretMissing`, `InvalidEffectiveSpec` — as
# "Awaiting controller reconcile", so an application waiting on a person read
# as one waiting on a controller, until 0.2.77.
#
# ## What it does
#
#   1. Exports every `resource.customizations.health.*` script out of the CUE.
#   2. Runs `scripts/argocd-health-lua-test.lua` over them with fixture
#      objects, in the same shape Argo CD evaluates them (a global `obj`, a
#      global `hs` read back).
#   3. Asserts the set of shipped scripts EQUALS the set the fixtures cover,
#      in both directions. A new health script with no fixture fails here;
#      so does a fixture for a script that no longer ships.
#
# Lua comes from the flake (`nix develop`) or, in a bare checkout, from
# `nix run nixpkgs#lua` — the same fallback `lint-cue.sh` uses for cue. There
# is no skip path: a gate that passes when its interpreter is missing is the
# failure mode this file exists to prevent.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
chart_cue="$repo_root/platform-stack/cue"

# Resolve `lua`, preferring a bare binary and falling back to `nix run`.
# Echoes a command line the caller evals.
#
# NOT used for cue: `scripts/cue` is the single resolver for this repo's
# PINNED cue, and both branches below would give whatever the machine or
# nixpkgs happens to carry instead. That is a real difference, not a
# preference -- the root cue.mod declares a language version, and an older cue
# rejects it outright rather than producing a slightly different answer.
resolve_tool() {
    local bin="$1" attr="$2"
    if command -v "$bin" >/dev/null 2>&1; then
        printf '%s' "$bin"
        return 0
    fi
    if command -v nix >/dev/null 2>&1; then
        printf 'nix run nixpkgs#%s --' "$attr"
        return 0
    fi
    echo "::error::neither \`$bin\` nor \`nix\` is on PATH. Install $bin, or run under \`nix develop\`." >&2
    exit 2
}

CUE_CMD="$(git rev-parse --show-toplevel)/scripts/cue"
LUA_CMD="$(resolve_tool lua lua)"

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

# The `cm` map as JSON, read once. `-e` on the component rather than on the
# rendered chart: this is the source of truth both the chart template and the
# argocd subchart values are built from.
cm_json="$workdir/cm.json"
# shellcheck disable=SC2086
if ! (cd "$chart_cue" && $CUE_CMD export -e '_components.argocd.values.configs.cm' \
        --out json ./...) > "$cm_json" 2>"$workdir/cue.err"; then
    echo "::error::could not export the argocd-cm map from the chart CUE:" >&2
    cat "$workdir/cue.err" >&2
    exit 1
fi

# One `<key>.lua` per health script, named by the part after the prefix —
# `apprafter.io_Application`, `ConfigMap`, and so on. That name is the same
# one the fixtures use, so the two halves cannot drift on spelling.
python3 - "$cm_json" "$workdir" <<'PY'
import json, os, sys
cm_path, out_dir = sys.argv[1], sys.argv[2]
prefix = "resource.customizations.health."
with open(cm_path) as fh:
    cm = json.load(fh)
keys = sorted(k[len(prefix):] for k in cm if k.startswith(prefix))
if not keys:
    sys.exit("::error::no resource.customizations.health.* keys in the chart — "
             "the key shape moved, or the export selected the wrong node")
for key in keys:
    with open(os.path.join(out_dir, key + ".lua"), "w") as fh:
        fh.write(cm[prefix + key])
with open(os.path.join(out_dir, "shipped.txt"), "w") as fh:
    fh.write("\n".join(keys) + "\n")
PY

echo "==> health scripts in the chart: $(tr '\n' ' ' < "$workdir/shipped.txt")"

# shellcheck disable=SC2086
HEALTH_DIR="$workdir" $LUA_CMD "$repo_root/scripts/argocd-health-lua-test.lua"

# The coverage contract. The fixture file writes what it covers only after
# every assertion passed, so a missing `covered.txt` here means the run
# above did not reach the end — which `set -e` has already caught, but the
# explicit check keeps this from ever reading as "covered nothing, fine".
if [[ ! -f "$workdir/covered.txt" ]]; then
    echo "::error::the fixture run produced no coverage list" >&2
    exit 1
fi

if ! diff_out="$(diff <(sort "$workdir/shipped.txt") <(sort "$workdir/covered.txt"))"; then
    echo "::error::the shipped health scripts and the fixtures disagree." >&2
    echo "  '<' ships with no fixture; '>' is a fixture for a script that no longer ships." >&2
    echo "  Fix by editing COVERED in scripts/argocd-health-lua-test.lua and adding" >&2
    echo "  assertions for the new kind — a health script nothing executes is how" >&2
    echo "  0.2.30 and 0.2.77 both happened." >&2
    echo "$diff_out" >&2
    exit 1
fi

echo "argocd health Lua OK: every shipped script compiles, runs, and is covered"
