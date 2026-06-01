#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter GitOps-walk e2e — CMP render → Argo sync → operator
# reconcile (plan item 1.81).
#
# This script exercises the full GitOps loop on a local k3d cluster:
#
#   1. k3d_up (trap k3d_down on EXIT)
#   2. apprafter cluster-bootstrap → Cilium + Argo CD + operator +
#      CMP sidecar + AppProjects reconciling from platform-stack OCI.
#   3. Spin up a local git daemon serving the in-repo fixture at
#      e2e/fixtures/gitops-app/ — Argo CD pods reach it via the
#      Docker/Podman bridge gateway IP.
#   4. Register the fixture as an Argo CD Application
#      (apprafter app add … --no-ping --no-interactive).
#   5. ASSERT: Argo CD Application exists and syncs, the CMP renders
#      an AppRafter Application CR, the operator reconciles it to an
#      Available Deployment.
#   6. Push a change to the fixture (replicas: 1 → 2), re-sync,
#      ASSERT the Deployment converges.
#
# Fixture repo approach
# ---------------------
# The fixture lives at e2e/fixtures/gitops-app/ (tracked in the
# monorepo). At runtime the script:
#   a) git-init a bare clone of that subtree at /tmp/e2e-git-repos/
#   b) runs `git daemon` on the host (port 9418)
#   c) detects the Docker/Podman bridge gateway IP so that Argo CD
#      pods inside k3d can clone via git://GATEWAY_IP:9418/gitops-app
#
# CLI state injection
# -------------------
# apprafter cluster-bootstrap + app add read the kubeconfig from the
# CLI's per-target state store (not from $KUBECONFIG). We set
# APPRAFTER_CONFIG_DIR to a tmpdir and seed it with a minimal
# config.yaml (active_target: k3d) and a state.json carrying the k3d
# kubeconfig as kubeconfig_yaml (plaintext) so the CLI binds to the
# right cluster without an age-encrypted secret.
#
# Required: docker (or podman aliased to docker), git, cargo, kubectl
#   — all satisfied inside `nix develop` or on a standard CI runner.
#
# Exit codes:
#   0 — loop green
#   1 — assertion failure
#   2 — precondition missing

set -euo pipefail

# ---------------------------------------------------------------
# Source shared helpers
# ---------------------------------------------------------------

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-e2e-gitops"
FIXTURE_SRC="${REPO_ROOT}/e2e/fixtures/gitops-app"
APP_NAME="gitops-app"
APP_NS="apprafter"
GIT_DAEMON_PORT="9418"

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------

for tool in git cargo kubectl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done

# docker or podman (podman is typically aliased to docker in CI)
if ! command -v docker >/dev/null 2>&1; then
    printf 'ERROR: "docker" (or podman aliased as docker) not found on PATH\n' >&2
    exit 2
fi

# ---------------------------------------------------------------
# Temp workspace
# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
GIT_REPOS_DIR="${TMPDIR_WORK}/git-repos"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"
GIT_DAEMON_PID=""

cleanup() {
    local exit_code=$?

    # Kill the git daemon first so the port is free
    if [ -n "$GIT_DAEMON_PID" ] && kill -0 "$GIT_DAEMON_PID" 2>/dev/null; then
        kill "$GIT_DAEMON_PID" 2>/dev/null || true
    fi

    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! gitops-walk FAILED at %s (exit %d) !!!\n' \
            "$(elapsed)" "$exit_code" >&2
        dump_diagnostics
        printf 'Tearing down k3d cluster (set APPRAFTER_E2E_SKIP_DESTROY=1 to keep).\n' >&2
    fi

    if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
        k3d_down "$CLUSTER_NAME" || true
    else
        printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving k3d cluster %s up.\n' \
            "$CLUSTER_NAME"
        printf 'Run: k3d cluster delete %s\n' "$CLUSTER_NAME"
    fi

    rm -rf "$TMPDIR_WORK"
    exit "$exit_code"
}
trap cleanup EXIT

# ---------------------------------------------------------------
# Helper: apprafter CLI with cwd = fixture src dir
#
# The lib.sh `apprafter()` function always cd's to REPO_ROOT/cli
# before invoking cargo run, which means std::env::current_dir()
# inside the binary is REPO_ROOT/cli. `apprafter app add` uses the
# cwd to locate apprafter/Application.cue for the scaffold-step
# gate. We override with --manifest-path so cargo picks up the
# workspace from the correct Cargo.toml while the binary's cwd is
# the fixture directory (which has apprafter/Application.cue).
# ---------------------------------------------------------------
apprafter_from_fixture() {
    (
        cd "${FIXTURE_SRC}"
        cargo run \
            --manifest-path "${REPO_ROOT}/cli/Cargo.toml" \
            --quiet \
            --bin apprafter \
            -- "$@"
    )
}

# ---------------------------------------------------------------
# Helper: seed the CLI state store
#
# Writes:
#   $APPRAFTER_CONFIG_DIR/config.yaml      — active_target: k3d
#   $APPRAFTER_CONFIG_DIR/state/k3d/.apprafter/state.json
#     hetzner_cloud.kubeconfig_yaml = <k3d kubeconfig plaintext>
#     hetzner_cloud.server_id = 0  (dummy — not used by bootstrap)
#     hetzner_cloud.server_name = "k3d-local"
#
# Called after the k3d cluster is up and the kubeconfig is fetched.
# ---------------------------------------------------------------
seed_apprafter_state() {
    local kubeconfig_content="$1"

    mkdir -p "${APPRAFTER_CONFIG_DIR}"
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/k3d/.apprafter"

    # Global config — sets the active target name to "k3d".
    # Fields mirror GlobalConfig in cli-core/src/target.rs:
    #   active_target: String, version: u32
    cat >"${APPRAFTER_CONFIG_DIR}/config.yaml" <<'YAML'
active_target: k3d
version: 1
YAML

    # Escape the kubeconfig for JSON embedding (replace newlines
    # with \n, escape double-quotes and backslashes)
    local kc_escaped
    kc_escaped=$(printf '%s' "$kubeconfig_content" \
        | sed 's/\\/\\\\/g' \
        | sed 's/"/\\"/g' \
        | awk '{printf "%s\\n", $0}')

    # State file — the CLI reads hetzner_cloud.kubeconfig_yaml
    # (or kubeconfig_age when present; plaintext is the fallback).
    # server_id / server_name are required fields in the Rust
    # struct and must be non-zero / non-empty for deserialization.
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
# Helper: init the fixture git repo and start git daemon
# ---------------------------------------------------------------
setup_git_server() {
    local repo_dst="${GIT_REPOS_DIR}/gitops-app"

    mkdir -p "${GIT_REPOS_DIR}"

    # Create a proper git repo from the in-repo fixture
    cp -r "${FIXTURE_SRC}" "${repo_dst}"
    (
        cd "${repo_dst}"
        git init -b main
        git config user.email "e2e@apprafter.io"
        git config user.name "AppRafter E2E"
        git add .
        git commit -m "feat: initial gitops-app fixture"
    )
    # Mark as exportable by git-daemon
    touch "${repo_dst}/.git/git-daemon-export-ok"

    # Start git daemon on all interfaces, --base-path lets Argo CD
    # clone as git://HOST:9418/gitops-app (without the full path)
    git daemon \
        --reuseaddr \
        --base-path="${GIT_REPOS_DIR}" \
        --export-all \
        --port="${GIT_DAEMON_PORT}" \
        --detach \
        "${GIT_REPOS_DIR}"

    # Capture PID for cleanup
    GIT_DAEMON_PID=$(pgrep -f "git daemon.*${GIT_DAEMON_PORT}" | head -1 || true)
    printf '  git daemon started (port %s, base %s)\n' \
        "$GIT_DAEMON_PORT" "$GIT_REPOS_DIR"
}

# ---------------------------------------------------------------
# Helper: detect gateway IP of the k3d Docker/Podman network so
# that pods inside k3d can reach the host git daemon.
# ---------------------------------------------------------------
detect_host_gateway_ip() {
    local net_name="k3d-${CLUSTER_NAME}"
    local gw
    gw=$(docker network inspect "$net_name" \
        --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}' 2>/dev/null \
        | head -1)
    if [ -z "$gw" ]; then
        printf 'ERROR: could not detect gateway IP of Docker network %s\n' \
            "$net_name" >&2
        printf 'Ensure k3d is up and docker network inspect is available.\n' >&2
        exit 1
    fi
    printf '%s' "$gw"
}

# ---------------------------------------------------------------
# Helper: wait until the Argo CD Application reaches Synced+Healthy
# ---------------------------------------------------------------
wait_argo_app_synced() {
    local app_name="$1"
    local deadline=$(( $(date +%s) + 600 ))  # 10 min max

    printf '  waiting for Argo CD Application %s to be Synced+Healthy ...\n' \
        "$app_name"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        local sync_status health_status
        sync_status=$(kubectl -n argocd get applications.argoproj.io "$app_name" \
            -o jsonpath='{.status.sync.status}' 2>/dev/null || true)
        health_status=$(kubectl -n argocd get applications.argoproj.io "$app_name" \
            -o jsonpath='{.status.health.status}' 2>/dev/null || true)
        if [ "$sync_status" = "Synced" ] && [ "$health_status" = "Healthy" ]; then
            printf '  Application %s -> Synced + Healthy\n' "$app_name"
            return 0
        fi
        printf '    %s: sync=%s health=%s\n' \
            "$(date +%H:%M:%S)" "$sync_status" "$health_status"
        sleep 10
    done
    printf 'ERROR: Application %s did not reach Synced+Healthy within 10 min\n' \
        "$app_name" >&2
    kubectl -n argocd describe applications.argoproj.io "$app_name" >&2 || true
    return 1
}

# ---------------------------------------------------------------
# Phase 0: bring up the k3d cluster
# ---------------------------------------------------------------

phase "Phase 0: k3d_up ${CLUSTER_NAME}"

k3d_up "$CLUSTER_NAME"

# Export the k3d kubeconfig so kubectl commands below work
k3d_bin="k3d"
command -v k3d >/dev/null 2>&1 || k3d_bin="nix run nixpkgs#k3d --"
$k3d_bin kubeconfig write "$CLUSTER_NAME" --output "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

# ---------------------------------------------------------------
# Phase 1: seed CLI state and run cluster-bootstrap
# ---------------------------------------------------------------

phase "Phase 1: cluster-bootstrap (Cilium → Argo CD → platform-stack)"

# Read the k3d kubeconfig content and inject it into the CLI state
# store so `apprafter cluster-bootstrap` / `app add` can find the
# cluster without a Hetzner Cloud state.
kubeconfig_content=$(cat "$KUBECONFIG_FILE")
seed_apprafter_state "$kubeconfig_content"
export APPRAFTER_CONFIG_DIR

printf '  APPRAFTER_CONFIG_DIR=%s\n' "$APPRAFTER_CONFIG_DIR"
printf '  Running cluster-bootstrap (installs Cilium, Argo CD, platform-stack)...\n'

apprafter cluster-bootstrap

printf '  cluster-bootstrap complete\n'

# ---------------------------------------------------------------
# Phase 2: wait for the platform AppProject to exist
#
# The `apps` AppProject is created by the platform-stack chart
# (chart ≥ 0.1.40). `apprafter app add` writes into that project.
# Poll until the AppProject CR appears in the argocd namespace.
# ---------------------------------------------------------------

phase "Phase 2: wait for 'apps' AppProject and platform components"

printf '  waiting for AppProject apps to appear ...\n'
deadline=$(( $(date +%s) + 600 ))  # 10 min
while [ "$(date +%s)" -lt "$deadline" ]; do
    if kubectl -n argocd get appproject.argoproj.io apps \
            >/dev/null 2>&1; then
        printf '  AppProject apps -> found\n'
        break
    fi
    sleep 10
done
kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1 || {
    printf 'ERROR: AppProject apps not found after 10 min\n' >&2
    kubectl -n argocd get appproject.argoproj.io >&2 || true
    exit 1
}

# ---------------------------------------------------------------
# Phase 3: set up local git server with the fixture repo
# ---------------------------------------------------------------

phase "Phase 3: git daemon — fixture repo"

setup_git_server

# The repoURL Argo CD (running inside k3d) uses to clone the fixture
# is `host.k3d.internal` — k3d's built-in host alias, injected into
# the cluster CoreDNS, that pods resolve to the host. This is more
# portable than a dynamically-detected Docker bridge gateway IP, which
# is not reliably routable from pods on GitHub Actions runners
# (`detect_host_gateway_ip` is kept as a documented fallback only).
GIT_REPO_URL="git://host.k3d.internal:${GIT_DAEMON_PORT}/gitops-app"
# The host itself cannot resolve `host.k3d.internal`, so the host-side
# liveness check targets the loopback (the git daemon binds 0.0.0.0).
GIT_REPO_URL_HOST="git://127.0.0.1:${GIT_DAEMON_PORT}/gitops-app"
printf '  fixture repo URL (in-cluster): %s\n' "$GIT_REPO_URL"

# Sanity-check: the git daemon should be reachable from the host
printf '  verifying git daemon is up (local clone check)...\n'
retry 6 5 -- git ls-remote "$GIT_REPO_URL_HOST" >/dev/null
printf '  git daemon is reachable\n'

# ---------------------------------------------------------------
# Phase 4: register the fixture as an Argo CD Application
# ---------------------------------------------------------------

phase "Phase 4: apprafter app add (register fixture)"

# Run app add from the fixture source directory so the CLI finds
# apprafter/Application.cue for the scaffold-step gate check.
apprafter_from_fixture app add \
    "$GIT_REPO_URL" \
    --name    "$APP_NAME" \
    --branch  main \
    --path    "/" \
    --namespace "$APP_NS" \
    --project apps \
    --no-ping \
    --no-interactive

printf '  app add complete\n'

# ---------------------------------------------------------------
# Phase 5: assert (a) Argo CD Application, (b) AppRafter CR, (c)
#           Deployment Available
# ---------------------------------------------------------------

phase "Phase 5a: assert Argo CD Application exists"

retry 12 5 -- kubectl -n argocd get applications.argoproj.io "$APP_NAME" \
    >/dev/null
printf '  Argo CD Application %s -> found\n' "$APP_NAME"

wait_argo_app_synced "$APP_NAME"

phase "Phase 5b: assert AppRafter Application CR exists (CMP rendered)"

# The CMP sidecar renders Application.cue → AppRafter Application CR
# which Argo CD SSAs into the cluster. Poll up to 5 min.
deadline=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    if kubectl -n "$APP_NS" get applications.apprafter.io "$APP_NAME" \
            >/dev/null 2>&1; then
        printf '  Application.apprafter.io %s -> found\n' "$APP_NAME"
        break
    fi
    sleep 10
done
kubectl -n "$APP_NS" get applications.apprafter.io "$APP_NAME" >/dev/null 2>&1 || {
    printf 'ERROR: Application.apprafter.io %s not found after 5 min\n' \
        "$APP_NAME" >&2
    printf 'CMP may not have rendered or Argo CD sync failed. Argo CD Application:\n' >&2
    kubectl -n argocd describe applications.argoproj.io "$APP_NAME" >&2 || true
    exit 1
}

phase "Phase 5c: assert operator reconciled Deployment Available"

kubectl wait \
    --for=condition=Available \
    "deployment/${APP_NAME}" \
    --namespace "$APP_NS" \
    --timeout=300s
printf '  Deployment %s -> Available\n' "$APP_NAME"

# ---------------------------------------------------------------
# Phase 6: change detection — bump replicas, re-sync, assert
#
# Modify the fixture's Application.cue in-place (replicas: 1 → 2),
# commit, and let Argo CD's automated sync pick it up. This proves
# the loop reacts to source changes — not just initial sync.
# ---------------------------------------------------------------

phase "Phase 6: source change — bump replicas 1 → 2"

# Edit the fixture repo copy (in the tmpdir, not the tracked source)
FIXTURE_COPY="${GIT_REPOS_DIR}/gitops-app/apprafter/Application.cue"
sed -i 's/replicas: 1/replicas: 2/' "$FIXTURE_COPY"
(
    cd "${GIT_REPOS_DIR}/gitops-app"
    git add apprafter/Application.cue
    git commit -m "chore(e2e): bump replicas 1->2 (change-detection assertion)"
)
printf '  committed replicas bump to git daemon repo\n'

# Trigger Argo CD hard-refresh on our Application to pick up the
# new commit without waiting the full reconcile interval (~3 min).
printf '  triggering Argo CD hard-refresh on Application %s ...\n' "$APP_NAME"
kubectl -n argocd annotate \
    applications.argoproj.io "$APP_NAME" \
    "argocd.argoproj.io/refresh=hard" \
    --overwrite

# Wait for Argo CD to re-sync
wait_argo_app_synced "$APP_NAME"

# Poll until the Deployment shows 2 replicas available
printf '  waiting for Deployment to scale to 2 Available replicas ...\n'
deadline=$(( $(date +%s) + 300 ))  # 5 min
while [ "$(date +%s)" -lt "$deadline" ]; do
    avail=$(kubectl -n "$APP_NS" get deployment/"$APP_NAME" \
        -o jsonpath='{.status.availableReplicas}' 2>/dev/null || true)
    if [ "${avail:-0}" -ge 2 ]; then
        printf '  Deployment %s -> %s replicas Available\n' "$APP_NAME" "$avail"
        break
    fi
    printf '    %s: availableReplicas=%s\n' "$(date +%H:%M:%S)" "${avail:-0}"
    sleep 10
done
avail=$(kubectl -n "$APP_NS" get deployment/"$APP_NAME" \
    -o jsonpath='{.status.availableReplicas}' 2>/dev/null || true)
if [ "${avail:-0}" -lt 2 ]; then
    printf 'ERROR: Deployment %s did not scale to 2 replicas within 5 min\n' \
        "$APP_NAME" >&2
    kubectl -n "$APP_NS" describe deployment/"$APP_NAME" >&2 || true
    exit 1
fi

# ---------------------------------------------------------------
# Done — tear down on success path
# ---------------------------------------------------------------

# Remove the EXIT trap so cleanup() does not fire again after we
# handle tear-down inline here. The trap only fires on abnormal
# paths from here; on success we own the teardown explicitly.
trap - EXIT

# Kill the git daemon
if [ -n "$GIT_DAEMON_PID" ] && kill -0 "$GIT_DAEMON_PID" 2>/dev/null; then
    kill "$GIT_DAEMON_PID" 2>/dev/null || true
fi

if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    k3d_down "$CLUSTER_NAME" || true
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving k3d cluster %s up.\n' \
        "$CLUSTER_NAME"
fi

rm -rf "$TMPDIR_WORK"

printf '\ngitops-walk GREEN in %s\n' "$(elapsed)"
printf 'Loop proven: CMP render -> Argo sync -> operator reconcile\n'
