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
# wait_condition <ns> <kind/name> <condition> [timeout-secs]
#
# `kubectl wait` does NOT wait for an object to be CREATED. Against a
# name that does not exist yet it returns immediately with
# `Error from server (NotFound)`; the --timeout only governs how long it
# waits for the CONDITION once the object is already there.
#
# That makes a bare `kubectl wait --for=condition=Available deployment/X`
# a race whenever X is created asynchronously by a controller rather than
# by the walk's own `kubectl apply` — and the race is silent until it is
# lost. It cost the `needs.redis` nightly two runs (2026-09-06 and
# 2026-09-12, both `deployments.apps "api" not found` at Phase 14) while
# reading as a product failure both times: the ResourceClaim had gone
# ready, but the Application controller had not yet rendered the
# Deployment. Ten of twelve nights passed, which is exactly why nothing
# caught it.
#
# So: poll for existence first (tolerating NotFound), then hand over to
# `kubectl wait` for the condition itself. The timeout covers BOTH
# phases, so a caller's budget means what it says.
# ---------------------------------------------------------------
wait_condition() {
    local ns="$1" ref="$2" cond="$3" timeout="${4:-300}"
    local deadline
    deadline=$(( $(date +%s) + timeout ))

    printf '  wait %s -n %s for condition=%s (timeout %ss) ...\n' \
        "$ref" "$ns" "$cond" "$timeout"

    until kubectl -n "$ns" get "$ref" >/dev/null 2>&1; do
        if [ "$(date +%s)" -ge "$deadline" ]; then
            printf 'FAILED: %s never appeared in %s within %ss (the controller did not create it)\n' \
                "$ref" "$ns" "$timeout" >&2
            kubectl -n "$ns" get all >&2 2>&1 || true
            return 1
        fi
        sleep 2
    done

    local left=$(( deadline - $(date +%s) ))
    [ "$left" -lt 5 ] && left=5
    kubectl -n "$ns" wait --for="condition=${cond}" "$ref" --timeout="${left}s"
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

# The Kubernetes version every e2e cluster runs, written down ON PURPOSE.
# Until 2026-09 no call site passed `--image`, so the KIND BINARY silently
# chose it: the pinned kind v0.24.0 defaulted to k8s v1.31.0 while production
# (k3s stable channel, unpinned — cli/cli-providers/src/hetzner_cloud/user_data.rs)
# had moved to 1.36.x. The whole e2e island was therefore testing a server six
# minors behind the one customers run, and a `kind` bump would have moved that
# server version as an invisible side effect of a "tooling" change.
#
# Keep this in step with the kind binary pinned in .github/workflows/*.yml:
# kind supports a node image only with the release that ships it, so the pair
# moves together or not at all. Current pair: kind v0.33.0 + k8s v1.36.4 —
# which is EXACTLY the version production runs, so a CRD or API assertion here
# now means what it appears to mean.
#
# Digest-pinned, not tag-pinned: `kindest/node:v1.36.4` is mutable upstream,
# and a moved tag would change the apiserver under the e2e island without a
# diff — the same class of invisible version drift this variable exists to end.
: "${APPRAFTER_KIND_NODE_IMAGE:=kindest/node:v1.36.4@sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed}"
export APPRAFTER_KIND_NODE_IMAGE

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
    _kind create cluster --name "$cluster_name" --image "$APPRAFTER_KIND_NODE_IMAGE"
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
    _kind create cluster --name "$cluster_name" --image "$APPRAFTER_KIND_NODE_IMAGE" --config - <<'KINDCFG'
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
# ECR Public images and the month-end quota
#
# cluster-bootstrap installs Argo CD from the upstream argo-cd chart, whose
# Redis image is public.ecr.aws/docker/library/redis:<tag>. ECR Public serves
# anonymous pulls 500 GB a month, "limited by source IP". GitHub-hosted runners
# share egress IPs with other tenants, and in the last days of a month some of
# those IPs have spent the quota: every anonymous GET then answers 429
# "toomanyrequests: Data limit exceeded", argocd-redis never starts and helm's
# --wait times out. That failed one walk on 2026-08-31 and three on 2026-09-30,
# and none on the 1st of any month. bootstrap_with_retry's retry cannot help:
# it runs on the same IP, and the quota lasts until the month changes.
#
# So the walks put the SAME image into each node's containerd before
# bootstrap, under its ECR name, from a registry without that quota:
# mirror.gcr.io (Google's cache of Docker Hub), then Docker Hub itself. Both
# serve the Docker Official Images that ECR Public's docker/library mirrors;
# the index digest of redis:7.4.1-alpine is identical on all three. The chart
# sets imagePullPolicy IfNotPresent, so the kubelet uses that copy and never
# asks ECR. Chart, values and image reference stay the product's own; only
# where the bytes come from changes. e2e/mvp.sh (the real-Hetzner nightly)
# does not seed, so the genuine anonymous ECR pull keeps its test.
#
# Both value sets that install the chart are covered: the loader's, which
# cluster-bootstrap installs, and the platform-stack component's, which Argo
# CD re-syncs itself to afterwards. One window stays open by design: on a PR
# that bumps the argo-cd chart, Argo CD re-syncs to the PUBLISHED platform-stack
# chart, which still pins the old version until the bump is released, so that
# version's Redis is pulled from ECR there.
#
# A containerd registry mirror (hosts.toml) cannot do this: containerd appends
# the repository path to the mirror host, and mirror.gcr.io has no
# docker/library/ path (404). The seed is best effort, so a regression in it
# is quiet in a walk; scripts/check-ecr-seed.sh is where it fails: it runs
# these functions, asks the seed sources and ECR itself, and checks the pull
# policy.
# ---------------------------------------------------------------

# argocd_charts_rendered
#   Render the Argo CD chart with each value set that installs it, and print
#   both renders: the loader's (`_loaderValues.argocd`, the export
#   cli/cli-providers/build.rs compiles into the CLI) and the platform-stack
#   component's (`_components.argocd`). Needs helm, the pinned cue and the
#   network (the chart repository). Returns non-zero when either fails.
argocd_charts_rendered() {
    local cue_dir="${REPO_ROOT}/platform-stack/cue" cue="${REPO_ROOT}/scripts/cue"
    local repo chart pair values_expr version_expr values version
    repo="$(cd "$cue_dir" && "$cue" export -e '_components.argocd.source.repoURL' --out text ./...)" || return 1
    chart="$(cd "$cue_dir" && "$cue" export -e '_components.argocd.source.chart' --out text ./...)" || return 1
    for pair in _loaderValues.argocd.values:_loaderValues.argocd.chartVersion \
        _components.argocd.values:_components.argocd.version; do
        values_expr="${pair%%:*}"
        version_expr="${pair#*:}"
        values="$(cd "$cue_dir" && "$cue" export -e "$values_expr" --out yaml ./...)" || return 1
        version="$(cd "$cue_dir" && "$cue" export -e "$version_expr" --out text ./...)" || return 1
        printf '%s\n' "$values" \
            | helm template argocd "$chart" --repo "$repo" --version "$version" --namespace argocd -f - \
            || return 1
        printf -- '---\n'
    done
}

# ecr_public_images_in
#   Read rendered manifests on stdin and print, one per line, every ECR Public
#   image (public.ecr.aws or ecr-public.aws.com; argo-helm moved to the latter
#   after 7.7.7) they name. Returns 1 when the manifests name no image at all:
#   the Argo CD chart always renders its own, so an empty list means the
#   extraction broke, and that must not read as "nothing to seed".
ecr_public_images_in() {
    local images
    images="$(sed -nE "s/^[[:space:]]*(-[[:space:]]+)?image:[[:space:]]*[\"']?([^\"'[:space:]]+).*/\\2/p" | sort -u)"
    [ -n "$images" ] || return 1
    printf '%s\n' "$images" | { grep -E '^(public\.ecr\.aws|ecr-public\.aws\.com)/' || true; }
}

# ecr_public_images_rendered
#   Every ECR Public image the Argo CD chart renders, one per line. Returns
#   non-zero when the chart cannot be rendered or no image could be read.
ecr_public_images_rendered() {
    local rendered
    rendered="$(argocd_charts_rendered)" || return 1
    printf '%s\n' "$rendered" | ecr_public_images_in
}

# ecr_public_seed_sources <image>
#   The registries holding the same content as an ECR Public docker/library
#   image, best first: <ecr-host>/docker/library/<name>:<tag> becomes
#   mirror.gcr.io/library/<name>:<tag>, then docker.io/library/<name>:<tag>.
#   Prints nothing and returns 1 for any other ECR Public path, since only
#   the Docker Official Images have a copy elsewhere, and for a digest-pinned
#   reference: CRI stores a `name:tag@digest` pull under `name@digest`, which
#   the `ctr images tag` below would not find.
ecr_public_seed_sources() {
    local image="$1" rest
    case "$image" in
        *@*) return 1 ;;
        public.ecr.aws/docker/library/*) rest="${image#public.ecr.aws/docker/library/}" ;;
        ecr-public.aws.com/docker/library/*) rest="${image#ecr-public.aws.com/docker/library/}" ;;
        *) return 1 ;;
    esac
    printf 'mirror.gcr.io/library/%s\ndocker.io/library/%s\n' "$rest" "$rest"
}

# _in_cluster_node <node> <command...>
#   Run a command inside a cluster node's container, with the engine that runs
#   it. kind and k3d both name the container after the node.
_in_cluster_node() {
    local node="$1"
    shift
    if [ "$(cluster_runtime)" = "kind" ] && _kind_uses_podman; then
        podman exec "$node" "$@"
    else
        docker exec "$node" "$@"
    fi
}

# _seed_warn <message>
#   A seed WARN on stderr and, under GitHub Actions, also a ::warning::
#   annotation, so a degraded seed shows on the summary of a walk that
#   otherwise goes green.
_seed_warn() {
    printf '  WARN: %s\n' "$1" >&2
    if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
        printf '::warning title=ECR Public seed::%s\n' "$1"
    fi
}

# seed_ecr_public_images
#   Put every ECR Public image the Argo CD chart renders into each node's
#   containerd, under its ECR name, from ecr_public_seed_sources. Best effort
#   and loud: one line per image and node, and every failure is a WARN that
#   leaves the node to pull from ECR Public as before. It never fails harder
#   than not seeding, and it is safe to run again. Requires $KUBECONFIG
#   exported.
seed_ecr_public_images() {
    local images nodes image node source sources seeded inspect digest
    if ! images="$(ecr_public_images_rendered)"; then
        _seed_warn "could not render the Argo CD chart or read its images; nodes pull ECR Public images from ECR Public"
        return 0
    fi
    if [ -z "$images" ]; then
        printf '  the Argo CD chart renders no ECR Public image; nothing to seed\n'
        return 0
    fi
    nodes="$(kubectl get nodes -o jsonpath='{.items[*].metadata.name}' 2>/dev/null)" || nodes=""
    if [ -z "$nodes" ]; then
        _seed_warn "no cluster node listed; nodes pull ECR Public images from ECR Public"
        return 0
    fi
    for image in $images; do
        if ! sources="$(ecr_public_seed_sources "$image")"; then
            _seed_warn "${image} is digest-pinned or not a docker/library image, so it is not seeded; nodes pull it from ECR Public"
            continue
        fi
        for node in $nodes; do
            seeded=""
            for source in $sources; do
                # The last step asks the CRI, which is what the kubelet asks:
                # a ctr tag the CRI image service does not see would seed nothing.
                if _in_cluster_node "$node" crictl pull "$source" >/dev/null \
                    && _in_cluster_node "$node" ctr -n k8s.io images tag --force "$source" "$image" >/dev/null \
                    && inspect="$(_in_cluster_node "$node" crictl inspecti -o json "$image")"; then
                    digest="$(printf '%s' "$inspect" \
                        | jq -r '[.status.repoDigests[]? | select(contains("@")) | sub("^[^@]*@"; "")] | first // empty' \
                            2>/dev/null)" || digest=""
                    printf '  seeded %s on %s from %s (%s)\n' "$image" "$node" "$source" "${digest:-digest unknown}"
                    seeded=1
                    break
                fi
                printf '  could not seed %s on %s from %s; trying the next source\n' "$image" "$node" "$source" >&2
            done
            [ -n "$seeded" ] || _seed_warn "could not seed ${image} on ${node}; it pulls from ECR Public"
        done
    done
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
    seed_ecr_public_images
    # cluster-bootstrap is idempotent (helm upgrade --install + SSA),
    # so a plain re-run is the safety net — do NOT `helm uninstall`
    # anything (that orphans argocd-server, so the next install fails
    # to adopt it). The webhook-readiness race at step 5 is handled
    # inside cluster-bootstrap now, so this rarely fires.
    apprafter cluster-bootstrap || {
        printf '  cluster-bootstrap failed; retrying once (idempotent)\n' >&2
        sleep 15
        seed_ecr_public_images
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
    seed_ecr_public_images
    apprafter cluster-bootstrap || {
        printf '  cluster-bootstrap (Cilium-on) failed; retrying once (idempotent)\n' >&2
        sleep 20
        seed_ecr_public_images
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

# ---------------------------------------------------------------
# dump_diagnostics helpers (_diag_*). Internal; see dump_diagnostics.
# ---------------------------------------------------------------

# The Display of operator_core::deadline::ReconcileTimedOut
# (operator/operator-core/src/deadline.rs): what the operator logs when a
# reconcile runs out of its WI-400 deadline. Every controller's run() stream
# logs it as "reconciler for object <ref> failed: <this> <N>s", and most
# error_policies log it again. scripts/check-dump-diagnostics.sh fails when
# this drifts from the Rust text, so the summary cannot go silently empty.
_DIAG_DEADLINE_TEXT='reconcile did not finish within'

# _diag_strip_ansi — drop ANSI colour sequences. The operator and the
# webhook colour their logs with no TTY attached, and a downloaded job
# log renders every code as literal `^[[2m` text: about a third of the
# bytes of each operator line.
_diag_strip_ansi() {
    sed $'s/\033\\[[0-9;]*[A-Za-z]//g'
}

# _diag_log_window — the one `kubectl logs` argument that selects THIS
# walk: --since-time=<walk start minus 60s>, in RFC3339. The slack absorbs
# clock skew against a remote node (the Hetzner walks); kind and k3d nodes
# read this host's clock. When START_NS is not a number, or `date` cannot
# convert it (BSD date has no `-d @`), the window is the whole container
# log (--tail=-1) — never a tail.
_diag_log_window() {
    local start_s stamp=''
    case "${START_NS:-}" in
        '' | *[!0-9]*) printf '%s\n' '--tail=-1'; return 0 ;;
    esac
    start_s=$(( START_NS / 1000000000 - 60 ))
    stamp="$(date -u -d "@${start_s}" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)" || stamp=''
    if [ -n "$stamp" ]; then
        printf -- '--since-time=%s\n' "$stamp"
    else
        printf '%s\n' '--tail=-1'
    fi
}

# _diag_fetch_log <ns> <pod> <container> <previous: true|false> <window> <file>
#   One container's log, ANSI-stripped, into <file>. kubectl's own error
#   (a container still waiting, an apiserver gone) lands in the file as
#   well, so a section that looks empty still says why.
_diag_fetch_log() {
    kubectl -n "$1" logs "$2" -c "$3" --previous="$4" "$5" 2>&1 \
        | _diag_strip_ansi >"$6" || true
}

# _diag_print_log <file> <title> <artifact dir, or empty>
#   The last APPRAFTER_E2E_DIAG_CONSOLE_LINES (default 2000) lines of
#   <file> to stderr, saying how many lines were left out and where the
#   whole log is.
_diag_print_log() {
    local file="$1" title="$2" out="$3" keep total
    keep="${APPRAFTER_E2E_DIAG_CONSOLE_LINES:-2000}"
    case "$keep" in '' | *[!0-9]*) keep=2000 ;; esac
    total="$(wc -l <"$file" 2>/dev/null | tr -d ' ')" || total=0
    case "$total" in '' | *[!0-9]*) total=0 ;; esac
    printf '\n=== %s (%s line(s)) ===\n' "$title" "$total" >&2
    if [ "$total" -gt "$keep" ]; then
        if [ -n "$out" ]; then
            printf '... %s earlier line(s) omitted here; the whole log is %s in the e2e-diagnostics artifact\n' \
                "$(( total - keep ))" "${file#"$out"/}" >&2
        else
            printf '... %s earlier line(s) omitted here; set APPRAFTER_E2E_DIAG_DIR to keep the whole log\n' \
                "$(( total - keep ))" >&2
        fi
    fi
    tail -n "$keep" "$file" >&2 || true
}

# _diag_control_plane_logs <dir> <window> <artifact dir, or empty>
#   Fetch the log of every container of every pod of the operator and the
#   webhook deployments into <dir>/apprafter-system/ — and the previous
#   instance of each container that restarted — then print each one's
#   tail. Pods, not `deploy/<name>`: `kubectl logs deploy/x` reads ONE pod,
#   which during a rollout may be the wrong one. The resourceclaim
#   scheduler, provisioner and GC are tasks inside the operator binary,
#   not deployments of their own.
_diag_control_plane_logs() {
    local dir="$1/apprafter-system" window="$2" out="$3" dep sel pod ctr restarts f i
    local -a logs=() titles=()
    mkdir -p "$dir" 2>/dev/null || return 0
    for dep in apprafter-operator admission-webhook; do
        # $k and $v are go-template variables, not shell ones.
        # shellcheck disable=SC2016
        sel="$(kubectl -n apprafter-system get deploy "$dep" \
            -o go-template='{{range $k, $v := .spec.selector.matchLabels}}{{$k}}={{$v}},{{end}}' \
            2>/dev/null)" || continue
        sel="${sel%,}"
        [ -n "$sel" ] || continue
        for pod in $(kubectl -n apprafter-system get pods -l "$sel" \
            -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}' 2>/dev/null || true); do
            while read -r ctr restarts; do
                [ -n "$ctr" ] || continue
                case "$restarts" in '' | *[!0-9]*) restarts=0 ;; esac
                if [ "$restarts" -gt 0 ]; then
                    f="$dir/${pod}.${ctr}.previous.log"
                    _diag_fetch_log apprafter-system "$pod" "$ctr" true "$window" "$f"
                    logs+=("$f")
                    titles+=("logs apprafter-system/${pod} [${ctr}] PREVIOUS instance (restarted ${restarts}x)")
                fi
                f="$dir/${pod}.${ctr}.log"
                _diag_fetch_log apprafter-system "$pod" "$ctr" false "$window" "$f"
                logs+=("$f")
                titles+=("logs apprafter-system/${pod} [${ctr}]")
            done < <(kubectl -n apprafter-system get pod "$pod" \
                -o jsonpath='{range .status.containerStatuses[*]}{.name}{" "}{.restartCount}{"\n"}{end}' \
                2>/dev/null || true)
        done
    done
    printf '\n--- apprafter-system control-plane logs (%s) ---\n' "$window" >&2
    if [ "${#logs[@]}" -eq 0 ]; then
        printf '(no operator or webhook pod found)\n' >&2
        return 0
    fi
    # Deadline hits first, from the WHOLE logs, however far back they are
    # and whatever the console tail below cuts. Each names the object whose
    # reconcile hung, in kube-runtime's <Kind>.<version>.<group>/<name>.<ns>
    # form: the run() stream's WARN reads "reconciler for object <ref>
    # failed: …", and an error_policy WARN sits inside the span
    # `reconciling object{object.ref=<ref>}`. A hit usually shows twice.
    printf '\n--- reconcile deadline hits (whole walk) ---\n' >&2
    if ! grep -hF -- "$_DIAG_DEADLINE_TEXT" "${logs[@]}" >&2 2>/dev/null; then
        printf '(none)\n' >&2
    fi
    for i in "${!logs[@]}"; do
        _diag_print_log "${logs[$i]}" "${titles[$i]}" "$out"
    done
}

# _diag_artifact_dir — create and print this call's own subdirectory of
#   APPRAFTER_E2E_DIAG_DIR: "<kube-context>-<UTC time>-XXXXXX". One per
#   call, so the two clusters of a backup walk and the two runs of the
#   gateway job never overwrite each other. Prints nothing when the
#   variable is unset, or (with a warning) when the directory cannot be
#   made; the dump then goes to the console only.
_diag_artifact_dir() {
    local base="${APPRAFTER_E2E_DIAG_DIR:-}" ctx dir
    [ -n "$base" ] || return 0
    ctx="$(kubectl config current-context 2>/dev/null)" || ctx=''
    ctx="${ctx//[!A-Za-z0-9._-]/_}"
    [ -n "$ctx" ] || ctx=cluster
    if ! mkdir -p "$base" 2>/dev/null \
        || ! dir="$(mktemp -d "${base}/${ctx}-$(date -u +%Y%m%dT%H%M%SZ)-XXXXXX" 2>/dev/null)"; then
        printf 'WARN: cannot create a diagnostics directory under %s; the dump goes to the console only\n' "$base" >&2
        return 0
    fi
    printf '%s\n' "$dir"
}

# ---------------------------------------------------------------
# dump_diagnostics
#   Best-effort cluster-state dump for CI debugging. Call this on
#   failure BEFORE tearing the cluster down — otherwise the evidence
#   (stuck pods, events, helm-hook state) is destroyed with it.
#   No-op + never fails if KUBECONFIG/kubectl are unavailable, and
#   never fails under the caller's `set -euo pipefail` either: most
#   walks call it bare from their EXIT trap, where one failing command
#   would skip the teardown.
#
#   The operator and webhook logs cover the WHOLE walk: every
#   container of every pod of the two deployments, read --since-time
#   the walk started (START_NS), plus the previous instance of every
#   container that restarted. The console shows the last
#   APPRAFTER_E2E_DIAG_CONSOLE_LINES (default 2000) lines of each.
#   It used to show the last 120, and on run 37001547817 (needs-redis
#   nightly, 2026-10-02) those began 30s after the stalled Application
#   was created: steady-state requeue lines had pushed every line about
#   it out of the window. scripts/check-dump-diagnostics.sh guards this.
#
#   APPRAFTER_E2E_DIAG_DIR — when set, each call ALSO writes a fresh
#   subdirectory of it (see _diag_artifact_dir) holding what the console
#   has to cut: the whole control-plane logs, the whole logs of every
#   not-Ready pod, every event, and every apprafter.io and Argo CD
#   object as YAML. The e2e workflows set it and upload it as the run's
#   `e2e-diagnostics-*` artifact when the job fails.
# ---------------------------------------------------------------
dump_diagnostics() {
    command -v kubectl >/dev/null 2>&1 || return 0
    [ -n "${KUBECONFIG:-}" ] || return 0
    local window out='' work='' res
    window="$(_diag_log_window)"
    out="$(_diag_artifact_dir)" || out=''
    if [ -n "$out" ]; then
        work="$out"
    else
        work="$(mktemp -d 2>/dev/null)" || work=''
    fi
    printf '\n----- cluster diagnostics (failure) -----\n' >&2
    if [ -n "$out" ]; then
        printf 'whole logs, events and objects: %s (the e2e-diagnostics artifact)\n' "$out" >&2
        {
            printf 'walk: %s\n' "$0"
            printf 'kube context: %s\n' "$(kubectl config current-context 2>/dev/null || true)"
            printf 'log window: %s\n' "$window"
            printf 'dumped at: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        } >"$out/context.txt" 2>/dev/null || true
    fi
    kubectl get nodes -o wide >&2 2>&1 || true
    printf '\n--- pods (all namespaces) ---\n' >&2
    kubectl get pods -A -o wide >&2 2>&1 || true
    printf '\n--- not-Ready pods (describe + logs) ---\n' >&2
    # Catch pods that are not-Running/Completed AND Running-but-not-
    # fully-ready (e.g. a crash-looping `0/1 Running` cilium-agent —
    # READY ratio r[1] != r[2]). For each, dump describe + current and
    # previous-instance container logs (the crash reason lives there).
    # The trailing `|| true` is load-bearing: with the apiserver gone the
    # `kubectl get` fails, pipefail fails the pipe, and errexit used to end
    # the walk's EXIT trap right here — no teardown, and the walk's own
    # exit code replaced by 1.
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
            if [ -n "$out" ] && mkdir -p "$out/pods/$ns" 2>/dev/null; then
                kubectl -n "$ns" logs "$pod" --all-containers --prefix "$window" 2>&1 \
                    | _diag_strip_ansi >"$out/pods/$ns/$pod.log" || true
                kubectl -n "$ns" logs "$pod" --all-containers --prefix --previous "$window" 2>&1 \
                    | _diag_strip_ansi >"$out/pods/$ns/$pod.previous.log" || true
            fi
        done || true
    # apprafter-system control-plane logs ALWAYS — the operator and
    # admission-webhook run 1/1 Ready, so the not-Ready loop above skips
    # them, yet a reconcile that errors before writing any status (empty
    # `.status.phase`) leaves its only trace in the operator log. Dump
    # the full control-plane regardless of Ready state.
    if [ -n "$work" ]; then
        _diag_control_plane_logs "$work" "$window" "$out"
    else
        printf '\n--- apprafter-system control-plane logs: no scratch directory (mktemp -d failed), skipped ---\n' >&2
    fi
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
    if [ -n "$out" ]; then
        kubectl get events -A --sort-by=.lastTimestamp >"$out/events.txt" 2>&1 || true
        tail -60 "$out/events.txt" >&2 || true
        printf '(the last 60; every event is in events.txt in the artifact)\n' >&2
    else
        kubectl get events -A --sort-by=.lastTimestamp 2>/dev/null | tail -60 >&2 || true
    fi
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
    # Every apprafter.io object and every Argo CD Application in full: the
    # console above carries only the fields a wait loop usually needs.
    if [ -n "$out" ] && mkdir -p "$out/objects" 2>/dev/null; then
        for res in $(kubectl api-resources --api-group=apprafter.io -o name 2>/dev/null || true) \
            applications.argoproj.io; do
            kubectl get "$res" -A -o yaml >"$out/objects/${res}.yaml" 2>&1 || true
        done
    fi
    printf '%s\n' '----- end diagnostics -----' >&2
    if [ -n "$work" ] && [ -z "$out" ]; then
        rm -rf "$work"
    fi
    return 0
}
