#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-component-claim-templates.sh — render every Helm-chart component the
# umbrella chart can install, with the values its Argo CD Application
# carries, and assert that each StatefulSet volumeClaimTemplate is rendered
# the way the apiserver stores it.
#
# ## Why
#
# The apiserver adds four fields to every volumeClaimTemplate: apiVersion,
# kind, spec.volumeMode and status.phase. Argo CD 2.13 diffs a component with
# server-side apply, and that diff replaces the whole template list (it is
# atomic) with the rendered one. It then restores the two defaulted fields,
# cannot restore apiVersion or kind, and adds `metadata.creationTimestamp:
# null`. So a template rendered without those four fields is OutOfSync
# forever, and self-heal re-syncs it every few minutes. That is how the `nats`
# Application behaved on every cluster with an app declaring
# `needs.jetstream`: the nats chart renders none of the four, and nothing in
# `helm lint`, `cue vet` or a render of the umbrella chart ever looks inside
# a component's own chart. `component_nats.cue` now renders them through the
# chart's `merge` hook. This check keeps that true across a nats chart bump,
# and holds any new component that ships a StatefulSet to the same shape.
#
# It needs the network: it pulls each component's chart at the version the
# umbrella pins. The in-repo operator charts are rendered from
# `operator/charts/`, because their pinned version is often not published
# yet. Git-sourced components are plain manifests and are listed as not
# rendered. None of them ships a StatefulSet today.
#
# Usage: bash scripts/check-component-claim-templates.sh
# Exit 0 = every volumeClaimTemplate carries all four fields, and at least
# one was checked.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

CUE_CMD=("$REPO_ROOT/scripts/cue")
command -v helm >/dev/null 2>&1 || {
    echo "::error::helm is not on PATH — run under \`nix develop\`" >&2
    exit 2
}
if command -v yq >/dev/null 2>&1; then
    YQ=(yq)
else
    YQ=(nix run nixpkgs#yq-go --)
fi

version="$("${CUE_CMD[@]}" export ./platform-stack/cue/... -e currentVersion --out text)"
chart="platform-stack/dist/platform-stack-${version}"
if [[ ! -d "$chart" ]]; then
    echo "==> rendering $chart"
    make -C platform-stack render-only >/dev/null
fi
[[ -d "$chart" ]] || { echo "::error::no rendered chart at $chart" >&2; exit 1; }

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

# Every component switched on, so the ones that ship `enabled: false` and are
# turned on later by an override (nats, nack, backstage) are checked too.
"${YQ[@]}" -o json \
    '.components | keys | map({"key": ., "value": {"enabled": true}}) | from_entries | {"overrides": .}' \
    "$chart/values.yaml" > "$workdir/all-on.json"
helm template platform "$chart" --values "$workdir/all-on.json" > "$workdir/umbrella.yaml"

mapfile -t apps < <("${YQ[@]}" -N 'select(.kind == "Application") | .metadata.name' "$workdir/umbrella.yaml")
(( ${#apps[@]} > 0 )) || { echo "::error::the umbrella chart rendered no Application" >&2; exit 1; }

echo "==> volumeClaimTemplate shape, chart $version, ${#apps[@]} components"

# One line per template: <statefulset>/<template>|apiVersion|kind|volumeMode|phase
# shellcheck disable=SC2016  # $s is a yq variable, not a shell one
vct_lines='select(.kind == "StatefulSet") | .metadata.name as $s
    | (.spec.volumeClaimTemplates // [])[]
    | $s + "/" + (.metadata.name // "?") + "|" + (.apiVersion // "") + "|" + (.kind // "")
      + "|" + (.spec.volumeMode // "") + "|" + (.status.phase // "")'

failures=0
checked=0
for name in "${apps[@]}"; do
    app="$workdir/app-$name.yaml"
    NAME="$name" "${YQ[@]}" 'select(.kind == "Application" and .metadata.name == env(NAME))' \
        "$workdir/umbrella.yaml" > "$app"
    repo="$("${YQ[@]}" '.spec.source.repoURL' "$app")"
    src_chart="$("${YQ[@]}" '.spec.source.chart // ""' "$app")"
    rev="$("${YQ[@]}" '.spec.source.targetRevision' "$app")"
    ns="$("${YQ[@]}" '.spec.destination.namespace' "$app")"
    if [[ -z "$src_chart" ]]; then
        echo "  not rendered: $name (git source $repo, path $("${YQ[@]}" '.spec.source.path' "$app"))"
        continue
    fi
    "${YQ[@]}" '.spec.source.helm.valuesObject // {}' "$app" > "$workdir/values-$name.yaml"

    out="$workdir/out-$name.yaml"
    case "$repo" in
        ghcr.io/apprafter/charts)
            local_chart="operator/charts/$src_chart"
            [[ -d "$local_chart" ]] || {
                echo "::error::$name: pinned to $repo/$src_chart, and there is no $local_chart to render" >&2
                failures=$((failures + 1)); continue; }
            cmd=(helm template "$name" "$local_chart") ;;
        https://*)
            cmd=(helm template "$name" "$src_chart" --repo "$repo" --version "$rev") ;;
        *)
            cmd=(helm template "$name" "oci://$repo/$src_chart" --version "$rev") ;;
    esac
    if ! "${cmd[@]}" --namespace "$ns" --values "$workdir/values-$name.yaml" > "$out" 2> "$workdir/err-$name"; then
        echo "::error::$name: could not render $repo $src_chart $rev:" >&2
        sed 's/^/    /' "$workdir/err-$name" >&2
        failures=$((failures + 1))
        continue
    fi

    before=$checked
    while IFS='|' read -r where api kind mode phase; do
        [[ -n "$where" ]] || continue
        checked=$((checked + 1))
        missing=()
        [[ "$api" == "v1" ]] || missing+=("apiVersion: v1")
        [[ "$kind" == "PersistentVolumeClaim" ]] || missing+=("kind: PersistentVolumeClaim")
        [[ -n "$mode" ]] || missing+=("spec.volumeMode")
        [[ -n "$phase" ]] || missing+=("status.phase")
        if (( ${#missing[@]} > 0 )); then
            list="$(printf '%s, ' "${missing[@]}")"
            echo "::error::$name: StatefulSet $where renders without: ${list%, }" >&2
            failures=$((failures + 1))
        else
            echo "  ok: $name StatefulSet $where"
        fi
    done < <("${YQ[@]}" -N "$vct_lines" "$out")
    (( checked > before )) || echo "  rendered: $name (no volumeClaimTemplate)"
done

if (( failures > 0 )); then
    echo "::error::$failures problem(s) above: a chart that did not render, or a volumeClaimTemplate without those fields." >&2
    echo "  For the second, render the four fields the apiserver adds, e.g. through the chart's own" >&2
    echo "  merge/patch hook for that template — see config.jetstream.fileStore.pvc.merge" >&2
    echo "  in platform-stack/cue/component_nats.cue." >&2
    exit 1
fi
if (( checked == 0 )); then
    echo "::error::no volumeClaimTemplate was checked; the nats component should have one." >&2
    echo "  A check that sees nothing proves nothing." >&2
    exit 1
fi
echo "volumeClaimTemplates OK: $checked checked, each as the apiserver stores it"
