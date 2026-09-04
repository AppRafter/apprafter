#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter per-environment `expose` DEEP-MERGE e2e — the full 2.16c
# chain on a local kind/k3d cluster (plan item 2.16c Task 12).
#
# This walk proves the 2.16c promise end-to-end: a per-environment
# `expose` override is now PARTIAL. An env carries only the DIFF and
# deep-merges onto `base.expose`; it does NOT have to re-declare the
# whole block (in particular `port`, which is required on the base but
# OPTIONAL on the override). The chain is the 2.9 per-env chain
# (gitops-walk-per-env.sh) with expose-deep-merge assertions bolted on:
#
#   apprafter app add --env dev|prod  (CLI: ADR 0044)
#     -> Argo Application `web-<env>` (plugin.env APPRAFTER_APP_ENV)
#     -> argocd-cue-cmp sidecar injects spec.environment=<env>
#     -> operator effective_spec DEEP-MERGES environments[<env>].expose
#        onto base.expose (2.16c) and renders the env-resolved children.
#
# The manifest under test (e2e/fixtures/expose-deep-merge-app):
#
#   base:  expose { port: 8080, network: "public", hostname: "x.example.com" }
#   environments:
#     prod: {}                                    # base verbatim (public)
#     dev:  expose { network: "internal" }        # DIFF ONLY — no port/hostname
#
# Assertions (2.16c)
# ------------------
#   * dev inherits `port` from base: the dev Deployment container port
#     == 8080 AND the dev Service targetPort == 8080 (port was inherited,
#     NOT re-declared in the override).
#   * dev has NO public HTTPRoute: `kubectl get httproute -n web-dev`
#     returns nothing (effective network internal → inherited hostname
#     inert, no route leaked).
#   * base/prod keeps the public expose: prod Deployment container port
#     8080 + Service targetPort 8080 AND an HTTPRoute with hostname
#     `x.example.com` exists.
#   * H1 (real stored path): the dev AppRafter CR's
#     `spec.environments.dev.expose` carries ONLY `{network:"internal"}`
#     — no defaulted `port`/`hostname` materialized into the stored
#     override (the partial stays partial).
#
# Approach — same as gitops-walk-per-env.sh
# -----------------------------------------
# APPROACH A (true end-to-end): besides the operator + admission-webhook,
# side-load the WORKING-TREE argocd-cue-cmp image into the repo-server's
# cue-cmp sidecar (the per-env injection lives only in the working-tree
# entrypoint.sh). 2.16c is UNRELEASED, so this walk REQUIRES local-build
# mode — the operator's expose DEEP-MERGE (operator-rendering
# effective_spec / merge_expose) ships only in the working tree; the
# published operator predates it. Phase 1b builds + side-loads the
# working-tree operator + admission-webhook + cue-cmp and applies the
# branch CRDs/RBAC. To make that unconditional (this walk is meaningless
# against a published operator), the walk FORCES
# APPRAFTER_E2E_LOCAL_OPERATOR=1.
#
# CLI state injection & git-daemon fixture: identical to
# gitops-walk-per-env.sh (see its header).
#
# Required: docker (or podman), git, cargo, kubectl — all satisfied
# inside `nix develop` or on a standard CI runner. This walk uses the
# DEFAULT CNI (kindnet / flannel), NOT Cilium, so it does NOT need the
# rootful sandbox-run microVM.
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
# 2.16c REQUIRES the working-tree operator (expose deep-merge is
# unreleased). Force local-build mode so a bare invocation is faithful.
# ---------------------------------------------------------------
export APPRAFTER_E2E_LOCAL_OPERATOR=1

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-e2e-expose-dm"
FIXTURE_SRC="${REPO_ROOT}/e2e/fixtures/expose-deep-merge-app"
LOGICAL_APP="web"               # the AppRafter CR name in every namespace
NS_DEV="web-dev"                # ADR 0044: each env-deploy in its OWN namespace
NS_PROD="web-prod"
GIT_DAEMON_PORT="9421"          # distinct from the other gitops walks (9418/9420)
BASE_PORT="8080"                # base.expose.port — dev inherits this
PUBLIC_HOSTNAME="x.example.com" # base.expose.hostname — prod public route

# Group-qualify the collision-prone kinds so kubectl never resolves to
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
K3D_CREATED=0

cleanup() {
    local exit_code=$?

    if [ -n "$GIT_DAEMON_PID" ] && kill -0 "$GIT_DAEMON_PID" 2>/dev/null; then
        kill "$GIT_DAEMON_PID" 2>/dev/null || true
    fi
    pkill -f "git[ -]daemon.*${GIT_DAEMON_PORT}" 2>/dev/null || true

    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! expose-deep-merge-walk FAILED at %s (exit %d) !!!\n' \
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
# Helper: apprafter CLI with cwd = fixture src dir (for `--env`
# validation against the manifest's declared environments).
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
# Helper: seed the CLI state store (mirrors gitops-walk-per-env.sh).
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
    local repo_dst="${GIT_REPOS_DIR}/expose-deep-merge-app"

    mkdir -p "${GIT_REPOS_DIR}"

    cp -r "${FIXTURE_SRC}" "${repo_dst}"
    (
        cd "${repo_dst}"
        git init -b main
        git config user.email "e2e@apprafter.io"
        git config user.name "AppRafter E2E"
        git add .
        git commit -m "feat: initial expose-deep-merge-app fixture"
    )
    touch "${repo_dst}/.git/git-daemon-export-ok"

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
    printf 'ERROR (FAILED): %s — got %q, want %q\n' "$desc" "$got" "$want" >&2
    return 1
}

# ---------------------------------------------------------------
# Local helper: jp <kind> <ns> <name> <jsonpath>  (read one value)
# ---------------------------------------------------------------
jp() {
    kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true
}

# Read the container[0] port off the rendered Deployment.
deploy_container_port() {  # <ns>
    kubectl -n "$1" get deployment "$LOGICAL_APP" \
        -o jsonpath='{.spec.template.spec.containers[0].ports[0].containerPort}' \
        2>/dev/null || true
}

# Read the Service http port's targetPort off the rendered Service.
svc_target_port() {  # <ns>
    kubectl -n "$1" get service "$LOGICAL_APP" \
        -o jsonpath='{.spec.ports[0].targetPort}' \
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
# Phase 1b (APPRAFTER_E2E_LOCAL_OPERATOR — FORCED): build + side-load
# the working-tree operator + admission-webhook + argocd-cue-cmp, apply
# the branch CRDs + branch RBAC. 2.16c is UNRELEASED, so this is
# MANDATORY: the expose deep-merge (operator-rendering effective_spec /
# merge_expose) and the partial #ApplicationEnvOverride webhook rule
# ship only in the working tree. Mirrors gitops-walk-per-env.sh's
# Phase-1b block (+ the cue-cmp side-load, Approach A).
# NOTE: the if-body below is intentionally NOT indented.
# ---------------------------------------------------------------
if [ -n "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
phase "Phase 1b: build + load local operator + webhook + cue-cmp (FORCED for 2.16c)"
builder=podman; command -v podman >/dev/null 2>&1 || builder=docker

# `build_load_restart` now lives in e2e/lib.sh — ONE implementation, and it
# CACHES the built image by the content of operator/ + schemas/v1alpha1/.
# Thirteen walks carried a private copy that SHADOWED the shared one, so the
# cache benefited nobody: each still rebuilt the same image (3m04 measured).
build_load_restart apprafter-operator apprafter-operator
build_load_restart admission-webhook admission-webhook

# Wait until ONLY the branch webhook serves before any branch-typed apply.
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
# argocd-repo-server's cue-cmp sidecar (the 2.9 per-env injection lives
# only in the working-tree entrypoint.sh; the published cue-cmp predates
# it). Same mechanism as gitops-walk-per-env.sh.
printf '  building + side-loading the working-tree argocd-cue-cmp sidecar image ...\n'
retry 30 5 -- kubectl -n argocd get deploy argocd-repo-server >/dev/null
_cmp_img=$(kubectl -n argocd get deploy argocd-repo-server \
    -o jsonpath='{.spec.template.spec.containers[?(@.name=="cue-cmp")].image}')
if [ -z "$_cmp_img" ]; then
    printf 'ERROR: argocd-repo-server has no cue-cmp sidecar container — cannot side-load the CMP\n' >&2
    kubectl -n argocd get deploy argocd-repo-server \
        -o jsonpath='{.spec.template.spec.containers[*].name}' >&2 || true
    exit 1
fi
printf '  cue-cmp sidecar image ref: %s\n' "$_cmp_img"
# Build context is the REPO ROOT, not argocd-cue-cmp/ (2.12f): the
# Dockerfile COPYs both the sidecar files (`argocd-cue-cmp/…`) AND the
# AppRafter CUE schema (`schemas/v1alpha1/*.cue`), so it must see the whole
# tree. Passing the subdir as context fails with
# `COPY argocd-cue-cmp/plugin.yaml: no such file or directory` (exit 125).
"$builder" build -f "${REPO_ROOT}/argocd-cue-cmp/Dockerfile" \
    -t "$_cmp_img" "${REPO_ROOT}"
cluster_load_image "$CLUSTER_NAME" "$_cmp_img"
kubectl -n argocd rollout restart deploy/argocd-repo-server
kubectl -n argocd rollout status deploy/argocd-repo-server --timeout=180s
printf '  argocd-repo-server now running the working-tree cue-cmp\n'

# The released platform-stack chart predates the 2.16c CRD surface
# (partial environments[*].expose). Argo CD owns those CRDs via the
# apprafter-operator Application, so: disable automated sync on the
# parent + operator apps (else Argo reverts the drift), then apply the
# BRANCH-rendered CRDs server-side. Mirrors gitops-walk-per-env.sh.
printf '  applying branch operator CRDs (released chart predates the 2.16c schema) ...\n'
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
# Phase 2: platform readiness (apps AppProject, operator, webhook,
#          Gateway API HTTPRoute CRD)
# ===============================================================

phase "Phase 2: platform readiness (apps AppProject, operator, webhook, HTTPRoute CRD)"

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

# The HTTPRoute assertion needs the Gateway API CRD present (installed by
# the platform-stack gateway-api-crds component, synced by Argo). Wait
# for it so `kubectl get httproute` resolves rather than erroring.
printf '  waiting for the Gateway API HTTPRoute CRD (platform-stack gateway-api-crds) ...\n'
retry 60 10 -- kubectl get crd httproutes.gateway.networking.k8s.io >/dev/null
retry 12 5 -- kubectl wait --for=condition=Established \
    crd/httproutes.gateway.networking.k8s.io --timeout=30s
printf '  ok: HTTPRoute CRD Established\n'

# ===============================================================
# Phase 3: git daemon — the shared expose-deep-merge fixture repo
# ===============================================================

phase "Phase 3: git daemon — expose-deep-merge fixture repo"

setup_git_server

if [ "$(cluster_runtime)" = "kind" ]; then
    GIT_REPO_URL="git://$(detect_host_gateway_ip):${GIT_DAEMON_PORT}/expose-deep-merge-app"
else
    GIT_REPO_URL="git://host.k3d.internal:${GIT_DAEMON_PORT}/expose-deep-merge-app"
fi
GIT_REPO_URL_HOST="git://127.0.0.1:${GIT_DAEMON_PORT}/expose-deep-merge-app"
printf '  fixture repo URL (in-cluster): %s\n' "$GIT_REPO_URL"

printf '  verifying git daemon is up (local clone check)...\n'
retry 6 5 -- git ls-remote "$GIT_REPO_URL_HOST" >/dev/null
printf '  git daemon is reachable\n'

# ===============================================================
# Phase 4: register the SAME repo twice — once per environment
# ===============================================================

phase "Phase 4: apprafter app add --env dev (ns ${NS_DEV}) + --env prod (ns ${NS_PROD})"

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
# Phase 5: the two Argo Applications reach Synced+Healthy
# ===============================================================

phase "Phase 5: two Argo Applications web-dev + web-prod Synced+Healthy"

retry 12 5 -- kubectl -n argocd get "$ARGO_APP" "${LOGICAL_APP}-dev" >/dev/null
retry 12 5 -- kubectl -n argocd get "$ARGO_APP" "${LOGICAL_APP}-prod" >/dev/null

dev_plugin_env=$(kubectl -n argocd get "$ARGO_APP" "${LOGICAL_APP}-dev" \
    -o jsonpath='{.spec.source.plugin.env[?(@.name=="APPRAFTER_APP_ENV")].value}' \
    2>/dev/null || true)
assert_eq "web-dev Argo plugin.env APPRAFTER_APP_ENV" "$dev_plugin_env" "dev"
prod_plugin_env=$(kubectl -n argocd get "$ARGO_APP" "${LOGICAL_APP}-prod" \
    -o jsonpath='{.spec.source.plugin.env[?(@.name=="APPRAFTER_APP_ENV")].value}' \
    2>/dev/null || true)
assert_eq "web-prod Argo plugin.env APPRAFTER_APP_ENV" "$prod_plugin_env" "prod"

wait_argo_app_synced "${LOGICAL_APP}-dev"
wait_argo_app_synced "${LOGICAL_APP}-prod"

# ===============================================================
# Phase 6: the two AppRafter CRs carry the CMP-injected env + Ready
# ===============================================================

phase "Phase 6: AppRafter CRs (CMP injected spec.environment dev/prod) reach Ready"

wait_jsonpath "$AR_APP" "$NS_DEV" "$LOGICAL_APP" '{.spec.environment}' dev 300
wait_jsonpath "$AR_APP" "$NS_PROD" "$LOGICAL_APP" '{.spec.environment}' prod 300

wait_jsonpath "$AR_APP" "$NS_DEV" "$LOGICAL_APP" '{.status.phase}' Ready 240
wait_jsonpath "$AR_APP" "$NS_PROD" "$LOGICAL_APP" '{.status.phase}' Ready 240

kubectl -n "$NS_DEV" wait --for=condition=Available \
    "deployment/${LOGICAL_APP}" --timeout=300s
kubectl -n "$NS_PROD" wait --for=condition=Available \
    "deployment/${LOGICAL_APP}" --timeout=300s

# ===============================================================
# Phase 7 (2.16c CORE): dev INHERITS `port` from base.expose
# ===============================================================

phase "Phase 7 (2.16c): dev inherits port ${BASE_PORT} from base (partial override)"

# The dev override was ONLY {network:"internal"} — no port. If the
# deep-merge works, the rendered dev container port + Service targetPort
# both equal the base port 8080 (inherited, not re-declared).
dev_cport=$(deploy_container_port "$NS_DEV")
assert_eq "web (dev) Deployment container port (INHERITED from base)" \
    "$dev_cport" "$BASE_PORT"
dev_tport=$(svc_target_port "$NS_DEV")
assert_eq "web (dev) Service targetPort (INHERITED from base)" \
    "$dev_tport" "$BASE_PORT"

# ===============================================================
# Phase 8 (2.16c CORE): dev has NO public HTTPRoute; prod DOES
# ===============================================================

phase "Phase 8 (2.16c): dev has NO public HTTPRoute; base/prod keeps it"

# dev: effective network is `internal` (override-wins) so the inherited
# hostname is inert — NO HTTPRoute should exist in web-dev. Give the
# operator a beat, then assert emptiness.
sleep 8
dev_routes=$(kubectl -n "$NS_DEV" get httproute \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || true)
if [ -z "$dev_routes" ]; then
    printf '  ok: web (dev) has NO HTTPRoute (internal → inherited hostname inert)\n'
else
    printf 'ERROR (FAILED): web (dev) unexpectedly has HTTPRoute(s): %q\n' \
        "$dev_routes" >&2
    kubectl -n "$NS_DEV" get httproute -o yaml >&2 || true
    exit 1
fi

# prod: base is public + hostname x.example.com → exactly one HTTPRoute
# named `web` with that hostname.
printf '  waiting for the prod HTTPRoute (base public expose) ...\n'
retry 24 5 -- kubectl -n "$NS_PROD" get httproute "$LOGICAL_APP" >/dev/null
prod_route_host=$(kubectl -n "$NS_PROD" get httproute "$LOGICAL_APP" \
    -o jsonpath='{.spec.hostnames[0]}' 2>/dev/null || true)
assert_eq "web (prod) HTTPRoute hostname (base public expose retained)" \
    "$prod_route_host" "$PUBLIC_HOSTNAME"

# prod container/Service port must also be the base 8080 (sanity: base
# verbatim, no expose diff).
prod_cport=$(deploy_container_port "$NS_PROD")
assert_eq "web (prod) Deployment container port (base verbatim)" \
    "$prod_cport" "$BASE_PORT"
prod_tport=$(svc_target_port "$NS_PROD")
assert_eq "web (prod) Service targetPort (base verbatim)" \
    "$prod_tport" "$BASE_PORT"

# ===============================================================
# Phase 9 (2.16c H1): the STORED dev CR's expose override is PARTIAL
# ===============================================================

phase "Phase 9 (2.16c H1): stored dev CR env override carries ONLY {network:internal}"

# Read the FULL dev env-override expose object off the STORED CR (the
# real cue-cmp export path, not a host render). It must be EXACTLY
# {"network":"internal"} — no defaulted port/hostname/tls materialized
# into the stored partial. We compare the JSON keys + values.
dev_expose_json=$(kubectl -n "$NS_DEV" get "$AR_APP" "$LOGICAL_APP" \
    -o jsonpath='{.spec.environments.dev.expose}' 2>/dev/null || true)
printf '  stored dev env-override expose: %s\n' "$dev_expose_json"

# network must be internal ...
dev_expose_net=$(kubectl -n "$NS_DEV" get "$AR_APP" "$LOGICAL_APP" \
    -o jsonpath='{.spec.environments.dev.expose.network}' 2>/dev/null || true)
assert_eq "stored dev env-override expose.network" "$dev_expose_net" "internal"

# ... and NO port/hostname/tls should have leaked into the STORED override
# (H1: the partial stays partial; inheritance happens at RENDER time in
# the operator, it is NOT baked into the stored CR).
dev_expose_port=$(kubectl -n "$NS_DEV" get "$AR_APP" "$LOGICAL_APP" \
    -o jsonpath='{.spec.environments.dev.expose.port}' 2>/dev/null || true)
assert_eq "stored dev env-override expose.port is ABSENT (partial stays partial)" \
    "${dev_expose_port:-<absent>}" "<absent>"
dev_expose_host=$(kubectl -n "$NS_DEV" get "$AR_APP" "$LOGICAL_APP" \
    -o jsonpath='{.spec.environments.dev.expose.hostname}' 2>/dev/null || true)
assert_eq "stored dev env-override expose.hostname is ABSENT (partial stays partial)" \
    "${dev_expose_host:-<absent>}" "<absent>"

# Sanity: the base expose (public + 8080 + hostname) is intact on the CR.
base_port=$(kubectl -n "$NS_DEV" get "$AR_APP" "$LOGICAL_APP" \
    -o jsonpath='{.spec.base.expose.port}' 2>/dev/null || true)
assert_eq "stored base.expose.port intact" "$base_port" "$BASE_PORT"
base_net=$(kubectl -n "$NS_DEV" get "$AR_APP" "$LOGICAL_APP" \
    -o jsonpath='{.spec.base.expose.network}' 2>/dev/null || true)
assert_eq "stored base.expose.network intact" "$base_net" "public"

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

printf '\nexpose-deep-merge-walk GREEN in %s\n' "$(elapsed)"
printf 'Chain proven (2.16c): partial environments.dev.expose {network:internal} deep-merges onto base.expose -> dev INHERITS port %s (container+Service targetPort), effective internal emits NO public HTTPRoute (inherited hostname inert); base/prod keeps the public HTTPRoute on %s; the STORED dev override stays partial ({network:internal} only, no defaulted keys)\n' \
    "$BASE_PORT" "$PUBLIC_HOSTNAME"
