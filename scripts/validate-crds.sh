#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Validate every hand-rolled CRD against a REAL Kubernetes apiserver.
#
# `helm lint` does NOT validate CRD structural schemas — the apiserver's
# apiextensions validation does (e.g. "additionalProperties and properties are
# mutual exclusive"). This spins an ephemeral `kind` cluster, applies all
# `operator/charts/*/templates/crd-*.yaml` (rendered via helm), and asserts
# each CRD reaches `Established=True`. Run it before any CRD-changing release;
# the full `just e2e` is the comprehensive backstop, this is the fast, focused
# CRD gate (~30s vs ~20m).
#
# Requires: kind (+ a working docker/podman), kubectl, helm. The repo flake /
# `nix shell nixpkgs#kind nixpkgs#kubectl` provides them.
set -euo pipefail

CLUSTER="apprafter-crd-validate"
cleanup() { kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "==> creating ephemeral kind cluster '$CLUSTER'"
kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true
kind create cluster --name "$CLUSTER" --wait 90s >/dev/null
CTX="kind-$CLUSTER"

rendered=$(mktemp)
for chart in operator/charts/*/; do
    [[ -d "${chart}templates" ]] || continue
    ls "${chart}templates"/crd-*.yaml >/dev/null 2>&1 || continue
    helm template "$chart" 2>/dev/null >> "$rendered" || {
        echo "==> helm template failed for $chart" >&2; exit 1; }
done

# Extract just the CRD documents (cluster-scoped, no namespace deps).
crds=$(mktemp)
if command -v yq >/dev/null 2>&1; then
    yq 'select(.kind == "CustomResourceDefinition")' "$rendered" > "$crds"
else
    nix run nixpkgs#yq-go -- 'select(.kind == "CustomResourceDefinition")' "$rendered" > "$crds"
fi

echo "==> applying $(grep -c '^kind: CustomResourceDefinition' "$crds") CRDs to the apiserver"
kubectl --context "$CTX" apply -f "$crds"

echo "==> waiting for every CRD to be Established"
kubectl --context "$CTX" wait --for=condition=Established --timeout=60s \
    -f "$crds"

# Regression proof for the imagePolicy.resolve enum (ADR 0040): the generated
# CRD must accept the STRING "off" as a valid enum value. Before the crdgen
# YAML-quoting fix, `off` was emitted bare and coerced to boolean `false` by
# the apiserver's YAML-1.1 parser, so the OpenAPI enum became ["digest", false]
# and this apply was HARD-REJECTED ("Unsupported value: \"off\": supported
# values: \"digest\", \"false\""). A server-side dry-run validates against the
# live OpenAPI schema without needing the operator to run.
echo "==> regression: Application imagePolicy.resolve: \"off\" must be accepted"
kubectl --context "$CTX" create namespace crd-validate >/dev/null 2>&1 || true
if kubectl --context "$CTX" apply --dry-run=server -f - >/dev/null 2>/tmp/crd-off-err.txt <<'YAML'
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: crd-validate-off
  namespace: crd-validate
spec:
  base:
    image: example.com/app:latest
    imagePolicy:
      resolve: "off"
YAML
then
    echo "    OK: resolve: \"off\" accepted by the apiserver"
else
    echo "==> REGRESSION: apiserver REJECTED imagePolicy.resolve: \"off\"" >&2
    cat /tmp/crd-off-err.txt >&2
    exit 1
fi

echo "==> CRD apiserver validation PASSED (all CRDs accepted + Established)"
