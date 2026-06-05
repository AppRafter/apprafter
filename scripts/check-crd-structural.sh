#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# CRD structural-schema guard.
#
# Kubernetes apiextensions/v1 rejects a CRD whose schema node sets BOTH
# `properties` and `additionalProperties` at the same level:
#   "additionalProperties and properties are mutual exclusive"
# A closed object in a CRD does NOT use `additionalProperties: false` — it
# relies on structural-schema pruning (preserveUnknownFields defaults to
# false, so unknown fields are dropped automatically). A legitimate MAP uses
# `additionalProperties: <schema>` WITHOUT a sibling `properties` — that is
# fine and is NOT what this guard flags.
#
# This catches the 2.4h regression (v0.2.15) where `additionalProperties:
# false` shipped on `imagePolicy` + `status.image` and made every
# `applications.apprafter.io` CRD apply fail server-side — which `helm lint`
# does not validate, so it only surfaced in the e2e after publish.
set -euo pipefail

shopt -s nullglob
crds=(operator/charts/*/templates/crd-*.yaml)
if [[ ${#crds[@]} -eq 0 ]]; then
    echo "==> no CRD templates found — skipping CRD structural check"
    exit 0
fi

hits=$(grep -rnE 'additionalProperties:[[:space:]]*false' "${crds[@]}" || true)
if [[ -n "$hits" ]]; then
    echo "==> CRD structural-schema violation: 'additionalProperties: false' must NEVER appear in a CRD template." >&2
    echo "    apiextensions rejects 'additionalProperties' alongside 'properties' (mutually exclusive)." >&2
    echo "    Remove it — closed CRD objects rely on structural-schema pruning." >&2
    echo "$hits" >&2
    exit 1
fi

echo "==> CRD structural-schema check passed (no additionalProperties:false in CRD templates)"
