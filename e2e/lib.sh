# SPDX-License-Identifier: FSL-1.1-Apache-2.0
# shellcheck shell=bash
#
# AppRafter e2e shared harness library.
#
# Source this file from an e2e script:
#   # shellcheck source=e2e/lib.sh
#   source "$(dirname "$0")/lib.sh"
#
# The sourcing script owns `set -euo pipefail`. This file has no
# top-level side effects except initialising START_NS once (only if
# the caller has not already set it). Functions are defined only.

# Initialise the run timer if not already set by the caller.
START_NS="${START_NS:-$(date +%s%N)}"

# Resolve the repository root from this file's own location so that
# helper functions can reference project paths correctly.
_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${_LIB_DIR}/.." && pwd)"
export REPO_ROOT

# ---------------------------------------------------------------
# elapsed — print human "Xm Ys" since START_NS
# ---------------------------------------------------------------
elapsed() {
    local end now
    end=$(date +%s%N)
    now=$(( (end - START_NS) / 1000000000 ))
    printf '%dm %ds' $(( now / 60 )) $(( now % 60 ))
}

# ---------------------------------------------------------------
# phase "<msg>" — print a section banner with elapsed time
# ---------------------------------------------------------------
phase() {
    printf '\n=== %s (elapsed %s) ===\n' "$1" "$(elapsed)"
}

# ---------------------------------------------------------------
# require_env VAR [VAR ...] — exit 2 if any var is unset or empty
# ---------------------------------------------------------------
require_env() {
    local var missing=0
    for var in "$@"; do
        # indirect expansion: ${!var} expands the variable named by $var
        if [ -z "${!var:-}" ]; then
            printf 'ERROR: required env var %s is unset or empty\n' "$var" >&2
            missing=1
        fi
    done
    if [ "$missing" -ne 0 ]; then
        exit 2
    fi
}

# ---------------------------------------------------------------
# retry <attempts> <sleep_seconds> -- <cmd...>
#   Run <cmd>, retrying on non-zero exit up to <attempts> times,
#   sleeping <sleep_seconds> between each attempt.
#   Exits with the last non-zero exit code after exhausting attempts.
# ---------------------------------------------------------------
retry() {
    local attempts="$1"
    local sleep_secs="$2"
    shift 2
    # consume the optional '--' separator
    if [ "${1:-}" = '--' ]; then shift; fi
    local i rc=0
    for i in $(seq 1 "$attempts"); do
        rc=0
        "$@" || rc=$?
        if [ "$rc" -eq 0 ]; then
            return 0
        fi
        if [ "$i" -lt "$attempts" ]; then
            printf '  retry: attempt %d/%d failed (exit %d), sleeping %ds\n' \
                "$i" "$attempts" "$rc" "$sleep_secs" >&2
            sleep "$sleep_secs"
        fi
    done
    printf '  retry: all %d attempts failed (last exit %d)\n' \
        "$attempts" "$rc" >&2
    return "$rc"
}

# ---------------------------------------------------------------
# k3d_up <cluster-name>
#   Create a local k3d cluster matching the `just e2e-up` target.
#   Traefik and servicelb are disabled because Cilium replaces them.
#   Falls back to `nix run nixpkgs#k3d` when k3d is not on PATH.
# ---------------------------------------------------------------
k3d_up() {
    local cluster_name="$1"
    local k3d_bin
    if command -v k3d >/dev/null 2>&1; then
        k3d_bin="k3d"
    else
        k3d_bin="nix run nixpkgs#k3d --"
    fi
    # Default CNI (flannel + kube-proxy), NOT Cilium. Cilium's eBPF
    # datapath converges pathologically slowly on k3d-in-CI (~10 min,
    # independent of single/dual-stack), so the e2e runs cluster-bootstrap
    # with APPRAFTER_BOOTSTRAP_SKIP_CILIUM=1 (see bootstrap_with_retry):
    # the GitOps + migration logic the e2e actually exercises needs only
    # a working CNI, and k3d's default flannel + kube-proxy is fast and
    # reliable. Cilium itself (kube-proxy replacement, dual-stack, L2) is
    # validated on real hardware by the nightly Hetzner e2e/mvp.sh.
    # Only traefik is disabled — it would clash with the platform's
    # Gateway API on ports 80/443; flannel, kube-proxy and servicelb stay.
    # shellcheck disable=SC2086
    $k3d_bin cluster create "$cluster_name" \
        --servers 1 --agents 0 \
        --port "8080:80@loadbalancer" \
        --port "8443:443@loadbalancer" \
        --k3s-arg "--disable=traefik@server:0"
    printf '  k3d cluster %s is ready. kubectl context: k3d-%s\n' \
        "$cluster_name" "$cluster_name"
}

# ---------------------------------------------------------------
# bootstrap_with_retry
#   Runs `apprafter cluster-bootstrap` with APPRAFTER_BOOTSTRAP_SKIP_CILIUM
#   so it leaves k3d's default CNI in place (see k3d_up for why). That
#   makes the bootstrap fast + reliable, so the retry is just a cheap
#   safety net (cluster-bootstrap is idempotent). Requires $KUBECONFIG
#   exported (kubectl/helm).
# ---------------------------------------------------------------
bootstrap_with_retry() {
    export APPRAFTER_BOOTSTRAP_SKIP_CILIUM=1
    if apprafter cluster-bootstrap; then
        return 0
    fi
    printf '  cluster-bootstrap failed; clearing any partial argocd release and retrying\n' >&2
    helm -n argocd uninstall argocd >/dev/null 2>&1 || true
    apprafter cluster-bootstrap
}

# ---------------------------------------------------------------
# k3d_down <cluster-name>
#   Delete a k3d cluster. Safe (no-op) when the cluster does not
#   exist.
# ---------------------------------------------------------------
k3d_down() {
    local cluster_name="$1"
    local k3d_bin
    if command -v k3d >/dev/null 2>&1; then
        k3d_bin="k3d"
    else
        k3d_bin="nix run nixpkgs#k3d --"
    fi
    # `k3d cluster delete` exits 0 even when the cluster is absent.
    # shellcheck disable=SC2086
    $k3d_bin cluster delete "$cluster_name" || true
}

# ---------------------------------------------------------------
# apprafter <args...>
#   Run the AppRafter CLI from source so changes under cli/ are
#   always reflected without a separate install step.
# ---------------------------------------------------------------
apprafter() {
    (cd "${REPO_ROOT}/cli" && cargo run --quiet --bin apprafter -- "$@")
}

# ---------------------------------------------------------------
# dump_diagnostics
#   Best-effort cluster-state dump for CI debugging. Call this on
#   failure BEFORE tearing the cluster down — otherwise the evidence
#   (stuck pods, events, helm-hook state) is destroyed with it.
#   No-op + never fails if KUBECONFIG/kubectl are unavailable.
# ---------------------------------------------------------------
dump_diagnostics() {
    command -v kubectl >/dev/null 2>&1 || return 0
    [ -n "${KUBECONFIG:-}" ] || return 0
    printf '\n----- cluster diagnostics (failure) -----\n' >&2
    kubectl get nodes -o wide >&2 2>&1 || true
    printf '\n--- pods (all namespaces) ---\n' >&2
    kubectl get pods -A -o wide >&2 2>&1 || true
    printf '\n--- not-Ready pods (describe + logs) ---\n' >&2
    # Catch pods that are not-Running/Completed AND Running-but-not-
    # fully-ready (e.g. a crash-looping `0/1 Running` cilium-agent —
    # READY ratio r[1] != r[2]). For each, dump describe + current and
    # previous-instance container logs (the crash reason lives there).
    kubectl get pods -A --no-headers 2>/dev/null \
        | awk '{split($3, r, "/");
                if ($4 != "Running" && $4 != "Completed") print $1, $2;
                else if (r[1] != r[2]) print $1, $2}' \
        | while read -r ns pod; do
            printf '\n=== describe %s/%s ===\n' "$ns" "$pod" >&2
            kubectl -n "$ns" describe pod "$pod" >&2 2>&1 || true
            printf '\n--- logs %s/%s (current) ---\n' "$ns" "$pod" >&2
            kubectl -n "$ns" logs "$pod" --all-containers --tail=60 >&2 2>&1 || true
            printf '\n--- logs %s/%s (previous instance) ---\n' "$ns" "$pod" >&2
            kubectl -n "$ns" logs "$pod" --all-containers --previous --tail=60 >&2 2>&1 || true
        done
    printf '\n--- recent events ---\n' >&2
    kubectl get events -A --sort-by=.lastTimestamp 2>/dev/null | tail -60 >&2 || true
    printf '\n--- helm releases ---\n' >&2
    (command -v helm >/dev/null 2>&1 && helm list -A >&2 2>&1) || true
    # Argo CD Applications + why each is not Synced/Healthy — the
    # operationState message carries chart-pull / render errors (e.g.
    # an unpublished OCI version or a CMP failure).
    printf '\n--- argo applications ---\n' >&2
    kubectl get applications.argoproj.io -A >&2 2>&1 || true
    for app in $(kubectl -n argocd get applications.argoproj.io -o name 2>/dev/null); do
        printf '\n=== %s ===\n' "$app" >&2
        kubectl -n argocd get "$app" -o jsonpath=\
'sync={.status.sync.status} health={.status.health.status}{"\n"}conditions={range .status.conditions[*]}[{.type}: {.message}]{end}{"\n"}op={.status.operationState.phase}: {.status.operationState.message}{"\n"}' \
            >&2 2>&1 || true
    done
    printf '%s\n' '----- end diagnostics -----' >&2
}
