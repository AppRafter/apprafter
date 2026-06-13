#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter gateway-walk e2e — the 1.83a host-network platform Gateway PLUS the
# 1.83b operator-rendered per-Application public ingress, on a local kind+Cilium
# cluster (plan items 1.83a / 1.83b; 1.83a release 0.2.27 / commit 2bf0e25).
#
# WHY THIS WALK EXISTS
# --------------------
# 1.83a shipped a BLOCKER (F1) that NO kind/sandbox test caught: the
# `gateway-api-crds` component installed the Gateway API STANDARD channel, but
# Cilium 1.16.5's gateway controller runs a required-resources check at startup
# that includes `tlsroutes.gateway.networking.k8s.io` (v1alpha2) — a CRD that
# lives ONLY in the EXPERIMENTAL channel. With the standard channel that check
# fails, the `cilium` GatewayClass never Accepts, and no Gateway ever reaches
# Programmed. kind (T6) only did STRUCTURAL CRD validation; no real Cilium
# gateway controller ran there. The fix (0.2.27) switched the component to
# `config/crd/experimental`.
#
# This walk runs REAL Cilium 1.16.5 + its gateway controller under sandbox-run
# (a rootful microVM — unlimited memlock, so cilium-agent starts) with the
# host-network gateway values the chart ships, installs the EXPERIMENTAL Gateway
# API CRDs, applies the chart's rendered Gateway + HTTPRoute, and asserts:
#
#   * `tlsroutes.gateway.networking.k8s.io` CRD is Established (the F1 marker).
#   * Gateway `platform` reaches Programmed=True — THE LOAD-BEARING F1 GATE: the
#     cilium gateway controller only programs the Gateway (sets Accepted +
#     Programmed + all listeners Programmed) once it passes its startup
#     required-resources check, which needs tlsroutes. With the standard channel
#     the controller never starts and the Gateway never reaches Programmed.
#   * GatewayClass `cilium` Accepted — INFORMATIONAL ONLY: Cilium 1.16.5 does
#     NOT reliably write the Accepted status back onto the GatewayClass object
#     (it stays Unknown/"Waiting for controller" even on a fully-working install
#     where the Gateway IS Programmed — confirmed live 2026-06-12). The Gateway
#     Programmed status is the real signal, so we read the GatewayClass status
#     but do NOT gate on it.
#
# Run STANDARD-channel CRDs instead (APPRAFTER_GW_WALK_NEGATIVE=1) and the walk
# asserts the F1 SYMPTOM: the Gateway does NOT reach Programmed — proving the
# guard this walk encodes actually catches the regression.
#
# 1.83b — OPERATOR-RENDERED PUBLIC INGRESS (positive path only)
# -------------------------------------------------------------
# After the Gateway is Programmed (Phase 5), Phases 6-9 validate that the
# operator turns `expose.network: public` into a working public route:
#
#   Phase 6  — deploy the WORKING-TREE operator + Application CRD + admission
#              webhook (1.83b is UNRELEASED, so build the images from the
#              working tree + kind-load them, mirroring
#              needs-networkpolicy-walk.sh's Phase 1b). This walk does NOT run
#              `apprafter cluster-bootstrap`, so the chart's CRDs/RBAC/Deployment
#              are applied straight from the chart, and cert-manager is installed
#              standalone first (the webhook chart's Certificate + CA-injection
#              depend on it). Seeds a PlatformStack/default whose
#              gateway.allowedDomains carries the $SAMPLE_DOMAIN zone so the
#              operator's PublicRouteReady zone-coverage check passes.
#   Phase 7  — apply a public Application `web` (traefik/whoami, hostname
#              app.$SAMPLE_DOMAIN). Assert: operator renders httproute/web with
#              parentRefs[0].port=443; the route is Accepted + ResolvedRefs;
#              Application web PublicRouteReady=True + endpointURL
#              https://app.$SAMPLE_DOMAIN/; a curl through the node IP returns
#              HTTP 200 with a whoami body; http://:80 still 301-redirects (the
#              1.83a catch-all is untouched — the app route attaches to :443).
#   Phase 8  — apply an internal Application `internal-web`. Assert NO HTTPRoute
#              is created + its endpointURL is the cluster-DNS form.
#   Phase 9  — patch `web` public->internal. Assert httproute/web is pruned +
#              PublicRouteReady disappears from its status.
#
# In NEGATIVE mode (Gateway never programs) Phases 6-9 are SKIPPED — there is no
# working platform Gateway for an HTTPRoute to attach to.
#
# Required: kind (+ a container runtime — also used to `podman/docker build` the
# working-tree operator + webhook images in Phase 6), kubectl, helm, cue. yq-go,
# openssl and curl fall back to `nix run nixpkgs#…` when absent. Reuses
# e2e/lib.sh's kind+Cilium setup (the same memlock + bpffs + apiserver-pin
# config the 2.10 needs-networkpolicy-walk uses). Run it under sandbox-run:
#
#   sandbox-run -- nix shell nixpkgs#kind nixpkgs#kubernetes-helm \
#       nixpkgs#kubectl nixpkgs#yq-go nixpkgs#curl -c bash e2e/gateway-walk.sh
#
# Env:
#   APPRAFTER_GW_WALK_NEGATIVE=1  install the STANDARD channel instead +
#                                 assert the GatewayClass does NOT Accept (F1
#                                 symptom — proves the guard). SKIPS Phases 6-9.
#   APPRAFTER_E2E_SKIP_DESTROY=1  keep the cluster up after the run.
#
# Exit codes:
#   0 — gateway green (Accepted + Programmed; or, in negative mode, NOT Accepted)
#   1 — assertion failure
#   2 — precondition missing

set -euo pipefail

# ---------------------------------------------------------------
# Source shared helpers (kind_up_cilium, require_cilium_memlock,
# cluster_kubeconfig_write, k3d_down, dump_diagnostics, _kind, …).
# ---------------------------------------------------------------
# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# This walk is Cilium-only: force the kind runtime (kind_up_cilium rejects k3d;
# Cilium's eBPF datapath is pathologically slow on k3d).
export APPRAFTER_E2E_RUNTIME=kind

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-gateway-walk"

OPERATOR_NS="apprafter-system"      # where the platform Gateway + cert Secret live
SAMPLE_DOMAIN="demo.example.com"    # apex zone admitted to the Gateway
TLS_SECRET="platform-tls"           # kubernetes.io/tls Secret the listeners ref

CILIUM_VERSION="1.16.5"             # pinned: _loaderValues.cilium.chartVersion
CILIUM_REPO="https://helm.cilium.io/"
GATEWAY_API_VERSION="v1.2.1"        # gateway-api-crds component targetRevision
CERT_MANAGER_VERSION="v1.16.2"      # pinned: platform-stack component_cert-manager.cue
CERT_MANAGER_URL="https://github.com/cert-manager/cert-manager/releases/download/${CERT_MANAGER_VERSION}/cert-manager.yaml"

# 1.83b: operator-rendered public-ingress phases (6-9). A tenant app namespace,
# a public `web` app + an `internal-web` app, the whoami echo image (listens on
# :80, echoes `Hostname:` so the curl body assertion has a stable marker), and
# the public hostname (single-label subdomain under the registered zone so the
# operator's zone-coverage check passes → PublicRouteReady=True).
APP_NS="gw-walk-app"                # tenant namespace for the 1.83b apps
APP_PUBLIC="web"                    # network: public + hostname → an HTTPRoute
APP_INTERNAL="internal-web"         # network: internal → NO HTTPRoute
WHOAMI_IMAGE="traefik/whoami:latest"  # tiny HTTP echo on :80 (body has `Hostname:`)
APP_HOSTNAME="app.${SAMPLE_DOMAIN}" # the public host (single-label subdomain)

# The operator + admission-webhook charts (1.83b Phase 6 deploys the
# WORKING-TREE build, mirroring needs-networkpolicy-walk.sh's Phase 1b).
OPERATOR_CHART="${REPO_ROOT}/operator/charts/apprafter-operator"
WEBHOOK_CHART="${REPO_ROOT}/operator/charts/apprafter-admission-webhook"

# The F1 marker CRD — present ONLY in the experimental channel; Cilium 1.16.5's
# gateway controller requires it at its startup required-resources check.
TLSROUTE_CRD="tlsroutes.gateway.networking.k8s.io"

GW_API_EXPERIMENTAL_URL="https://github.com/kubernetes-sigs/gateway-api/releases/download/${GATEWAY_API_VERSION}/experimental-install.yaml"
GW_API_STANDARD_URL="https://github.com/kubernetes-sigs/gateway-api/releases/download/${GATEWAY_API_VERSION}/standard-install.yaml"

# Negative mode installs the STANDARD channel + asserts the F1 SYMPTOM
# (GatewayClass does NOT Accept) instead of the positive Accepted/Programmed.
NEGATIVE="${APPRAFTER_GW_WALK_NEGATIVE:-0}"

REPO_STACK_DIR="${REPO_ROOT}/platform-stack"

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------

for tool in kubectl helm; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done

# kind needs a container runtime: docker or podman.
if ! command -v docker >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
    printf 'ERROR: neither "docker" nor "podman" found on PATH\n' >&2
    exit 2
fi

# cue is needed to read currentVersion (→ which dist chart to render). Prefer a
# bare `cue`, then the project's ~/bin/cue wrapper, else the nixpkgs fallback.
# `${HOME:-}` guards the case where HOME is empty at this point (sandbox-run —
# the temp-workspace block below re-points it) so `set -u` does not trip.
_cue() {
    if command -v cue >/dev/null 2>&1; then cue "$@";
    elif [ -n "${HOME:-}" ] && [ -x "${HOME}/bin/cue" ]; then "${HOME}/bin/cue" "$@";
    else nix run nixpkgs#cue -- "$@"; fi
}
if ! command -v cue >/dev/null 2>&1 && [ ! -x "${HOME:-}/bin/cue" ] && ! command -v nix >/dev/null 2>&1; then
    # shellcheck disable=SC2016  # literal backticks in the user-facing message
    printf 'ERROR: no `cue` binary, no ~/bin/cue, and no `nix` for the fallback\n' >&2
    exit 2
fi

# ---------------------------------------------------------------
# Temp workspace
# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"

# A writable HOME is required by `cue` (its cache dir: "cannot determine system
# cache directory: neither $XDG_CACHE_HOME nor $HOME are defined") and by `helm`
# (`$HOME/.config/helm` for `helm repo add`). Under `sandbox-run` the microVM
# starts with HOME EMPTY (the help text's "writable /root" is not exported as
# HOME), so point both at the ephemeral temp workspace when HOME is unset/empty.
if [ -z "${HOME:-}" ]; then
    export HOME="${TMPDIR_WORK}/home"
    mkdir -p "$HOME"
    printf '  HOME was empty (sandbox-run) — set HOME=%s for cue/helm caches\n' "$HOME"
fi
export XDG_CACHE_HOME="${XDG_CACHE_HOME:-${TMPDIR_WORK}/cache}"
mkdir -p "$XDG_CACHE_HOME"

# Set to 1 only after the cluster is up AND $KUBECONFIG points at it. Until
# then, dump_diagnostics / k3d_down must NOT run — on a cluster-up failure
# $KUBECONFIG still points at the ambient cluster, and an e2e must never touch
# a non-test cluster.
K3D_CREATED=0

# ---------------------------------------------------------------
# Gateway-specific failure diagnostics (per the task spec).
# ---------------------------------------------------------------
dump_gateway_diagnostics() {
    [ -n "${KUBECONFIG:-}" ] || return 0
    printf '\n----- gateway diagnostics (failure) -----\n' >&2
    printf '\n--- gatewayclass cilium (-o yaml) ---\n' >&2
    kubectl get gatewayclass cilium -o yaml >&2 2>&1 || true
    printf '\n--- gateway platform -n %s (-o yaml) ---\n' "$OPERATOR_NS" >&2
    kubectl -n "$OPERATOR_NS" get gateway platform -o yaml >&2 2>&1 || true
    printf '\n--- gateway-api CRDs ---\n' >&2
    kubectl get crd -o name 2>/dev/null | grep -i 'gateway.networking.k8s.io' >&2 || true
    printf '\n--- cilium-operator log (gateway|tlsroute) ---\n' >&2
    kubectl -n kube-system logs deploy/cilium-operator --all-containers --tail=400 2>/dev/null \
        | grep -iE 'gateway|tlsroute|gatewayclass' | tail -40 >&2 || true
    printf '%s\n' '----- end gateway diagnostics -----' >&2
}

cleanup() {
    local exit_code=$?

    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! gateway-walk FAILED at %s (exit %d) !!!\n' \
            "$(elapsed)" "$exit_code" >&2
        if [ "$K3D_CREATED" -eq 1 ]; then
            dump_diagnostics
            dump_gateway_diagnostics
            printf 'Tearing down cluster (set APPRAFTER_E2E_SKIP_DESTROY=1 to keep).\n' >&2
        else
            printf 'cluster %s was never created (kind/podman unavailable in this shell?) — skipping diagnostics + teardown; your ambient KUBECONFIG was NOT touched.\n' \
                "$CLUSTER_NAME" >&2
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
# Local helper: wait_jsonpath <kind> <ns|-> <name> <jsonpath> <want> [timeout]
#   ns="-" → cluster-scoped (no -n). Polls until the jsonpath equals <want>.
# ---------------------------------------------------------------
wait_jsonpath() {
    local kind="$1" ns="$2" name="$3" jsonpath="$4" want="$5"
    local timeout="${6:-180}"
    local deadline got nsargs=()
    [ "$ns" != "-" ] && nsargs=(-n "$ns")
    deadline=$(( $(date +%s) + timeout ))

    printf '  wait %s/%s [%s] == %q (timeout %ss) ...\n' \
        "$kind" "$name" "$jsonpath" "$want" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        got=$(kubectl "${nsargs[@]}" get "$kind" "$name" \
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
    return 1
}

# ---------------------------------------------------------------
# Local helper: _yq <args...> — yq-go with a nixpkgs fallback (mirrors
# needs-networkpolicy-walk.sh). Used to select Kinds out of `helm template`.
# ---------------------------------------------------------------
_yq() { if command -v yq >/dev/null 2>&1; then yq "$@"; else nix run nixpkgs#yq-go -- "$@"; fi; }

# ---------------------------------------------------------------
# Local helper: assert_not_found <kind> <ns|-> <name> [settle_secs]
#   Negative assertion for Phases 8/9 — assert a resource STAYS absent.
#   wait_jsonpath waits for a value to APPEAR, which is the wrong shape here,
#   so this polls: every 5s for <settle_secs> the resource must be NotFound;
#   the moment it is ever observed present the assertion FAILS. Use for "no
#   route was created" (a fixed settle window proves the operator had its
#   reconcile chance and emitted nothing).
# ---------------------------------------------------------------
assert_not_found() {
    local kind="$1" ns="$2" name="$3" settle="${4:-45}"
    local deadline nsargs=()
    [ "$ns" != "-" ] && nsargs=(-n "$ns")
    deadline=$(( $(date +%s) + settle ))
    printf '  assert %s/%s STAYS NotFound for %ss ...\n' "$kind" "$name" "$settle"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if kubectl "${nsargs[@]}" get "$kind" "$name" >/dev/null 2>&1; then
            printf 'ERROR: %s/%s exists but should be NotFound\n' "$kind" "$name" >&2
            kubectl "${nsargs[@]}" get "$kind" "$name" -o yaml >&2 2>&1 || true
            return 1
        fi
        sleep 5
    done
    printf '  ok: %s/%s NotFound throughout the %ss settle window\n' "$kind" "$name" "$settle"
    return 0
}

# ---------------------------------------------------------------
# Local helper: wait_not_found <kind> <ns|-> <name> [timeout]
#   Negative assertion for Phase 9 — assert a resource that exists now
#   DISAPPEARS within <timeout> (the prune path). Polls until the GET 404s.
# ---------------------------------------------------------------
wait_not_found() {
    local kind="$1" ns="$2" name="$3" timeout="${4:-90}"
    local deadline nsargs=()
    [ "$ns" != "-" ] && nsargs=(-n "$ns")
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait %s/%s to become NotFound (timeout %ss) ...\n' "$kind" "$name" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if ! kubectl "${nsargs[@]}" get "$kind" "$name" >/dev/null 2>&1; then
            printf '  ok: %s/%s is NotFound (pruned)\n' "$kind" "$name"
            return 0
        fi
        printf '    %s: %s/%s still present, waiting ...\n' "$(date +%H:%M:%S)" "$kind" "$name"
        sleep 5
    done
    printf 'ERROR: %s/%s never became NotFound within %ss\n' "$kind" "$name" "$timeout" >&2
    kubectl "${nsargs[@]}" get "$kind" "$name" -o yaml >&2 2>&1 || true
    return 1
}

# ---------------------------------------------------------------
# Local helper: gateway_node_ip
#   The host-network Gateway binds 80/443 on the kind NODE's IP (the cilium-envoy
#   DaemonSet runs with hostNetwork, so the listeners live on the node, not on a
#   ClusterIP/LB). The curl probes `--resolve app.<domain>:443:<this-ip>`. We read
#   the kind node's InternalIP (the node container's address on the cluster's
#   docker/podman network), which is reachable from the host that runs the walk
#   (the kind node container is attached to a host-visible bridge). Echoes the IP.
#   NOTE (controller): this is a GUESS at the right reachable address — verify
#   live. detect_host_gateway_ip() is the HOST's address as seen FROM a pod (the
#   wrong direction here: we curl FROM the host INTO the node), so it is not
#   reused. If the node InternalIP is not host-reachable under this runtime,
#   fall back to `127.0.0.1` with a kind extraPortMapping (not configured here).
# ---------------------------------------------------------------
gateway_node_ip() {
    kubectl get node -o jsonpath='{.items[0].status.addresses[?(@.type=="InternalIP")].address}' 2>/dev/null \
        | awk '{print $1; exit}'
}

# ===============================================================
# Phase 0: bring up the kind+Cilium cluster (default CNI + kube-proxy off)
# ===============================================================

phase "Phase 0: kind_up_cilium ${CLUSTER_NAME} (default CNI + kube-proxy disabled)"

kind_up_cilium "$CLUSTER_NAME"

cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
# The cluster exists and $KUBECONFIG points at it — cleanup may now safely
# diagnose/tear it down (and only it).
K3D_CREATED=1
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

# ===============================================================
# Phase 1: install Gateway API CRDs (EXPERIMENTAL channel) + assert TLSRoute
# ===============================================================
#
# The CRDs MUST exist before Cilium reconciles its gateway controller (the
# component syncs at -25, strictly before cilium at -20). We install them
# first, exactly as the chart's gateway-api-crds component does — the
# experimental channel artifact is the equivalent of `config/crd/experimental`.
# In NEGATIVE mode install the STANDARD channel instead (which LACKS TLSRoute)
# to reproduce the F1 symptom.
# ===============================================================

if [ "$NEGATIVE" = 1 ]; then
    phase "Phase 1: install Gateway API STANDARD CRDs ${GATEWAY_API_VERSION} (NEGATIVE — F1 repro)"
    printf '  applying %s ...\n' "$GW_API_STANDARD_URL"
    retry 5 5 -- kubectl apply -f "$GW_API_STANDARD_URL"
    # The standard channel must NOT carry tlsroutes — that absence IS the F1
    # condition. Assert it is absent so the negative run is meaningful.
    if kubectl get crd "$TLSROUTE_CRD" >/dev/null 2>&1; then
        printf 'ERROR: NEGATIVE mode but %s exists (standard channel should lack it)\n' "$TLSROUTE_CRD" >&2
        exit 1
    fi
    printf '  ok: standard channel has NO %s (the F1 condition)\n' "$TLSROUTE_CRD"
else
    phase "Phase 1: install Gateway API EXPERIMENTAL CRDs ${GATEWAY_API_VERSION} + assert TLSRoute"
    printf '  applying %s ...\n' "$GW_API_EXPERIMENTAL_URL"
    # The experimental install manifest is large; --server-side avoids the
    # last-applied-annotation size limit some CRDs blow.
    retry 5 5 -- kubectl apply --server-side -f "$GW_API_EXPERIMENTAL_URL"
    # F1 MARKER: tlsroutes must be served + Established. With the standard
    # channel this CRD is absent and Cilium's controller never Accepts.
    wait_jsonpath crd - "$TLSROUTE_CRD" \
        '{.status.conditions[?(@.type=="Established")].status}' True 120
    printf '  ok: %s is Established (the F1 marker — present only in experimental)\n' "$TLSROUTE_CRD"
fi

# ===============================================================
# Phase 2: install Cilium 1.16.5 with the chart's host-network gateway values
# ===============================================================
#
# Get the values straight from the chart source so the walk validates the
# SAME config the platform ships: `_components.cilium.values` carries
# gatewayAPI.{enabled,hostNetwork.enabled}, the envoy NET_BIND_SERVICE
# capabilities, the cilium-config-rev annotation, kube-proxy replacement +
# k8sServiceHost/Port (matching the kind_up_cilium apiserver pin), IPAM, etc.
# ===============================================================

phase "Phase 2: install Cilium ${CILIUM_VERSION} with the chart's host-network gateway values"

CILIUM_VALUES="${TMPDIR_WORK}/cilium-values.yaml"
( cd "$REPO_STACK_DIR" && _cue export ./cue -e '_components.cilium.values' --out yaml ) >"$CILIUM_VALUES"
printf '  rendered chart cilium values -> %s\n' "$CILIUM_VALUES"
printf '  --- cilium values (gateway-relevant) ---\n'
grep -iE 'gatewayAPI|hostNetwork|enabled|NET_BIND_SERVICE|keepCapNetBindService|kubeProxyReplacement|k8sService' "$CILIUM_VALUES" | sed 's/^/    /'

helm repo add cilium "$CILIUM_REPO" >/dev/null 2>&1 || true
helm repo update cilium >/dev/null 2>&1 || helm repo update >/dev/null 2>&1 || true

# Install Cilium with the chart values. kind_up_cilium already pinned the
# apiserver to 127.0.0.1:6443 inside the node, matching the chart's
# k8sServiceHost/k8sServicePort, so kube-proxy replacement converges.
helm install cilium cilium/cilium \
    --version "$CILIUM_VERSION" \
    --namespace kube-system \
    -f "$CILIUM_VALUES" \
    --wait --timeout 8m

printf '  waiting for cilium-operator + cilium-agent + cilium-envoy Ready ...\n'
# cilium-operator runs the gateway controller; the agent owns the datapath;
# the STANDALONE cilium-envoy DaemonSet binds the host 80/443 listeners.
kubectl -n kube-system rollout status deploy/cilium-operator --timeout=300s
kubectl -n kube-system rollout status ds/cilium --timeout=300s
# The standalone envoy DaemonSet (envoy.enabled: true). Name is `cilium-envoy`.
if kubectl -n kube-system get ds cilium-envoy >/dev/null 2>&1; then
    kubectl -n kube-system rollout status ds/cilium-envoy --timeout=300s
    printf '  ok: cilium-envoy DaemonSet Ready (standalone host-network proxy)\n'
else
    printf '  WARN: no cilium-envoy DaemonSet found (embedded-envoy mode?)\n' >&2
fi

# Node must flip Ready now that Cilium owns the CNI.
kubectl wait --for=condition=Ready node --all --timeout=180s
printf '  node Ready (Cilium CNI converged)\n'

# ===============================================================
# Phase 3: the platform Gateway prerequisites — namespace + self-signed TLS Secret
# ===============================================================

phase "Phase 3: apprafter-system ns + self-signed kubernetes.io/tls Secret (SAN apex+wildcard)"

kubectl create namespace "$OPERATOR_NS" 2>/dev/null || true

# Self-signed cert covering the apex + wildcard (the two HTTPS listeners). The
# Gateway's certificateRefs point at this Secret in apprafter-system.
TLS_KEY="${TMPDIR_WORK}/tls.key"
TLS_CRT="${TMPDIR_WORK}/tls.crt"
_openssl() { if command -v openssl >/dev/null 2>&1; then openssl "$@"; else nix run nixpkgs#openssl -- "$@"; fi; }
_openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$TLS_KEY" -out "$TLS_CRT" -days 3 \
    -subj "/CN=${SAMPLE_DOMAIN}" \
    -addext "subjectAltName=DNS:${SAMPLE_DOMAIN},DNS:*.${SAMPLE_DOMAIN}" >/dev/null 2>&1
kubectl -n "$OPERATOR_NS" create secret tls "$TLS_SECRET" \
    --cert="$TLS_CRT" --key="$TLS_KEY" \
    --dry-run=client -o yaml | kubectl apply -f -
printf '  ok: Secret %s/%s created (SAN %s + *.%s)\n' \
    "$OPERATOR_NS" "$TLS_SECRET" "$SAMPLE_DOMAIN" "$SAMPLE_DOMAIN"

# ===============================================================
# Phase 4: render the chart's Gateway + HTTPRoute and apply them
# ===============================================================
#
# Render the SAME gateway.yaml the platform ships (templates/gateway.yaml in
# the dist chart at currentVersion) with a sample allowedDomains entry. The
# guard `{{- if .Values.gateway.allowedDomains }}` emits the Gateway +
# HTTPRoute only when non-empty, so we pass one entry referencing our TLS
# Secret. `--show-only` extracts exactly those two documents.
# ===============================================================

phase "Phase 4: render + apply the chart Gateway 'platform' + HTTPRoute (allowedDomains=[${SAMPLE_DOMAIN}])"

CHART_VER="$( cd "$REPO_STACK_DIR" && _cue export ./cue -e 'currentVersion' --out text )"
DIST_CHART="${REPO_STACK_DIR}/dist/platform-stack-${CHART_VER}"
printf '  chart currentVersion=%s -> %s\n' "$CHART_VER" "$DIST_CHART"
if [ ! -d "$DIST_CHART" ]; then
    # shellcheck disable=SC2016  # literal backticks in the user-facing message
    printf '  dist chart absent; running `make -C %s render-only` ...\n' "$REPO_STACK_DIR"
    make -C "$REPO_STACK_DIR" render-only
fi
[ -d "$DIST_CHART" ] || { printf 'ERROR: dist chart %s not found even after render\n' "$DIST_CHART" >&2; exit 1; }

GATEWAY_MANIFEST="${TMPDIR_WORK}/gateway.yaml"
helm template platform "$DIST_CHART" \
    --namespace "$OPERATOR_NS" \
    --set-json "gateway.allowedDomains=[{\"domain\":\"${SAMPLE_DOMAIN}\",\"certMode\":\"imported\",\"importedCertRef\":\"${TLS_SECRET}\",\"addedAt\":\"2026-06-12\",\"addedBy\":\"gateway-walk\"}]" \
    --show-only templates/gateway.yaml >"$GATEWAY_MANIFEST"

printf '  --- rendered Gateway + HTTPRoute ---\n'
sed 's/^/    /' "$GATEWAY_MANIFEST"

# Sanity: the render must carry a Gateway named `platform` on gatewayClassName
# cilium (else the chart guard / values shape changed under us).
grep -q 'kind: Gateway' "$GATEWAY_MANIFEST" || { printf 'ERROR: rendered manifest has no Gateway (allowedDomains guard?)\n' >&2; exit 1; }
grep -q 'gatewayClassName: cilium' "$GATEWAY_MANIFEST" || { printf 'ERROR: rendered Gateway is not gatewayClassName cilium\n' >&2; exit 1; }

kubectl apply -f "$GATEWAY_MANIFEST"
printf '  Gateway + HTTPRoute applied\n'

phase "Phase 5: the core gate — Gateway platform Programmed (F1 acceptance)"

# ===============================================================
# Phase 5: THE CORE GATE — Gateway platform reaches Programmed (or NOT, negative)
# ===============================================================
#
# The LOAD-BEARING signal is the GATEWAY's Programmed condition, NOT the
# GatewayClass's Accepted condition. Cilium 1.16.5's gateway controller only
# reconciles a Gateway after passing its startup required-resources check (which
# needs `tlsroutes` from the experimental channel — the F1 dependency); a
# Programmed=True Gateway therefore PROVES the check passed. The GatewayClass
# `Accepted` status is read for context but NOT gated: Cilium 1.16.5 leaves it
# at the CRD default ("Waiting for controller", Unknown, 1970 timestamp) even on
# a fully-working install where the Gateway IS Programmed (confirmed live
# 2026-06-12) — so asserting GatewayClass Accepted=True would false-fail a
# healthy gateway.
# ===============================================================

# Informational: read the GatewayClass status (do NOT gate on it).
gc_accepted=$(kubectl get gatewayclass cilium \
    -o jsonpath='{.status.conditions[?(@.type=="Accepted")].status}' 2>/dev/null || true)
printf '  (info) GatewayClass cilium Accepted=%q (Cilium 1.16.5 may leave this Unknown — not gated)\n' \
    "${gc_accepted:-<empty>}"

if [ "$NEGATIVE" = 1 ]; then
    # NEGATIVE: with the STANDARD channel (no tlsroutes) Cilium's gateway
    # controller fails its required-resources check and never reconciles the
    # Gateway, so `platform` must NOT reach Programmed=True within a generous
    # window. Poll for it to STAY not-Programmed; if it ever flips True the
    # guard would be meaningless (the standard channel would suffice → no F1).
    phase "Phase 5 (NEGATIVE): Gateway platform must NOT reach Programmed (F1 symptom)"
    deadline=$(( $(date +%s) + 180 ))
    programmed=""
    while [ "$(date +%s)" -lt "$deadline" ]; do
        programmed=$(kubectl -n "$OPERATOR_NS" get gateway platform \
            -o jsonpath='{.status.conditions[?(@.type=="Programmed")].status}' 2>/dev/null || true)
        if [ "$programmed" = "True" ]; then
            printf 'ERROR: NEGATIVE mode but Gateway platform Programmed=True with the standard channel — the F1 guard does NOT hold!\n' >&2
            kubectl -n "$OPERATOR_NS" get gateway platform -o yaml >&2 2>&1 || true
            exit 1
        fi
        printf '    %s: Gateway Programmed=%q (want NOT True)\n' "$(date +%H:%M:%S)" "${programmed:-<empty>}"
        sleep 10
    done
    printf '  ok: Gateway platform did NOT reach Programmed with the standard channel (Programmed=%q) — F1 symptom reproduced, guard holds\n' "${programmed:-<empty>}"
    printf '\n--- cilium-operator log (the F1 cause: missing tlsroutes) ---\n'
    kubectl -n kube-system logs deploy/cilium-operator --all-containers --tail=400 2>/dev/null \
        | grep -iE 'tlsroute|gateway|required|crd' | tail -20 | sed 's/^/    /' || true
else
    # POSITIVE: THE F1 ACCEPTANCE — the Gateway reaches Programmed=True. With the
    # experimental CRDs the controller passes its tlsroutes check, reconciles the
    # Gateway, and programs the Envoy 80/443 listeners. kubectl wait gives a clean
    # signal; a jsonpath fallback assert covers a status-write race.
    phase "Phase 5: assert Gateway platform reaches Programmed=True (F1 acceptance)"
    printf '  waiting for Gateway platform to reach Programmed=True ...\n'
    if ! kubectl -n "$OPERATOR_NS" wait --for=condition=Programmed \
        gateway/platform --timeout=240s; then
        printf '  kubectl wait timed out; final jsonpath check ...\n' >&2
        wait_jsonpath gateway "$OPERATOR_NS" platform \
            '{.status.conditions[?(@.type=="Programmed")].status}' True 60
    fi
    programmed=$(kubectl -n "$OPERATOR_NS" get gateway platform \
        -o jsonpath='{.status.conditions[?(@.type=="Programmed")].status}' 2>/dev/null || true)
    assert_eq "Gateway platform Programmed" "$programmed" "True"

    # The Gateway also reports Accepted=True (the controller scheduled it).
    gw_accepted=$(kubectl -n "$OPERATOR_NS" get gateway platform \
        -o jsonpath='{.status.conditions[?(@.type=="Accepted")].status}' 2>/dev/null || true)
    assert_eq "Gateway platform Accepted" "$gw_accepted" "True"

    # Every listener (the two 443 HTTPS + the 80 HTTP) must be Programmed — this
    # is what proves the Envoy host-network 80/443 bind config was materialised.
    printf '  --- Gateway listener statuses ---\n'
    kubectl -n "$OPERATOR_NS" get gateway platform \
        -o jsonpath='{range .status.listeners[*]}listener={.name} programmed={.conditions[?(@.type=="Programmed")].status}{"\n"}{end}' \
        2>/dev/null | sed 's/^/    /' || true
    n_listeners=$(kubectl -n "$OPERATOR_NS" get gateway platform \
        -o jsonpath='{range .status.listeners[*]}{.name}{"\n"}{end}' 2>/dev/null | grep -c . || true)
    not_programmed=$(kubectl -n "$OPERATOR_NS" get gateway platform \
        -o jsonpath='{range .status.listeners[*]}{.conditions[?(@.type=="Programmed")].status}{"\n"}{end}' 2>/dev/null \
        | grep -cvi '^True$' || true)
    if [ "${n_listeners:-0}" -lt 3 ]; then
        printf 'ERROR: expected 3 Gateway listeners (apex+wildcard 443, http 80), saw %s\n' "${n_listeners:-0}" >&2
        exit 1
    fi
    if [ "${not_programmed:-0}" -gt 0 ]; then
        printf 'ERROR: %s of %s listener(s) NOT Programmed\n' "$not_programmed" "$n_listeners" >&2
        exit 1
    fi
    printf '  ok: all %s listeners Programmed (Envoy host-network 80/443 config materialised)\n' "$n_listeners"

    # The standalone cilium-envoy pods own the host 80/443 bind — assert at
    # least one is Running (the Gateway being Programmed already implies Cilium
    # materialised the listener config into Envoy; this is corroboration).
    envoy_running=$(kubectl -n kube-system get pods -l k8s-app=cilium-envoy \
        --no-headers 2>/dev/null | grep -c 'Running' || true)
    assert_eq "at least one cilium-envoy pod Running" "$([ "${envoy_running:-0}" -ge 1 ] && echo yes || echo no)" "yes"
    printf '  ok: %s cilium-envoy pod(s) Running (host-network 80/443 proxy)\n' "${envoy_running:-0}"
fi

# ===============================================================
# Phases 6-9: operator-rendered public ingress (1.83b)
# ===============================================================
#
# POSITIVE PATH ONLY. The negative path proved the Gateway never reaches
# Programmed (the F1 symptom), so there is no working platform Gateway for an
# HTTPRoute to attach to — these phases would be meaningless there. Skip them.
#
# Phase 6 deploys the WORKING-TREE operator + admission-webhook (1.83b is
# UNRELEASED, so it cannot use the published image — mirrors
# needs-networkpolicy-walk.sh's Phase 1b: build the images from the working
# tree + kind-load them via cluster_load_image). Unlike the needs/* walks this
# walk does NOT run `apprafter cluster-bootstrap` (it installs Cilium directly),
# so the operator chart's CRDs/RBAC/Deployment are applied straight from the
# chart here, and cert-manager (a hard dependency of the webhook chart's
# Certificate + cert-manager.io/inject-ca-from CA injection) is installed
# standalone first so the `failurePolicy: Fail` ValidatingWebhookConfiguration
# does not block the Application applies in Phases 7-9.
if [ "$NEGATIVE" != 1 ]; then

# ===============================================================
# Phase 6: deploy the working-tree operator + Application CRD + webhook
# ===============================================================

phase "Phase 6: deploy working-tree operator + CRDs + admission-webhook (1.83b unreleased)"

builder=podman; command -v podman >/dev/null 2>&1 || builder=docker
printf '  container builder: %s\n' "$builder"

# (a) CRDs first — the operator needs application + platformstack served before
# it reconciles, and the webhook's ValidatingWebhookConfiguration references the
# Application resource. Render the operator chart's CRDs + RBAC + ServiceAccount
# + Deployment with `--namespace "$OPERATOR_NS"` (LOAD-BEARING: the
# ClusterRoleBinding subject is templated as `namespace: {{ .Release.Namespace
# }}`; without -n Helm defaults Release.Namespace to `default`, so the binding
# points at the wrong SA and the operator 403s on every reconcile —
# needs-networkpolicy-walk.sh learned this the hard way, 2026-06-09).
kubectl create namespace "$OPERATOR_NS" 2>/dev/null || true

printf '  applying operator CRDs (server-side) ...\n'
helm template apprafter-operator "$OPERATOR_CHART" \
    --namespace "$OPERATOR_NS" \
    | _yq 'select(.kind == "CustomResourceDefinition")' \
    | kubectl apply --server-side --force-conflicts -f -
for _crd in applications platformstacks; do
    retry 12 5 -- kubectl wait --for=condition=Established \
        "crd/${_crd}.apprafter.io" --timeout=30s
done
printf '  ok: applications + platformstacks CRDs Established\n'

# (b) operator RBAC + ServiceAccount + Deployment from the chart.
printf '  applying operator ServiceAccount + RBAC + Deployment ...\n'
helm template apprafter-operator "$OPERATOR_CHART" \
    --namespace "$OPERATOR_NS" \
    | _yq 'select(.kind == "ServiceAccount" or .kind == "ClusterRole" or .kind == "ClusterRoleBinding" or .kind == "Role" or .kind == "RoleBinding" or .kind == "Service" or .kind == "Deployment")' \
    | kubectl apply --server-side --force-conflicts -f -

# (c) build + side-load the working-tree operator image, tagged as the image
# ref the just-applied Deployment references (so IfNotPresent serves it).
OP_IMG=$(kubectl -n "$OPERATOR_NS" get deploy apprafter-operator \
    -o jsonpath='{.spec.template.spec.containers[0].image}')
printf '  building operator image %s from the working tree ...\n' "$OP_IMG"
"$builder" build -f "${REPO_ROOT}/operator/apprafter-operator/Dockerfile" \
    -t "$OP_IMG" "${REPO_ROOT}/operator"
cluster_load_image "$CLUSTER_NAME" "$OP_IMG"
kubectl -n "$OPERATOR_NS" rollout restart deploy/apprafter-operator
kubectl -n "$OPERATOR_NS" rollout status deploy/apprafter-operator --timeout=240s
printf '  ok: apprafter-operator Available (working-tree build)\n'

# (d) cert-manager — a hard dependency of the webhook chart (the Certificate
# CR + the cert-manager.io/inject-ca-from CA injection on the
# ValidatingWebhookConfiguration). cluster-bootstrap would install this; this
# walk skips bootstrap, so install it standalone (pinned to the platform
# version) before the webhook chart.
phase "Phase 6b: cert-manager ${CERT_MANAGER_VERSION} (webhook CA-injection dependency)"
printf '  applying %s ...\n' "$CERT_MANAGER_URL"
retry 5 10 -- kubectl apply -f "$CERT_MANAGER_URL"
kubectl -n cert-manager rollout status deploy/cert-manager --timeout=300s
kubectl -n cert-manager rollout status deploy/cert-manager-webhook --timeout=300s
kubectl -n cert-manager rollout status deploy/cert-manager-cainjector --timeout=300s
printf '  ok: cert-manager rollout complete\n'

# (e) admission-webhook chart (Certificate + ClusterIssuer + Service +
# Deployment + ValidatingWebhookConfiguration). The chart's ClusterIssuer
# `apprafter-selfsigned` signs the webhook cert; cainjector wires the caBundle.
phase "Phase 6c: deploy working-tree admission-webhook"
printf '  applying admission-webhook chart objects ...\n'
# cert-manager's OWN validating webhook intercepts Certificate/ClusterIssuer
# applies; it can briefly 503 right after rollout, so retry the apply.
WEBHOOK_RENDER="${TMPDIR_WORK}/webhook.yaml"
helm template apprafter-admission-webhook "$WEBHOOK_CHART" \
    --namespace "$OPERATOR_NS" >"$WEBHOOK_RENDER"
retry 8 8 -- kubectl apply --server-side --force-conflicts -f "$WEBHOOK_RENDER"
WH_IMG=$(kubectl -n "$OPERATOR_NS" get deploy apprafter-admission-webhook \
    -o jsonpath='{.spec.template.spec.containers[0].image}')
printf '  building webhook image %s from the working tree ...\n' "$WH_IMG"
"$builder" build -f "${REPO_ROOT}/operator/admission-webhook/Dockerfile" \
    -t "$WH_IMG" "${REPO_ROOT}/operator"
cluster_load_image "$CLUSTER_NAME" "$WH_IMG"
kubectl -n "$OPERATOR_NS" rollout restart deploy/apprafter-admission-webhook
kubectl -n "$OPERATOR_NS" rollout status deploy/apprafter-admission-webhook --timeout=240s
printf '  ok: admission-webhook Available (working-tree build)\n'

# (f) seed the singleton PlatformStack/default in apprafter-system so the
# operator's PublicRouteReady zone-coverage check finds the $SAMPLE_DOMAIN zone
# (read_allowed_domains reads spec.values.gateway.allowedDomains[].domain).
# spec requires channel + autoUpgrade + source + values{tier}; gateway lives
# under values (x-kubernetes-preserve-unknown-fields).
phase "Phase 6d: seed PlatformStack/default (gateway.allowedDomains=[${SAMPLE_DOMAIN}])"
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: PlatformStack
metadata:
  name: default
  namespace: ${OPERATOR_NS}
spec:
  channel: stable
  autoUpgrade: false
  source:
    upstream: oci://ghcr.io/apprafter/platform-stack
    repoURL: oci://ghcr.io/apprafter/platform-stack
    checkInterval: 1h
  values:
    tier: 1
    gateway:
      allowedDomains:
        - domain: ${SAMPLE_DOMAIN}
          certMode: imported
          importedCertRef: ${TLS_SECRET}
          addedAt: "2026-06-12"
          addedBy: gateway-walk
YAML
wait_jsonpath platformstack "$OPERATOR_NS" default \
    '{.spec.values.gateway.allowedDomains[0].domain}' "$SAMPLE_DOMAIN" 60
printf '  ok: PlatformStack/default seeded with the %s zone\n' "$SAMPLE_DOMAIN"

# ===============================================================
# Phase 7: public app acceptance — operator renders an HTTPRoute on :443
# ===============================================================

phase "Phase 7: public Application '${APP_PUBLIC}' -> HTTPRoute on :443 + curl 200"

kubectl create namespace "$APP_NS" 2>/dev/null || true

kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP_PUBLIC}
  namespace: ${APP_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  base:
    image: ${WHOAMI_IMAGE}
    replicas: 1
    expose:
      port: 80
      network: public
      hostname: ${APP_HOSTNAME}
YAML

# The operator renders httproute/web within ~90s.
wait_jsonpath httproute "$APP_NS" "$APP_PUBLIC" \
    '{.metadata.name}' "$APP_PUBLIC" 120
printf '  ok: operator rendered httproute/%s\n' "$APP_PUBLIC"

# parentRefs[0].port == 443 (attaches to the HTTPS listeners ONLY).
route_port=$(kubectl -n "$APP_NS" get httproute "$APP_PUBLIC" \
    -o jsonpath='{.spec.parentRefs[0].port}' 2>/dev/null || true)
assert_eq "httproute/${APP_PUBLIC} parentRefs[0].port" "$route_port" "443"

# The route reaches Accepted=True AND ResolvedRefs=True (status.parents[*]).
# Modelled on Phase 5's Gateway condition jsonpath, over the HTTPRoute's
# per-parent conditions (the operator's route_accepted_resolved reads exactly
# these).
wait_jsonpath httproute "$APP_NS" "$APP_PUBLIC" \
    '{.status.parents[0].conditions[?(@.type=="Accepted")].status}' True 120
wait_jsonpath httproute "$APP_NS" "$APP_PUBLIC" \
    '{.status.parents[0].conditions[?(@.type=="ResolvedRefs")].status}' True 60
printf '  ok: httproute/%s Accepted=True + ResolvedRefs=True\n' "$APP_PUBLIC"

# Application/web PublicRouteReady=True + endpointURL https://app.<domain>/.
wait_jsonpath application.apprafter.io "$APP_NS" "$APP_PUBLIC" \
    '{.status.conditions[?(@.type=="PublicRouteReady")].status}' True 120
ep_url=$(kubectl -n "$APP_NS" get application.apprafter.io "$APP_PUBLIC" \
    -o jsonpath='{.status.endpointURL}' 2>/dev/null || true)
assert_eq "Application/${APP_PUBLIC} endpointURL" "$ep_url" "https://${APP_HOSTNAME}/"

# The whoami Deployment is Available, then curl the public endpoint via the
# host-network Gateway on the node IP.
kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP_PUBLIC}" --timeout=240s
printf '  ok: %s Deployment Available\n' "$APP_PUBLIC"

NODE_IP="$(gateway_node_ip)"
[ -n "$NODE_IP" ] || { printf 'ERROR: could not resolve the kind node InternalIP for the curl probe\n' >&2; exit 1; }
printf '  node IP for curl --resolve: %s\n' "$NODE_IP"

_curl() { if command -v curl >/dev/null 2>&1; then curl "$@"; else nix run nixpkgs#curl -- "$@"; fi; }

# HTTPS :443 — expect 200 with a whoami body (contains `Hostname:`). Retry a
# window: the Envoy listener + route programming lags the status flip.
printf '  curl https://%s/ (via %s:443) ...\n' "$APP_HOSTNAME" "$NODE_IP"
curl_ok=0
for _i in $(seq 1 18); do
    body=$(_curl -sk --max-time 10 \
        --resolve "${APP_HOSTNAME}:443:${NODE_IP}" \
        "https://${APP_HOSTNAME}/" 2>/dev/null || true)
    code=$(_curl -sk -o /dev/null -w '%{http_code}' --max-time 10 \
        --resolve "${APP_HOSTNAME}:443:${NODE_IP}" \
        "https://${APP_HOSTNAME}/" 2>/dev/null || true)
    if [ "$code" = "200" ] && printf '%s' "$body" | grep -q 'Hostname:'; then
        curl_ok=1
        printf '  ok: HTTPS 200 with a whoami body (try %d)\n' "$_i"
        break
    fi
    printf '    %s: code=%q hostname-marker=%s (try %d), retrying ...\n' \
        "$(date +%H:%M:%S)" "$code" "$(printf '%s' "$body" | grep -qc 'Hostname:' 2>/dev/null && echo yes || echo no)" "$_i"
    sleep 5
done
if [ "$curl_ok" -ne 1 ]; then
    # shellcheck disable=SC2016  # literal backticks in the user-facing message
    printf 'ERROR: curl https://%s/ never returned 200 with a whoami `Hostname:` body\n' "$APP_HOSTNAME" >&2
    exit 1
fi

# HTTP :80 — 1.83a's catch-all redirect is untouched (the app route attaches to
# :443 only), so a plain http request to the same host still 301s.
printf '  curl -I http://%s/ (via %s:80) — expect 301 ...\n' "$APP_HOSTNAME" "$NODE_IP"
redirect_ok=0
for _i in $(seq 1 12); do
    rcode=$(_curl -sI -o /dev/null -w '%{http_code}' --max-time 10 \
        --resolve "${APP_HOSTNAME}:80:${NODE_IP}" \
        "http://${APP_HOSTNAME}/" 2>/dev/null || true)
    if [ "$rcode" = "301" ]; then
        redirect_ok=1
        printf '  ok: http://%s/ -> 301 (1.83a catch-all redirect intact)\n' "$APP_HOSTNAME"
        break
    fi
    printf '    %s: http code=%q (want 301, try %d), retrying ...\n' "$(date +%H:%M:%S)" "$rcode" "$_i"
    sleep 5
done
[ "$redirect_ok" -eq 1 ] || { printf 'ERROR: http://%s/ did not return 301 (the :80 catch-all redirect regressed)\n' "$APP_HOSTNAME" >&2; exit 1; }

# ===============================================================
# Phase 8: internal app emits NO route
# ===============================================================

phase "Phase 8: internal Application '${APP_INTERNAL}' emits NO HTTPRoute"

kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP_INTERNAL}
  namespace: ${APP_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  base:
    image: ${WHOAMI_IMAGE}
    replicas: 1
    expose:
      port: 80
      network: internal
YAML

# Let the operator reconcile to Ready, then assert NO httproute exists for it
# (a public app got a route in ~30s, so a 60s settle is a generous proof the
# internal app emitted nothing).
wait_jsonpath application.apprafter.io "$APP_NS" "$APP_INTERNAL" \
    '{.status.phase}' Ready 180
assert_not_found httproute "$APP_NS" "$APP_INTERNAL" 60

# Its endpointURL is the internal cluster-DNS form (http://...svc.cluster.local),
# NOT an https:// public URL.
int_ep=$(kubectl -n "$APP_NS" get application.apprafter.io "$APP_INTERNAL" \
    -o jsonpath='{.status.endpointURL}' 2>/dev/null || true)
printf '  internal endpointURL=%q\n' "$int_ep"
case "$int_ep" in
    http://*.svc.cluster.local:*) printf '  ok: internal endpointURL is the cluster-DNS form\n' ;;
    https://*) printf 'ERROR: internal app has a public https:// endpointURL: %q\n' "$int_ep" >&2; exit 1 ;;
    *) printf 'ERROR: internal endpointURL is not the cluster-DNS form: %q\n' "$int_ep" >&2; exit 1 ;;
esac
# It must also carry NO PublicRouteReady condition (internal app).
int_prr=$(kubectl -n "$APP_NS" get application.apprafter.io "$APP_INTERNAL" \
    -o jsonpath='{.status.conditions[?(@.type=="PublicRouteReady")].status}' 2>/dev/null || true)
assert_eq "internal app has NO PublicRouteReady condition" "${int_prr:-<absent>}" "<absent>"

# ===============================================================
# Phase 9: flip public -> internal prunes the route
# ===============================================================

phase "Phase 9: flip ${APP_PUBLIC} public->internal — HTTPRoute pruned"

# Merge-patch: set expose.network=internal and DROP the hostname (a public
# hostname-less app would also lose its route, but flipping to internal is the
# user-facing op). A strategic/merge patch cannot delete a scalar map key, so
# set hostname to null explicitly.
kubectl -n "$APP_NS" patch application.apprafter.io "$APP_PUBLIC" --type=merge \
    -p '{"spec":{"base":{"expose":{"network":"internal","hostname":null}}}}'

# The operator prunes httproute/web within ~90s.
wait_not_found httproute "$APP_NS" "$APP_PUBLIC" 120

# The PublicRouteReady condition disappears from web's status (no longer public).
printf '  waiting for PublicRouteReady to disappear from %s status ...\n' "$APP_PUBLIC"
prr_gone=0
deadline=$(( $(date +%s) + 120 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    prr=$(kubectl -n "$APP_NS" get application.apprafter.io "$APP_PUBLIC" \
        -o jsonpath='{.status.conditions[?(@.type=="PublicRouteReady")].status}' 2>/dev/null || true)
    if [ -z "$prr" ]; then
        prr_gone=1
        printf '  ok: PublicRouteReady condition gone from %s\n' "$APP_PUBLIC"
        break
    fi
    printf '    %s: PublicRouteReady still present (=%q), waiting ...\n' "$(date +%H:%M:%S)" "$prr"
    sleep 5
done
[ "$prr_gone" -eq 1 ] || { printf 'ERROR: PublicRouteReady condition never disappeared from %s after the internal flip\n' "$APP_PUBLIC" >&2; exit 1; }

# And its endpointURL flipped to the internal cluster-DNS form.
flipped_ep=$(kubectl -n "$APP_NS" get application.apprafter.io "$APP_PUBLIC" \
    -o jsonpath='{.status.endpointURL}' 2>/dev/null || true)
printf '  post-flip endpointURL=%q\n' "$flipped_ep"
case "$flipped_ep" in
    http://*.svc.cluster.local:*) printf '  ok: %s endpointURL flipped to the internal cluster-DNS form\n' "$APP_PUBLIC" ;;
    *) printf '  WARN: %s endpointURL after the internal flip is %q (expected the cluster-DNS form)\n' "$APP_PUBLIC" "$flipped_ep" >&2 ;;
esac

fi  # end NEGATIVE != 1 (Phases 6-9)

# ===============================================================
# Done — tear down on success path
# ===============================================================

trap - EXIT

if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    k3d_down "$CLUSTER_NAME" || true
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' \
        "$CLUSTER_NAME"
fi

rm -rf "$TMPDIR_WORK"

if [ "$NEGATIVE" = 1 ]; then
    printf '\ngateway-walk (NEGATIVE) GREEN in %s\n' "$(elapsed)"
    printf 'Proven: with the STANDARD Gateway API channel (no tlsroutes) the cilium gateway controller never reconciles and the platform Gateway does NOT reach Programmed — the F1 symptom. The experimental-channel fix (0.2.27) is what makes the Gateway Program.\n'
else
    printf '\ngateway-walk GREEN in %s\n' "$(elapsed)"
    printf 'Proven (1.83a): EXPERIMENTAL Gateway API CRDs (tlsroutes Established) -> Cilium %s gateway controller passes its required-resources check + reconciles -> the chart host-network Gateway platform reaches Programmed=True with all 3 listeners (apex+wildcard 443, http 80) Programmed. The 1.83a F1 fix (0.2.27) validated under real Cilium.\n' "$CILIUM_VERSION"
    printf 'Proven (1.83b): the working-tree operator renders a per-Application HTTPRoute on :443 for a public app (Accepted + ResolvedRefs; PublicRouteReady=True; endpointURL https://%s/), the app serves HTTP 200 through the Gateway while http://:80 still 301-redirects; an internal app emits NO route (cluster-DNS endpointURL); flipping public->internal prunes the route + drops PublicRouteReady.\n' "$APP_HOSTNAME"
fi
