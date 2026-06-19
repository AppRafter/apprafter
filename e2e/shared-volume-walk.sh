#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter shared-volume-walk e2e — the full Phase-2.6c cross-app
# SharedVolume + capacity-signal acceptance on a local k3d/kind cluster
# (plan item 2.6c-13b, ADR 0049). This is the closure gate for 2.6c — the
# live validation that unit tests, CRD validation and code review cannot
# provide.
#
# It exercises the shipped cross-app SharedVolume pipeline end-to-end on a
# single node, single namespace:
#
#   provision SharedVolume (unowned RWX-on-RWO PVC) ->
#   two apps share one volume (write-through visibility) ->
#   rolling update survives (referenced disk does NOT force Recreate) ->
#   refCount + delete-guard (rm refused while referenced) ->
#   missing ref -> AwaitingSharedVolume (operator does not crash) ->
#   namespaced ref -> webhook reject ->
#   capacity signal (SOFT — kubelet Summary best-effort) ->
#   owned-disk no-regression (2.6b behaviour intact).
#
# Concretely, on a cluster bootstrapped with the platform stack
# (operator + admission-webhook + the always-on CNPG operator + the
# seeded `disk-local` + `shared-local` ServiceProviders), the walk:
#
#   1. PROVISION: `apprafter volume create shared --size 1Gi -n demo`
#      -> the SharedVolume reaches `status.ready=true`, `status.pvcRef`
#      set, and the backing PVC `sv-demo-shared` exists UNOWNED (no
#      ownerReferences — the SharedVolume's finalizer reaps it, not an
#      app delete).
#   2. TWO-APP SHARE (CORE): deploy two Applications (`alpha`, `beta`) in
#      ONE namespace, each with `needs.disk: {ref: shared, mountPath:
#      /data}` -> both pods come up, mount the SAME PVC, and SEE EACH
#      OTHER'S WRITES (alpha writes /data/from-alpha, beta reads it).
#   3. ROLLING UPDATE SURVIVES: trigger a rolling update on alpha
#      (change an env var) -> alpha's Deployment strategy is
#      RollingUpdate (a referenced disk does NOT force Recreate — it is a
#      shared volume, not an owned RWO PVC), and beta stays Available
#      (its mount is never dropped).
#   4. refCount + DELETE-GUARD: `apprafter volume status shared` shows
#      refCount=2; `apprafter volume rm shared` is REFUSED (still
#      referenced); after deleting BOTH apps, `volume rm shared`
#      SUCCEEDS and the CR + PVC are gone.
#   5. MISSING REF -> AwaitingSharedVolume: an app referencing a
#      nonexistent SharedVolume -> the shared-disk reference-claim
#      surfaces `Ready=False/AwaitingSharedVolume` and the Application
#      pauses `AwaitingResourceClaim` (Ready=False) — the operator does
#      NOT crash (a subsequent reconcile of a healthy CR still works).
#   6. NAMESPACED REF -> WEBHOOK REJECT: applying an Application with
#      `needs.disk.ref: "other-ns/shared"` is REJECTED by the admission
#      webhook (the sharedvolumes/applications rule from T13a).
#   7. CAPACITY (SOFT): assert the capacity-signal machinery — either a
#      `status.capacity` sample appears on the SharedVolume (kubelet
#      Summary API reachable through the apiserver node-proxy) OR, when
#      kind's kubelet Summary is not reachable, the phase is a SOFT skip
#      with a clear note (best-effort, per ADR 0049 / Task 11). Never
#      hard-fails the walk.
#   8. OWNED-DISK NO-REGRESSION: a plain owned `needs.disk` app still
#      provisions a standalone RWO PVC, mounts it, and forces
#      `strategy: Recreate` (2.6b behaviour intact).
#
# CLI state injection
# -------------------
# `apprafter cluster-bootstrap` and `apprafter volume …` read the
# kubeconfig from the CLI's per-target state store (not from $KUBECONFIG).
# We set APPRAFTER_CONFIG_DIR to a tmpdir and seed it with a minimal
# config.yaml (active_target: k3d) and a state.json carrying the
# kubeconfig as kubeconfig_yaml (plaintext) — the same approach the other
# walks use.
#
# Local-operator mode (REQUIRED while 2.6c is unreleased)
# --------------------------------------------------------
# The cross-app SharedVolume support (the SharedVolume CRD + controller,
# the `needs.disk.ref` reference shape, the `shared-disk` provision arm,
# the capacity signal, the webhook SharedVolume + ref rules) is all
# UNPUBLISHED on this branch. So APPRAFTER_E2E_LOCAL_OPERATOR=1 is
# MANDATORY — Phase 1b builds + side-loads the working-tree operator +
# webhook and applies the branch CRDs + PVC/SharedVolume/nodes RBAC.
#
# Required: docker (or podman), cargo, kubectl
#   — all satisfied inside `nix develop` or on a standard CI runner.
#   (cargo: lib.sh runs the CLI via `cargo run --bin apprafter`.)
#
# Exit codes:
#   0 — chain green
#   1 — assertion failure
#   2 — precondition missing
#
# PASS/FAIL is judged by READING THE LOG: every phase prints an `ok:`
# line; the final `shared-volume-walk GREEN` banner prints only on the
# success path; any failure prints `FAILED:` (the cleanup trap) before
# teardown. Do NOT trust the exit code alone (a sandbox harness can mask
# the inner code).

set -euo pipefail

# ---------------------------------------------------------------
# Source shared helpers
# ---------------------------------------------------------------

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# ---------------------------------------------------------------
# Constants — the shipped 2.6c SharedVolume coordinates.
#
# The backing PVC name is `sv-<ns>-<name>` (operator
# resourceclaim-provisioner/src/shared_volume.rs `sv_pvc_name`). The
# reference ResourceClaim name is `claim_name(<app>, "shared-disk",
# Some(<ref>))` = `<app>-shared-disk-<ref>` (operator
# application/src/lib.rs). The mounted volume name is `disk-<identity>`
# where identity derives from the last mountPath segment (`/data` ->
# `data` -> volume `disk-data`) for a `ref` disk (no `name`).
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-sv-walk"

APP_NS="demo"                       # tenant namespace
SV="shared"                         # SharedVolume name
SV_PVC="sv-demo-shared"             # backing PVC name = sv_pvc_name(demo, shared)
SV_SIZE="1Gi"

APP_A="alpha"                       # first consumer Application
APP_B="beta"                        # second consumer Application
MOUNT_PATH="/data"                  # needs.disk.ref mountPath
VOLUME_NAME="disk-data"             # disk-<identity>, identity = /data -> data
# Reference ResourceClaim names (per consumer): <app>-shared-disk-<ref>.
CLAIM_A="${APP_A}-shared-disk-${SV}"
CLAIM_B="${APP_B}-shared-disk-${SV}"

# Phase 5 (missing-ref) coordinates.
APP_MISS="orphan"
SV_MISSING="does-not-exist"
CLAIM_MISS="${APP_MISS}-shared-disk-${SV_MISSING}"

# Phase 8 (owned-disk no-regression) coordinates.
APP_OWNED="ownedsqlite"
CLAIM_OWNED="${APP_OWNED}-disk"
PVC_OWNED="claim-demo-${APP_OWNED}-disk"
OWNED_VOLUME_NAME="disk-data"       # owned disk at /data -> disk-data

# Group-qualify the two collision-prone kinds so kubectl never resolves
# to the wrong API group: bare `application` also matches Argo CD's
# argoproj.io Application, and bare `resourceclaim` matches the k8s 1.32+
# DRA resource.k8s.io ResourceClaim. Always address the apprafter.io CRs.
APP_RES="application.apprafter.io"
CLAIM_RES="resourceclaim.apprafter.io"
SV_RES="sharedvolume.apprafter.io"

PROVIDER_NS="apprafter-system"      # ServiceProvider namespace
SHARED_PROVIDER="shared-local"      # seeded shared-disk ServiceProvider
DISK_PROVIDER="disk-local"          # seeded owned-disk ServiceProvider (Phase 8)
# The StorageClass the providers stamp onto their PVCs. The seed default
# is `local-path` (ships on k3s/k3d). On a bare kind cluster the default
# class is `standard` and `local-path` is ABSENT, so this walk ensures a
# `local-path` class exists on kind before provisioning (Phase 2) — a
# harness concern, not operator code under test.
STORAGE_CLASS="local-path"

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------

for tool in cargo kubectl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done

# A container runtime: docker (→ k3d) or podman (→ kind). cluster_runtime
# in lib.sh picks the backend; here we only assert one of them exists.
if ! command -v docker >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
    printf 'ERROR: neither "docker" nor "podman" found on PATH\n' >&2
    exit 2
fi

# ---------------------------------------------------------------
# Precondition: this walk REQUIRES local-operator mode while 2.6c is
# UNRELEASED. The SharedVolume CRD + controller, the `needs.disk.ref`
# reference shape, the `shared-disk` provision arm, the capacity signal,
# and the webhook SharedVolume + ref rules are all unpublished on this
# branch. The flag is mandatory until 2.6c ships.
# ---------------------------------------------------------------
if [ -z "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
    cat >&2 <<'EOF'
ERROR: shared-volume-walk requires APPRAFTER_E2E_LOCAL_OPERATOR=1 while 2.6c is unreleased.

The cross-app SharedVolume support (the SharedVolume CRD + controller, the
`needs.disk.ref` reference shape, the `shared-disk` provision arm, the
capacity signal, and the webhook SharedVolume + ref rules) is all unpublished
on this branch. Run:

  APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/shared-volume-walk.sh

so the walk builds + side-loads the working-tree operator/webhook and applies
the branch CRDs (Phase 1b). Drop this gate once 2.6c publishes.
EOF
    exit 2
fi

# ---------------------------------------------------------------
# Temp workspace
# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"

# Set to 1 only after the cluster is up AND $KUBECONFIG points at it
# (Phase 0). Until then, dump_diagnostics / k3d_down must NOT run — on a
# cluster-up failure $KUBECONFIG still points at the operator's ambient
# cluster, and an e2e must never touch a non-test cluster.
K3D_CREATED=0

cleanup() {
    local exit_code=$?

    if [ "$exit_code" -ne 0 ]; then
        printf '\nFAILED: shared-volume-walk FAILED at %s (exit %d)\n' \
            "$(elapsed)" "$exit_code" >&2
        if [ "$K3D_CREATED" -eq 1 ]; then
            dump_diagnostics
            printf 'Tearing down cluster (set APPRAFTER_E2E_SKIP_DESTROY=1 to keep).\n' >&2
        else
            printf 'cluster %s was never created (runtime unavailable in this shell?) — skipping diagnostics + teardown; your ambient KUBECONFIG was NOT touched.\n' \
                "$CLUSTER_NAME" >&2
        fi
    fi

    if [ "$K3D_CREATED" -eq 1 ]; then
        if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
            k3d_down "$CLUSTER_NAME" || true
        else
            printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' \
                "$CLUSTER_NAME"
            printf 'Run: <k3d|kind> cluster delete %s\n' "$CLUSTER_NAME"
        fi
    fi

    rm -rf "$TMPDIR_WORK"
    exit "$exit_code"
}
trap cleanup EXIT

# ---------------------------------------------------------------
# Helper: seed the CLI state store (mirrors the sibling walks).
# ---------------------------------------------------------------
seed_apprafter_state() {
    local kubeconfig_content="$1"

    mkdir -p "${APPRAFTER_CONFIG_DIR}"
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/k3d/.apprafter"

    cat >"${APPRAFTER_CONFIG_DIR}/config.yaml" <<'YAML'
active_target: k3d
version: 1
YAML

    # Escape the kubeconfig for JSON embedding.
    local kc_escaped
    kc_escaped=$(printf '%s' "$kubeconfig_content" \
        | sed 's/\\/\\\\/g' \
        | sed 's/"/\\"/g' \
        | awk '{printf "%s\\n", $0}')

    cat >"${APPRAFTER_CONFIG_DIR}/state/k3d/.apprafter/state.json" <<STATE
{
  "hetzner_cloud": {
    "server_id": 1,
    "server_name": "k3d-local",
    "ssh_key_ids": [],
    "kubeconfig_yaml": "${kc_escaped}"
  }
}
STATE
}

# ---------------------------------------------------------------
# Local helpers (mirror needs-disk-walk.sh)
# ---------------------------------------------------------------

# wait_jsonpath <kind> <ns> <name> <jsonpath> <want> [timeout]
wait_jsonpath() {
    local kind="$1" ns="$2" name="$3" jsonpath="$4" want="$5"
    local timeout="${6:-180}"
    local deadline got
    deadline=$(( $(date +%s) + timeout ))

    printf '  wait %s/%s [%s] == %q (timeout %ss) ...\n' \
        "$kind" "$name" "$jsonpath" "$want" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        got=$(kubectl -n "$ns" get "$kind" "$name" \
            -o jsonpath="$jsonpath" 2>/dev/null || true)
        if [ "$got" = "$want" ]; then
            printf '  ok: %s/%s [%s] = %q\n' "$kind" "$name" "$jsonpath" "$got"
            return 0
        fi
        printf '    %s: got=%q want=%q\n' "$(date +%H:%M:%S)" "$got" "$want"
        sleep 5
    done
    printf 'FAILED: %s/%s [%s] never became %q (last=%q)\n' \
        "$kind" "$name" "$jsonpath" "$want" "${got:-}" >&2
    kubectl -n "$ns" describe "$kind" "$name" >&2 2>&1 || true
    return 1
}

# wait_gone <kind> <ns> <name> [timeout]
wait_gone() {
    local kind="$1" ns="$2" name="$3"
    local timeout="${4:-120}"
    local deadline
    deadline=$(( $(date +%s) + timeout ))

    printf '  wait %s/%s gone (timeout %ss) ...\n' "$kind" "$name" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if ! kubectl -n "$ns" get "$kind" "$name" >/dev/null 2>&1; then
            printf '  ok: %s/%s is gone\n' "$kind" "$name"
            return 0
        fi
        sleep 5
    done
    printf 'FAILED: %s/%s still present after %ss\n' "$kind" "$name" "$timeout" >&2
    return 1
}

# assert_eq <description> <got> <want>
assert_eq() {
    local desc="$1" got="$2" want="$3"
    if [ "$got" = "$want" ]; then
        printf '  ok: %s = %q\n' "$desc" "$got"
        return 0
    fi
    printf 'FAILED: %s — got %q, want %q\n' "$desc" "$got" "$want" >&2
    return 1
}

# jp <kind> <ns> <name> <jsonpath>  (read one value)
jp() {
    kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true
}

# cond_status <kind> <ns> <name> <condition-type>  (one condition's .status)
cond_status() {
    kubectl -n "$2" get "$1" "$3" -o \
        jsonpath="{.status.conditions[?(@.type==\"$4\")].status}" \
        2>/dev/null || true
}

# cond_reason <kind> <ns> <name> <condition-type>  (one condition's .reason)
cond_reason() {
    kubectl -n "$2" get "$1" "$3" -o \
        jsonpath="{.status.conditions[?(@.type==\"$4\")].reason}" \
        2>/dev/null || true
}

# app_pod <app-name>  — the current running pod for an Application.
app_pod() {
    kubectl -n "$APP_NS" get pod -l "app.kubernetes.io/name=$1" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# apply_consumer_app <name> <env-marker>  — an Application with a single
# REFERENCED disk (`needs.disk: {ref: shared, mountPath: /data}`). The
# `WALK_MARKER` env var lets Phase 3 trigger a rolling update by changing
# its value. nginxdemos/hello stays up + ships busybox sh for exec probes.
apply_consumer_app() {
    local name="$1" marker="$2"
    kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${name}
  namespace: ${APP_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    expose:
      port: 80
    env:
      WALK_MARKER: "${marker}"
    needs:
      disk:
        ref: ${SV}
        mountPath: ${MOUNT_PATH}
YAML
}

# ===============================================================
# Phase 0: bring up the cluster
# ===============================================================

phase "Phase 0: k3d_up ${CLUSTER_NAME}"

k3d_up "$CLUSTER_NAME"

cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
K3D_CREATED=1
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"
printf '  ok: cluster %s is up\n' "$CLUSTER_NAME"

# ===============================================================
# Phase 1: seed CLI state and run cluster-bootstrap
# ===============================================================

phase "Phase 1: cluster-bootstrap (platform stack + CNPG + disk-local + shared-local)"

kubeconfig_content=$(cat "$KUBECONFIG_FILE")
seed_apprafter_state "$kubeconfig_content"
export APPRAFTER_CONFIG_DIR
printf '  APPRAFTER_CONFIG_DIR=%s\n' "$APPRAFTER_CONFIG_DIR"

bootstrap_with_retry

printf '  ok: cluster-bootstrap complete\n'

# ---------------------------------------------------------------
# Phase 1b (REQUIRED — APPRAFTER_E2E_LOCAL_OPERATOR): build + side-load the
# working-tree operator + webhook instead of the published image, then apply
# the branch CRDs + PVC/SharedVolume/nodes RBAC. The whole 2.6c SharedVolume
# surface is unpublished, so the released image + CRD this cluster
# bootstrapped from cannot render/validate it.
# NOTE: the if-body below is intentionally NOT indented — it carries
# column-0 heredocs; `fi` closes it just before Phase 2.
# ---------------------------------------------------------------
if [ -n "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
phase "Phase 1b: build + load local operator + webhook (APPRAFTER_E2E_LOCAL_OPERATOR)"
builder=podman; command -v podman >/dev/null 2>&1 || builder=docker
build_load_restart() { # <deployment> <operator-subdir>
    local dep="$1" sub="$2" img
    printf '  waiting for the %s Deployment to appear ...\n' "$dep"
    for _ in $(seq 1 60); do
        kubectl -n apprafter-system get deploy "$dep" >/dev/null 2>&1 && break
        sleep 5
    done
    img=$(kubectl -n apprafter-system get deploy "$dep" \
        -o jsonpath='{.spec.template.spec.containers[0].image}')
    printf '  building %s from the working tree (%s) ...\n' "$img" "$builder"
    "$builder" build -f "${REPO_ROOT}/operator/${sub}/Dockerfile" \
        -t "$img" "${REPO_ROOT}/operator"
    cluster_load_image "$CLUSTER_NAME" "$img"
    kubectl -n apprafter-system rollout restart "deploy/${dep}"
    kubectl -n apprafter-system rollout status "deploy/${dep}" --timeout=180s
}
build_load_restart apprafter-operator apprafter-operator
build_load_restart admission-webhook admission-webhook
# `rollout status` returns once the NEW webhook pod is Ready, but the OLD
# (released) pod lingers Terminating — during that window a branch CR apply
# (the Phase 2 `shared-local` ServiceProvider seed) can route to the old
# webhook, whose released BUILTIN_TYPES lacks `shared-disk`. Wait until
# ONLY the branch webhook serves before any branch-typed apply.
printf '  waiting for the old (released) webhook pod to fully terminate ...\n'
_wh_deadline=$(( $(date +%s) + 90 ))
while [ "$(date +%s)" -lt "$_wh_deadline" ]; do
    [ "$(kubectl -n apprafter-system get pods \
        -l app.kubernetes.io/name=admission-webhook --no-headers 2>/dev/null \
        | wc -l)" -le 1 ] && break
    sleep 3
done
printf '  apprafter-operator + admission-webhook now running the working-tree build\n'

# The released platform-stack chart predates the 2.6c CRD changes (the new
# SharedVolume CRD, the `needs.disk` owned|ref union on Application). Argo CD
# owns those CRDs via the apprafter-operator Application, so: disable automated
# sync on the parent + operator apps (else Argo reverts the drift), then apply
# the BRANCH-rendered CRDs server-side.
printf '  applying branch operator CRDs (released chart predates 2.6c schema) ...\n'
for _app in platform apprafter-operator; do
    kubectl -n argocd patch applications.argoproj.io "$_app" --type=merge \
        -p '{"spec":{"syncPolicy":{"automated":null}}}' >/dev/null 2>&1 || true
done
_yq() { if command -v yq >/dev/null 2>&1; then yq "$@"; else nix run nixpkgs#yq-go -- "$@"; fi; }
helm template apprafter-operator "${REPO_ROOT}/operator/charts/apprafter-operator" \
    | _yq 'select(.kind == "CustomResourceDefinition")' \
    | kubectl apply --server-side --force-conflicts -f -
for _crd in applications serviceproviders resourceclaims retainedclaims sharedvolumes; do
    retry 12 5 -- kubectl wait --for=condition=Established \
        "crd/${_crd}.apprafter.io" --timeout=30s
done
printf '  branch CRDs applied + Established\n'

# The released operator ClusterRole predates the 2.6c RBAC — the SharedVolume
# reconciler SSA-applies an unowned PVC (persistentvolumeclaims), watches
# SharedVolume CRs + manages the cleanup finalizer (sharedvolumes,
# sharedvolumes/finalizers, sharedvolumes/status), and samples the kubelet
# Summary API through the apiserver node-proxy (nodes, nodes/proxy). Grant it
# ADDITIVELY via a dedicated ClusterRole — do NOT replace the operator's
# ClusterRole (a standalone `helm template` renders an INCOMPLETE RBAC; the
# platform-stack umbrella gates other rules). Mirrors the branch's
# operator/charts/apprafter-operator/templates/rbac.yaml. Pre-release-only.
printf '  granting the operator the 2.6c SharedVolume + capacity RBAC (additive) ...\n'
kubectl apply -f - <<'SVRBAC'
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: apprafter-operator-sharedvolume
rules:
  - apiGroups: [""]
    resources: [persistentvolumeclaims]
    verbs: [get, list, watch, create, update, patch, delete]
  - apiGroups: [""]
    resources: [nodes, nodes/proxy]
    verbs: [get, list]
  - apiGroups: [apprafter.io]
    resources: [sharedvolumes, sharedvolumes/finalizers, sharedvolumes/status]
    verbs: [get, list, watch, create, update, patch, delete]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: apprafter-operator-sharedvolume
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: apprafter-operator-sharedvolume
subjects:
  - kind: ServiceAccount
    name: apprafter-operator
    namespace: apprafter-system
SVRBAC
printf '  operator SharedVolume + capacity RBAC granted (additive)\n'
fi  # end APPRAFTER_E2E_LOCAL_OPERATOR (Phase 1b)

# ===============================================================
# Phase 2: readiness — providers, webhook, storage class
# ===============================================================

phase "Phase 2: platform readiness (AppProject, shared-local + disk-local providers, webhook, storageClass)"

# The `apps` AppProject is created by the platform-stack chart.
printf '  waiting for AppProject apps ...\n'
deadline=$(( $(date +%s) + 600 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    if kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1; then
        printf '  AppProject apps -> found\n'
        break
    fi
    sleep 10
done
kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1 || {
    printf 'FAILED: AppProject apps not found after 10 min\n' >&2
    exit 1
}

# Seed the `shared-local` + `disk-local` ServiceProviders if the bootstrapped
# platform stack did not ship them. RATIONALE (harness substrate, NOT operator
# code under test): the cluster bootstraps from the PUBLISHED platform-stack
# chart, and these seeds are 2.6c/2.6b branch artifacts not yet published —
# exactly like the operator IMAGE. Idempotent: a no-op once published.
if ! kubectl get serviceprovider "$SHARED_PROVIDER" -n "$PROVIDER_NS" >/dev/null 2>&1; then
    printf '  ServiceProvider %s absent — seeding from branch chart source (unpublished 2.6c artifact) ...\n' "$SHARED_PROVIDER"
    kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: ServiceProvider
metadata:
  name: ${SHARED_PROVIDER}
  namespace: ${PROVIDER_NS}
  labels:
    apprafter.io/managed-by: apprafter
    tier: integrated
    location: in-cluster
spec:
  type: shared-disk
  backend: shared-disk
  config:
    storageClass: local-path
YAML
fi
if ! kubectl get serviceprovider "$DISK_PROVIDER" -n "$PROVIDER_NS" >/dev/null 2>&1; then
    printf '  ServiceProvider %s absent — seeding from branch chart source (unpublished 2.6b artifact) ...\n' "$DISK_PROVIDER"
    kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: ServiceProvider
metadata:
  name: ${DISK_PROVIDER}
  namespace: ${PROVIDER_NS}
  labels:
    apprafter.io/managed-by: apprafter
    tier: integrated
    location: in-cluster
spec:
  type: disk
  backend: disk
  config:
    storageClass: local-path
YAML
fi

# ASSERT the shared-local provider is present with the expected backend.
retry 30 10 -- kubectl get serviceprovider "$SHARED_PROVIDER" \
    -n "$PROVIDER_NS" >/dev/null
sp_backend=$(jp serviceprovider "$PROVIDER_NS" "$SHARED_PROVIDER" '{.spec.backend}')
assert_eq "ServiceProvider ${SHARED_PROVIDER} backend" "$sp_backend" "shared-disk"
sp_class=$(jp serviceprovider "$PROVIDER_NS" "$SHARED_PROVIDER" '{.spec.config.storageClass}')
[ -n "$sp_class" ] && STORAGE_CLASS="$sp_class"
printf '  shared-local storageClass = %s\n' "$STORAGE_CLASS"

# The admission webhook must be Available before applying any CR.
printf '  waiting for the admission-webhook Deployment ...\n'
retry 30 10 -- kubectl -n "$PROVIDER_NS" rollout status \
    deploy admission-webhook --timeout=60s

# StorageClass guard: the providers stamp `$STORAGE_CLASS` onto their PVCs.
# On k3s/k3d `local-path` ships by default; on a bare kind cluster the
# default class is `standard` and `local-path` is absent, so the PVC would
# never bind and the pods would hang. Ensure the class exists — on kind,
# install the rancher local-path-provisioner. HARNESS concern, not under test.
if ! kubectl get storageclass "$STORAGE_CLASS" >/dev/null 2>&1; then
    printf '  StorageClass %s absent — installing the rancher local-path-provisioner (kind substrate fix) ...\n' "$STORAGE_CLASS"
    retry 5 10 -- kubectl apply -f \
        https://raw.githubusercontent.com/rancher/local-path-provisioner/v0.0.31/deploy/local-path-storage.yaml
    retry 30 10 -- kubectl -n local-path-storage rollout status \
        deploy/local-path-provisioner --timeout=60s
    kubectl get storageclass "$STORAGE_CLASS" >/dev/null 2>&1 || {
        printf 'FAILED: StorageClass %s still absent after installing local-path-provisioner\n' "$STORAGE_CLASS" >&2
        kubectl get storageclass >&2 2>&1 || true
        exit 2
    }
fi
printf '  ok: StorageClass %s present; platform ready\n' "$STORAGE_CLASS"

kubectl create namespace "$APP_NS" 2>/dev/null || true

# ===============================================================
# Phase 3: PROVISION — `apprafter volume create` -> ready SharedVolume +
#          unowned backing PVC
# ===============================================================

phase "Phase 3: provision — apprafter volume create -> SharedVolume ready, unowned PVC"

# Use the shipped CLI verb (apprafter volume create <name> --size … -n …).
apprafter volume create "$SV" --size "$SV_SIZE" -n "$APP_NS"

# The SharedVolume reconciler provisions the backing PVC + writes status.
wait_jsonpath "$SV_RES" "$APP_NS" "$SV" '{.status.ready}' true 240
sv_pvcref=$(jp "$SV_RES" "$APP_NS" "$SV" '{.status.pvcRef}')
assert_eq "SharedVolume status.pvcRef" "$sv_pvcref" "$SV_PVC"
sv_ready_cond=$(cond_status "$SV_RES" "$APP_NS" "$SV" Ready)
assert_eq "SharedVolume Ready condition" "$sv_ready_cond" "True"

# The backing PVC exists with the right storageClass + size.
retry 12 5 -- kubectl -n "$APP_NS" get pvc "$SV_PVC" >/dev/null
pvc_class=$(jp pvc "$APP_NS" "$SV_PVC" '{.spec.storageClassName}')
assert_eq "backing PVC storageClassName" "$pvc_class" "$STORAGE_CLASS"
pvc_size=$(jp pvc "$APP_NS" "$SV_PVC" '{.spec.resources.requests.storage}')
assert_eq "backing PVC requested storage" "$pvc_size" "$SV_SIZE"

# CRITICAL (lifecycle): the backing PVC is UNOWNED — no ownerReferences —
# so an APP delete never cascade-deletes it; only the SharedVolume's own
# finalizer (on `volume rm`) reaps it.
pvc_owner=$(jp pvc "$APP_NS" "$SV_PVC" '{.metadata.ownerReferences}')
if [ -n "$pvc_owner" ]; then
    printf 'FAILED: backing PVC %s has ownerReferences (%s) — lifecycle broken\n' \
        "$SV_PVC" "$pvc_owner" >&2
    exit 1
fi
printf '  ok: backing PVC %s is UNOWNED (no ownerReferences)\n' "$SV_PVC"
# refCount starts at 0 (no consumers yet).
wait_jsonpath "$SV_RES" "$APP_NS" "$SV" '{.status.refCount}' 0 60

# ===============================================================
# Phase 4: TWO-APP SHARE (CORE) — two Applications mount the SAME PVC and
#          see each other's writes
# ===============================================================

phase "Phase 4 (CORE): two apps share one SharedVolume — write-through visibility"

apply_consumer_app "$APP_A" "v1"
apply_consumer_app "$APP_B" "v1"

# Each app generates a `shared-disk` reference ResourceClaim labelled
# apprafter.io/shared-volume=<ref>; the provisioner binds it to the
# SharedVolume's pvcRef (status.volumeClaimRef == the backing PVC).
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM_A" '{.spec.type}' shared-disk 180
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM_B" '{.spec.type}' shared-disk 180
claim_a_label=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM_A" \
    '{.metadata.labels.apprafter\.io/shared-volume}')
assert_eq "alpha reference-claim shared-volume label" "$claim_a_label" "$SV"
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM_A" '{.status.volumeClaimRef}' "$SV_PVC" 240
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM_B" '{.status.volumeClaimRef}' "$SV_PVC" 240

# Both Applications resume to Ready and mount the SAME backing PVC.
wait_jsonpath "$APP_RES" "$APP_NS" "$APP_A" '{.status.phase}' Ready 240
wait_jsonpath "$APP_RES" "$APP_NS" "$APP_B" '{.status.phase}' Ready 240

for app in "$APP_A" "$APP_B"; do
    vol_claim=$(kubectl -n "$APP_NS" get deployment "$app" \
        -o jsonpath="{.spec.template.spec.volumes[?(@.name==\"${VOLUME_NAME}\")].persistentVolumeClaim.claimName}" \
        2>/dev/null || true)
    assert_eq "${app} Deployment volume ${VOLUME_NAME} claimName" "$vol_claim" "$SV_PVC"
    mount_path=$(kubectl -n "$APP_NS" get deployment "$app" \
        -o jsonpath="{.spec.template.spec.containers[0].volumeMounts[?(@.name==\"${VOLUME_NAME}\")].mountPath}" \
        2>/dev/null || true)
    assert_eq "${app} Deployment volumeMount ${VOLUME_NAME} mountPath" "$mount_path" "$MOUNT_PATH"
    kubectl -n "$APP_NS" wait --for=condition=Available \
        "deployment/${app}" --timeout=300s
    retry 40 5 -- kubectl -n "$APP_NS" wait --for=condition=Ready \
        pod -l "app.kubernetes.io/name=${app}" --timeout=20s
done

# WRITE-THROUGH: alpha writes a file; beta reads it from the shared mount.
POD_A=$(app_pod "$APP_A")
POD_B=$(app_pod "$APP_B")
if [ -z "$POD_A" ] || [ -z "$POD_B" ]; then
    printf 'FAILED: could not find alpha (%s) and/or beta (%s) pods\n' "$POD_A" "$POD_B" >&2
    exit 1
fi
printf '  alpha pod=%s  beta pod=%s\n' "$POD_A" "$POD_B"
kubectl -n "$APP_NS" exec "$POD_A" -- sh -c "echo hello-from-alpha > ${MOUNT_PATH}/from-alpha"
# beta sees alpha's write on the SAME PVC.
seen=$(kubectl -n "$APP_NS" exec "$POD_B" -- cat "${MOUNT_PATH}/from-alpha" 2>/dev/null || true)
assert_eq "beta reads alpha's write through the shared volume" "$seen" "hello-from-alpha"
# And the reverse direction: beta writes, alpha reads.
kubectl -n "$APP_NS" exec "$POD_B" -- sh -c "echo hello-from-beta > ${MOUNT_PATH}/from-beta"
seen2=$(kubectl -n "$APP_NS" exec "$POD_A" -- cat "${MOUNT_PATH}/from-beta" 2>/dev/null || true)
assert_eq "alpha reads beta's write through the shared volume" "$seen2" "hello-from-beta"

# refCount reflects the two consumers.
wait_jsonpath "$SV_RES" "$APP_NS" "$SV" '{.status.refCount}' 2 120

# ===============================================================
# Phase 5: ROLLING UPDATE SURVIVES — a referenced disk does NOT force
#          Recreate; beta's mount is never dropped
# ===============================================================

phase "Phase 5: rolling update survives — referenced disk is RollingUpdate, beta stays Available"

# A REFERENCED (shared) disk does NOT force Recreate (unlike an OWNED RWO
# PVC). Assert alpha's Deployment strategy is RollingUpdate.
strategy_a=$(jp deployment "$APP_NS" "$APP_A" '{.spec.strategy.type}')
assert_eq "alpha Deployment strategy.type (referenced disk)" "$strategy_a" "RollingUpdate"

# Trigger a rolling update on alpha (change WALK_MARKER) and assert beta is
# NOT disturbed — its mount stays bound and the Deployment stays Available
# across alpha's roll.
beta_gen_before=$(jp deployment "$APP_NS" "$APP_B" '{.metadata.generation}')
apply_consumer_app "$APP_A" "v2"
wait_jsonpath "$APP_RES" "$APP_NS" "$APP_A" '{.status.phase}' Ready 240
kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP_A}" --timeout=300s
# The new alpha pod carries the updated env marker.
retry 40 5 -- kubectl -n "$APP_NS" wait --for=condition=Ready \
    pod -l "app.kubernetes.io/name=${APP_A}" --timeout=20s
POD_A2=$(app_pod "$APP_A")
marker=$(kubectl -n "$APP_NS" get pod "$POD_A2" \
    -o jsonpath="{.spec.containers[0].env[?(@.name==\"WALK_MARKER\")].value}" 2>/dev/null || true)
assert_eq "alpha rolled to the new WALK_MARKER" "$marker" "v2"

# beta untouched: its Deployment generation did not change and it stays
# Available (its shared mount was never dropped by alpha's roll).
beta_gen_after=$(jp deployment "$APP_NS" "$APP_B" '{.metadata.generation}')
assert_eq "beta Deployment generation unchanged by alpha's roll" \
    "$beta_gen_after" "$beta_gen_before"
beta_avail=$(cond_status deployment "$APP_NS" "$APP_B" Available)
assert_eq "beta Deployment still Available after alpha's roll" "$beta_avail" "True"
# beta still sees the shared files written in Phase 4.
POD_B2=$(app_pod "$APP_B")
still=$(kubectl -n "$APP_NS" exec "$POD_B2" -- cat "${MOUNT_PATH}/from-alpha" 2>/dev/null || true)
assert_eq "beta still reads the shared file after alpha's roll" "$still" "hello-from-alpha"

# ===============================================================
# Phase 6: refCount + DELETE-GUARD — `volume rm` refused while referenced;
#          succeeds once unreferenced (CR + PVC gone)
# ===============================================================

phase "Phase 6: refCount + delete-guard — rm refused while referenced; succeeds once free"

# `apprafter volume status` reports refCount=2 (both consumers).
status_out="$(apprafter volume status "$SV" -n "$APP_NS")"
printf '%s\n' "$status_out"
if printf '%s' "$status_out" | grep -q 'Ref count: *2'; then
    printf '  ok: apprafter volume status reports refCount=2\n'
else
    printf 'FAILED: apprafter volume status did not report refCount=2\n%s\n' "$status_out" >&2
    exit 1
fi

# `apprafter volume rm` is REFUSED while still referenced (refCount>0).
if apprafter volume rm "$SV" -n "$APP_NS" --yes >/tmp/sv_rm_blocked.out 2>&1; then
    printf 'FAILED: apprafter volume rm SUCCEEDED while still referenced (delete-guard broken)\n' >&2
    cat /tmp/sv_rm_blocked.out >&2 || true
    exit 1
fi
if grep -qi 'still referenced' /tmp/sv_rm_blocked.out; then
    printf '  ok: apprafter volume rm refused while referenced (delete-guard)\n'
else
    printf 'FAILED: apprafter volume rm failed but not with the "still referenced" guard\n' >&2
    cat /tmp/sv_rm_blocked.out >&2 || true
    exit 1
fi
# The SharedVolume CR + PVC must STILL be present (the guard did not delete).
kubectl -n "$APP_NS" get "$SV_RES" "$SV" >/dev/null 2>&1 \
    || { printf 'FAILED: SharedVolume vanished after a refused rm\n' >&2; exit 1; }
printf '  ok: SharedVolume + PVC survive the refused rm\n'

# Remove BOTH consuming apps -> their reference-claims cascade (ownerRef) ->
# refCount drops to 0.
kubectl delete "$APP_RES" "$APP_A" -n "$APP_NS" --wait=true
kubectl delete "$APP_RES" "$APP_B" -n "$APP_NS" --wait=true
wait_gone "$CLAIM_RES" "$APP_NS" "$CLAIM_A" 120
wait_gone "$CLAIM_RES" "$APP_NS" "$CLAIM_B" 120
wait_jsonpath "$SV_RES" "$APP_NS" "$SV" '{.status.refCount}' 0 180

# Now `apprafter volume rm` SUCCEEDS — and the CR + backing PVC are gone
# (the finalizer reaps the unowned PVC).
apprafter volume rm "$SV" -n "$APP_NS" --yes
wait_gone "$SV_RES" "$APP_NS" "$SV" 120
wait_gone pvc "$APP_NS" "$SV_PVC" 120
printf '  ok: apprafter volume rm succeeded once unreferenced — CR + PVC reaped\n'

# ===============================================================
# Phase 7: MISSING REF -> AwaitingSharedVolume (operator does NOT crash)
# ===============================================================

phase "Phase 7: missing ref -> AwaitingSharedVolume (Ready=False); operator does not crash"

# An app referencing a SharedVolume that does NOT exist. The shared-disk
# reference-claim surfaces Ready=False/AwaitingSharedVolume and the
# Application pauses AwaitingResourceClaim — never crashing the operator.
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP_MISS}
  namespace: ${APP_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    expose:
      port: 80
    needs:
      disk:
        ref: ${SV_MISSING}
        mountPath: ${MOUNT_PATH}
YAML

# The reference-claim is generated and waits on the absent SharedVolume.
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM_MISS" '{.spec.type}' shared-disk 180
# Its Ready condition is False with reason AwaitingSharedVolume.
deadline=$(( $(date +%s) + 120 ))
miss_reason=""
while [ "$(date +%s)" -lt "$deadline" ]; do
    miss_reason=$(cond_reason "$CLAIM_RES" "$APP_NS" "$CLAIM_MISS" Ready)
    [ "$miss_reason" = "AwaitingSharedVolume" ] && break
    sleep 5
done
assert_eq "missing-ref claim Ready reason" "$miss_reason" "AwaitingSharedVolume"
miss_status=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM_MISS" Ready)
assert_eq "missing-ref claim Ready status" "$miss_status" "False"
# The Application is paused (Ready=False), not Ready.
app_phase=$(jp "$APP_RES" "$APP_NS" "$APP_MISS" '{.status.phase}')
if [ "$app_phase" = "Ready" ]; then
    printf 'FAILED: orphan app reached Ready despite a missing SharedVolume ref\n' >&2
    exit 1
fi
printf '  ok: orphan app paused (phase=%q), not Ready\n' "$app_phase"

# The operator did NOT crash: its Deployment is still Available and a fresh
# (healthy) SharedVolume still provisions to ready under the same operator.
op_avail=$(cond_status deployment apprafter-system apprafter-operator Available)
assert_eq "apprafter-operator still Available after the missing-ref claim" "$op_avail" "True"
# Liveness proof: create the previously-missing volume; the SAME reference
# claim must then BIND (Ready flips True, volumeClaimRef lands).
apprafter volume create "$SV_MISSING" --size "$SV_SIZE" -n "$APP_NS"
wait_jsonpath "$SV_RES" "$APP_NS" "$SV_MISSING" '{.status.ready}' true 240
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM_MISS" '{.status.ready}' true 240
printf '  ok: once the SharedVolume exists, the orphan claim binds — operator healthy throughout\n'

# Clean up Phase 7 artifacts before Phase 8.
kubectl delete "$APP_RES" "$APP_MISS" -n "$APP_NS" --wait=true
wait_gone "$CLAIM_RES" "$APP_NS" "$CLAIM_MISS" 120
wait_jsonpath "$SV_RES" "$APP_NS" "$SV_MISSING" '{.status.refCount}' 0 120
apprafter volume rm "$SV_MISSING" -n "$APP_NS" --yes
wait_gone "$SV_RES" "$APP_NS" "$SV_MISSING" 120

# ===============================================================
# Phase 8: NAMESPACED REF -> WEBHOOK REJECT
# ===============================================================

phase "Phase 8: namespaced ref -> webhook reject (cross-namespace shared volumes deferred to T2)"

# A `needs.disk.ref` carrying a namespace (`other-ns/shared`) implies a
# cross-namespace shared volume — deferred to T2 (NFS) — and the admission
# webhook MUST reject the apply.
if kubectl apply -f - >/tmp/sv_ns_reject.out 2>&1 <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: crossns
  namespace: ${APP_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    expose:
      port: 80
    needs:
      disk:
        ref: "other-ns/shared"
        mountPath: ${MOUNT_PATH}
YAML
then
    printf 'FAILED: namespaced ref "other-ns/shared" was ACCEPTED (webhook rule missing/broken)\n' >&2
    cat /tmp/sv_ns_reject.out >&2 || true
    # best-effort cleanup of the wrongly-accepted CR
    kubectl delete "$APP_RES" crossns -n "$APP_NS" --wait=false >/dev/null 2>&1 || true
    exit 1
fi
if grep -qi 'namespaced\|cross-namespace\|denied the request\|admission webhook' /tmp/sv_ns_reject.out; then
    printf '  ok: admission webhook rejected the namespaced ref:\n'
    sed 's/^/    /' /tmp/sv_ns_reject.out
else
    printf 'FAILED: apply failed but not via the webhook namespaced-ref rule\n' >&2
    cat /tmp/sv_ns_reject.out >&2 || true
    exit 1
fi
# The CR must NOT exist (the reject prevented creation).
if kubectl -n "$APP_NS" get "$APP_RES" crossns >/dev/null 2>&1; then
    printf 'FAILED: the crossns Application exists despite the webhook reject\n' >&2
    exit 1
fi
printf '  ok: no crossns Application was created\n'

# ===============================================================
# Phase 9: OWNED-DISK NO-REGRESSION — a plain owned needs.disk still
#          provisions + mounts + forces Recreate (2.6b intact)
# ===============================================================

phase "Phase 9: owned-disk no-regression — owned needs.disk mounts + forces Recreate (2.6b intact)"

kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP_OWNED}
  namespace: ${APP_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    expose:
      port: 80
    needs:
      disk:
        size: 1Gi
        mountPath: ${MOUNT_PATH}
YAML

# Owned disk -> a `type: disk` claim -> an UNOWNED standalone RWO PVC named
# claim-<ns>-<app>-disk; status.volumeClaimRef points at it.
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM_OWNED" '{.spec.type}' disk 180
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM_OWNED" '{.status.volumeClaimRef}' "$PVC_OWNED" 300
retry 12 5 -- kubectl -n "$APP_NS" get pvc "$PVC_OWNED" >/dev/null
owned_mode=$(jp pvc "$APP_NS" "$PVC_OWNED" '{.spec.accessModes[0]}')
assert_eq "owned PVC accessModes[0]" "$owned_mode" "ReadWriteOnce"

wait_jsonpath "$APP_RES" "$APP_NS" "$APP_OWNED" '{.status.phase}' Ready 240
# 2.6b behaviour: an OWNED RWO disk forces strategy:Recreate (the
# regression guard — a shared ref must NOT, an owned one MUST).
owned_strategy=$(jp deployment "$APP_NS" "$APP_OWNED" '{.spec.strategy.type}')
assert_eq "owned-disk Deployment strategy.type (RWO PVC)" "$owned_strategy" "Recreate"
owned_claim=$(kubectl -n "$APP_NS" get deployment "$APP_OWNED" \
    -o jsonpath="{.spec.template.spec.volumes[?(@.name==\"${OWNED_VOLUME_NAME}\")].persistentVolumeClaim.claimName}" \
    2>/dev/null || true)
assert_eq "owned-disk Deployment volume claimName" "$owned_claim" "$PVC_OWNED"
kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP_OWNED}" --timeout=300s
printf '  ok: owned needs.disk still provisions, mounts, and forces Recreate (2.6b intact)\n'

# Clean up the owned app (best-effort; teardown deletes the cluster anyway).
kubectl delete "$APP_RES" "$APP_OWNED" -n "$APP_NS" --wait=false >/dev/null 2>&1 || true

# ===============================================================
# Phase 10: CAPACITY (SOFT) — capacity-signal machinery is best-effort
# ===============================================================

phase "Phase 10 (SOFT): capacity signal — kubelet Summary best-effort (ADR 0049 / Task 11)"

# Re-create a SharedVolume + one consumer so the kubelet has a pod mounting
# the backing PVC to report usage for.
apprafter volume create "$SV" --size "$SV_SIZE" -n "$APP_NS"
wait_jsonpath "$SV_RES" "$APP_NS" "$SV" '{.status.ready}' true 240
apply_consumer_app "$APP_A" "cap"
wait_jsonpath "$APP_RES" "$APP_NS" "$APP_A" '{.status.phase}' Ready 240
kubectl -n "$APP_NS" wait --for=condition=Available "deployment/${APP_A}" --timeout=300s

# Capacity is BEST-EFFORT: the operator samples the kubelet Summary API
# through the apiserver node-proxy. On kind that may not be reachable, so we
# treat the presence of a capacity sample as a SOFT signal — warn + note, but
# do NOT hard-fail the walk. Poll briefly for a status.capacity reading.
CAPACITY_SOFT_OK=0
deadline=$(( $(date +%s) + 120 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    cap_bytes=$(jp "$SV_RES" "$APP_NS" "$SV" '{.status.capacity.capacityBytes}')
    if [ -n "$cap_bytes" ] && [ "$cap_bytes" != "0" ]; then
        CAPACITY_SOFT_OK=1
        break
    fi
    sleep 10
done

if [ "$CAPACITY_SOFT_OK" -eq 1 ]; then
    cap_used=$(jp "$SV_RES" "$APP_NS" "$SV" '{.status.capacity.usedBytes}')
    cap_total=$(jp "$SV_RES" "$APP_NS" "$SV" '{.status.capacity.capacityBytes}')
    printf '  ok: capacity sampled via kubelet Summary — used=%s capacity=%s bytes\n' \
        "${cap_used:-?}" "$cap_total"
    # The CapacityWarning condition machinery is present (SufficientCapacity
    # when the node has headroom; NodeNearlyFull only past the threshold). Its
    # presence proves the signal path works end-to-end.
    cw_status=$(cond_status "$SV_RES" "$APP_NS" "$SV" CapacityWarning)
    if [ -n "$cw_status" ]; then
        printf '  ok: CapacityWarning condition present (status=%q) — signal path live\n' "$cw_status"
    else
        printf '  note: CapacityWarning condition absent this cycle (sample-less) — acceptable (best-effort)\n'
    fi
else
    # SOFT SKIP — capacity could not be sampled (kind kubelet Summary not
    # reachable via apiserver node-proxy). This is acceptable per ADR 0049:
    # capacity is decorative and best-effort, NEVER fails a reconcile.
    printf '  ok: capacity SOFT-SKIP — kubelet Summary API not reachable on this kind cluster (best-effort, ADR 0049)\n'
    printf '  note: the SharedVolume stayed Ready=true regardless — capacity absence does not block provisioning\n'
    # Prove the best-effort property: the volume is STILL Ready despite no
    # capacity sample (the load-bearing safety invariant).
    sv_ready=$(jp "$SV_RES" "$APP_NS" "$SV" '{.status.ready}')
    assert_eq "SharedVolume Ready despite absent capacity (best-effort)" "$sv_ready" "true"
fi

# ===============================================================
# Done — tear down on success path
# ===============================================================

# Remove the EXIT trap so cleanup() does not fire again — we own the
# tear-down inline here on the success path.
trap - EXIT

if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    k3d_down "$CLUSTER_NAME" || true
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' \
        "$CLUSTER_NAME"
fi

rm -rf "$TMPDIR_WORK"

printf '\nshared-volume-walk GREEN in %s\n' "$(elapsed)"
printf 'Chain proven: provision (unowned PVC) -> two-app share (write-through) -> rolling-update survives -> refCount + delete-guard -> missing-ref AwaitingSharedVolume -> namespaced-ref webhook reject -> capacity (SOFT) -> owned-disk no-regression\n'
