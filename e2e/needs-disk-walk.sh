#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter needs.disk-walk e2e — the full Phase-2.6b ResourceClaim
# chain on a local k3d/kind cluster (plan item 2.6b-6, ADR 0043).
#
# This script exercises the shipped needs.disk pipeline end-to-end,
# reusing the generic 2.4 machinery unchanged and adding the disk
# (standalone RWO PVC) backend:
#
#   generate (2.4d) -> schedule (2.3) -> provision (2.6b-3) ->
#   resume + mount (2.6b-4) -> data durability (pod restart) ->
#   delete + snapshot + reattach (2.6b-5) -> force-GC (2.6b-5)
#
# It ALSO proves the 2.6b-2 multi-claim surface in passing: a second
# Application declares `needs.pg` as a NAMED ARRAY of two entries
# (primary + analytics) with EXPLICIT env claim refs (2.12 / ADR 0046),
# and the resumed Deployment must carry the two disambiguated env vars
# DATABASE_URL_PRIMARY + DATABASE_URL_ANALYTICS resolved to secretKeyRefs
# into the `url` key of each connection Secret.
#
# Concretely, on a cluster bootstrapped with the platform stack
# (operator + admission-webhook + the always-on CNPG operator + the
# seeded `disk-local` + `pg-integrated` ServiceProviders):
#
#   1. Apply an AppRafter Application `sqlite` with a single scalar
#      `spec.base.needs.disk` (size 1Gi, mountPath /data) AND a second
#      Application `multi` with `needs.pg` as a 2-entry named array.
#   2. ASSERT the operator generates the disk ResourceClaim — the gate's
#      load-bearing action: it emits a claim per need instead of rendering
#      the Deployment immediately (the transient AwaitingResourceClaim phase
#      itself is not polled — provisioning is too fast here to observe it
#      reliably; see the Phase 4 note); the two multi-pg claims too.
#   3. ASSERT the scheduler matches the disk claim to `disk-local`
#      (status.provider, Scheduled=True).
#   4. ASSERT the provisioner creates an UNOWNED RWO PVC (the right
#      storageClass + accessModes, NO ownerReferences for retention),
#      records status.volumeClaimRef, and lands the claim ready.
#   5. ASSERT the Application resumes to Ready and the rendered
#      Deployment mounts the PVC at /data with strategy:Recreate; the
#      multi Deployment carries DATABASE_URL_PRIMARY + _ANALYTICS resolved
#      to secretKeyRefs on the `url` key of each connection Secret (2.12).
#   6. Data durability: exec the pod, write /data/probe, delete the pod,
#      and ASSERT the file survives the rebind (RWO PVC on one node).
#   7. Delete the Application; ASSERT a disk RetainedClaim is snapshotted
#      and the PVC SURVIVES the grace floor. Re-apply the Application and
#      ASSERT it reattaches to the SAME PVC and /data/probe is still there.
#   8. Force GC by deleting + re-creating the RetainedClaim with a past
#      retainUntil; ASSERT gc_drop_disk deletes the PVC and the snapshot.
#
# CLI state injection
# -------------------
# `apprafter cluster-bootstrap` reads the kubeconfig from the CLI's
# per-target state store (not from $KUBECONFIG). We set
# APPRAFTER_CONFIG_DIR to a tmpdir and seed it with a minimal
# config.yaml (active_target: k3d) and a state.json carrying the
# kubeconfig as kubeconfig_yaml (plaintext) — the same approach
# needs-pg-walk.sh / needs-redis-walk.sh use.
#
# Local-operator mode (REQUIRED while 2.12 is unreleased)
# ---------------------------------------------------------
# ADR 0046 (Phase 2.12) removed the 2.4e implicit DATABASE_URL_<NAME> injection
# and the connection-Secret's composed `DATABASE_URL` key. The multi app now
# binds the DSNs via EXPLICIT claim refs (env: {DATABASE_URL_PRIMARY:
# {claim: "pg.primary.url"}, DATABASE_URL_ANALYTICS: {claim: "pg.analytics.url"}}),
# which only the branch operator + webhook + CRD render/validate. So
# APPRAFTER_E2E_LOCAL_OPERATOR=1 is MANDATORY until 2.12 ships (Phase 1b builds
# + side-loads the working-tree operator/webhook and applies the branch CRDs +
# PVC RBAC). Both modes need the same tools (the operator build reuses the
# already-required container runtime).
#
# Required: docker (or podman), cargo, kubectl
#   — all satisfied inside `nix develop` or on a standard CI runner.
#   (cargo: lib.sh runs the CLI via `cargo run --bin apprafter`.)
#
# NOTE: the GC step deletes + RE-CREATES the RetainedClaim with a past
# retainUntil rather than patching it in place — the RetainedClaim is
# immutable (CEL `self == oldSelf`), so an in-place patch is rejected;
# the e2e/walk admin kubeconfig is `system:masters`, which the
# operator-only webhook permits to CREATE.
#
# Exit codes:
#   0 — chain green
#   1 — assertion failure
#   2 — precondition missing

set -euo pipefail

# ---------------------------------------------------------------
# Source shared helpers
# ---------------------------------------------------------------

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# ---------------------------------------------------------------
# Constants — the shipped needs.disk coordinates. The disk claim name
# is derived from the Application name (`<app>-disk` for the unnamed
# scalar form); the PVC / RetainedClaim names are derived from the
# claim's (namespace, name) by the 2.6b provisioner via
# `cnpg::k8s_name`. See
# operator/operator-controllers/resourceclaim-provisioner/src/disk.rs
# + reconcile.rs (`provision_disk` / `retained_claim_disk_object`) and
# operator/operator-controllers/application/src/lib.rs (`claim_name`).
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-disk-walk"

APP_NS="demo"                       # tenant namespace
APP="sqlite"                        # disk Application name
CLAIM="sqlite-disk"                 # generated disk ResourceClaim name
# PVC name = cnpg::k8s_name(demo, sqlite-disk); also the snapshot name +
# the deterministic name the provisioner cancels on reattach.
PVC="claim-demo-sqlite-disk"
RETAINED="claim-demo-sqlite-disk"   # RetainedClaim name = k8s_name(...)
MOUNT_PATH="/data"                  # needs.disk.mountPath -> volumeMount
# disk_identity_name(scalar, no name) folds the last mountPath segment:
# /data -> data -> volume name `disk-data`.
VOLUME_NAME="disk-data"

# A second app proves the 2.6b-2 multi-claim (named-array) surface: two
# pg claims, two disambiguated env vars on the resumed Deployment.
APP2="multi"
CLAIM2A="multi-pg-primary"          # claim_name(multi, pg, primary)
CLAIM2B="multi-pg-analytics"        # claim_name(multi, pg, analytics)

# Group-qualify the two collision-prone kinds so kubectl never resolves
# to the wrong API group: bare `application` also matches Argo CD's
# argoproj.io Application, and bare `resourceclaim` matches the k8s 1.32+
# DRA resource.k8s.io ResourceClaim. Always address the apprafter.io CRs.
APP_RES="application.apprafter.io"
CLAIM_RES="resourceclaim.apprafter.io"

RETAINED_NS="apprafter-system"      # RetainedClaim + ServiceProvider ns
PROVIDER="disk-local"               # seeded ServiceProvider
# The StorageClass the disk-local provider stamps onto the PVC. The seed
# default is `local-path` (ships on k3s/k3d). On a bare kind cluster the
# default class is `standard` (rancher.io/local-path) and `local-path`
# is ABSENT, so this walk ensures a `local-path` class exists on kind
# before provisioning (Phase 2) — a harness concern, not operator code.
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
# Precondition: this walk REQUIRES local-operator mode while 2.12 is
# UNRELEASED. ADR 0046 removed the 2.4e implicit DATABASE_URL_<NAME>
# injection and the connection-Secret's composed `DATABASE_URL` key — the
# multi app now binds the DSNs by EXPLICIT claim refs
# (`env: {DATABASE_URL_PRIMARY: {claim: "pg.primary.url"}, ...}`), which
# only the branch operator + webhook + CRD render/validate. The flag is
# mandatory until 2.12 ships.
# ---------------------------------------------------------------
if [ -z "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
    cat >&2 <<'EOF'
ERROR: needs-disk-walk requires APPRAFTER_E2E_LOCAL_OPERATOR=1 while 2.12 is unreleased.

ADR 0046 (Phase 2.12) removed the 2.4e implicit DATABASE_URL_<NAME> injection and
the connection-Secret's composed `DATABASE_URL` key. This walk now declares the DSNs
explicitly (`env: {DATABASE_URL_PRIMARY: {claim: "pg.primary.url"},
DATABASE_URL_ANALYTICS: {claim: "pg.analytics.url"}}`), which only the branch
operator + webhook + CRD render/validate. Run:

  APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/needs-disk-walk.sh

so the walk builds + side-loads the working-tree operator/webhook and applies
the branch CRDs (Phase 1b). Drop this gate once 2.12 publishes.
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
        printf '\n!!! needs-disk-walk FAILED at %s (exit %d) !!!\n' \
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
# Helper: seed the CLI state store (mirrors needs-pg-walk.sh).
#
# Writes:
#   $APPRAFTER_CONFIG_DIR/config.yaml      — active_target: k3d
#   $APPRAFTER_CONFIG_DIR/state/k3d/.apprafter/state.json
#     hetzner_cloud.kubeconfig_yaml = <kubeconfig plaintext>
# ---------------------------------------------------------------
seed_apprafter_state() {
    local kubeconfig_content="$1"

    mkdir -p "${APPRAFTER_CONFIG_DIR}"
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/k3d/.apprafter"

    cat >"${APPRAFTER_CONFIG_DIR}/config.yaml" <<'YAML'
active_target: k3d
version: 1
YAML

    # Escape the kubeconfig for JSON embedding (replace newlines with
    # \n, escape double-quotes and backslashes).
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
# Local helper: wait_jsonpath <kind> <ns> <name> <jsonpath> <want> [timeout]
#   Poll `kubectl get -o jsonpath` every 5s until the rendered value
#   equals <want> or the deadline passes. Prints an ERROR + returns 1
#   on timeout (with a describe for diagnostics).
# ---------------------------------------------------------------
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
    printf 'ERROR: %s/%s [%s] never became %q (last=%q)\n' \
        "$kind" "$name" "$jsonpath" "$want" "${got:-}" >&2
    kubectl -n "$ns" describe "$kind" "$name" >&2 2>&1 || true
    return 1
}

# ---------------------------------------------------------------
# Local helper: wait_gone <kind> <ns> <name> [timeout]
#   Poll until `kubectl get` 404s (the object is gone). Prints an
#   ERROR + returns 1 on timeout.
# ---------------------------------------------------------------
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
    printf 'ERROR: %s/%s still present after %ss\n' "$kind" "$name" "$timeout" >&2
    return 1
}

# ---------------------------------------------------------------
# Local helper: assert_eq <description> <got> <want>
# ---------------------------------------------------------------
assert_eq() {
    local desc="$1" got="$2" want="$3"
    if [ "$got" = "$want" ]; then
        printf '  ok: %s = %q\n' "$desc" "$got"
        return 0
    fi
    printf 'ERROR: %s — got %q, want %q\n' "$desc" "$got" "$want" >&2
    return 1
}

# ---------------------------------------------------------------
# Local helper: jp <kind> <ns> <name> <jsonpath>  (read one value)
# ---------------------------------------------------------------
jp() {
    kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true
}

# Condition-status reader: the `.status` of a condition selected by
# `type`. kubectl jsonpath has no boolean filter for the operator's
# verbs, so we pull the conditions array as JSON and grep it via a
# small python-free jsonpath the kubectl client supports.
cond_status() {
    # $1 kind, $2 ns, $3 name, $4 condition-type
    kubectl -n "$2" get "$1" "$3" -o \
        jsonpath="{.status.conditions[?(@.type==\"$4\")].status}" \
        2>/dev/null || true
}

# ---------------------------------------------------------------
# Local helper: sqlite_pod  — the current running sqlite pod name.
# ---------------------------------------------------------------
sqlite_pod() {
    kubectl -n "$APP_NS" get pod -l app.kubernetes.io/name=sqlite \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# ===============================================================
# Phase 0: bring up the cluster
# ===============================================================

phase "Phase 0: k3d_up ${CLUSTER_NAME}"

k3d_up "$CLUSTER_NAME"

cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
# The cluster exists and $KUBECONFIG now points at it — cleanup may
# safely diagnose/tear it down (and only it).
K3D_CREATED=1
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

# ===============================================================
# Phase 1: seed CLI state and run cluster-bootstrap
# ===============================================================

phase "Phase 1: cluster-bootstrap (platform stack + CNPG + disk-local + pg-integrated)"

kubeconfig_content=$(cat "$KUBECONFIG_FILE")
seed_apprafter_state "$kubeconfig_content"
export APPRAFTER_CONFIG_DIR
printf '  APPRAFTER_CONFIG_DIR=%s\n' "$APPRAFTER_CONFIG_DIR"

bootstrap_with_retry

printf '  cluster-bootstrap complete\n'

# ---------------------------------------------------------------
# Phase 1b (REQUIRED — APPRAFTER_E2E_LOCAL_OPERATOR): build + side-load the
# working-tree operator + webhook instead of the published image, then apply
# the branch CRDs + PVC RBAC. 2.12 (ADR 0046) drops the 2.4e auto-inject +
# the composed `DATABASE_URL` connection-Secret key and adds the `env` value
# node + webhook env-ref rules — all UNRELEASED, so the published image + CRD
# this cluster bootstrapped from cannot render/validate the explicit DSN refs
# in Phase 3. The flag is mandatory until 2.12 ships (the precondition gate
# above ensures this).
# NOTE: the if-body below is intentionally NOT indented — it carries a
# column-0 heredoc (the PVC-RBAC apply); `fi` closes it just before Phase 2.
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
# (released) pod lingers Terminating for its grace period — and during that
# window a branch CR apply (the Phase 2 `disk-local` ServiceProvider seed) can
# still route to the old webhook, whose released BUILTIN_TYPES lacks `disk`
# (spurious "spec.type must be one of pg|...|notifications (got disk)"). Wait
# until ONLY the branch webhook serves before any branch-typed apply.
printf '  waiting for the old (released) webhook pod to fully terminate ...\n'
_wh_deadline=$(( $(date +%s) + 90 ))
while [ "$(date +%s)" -lt "$_wh_deadline" ]; do
    [ "$(kubectl -n apprafter-system get pods \
        -l app.kubernetes.io/name=admission-webhook --no-headers 2>/dev/null \
        | wc -l)" -le 1 ] && break
    sleep 3
done
printf '  apprafter-operator + admission-webhook now running the working-tree build\n'

# The released platform-stack chart this cluster bootstrapped from predates
# BOTH the 2.6b CRD changes (the `disk` type enums, ResourceClaim.status.volumeClaimRef,
# the RetainedClaim disk fields, the generalized `needs` scalar|array schema) AND
# the 2.12 CRD changes (Application.spec.base.env / environments[*].env now accept
# a string-OR-object #EnvValue). Argo CD owns those CRDs via the apprafter-operator
# Application, so: disable automated sync on the parent + operator apps (else Argo
# reverts the drift), then apply the BRANCH-rendered CRDs server-side. Same rationale
# as side-loading the operator image — all are unpublished 2.12 artifacts.
# Mirrors needs-pg-walk Phase 1b + scripts/validate-crds.sh's render.
printf '  applying branch operator CRDs (released chart predates 2.6b/2.12 schema) ...\n'
for _app in platform apprafter-operator; do
    kubectl -n argocd patch applications.argoproj.io "$_app" --type=merge \
        -p '{"spec":{"syncPolicy":{"automated":null}}}' >/dev/null 2>&1 || true
done
_yq() { if command -v yq >/dev/null 2>&1; then yq "$@"; else nix run nixpkgs#yq-go -- "$@"; fi; }
helm template apprafter-operator "${REPO_ROOT}/operator/charts/apprafter-operator" \
    | _yq 'select(.kind == "CustomResourceDefinition")' \
    | kubectl apply --server-side --force-conflicts -f -
for _crd in applications serviceproviders resourceclaims retainedclaims; do
    retry 12 5 -- kubectl wait --for=condition=Established \
        "crd/${_crd}.apprafter.io" --timeout=30s
done
printf '  branch CRDs applied + Established\n'

# The released operator ClusterRole predates the 2.6b PVC RBAC (Backend::Disk
# SSA-applies a PersistentVolumeClaim). Grant it ADDITIVELY via a dedicated
# ClusterRole — do NOT replace the operator's ClusterRole: a standalone
# `helm template` of the operator chart renders an INCOMPLETE RBAC (the
# platform-stack umbrella passes values that gate other rules), so replacing it
# strips the operator's resourceclaims/retainedclaims/migrationplans watches.
# Mirrors the persistentvolumeclaims rule the branch adds to
# operator/charts/apprafter-operator/templates/rbac.yaml. Pre-release-only.
printf '  granting the operator the 2.6b PVC RBAC (additive) ...\n'
kubectl apply -f - <<'PVCRBAC'
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: apprafter-operator-disk-pvc
rules:
  - apiGroups: [""]
    resources: [persistentvolumeclaims]
    verbs: [get, list, watch, create, update, patch, delete]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: apprafter-operator-disk-pvc
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: apprafter-operator-disk-pvc
subjects:
  - kind: ServiceAccount
    name: apprafter-operator
    namespace: apprafter-system
PVCRBAC
printf '  operator PVC RBAC granted (additive)\n'
fi  # end APPRAFTER_E2E_LOCAL_OPERATOR (Phase 1b)

# ===============================================================
# Phase 2: readiness — provider, webhook, CNPG operator, storage class
# ===============================================================

phase "Phase 2: platform readiness (AppProject, disk-local provider, webhook, CNPG, storageClass)"

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
    printf 'ERROR: AppProject apps not found after 10 min\n' >&2
    exit 1
}

# Seed the `disk-local` ServiceProvider if the bootstrapped platform
# stack did not ship it. RATIONALE (harness substrate, NOT operator code
# under test): the cluster bootstraps from the PUBLISHED platform-stack
# chart on ghcr.io, and the `disk-local` seed
# (platform-stack/cue/service_providers.cue) is a 2.6b branch artifact
# that has NOT been published yet — exactly like the operator IMAGE,
# which is why this walk also builds + side-loads the operator from
# source (Phase 1b). So, parallel to local-operator mode, the walk
# provides the unreleased seed itself when absent, mirroring the branch
# CUE source verbatim (labels tier=integrated/location=in-cluster,
# type=disk, backend=disk, config.storageClass=local-path). Idempotent:
# a no-op once the published chart carries the seed.
if ! kubectl get serviceprovider "$PROVIDER" -n "$RETAINED_NS" >/dev/null 2>&1; then
    printf '  ServiceProvider %s absent — seeding it from the branch chart source (unpublished 2.6b artifact) ...\n' "$PROVIDER"
    kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: ServiceProvider
metadata:
  name: ${PROVIDER}
  namespace: ${RETAINED_NS}
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

# ASSERT the `disk-local` ServiceProvider is present with the
# tier=integrated label the needs.disk selector matches AND backend=disk.
retry 30 10 -- kubectl get serviceprovider "$PROVIDER" \
    -n "$RETAINED_NS" >/dev/null
sp_tier=$(jp serviceprovider "$RETAINED_NS" "$PROVIDER" '{.metadata.labels.tier}')
assert_eq "ServiceProvider ${PROVIDER} label tier" "$sp_tier" "integrated"
sp_backend=$(jp serviceprovider "$RETAINED_NS" "$PROVIDER" '{.spec.backend}')
assert_eq "ServiceProvider ${PROVIDER} backend" "$sp_backend" "disk"
# Resolve the StorageClass the provider will stamp (the seed default is
# `local-path`); the PVC assertions in Phase 6 check against it.
sp_class=$(jp serviceprovider "$RETAINED_NS" "$PROVIDER" '{.spec.config.storageClass}')
[ -n "$sp_class" ] && STORAGE_CLASS="$sp_class"
printf '  disk-local storageClass = %s\n' "$STORAGE_CLASS"

# The `multi` app's needs.pg routes to the always-on CNPG operator — wait
# for it (the provisioner SSA-applies CNPG Cluster/Database CRs).
printf '  waiting for the CNPG operator Deployment ...\n'
retry 30 10 -- kubectl -n cnpg-system rollout status \
    deploy -l app.kubernetes.io/name=cloudnative-pg --timeout=60s

# The admission webhook must be Available before applying the Application
# (it validates the CR — incl. the 2.6b-4 disk guards — on CREATE).
printf '  waiting for the admission-webhook Deployment ...\n'
retry 30 10 -- kubectl -n "$RETAINED_NS" rollout status \
    deploy admission-webhook --timeout=60s

# StorageClass guard: the disk-local provider stamps `$STORAGE_CLASS`
# onto the PVC. On k3s/k3d `local-path` ships by default; on a bare kind
# cluster the default class is `standard` and `local-path` is absent, so
# the PVC would never bind and the pod (Phase 6+) would hang. Ensure the
# class exists — on kind, install the rancher local-path-provisioner
# (which creates a `local-path` class). This is a HARNESS concern (test
# substrate), NOT operator code under test.
if ! kubectl get storageclass "$STORAGE_CLASS" >/dev/null 2>&1; then
    printf '  StorageClass %s absent — installing the rancher local-path-provisioner (kind substrate fix) ...\n' "$STORAGE_CLASS"
    retry 5 10 -- kubectl apply -f \
        https://raw.githubusercontent.com/rancher/local-path-provisioner/v0.0.31/deploy/local-path-storage.yaml
    retry 30 10 -- kubectl -n local-path-storage rollout status \
        deploy/local-path-provisioner --timeout=60s
    kubectl get storageclass "$STORAGE_CLASS" >/dev/null 2>&1 || {
        printf 'ERROR: StorageClass %s still absent after installing local-path-provisioner\n' "$STORAGE_CLASS" >&2
        kubectl get storageclass >&2 2>&1 || true
        exit 2
    }
fi
printf '  ok: StorageClass %s present\n' "$STORAGE_CLASS"

# ===============================================================
# Phase 3: apply the needs.disk Application + the multi-pg Application
# ===============================================================

phase "Phase 3: apply Applications (sqlite needs.disk scalar; multi needs.pg named array + explicit DSN refs)"

kubectl create namespace "$APP_NS" 2>/dev/null || true

# The disk app: a long-lived nginx-based image (Application has no
# `command` field, so the image must run a foreground process by default).
# nginxdemos/hello stays up and ships a busybox `sh`, so we can exec +
# write a file into the PVC mounted at /data. replicas:1 (RWO), a single
# scalar disk need (no explicit name → the claim is `<app>-disk`, the
# volume `disk-data` derived from the /data mountPath segment).
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP}
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

# The multi-claim app: needs.pg as a NAMED ARRAY of two entries. Proves
# the 2.6b-2 per-(type,name) claim-gen + env disambiguation surface.
# 2.12 (ADR 0046 #5): the 2.4e implicit DATABASE_URL_<NAME> injection is
# removed — the app binds each DSN by an EXPLICIT named claim ref. The
# payload for a named claim ref is "<type>.<name>.<field>" so that the
# operator can route it to the correct (type, name) connection Secret.
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP2}
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
      pg:
        - name: primary
          selector:
            tier: integrated
        - name: analytics
          selector:
            tier: integrated
    env:
      DATABASE_URL_PRIMARY:
        claim: "pg.primary.url"
      DATABASE_URL_ANALYTICS:
        claim: "pg.analytics.url"
YAML

# ===============================================================
# Phase 4: generate (2.4d) — disk ResourceClaim + the two multi-pg named
#          claims generated (the deterministic gate evidence; the transient
#          AwaitingResourceClaim phase is not polled — see the note below)
# ===============================================================

phase "Phase 4: generate — disk ResourceClaim + multi-pg named claims created (gate evidence)"

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.spec.type}' disk 180

claim_tier=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.spec.selector.tier}')
assert_eq "disk ResourceClaim selector.tier" "$claim_tier" "integrated"
claim_size=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.spec.size}')
assert_eq "disk ResourceClaim size" "$claim_size" "1Gi"
claim_owner_kind=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" \
    '{.metadata.ownerReferences[0].kind}')
assert_eq "disk ResourceClaim ownerRef Kind" "$claim_owner_kind" "Application"

# The two named pg claims exist (2.6b-2 multi-claim generation).
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM2A" '{.spec.type}' pg 120
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM2B" '{.spec.type}' pg 120
claim2a_name=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM2A" '{.spec.name}')
assert_eq "multi-pg-primary spec.name" "$claim2a_name" "primary"
claim2b_name=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM2B" '{.spec.name}')
assert_eq "multi-pg-analytics spec.name" "$claim2b_name" "analytics"

# NOTE — the gate (status.phase=AwaitingResourceClaim, ResourceClaimPending=
# True until every claim is ready) is deliberately NOT polled here. Both
# backends provision fast in this walk: local-path PVC creation is single-
# pass/instant (no pod boot), and the shared CNPG cluster is pre-warmed by
# bootstrap, so pg DB/role creation is near-instant too — an Application
# transitions AwaitingResourceClaim -> Ready in well under a poll interval,
# making the transient gate phase a coin-flip to observe (a timing artifact
# of a fast cluster, not a product behaviour to assert). The gate is instead
# proven DETERMINISTICALLY by (a) claim GENERATION above — the operator
# emitted a ResourceClaim per need rather than rendering the Deployment
# immediately, the gate's load-bearing action; and (b) the Ready + mount +
# strategy:Recreate (Phase 7) and DATABASE_URL_<NAME> claim ref resolution
# (Phase 7) assertions — the workload only receives its volume / DSN once the
# claim is ready. The pause-while-unready logic itself is unit-tested in the
# operator (unready_claim_names / build_resource_claim_paused_status).

# ===============================================================
# Phase 5: schedule (2.3) — the scheduler matches `disk-local`
# ===============================================================

phase "Phase 5: schedule — disk claim matched to disk-local, Scheduled=True"

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.provider}' \
    "$PROVIDER" 120
sched_cond=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM" Scheduled)
assert_eq "disk ResourceClaim Scheduled condition" "$sched_cond" "True"

# ===============================================================
# Phase 6: provision (2.6b-3) — unowned RWO PVC + status.volumeClaimRef
# ===============================================================

phase "Phase 6: provision — unowned RWO PVC, status.volumeClaimRef set"

# The disk provision is single-pass: ready = the PVC EXISTS (not Bound —
# local-path binds WaitForFirstConsumer, so binding waits for the pod).
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.ready}' true 300

claim_vcr=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.volumeClaimRef}')
assert_eq "disk ResourceClaim status.volumeClaimRef" "$claim_vcr" "$PVC"

# The PVC exists with the right storageClass + RWO accessModes.
retry 12 5 -- kubectl -n "$APP_NS" get pvc "$PVC" >/dev/null
pvc_class=$(jp pvc "$APP_NS" "$PVC" '{.spec.storageClassName}')
assert_eq "PVC storageClassName" "$pvc_class" "$STORAGE_CLASS"
pvc_mode=$(jp pvc "$APP_NS" "$PVC" '{.spec.accessModes[0]}')
assert_eq "PVC accessModes[0]" "$pvc_mode" "ReadWriteOnce"
pvc_size=$(jp pvc "$APP_NS" "$PVC" '{.spec.resources.requests.storage}')
assert_eq "PVC requested storage" "$pvc_size" "1Gi"

# CRITICAL (retention): the PVC is UNOWNED — no ownerReferences — so an
# app delete does NOT cascade-delete it; the 2.6b-5 GC is the only thing
# that drops it (after grace).
pvc_owner=$(jp pvc "$APP_NS" "$PVC" '{.metadata.ownerReferences}')
if [ -n "$pvc_owner" ]; then
    printf 'ERROR: PVC %s has ownerReferences (%s) — retention broken, an app delete would cascade it\n' \
        "$PVC" "$pvc_owner" >&2
    exit 1
fi
printf '  ok: PVC %s is UNOWNED (no ownerReferences — survives app delete)\n' "$PVC"

# SSA-split guard: the provisioner's status write (ready /
# volumeClaimRef / Ready) must NOT have clobbered the scheduler's
# Scheduled=True. Re-assert it after provisioning.
sched_after=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM" Scheduled)
assert_eq "Scheduled still True after provision (SSA split)" "$sched_after" "True"

# ===============================================================
# Phase 7: resume + mount (2.6b-4) — Application Ready, volumeMount +
#          volume + strategy:Recreate; multi Deployment gets two env vars
# ===============================================================

phase "Phase 7: resume — Application Ready, /data mounted, strategy:Recreate"

wait_jsonpath "$APP_RES" "$APP_NS" "$APP" '{.status.phase}' Ready 180

# The container volumeMount targets /data with the derived volume name.
mount_path=$(kubectl -n "$APP_NS" get deployment "$APP" \
    -o jsonpath="{.spec.template.spec.containers[0].volumeMounts[?(@.name==\"${VOLUME_NAME}\")].mountPath}" \
    2>/dev/null || true)
assert_eq "Deployment volumeMount ${VOLUME_NAME} mountPath" "$mount_path" "$MOUNT_PATH"

# The pod volume references the provisioned PVC by claimName.
vol_claim=$(kubectl -n "$APP_NS" get deployment "$APP" \
    -o jsonpath="{.spec.template.spec.volumes[?(@.name==\"${VOLUME_NAME}\")].persistentVolumeClaim.claimName}" \
    2>/dev/null || true)
assert_eq "Deployment volume ${VOLUME_NAME} PVC claimName" "$vol_claim" "$PVC"

# An RWO PVC forces strategy:Recreate (old + new pod cannot hold it).
strategy=$(jp deployment "$APP_NS" "$APP" '{.spec.strategy.type}')
assert_eq "Deployment strategy.type (RWO PVC)" "$strategy" "Recreate"

kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP}" --timeout=300s
printf '  Deployment %s -> Available\n' "$APP"

# ---- multi-pg (2.6b-2 / 2.12): both named pg claims provision + the resumed
#      Deployment resolves the explicit DSN claim refs ----------------
# 2.12 (ADR 0046 #8): `{claim: "pg.primary.url"}` resolves to a secretKeyRef
# into the primary connection Secret's `url` key; same for analytics. The old
# composed `DATABASE_URL` key is gone from the connection Secrets.
printf '  waiting for the multi-pg claims to provision ...\n'
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM2A" '{.status.ready}' true 360
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM2B" '{.status.ready}' true 360
wait_jsonpath "$APP_RES" "$APP_NS" "$APP2" '{.status.phase}' Ready 180

env_primary=$(kubectl -n "$APP_NS" get deployment "$APP2" \
    -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"DATABASE_URL_PRIMARY\")].valueFrom.secretKeyRef.name}" \
    2>/dev/null || true)
assert_eq "multi Deployment env DATABASE_URL_PRIMARY secretKeyRef.name" \
    "$env_primary" "${CLAIM2A}-conn"
env_primary_key=$(kubectl -n "$APP_NS" get deployment "$APP2" \
    -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"DATABASE_URL_PRIMARY\")].valueFrom.secretKeyRef.key}" \
    2>/dev/null || true)
assert_eq "multi Deployment env DATABASE_URL_PRIMARY secretKeyRef.key" \
    "$env_primary_key" "url"
env_analytics=$(kubectl -n "$APP_NS" get deployment "$APP2" \
    -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"DATABASE_URL_ANALYTICS\")].valueFrom.secretKeyRef.name}" \
    2>/dev/null || true)
assert_eq "multi Deployment env DATABASE_URL_ANALYTICS secretKeyRef.name" \
    "$env_analytics" "${CLAIM2B}-conn"
env_analytics_key=$(kubectl -n "$APP_NS" get deployment "$APP2" \
    -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"DATABASE_URL_ANALYTICS\")].valueFrom.secretKeyRef.key}" \
    2>/dev/null || true)
assert_eq "multi Deployment env DATABASE_URL_ANALYTICS secretKeyRef.key" \
    "$env_analytics_key" "url"
# The old composed keys must be GONE from each connection Secret (2.4e dropped).
old_primary=$(kubectl -n "$APP_NS" get secret "${CLAIM2A}-conn" \
    -o jsonpath='{.data.DATABASE_URL}' 2>/dev/null || true)
assert_eq "primary conn Secret has NO DATABASE_URL key (2.4e dropped)" "$old_primary" ""
old_analytics=$(kubectl -n "$APP_NS" get secret "${CLAIM2B}-conn" \
    -o jsonpath='{.data.DATABASE_URL}' 2>/dev/null || true)
assert_eq "analytics conn Secret has NO DATABASE_URL key (2.4e dropped)" "$old_analytics" ""

# ===============================================================
# Phase 8: data durability — write /data/probe, restart the pod, ASSERT
#          the file survives the RWO PVC rebind (same node)
# ===============================================================

phase "Phase 8: data durability — file under /data survives a pod restart"

POD=$(sqlite_pod)
if [ -z "$POD" ]; then
    printf 'ERROR: could not find the sqlite pod\n' >&2
    kubectl -n "$APP_NS" get pod -l app.kubernetes.io/name=sqlite >&2 2>&1 || true
    exit 1
fi
printf '  sqlite pod: %s\n' "$POD"

# Write a marker into the mounted PVC.
kubectl -n "$APP_NS" exec "$POD" -- sh -c "echo durable-disk-probe > ${MOUNT_PATH}/probe"
wrote=$(kubectl -n "$APP_NS" exec "$POD" -- cat "${MOUNT_PATH}/probe" 2>/dev/null || true)
assert_eq "wrote ${MOUNT_PATH}/probe" "$wrote" "durable-disk-probe"

# Delete the pod; the Deployment recreates it (Recreate strategy → the
# old pod fully terminates first, releasing the RWO PVC).
printf '  deleting the sqlite pod (PVC must rebind to the replacement) ...\n'
kubectl -n "$APP_NS" delete pod "$POD" --wait=true

# Wait for the Deployment to roll a fresh, ready pod.
kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP}" --timeout=300s
# The replacement pod has a new name — re-resolve it, then poll for the
# container to be ready enough to exec.
printf '  waiting for the replacement pod to be Ready ...\n'
retry 40 5 -- kubectl -n "$APP_NS" wait --for=condition=Ready \
    pod -l app.kubernetes.io/name=sqlite --timeout=20s
POD2=$(sqlite_pod)
if [ -z "$POD2" ]; then
    printf 'ERROR: could not find the replacement sqlite pod\n' >&2
    exit 1
fi
printf '  replacement sqlite pod: %s\n' "$POD2"

survived=$(kubectl -n "$APP_NS" exec "$POD2" -- cat "${MOUNT_PATH}/probe" 2>/dev/null || true)
assert_eq "${MOUNT_PATH}/probe survived the pod restart" "$survived" "durable-disk-probe"

# ===============================================================
# Phase 9: delete + snapshot + reattach (2.6b-5) — RetainedClaim, the
#          PVC survives the grace floor, a redeploy reattaches the SAME PVC
# ===============================================================

phase "Phase 9: delete Application — disk RetainedClaim + PVC survives grace; redeploy reattaches"

# Delete the APPLICATION, not just the ResourceClaim: the claim is owned
# by the Application (ownerRef), so deleting the claim alone while the app
# still declares needs.disk makes the Application controller regenerate
# it, and the provisioner CANCELS the just-written RetainedClaim
# (cancel-on-reprovision) + reattaches the SAME PVC — so the snapshot
# never persists for the grace/GC assertions below. Deleting the app
# cascades to the claim with no regeneration. (Mirrors the needs-pg /
# needs-redis walks.)
kubectl delete "$APP_RES" "$APP" -n "$APP_NS" --wait=true

# The finalizer snapshots a disk-shaped RetainedClaim into apprafter-system.
wait_jsonpath retainedclaim "$RETAINED_NS" "$RETAINED" \
    '{.spec.claimRef.name}' "$CLAIM" 120
rc_backend=$(jp retainedclaim "$RETAINED_NS" "$RETAINED" '{.spec.backend}')
assert_eq "RetainedClaim spec.backend" "$rc_backend" "disk"
rc_vcr=$(jp retainedclaim "$RETAINED_NS" "$RETAINED" '{.spec.volumeClaimRef}')
assert_eq "RetainedClaim spec.volumeClaimRef" "$rc_vcr" "$PVC"
rc_vcn=$(jp retainedclaim "$RETAINED_NS" "$RETAINED" '{.spec.volumeClaimNamespace}')
assert_eq "RetainedClaim spec.volumeClaimNamespace" "$rc_vcn" "$APP_NS"
rc_provider=$(jp retainedclaim "$RETAINED_NS" "$RETAINED" '{.spec.provider}')
assert_eq "RetainedClaim spec.provider" "$rc_provider" "$PROVIDER"
rc_retain=$(jp retainedclaim "$RETAINED_NS" "$RETAINED" '{.spec.retainUntil}')
# RFC3339: a non-empty timestamp starting with a 4-digit year.
case "$rc_retain" in
    [0-9][0-9][0-9][0-9]-*) printf '  ok: RetainedClaim retainUntil = %s\n' "$rc_retain" ;;
    *) printf 'ERROR: RetainedClaim retainUntil not RFC3339: %q\n' "$rc_retain" >&2; exit 1 ;;
esac

# The grace floor holds: the unowned PVC SURVIVES the app delete (GC has
# NOT fired — retainUntil is days away — and the PVC has no ownerRef).
sleep 10
pvc_floor=$(jp pvc "$APP_NS" "$PVC" '{.metadata.name}')
assert_eq "PVC STILL present during grace (unowned, retained)" "$pvc_floor" "$PVC"

# ---- reattach: re-apply the Application → the provisioner reattaches the
#      SAME PVC (idempotent SSA), cancels the RetainedClaim, and the data
#      written in Phase 8 is STILL there (survives delete + redeploy) -----
printf '  re-applying the sqlite Application (must reattach the SAME PVC) ...\n'
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP}
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

# The re-provision reattaches the SAME PVC name (status.volumeClaimRef
# unchanged) and cancels the RetainedClaim (cancel-on-reprovision).
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.volumeClaimRef}' "$PVC" 300
wait_gone retainedclaim "$RETAINED_NS" "$RETAINED" 120
# Still the same UNOWNED PVC (not deleted/recreated — reattach).
pvc_reattach=$(jp pvc "$APP_NS" "$PVC" '{.metadata.name}')
assert_eq "PVC reattached (same name, not recreated)" "$pvc_reattach" "$PVC"

wait_jsonpath "$APP_RES" "$APP_NS" "$APP" '{.status.phase}' Ready 180
kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP}" --timeout=300s
retry 40 5 -- kubectl -n "$APP_NS" wait --for=condition=Ready \
    pod -l app.kubernetes.io/name=sqlite --timeout=20s
POD3=$(sqlite_pod)
if [ -z "$POD3" ]; then
    printf 'ERROR: could not find the redeployed sqlite pod\n' >&2
    exit 1
fi
printf '  redeployed sqlite pod: %s\n' "$POD3"

reattached=$(kubectl -n "$APP_NS" exec "$POD3" -- cat "${MOUNT_PATH}/probe" 2>/dev/null || true)
assert_eq "${MOUNT_PATH}/probe survived delete + redeploy (data reattached)" \
    "$reattached" "durable-disk-probe"

# ===============================================================
# Phase 10: force GC (2.6b-5) — re-create the RetainedClaim with a past
#           retainUntil; gc_drop_disk deletes the PVC + the snapshot
# ===============================================================

phase "Phase 10: force GC — past retainUntil drops the PVC + the snapshot"

# Delete the live app again so a fresh RetainedClaim is snapshotted (and
# the source claim is genuinely gone — the GC live-guard skips the PVC
# delete while the claim is back).
kubectl delete "$APP_RES" "$APP" -n "$APP_NS" --wait=true
wait_jsonpath retainedclaim "$RETAINED_NS" "$RETAINED" \
    '{.spec.volumeClaimRef}' "$PVC" 120

# The RetainedClaim is immutable (CEL self==oldSelf); an in-place patch
# is rejected. Delete it and RE-CREATE with the same coordinates but a
# retainUntil in the past so the GC fires immediately.
kubectl delete retainedclaim "$RETAINED" -n "$RETAINED_NS" --wait=true

kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: RetainedClaim
metadata:
  name: ${RETAINED}
  namespace: ${RETAINED_NS}
spec:
  claimRef:
    name: ${CLAIM}
    namespace: ${APP_NS}
  provider: ${PROVIDER}
  backend: disk
  volumeClaimRef: ${PVC}
  volumeClaimNamespace: ${APP_NS}
  retainUntil: "2000-01-01T00:00:00Z"
YAML

# gc_drop_disk deletes the unowned PVC, then the snapshot.
wait_gone pvc "$APP_NS" "$PVC" 180
wait_gone retainedclaim "$RETAINED_NS" "$RETAINED" 120

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

printf '\nneeds-disk-walk GREEN in %s\n' "$(elapsed)"
printf 'Chain proven: generate -> schedule -> provision (unowned RWO PVC) -> resume + mount -> data durability -> delete + snapshot + reattach -> force-GC (PVC dropped)\n'
