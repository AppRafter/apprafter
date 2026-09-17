#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter ingress-datapath ORDERING probe on a real kind+Cilium cluster —
# the empirical answer to why `cluster-bootstrap`'s ingress gate
# (`wait_for_ingress_datapath`, commit ea4b3f2) fails on EVERY run that
# actually exercises it.
#
# THE QUESTION
# ------------
# Since ea4b3f2 reached CI (pushed 2026-09-16), the two nightlies that run
# `apprafter cluster-bootstrap` against REAL Cilium — `nightly` (e2e/mvp.sh,
# Hetzner) and `e2e-networkpolicy-nightly` (kind+Cilium) — fail at step (c)
# of the gate:
#
#   these cilium pods started BEFORE the Gateway API CRDs existed …
#
# while EVERY Argo CD Application reports Synced/Healthy. Two readings fit
# the CI logs and they call for opposite fixes:
#
#   TRUE POSITIVE  — the wave -25 (`gateway-api-crds`) → wave -20 (`cilium`)
#                    ordering is NOT enforced between child Applications, so
#                    the gateway-enabled cilium upgrade races the CRD install.
#                    The cilium pods that win the race never register the
#                    Gateway API controller and the cluster's ingress is dead
#                    — exactly the Cloudflare-521 incident the gate was
#                    written for. The gate is right and the chart is wrong.
#   FALSE POSITIVE — the pods DID roll after the CRDs and the gate is reading
#                    the wrong timestamps (e.g. the CRD is re-created by a
#                    later sync, resetting `creationTimestamp` past pods that
#                    are perfectly current). The gate is wrong.
#
# The CI logs cannot separate these: `kubectl get pods` AGE has 1-second
# resolution and the two candidate pod/CRD timestamps sit ~2 seconds apart.
# This probe reads the timestamps themselves.
#
# HOW IT WORKS
# ------------
# Reuses the 2.10 walk's setup EXACTLY up to the failure point (kind_up_cilium
# → bootstrap_with_cilium) and lets `cluster-bootstrap` fail the way CI does.
# Then it prints, to the microsecond the apiserver recorded:
#
#   1. ORDERING — `creationTimestamp` of every gateway.networking.k8s.io CRD
#      and of every cilium agent/operator/envoy pod, plus the two child
#      Applications' `operationState.{startedAt,finishedAt}`. Wave -25 must
#      finish before wave -20 starts; if the two overlap, the ordering the
#      chart documents does not hold.
#   2. PRODUCT TRUTH — does the ingress actually work? A minimal
#      `GatewayClass cilium` Gateway is applied and given 3 minutes to reach
#      `Programmed=True`. Cilium programs a Gateway in about a second once
#      its Gateway API controller is registered, so this separates "the gate
#      is pedantic" from "nothing will ever serve a request here".
#   3. The cilium-operator log's own account of whether it registered the
#      Gateway API controller at startup.
#
# This probe does NOT modify shipped code; it only reads. It leaves the
# cluster torn down unless APPRAFTER_E2E_SKIP_DESTROY=1.
#
# INVOCATION (rootful sandbox-run microVM — rootless podman's 8MB memlock
# kills cilium-agent; see reference_sandbox_run_cilium_walk):
#
#   sandbox-run -- env HOME=/root XDG_CACHE_HOME=/root/.cache \
#     NIX_CONFIG="experimental-features = nix-command flakes" \
#     CARGO_TARGET_DIR=/tmp/target \
#     nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc nixpkgs#pkg-config \
#       nixpkgs#kubectl nixpkgs#kubernetes-helm nixpkgs#kind nixpkgs#cilium-cli \
#       nixpkgs#cue nixpkgs#jq \
#     -c bash e2e/gateway-order-probe.sh
#
# Judge PASS/FAIL by READING THE LOG (sandbox-run masks the inner exit code):
# grep for the `PROBE RESULT:` banner.
#
# Exit codes (informational only — sandbox-run masks them):
#   0 — probe ran to a conclusion (read the banner)
#   2 — precondition missing

set -uo pipefail

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

export APPRAFTER_E2E_RUNTIME=kind

CLUSTER_NAME="apprafter-gwprobe"
GWCLASS_CRD="gatewayclasses.gateway.networking.k8s.io"
PROBE_GW_NS="apprafter-system"
PROBE_GW="probe"

for tool in cargo kubectl helm jq; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done
if ! command -v docker >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
    printf 'ERROR: neither "docker" nor "podman" found on PATH\n' >&2
    exit 2
fi

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"
K3D_CREATED=0

cleanup() {
    if [ "$K3D_CREATED" = 1 ] && [ "${APPRAFTER_E2E_SKIP_DESTROY:-0}" != 1 ]; then
        printf '\nTearing down cluster (set APPRAFTER_E2E_SKIP_DESTROY=1 to keep).\n'
        _kind delete cluster --name "$CLUSTER_NAME" >/dev/null 2>&1 || true
    fi
    rm -rf "$TMPDIR_WORK"
}
trap cleanup EXIT

seed_apprafter_state() {
    local kubeconfig_content="$1" kc_escaped
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/k3d/.apprafter"
    cat >"${APPRAFTER_CONFIG_DIR}/config.yaml" <<'YAML'
active_target: k3d
version: 1
YAML
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

# ===============================================================
# Phase 0: bring up kind+Cilium
# ===============================================================

phase "Phase 0: kind_up_cilium ${CLUSTER_NAME} (default CNI + kube-proxy disabled)"
# `set -e` is deliberately OFF for the probe (Phase 1 is EXPECTED to fail), so
# cluster-up has to be checked by hand — without a cluster every later phase
# would read an ambient KUBECONFIG, which an e2e must never touch.
if ! kind_up_cilium "$CLUSTER_NAME"; then
    printf 'ERROR: kind_up_cilium failed — no cluster, nothing to probe.\n' >&2
    exit 2
fi
cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
K3D_CREATED=1
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

# ===============================================================
# Phase 1: cluster-bootstrap WITH Cilium — expected to fail at the gate
# ===============================================================

phase "Phase 1: cluster-bootstrap (Cilium on) — the gate may fail; we continue either way"
kubeconfig_content=$(cat "$KUBECONFIG_FILE")
seed_apprafter_state "$kubeconfig_content"
export APPRAFTER_CONFIG_DIR
printf '  APPRAFTER_CONFIG_DIR=%s\n' "$APPRAFTER_CONFIG_DIR"

BOOTSTRAP_RC=0
unset APPRAFTER_BOOTSTRAP_SKIP_CILIUM
( cd "${REPO_ROOT}/cli" && cargo run --quiet --bin apprafter -- cluster-bootstrap ) \
    || BOOTSTRAP_RC=$?
printf '\n  cluster-bootstrap exit code: %s\n' "$BOOTSTRAP_RC"

# ===============================================================
# Phase 2: the timestamps themselves
# ===============================================================

phase "Phase 2: ORDERING — who was created when, to the apiserver's own precision"

printf '\n--- gateway.networking.k8s.io CRDs (creationTimestamp) ---\n'
kubectl get crd -o json 2>/dev/null \
    | jq -r '.items[]
             | select(.spec.group=="gateway.networking.k8s.io")
             | "\(.metadata.creationTimestamp)  \(.metadata.name)"' \
    | sort || true

printf '\n--- cilium pods (creationTimestamp / status.startTime / uid) ---\n'
kubectl -n kube-system get pods \
    -l 'k8s-app in (cilium,cilium-envoy)' -o json 2>/dev/null \
    | jq -r '.items[] | "\(.metadata.creationTimestamp)  start=\(.status.startTime)  \(.metadata.name)  uid=\(.metadata.uid)"' \
    | sort || true
kubectl -n kube-system get pods -l 'io.cilium/app=operator' -o json 2>/dev/null \
    | jq -r '.items[] | "\(.metadata.creationTimestamp)  start=\(.status.startTime)  \(.metadata.name)  uid=\(.metadata.uid)"' \
    | sort || true

printf '\n--- cilium workload controllers (creation + observed generation) ---\n'
for w in ds/cilium ds/cilium-envoy deploy/cilium-operator; do
    printf '  %s: created=%s generation=%s\n' "$w" \
        "$(kubectl -n kube-system get "$w" -o jsonpath='{.metadata.creationTimestamp}' 2>/dev/null)" \
        "$(kubectl -n kube-system get "$w" -o jsonpath='{.metadata.generation}' 2>/dev/null)"
done

printf '\n--- the two child Applications: wave, sync window, revision ---\n'
# GROUP-QUALIFIED: by this point the cluster serves BOTH `argoproj.io`
# Applications and `apprafter.io` ones, and a bare `application` resolves to
# whichever the discovery cache prefers — on the first run of this probe it
# resolved to apprafter.io and every field below came back empty, which reads
# as "Argo CD never created these apps" rather than "you asked the wrong API".
for app in gateway-api-crds cilium; do
    printf '  === %s ===\n' "$app"
    kubectl -n argocd get applications.argoproj.io "$app" -o json 2>/dev/null | jq -r '
        "    sync-wave      = \(.metadata.annotations["argocd.argoproj.io/sync-wave"] // "-")",
        "    created        = \(.metadata.creationTimestamp)",
        "    op.startedAt   = \(.status.operationState.startedAt // "-")",
        "    op.finishedAt  = \(.status.operationState.finishedAt // "-")",
        "    op.phase       = \(.status.operationState.phase // "-")",
        "    sync/health    = \(.status.sync.status // "-")/\(.status.health.status // "-")",
        "    reconciledAt   = \(.status.reconciledAt // "-")"' || true
done

printf '\n--- root application platform: sync window ---\n'
kubectl -n argocd get applications.argoproj.io platform -o json 2>/dev/null | jq -r '
    "    created        = \(.metadata.creationTimestamp)",
    "    op.startedAt   = \(.status.operationState.startedAt // "-")",
    "    op.finishedAt  = \(.status.operationState.finishedAt // "-")",
    "    sync/health    = \(.status.sync.status // "-")/\(.status.health.status // "-")"' || true

printf '\n--- what the gate itself reads ---\n'
CRD_TS=$(kubectl get crd "$GWCLASS_CRD" -o jsonpath='{.metadata.creationTimestamp}' 2>/dev/null)
printf '  %s creationTimestamp = %s\n' "$GWCLASS_CRD" "${CRD_TS:-<absent>}"

printf '\n--- cilium-config: is gateway-api on in the ConfigMap? ---\n'
printf '  enable-gateway-api = %s\n' \
    "$(kubectl -n kube-system get cm cilium-config -o jsonpath='{.data.enable-gateway-api}' 2>/dev/null)"

printf '\n--- GatewayClass cilium ---\n'
kubectl get gatewayclass cilium -o json 2>/dev/null \
    | jq -r '"  created=\(.metadata.creationTimestamp)",
             "  conditions=\(.status.conditions // [] | map("\(.type)=\(.status)") | join(" "))"' \
    || printf '  <absent>\n'

printf '\n--- did cilium-operator register the Gateway API controller? ---\n'
kubectl -n kube-system logs deploy/cilium-operator --tail=400 2>/dev/null \
    | grep -iE 'gateway|gatewayclass|tlsroute' | head -40 \
    || printf '  (no gateway lines in the operator log)\n'

# ===============================================================
# Phase 3: PRODUCT TRUTH — can a Gateway ever be programmed here?
# ===============================================================

phase "Phase 3: apply a minimal Gateway and see whether Cilium programs it"

GW_PROGRAMMED="unknown"
if [ -z "${CRD_TS:-}" ]; then
    printf '  the gatewayclasses CRD is absent — no Gateway to apply.\n'
    GW_PROGRAMMED="no-crd"
else
    kubectl create namespace "$PROBE_GW_NS" >/dev/null 2>&1 || true
    cat <<YAML | kubectl apply -f - || true
apiVersion: gateway.networking.k8s.io/v1
kind: Gateway
metadata:
  name: ${PROBE_GW}
  namespace: ${PROBE_GW_NS}
spec:
  gatewayClassName: cilium
  listeners:
    - name: http
      protocol: HTTP
      port: 8080
      allowedRoutes:
        namespaces:
          from: All
YAML
    printf '  waiting up to 180s for Gateway/%s to report Programmed=True ...\n' "$PROBE_GW"
    deadline=$(( $(date +%s) + 180 ))
    GW_PROGRAMMED="False"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        got=$(kubectl -n "$PROBE_GW_NS" get gateway "$PROBE_GW" \
            -o jsonpath='{.status.conditions[?(@.type=="Programmed")].status}' 2>/dev/null)
        if [ "$got" = "True" ]; then
            GW_PROGRAMMED="True"; break
        fi
        sleep 10
    done
    printf '  Gateway/%s Programmed = %s\n' "$PROBE_GW" "$GW_PROGRAMMED"
    kubectl -n "$PROBE_GW_NS" get gateway "$PROBE_GW" -o json 2>/dev/null \
        | jq -r '"  conditions=\(.status.conditions // [] | map("\(.type)=\(.status)(\(.reason))") | join(" "))"' || true
fi

# ===============================================================
# Verdict
# ===============================================================

phase "PROBE RESULT"

printf '  cluster-bootstrap exit code ......... %s\n' "$BOOTSTRAP_RC"
printf '  gatewayclasses CRD creationTimestamp  %s\n' "${CRD_TS:-<absent>}"
printf '  Gateway Programmed .................. %s\n' "$GW_PROGRAMMED"
printf '\n'
if [ "$GW_PROGRAMMED" = "True" ] && [ "$BOOTSTRAP_RC" != 0 ]; then
    printf '  PROBE RESULT: GATE IS A FALSE POSITIVE — a Gateway programs on this\n'
    printf '  cluster, so the ingress datapath is live and the pod-age check is\n'
    printf '  reading the wrong thing. Compare the Phase 2 timestamps.\n'
elif [ "$GW_PROGRAMMED" != "True" ] && [ "$BOOTSTRAP_RC" != 0 ]; then
    printf '  PROBE RESULT: GATE IS A TRUE POSITIVE — no Gateway is ever programmed\n'
    printf '  on this cluster. The ingress datapath really is dead while every Argo\n'
    printf '  CD Application reports Synced/Healthy. Read the Phase 2 ordering to\n'
    printf '  see which side of the -25/-20 boundary actually landed first.\n'
else
    printf '  PROBE RESULT: bootstrap SUCCEEDED (the gate did not fire).\n'
    printf '  Two different things produce this line, and only Phase 2 tells them\n'
    printf '  apart — read the margin, not the exit code:\n'
    printf '    * ORDERED — the `cilium` child Application was CREATED only after\n'
    printf '      `gateway-api-crds` reported op.phase=Succeeded and Synced/Healthy,\n'
    printf '      and the CRD timestamps precede every cilium pod by a wide margin\n'
    printf '      (~20s measured). The wave really is enforced.\n'
    printf '    * LUCKY — the two Applications were created seconds apart and cilium\n'
    printf '      merely lost the race this time. The next run can go the other way.\n'
    printf '  A third tell is decisive and free: the cilium-operator log above either\n'
    printf '  carries `Required GatewayAPI resources are not found` or goes straight\n'
    printf '  on to `Starting Controller … controllerKind=Gateway`.\n'
fi

exit 0
