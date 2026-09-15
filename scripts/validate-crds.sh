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
echo "==> regression: MigrationPlan base-env scope (environment: \"\") must be accepted"
kubectl --context "$CTX" create namespace crd-validate >/dev/null 2>&1 || true
if kubectl --context "$CTX" apply --dry-run=server -f - >/dev/null 2>/tmp/crd-baseenv-err.txt <<'YAML'
apiVersion: apprafter.io/v1alpha1
kind: MigrationPlan
metadata:
  name: crd-validate-base-env
  namespace: crd-validate
spec:
  scope:
    type: application
    application:
      ref:
        name: parser
        namespace: crd-validate
      environment: ""
  trigger:
    type: needs-removal
    field: needs.pg
YAML
then
    echo "    OK: base-env MigrationPlan accepted by the apiserver"
else
    echo "    FAIL: the apiserver rejected a base-env MigrationPlan." >&2
    echo "    An Application with no spec.environment is the COMMON case, and the" >&2
    echo "    operator writes \"\" for it everywhere. Rejecting it here means every" >&2
    echo "    destructive change on such an app retries a 422 forever: the plan is" >&2
    echo "    never created, the gate never engages, and the reconcile freezes." >&2
    cat /tmp/crd-baseenv-err.txt >&2
    exit 1
fi

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

# 2.22g / D2. `spec.backup` is FULLY STRUCTURAL — the only
# preserve-unknown-fields markers in this CRD are on spec.overrides.*.values,
# spec.values and status. So an un-declared key is PRUNED: the apiserver
# answers 200, stores every field it knows, and silently drops the one it does
# not. For `timeZone` that failure is invisible and expensive — backups would
# genuinely run, on a schedule in the wrong zone, with the CLI reporting the
# zone it thought it had set. Assert the field survives a round trip rather
# than assuming the CRD carries what crd-check says it carries.
echo "==> regression: PlatformStack spec.backup.timeZone must round-trip (2.22g / D2)"
kubectl --context "$CTX" create namespace crd-validate >/dev/null 2>&1 || true
cat >/tmp/crd-validate-tz.yaml <<'YAML'
apiVersion: apprafter.io/v1alpha1
kind: PlatformStack
metadata:
  name: crd-validate-tz
  namespace: crd-validate
spec:
  channel: stable
  autoUpgrade: false
  source:
    upstream: oci://ghcr.io/apprafter/charts
    repoURL: oci://ghcr.io/apprafter/charts
    checkInterval: 6h
  values:
    tier: 1
  backup:
    enabled: true
    bucket: s3:https://example.invalid/bucket
    credentialRef:
      name: crd-validate-creds
    schedule: "30 22 * * *"
    checkSchedule: ""
    stagingMode: monolithic
    checkReadData: false
    timeZone: Europe/Belgrade
    clusterName: crd-validate-cluster
YAML
if ! kubectl --context "$CTX" apply -f /tmp/crd-validate-tz.yaml >/dev/null 2>/tmp/crd-tz-err.txt; then
    echo "==> REGRESSION: apiserver REJECTED spec.backup.timeZone" >&2
    cat /tmp/crd-tz-err.txt >&2
    exit 1
fi
_tz=$(kubectl --context "$CTX" -n crd-validate get platformstack crd-validate-tz \
    -o jsonpath='{.spec.backup.timeZone}' 2>/dev/null || true)
if [ "$_tz" = "Europe/Belgrade" ]; then
    echo "    OK: spec.backup.timeZone stored (not pruned)"
else
    echo "==> REGRESSION: spec.backup.timeZone was PRUNED — read back '${_tz}'." >&2
    echo "    The apply succeeded and the field vanished, which is how a backup" >&2
    echo "    ends up running in the wrong zone with the CLI reporting the right one." >&2
    exit 1
fi
# E1, same failure mode. `clusterName` is the restic `--host` every snapshot
# of this cluster is listed under; pruned, the apply succeeds, the CLI reports
# the name it set, and the repository goes on listing every row under the
# anonymous `apprafter-backup` host that made two clusters indistinguishable.
_cn=$(kubectl --context "$CTX" -n crd-validate get platformstack crd-validate-tz \
    -o jsonpath='{.spec.backup.clusterName}' 2>/dev/null || true)
if [ "$_cn" = "crd-validate-cluster" ]; then
    echo "    OK: spec.backup.clusterName stored (not pruned)"
else
    echo "==> REGRESSION: spec.backup.clusterName was PRUNED — read back '${_cn}'." >&2
    exit 1
fi
_cs=$(kubectl --context "$CTX" -n crd-validate get platformstack crd-validate-tz \
    -o jsonpath='{.spec.backup.checkSchedule}' 2>/dev/null; echo "|")
if [ "$_cs" = "|" ]; then
    echo "    OK: an empty checkSchedule is accepted (the --check off path)"
else
    echo "==> REGRESSION: empty checkSchedule did not survive: '${_cs}'" >&2
    exit 1
fi

# A4, the same pruning failure mode one level up. `spec.firewall` is the ONLY
# record of the Cloudflare origin-firewall intent that a backup can see — the
# toggle itself is a cloud object the CLI reconciles from the operator's local
# target store. Pruned, every write succeeds, every read says "the cluster
# never recorded one", and a restore onto a new machine brings the node up
# with 80/443 open to the internet without a word. Nothing else in the
# repository would catch it: the CRD stays structurally valid either way.
echo "==> regression: PlatformStack spec.firewall.cloudflareOrigin must round-trip (A4)"
kubectl --context "$CTX" -n crd-validate patch platformstack crd-validate-tz \
    --type=merge -p '{"spec":{"firewall":{"cloudflareOrigin":true}}}' >/dev/null 2>/tmp/crd-fw-err.txt || {
    echo "==> REGRESSION: apiserver REJECTED spec.firewall.cloudflareOrigin" >&2
    cat /tmp/crd-fw-err.txt >&2
    exit 1
}
_fw=$(kubectl --context "$CTX" -n crd-validate get platformstack crd-validate-tz \
    -o jsonpath='{.spec.firewall.cloudflareOrigin}' 2>/dev/null || true)
if [ "$_fw" = "true" ]; then
    echo "    OK: spec.firewall.cloudflareOrigin stored (not pruned)"
else
    echo "==> REGRESSION: spec.firewall.cloudflareOrigin was PRUNED — read back '${_fw}'." >&2
    echo "    The patch succeeded and the field vanished, which is how a restored" >&2
    echo "    cluster comes up with its 80/443 open while the source had them shut." >&2
    exit 1
fi

# 2.28 / ADR 0065 §1. `spec.base.probes` is FULLY STRUCTURAL, so an
# undeclared key is PRUNED: the apiserver answers 200, stores what it knows
# and silently drops the rest. For a probe that failure is the expensive kind
# — the pod comes up with no health check at all while `kubectl apply` said
# nothing — so assert the fields survive a round trip rather than trusting
# that crd-check's field comparison implies the apiserver stores them.
#
# The same block also proves the `^/` pattern is LIVE. Both halves are needed:
# a round-trip test alone passes against a CRD whose pattern was dropped, and
# a pattern test alone passes against one that prunes every probe field.
echo "==> regression: Application probes must round-trip and enforce ^/ (2.28)"
kubectl --context "$CTX" create namespace crd-validate >/dev/null 2>&1 || true
cat >/tmp/crd-validate-probes.yaml <<'YAML'
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: crd-validate-probes
  namespace: crd-validate
spec:
  base:
    image: example.com/app:latest
    expose:
      port: 8080
    probes:
      readiness:
        path: /healthz
        periodSeconds: 3
      liveness:
        path: /livez
        scheme: https
        headers:
          X-Probe: apprafter
      startup:
        port: 8080
        failureThreshold: 60
YAML
if ! kubectl --context "$CTX" apply -f /tmp/crd-validate-probes.yaml \
    >/dev/null 2>/tmp/crd-probe-err.txt; then
    echo "==> REGRESSION: apiserver REJECTED a valid probes block" >&2
    cat /tmp/crd-probe-err.txt >&2
    exit 1
fi
_probe_path=$(kubectl --context "$CTX" -n crd-validate get application crd-validate-probes \
    -o jsonpath='{.spec.base.probes.readiness.path}' 2>/dev/null || true)
_probe_period=$(kubectl --context "$CTX" -n crd-validate get application crd-validate-probes \
    -o jsonpath='{.spec.base.probes.readiness.periodSeconds}' 2>/dev/null || true)
_probe_header=$(kubectl --context "$CTX" -n crd-validate get application crd-validate-probes \
    -o jsonpath='{.spec.base.probes.liveness.headers.X-Probe}' 2>/dev/null || true)
if [ "$_probe_path" = "/healthz" ] && [ "$_probe_period" = "3" ] && [ "$_probe_header" = "apprafter" ]; then
    echo "    OK: probe path, timing and headers stored (not pruned)"
else
    echo "==> REGRESSION: a probe field was PRUNED — read back path='${_probe_path}'," >&2
    echo "    periodSeconds='${_probe_period}', header='${_probe_header}'." >&2
    echo "    The apply succeeded and the field vanished, which is how a workload" >&2
    echo "    comes up unchecked while its manifest says otherwise." >&2
    exit 1
fi

if kubectl --context "$CTX" apply --dry-run=server -f - >/dev/null 2>/tmp/crd-probe-bad.txt <<'YAML'
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: crd-validate-probes-bad
  namespace: crd-validate
spec:
  base:
    image: example.com/app:latest
    expose:
      port: 8080
    probes:
      readiness:
        path: healthz
YAML
then
    echo "==> REGRESSION: the apiserver ACCEPTED a probe path with no leading '/'." >&2
    echo "    The ^/ pattern in schemas/crdmeta/meta.cue is not reaching the CRD," >&2
    echo "    so the only thing rejecting it is the webhook — and a cluster whose" >&2
    echo "    webhook is unavailable would take it." >&2
    exit 1
else
    echo "    OK: a probe path without a leading '/' is rejected by the apiserver"
fi

echo "==> CRD apiserver validation PASSED (all CRDs accepted + Established)"
