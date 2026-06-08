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
# cluster_runtime — which local-cluster tool to use.
#   "k3d"  when a docker daemon is reachable (CI runners use docker).
#   "kind" otherwise — the nix-dev default is rootless podman, and kind
#          has first-class podman support (KIND_EXPERIMENTAL_PROVIDER=
#          podman), whereas k3d's tools node bind-mounts the literal
#          /var/run/docker.sock, which rootless podman cannot create
#          (mkdir … permission denied). Override: APPRAFTER_E2E_RUNTIME=
#          k3d|kind.
# ---------------------------------------------------------------
cluster_runtime() {
    # kind is the default everywhere — local nix-dev (rootless podman) AND CI.
    # It has first-class podman support and, unlike k3d, runs Cilium without
    # the eBPF-convergence slowdown, so the 2.10 egress walk can enable it.
    # k3d's tools node also bind-mounts the literal /var/run/docker.sock,
    # which rootless podman cannot create. k3d stays an opt-in escape hatch
    # via APPRAFTER_E2E_RUNTIME=k3d.
    printf '%s' "${APPRAFTER_E2E_RUNTIME:-kind}"
}

_k3d_bin()  { if command -v k3d  >/dev/null 2>&1; then echo "k3d";  else echo "nix run nixpkgs#k3d --";  fi; }
_kind_bin() { if command -v kind >/dev/null 2>&1; then echo "kind"; else echo "nix run nixpkgs#kind --"; fi; }

# _kind_uses_podman — true when podman is the container runtime, so kind
# should run under KIND_EXPERIMENTAL_PROVIDER=podman; false when a real
# docker daemon answers (kind's default, and the most battle-tested provider
# on CI runners). The rootless nix-dev shell has neither a docker shim nor
# DOCKER_HOST but uses podman, so default to podman when no docker responds.
_kind_uses_podman() {
    case "${DOCKER_HOST:-}" in *podman*) return 0 ;; esac
    docker --version 2>/dev/null | grep -qi podman && return 0
    docker info 2>/dev/null | grep -qi podman && return 0
    if docker info >/dev/null 2>&1; then return 1; else return 0; fi
}

# _kind <args...> — run kind, selecting the podman provider when podman is
# the runtime, else kind's docker default.
_kind() {
    local bin; bin="$(_kind_bin)"
    if _kind_uses_podman; then
        # shellcheck disable=SC2086
        KIND_EXPERIMENTAL_PROVIDER=podman $bin "$@"
    else
        # shellcheck disable=SC2086
        $bin "$@"
    fi
}

# _cilium <args...> / _hubble <args...> — the Cilium / Hubble CLIs, wrapping a
# `nix run nixpkgs#…` fallback when the bare binary is absent (the project
# convention — `nix develop` / flake.nix ships `cilium-cli`, a fresh checkout
# falls back to the pinned nixpkgs build). Used only by the 2.10 egress walk.
_cilium() { if command -v cilium >/dev/null 2>&1; then cilium "$@"; else nix run nixpkgs#cilium-cli -- "$@"; fi; }
_hubble() { if command -v hubble >/dev/null 2>&1; then hubble "$@"; else nix run nixpkgs#hubble -- "$@"; fi; }

# ---------------------------------------------------------------
# k3d_up <cluster-name>
#   Bring up a local single-node cluster — kind by default (k3d opt-in via
#   APPRAFTER_E2E_RUNTIME=k3d). Default CNI (kind kindnet / k3d flannel) +
#   kube-proxy, NOT Cilium: cluster-bootstrap runs with
#   APPRAFTER_BOOTSTRAP_SKIP_CILIUM=1 (see bootstrap_with_retry) because
#   Cilium's eBPF datapath converges pathologically slowly on a local
#   cluster; the GitOps/claim logic the e2e exercises needs only a working
#   CNI. Cilium itself is validated on real hardware by e2e/mvp.sh.
# ---------------------------------------------------------------
k3d_up() {
    if [ "$(cluster_runtime)" = "kind" ]; then _kind_up "$1"; else _k3d_up "$1"; fi
}

_k3d_up() {
    local cluster_name="$1" k3d_bin
    k3d_bin="$(_k3d_bin)"
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

_kind_up() {
    local cluster_name="$1"
    # Bare single-node cluster: kindnet CNI + kube-proxy ship by default
    # (a working CNI — same role as k3d's flannel), the control-plane node
    # is schedulable, and kind publishes the API server on a random host
    # port (in the kubeconfig). Deliberately NO host port-mappings: the
    # claim/DSN/GC walks are all in-cluster (no ingress), kind has no
    # servicelb anyway, and binding 80/443 just risks a host-port clash for
    # no benefit.
    _kind create cluster --name "$cluster_name"
    printf '  kind cluster %s is ready. kubectl context: kind-%s\n' \
        "$cluster_name" "$cluster_name"
}

# ---------------------------------------------------------------
# kind_up_cilium <cluster-name>
#   Bring up a kind cluster whose datapath is owned by CILIUM (not the
#   default kindnet CNI + kube-proxy), so the 2.10 egress walk can run real
#   CiliumNetworkPolicy enforcement + Hubble. The kind config:
#     networking.disableDefaultCNI: true   — no kindnet; the node stays
#       NotReady until cluster-bootstrap installs Cilium (Cilium's DaemonSet
#       tolerates the not-ready taint, same as on k3s).
#     networking.kubeProxyMode: "none"     — no kube-proxy; Cilium runs as the
#       kube-proxy replacement (the platform's Cilium values pin
#       kubeProxyReplacement: true + k8sServiceHost: 127.0.0.1 / k8sServicePort:
#       6443 — see platform-stack/cue/loader_values.cue).
#   The apiServerAddress/apiServerPort are pinned to 127.0.0.1:6443 INSIDE the
#   node so Cilium's k8sServiceHost: 127.0.0.1 / k8sServicePort: 6443 resolve
#   the apiserver with no kube-proxy. kind always exposes the apiserver on the
#   node's loopback at the in-config port, so this pin is what makes the
#   kube-proxy-replacement bootstrap converge.
#   Cilium-only (k3d does not get a Cilium-on variant): this walk forces
#   APPRAFTER_E2E_RUNTIME=kind (Cilium's eBPF datapath is pathologically slow
#   on k3d). The caller asserts `cilium status --wait` before any enforcement.
# ---------------------------------------------------------------
kind_up_cilium() {
    local cluster_name="$1"
    if [ "$(cluster_runtime)" != "kind" ]; then
        printf 'ERROR: kind_up_cilium requires the kind runtime (got %s); set APPRAFTER_E2E_RUNTIME=kind\n' \
            "$(cluster_runtime)" >&2
        return 2
    fi
    # The kind config is fed on stdin (`--config -`). Disable the default CNI
    # + kube-proxy so Cilium owns L3/L4 and service routing; pin the apiserver
    # to 127.0.0.1:6443 to match the platform's Cilium k8sServiceHost pin.
    #
    # extraMounts /sys/fs/bpf: Cilium's `mount-bpf-fs` init container mounts
    # the BPF filesystem, which a rootless-podman kind node cannot do itself
    # (`mount: /sys/fs/bpf: permission denied`); bind-mounting the host bpffs
    # in lets the init proceed. On CI (rootful Docker) it is a harmless re-bind.
    # NOTE: even with the mount, a rootless-podman host with a capped memlock
    # ulimit (e.g. 8 MB) still cannot run the cilium-agent (`failed to set
    # memlock rlimit: operation not permitted`) — the FULL enforcement run
    # needs a rootful runtime (CI) or a host memlock raise. The bootstrap +
    # `cilium status --wait` converge under CI's rootful Docker.
    _kind create cluster --name "$cluster_name" --config - <<'KINDCFG'
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
networking:
  disableDefaultCNI: true
  kubeProxyMode: "none"
  apiServerAddress: "127.0.0.1"
  apiServerPort: 6443
nodes:
  - role: control-plane
    extraMounts:
      - hostPath: /sys/fs/bpf
        containerPath: /sys/fs/bpf
KINDCFG
    printf '  kind+Cilium cluster %s created (default CNI + kube-proxy disabled). kubectl context: kind-%s\n' \
        "$cluster_name" "$cluster_name"
}

# ---------------------------------------------------------------
# cluster_kubeconfig_write <cluster-name> <output-file>
#   Write the cluster's kubeconfig (k3d or kind) to <output-file>.
# ---------------------------------------------------------------
cluster_kubeconfig_write() {
    local cluster_name="$1" out="$2"
    if [ "$(cluster_runtime)" = "kind" ]; then
        _kind get kubeconfig --name "$cluster_name" >"$out"
    else
        # shellcheck disable=SC2086
        $(_k3d_bin) kubeconfig write "$cluster_name" --output "$out"
    fi
}

# ---------------------------------------------------------------
# cluster_load_image <cluster-name> <image-ref>
#   Side-load a locally-built image into the cluster's node store
#   (k3d image import / kind load). Used by the optional local-operator
#   override (build the operator from source, tag it as the released ref,
#   side-load it; the node serves it under imagePullPolicy IfNotPresent so
#   Argo CD does not fight the unchanged image ref).
# ---------------------------------------------------------------
cluster_load_image() {
    local cluster_name="$1" image="$2"
    if [ "$(cluster_runtime)" = "kind" ]; then
        _kind load docker-image "$image" --name "$cluster_name"
    else
        # shellcheck disable=SC2086
        $(_k3d_bin) image import "$image" --cluster "$cluster_name"
    fi
}

# ---------------------------------------------------------------
# bootstrap_with_retry
#   Runs `apprafter cluster-bootstrap` with APPRAFTER_BOOTSTRAP_SKIP_CILIUM
#   so it leaves the cluster's default CNI in place (see k3d_up for why). That
#   makes the bootstrap fast + reliable, so the retry is just a cheap
#   safety net (cluster-bootstrap is idempotent). Requires $KUBECONFIG
#   exported (kubectl/helm).
# ---------------------------------------------------------------
bootstrap_with_retry() {
    export APPRAFTER_BOOTSTRAP_SKIP_CILIUM=1
    # cluster-bootstrap is idempotent (helm upgrade --install + SSA),
    # so a plain re-run is the safety net — do NOT `helm uninstall`
    # anything (that orphans argocd-server, so the next install fails
    # to adopt it). The webhook-readiness race at step 5 is handled
    # inside cluster-bootstrap now, so this rarely fires.
    apprafter cluster-bootstrap || {
        printf '  cluster-bootstrap failed; retrying once (idempotent)\n' >&2
        sleep 15
        apprafter cluster-bootstrap
    }
}

# ---------------------------------------------------------------
# bootstrap_with_cilium
#   Like bootstrap_with_retry but leaves Cilium ENABLED — it explicitly does
#   NOT set APPRAFTER_BOOTSTRAP_SKIP_CILIUM, so `apprafter cluster-bootstrap`
#   installs Cilium (kube-proxy replacement) as step 0 before Argo CD. Required
#   by the 2.10 egress walk (real CiliumNetworkPolicy enforcement + Hubble) and
#   ONLY safe on a cluster brought up via kind_up_cilium (default CNI +
#   kube-proxy disabled). Cilium's eBPF datapath is slower to converge than a
#   default CNI, so the caller MUST gate any assertion on `cilium status
#   --wait` (see e2e/needs-networkpolicy-walk.sh). Requires $KUBECONFIG
#   exported (kubectl/helm).
# ---------------------------------------------------------------
bootstrap_with_cilium() {
    # Defensive: a prior bootstrap_with_retry in the same shell would have
    # exported the skip flag — unset it so Cilium installs.
    unset APPRAFTER_BOOTSTRAP_SKIP_CILIUM
    apprafter cluster-bootstrap || {
        printf '  cluster-bootstrap (Cilium-on) failed; retrying once (idempotent)\n' >&2
        sleep 20
        apprafter cluster-bootstrap
    }
}

# ---------------------------------------------------------------
# cilium_cli <args...> / hubble_cli <args...>
#   Public wrappers over the Cilium / Hubble CLIs (nix-fallback aware) for
#   sourcing walks. `cilium status --wait`, `cilium hubble enable`,
#   `hubble observe …` — see e2e/needs-networkpolicy-walk.sh.
# ---------------------------------------------------------------
cilium_cli() { _cilium "$@"; }
hubble_cli() { _hubble "$@"; }

# ---------------------------------------------------------------
# k3d_down <cluster-name>
#   Delete the local cluster (k3d or kind, per cluster_runtime).
#   Safe (no-op) when the cluster does not exist.
# ---------------------------------------------------------------
k3d_down() {
    local cluster_name="$1"
    # `cluster delete` exits 0 even when the cluster is absent.
    if [ "$(cluster_runtime)" = "kind" ]; then
        _kind delete cluster --name "$cluster_name" || true
    else
        # shellcheck disable=SC2086
        $(_k3d_bin) cluster delete "$cluster_name" || true
    fi
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
