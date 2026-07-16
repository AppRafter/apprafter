#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter 2.6d export pg-helper reachability PROBE on a real kind+Cilium
# cluster (the empirical answer the kind walks can't give — they SKIP Cilium).
#
# THE QUESTION
# ------------
# On a REAL Cilium cluster (with the 2.10 per-app egress CiliumNetworkPolicy
# in place), does `apprafter export`'s ephemeral pg helper pod — a plain
# `postgres:<major>-alpine` pod labelled ONLY `apprafter.io/backup-helper`
# (NO `apprafter.io/application` label, so NOT selected by any per-app
# egress CNP), created in the APP namespace — SUCCESSFULLY reach the CNPG
# `<cluster>-rw.cnpg-system.svc:5432` and produce a pg_dump, OR is it BLOCKED
# (e.g. `Operation not permitted` / timeout) by a Cilium policy?
#
# A real-Hetzner DR walk saw a plain psql pod (app ns, no app labels)
# `Operation not permitted` connecting to platform-postgres-rw.cnpg-system.
# The 2.6d export/backup pg helper (cli/cli-providers/src/backup/{helper_pod,
# extract}.rs) takes the SAME path. Static analysis says nothing obviously
# blocks it (default-deny NP is `default`-ns + Ingress-only; the 2.10 CNP
# selects only the app's own pods, so an unselected pod is default-allow
# egress in Cilium). This probe gets the EMPIRICAL answer.
#
# HOW IT WORKS
# ------------
# Reuses the 2.10 walk's setup EXACTLY (kind_up_cilium -> bootstrap_with_cilium
# -> branch operator/webhook + CRDs -> a `needs.pg` Application `web` in `demo`
# that reaches Ready + its `web-egress` CNP). Then it DRIVES `apprafter export`
# (which enumerates ResourceClaims, spins the pg helper pod, execs pg_dump,
# streams to a local dir) and JUDGES:
#
#   * export SUCCEEDS + `pg/demo/<claim>.dump` exists + is non-empty
#       -> the pg helper REACHED pg. The real-Hetzner `Operation not permitted`
#          was a fluke/other cause; 2.6d export is FINE on Cilium.  -> GREEN
#   * export ERRORS (pg_dump connection refused / Operation not permitted /
#       timeout)
#       -> the pg helper was BLOCKED. Then dump the CNP/NP state + Hubble to
#          identify the policy mechanism.                            -> BLOCKED
#
# It ALSO runs `apprafter backup create` for completeness (same pg-helper path).
#
# This is a READ-ONLY reproduction: it does NOT modify shipped 2.6d code.
#
# INVOCATION (rootful sandbox-run microVM — rootless podman's 8MB memlock
# kills cilium-agent; see reference_sandbox_run_cilium_walk):
#
#   sandbox-run -- env HOME=/root XDG_CACHE_HOME=/root/.cache \
#     NIX_CONFIG="experimental-features = nix-command flakes" \
#     CARGO_TARGET_DIR=/tmp/target APPRAFTER_E2E_LOCAL_OPERATOR=1 \
#     nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc nixpkgs#pkg-config \
#       nixpkgs#kubectl nixpkgs#kubernetes-helm nixpkgs#kind nixpkgs#cilium-cli \
#       nixpkgs#hubble nixpkgs#cue nixpkgs#jq nixpkgs#restic \
#     -c bash e2e/export-cilium-probe.sh
#
# Judge PASS/FAIL by READING THE LOG (sandbox-run masks the inner exit code):
# grep for the `PROBE RESULT:` banner and the pg-helper connection outcome.
#
# Exit codes (informational only — sandbox-run masks them):
#   0 — probe ran to a conclusion (read the banner for REACHED vs BLOCKED)
#   1 — setup/assertion failure BEFORE the export question could be answered
#   2 — precondition missing

set -euo pipefail

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# Cilium-only: force the kind runtime (Cilium's eBPF datapath is slow on k3d;
# kind_up_cilium rejects k3d anyway).
export APPRAFTER_E2E_RUNTIME=kind

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-export-cilium-probe"

APP_NS="demo"                       # tenant namespace (== app ns == helper ns)
APP_PG="web"                        # needs.pg Application
CNP_PG="${APP_PG}-egress"           # rendered CNP name

APP_RES="application.apprafter.io"
CNP_RES="ciliumnetworkpolicies.cilium.io"

CNPG_NS="cnpg-system"
PG_SERVICE="platform-postgres-rw.${CNPG_NS}"

OPERATOR_NS="apprafter-system"

# ---------------------------------------------------------------
# Tool checks
# ---------------------------------------------------------------

for tool in cargo kubectl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done
if ! command -v docker >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
    printf 'ERROR: neither "docker" nor "podman" found on PATH\n' >&2
    exit 2
fi
if ! command -v cilium >/dev/null 2>&1 && ! command -v nix >/dev/null 2>&1; then
    # shellcheck disable=SC2016
    printf 'ERROR: neither a `cilium` binary nor `nix` is on PATH\n' >&2
    exit 2
fi

# `apprafter backup` shells out to restic — ensure it resolves for the child.
ensure_restic_on_path

# ---------------------------------------------------------------
# Temp workspace
# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"
EXPORT_DIR="${TMPDIR_WORK}/export-out"

K3D_CREATED=0

cleanup() {
    local exit_code=$?
    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! export-cilium-probe FAILED at %s (exit %d) !!!\n' \
            "$(elapsed)" "$exit_code" >&2
        if [ "$K3D_CREATED" -eq 1 ]; then
            dump_diagnostics
            probe_cilium_diagnostics
        fi
    fi
    if [ "$K3D_CREATED" -eq 1 ]; then
        if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
            k3d_down "$CLUSTER_NAME" || true
        else
            printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' \
                "$CLUSTER_NAME"
            printf 'Run: kind delete cluster --name %s\n' "$CLUSTER_NAME"
        fi
    fi
    rm -rf "$TMPDIR_WORK"
    exit "$exit_code"
}
trap cleanup EXIT

# ---------------------------------------------------------------
# Cilium/policy diagnostics — the "which policy" answer if BLOCKED.
# ---------------------------------------------------------------
probe_cilium_diagnostics() {
    [ -n "${KUBECONFIG:-}" ] || return 0
    printf '\n===== policy diagnostics (mechanism) =====\n' >&2
    printf '\n--- CiliumNetworkPolicies (all namespaces) ---\n' >&2
    kubectl get "$CNP_RES" -A >&2 2>&1 || true
    printf '\n--- CiliumClusterwideNetworkPolicies ---\n' >&2
    kubectl get ciliumclusterwidenetworkpolicies.cilium.io >&2 2>&1 || true
    printf '\n--- NetworkPolicies (all namespaces) ---\n' >&2
    kubectl get networkpolicies -A >&2 2>&1 || true
    printf '\n--- web-egress CNP spec.egress ---\n' >&2
    kubectl -n "$APP_NS" get "$CNP_RES" "$CNP_PG" -o jsonpath='{.spec.egress}' >&2 2>&1 || true
    printf '\n--- recent Hubble flows (demo ns, backup-helper) ---\n' >&2
    hubble_cli observe --namespace "$APP_NS" --last 80 >&2 2>&1 || true
    printf '\n===== end policy diagnostics =====\n' >&2
}

# ---------------------------------------------------------------
# seed the CLI state store (mirrors needs-networkpolicy-walk.sh).
# ---------------------------------------------------------------
seed_apprafter_state() {
    local kubeconfig_content="$1"
    mkdir -p "${APPRAFTER_CONFIG_DIR}"
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/k3d/.apprafter"
    cat >"${APPRAFTER_CONFIG_DIR}/config.yaml" <<'YAML'
active_target: k3d
version: 1
YAML
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
# wait_jsonpath <kind> <ns> <name> <jsonpath> <want> [timeout]
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

jp() { kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true; }

# ===============================================================
# Phase 0: bring up kind+Cilium
# ===============================================================

phase "Phase 0: kind_up_cilium ${CLUSTER_NAME} (default CNI + kube-proxy disabled)"
kind_up_cilium "$CLUSTER_NAME"
cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
K3D_CREATED=1
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

# ===============================================================
# Phase 1: cluster-bootstrap WITH Cilium + Hubble (best-effort)
# ===============================================================

phase "Phase 1: cluster-bootstrap (Cilium kube-proxy replacement) + Hubble"
kubeconfig_content=$(cat "$KUBECONFIG_FILE")
seed_apprafter_state "$kubeconfig_content"
export APPRAFTER_CONFIG_DIR
printf '  APPRAFTER_CONFIG_DIR=%s\n' "$APPRAFTER_CONFIG_DIR"

bootstrap_with_cilium
printf '  cluster-bootstrap complete; waiting for Cilium to converge ...\n'
cilium_cli status --wait --wait-duration 8m

HUBBLE_OK=0
printf '  enabling Hubble (best-effort; supplementary drop-verdict evidence) ...\n'
if cilium_cli hubble enable 2>/dev/null; then
    cilium_cli status --wait --wait-duration 5m 2>/dev/null || true
    deadline=$(( $(date +%s) + 180 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if cilium_cli status 2>/dev/null | grep -qi 'Hubble Relay.*OK'; then
            HUBBLE_OK=1; break
        fi
        sleep 10
    done
fi
if [ "$HUBBLE_OK" = 1 ]; then
    cilium_cli hubble port-forward >/dev/null 2>&1 &
    sleep 5
    printf '  Hubble Relay -> OK\n'
else
    printf '  WARN: Hubble unavailable; drop-verdict evidence skipped (dump outcome is decisive)\n' >&2
fi

# ---------------------------------------------------------------
# Phase 1b: build + side-load the WORKING-TREE operator + webhook and apply
# branch CRDs/RBAC (2.6d/2.10 surface; mirrors needs-networkpolicy-walk.sh).
# ---------------------------------------------------------------
phase "Phase 1b: build + load working-tree operator + webhook"
builder=podman; command -v podman >/dev/null 2>&1 || builder=docker

build_load_restart() { # <deployment> <operator-subdir>
    local dep="$1" sub="$2" img
    printf '  waiting for the %s Deployment to appear ...\n' "$dep"
    for _ in $(seq 1 60); do
        kubectl -n "$OPERATOR_NS" get deploy "$dep" >/dev/null 2>&1 && break
        sleep 5
    done
    img=$(kubectl -n "$OPERATOR_NS" get deploy "$dep" \
        -o jsonpath='{.spec.template.spec.containers[0].image}')
    printf '  building %s from the working tree (%s) ...\n' "$img" "$builder"
    "$builder" build -f "${REPO_ROOT}/operator/${sub}/Dockerfile" \
        -t "$img" "${REPO_ROOT}/operator"
    cluster_load_image "$CLUSTER_NAME" "$img"
    kubectl -n "$OPERATOR_NS" rollout restart "deploy/${dep}"
    kubectl -n "$OPERATOR_NS" rollout status "deploy/${dep}" --timeout=180s
}
build_load_restart apprafter-operator apprafter-operator
build_load_restart admission-webhook admission-webhook

printf '  waiting for the old (released) webhook pod to fully terminate ...\n'
_wh_deadline=$(( $(date +%s) + 90 ))
while [ "$(date +%s)" -lt "$_wh_deadline" ]; do
    [ "$(kubectl -n "$OPERATOR_NS" get pods \
        -l app.kubernetes.io/name=admission-webhook --no-headers 2>/dev/null \
        | wc -l)" -le 1 ] && break
    sleep 3
done

printf '  applying branch operator CRDs + RBAC ...\n'
for _app in platform apprafter-operator; do
    kubectl -n argocd patch application.argoproj.io "$_app" --type=merge \
        -p '{"spec":{"syncPolicy":{"automated":null}}}' >/dev/null 2>&1 || true
done
_yq() { if command -v yq >/dev/null 2>&1; then yq "$@"; else nix run nixpkgs#yq-go -- "$@"; fi; }
helm template apprafter-operator "${REPO_ROOT}/operator/charts/apprafter-operator" \
    --namespace "$OPERATOR_NS" \
    | _yq 'select(.kind == "CustomResourceDefinition")' \
    | kubectl apply --server-side --force-conflicts -f -
helm template apprafter-operator "${REPO_ROOT}/operator/charts/apprafter-operator" \
    --namespace "$OPERATOR_NS" \
    | _yq 'select(.kind == "ClusterRole" or .kind == "ClusterRoleBinding")' \
    | kubectl apply --server-side --force-conflicts -f -
for _crd in applications serviceproviders resourceclaims retainedclaims platformstacks; do
    retry 12 5 -- kubectl wait --for=condition=Established \
        "crd/${_crd}.apprafter.io" --timeout=30s
done
kubectl -n "$OPERATOR_NS" rollout restart deploy/apprafter-operator
kubectl -n "$OPERATOR_NS" rollout status deploy/apprafter-operator --timeout=180s
printf '  operator + webhook now running the working-tree build\n'

# ===============================================================
# Phase 2: platform readiness (AppProject, CNPG operator, webhook)
# ===============================================================

phase "Phase 2: platform readiness"
printf '  waiting for AppProject apps ...\n'
deadline=$(( $(date +%s) + 600 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1 && break
    sleep 10
done
kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1 || {
    printf 'ERROR: AppProject apps not found after 10 min\n' >&2; exit 1; }
printf '  waiting for the CNPG operator Deployment ...\n'
retry 30 10 -- kubectl -n "$CNPG_NS" rollout status \
    deploy -l app.kubernetes.io/name=cloudnative-pg --timeout=60s
printf '  waiting for the admission-webhook Deployment ...\n'
retry 30 10 -- kubectl -n "$OPERATOR_NS" rollout status \
    deploy admission-webhook --timeout=60s

# ===============================================================
# Phase 3: apply a needs.pg app -> Ready + its egress CNP
# ===============================================================

phase "Phase 3: apply needs.pg Application '${APP_PG}' in ns '${APP_NS}'"
kubectl create namespace "$APP_NS" 2>/dev/null || true
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP_PG}
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
        selector:
          tier: integrated
        size: small
YAML

wait_jsonpath "$APP_RES" "$APP_NS" "$APP_PG" '{.status.phase}' Ready 420
kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP_PG}" --timeout=300s
printf '  web Deployment Available\n'

# Confirm the 2.10 per-app egress CNP is in place (this is the policy whose
# presence — but NON-selection of the helper pod — is the crux of the question).
wait_jsonpath "$CNP_RES" "$APP_NS" "$CNP_PG" '{.metadata.name}' "$CNP_PG" 120
web_egress_json=$(jp "$CNP_RES" "$APP_NS" "$CNP_PG" '{.spec.egress}')
printf '  web-egress CNP spec.egress: %s\n' "$web_egress_json"
case "$web_egress_json" in
    *"$CNPG_NS"*) printf '  ok: web-egress references cnpg-system (the app IS allowed to pg)\n' ;;
    *) printf '  WARN: web-egress has no cnpg-system rule (unexpected): %s\n' "$web_egress_json" >&2 ;;
esac

# Sanity: the shared CNPG cluster + its rw Service exist (the helper's target).
printf '  CNPG cluster + rw Service:\n'
kubectl -n "$CNPG_NS" get cluster.postgresql.cnpg.io 2>&1 | sed 's/^/    /' || true
kubectl -n "$CNPG_NS" get svc "${PG_SERVICE%%.*}" 2>&1 | sed 's/^/    /' || true

# Record the connection Secret the extractor will read (host/user/port/db).
CONN_SECRET=$(kubectl -n "$APP_NS" get resourceclaims.apprafter.io \
    -o jsonpath='{.items[0].status.connectionSecretRef}' 2>/dev/null || true)
printf '  claim connectionSecretRef: %s\n' "${CONN_SECRET:-<none>}"
if [ -n "$CONN_SECRET" ]; then
    _h=$(kubectl -n "$APP_NS" get secret "$CONN_SECRET" -o jsonpath='{.data.host}' 2>/dev/null | base64 -d 2>/dev/null || true)
    printf '  connection host (helper pg_dump -h target): %s\n' "${_h:-<unreadable>}"
fi

# ===============================================================
# Phase 4: THE PROBE — apprafter export drives the pg helper pod
# ===============================================================

phase "Phase 4: apprafter export — pg helper pod reaches (or is blocked by) Cilium"

mkdir -p "$EXPORT_DIR"

# Run export in the BACKGROUND so we can catch the transient bk-pg-* helper
# pod live (it is short-lived: apply -> exec pg_dump -> delete). Capture its
# full output for the verdict.
EXPORT_LOG="${TMPDIR_WORK}/export.log"
# shellcheck disable=SC2016  # literal backticks in the user-facing message
printf '  launching `apprafter export --out %s` (background; watching for the bk-pg-* helper) ...\n' "$EXPORT_DIR"
( apprafter export --out "$EXPORT_DIR" >"$EXPORT_LOG" 2>&1; echo "EXPORT_EXIT=$?" >>"$EXPORT_LOG" ) &
EXPORT_BG=$!

# Poll for the helper pod for up to ~5 min; snapshot its describe + logs the
# moment it appears (it is deleted best-effort after the stream).
HELPER_SEEN=0
_probe_deadline=$(( $(date +%s) + 330 ))
while [ "$(date +%s)" -lt "$_probe_deadline" ]; do
    _hp=$(kubectl -n "$APP_NS" get pod -l apprafter.io/backup-helper=true \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
    if [ -n "$_hp" ]; then
        HELPER_SEEN=1
        printf '  >>> pg helper pod appeared: %s\n' "$_hp"
        printf '  --- helper pod describe (labels + status) ---\n'
        kubectl -n "$APP_NS" get pod "$_hp" \
            -o jsonpath='{"  labels="}{.metadata.labels}{"\n  phase="}{.status.phase}{"\n"}' 2>&1 | sed 's/^/    /' || true
        # Let pg_dump run; the helper is deleted after the stream, so grab logs while alive.
        for _ in $(seq 1 12); do
            kubectl -n "$APP_NS" get pod "$_hp" >/dev/null 2>&1 || break
            sleep 2
        done
        break
    fi
    kill -0 "$EXPORT_BG" 2>/dev/null || break   # export already finished
    sleep 2
done

# Wait for export to finish.
wait "$EXPORT_BG" 2>/dev/null || true
printf '\n  ===== apprafter export output =====\n'
sed 's/^/    /' "$EXPORT_LOG" || true
printf '  ===== end export output =====\n\n'
EXPORT_EXIT=$(grep -oE 'EXPORT_EXIT=[0-9]+' "$EXPORT_LOG" | tail -1 | cut -d= -f2)
EXPORT_EXIT="${EXPORT_EXIT:-1}"

# ---------------------------------------------------------------
# VERDICT: did the pg helper reach pg (a non-empty dump) or was it blocked?
# ---------------------------------------------------------------
DUMP_FILE=$(find "$EXPORT_DIR/pg" -name '*.dump' 2>/dev/null | head -1 || true)
DUMP_OK=0
if [ -n "$DUMP_FILE" ] && [ -s "$DUMP_FILE" ]; then
    DUMP_OK=1
    DUMP_SIZE=$(wc -c <"$DUMP_FILE" 2>/dev/null || echo 0)
fi

printf '=====================================================================\n'
if [ "$EXPORT_EXIT" = "0" ] && [ "$DUMP_OK" -eq 1 ]; then
    printf 'PROBE RESULT: pg helper REACHED pg — 2.6d export WORKS on Cilium.\n'
    printf '  export exit=0; dump=%s (%s bytes, non-empty pg_dump -Fc output).\n' \
        "$DUMP_FILE" "${DUMP_SIZE:-?}"
    # shellcheck disable=SC2016  # literal backticks in the user-facing message
    printf '  => The earlier real-Hetzner `Operation not permitted` was NOT reproduced\n'
    printf '     here; on kind+Cilium the plain backup-helper pod is default-allow egress.\n'
    PROBE_VERDICT=REACHED
else
    printf 'PROBE RESULT: pg helper appears BLOCKED / export FAILED on Cilium.\n'
    printf '  export exit=%s; dump present+nonempty=%s (file=%s).\n' \
        "$EXPORT_EXIT" "$DUMP_OK" "${DUMP_FILE:-<none>}"
    printf '  Decisive export error lines:\n'
    grep -iE 'operation not permitted|connection|could not connect|timed out|timeout|refused|pg_dump|exec exited|no route' \
        "$EXPORT_LOG" 2>/dev/null | sed 's/^/    /' || true
    PROBE_VERDICT=BLOCKED
    # Dump the policy mechanism (which CNP/NP/clusterwide caused it).
    probe_cilium_diagnostics
fi
printf '  helper pod was%s observed live.\n' \
    "$( [ "$HELPER_SEEN" -eq 1 ] && echo '' || echo ' NOT' )"
printf '=====================================================================\n\n'

# ===============================================================
# Phase 5: apprafter backup create — same pg-helper path, for completeness
# ===============================================================

phase "Phase 5: apprafter backup create (same pg-helper path; completeness)"
BACKUP_REPO="${TMPDIR_WORK}/restic-repo"
BACKUP_LOG="${TMPDIR_WORK}/backup.log"
export RESTIC_PASSWORD="probe-passphrase"
if apprafter backup create --repo "$BACKUP_REPO" --passphrase "$RESTIC_PASSWORD" \
        >"$BACKUP_LOG" 2>&1; then
    printf '  backup create exit=0\n'
    BACKUP_VERDICT=OK
else
    printf '  backup create FAILED (exit %d)\n' "$?"
    BACKUP_VERDICT=FAIL
fi
printf '  ===== apprafter backup output (tail) =====\n'
tail -40 "$BACKUP_LOG" 2>/dev/null | sed 's/^/    /' || true
printf '  ===== end backup output =====\n'

# ===============================================================
# Done
# ===============================================================
trap - EXIT
if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    k3d_down "$CLUSTER_NAME" || true
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' "$CLUSTER_NAME"
fi
[ -n "${RESTIC_WRAPPER_BIN_DIR:-}" ] && rm -rf "$RESTIC_WRAPPER_BIN_DIR"
rm -rf "$TMPDIR_WORK"

printf '\n========================= PROBE SUMMARY =========================\n'
printf 'export pg-helper verdict : %s\n' "${PROBE_VERDICT:-UNKNOWN}"
printf 'backup create verdict    : %s\n' "${BACKUP_VERDICT:-UNKNOWN}"
printf 'elapsed                  : %s\n' "$(elapsed)"
printf '=================================================================\n'
printf '\nexport-cilium-probe COMPLETE (read PROBE RESULT above for REACHED vs BLOCKED)\n'
