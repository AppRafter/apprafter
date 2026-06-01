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
    # The k3s flags MUST mirror the real Tier-1 cluster's k3s install
    # (cli-providers/.../user_data.rs) so Cilium can take over the CNI:
    #   --flannel-backend=none  — k3s flannel-vxlan otherwise claims
    #     UDP 8472, the same port Cilium's cilium_vxlan needs; without
    #     it the cilium-agent crash-loops on "address already in use".
    #   --disable-network-policy — k3s kube-router conflicts with
    #     Cilium's own NetworkPolicy enforcement (dueling iptables).
    #   --disable-kube-proxy — Cilium's eBPF kubeProxyReplacement
    #     (k8sServiceHost 127.0.0.1:6443 reaches the node-local API)
    #     takes over service routing.
    # traefik + servicelb are replaced by Gateway API / Cilium L2.
    #
    # Dual-stack (ADR 0017): the production Cilium config enables IPv6,
    # so the cilium-agent blocks on "required IPv6 PodCIDR not available"
    # until the node carries one. k3s only allocates an IPv6 PodCIDR
    # when (a) cluster-cidr/service-cidr are dual-stack (the exact pair
    # from user_data.rs CLUSTER_CIDR_DUAL_STACK / SERVICE_CIDR_DUAL_STACK)
    # AND (b) the node itself has an IPv6 address — so k3d runs on a
    # pre-created IPv6-enabled docker network.
    # The network must be DUAL-stack: k3d needs an IPv4 gateway (an
    # IPv6-only network fails cluster creation with "no gateway
    # defined"), and the IPv6 subnet gives the node an IPv6 address so
    # k3s enables dual-stack. Provide both an IPv4 and an IPv6 subnet.
    local net="k3d-${cluster_name}-net"
    docker network rm "$net" >/dev/null 2>&1 || true
    # Explicit --gateway for BOTH families: k3d reads the network's
    # IPv4 gateway and fails ("no gateway defined") if it is unset.
    # Output is left visible + inspected so a create failure or a
    # missing gateway is diagnosable straight from the CI log.
    docker network create --ipv6 \
        --subnet "172.45.0.0/16" --gateway "172.45.0.1" \
        --subnet "fd00:dead:42::/64" --gateway "fd00:dead:42::1" \
        "$net" || true
    docker network inspect "$net" \
        --format 'k3d net {{.Name}}: {{range .IPAM.Config}}{{.Subnet}} gw={{.Gateway}} {{end}}' \
        2>&1 || true
    # shellcheck disable=SC2086
    $k3d_bin cluster create "$cluster_name" \
        --network "$net" \
        --servers 1 --agents 0 \
        --port "8080:80@loadbalancer" \
        --port "8443:443@loadbalancer" \
        --k3s-arg "--flannel-backend=none@server:0" \
        --k3s-arg "--disable-network-policy@server:0" \
        --k3s-arg "--disable-kube-proxy@server:0" \
        --k3s-arg "--disable=traefik@server:0" \
        --k3s-arg "--disable=servicelb@server:0" \
        --k3s-arg "--cluster-cidr=10.42.0.0/16,fd00:42::/64@server:0" \
        --k3s-arg "--service-cidr=10.43.0.0/16,fd00:43::/112@server:0"
    printf '  k3d cluster %s is ready. kubectl context: k3d-%s\n' \
        "$cluster_name" "$cluster_name"
}

# ---------------------------------------------------------------
# bootstrap_with_retry
#   `apprafter cluster-bootstrap` installs Cilium first, then Argo CD.
#   On k3d-in-CI Cilium is slow to become Ready (~5 min), and until it
#   is, the kube API ClusterIP (10.43.0.1) has no service routing
#   (kube-proxy is disabled), so Argo CD's pods time out reaching the
#   API and `helm --wait argocd` expires. cluster-bootstrap is
#   idempotent, so: run it once; if it fails, wait for the Cilium
#   DaemonSet to be Ready, clear the half-installed Argo CD release,
#   and run it again — the second pass finds Cilium Ready and Argo CD
#   comes up. Requires $KUBECONFIG to be exported (kubectl/helm).
# ---------------------------------------------------------------
bootstrap_with_retry() {
    if apprafter cluster-bootstrap; then
        return 0
    fi
    printf '  first cluster-bootstrap failed; waiting for Cilium, then retrying\n' >&2
    kubectl -n kube-system rollout status ds/cilium --timeout=8m || true
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
    # Remove the IPv6 network k3d_up created (k3d only deletes networks
    # it created itself, not a pre-existing `--network`).
    docker network rm "k3d-${cluster_name}-net" >/dev/null 2>&1 || true
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
    printf '%s\n' '----- end diagnostics -----' >&2
}
