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
    # Writable kubeconfig target (see kind_up_cilium for why) — a read-only cwd
    # / unset HOME would otherwise fail kind's `mkdir .kube`.
    KUBECONFIG="$(mktemp -t apprafter-kube.XXXXXX)"
    export KUBECONFIG
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
#   `apiServerAddress` is pinned to 127.0.0.1 so the apiserver's serving cert
#   carries it as a SAN — that IS load-bearing for Cilium's
#   k8sServiceHost: 127.0.0.1.
#
#   `apiServerPort` is NOT. It is the HOST-side published port only; inside the
#   node kind always binds the well-known 6443, which is what Cilium (running in
#   hostNetwork) actually dials. kind's own kubeadm template says so: "we use a
#   well known port for making the API server discoverable inside docker network
#   / from the host machine such port will be accessible via a random local port
#   instead". An earlier version of this comment claimed the pin was what made
#   the kube-proxy-replacement bootstrap converge; that was wrong, and it is
#   worth knowing because the pin is also what stops two Cilium clusters
#   coexisting on one host.
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
    # Fail fast (with the exact one-time host remedy) when a rootless-podman host
    # caps the kind node's memlock below what cilium-agent needs — otherwise the
    # agent CrashLoopBackOffs ~7 min into the run. No-op on rootful Docker (CI).
    require_cilium_memlock
    # The kind config is fed on stdin (`--config -`). Disable the default CNI
    # + kube-proxy so Cilium owns L3/L4 and service routing; pin the apiserver
    # to 127.0.0.1:6443 to match the platform's Cilium k8sServiceHost pin.
    #
    # extraMounts /sys/fs/bpf: Cilium's `mount-bpf-fs` init container mounts
    # the BPF filesystem, which a rootless-podman kind node cannot do itself
    # (`mount: /sys/fs/bpf: permission denied`); bind-mounting the host bpffs
    # in lets the init proceed. On CI (rootful Docker) it is a harmless re-bind.
    # NOTE: rootless podman caps the kind node's memlock at the host user's
    # systemd hard limit (default 8 MB); cilium-agent raises RLIMIT_MEMLOCK to
    # infinity and Fatals otherwise (`failed to set memlock rlimit`). No
    # container flag (privileged / CAP_SYS_RESOURCE / --ulimit) bypasses it —
    # only a one-time root host change does (see require_cilium_memlock, which
    # fails fast above with the exact fix). Rootful Docker (CI) ships
    # LimitMEMLOCK=infinity, so CI needs nothing.
    # kind writes a kubeconfig during `create`; point it at a writable temp so a
    # read-only cwd / unset HOME (e.g. sandbox-run shares /project read-only)
    # does not fail with `mkdir .kube: read-only file system`. The walk
    # re-exports its own KUBECONFIG via cluster_kubeconfig_write afterwards.
    KUBECONFIG="$(mktemp -t apprafter-kube.XXXXXX)"
    export KUBECONFIG
    _kind create cluster --name "$cluster_name" --config - <<'KINDCFG'
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
networking:
  disableDefaultCNI: true
  kubeProxyMode: "none"
  # The platform's Cilium ships ipv6.enabled=true (component_cilium.cue), so the
  # cluster must be DUAL-STACK or the agent blocks forever on "required IPv6
  # PodCIDR not available" (it waits for an IPv6 PodCIDR the node never gets on
  # an IPv4-only kind cluster) and never reaches Ready.
  ipFamily: dual
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
# ---------------------------------------------------------------
# branch_image_build <operator-subdir> <image-ref>
#   Build an operator-workspace image from the working tree — ONCE PER SUITE,
#   not once per walk.
#
# WHY THIS EXISTS
#
# Every walk that sets APPRAFTER_E2E_LOCAL_OPERATOR carried its OWN copy of a
# `build_load_restart` helper, and every copy rebuilt the same image. Measured
# on the former `needs-env-refs-walk` (since merged into
# `env-and-secrets-walk`): 8m46s total, of which the cluster is 17s, the
# bootstrap 2m01, the BUILD 3m04, and the assertions 3m24. Sixteen walks build
# it. A full suite therefore spends roughly forty minutes producing an artefact
# that is byte-identical every time. (That same duplication is what the
# needs-env-refs / secrets-ux merge removed for those two specifically.)
#
# The cache key is the content of everything the image is built from —
# `operator/` plus the bundled `schemas/v1alpha1/` — so it invalidates exactly
# when the image would differ, and never when it would not. A dirty working
# tree is handled by hashing FILE CONTENT rather than the git revision: an
# uncommitted edit produces a different key, which is the whole point during
# development.
#
# Cache lives under $APPRAFTER_E2E_IMAGE_CACHE (default: a stable path under
# TMPDIR), so a suite driver gets reuse across walks for free while a single
# ad-hoc walk still works with no setup.
# ---------------------------------------------------------------
branch_image_cache_key() {
    # Hash the tracked+untracked content of the build context. `find | sort`
    # keeps it deterministic; -print0/-0 survives odd filenames.
    { find "${REPO_ROOT}/operator" "${REPO_ROOT}/schemas/v1alpha1" \
        -type f \( -name '*.rs' -o -name '*.toml' -o -name '*.lock' \
        -o -name '*.cue' -o -name 'Dockerfile' -o -name '*.yaml' \) -print0 2>/dev/null \
        | sort -z | xargs -0 sha256sum 2>/dev/null; } | sha256sum | cut -c1-16
}

BRANCH_IMAGE_CACHE="${APPRAFTER_E2E_IMAGE_CACHE:-${TMPDIR:-/tmp}/apprafter-e2e-images}"

branch_image_build() { # <operator-subdir> <image-ref>
    local sub="$1" img="$2" builder key tar
    builder=podman
    command -v podman >/dev/null 2>&1 || builder=docker
    key="$(branch_image_cache_key)"
    mkdir -p "$BRANCH_IMAGE_CACHE"
    tar="${BRANCH_IMAGE_CACHE}/${sub}-${key}.tar"

    if [ -s "$tar" ]; then
        printf '  reusing cached %s image (key %s) — no rebuild\n' "$sub" "$key"
        "$builder" load -i "$tar" >/dev/null
        # The cached tarball carries whatever ref it was built under; retag to
        # the ref THIS cluster renders, so a chart version bump between walks
        # does not force a rebuild of an identical binary.
        local cached_ref
        cached_ref="$("$builder" load -i "$tar" 2>&1 | sed -n 's/.*: \(.*\)$/\1/p' | tail -1)"
        [ -n "$cached_ref" ] && [ "$cached_ref" != "$img" ] && \
            "$builder" tag "$cached_ref" "$img" 2>/dev/null || true
        return 0
    fi

    printf '  building %s from the working tree (%s, cache key %s) ...\n' "$img" "$builder" "$key"
    "$builder" build -f "${REPO_ROOT}/operator/${sub}/Dockerfile" -t "$img" "${REPO_ROOT}/operator"
    "$builder" save -o "$tar" "$img" 2>/dev/null \
        || printf '  (could not cache %s — the next walk rebuilds)\n' "$sub" >&2
}

# ---------------------------------------------------------------
# build_load_restart <deployment> <operator-subdir>
#   The whole local-operator override in one call: wait for the released
#   Deployment, read the ref IT renders, build (or reuse) that ref from the
#   working tree, side-load it, and roll.
#
#   Reads the ref off the LIVE object rather than hardcoding it, so a chart
#   version bump never silently side-loads under a name nothing pulls — the
#   D24 failure, where a walk built an image and tested a different one.
#
#   Ten walks carried a byte-identical private copy of this. One copy means one
#   place to fix when the shape changes again.
# ---------------------------------------------------------------
build_load_restart() { # <deployment> <operator-subdir> [cluster-name]
    local dep="$1" sub="$2" cluster="${3:-$CLUSTER_NAME}" img
    printf '  waiting for the %s Deployment to appear ...\n' "$dep"
    for _ in $(seq 1 60); do
        kubectl -n apprafter-system get deploy "$dep" >/dev/null 2>&1 && break
        sleep 5
    done
    img=$(kubectl -n apprafter-system get deploy "$dep" \
        -o jsonpath='{.spec.template.spec.containers[0].image}')
    if [ -z "$img" ]; then
        printf 'ERROR: %s Deployment never appeared — cannot learn which image to build\n' "$dep" >&2
        return 1
    fi
    branch_image_build "$sub" "$img"
    cluster_load_image "$cluster" "$img"
    kubectl -n apprafter-system rollout restart "deploy/${dep}"
    kubectl -n apprafter-system rollout status "deploy/${dep}" --timeout=240s
}

cluster_load_image() {
    local cluster_name="$1" image="$2"
    # Load from whichever engine's store actually HAS the image, NOT from the
    # cluster provider's store. The local-operator build prefers podman
    # (`builder=podman; … || builder=docker`), but a CI runner ships BOTH
    # podman AND docker, and kind/k3d there read the DOCKER store — so a
    # podman-built image is "image … not present locally" to `kind load
    # docker-image` / `k3d image import` (the failure the pg/redis/disk/
    # networkpolicy/env-refs nightlies hit). `_kind_uses_podman` reflects the
    # CLUSTER provider, not where the BUILD landed, so it's the wrong signal.
    # When the image lives in podman's store, export a docker-format tarball
    # and load THAT — store-agnostic for both kind and k3d.
    if command -v podman >/dev/null 2>&1 && podman image exists "$image" 2>/dev/null; then
        local _imgtar
        _imgtar="$(mktemp -t apprafter-img.XXXXXX.tar)"
        podman save -o "$_imgtar" "$image"
        if [ "$(cluster_runtime)" = "kind" ]; then
            _kind load image-archive "$_imgtar" --name "$cluster_name"
        else
            # shellcheck disable=SC2086
            $(_k3d_bin) image import "$_imgtar" --cluster "$cluster_name"
        fi
        rm -f "$_imgtar"
    elif [ "$(cluster_runtime)" = "kind" ]; then
        _kind load docker-image "$image" --name "$cluster_name"
    else
        # shellcheck disable=SC2086
        $(_k3d_bin) image import "$image" --cluster "$cluster_name"
    fi
}

# ---------------------------------------------------------------
# detect_host_gateway_ip
#   The host IP that in-cluster PODS use to reach a service the walk runs on
#   the HOST (e.g. the gitops `git daemon` on :9418). Runtime-aware, because
#   the engines differ fundamentally:
#     * kind + rootless podman: the bridge gateway lives in the rootless
#       network namespace and does NOT route to the host (a pod dial gets
#       "connection refused"). Podman injects `host.containers.internal`
#       (netavark's link-local host endpoint, ~169.254.x) into every node's
#       /etc/hosts — read its IP off the node.
#     * kind + docker (CI runners): the node attaches to the FIXED `kind`
#       docker network whose bridge gateway IS the host and is routable from
#       pods; `host.containers.internal` is podman-only, so resolve the
#       gateway from `docker network inspect kind` instead.
#     * k3d + docker: same idea, per-cluster network `k3d-<name>`.
#   Echoes the IP on stdout; exits non-zero with a diagnostic otherwise.
#   (Always called in `$(...)`, so `exit` only unwinds the subshell.)
# ---------------------------------------------------------------
detect_host_gateway_ip() {
    local gw net_name
    if [ "$(cluster_runtime)" = "kind" ] && _kind_uses_podman; then
        gw=$(podman exec "${CLUSTER_NAME}-control-plane" \
            getent hosts host.containers.internal 2>/dev/null | awk '{print $1; exit}')
        if [ -z "$gw" ]; then
            printf 'ERROR: could not resolve host.containers.internal on kind node %s-control-plane\n' \
                "$CLUSTER_NAME" >&2
            exit 1
        fi
        printf '%s' "$gw"
        return 0
    fi
    # docker-backed: kind → the fixed `kind` network; k3d → `k3d-<cluster>`.
    if [ "$(cluster_runtime)" = "kind" ]; then
        net_name="kind"
    else
        net_name="k3d-${CLUSTER_NAME}"
    fi
    gw=$(docker network inspect "$net_name" 2>/dev/null \
        | jq -r '.[0] | ((.subnets // .IPAM.Config // [])[]
                 | (.gateway // .Gateway // empty))' 2>/dev/null \
        | grep -E '^[0-9]+\.' | head -1)
    if [ -z "$gw" ]; then
        printf 'ERROR: could not detect IPv4 gateway of docker network %s\n' "$net_name" >&2
        exit 1
    fi
    printf '%s' "$gw"
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
# require_cilium_memlock
#   cilium-agent raises RLIMIT_MEMLOCK to infinity at startup and Fatals on
#   failure (`failed to set memlock rlimit: operation not permitted`). Under
#   rootless podman, the kind node container is capped at the host user's
#   systemd memlock HARD limit (default 8 MB) and CANNOT exceed it — no
#   container flag (privileged / CAP_SYS_RESOURCE / --ulimit) helps (the cap is
#   the host user-manager limit, verified). So fail fast HERE with the exact
#   one-time root remedy instead of letting the dev watch a ~7-minute
#   CrashLoopBackOff. Rootful Docker (CI) ships LimitMEMLOCK=infinity, so this
#   is a no-op there. Other walks use kindnet (no Cilium) and never hit this.
# ---------------------------------------------------------------
require_cilium_memlock() {
    _kind_uses_podman || return 0   # rootful docker node already gets unlimited memlock
    local hard
    hard="$(podman run --rm docker.io/library/busybox:latest sh -c 'ulimit -Hl' 2>/dev/null || true)"
    [ "$hard" = "unlimited" ] && return 0
    cat >&2 <<EOF
ERROR: Cilium cannot start on this rootless podman host — the kind node's memlock
hard limit is ${hard:-too low} KB, but cilium-agent needs it unlimited (it raises
RLIMIT_MEMLOCK to infinity and Fatals: "failed to set memlock rlimit: operation
not permitted"). No container flag can exceed the host user's systemd cap.

One-time host fix (root), then re-login:
  sudo mkdir -p /etc/systemd/system/user@.service.d
  printf '[Service]\nLimitMEMLOCK=infinity\n' | \\
    sudo tee /etc/systemd/system/user@.service.d/90-memlock.conf
  sudo systemctl daemon-reload
  loginctl terminate-user "\$USER"   # or log out/in (a reboot also works)
  # verify: podman run --rm busybox sh -c 'ulimit -Hl'   # must print: unlimited

GitHub Actions / rootful Docker need nothing (dockerd runs LimitMEMLOCK=infinity).
See docs/operator-guide/egress-policy.md.
EOF
    exit 2
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
# ensure_restic_on_path
#   The 2.6d backup/restore CLI shells out to `restic` via
#   `Command::new("restic")`, so restic MUST be resolvable on $PATH
#   for the CLI subprocess (NOT just inside this shell). When the bare
#   binary is absent (a fresh nix-dev checkout), install a thin wrapper
#   under a temp bin dir and PREPEND it to $PATH so both this shell and
#   the `cargo run` child inherit it. The wrapper execs
#   `nix run nixpkgs#restic --` (the project's standard
#   missing-binary fallback — same pattern as ~/bin/cue / ~/bin/helm).
#   Idempotent: a no-op when restic is already on $PATH.
#   Modifies $PATH IN THE CURRENT SHELL (so the `cargo run` child inherits it)
#   and sets the global RESTIC_WRAPPER_BIN_DIR to the wrapper dir (empty when
#   restic was already present) so the caller can clean it up. Call WITHOUT a
#   subshell (`ensure_restic_on_path`, not `$(ensure_restic_on_path)`), else
#   the PATH export is lost with the subshell.
# ---------------------------------------------------------------
RESTIC_WRAPPER_BIN_DIR=""
ensure_restic_on_path() {
    if command -v restic >/dev/null 2>&1; then
        return 0
    fi
    local bindir
    bindir="$(mktemp -d -t apprafter-restic-bin.XXXXXX)"
    cat >"${bindir}/restic" <<'WRAP'
#!/usr/bin/env bash
exec nix run nixpkgs#restic -- "$@"
WRAP
    chmod +x "${bindir}/restic"
    PATH="${bindir}:${PATH}"
    export PATH
    RESTIC_WRAPPER_BIN_DIR="$bindir"
}

# ---------------------------------------------------------------
# dump_diagnostics
#   Best-effort cluster-state dump for CI debugging. Call this on
#   failure BEFORE tearing the cluster down — otherwise the evidence
#   (stuck pods, events, helm-hook state) is destroyed with it.
#   No-op + never fails if KUBECONFIG/kubectl are unavailable.
# ---------------------------------------------------------------
# ---------------------------------------------------------------
# apply_branch_operator_rbac
#
# In APPRAFTER_E2E_LOCAL_OPERATOR mode the walks swap the operator IMAGE but
# leave the cluster's RBAC as the published chart wrote it. So a rule added in
# the same commit as the code that needs it is invisible to every local walk:
# the new binary runs against the old ClusterRole and 403s.
#
# That is not hypothetical. The 2.22 battery spent three runs establishing that
# the D8 Postgres size sampler "was not reaching the claim", and the answer was
# `pods/proxy` forbidden in cnpg-system — a verb granted in the branch chart
# and absent from the published one. The repo's own recurring lesson is that
# only a live cluster catches an RBAC/verb mismatch; this makes the local
# clusters able to catch it too.
#
# `-n apprafter-system` is load-bearing: without it `helm template` renders the
# ClusterRoleBinding subject with the wrong namespace and the binding silently
# grants nothing (walk-fix 3ac1972).
# ---------------------------------------------------------------
# ---------------------------------------------------------------
# apply_branch_operator_crds
#
# The companion to `apply_branch_operator_rbac`, for the same reason and with
# the same failure mode: a walk running the branch's operator against the
# PUBLISHED CRDs cannot exercise a field the branch added. The apiserver
# accepts the write and prunes the unknown key, so the feature reads as broken
# with no error anywhere.
#
# The 2.22 battery hit exactly that: `backup enable --timezone Europe/Berlin`
# refused with "the cluster did not store the timezone (it reads back as
# None)" — 2.22g's own read-back guard working perfectly, against a CRD that
# predates `spec.backup.timeZone`.
# ---------------------------------------------------------------
apply_branch_operator_crds() {
    local chart="${REPO_ROOT}/operator/charts/apprafter-operator"
    [ -d "$chart" ] || { printf '  (no operator chart at %s — skipping branch CRDs)\n' "$chart"; return 0; }
    _crd_yq() { if command -v yq >/dev/null 2>&1; then yq "$@"; else nix run nixpkgs#yq-go -- "$@"; fi; }
    local out
    out=$(helm template apprafter-operator "$chart" \
        | _crd_yq 'select(.kind == "CustomResourceDefinition")' \
        | kubectl apply --server-side --force-conflicts -f - 2>&1) || {
        printf 'ERROR: applying branch CRDs failed:\n%s\n' "$out" >&2
        return 1
    }
    # Report the COUNT, not a bare "applied". An unconditional success line
    # proves nothing, and this helper spent three walk rounds appearing to
    # work while the field it exists for stayed pruned.
    printf '  branch CRDs applied (%s object(s))\n' "$(printf '%s\n' "$out" | grep -c .)"
}

apply_branch_operator_rbac() {
    local chart="${REPO_ROOT}/operator/charts/apprafter-operator"
    [ -d "$chart" ] || { printf '  (no operator chart at %s — skipping branch RBAC)\n' "$chart"; return 0; }
    _rbac_yq() { if command -v yq >/dev/null 2>&1; then yq "$@"; else nix run nixpkgs#yq-go -- "$@"; fi; }
    helm template apprafter-operator "$chart" -n apprafter-system \
        | _rbac_yq 'select(.kind == "ClusterRole" or .kind == "ClusterRoleBinding" or .kind == "Role" or .kind == "RoleBinding" or .kind == "ServiceAccount")' \
        | kubectl apply --server-side --force-conflicts -f - >/dev/null
    printf '  branch operator RBAC applied (ClusterRole/Binding, Role/Binding, SA)\n'
}

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
    # apprafter-system control-plane logs ALWAYS — the operator and
    # admission-webhook run 1/1 Ready, so the not-Ready loop above skips
    # them, yet a reconcile that errors before writing any status (empty
    # `.status.phase`) leaves its only trace in the operator log. Dump
    # the full control-plane regardless of Ready state.
    printf '\n--- apprafter-system control-plane logs ---\n' >&2
    for dep in apprafter-operator admission-webhook resourceclaim-provisioner resourceclaim-scheduler; do
        kubectl -n apprafter-system get deploy "$dep" >/dev/null 2>&1 || continue
        printf '\n=== logs deploy/%s (tail 120) ===\n' "$dep" >&2
        kubectl -n apprafter-system logs "deploy/$dep" --all-containers --tail=120 >&2 2>&1 || true
    done
    # Application + ResourceClaim CRs in the workload namespace — the
    # full status (phase, conditions) that the wait-loop only sampled.
    printf '\n--- Application + ResourceClaim CRs (all namespaces) ---\n' >&2
    kubectl get applications.apprafter.io -A -o wide >&2 2>&1 || true
    kubectl get resourceclaims.apprafter.io -A -o wide >&2 2>&1 || true
    for app in $(kubectl get applications.apprafter.io -A -o jsonpath='{range .items[*]}{.metadata.namespace}/{.metadata.name}{"\n"}{end}' 2>/dev/null); do
        ns="${app%%/*}"; nm="${app##*/}"
        printf '\n=== application.apprafter.io/%s (-n %s) status ===\n' "$nm" "$ns" >&2
        kubectl -n "$ns" get application.apprafter.io "$nm" -o jsonpath=\
'uid={.metadata.uid}{"\n"}phase={.status.phase}{"\n"}needs(base)={.spec.base.needs}{"\n"}conditions={range .status.conditions[*]}[{.type}={.status}: {.message}]{end}{"\n"}' \
            >&2 2>&1 || true
    done
    # The DECLARED need set (above) against the OWNED claim set (below) is the
    # pair that explains every prune outcome: a claim survives a removal either
    # because the spec still declares it or because its controller ownerRef uid
    # does not match the Application's. Both halves were absent from this dump,
    # so a needs-removal failure could only be guessed at post-mortem.
    printf '\n--- claim -> controller ownerRef uid ---\n' >&2
    kubectl get resourceclaims.apprafter.io -A -o jsonpath=\
'{range .items[*]}{.metadata.namespace}/{.metadata.name} owner={range .metadata.ownerReferences[?(@.controller==true)]}{.kind}/{.name}:{.uid}{end} deleting={.metadata.deletionTimestamp}{"\n"}{end}' \
        >&2 2>&1 || true
    # MigrationPlans. A gated change (needs removal, destructive edit) stalls
    # until its plan is approved AND executed, so the plan state is the first
    # thing to read when a gated operation "never happened".
    printf '\n--- migration plans ---\n' >&2
    kubectl get migrationplans.apprafter.io -A >&2 2>&1 || true
    for plan in $(kubectl get migrationplans.apprafter.io -A \
        -o jsonpath='{range .items[*]}{.metadata.namespace}/{.metadata.name}{"\n"}{end}' 2>/dev/null); do
        ns="${plan%%/*}"; nm="${plan##*/}"
        printf '\n=== migrationplan/%s (-n %s) ===\n' "$nm" "$ns" >&2
        kubectl -n "$ns" get migrationplan.apprafter.io "$nm" -o jsonpath=\
'trigger={.spec.trigger} class={.spec.classification} app={.spec.applicationRef.name}{"\n"}phase={.status.phase} approvedAt={.status.approvedAt} message={.status.message}{"\n"}' \
            >&2 2>&1 || true
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
