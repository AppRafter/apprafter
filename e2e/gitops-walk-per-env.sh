#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter per-environment GitOps-walk e2e — the full 2.9 chain on a
# local k3d/kind cluster (plan item 2.9 Task 11, ADR 0044).
#
# This walk proves the 2.9 promise end-to-end: the SAME app repo,
# deployed per environment, yields TWO self-contained Argo CD
# Applications in TWO namespaces, each rendered with its env-resolved
# spec. The chain is:
#
#   apprafter app add --env dev|prod  (CLI: ADR 0044)
#     -> Argo Application `web-<env>` with
#        spec.source.plugin.env = [{APPRAFTER_APP_ENV: <env>}]
#        + labels apprafter.io/application=web, apprafter.io/environment=<env>
#     -> argocd-cue-cmp sidecar reads APPRAFTER_APP_ENV and INJECTS
#        spec.environment=<env> (+ the env label) into the rendered
#        AppRafter Application CR
#     -> operator unifies spec.environments[<env>] onto spec.base and
#        renders the env-resolved Deployment (replicas/env differ per env),
#        and surfaces status.environment=<env>.
#
# Approach
# --------
# APPROACH A (true end-to-end). Besides the operator + admission-webhook,
# this walk ALSO side-loads the WORKING-TREE argocd-cue-cmp image into the
# argocd-repo-server's cue-cmp sidecar. RATIONALE: the per-env injection
# (spec.environment + the env label) lives ONLY in the working-tree
# `argocd-cue-cmp/entrypoint.sh` — the PUBLISHED cue-cmp image that
# cluster-bootstrap installs predates 2.9 and does NOT inject. So a
# faithful e2e must run the working-tree cue-cmp; we build it from
# argocd-cue-cmp/Dockerfile tagged as the EXACT image ref the repo-server's
# cue-cmp sidecar already references, side-load it, and rollout-restart the
# repo-server. The sidecar's plugin.yaml comes from a chart ConfigMap
# (cue-cmp-plugin-config) and is 2.9-agnostic — only entrypoint.sh (shipped
# IN the image) carries the injection, so the image side-load is sufficient.
# (The host-side unit coverage of the injection itself lives in
# argocd-cue-cmp/test-inject.sh.)
#
# Local-operator scaffolding
# --------------------------
# By default this walk runs against the PUBLISHED platform-stack (operator +
# admission-webhook v0.2.22 + argocd-cue-cmp v0.1.8 -- 2.9 shipped, so the
# released chart carries the spec.environment / status.environment schema and
# the ARGOCD_ENV_ injection fix). Set APPRAFTER_E2E_LOCAL_OPERATOR=1 (the
# e2e-per-env Justfile target) to instead build + side-load the working-tree
# operator + admission-webhook + cue-cmp and apply the branch CRDs/RBAC
# (Phase-1b, mirroring needs-disk-walk.sh) -- for pre-release validation of an
# unreleased schema change.
#
# Assertions
# ----------
#   * Two Argo Applications web-dev + web-prod reach Synced + Healthy.
#   * Two AppRafter Application CRs `web` (one in ns web-dev, one in ns
#     web-prod) carry status.environment dev / prod (the CMP injected
#     spec.environment, the operator surfaced it).
#   * Two Deployments `web` with the env-RESOLVED spec: replicas 1 (dev)
#     vs 2 (prod), env TIER=dev vs TIER=prod, both labelled
#     apprafter.io/application=web + apprafter.io/environment=<env>.
#   * `apprafter app status web` aggregates BOTH env-deployments.
#   * `apprafter app remove web --env dev --yes` deletes ONLY web-dev
#     (web-prod survives) — per-env teardown is surgical.
#
# CLI state injection
# -------------------
# `apprafter cluster-bootstrap` / `app add` read the kubeconfig from the
# CLI's per-target state store (not $KUBECONFIG). We set APPRAFTER_CONFIG_DIR
# to a tmpdir and seed it with a minimal config.yaml (active_target: k3d) +
# a state.json carrying the kubeconfig as kubeconfig_yaml (plaintext) — the
# same approach gitops-walk.sh / needs-disk-walk.sh use.
#
# Required: docker (or podman), git, cargo, kubectl — all satisfied inside
# `nix develop` or on a standard CI runner.
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

CLUSTER_NAME="apprafter-e2e-per-env"
FIXTURE_SRC="${REPO_ROOT}/e2e/fixtures/per-env-app"
LOGICAL_APP="web"               # the AppRafter CR name in every namespace
NS_DEV="web-dev"                # ADR 0044: each env-deploy in its OWN namespace
NS_PROD="web-prod"
GIT_DAEMON_PORT="9420"          # distinct from the gitops/disk walks (9418)

# Group-qualify the two collision-prone kinds so kubectl never resolves to
# the wrong API group (Argo CD's argoproj.io Application vs apprafter.io).
ARGO_APP="application.argoproj.io"
AR_APP="application.apprafter.io"

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------

for tool in git cargo kubectl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done

if ! command -v docker >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
    printf 'ERROR: neither "docker" nor "podman" found on PATH\n' >&2
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

# Set to 1 only after the cluster is up AND $KUBECONFIG points at it.
# Until then, dump_diagnostics / k3d_down must NOT run — on a cluster-up
# failure $KUBECONFIG still points at the ambient cluster, and an e2e must
# never touch a non-test cluster.
K3D_CREATED=0

cleanup() {
    local exit_code=$?

    # Kill the git daemon first so the port is free. `git daemon --detach`
    # double-forks, so also pkill by the unique port pattern so a detached
    # daemon can't leak and wedge the port for the next run.
    if [ -n "$GIT_DAEMON_PID" ] && kill -0 "$GIT_DAEMON_PID" 2>/dev/null; then
        kill "$GIT_DAEMON_PID" 2>/dev/null || true
    fi
    pkill -f "git[ -]daemon.*${GIT_DAEMON_PORT}" 2>/dev/null || true

    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! gitops-walk-per-env FAILED at %s (exit %d) !!!\n' \
            "$(elapsed)" "$exit_code" >&2
        if [ "$K3D_CREATED" -eq 1 ]; then
            dump_diagnostics
            printf 'Tearing down cluster (set APPRAFTER_E2E_SKIP_DESTROY=1 to keep).\n' >&2
        else
            printf 'cluster %s was never created (runtime unavailable?) — skipping diagnostics + teardown; your ambient KUBECONFIG was NOT touched.\n' \
                "$CLUSTER_NAME" >&2
        fi
    fi

    if [ "$K3D_CREATED" -eq 1 ]; then
        if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
            k3d_down "$CLUSTER_NAME" || true
        else
            printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' \
                "$CLUSTER_NAME"
            printf 'Run: <k3d|kind> cluster delete %s\n' "$CLUSTER_NAME"
        fi
    fi

    rm -rf "$TMPDIR_WORK"
    exit "$exit_code"
}
trap cleanup EXIT

# ---------------------------------------------------------------
# Helper: apprafter CLI with cwd = fixture src dir.
#
# lib.sh's apprafter() always cd's to REPO_ROOT/cli; but `app add`
# uses the cwd to locate apprafter/Application.cue (the scaffold gate
# + the `--env` validation against the manifest's declared
# environments). We cd into the fixture dir and pass --manifest-path
# so cargo still picks up the CLI workspace.
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
# Helper: seed the CLI state store (mirrors gitops-walk.sh).
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
# Helper: init the fixture git repo and start git daemon.
# ---------------------------------------------------------------
setup_git_server() {
    local repo_dst="${GIT_REPOS_DIR}/per-env-app"

    mkdir -p "${GIT_REPOS_DIR}"

    cp -r "${FIXTURE_SRC}" "${repo_dst}"
    (
        cd "${repo_dst}"
        git init -b main
        git config user.email "e2e@apprafter.io"
        git config user.name "AppRafter E2E"
        git add .
        git commit -m "feat: initial per-env-app fixture"
    )
    touch "${repo_dst}/.git/git-daemon-export-ok"

    # Reap any leaked daemon holding the port from a prior run.
    pkill -f "git[ -]daemon.*${GIT_DAEMON_PORT}" 2>/dev/null || true

    git daemon \
        --reuseaddr \
        --base-path="${GIT_REPOS_DIR}" \
        --export-all \
        --port="${GIT_DAEMON_PORT}" \
        --detach \
        "${GIT_REPOS_DIR}"

    GIT_DAEMON_PID=$(pgrep -f "git[ -]daemon.*${GIT_DAEMON_PORT}" | head -1 || true)
    printf '  git daemon started (port %s, base %s)\n' \
        "$GIT_DAEMON_PORT" "$GIT_REPOS_DIR"
}

# `detect_host_gateway_ip` (the host IP in-cluster pods use to reach the host
# git daemon — runtime-aware for kind+podman / kind+docker / k3d+docker) lives
# in e2e/lib.sh so both gitops walks share one copy.

# ---------------------------------------------------------------
# Helper: wait until the Argo CD Application reaches Synced+Healthy.
# ---------------------------------------------------------------
wait_argo_app_synced() {
    local app_name="$1"
    local deadline=$(( $(date +%s) + 600 ))  # 10 min

    printf '  waiting for Argo CD Application %s to be Synced+Healthy ...\n' \
        "$app_name"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        local sync_status health_status
        sync_status=$(kubectl -n argocd get "$ARGO_APP" "$app_name" \
            -o jsonpath='{.status.sync.status}' 2>/dev/null || true)
        health_status=$(kubectl -n argocd get "$ARGO_APP" "$app_name" \
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
    kubectl -n argocd describe "$ARGO_APP" "$app_name" >&2 || true
    return 1
}

# ---------------------------------------------------------------
# Local helper: wait_jsonpath <kind> <ns> <name> <jsonpath> <want> [timeout]
#   Poll until the rendered value equals <want> or the deadline passes.
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
# Local helper: jp <kind> <ns> <name> <jsonpath>  (read one value)
# ---------------------------------------------------------------
jp() {
    kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true
}

# Read the TIER env value off the rendered Deployment's container[0].
deploy_tier() {  # <ns>
    kubectl -n "$1" get deployment "$LOGICAL_APP" \
        -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"TIER\")].value}" \
        2>/dev/null || true
}

# ===============================================================
# Phase 0: bring up the cluster
# ===============================================================

phase "Phase 0: cluster up ${CLUSTER_NAME}"

k3d_up "$CLUSTER_NAME"

cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
K3D_CREATED=1
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

# ===============================================================
# Phase 1: seed CLI state and run cluster-bootstrap
# ===============================================================

phase "Phase 1: cluster-bootstrap (Cilium-skipped -> Argo CD -> platform-stack)"

kubeconfig_content=$(cat "$KUBECONFIG_FILE")
seed_apprafter_state "$kubeconfig_content"
export APPRAFTER_CONFIG_DIR
printf '  APPRAFTER_CONFIG_DIR=%s\n' "$APPRAFTER_CONFIG_DIR"

bootstrap_with_retry

printf '  cluster-bootstrap complete\n'

# ---------------------------------------------------------------
# Phase 1b (APPRAFTER_E2E_LOCAL_OPERATOR): build + side-load the
# working-tree operator + admission-webhook + argocd-cue-cmp, apply the
# branch CRDs + branch RBAC. 2.9 is UNRELEASED, so this is REQUIRED (the
# e2e-per-env Justfile target sets the flag). Mirrors needs-disk-walk.sh's
# Phase-1b block, plus the cue-cmp sidecar side-load (Approach A).
# NOTE: the if-body below is intentionally NOT indented — it carries a
# column-0 heredoc (the RBAC apply); `fi` closes it just before Phase 2.
# ---------------------------------------------------------------
if [ -n "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
phase "Phase 1b: build + load local operator + webhook + cue-cmp (APPRAFTER_E2E_LOCAL_OPERATOR)"
builder=podman; command -v podman >/dev/null 2>&1 || builder=docker

build_load_restart() { # <deployment> <operator-subdir>
    local dep="$1" sub="$2" img
    printf '  waiting for the %s Deployment to appear ...\n' "$dep"
    for _ in $(seq 1 60); do
        kubectl -n apprafter-system get deploy "$dep" >/dev/null 2>&1 && break
        sleep 5
    done
    img=$(kubectl -n apprafter-system get deploy "$dep" \
        -o jsonpath='{.spec.template.spec.containers[0].image}')
    printf '  building %s from the working tree (%s) ...\n' "$img" "$builder"
    "$builder" build -f "${REPO_ROOT}/operator/${sub}/Dockerfile" \
        -t "$img" "${REPO_ROOT}/operator"
    cluster_load_image "$CLUSTER_NAME" "$img"
    kubectl -n apprafter-system rollout restart "deploy/${dep}"
    kubectl -n apprafter-system rollout status "deploy/${dep}" --timeout=180s
}
build_load_restart apprafter-operator apprafter-operator
build_load_restart admission-webhook admission-webhook

# `rollout status` returns once the NEW webhook pod is Ready, but the OLD
# (released) pod lingers Terminating for its grace period — wait until ONLY
# the branch webhook serves before any branch-typed apply.
printf '  waiting for the old (released) webhook pod to fully terminate ...\n'
_wh_deadline=$(( $(date +%s) + 90 ))
while [ "$(date +%s)" -lt "$_wh_deadline" ]; do
    [ "$(kubectl -n apprafter-system get pods \
        -l app.kubernetes.io/name=admission-webhook --no-headers 2>/dev/null \
        | wc -l)" -le 1 ] && break
    sleep 3
done
printf '  apprafter-operator + admission-webhook now running the working-tree build\n'

# ---- Approach A: side-load the WORKING-TREE argocd-cue-cmp into the
# argocd-repo-server's cue-cmp sidecar. The 2.9 per-env injection
# (spec.environment + the apprafter.io/environment label) lives ONLY in the
# working-tree entrypoint.sh, shipped IN the image; the published cue-cmp
# predates it. We build the image tagged as the EXACT ref the sidecar
# already references, side-load it (IfNotPresent → the node serves the
# rebuilt image under the unchanged ref), and rollout-restart the
# repo-server so the new pod picks it up.
printf '  building + side-loading the working-tree argocd-cue-cmp sidecar image ...\n'
retry 30 5 -- kubectl -n argocd get deploy argocd-repo-server >/dev/null
_cmp_img=$(kubectl -n argocd get deploy argocd-repo-server \
    -o jsonpath='{.spec.template.spec.containers[?(@.name=="cue-cmp")].image}')
if [ -z "$_cmp_img" ]; then
    printf 'ERROR: argocd-repo-server has no cue-cmp sidecar container — cannot side-load the 2.9 CMP\n' >&2
    kubectl -n argocd get deploy argocd-repo-server \
        -o jsonpath='{.spec.template.spec.containers[*].name}' >&2 || true
    exit 1
fi
printf '  cue-cmp sidecar image ref: %s\n' "$_cmp_img"
"$builder" build -f "${REPO_ROOT}/argocd-cue-cmp/Dockerfile" \
    -t "$_cmp_img" "${REPO_ROOT}/argocd-cue-cmp"
cluster_load_image "$CLUSTER_NAME" "$_cmp_img"
kubectl -n argocd rollout restart deploy/argocd-repo-server
kubectl -n argocd rollout status deploy/argocd-repo-server --timeout=180s
printf '  argocd-repo-server now running the working-tree cue-cmp (2.9 injection enabled)\n'

# The released platform-stack chart predates the 2.9 CRD surface
# (spec.environment, spec.environments, status.environment). Argo CD owns
# those CRDs via the apprafter-operator Application, so: disable automated
# sync on the parent + operator apps (else Argo reverts the drift), then
# apply the BRANCH-rendered CRDs server-side. Same rationale as side-loading
# the images — all unpublished 2.9 artifacts. Mirrors needs-disk-walk.sh.
printf '  applying branch operator CRDs (released chart predates the 2.9 schema) ...\n'
for _app in platform apprafter-operator; do
    kubectl -n argocd patch "$ARGO_APP" "$_app" --type=merge \
        -p '{"spec":{"syncPolicy":{"automated":null}}}' >/dev/null 2>&1 || true
done
_yq() { if command -v yq >/dev/null 2>&1; then yq "$@"; else nix run nixpkgs#yq-go -- "$@"; fi; }
helm template apprafter-operator "${REPO_ROOT}/operator/charts/apprafter-operator" \
    | _yq 'select(.kind == "CustomResourceDefinition")' \
    | kubectl apply --server-side --force-conflicts -f -
for _crd in applications serviceproviders resourceclaims retainedclaims; do
    retry 12 5 -- kubectl wait --for=condition=Established \
        "crd/${_crd}.apprafter.io" --timeout=30s
done
printf '  branch CRDs applied + Established\n'

# The IMAGE is the branch's; the cluster's RBAC is still the published
# chart's. A verb added in the same commit as the code that needs it would
# 403 here and nowhere else — which is exactly how the D8 Postgres sampler
# read as "inert" for three battery runs.
apply_branch_operator_rbac
fi  # end APPRAFTER_E2E_LOCAL_OPERATOR (Phase 1b)

# ===============================================================
# Phase 2: platform readiness (apps AppProject, operator, webhook)
# ===============================================================

phase "Phase 2: platform readiness (apps AppProject, operator, webhook)"

printf '  waiting for AppProject apps ...\n'
deadline=$(( $(date +%s) + 600 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    if kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1; then
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

# The admission webhook validates the AppRafter CR on CREATE — wait until
# it is Available before the first sync renders one.
printf '  waiting for the admission-webhook Deployment ...\n'
retry 30 10 -- kubectl -n apprafter-system rollout status \
    deploy admission-webhook --timeout=60s

# ===============================================================
# Phase 3: git daemon — the shared per-env fixture repo
# ===============================================================

phase "Phase 3: git daemon — per-env fixture repo"

setup_git_server

if [ "$(cluster_runtime)" = "kind" ]; then
    GIT_REPO_URL="git://$(detect_host_gateway_ip):${GIT_DAEMON_PORT}/per-env-app"
else
    GIT_REPO_URL="git://host.k3d.internal:${GIT_DAEMON_PORT}/per-env-app"
fi
GIT_REPO_URL_HOST="git://127.0.0.1:${GIT_DAEMON_PORT}/per-env-app"
printf '  fixture repo URL (in-cluster): %s\n' "$GIT_REPO_URL"

printf '  verifying git daemon is up (local clone check)...\n'
retry 6 5 -- git ls-remote "$GIT_REPO_URL_HOST" >/dev/null
printf '  git daemon is reachable\n'

# ===============================================================
# Phase 4: register the SAME repo twice — once per environment
# ===============================================================

phase "Phase 4: apprafter app add --env dev (ns ${NS_DEV}) + --env prod (ns ${NS_PROD})"

# `app add --env dev -n web-dev`: produces Argo Application `web-dev` with
# spec.source.plugin.env APPRAFTER_APP_ENV=dev + the apprafter.io labels,
# destination namespace web-dev. The SAME for prod. Both run from the
# fixture dir so the CLI's `--env` validation finds the manifest's declared
# dev/prod environments.
apprafter_from_fixture app add \
    "$GIT_REPO_URL" \
    --name      "$LOGICAL_APP" \
    --env       dev \
    --branch    main \
    --path      "/" \
    --namespace "$NS_DEV" \
    --project   apps \
    --no-ping \
    --no-interactive

apprafter_from_fixture app add \
    "$GIT_REPO_URL" \
    --name      "$LOGICAL_APP" \
    --env       prod \
    --branch    main \
    --path      "/" \
    --namespace "$NS_PROD" \
    --project   apps \
    --no-ping \
    --no-interactive

printf '  both app adds complete\n'

# ===============================================================
# Phase 5: assert the two Argo CD Applications + their plugin env/labels
# ===============================================================

phase "Phase 5: assert two Argo Applications web-dev + web-prod (plugin env + labels)"

retry 12 5 -- kubectl -n argocd get "$ARGO_APP" "${LOGICAL_APP}-dev" >/dev/null
retry 12 5 -- kubectl -n argocd get "$ARGO_APP" "${LOGICAL_APP}-prod" >/dev/null

# The CLI wrote the per-env plugin env var + the grouping/env labels.
dev_plugin_env=$(kubectl -n argocd get "$ARGO_APP" "${LOGICAL_APP}-dev" \
    -o jsonpath='{.spec.source.plugin.env[?(@.name=="APPRAFTER_APP_ENV")].value}' \
    2>/dev/null || true)
assert_eq "web-dev Argo plugin.env APPRAFTER_APP_ENV" "$dev_plugin_env" "dev"
prod_plugin_env=$(kubectl -n argocd get "$ARGO_APP" "${LOGICAL_APP}-prod" \
    -o jsonpath='{.spec.source.plugin.env[?(@.name=="APPRAFTER_APP_ENV")].value}' \
    2>/dev/null || true)
assert_eq "web-prod Argo plugin.env APPRAFTER_APP_ENV" "$prod_plugin_env" "prod"

dev_app_label=$(jp "$ARGO_APP" argocd "${LOGICAL_APP}-dev" \
    '{.metadata.labels.apprafter\.io/application}')
assert_eq "web-dev Argo label apprafter.io/application" "$dev_app_label" "$LOGICAL_APP"
dev_env_label=$(jp "$ARGO_APP" argocd "${LOGICAL_APP}-dev" \
    '{.metadata.labels.apprafter\.io/environment}')
assert_eq "web-dev Argo label apprafter.io/environment" "$dev_env_label" "dev"
prod_env_label=$(jp "$ARGO_APP" argocd "${LOGICAL_APP}-prod" \
    '{.metadata.labels.apprafter\.io/environment}')
assert_eq "web-prod Argo label apprafter.io/environment" "$prod_env_label" "prod"

wait_argo_app_synced "${LOGICAL_APP}-dev"
wait_argo_app_synced "${LOGICAL_APP}-prod"

# ===============================================================
# Phase 6: assert two AppRafter CRs with the CMP-injected env
# ===============================================================

phase "Phase 6: assert AppRafter CRs (CMP injected spec.environment dev/prod)"

# The cue-cmp sidecar injected spec.environment from APPRAFTER_APP_ENV; the
# operator surfaced it onto status.environment. One CR `web` per namespace.
wait_jsonpath "$AR_APP" "$NS_DEV" "$LOGICAL_APP" '{.spec.environment}' dev 300
wait_jsonpath "$AR_APP" "$NS_PROD" "$LOGICAL_APP" '{.spec.environment}' prod 300

# The injected env label landed on the CR too (CMP stamps both).
dev_cr_label=$(jp "$AR_APP" "$NS_DEV" "$LOGICAL_APP" \
    '{.metadata.labels.apprafter\.io/environment}')
assert_eq "web CR (dev) label apprafter.io/environment" "$dev_cr_label" "dev"

wait_jsonpath "$AR_APP" "$NS_DEV" "$LOGICAL_APP" '{.status.environment}' dev 180
wait_jsonpath "$AR_APP" "$NS_PROD" "$LOGICAL_APP" '{.status.environment}' prod 180

# ===============================================================
# Phase 7: assert the env-RESOLVED Deployments (replicas + TIER differ)
# ===============================================================

phase "Phase 7: assert env-resolved Deployments (replicas 1 vs 2, TIER dev vs prod)"

wait_jsonpath "$AR_APP" "$NS_DEV" "$LOGICAL_APP" '{.status.phase}' Ready 240
wait_jsonpath "$AR_APP" "$NS_PROD" "$LOGICAL_APP" '{.status.phase}' Ready 240

kubectl -n "$NS_DEV" wait --for=condition=Available \
    "deployment/${LOGICAL_APP}" --timeout=300s
kubectl -n "$NS_PROD" wait --for=condition=Available \
    "deployment/${LOGICAL_APP}" --timeout=300s

# dev: base replicas (1) + TIER=dev. prod: env-overridden replicas (2) +
# TIER=prod. This is the load-bearing per-env-resolution assertion.
dev_replicas=$(jp deployment "$NS_DEV" "$LOGICAL_APP" '{.spec.replicas}')
assert_eq "web (dev) Deployment replicas" "$dev_replicas" "1"
prod_replicas=$(jp deployment "$NS_PROD" "$LOGICAL_APP" '{.spec.replicas}')
assert_eq "web (prod) Deployment replicas" "$prod_replicas" "2"

dev_tier=$(deploy_tier "$NS_DEV")
assert_eq "web (dev) Deployment env TIER" "$dev_tier" "dev"
prod_tier=$(deploy_tier "$NS_PROD")
assert_eq "web (prod) Deployment env TIER" "$prod_tier" "prod"

# The operator stamps the grouping + env labels onto the rendered children.
dev_dep_app_label=$(jp deployment "$NS_DEV" "$LOGICAL_APP" \
    '{.metadata.labels.apprafter\.io/application}')
assert_eq "web (dev) Deployment label apprafter.io/application" "$dev_dep_app_label" "$LOGICAL_APP"
dev_dep_env_label=$(jp deployment "$NS_DEV" "$LOGICAL_APP" \
    '{.metadata.labels.apprafter\.io/environment}')
assert_eq "web (dev) Deployment label apprafter.io/environment" "$dev_dep_env_label" "dev"
prod_dep_env_label=$(jp deployment "$NS_PROD" "$LOGICAL_APP" \
    '{.metadata.labels.apprafter\.io/environment}')
assert_eq "web (prod) Deployment label apprafter.io/environment" "$prod_dep_env_label" "prod"

# Wait for the prod Deployment to actually scale to 2 available replicas
# (proves the env override drives the real workload, not just the spec).
printf '  waiting for web (prod) to reach 2 available replicas ...\n'
deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    avail=$(jp deployment "$NS_PROD" "$LOGICAL_APP" '{.status.availableReplicas}')
    [ "${avail:-0}" -ge 2 ] && break
    sleep 5
done
avail=$(jp deployment "$NS_PROD" "$LOGICAL_APP" '{.status.availableReplicas}')
assert_eq "web (prod) availableReplicas" "${avail:-0}" "2"

# ===============================================================
# Phase 8: `apprafter app status web` aggregates BOTH env-deployments
# ===============================================================

phase "Phase 8: apprafter app status web — aggregates both env-deployments"

status_out=$(apprafter_from_fixture app status "$LOGICAL_APP" 2>&1)
printf '%s\n' "$status_out"
# The aggregate status must mention both env-deployments (web-dev + web-prod
# Argo names) — they share the apprafter.io/application=web label.
if ! printf '%s\n' "$status_out" | grep -q "${LOGICAL_APP}-dev"; then
    printf 'ERROR: app status web did not list the dev env-deployment (web-dev)\n' >&2
    exit 1
fi
if ! printf '%s\n' "$status_out" | grep -q "${LOGICAL_APP}-prod"; then
    printf 'ERROR: app status web did not list the prod env-deployment (web-prod)\n' >&2
    exit 1
fi
printf '  ok: app status web lists BOTH web-dev and web-prod\n'

# ===============================================================
# Phase 9: surgical per-env teardown — remove ONLY web-dev
# ===============================================================

phase "Phase 9: apprafter app remove web --env dev --yes (web-prod survives)"

apprafter_from_fixture app remove "$LOGICAL_APP" --env dev --yes

# The web-dev Argo Application is gone (Argo cascade tears down the CR +
# Deployment via the resources finalizer).
wait_gone_argo() {  # <argo app name>
    local app_name="$1"
    local deadline=$(( $(date +%s) + 180 ))
    printf '  waiting for Argo Application %s to be gone ...\n' "$app_name"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        kubectl -n argocd get "$ARGO_APP" "$app_name" >/dev/null 2>&1 || {
            printf '  ok: Argo Application %s is gone\n' "$app_name"
            return 0
        }
        sleep 5
    done
    printf 'ERROR: Argo Application %s still present after 3 min\n' "$app_name" >&2
    return 1
}
wait_gone_argo "${LOGICAL_APP}-dev"

# web-prod MUST still be present (the remove was env-scoped to dev).
prod_still=$(jp "$ARGO_APP" argocd "${LOGICAL_APP}-prod" '{.metadata.name}')
assert_eq "web-prod Argo Application survives the dev-only remove" \
    "$prod_still" "${LOGICAL_APP}-prod"
# The prod CR + Deployment are untouched.
prod_cr_still=$(jp "$AR_APP" "$NS_PROD" "$LOGICAL_APP" '{.metadata.name}')
assert_eq "web CR (prod) survives the dev-only remove" "$prod_cr_still" "$LOGICAL_APP"
prod_dep_still=$(jp deployment "$NS_PROD" "$LOGICAL_APP" '{.metadata.name}')
assert_eq "web Deployment (prod) survives the dev-only remove" "$prod_dep_still" "$LOGICAL_APP"

# ===============================================================
# Done — tear down on success path
# ===============================================================

trap - EXIT

if [ -n "$GIT_DAEMON_PID" ] && kill -0 "$GIT_DAEMON_PID" 2>/dev/null; then
    kill "$GIT_DAEMON_PID" 2>/dev/null || true
fi
pkill -f "git[ -]daemon.*${GIT_DAEMON_PORT}" 2>/dev/null || true

if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    k3d_down "$CLUSTER_NAME" || true
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' \
        "$CLUSTER_NAME"
fi

rm -rf "$TMPDIR_WORK"

printf '\ngitops-walk-per-env GREEN in %s\n' "$(elapsed)"
printf 'Chain proven (Approach A): app add --env -> Argo plugin env -> cue-cmp injects spec.environment -> operator env-resolves -> two namespaces (replicas 1 vs 2, TIER dev vs prod); app status aggregates; app remove --env is surgical\n'
