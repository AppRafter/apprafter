#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter PROBES e2e — the 2.28 track-A chain on a local kind cluster
# (ADR 0065 §1).
#
# What this proves that no unit test can
# --------------------------------------
# The renderer's probe output is unit-pinned to the byte. What is not, and
# cannot be, is what the KUBELET does with it. Every assertion below is about
# an effect on a running pod:
#
#   1. An exposed workload that declares no probes still gets a readiness
#      probe — the TCP default (§1.3).
#   2. Readiness GATES TRAFFIC: while the process has not bound, the Service
#      has no endpoint for the pod. Asserted on `Endpoints`, not on the pod
#      condition — the condition is what we set, the endpoint is what it is
#      FOR, and only the second one is the promise to a caller.
#   3. A declared liveness probe restarts a wedged process (restartCount 0→≥1)
#      while readiness on a different path keeps passing — so the restart is
#      attributable to liveness and not to a crash.
#   4. The DERIVED startup probe (§1.4) rescues a slow starter that a bare
#      liveness probe would kill. Run as a CONTRAST: the same image, the same
#      delay, once with the derived budget and once with an explicit tiny
#      startup probe. Without the second half the phase would prove only that
#      the application was fast enough.
#   5. `readiness: {enabled: false}` removes the default rather than being
#      ignored.
#
# What it deliberately does NOT cover
# -----------------------------------
# The `apprafter app status` probes line. That command resolves an application
# through its Argo CD registration, which this walk has no way to create (see
# below), so invoking it here would test nothing. It is unit-covered instead:
# eight tests in `cli/platform-cli/src/commands/app.rs`, one of which reads
# `operator-rendering/src/probes.rs` and fails if the CLI's copy of the
# defaults drifts from the renderer's. Its cluster-side rendering rides the
# next full bootstrap walk.
#
# Deliberately NOT a full platform bootstrap
# ------------------------------------------
# Every other operator walk runs `apprafter cluster-bootstrap`, because it is
# testing something that rides the GitOps path (Argo CD → cue-cmp → operator).
# Probes ride none of it: the feature is `render_deployment` plus the kubelet.
# So this walk installs the operator chart directly onto a bare kind cluster
# and applies `Application` CRs with kubectl. Two consequences, both accepted
# and both stated rather than discovered:
#
#   * The ADMISSION WEBHOOK is not installed (its chart requires cert-manager,
#     which requires the bootstrap). The five webhook rules of §1.5 are
#     covered by unit tests, and the one rule that also exists at the
#     apiserver — the `^/` pattern on `path` — is proved on a real apiserver
#     by `scripts/validate-crds.sh`. Nothing here depends on the webhook.
#   * No CLI command is exercised: `app add` needs a git remote and Argo CD,
#     and every other `app` verb resolves through the registration `app add`
#     creates. The GitOps registration path is covered by `gitops-walk.sh`
#     and is orthogonal to what a probe does.
#
# It also means this walk runs under ROOTLESS podman, which the Cilium-based
# walks cannot: no CNI beyond kindnet is needed.
#
# The workload image
# ------------------
# `#ApplicationSpec` has no `command`/`args` — a manifest chooses an image and
# an environment, nothing more. So the behaviours under test are baked into a
# tiny image built by this walk and selected per-app through `spec.base.env`:
#
#   START_DELAY  seconds to wait before binding :8080   (slow starter)
#   WEDGE_AFTER  seconds after binding to stop serving /livez while /healthz
#                keeps answering                        (a wedged process)
#
# Usage:
#   bash e2e/probes-walk.sh
#
# JUDGE THIS WALK BY READING ITS LOG, not by its exit code — a sandboxed
# runner can mask the inner status. Every phase prints `ok:` lines; the final
# line is GREEN or the script has already printed FAILED and exited non-zero.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export REPO_ROOT
# shellcheck source=/dev/null
. "${REPO_ROOT}/e2e/lib.sh"

CLUSTER_NAME="${APPRAFTER_E2E_CLUSTER:-apprafter-probes}"
export CLUSTER_NAME
NS="probes"
APP_IMAGE="docker.io/library/apprafter-probe-app:walk"
CLUSTER_UP=0

fail() {
    printf 'FAILED: %s\n' "$1" >&2
    if [ "$CLUSTER_UP" = "1" ]; then
        printf '\n--- diagnostics ---\n' >&2
        kubectl -n "$NS" get pods -o wide >&2 2>&1 || true
        kubectl -n "$NS" get deploy -o yaml >&2 2>&1 | head -120 || true
        kubectl -n apprafter-system logs deploy/apprafter-operator --tail=60 >&2 2>&1 || true
    fi
    exit 1
}

cleanup() {
    if [ "${APPRAFTER_E2E_KEEP:-0}" = "1" ]; then
        printf '\nAPPRAFTER_E2E_KEEP=1 — leaving cluster %s up.\n' "$CLUSTER_NAME"
        return
    fi
    if [ "$CLUSTER_UP" = "1" ]; then
        printf '\n--- tearing down cluster %s ---\n' "$CLUSTER_NAME"
        _kind delete cluster --name "$CLUSTER_NAME" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

# ---------------------------------------------------------------
# Apply one Application and wait for its Deployment to exist.
#   app_apply <name> <yaml-body-on-stdin>
# ---------------------------------------------------------------
app_apply() {
    local name="$1"
    kubectl apply -f - >/dev/null || fail "applying Application $name"
    local i
    for i in $(seq 1 60); do
        if kubectl -n "$NS" get deploy "$name" >/dev/null 2>&1; then
            return 0
        fi
        sleep 2
    done
    fail "the operator never rendered a Deployment for $name"
}

# Read one jsonpath off the app's container. Empty string when absent.
container_field() { # <deployment> <jsonpath-after-containers[0]>
    kubectl -n "$NS" get deploy "$1" \
        -o "jsonpath={.spec.template.spec.containers[0].$2}" 2>/dev/null || true
}

# =================================================================
phase "0/8  preflight"
# =================================================================
for bin in kubectl helm kind cargo; do
    # kind/helm may be shimmed under ~/bin per this repo's convention.
    command -v "$bin" >/dev/null 2>&1 || [ -x "$HOME/bin/$bin" ] \
        || fail "missing required tool: $bin"
done
command -v podman >/dev/null 2>&1 || command -v docker >/dev/null 2>&1 \
    || fail "need podman or docker"
printf '  ok: tooling present\n'

# =================================================================
phase "1/8  kind cluster"
# =================================================================
_kind delete cluster --name "$CLUSTER_NAME" >/dev/null 2>&1 || true
_kind_up "$CLUSTER_NAME"
CLUSTER_UP=1
kubectl create namespace "$NS" >/dev/null
printf '  ok: cluster up, namespace %s created\n' "$NS"

# =================================================================
phase "2/8  operator from the working tree"
# =================================================================
# The chart carries its CRDs as TEMPLATES, not under `crds/`, so helm owns
# them and `apply_branch_operator_crds` must NOT run first: a pre-applied CRD
# has no helm ownership annotations and the install refuses to adopt it.
helm install apprafter-operator "${REPO_ROOT}/operator/charts/apprafter-operator" \
    --namespace apprafter-system --create-namespace \
    --timeout 180s >/dev/null \
    || fail "installing the operator chart"
kubectl wait --for=condition=Established --timeout=60s \
    crd/applications.apprafter.io >/dev/null || fail "the Application CRD never Established"
printf '  operator chart installed (CRDs owned by the release)\n'

OPERATOR_IMAGE=$(kubectl -n apprafter-system get deploy apprafter-operator \
    -o jsonpath='{.spec.template.spec.containers[0].image}')
[ -n "$OPERATOR_IMAGE" ] || fail "could not read the operator image ref off the Deployment"
branch_image_build apprafter-operator "$OPERATOR_IMAGE"
cluster_load_image "$CLUSTER_NAME" "$OPERATOR_IMAGE"
kubectl -n apprafter-system rollout restart deploy/apprafter-operator >/dev/null
kubectl -n apprafter-system rollout status deploy/apprafter-operator --timeout=240s >/dev/null \
    || fail "the working-tree operator never became ready"
printf '  ok: working-tree operator running (%s)\n' "$OPERATOR_IMAGE"

# =================================================================
phase "3/8  the walk's workload image"
# =================================================================
BUILDER=podman
command -v podman >/dev/null 2>&1 || BUILDER=docker
WORKDIR=$(mktemp -d)
cat >"${WORKDIR}/entrypoint.sh" <<'SH'
#!/bin/sh
# Behaviour is chosen by env, because an AppRafter manifest selects an image
# and an environment and nothing else.
set -eu
mkdir -p /www
echo ok > /www/healthz
echo ok > /www/livez
if [ "${WEDGE_AFTER:-0}" -gt 0 ]; then
    # Stop serving /livez while /healthz keeps answering: a wedged process,
    # not a crashed one. Only a liveness probe can notice this.
    ( sleep "${WEDGE_AFTER}"; rm -f /www/livez ) &
fi
# Nothing is listening during this window — the readiness probe's whole job.
sleep "${START_DELAY:-0}"
exec busybox httpd -f -p 8080 -h /www
SH
cat >"${WORKDIR}/Dockerfile" <<'DOCKER'
FROM busybox:1.36
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
ENTRYPOINT ["/bin/sh", "/entrypoint.sh"]
DOCKER
"$BUILDER" build -t "$APP_IMAGE" "$WORKDIR" >/dev/null 2>&1 \
    || fail "building the walk workload image"
cluster_load_image "$CLUSTER_NAME" "$APP_IMAGE"
printf '  ok: %s built and side-loaded\n' "$APP_IMAGE"

# =================================================================
phase "4/8  an exposed workload with NO probes gets the TCP default"
# =================================================================
app_apply plain <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: {name: plain, namespace: ${NS}}
spec:
  base:
    image: ${APP_IMAGE}
    expose: {port: 8080}
YAML
TCP_PORT=$(container_field plain 'readinessProbe.tcpSocket.port')
[ "$TCP_PORT" = "8080" ] \
    || fail "expected a default TCP readiness probe on 8080, got '${TCP_PORT}'"
[ -z "$(container_field plain 'livenessProbe')" ] \
    || fail "a liveness probe was invented for an app that declared none"
[ -z "$(container_field plain 'startupProbe')" ] \
    || fail "a startup probe was invented with no liveness to derive from"
printf '  ok: readiness tcp :8080 rendered; no liveness, no startup invented\n'

# =================================================================
phase "5/8  readiness gates TRAFFIC, not just a condition"
# =================================================================
app_apply slow <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: {name: slow, namespace: ${NS}}
spec:
  base:
    image: ${APP_IMAGE}
    expose: {port: 8080}
    env: {START_DELAY: "45"}
    probes:
      readiness: {path: /healthz, periodSeconds: 2}
YAML
# While the process has not bound, the Service must have NO endpoint. This is
# the assertion that matters: the pod condition is what we set, the endpoint
# is what it is for.
sleep 20
EP=$(kubectl -n "$NS" get endpoints slow -o jsonpath='{.subsets[*].addresses[*].ip}' 2>/dev/null || true)
[ -z "$EP" ] || fail "the Service already has an endpoint (${EP}) while the process is still sleeping"
printf '  ok: no Service endpoint while the process has not bound\n'
for i in $(seq 1 60); do
    EP=$(kubectl -n "$NS" get endpoints slow -o jsonpath='{.subsets[*].addresses[*].ip}' 2>/dev/null || true)
    [ -n "$EP" ] && break
    sleep 3
done
[ -n "$EP" ] || fail "the pod never became an endpoint after it bound"
printf '  ok: the endpoint appears once the process binds (%s)\n' "$EP"

# =================================================================
phase "6/8  liveness restarts a WEDGED process"
# =================================================================
app_apply wedge <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: {name: wedge, namespace: ${NS}}
spec:
  base:
    image: ${APP_IMAGE}
    expose: {port: 8080}
    env: {WEDGE_AFTER: "20"}
    probes:
      readiness: {path: /healthz, periodSeconds: 2}
      liveness:  {path: /livez, periodSeconds: 2, failureThreshold: 2}
      startup:   {path: /healthz, periodSeconds: 2, failureThreshold: 30}
YAML
RESTARTS=0
for i in $(seq 1 60); do
    RESTARTS=$(kubectl -n "$NS" get pods -l apprafter.io/application=wedge \
        -o jsonpath='{.items[0].status.containerStatuses[0].restartCount}' 2>/dev/null || echo 0)
    [ "${RESTARTS:-0}" -ge 1 ] && break
    sleep 3
done
[ "${RESTARTS:-0}" -ge 1 ] \
    || fail "the wedged process was never restarted (restartCount=${RESTARTS}); liveness did nothing"
printf '  ok: liveness restarted the wedged process (restartCount=%s) while /healthz kept serving\n' "$RESTARTS"

# =================================================================
phase "7/8  the DERIVED startup probe rescues a slow starter (contrast)"
# =================================================================
# Half 1: liveness only. Its own budget is 2s x 2 = 4s, far shorter than the
# 40s start — so WITHOUT the derived startup probe the kubelet would kill it.
app_apply derived <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: {name: derived, namespace: ${NS}}
spec:
  base:
    image: ${APP_IMAGE}
    expose: {port: 8080}
    env: {START_DELAY: "40"}
    probes:
      liveness: {path: /livez, periodSeconds: 2, failureThreshold: 2}
YAML
DERIVED_FT=$(container_field derived 'startupProbe.failureThreshold')
[ "$DERIVED_FT" = "60" ] \
    || fail "expected a derived startup probe with failureThreshold 60, got '${DERIVED_FT}'"
DERIVED_PERIOD=$(container_field derived 'startupProbe.periodSeconds')
[ "$DERIVED_PERIOD" = "5" ] \
    || fail "the derived startup probe inherited the liveness period instead of its own (got '${DERIVED_PERIOD}')"
printf '  ok: startup derived with its own cadence (period %ss, threshold %s)\n' \
    "$DERIVED_PERIOD" "$DERIVED_FT"

READY=""
for i in $(seq 1 60); do
    READY=$(kubectl -n "$NS" get pods -l apprafter.io/application=derived \
        -o jsonpath='{.items[0].status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || true)
    [ "$READY" = "True" ] && break
    sleep 3
done
DR=$(kubectl -n "$NS" get pods -l apprafter.io/application=derived \
    -o jsonpath='{.items[0].status.containerStatuses[0].restartCount}' 2>/dev/null || echo "?")
[ "$READY" = "True" ] || fail "the slow starter never became Ready — the derived startup probe did not protect it"
[ "$DR" = "0" ] || fail "the slow starter was restarted ${DR} time(s) despite the derived startup probe"
printf '  ok: slow starter reached Ready with restartCount 0\n'

# Half 2 — the control. Same image, same delay, an EXPLICIT tiny startup probe
# that wins outright over the derivation. It must be killed; otherwise half 1
# proved only that the application was fast enough.
app_apply killed <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: {name: killed, namespace: ${NS}}
spec:
  base:
    image: ${APP_IMAGE}
    expose: {port: 8080}
    env: {START_DELAY: "40"}
    probes:
      liveness: {path: /livez, periodSeconds: 2, failureThreshold: 2}
      startup:  {path: /livez, periodSeconds: 1, failureThreshold: 3}
YAML
KR=0
for i in $(seq 1 40); do
    KR=$(kubectl -n "$NS" get pods -l apprafter.io/application=killed \
        -o jsonpath='{.items[0].status.containerStatuses[0].restartCount}' 2>/dev/null || echo 0)
    [ "${KR:-0}" -ge 1 ] && break
    sleep 3
done
[ "${KR:-0}" -ge 1 ] \
    || fail "the explicit tiny startup probe did NOT kill the slow starter, so phase 7 half 1 proves nothing"
printf '  ok: control — an explicit tiny startup probe kills the same app (restartCount=%s)\n' "$KR"

# =================================================================
phase "8/8  enabled:false removes the default rather than being ignored"
# =================================================================
app_apply optout <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: {name: optout, namespace: ${NS}}
spec:
  base:
    image: ${APP_IMAGE}
    expose: {port: 8080}
    probes:
      readiness: {enabled: false}
YAML
[ -z "$(container_field optout 'readinessProbe')" ] \
    || fail "readiness: {enabled: false} was ignored — the default probe is still on the container"
printf '  ok: no readinessProbe on the container\n'

printf '\nGREEN — probes walk passed every phase.\n'
