#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter app-migration-walk e2e — the operator-side 2.16b app-scope
# migration state machine on a local kind/k3d cluster (plan item 2.16b,
# spec.md §3.8, ADR app-scope migration).
#
# This exercises the shipped operator + `apprafter migration` CLI surface
# end-to-end. Two Applications built from ONE logical manifest (byte-identical
# spec.base + spec.environments, differing only in metadata.name +
# spec.environment) let us prove that a destructive edit gates PER EFFECTIVE
# ENVIRONMENT — a change to `environments.dev.replicas` gates the `dev`
# Application but NOT the `prod` one, even though both CRs carry the same
# `environments` block.
#
# The chain (operator-only, no needs → no CNPG → the baseline stamps on the
# FIRST render):
#
#   deploy mig-dev + mig-prod (two env of one logical app) ->
#   assert status.lastAppliedSpec stamped (baseline) ->
#   destructive edit to environments.dev.replicas 2->0 (scale-to-zero, HARD) ->
#     mig-dev -> status.phase=AwaitingMigrationApproval + an app-scope
#       MigrationPlan appears IN THE APP NAMESPACE, owned by the Application CR;
#     mig-prod stays non-paused + NO plan (effective-per-env) ->
#   assert plan shape (ns, controlling ownerRef=Application/uid, scope.type=
#     application, risks.classification=requires-restart, trigger.type=
#     scale-to-zero, labels) + the ownerRef does NOT cascade-delete it ->
#   self-heal: delete the plan -> the operator recreates it, app stays paused ->
#   `apprafter migration list` shows the plan; `apprafter migration approve`
#     (auto-resolving the namespace) renders the new spec, stamps the baseline,
#     deletes the plan (consume), un-freezes — and NO second plan (anti-loop);
#     the Deployment's replicas == 0 (the approved scale-to-zero applied) ->
#   soft change (image TAG only) on mig-prod -> NOT gated + a k8s Event
#     reason=SoftDestructiveChange ->
#   revert-as-reject: gate mig-prod on a network public->internal edit, then
#     REVERT it in-place -> the plan self-deletes + mig-prod un-freezes.
#
# Argo surface (SOFT): the argocd-cm carries the
# `apprafter.io_Application` health Lua mapping `AwaitingMigrationApproval`
# (the Approve-node UI). We assert its presence but never fail the walk on
# Argo — the full Argo-UI Approve-node click is covered by the ADR 0048
# platform-approval walk + the real-cluster manual walk.
#
# Local-operator mode (REQUIRED — the released chart is stale)
# -----------------------------------------------------------
# The released platform-stack chart predates 2.16b: its Application CRD lacks
# `status.lastAppliedSpec`, its MigrationPlan CRD may lack the app-scope
# shape, and the released operator image cannot detect/gate/consume app-scope
# changes. So this walk ALWAYS builds + side-loads the working-tree
# apprafter-operator (Application controller + MigrationController both live in
# that one binary), applies the BRANCH-rendered CRDs, and grants the branch
# operator ClusterRole/ClusterRoleBinding (Phase 0). The admission-webhook is
# NOT rebuilt — this walk touches no webhook-only validation the released
# image lacks.
#
# CLI state injection
# -------------------
# `apprafter cluster-bootstrap` / `apprafter migration …` read the kubeconfig
# from the CLI's per-target state store (not $KUBECONFIG). We set
# APPRAFTER_CONFIG_DIR to a tmpdir and seed it with a config.yaml
# (active_target: k3d) + a state.json carrying the kubeconfig as
# kubeconfig_yaml (plaintext) — the same approach the sibling walks use.
#
# NO Cilium (bootstrap_with_retry sets APPRAFTER_BOOTSTRAP_SKIP_CILIUM=1), so
# NO sandbox-run microVM is needed — run it plainly:
#   bash e2e/app-migration-walk.sh
#
# Required: docker (or podman), cargo, kubectl, yq — all satisfied inside
#   `nix develop` or on a standard CI runner. (cargo: lib.sh runs the CLI via
#   `cargo run --bin apprafter`.)
#
# Exit codes:
#   0 — chain green
#   1 — assertion failure
#   2 — precondition missing
#
# PASS/FAIL is judged by READING THE LOG: every phase prints `ok:` lines, the
# final `app-migration-walk GREEN` banner prints only on the success path, and
# any failure prints `FAILED:` before teardown. Do NOT trust the exit code
# alone.

set -euo pipefail

# ---------------------------------------------------------------
# Source shared helpers
# ---------------------------------------------------------------

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-mig-walk"

APP_NS="mig-walk"                   # tenant namespace (app-scope plans live here)
APP_DEV="mig-dev"                   # dev-environment Application
APP_PROD="mig-prod"                 # prod-environment Application
APP_IMAGE="nginxdemos/hello:plain-text"   # runs on kind, no registry auth
APP_IMAGE_TAG2="nginxdemos/hello:0.4"     # same repo, different tag (soft change)

# Group-qualify the collision-prone Application kind: bare `application` also
# matches Argo CD's argoproj.io Application. Always address the apprafter.io CR.
APP_RES="application.apprafter.io"
# The MigrationPlan CRD kind. `mp` / `migplan` are the short names; we address
# the full `migrationplan` for clarity.
PLAN_RES="migrationplan"

PROVIDER_NS="apprafter-system"      # operator + webhook ns

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------

for tool in cargo kubectl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done
if ! command -v docker >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
    printf 'ERROR: neither "docker" nor "podman" found on PATH\n' >&2
    exit 2
fi

# yq (or the nix fallback) selects CRD/ClusterRole objects out of the branch
# `helm template` in Phase 0.
_yq() { if command -v yq >/dev/null 2>&1; then yq "$@"; else nix run nixpkgs#yq-go -- "$@"; fi; }

# ---------------------------------------------------------------
# Temp workspace
# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"

# Set to 1 only after the cluster is up AND $KUBECONFIG points at it (Phase 0).
# Until then, dump_diagnostics / k3d_down must NOT run — on a cluster-up
# failure $KUBECONFIG still points at the ambient cluster, and an e2e must
# never touch a non-test cluster.
K3D_CREATED=0

cleanup() {
    local exit_code=$?

    if [ "$exit_code" -ne 0 ]; then
        printf '\nFAILED: app-migration-walk FAILED at %s (exit %d)\n' \
            "$(elapsed)" "$exit_code" >&2
        if [ "$K3D_CREATED" -eq 1 ]; then
            dump_diagnostics || true
            printf 'Tearing down cluster (set APPRAFTER_E2E_SKIP_DESTROY=1 to keep).\n' >&2
        else
            printf 'cluster %s was never created (runtime unavailable in this shell?) — skipping diagnostics + teardown; your ambient KUBECONFIG was NOT touched.\n' \
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
# Helper: seed the CLI state store (mirrors needs-disk-walk.sh).
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
# Local assertion helpers (mirror the sibling walks)
# ---------------------------------------------------------------

# wait_jsonpath <kind> <ns> <name> <jsonpath> <want> [timeout]   (uses $KUBECONFIG)
wait_jsonpath() {
    local kind="$1" ns="$2" name="$3" jsonpath="$4" want="$5"
    local timeout="${6:-180}" deadline got
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait %s/%s [%s] == %q (timeout %ss) ...\n' "$kind" "$name" "$jsonpath" "$want" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        got=$(kubectl -n "$ns" get "$kind" "$name" -o jsonpath="$jsonpath" 2>/dev/null || true)
        if [ "$got" = "$want" ]; then
            printf '  ok: %s/%s [%s] = %q\n' "$kind" "$name" "$jsonpath" "$got"
            return 0
        fi
        printf '    %s: got=%q want=%q\n' "$(date +%H:%M:%S)" "$got" "$want"
        sleep 5
    done
    printf 'FAILED: %s/%s [%s] never became %q (last=%q)\n' "$kind" "$name" "$jsonpath" "$want" "${got:-}" >&2
    kubectl -n "$ns" describe "$kind" "$name" >&2 2>&1 || true
    return 1
}

# wait_gone <kind> <ns> <name> [timeout]   (uses $KUBECONFIG)
wait_gone() {
    local kind="$1" ns="$2" name="$3" timeout="${4:-120}" deadline
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait %s/%s gone (timeout %ss) ...\n' "$kind" "$name" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if ! kubectl -n "$ns" get "$kind" "$name" >/dev/null 2>&1; then
            printf '  ok: %s/%s is gone\n' "$kind" "$name"
            return 0
        fi
        sleep 5
    done
    printf 'FAILED: %s/%s still present after %ss\n' "$kind" "$name" "$timeout" >&2
    return 1
}

assert_eq() {
    local desc="$1" got="$2" want="$3"
    if [ "$got" = "$want" ]; then
        printf '  ok: %s = %q\n' "$desc" "$got"
        return 0
    fi
    printf 'FAILED: %s — got %q, want %q\n' "$desc" "$got" "$want" >&2
    return 1
}

# jp <kind> <ns> <name> <jsonpath>   (read one value, uses $KUBECONFIG)
jp() { kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true; }

# app_scope_plan_name <app-name>  — the name of the (at most one) app-scope
# MigrationPlan in $APP_NS whose `apprafter.io/application` label == <app>.
# Empty when none. (App-scope plans carry the label set the operator stamps in
# ApplicationMigrationStrategy::create_plan_for.)
app_scope_plan_name() {
    kubectl -n "$APP_NS" get "$PLAN_RES" \
        -l "apprafter.io/application=$1,apprafter.io/scope=application" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# plan_count_for <app-name>  — number of app-scope plans for <app> in $APP_NS.
plan_count_for() {
    kubectl -n "$APP_NS" get "$PLAN_RES" \
        -l "apprafter.io/application=$1,apprafter.io/scope=application" \
        --no-headers 2>/dev/null | grep -c . || true
}

# wait_plan_appears <app-name> [timeout]  — poll until an app-scope plan for
# <app> exists; echoes its name. Fails (returns 1) on timeout.
wait_plan_appears() {
    local app="$1" timeout="${2:-120}" deadline nm
    deadline=$(( $(date +%s) + timeout ))
    # Progress → STDERR: this function's STDOUT is captured as the plan NAME
    # (`DEV_PLAN="$(wait_plan_appears …)"`), so any stdout chatter pollutes it.
    printf '  wait an app-scope MigrationPlan for %s to appear (timeout %ss) ...\n' "$app" "$timeout" >&2
    while [ "$(date +%s)" -lt "$deadline" ]; do
        nm="$(app_scope_plan_name "$app")"
        if [ -n "$nm" ]; then
            printf '  ok: MigrationPlan %s/%s exists for %s\n' "$APP_NS" "$nm" "$app" >&2
            printf '%s' "$nm"
            return 0
        fi
        sleep 5
    done
    printf 'FAILED: no app-scope MigrationPlan for %s appeared within %ss\n' "$app" "$timeout" >&2
    kubectl -n "$APP_NS" get "$PLAN_RES" -o wide >&2 2>&1 || true
    return 1
}

# assert_non_paused <app-name>  — the Application is NOT paused
# (status.phase != AwaitingMigrationApproval). A steady app sits Ready or
# Progressing; either is fine, we only forbid the paused phase.
assert_non_paused() {
    local app="$1" phase
    phase=$(jp "$APP_RES" "$APP_NS" "$app" '{.status.phase}')
    if [ "$phase" = "AwaitingMigrationApproval" ]; then
        printf 'FAILED: %s is PAUSED (phase=AwaitingMigrationApproval) but should not be\n' "$app" >&2
        return 1
    fi
    printf '  ok: %s is not paused (phase=%q)\n' "$app" "${phase:-<empty>}"
    return 0
}

# wait_non_paused <app-name> [timeout]  — poll until the app leaves the paused
# phase (settles to Ready/Progressing/… anything but AwaitingMigrationApproval).
wait_non_paused() {
    local app="$1" timeout="${2:-120}" deadline phase
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait %s to leave AwaitingMigrationApproval (timeout %ss) ...\n' "$app" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        phase=$(jp "$APP_RES" "$APP_NS" "$app" '{.status.phase}')
        if [ -n "$phase" ] && [ "$phase" != "AwaitingMigrationApproval" ]; then
            printf '  ok: %s left the paused phase (phase=%q)\n' "$app" "$phase"
            return 0
        fi
        sleep 5
    done
    printf 'FAILED: %s never left AwaitingMigrationApproval within %ss (last=%q)\n' \
        "$app" "$timeout" "${phase:-<empty>}" >&2
    kubectl -n "$APP_NS" describe "$APP_RES" "$app" >&2 2>&1 || true
    return 1
}

# apply_app <name> <environment> [replicas_dev] [replicas_prod] [network] [image]
#   Emit ONE logical manifest as a raw Application CR. Both mig-dev and mig-prod
#   are generated from the SAME base + environments block (byte-identical modulo
#   metadata.name + spec.environment) so the effective-per-env assertion is
#   meaningful. The tunables (dev/prod replicas, network, image) let later
#   phases drive a destructive edit while keeping the rest identical.
apply_app() {
    local name="$1" environment="$2"
    local rdev="${3:-2}" rprod="${4:-2}" network="${5:-public}" image="${6:-$APP_IMAGE}"
    kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${name}
  namespace: ${APP_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  environment: ${environment}
  base:
    image: ${image}
    replicas: 2
    expose:
      port: 80
      network: ${network}
      hostname: dev.example.com
  environments:
    dev:
      replicas: ${rdev}
    prod:
      replicas: ${rprod}
YAML
}

# ===============================================================
# Phase 0: cluster up + bootstrap + branch CRD/RBAC + local operator
# ===============================================================

phase "Phase 0: kind up + bootstrap + branch CRDs/RBAC + local operator (${CLUSTER_NAME})"

k3d_up "$CLUSTER_NAME"
cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
K3D_CREATED=1
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

kubeconfig_content=$(cat "$KUBECONFIG_FILE")
seed_apprafter_state "$kubeconfig_content"
export APPRAFTER_CONFIG_DIR
printf '  APPRAFTER_CONFIG_DIR=%s\n' "$APPRAFTER_CONFIG_DIR"

bootstrap_with_retry
printf '  cluster-bootstrap complete\n'

# Wait for the platform-stack `apps` AppProject (so Argo has settled enough
# that patching automated-sync off, below, is meaningful).
printf '  waiting for AppProject apps ...\n'
deadline=$(( $(date +%s) + 600 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1 && break
    sleep 10
done
kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1 || {
    printf 'FAILED: AppProject apps not found after 10 min\n' >&2; exit 1; }

# The released chart predates 2.16b (no status.lastAppliedSpec, possibly no
# app-scope MigrationPlan shape). Argo CD owns those CRDs via the platform /
# apprafter-operator Applications, so disable automated sync on them (else Argo
# reverts the branch CRD drift), then apply the BRANCH-rendered CRDs +
# ClusterRole/ClusterRoleBinding server-side.
printf '  disabling Argo automated-sync on platform + apprafter-operator ...\n'
for _app in platform apprafter-operator; do
    kubectl -n argocd patch applications.argoproj.io "$_app" --type=merge \
        -p '{"spec":{"syncPolicy":{"automated":null}}}' >/dev/null 2>&1 || true
done

printf '  applying branch operator CRDs + ClusterRole/ClusterRoleBinding ...\n'
# `--namespace apprafter-system` is LOAD-BEARING: without it `helm template`
# renders the ClusterRoleBinding subject namespace as `default` (Helm's
# release-namespace default), and a `--force-conflicts` apply would then
# OVERWRITE the umbrella-installed binding that correctly targets the operator
# SA in apprafter-system → the operator SA loses every cluster-scoped
# permission and 403s on every apprafter.io list/watch (no reconcile, no
# status). The chart's ClusterRole rules themselves are complete from a bare
# template (only the binding's subject namespace was wrong).
helm template apprafter-operator "${REPO_ROOT}/operator/charts/apprafter-operator" \
    --namespace apprafter-system \
    | _yq 'select(.kind == "CustomResourceDefinition" or .kind == "ClusterRole" or .kind == "ClusterRoleBinding")' \
    | kubectl apply --server-side --force-conflicts -f -
retry 12 5 -- kubectl wait --for=condition=Established \
    crd/applications.apprafter.io --timeout=30s
retry 12 5 -- kubectl wait --for=condition=Established \
    crd/migrationplans.apprafter.io --timeout=30s
printf '  branch CRDs Established + operator RBAC applied\n'

# Build + side-load the working-tree operator, then restart it so the branch
# controller (Application detection + MigrationController — both in the one
# apprafter-operator binary) is what runs. Applied AFTER the branch CRD so the
# restarted operator observes the 2.16b schema (status.lastAppliedSpec).
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
printf '  ok: apprafter-operator now running the working-tree (2.16b) build\n'

# The admission webhook must be Available before any Application apply (it
# validates the CR on CREATE — network/hostname invariants).
retry 30 10 -- kubectl -n "$PROVIDER_NS" rollout status \
    deploy admission-webhook --timeout=60s
printf '  ok: platform ready (branch operator + webhook)\n'

# ===============================================================
# Phase 1: deploy mig-dev + mig-prod; baseline stamps on first render
# ===============================================================

phase "Phase 1: deploy mig-dev + mig-prod (two env of one logical app); baseline stamps"

kubectl create namespace "$APP_NS" 2>/dev/null || true

# Both start non-destructive (dev/prod replicas both 2, network public). The
# first render stamps status.lastAppliedSpec — proving the baseline lands even
# with NO needs (no CNPG round-trip to slow it down).
apply_app "$APP_DEV" dev
apply_app "$APP_PROD" prod

# Each reaches a steady non-paused phase (never AwaitingMigrationApproval on a
# first apply — there's no baseline to diff against). Poll for a non-empty
# phase, then assert it isn't paused.
for app in "$APP_DEV" "$APP_PROD"; do
    _deadline=$(( $(date +%s) + 240 ))
    while [ "$(date +%s)" -lt "$_deadline" ]; do
        _ph=$(jp "$APP_RES" "$APP_NS" "$app" '{.status.phase}')
        [ -n "$_ph" ] && break
        sleep 5
    done
    assert_non_paused "$app"
done

# The baseline (status.lastAppliedSpec) stamped for BOTH — the RAW spec, so its
# base.image is the shared image. This is what makes every later diff possible.
wait_jsonpath "$APP_RES" "$APP_NS" "$APP_DEV" '{.status.lastAppliedSpec.base.image}' "$APP_IMAGE" 120
wait_jsonpath "$APP_RES" "$APP_NS" "$APP_PROD" '{.status.lastAppliedSpec.base.image}' "$APP_IMAGE" 120
printf '  ok: baseline stamped (status.lastAppliedSpec present on both mig-dev + mig-prod)\n'

# Sanity: no app-scope plans exist yet (nothing destructive happened).
assert_eq "no plans for mig-dev yet" "$(plan_count_for "$APP_DEV")" "0"
assert_eq "no plans for mig-prod yet" "$(plan_count_for "$APP_PROD")" "0"

# ===============================================================
# Phase 2: effective-per-env destructive create (H4/H1)
# ===============================================================

phase "Phase 2: environments.dev.replicas 2->0 gates mig-dev ONLY (effective-per-env)"

# Scale-to-zero is HARD-destructive (requires-restart). Edit BOTH CRs' shared
# environments.dev.replicas 2->0 and re-apply both — byte-identical manifests.
# The EFFECTIVE spec of mig-dev (environment: dev) now has replicas 0 → gates.
# The EFFECTIVE spec of mig-prod (environment: prod) is unchanged (its dev
# override is irrelevant) → must NOT gate. That's the per-effective-environment
# diff (H4), each side under its own env vs its own stamped baseline.
apply_app "$APP_DEV" dev 0 2
apply_app "$APP_PROD" prod 0 2

# mig-dev pauses + an app-scope plan appears IN THE APP NAMESPACE.
wait_jsonpath "$APP_RES" "$APP_NS" "$APP_DEV" '{.status.phase}' AwaitingMigrationApproval 180
DEV_PLAN="$(wait_plan_appears "$APP_DEV" 120)"
[ -n "$DEV_PLAN" ] || { printf 'FAILED: dev plan name empty\n' >&2; exit 1; }
printf '  ok: mig-dev gated (AwaitingMigrationApproval) with plan %s/%s\n' "$APP_NS" "$DEV_PLAN"

# mig-prod stays non-paused AND has NO plan — the effective-per-env guarantee.
# Give the operator a beat to (not) act on prod, then assert.
sleep 10
assert_non_paused "$APP_PROD"
assert_eq "no plan for mig-prod (effective-per-env)" "$(plan_count_for "$APP_PROD")" "0"
printf '  ok: dev gated, prod untouched (effective-per-env)\n'

# ===============================================================
# Phase 3: plan shape (ownerRef / label / scope / classification / trigger)
# ===============================================================

phase "Phase 3: dev plan shape — ns, controlling ownerRef=Application/uid, scope, risks, trigger"

# .metadata.namespace == the app ns (app-scope plans live with the Application).
plan_ns=$(jp "$PLAN_RES" "$APP_NS" "$DEV_PLAN" '{.metadata.namespace}')
assert_eq "plan .metadata.namespace" "$plan_ns" "$APP_NS"

# controlling ownerRef → the mig-dev Application CR (kind + controller=true +
# uid match), so the plan cascade-deletes IFF the Application is deleted.
owner_kind=$(jp "$PLAN_RES" "$APP_NS" "$DEV_PLAN" '{.metadata.ownerReferences[0].kind}')
assert_eq "plan ownerRef[0].kind" "$owner_kind" "Application"
owner_ctrl=$(jp "$PLAN_RES" "$APP_NS" "$DEV_PLAN" '{.metadata.ownerReferences[0].controller}')
assert_eq "plan ownerRef[0].controller" "$owner_ctrl" "true"
owner_uid=$(jp "$PLAN_RES" "$APP_NS" "$DEV_PLAN" '{.metadata.ownerReferences[0].uid}')
dev_uid=$(jp "$APP_RES" "$APP_NS" "$APP_DEV" '{.metadata.uid}')
assert_eq "plan ownerRef[0].uid == mig-dev uid" "$owner_uid" "$dev_uid"

# scope.type == application (verified against crd-migrationplan.yaml:
# spec.scope.type enum application|platform).
scope_type=$(jp "$PLAN_RES" "$APP_NS" "$DEV_PLAN" '{.spec.scope.type}')
assert_eq "plan .spec.scope.type" "$scope_type" "application"
# scope.application.ref.name + environment carry the app identity.
scope_ref_name=$(jp "$PLAN_RES" "$APP_NS" "$DEV_PLAN" '{.spec.scope.application.ref.name}')
assert_eq "plan .spec.scope.application.ref.name" "$scope_ref_name" "$APP_DEV"
scope_env=$(jp "$PLAN_RES" "$APP_NS" "$DEV_PLAN" '{.spec.scope.application.environment}')
assert_eq "plan .spec.scope.application.environment" "$scope_env" "dev"

# risks.classification == requires-restart (scale-to-zero is requires-restart;
# NB the field is spec.risks.classification, NOT spec.classification).
classification=$(jp "$PLAN_RES" "$APP_NS" "$DEV_PLAN" '{.spec.risks.classification}')
assert_eq "plan .spec.risks.classification" "$classification" "requires-restart"

# trigger.type == scale-to-zero, trigger.field == replicas (verified against
# migration strategy detect_destructive; the CRD field is spec.trigger.type).
trigger_type=$(jp "$PLAN_RES" "$APP_NS" "$DEV_PLAN" '{.spec.trigger.type}')
assert_eq "plan .spec.trigger.type" "$trigger_type" "scale-to-zero"
trigger_field=$(jp "$PLAN_RES" "$APP_NS" "$DEV_PLAN" '{.spec.trigger.field}')
assert_eq "plan .spec.trigger.field" "$trigger_field" "replicas"

# labels sanity: non-empty + the app name present (apprafter.io/application).
plan_app_label=$(jp "$PLAN_RES" "$APP_NS" "$DEV_PLAN" '{.metadata.labels.apprafter\.io/application}')
assert_eq "plan label apprafter.io/application" "$plan_app_label" "$APP_DEV"

# The controlling ownerRef must NOT cascade-delete the plan while the
# Application is alive — re-check the plan still exists after ~60s.
printf '  sleeping 60s to confirm the ownerRef does not cascade-delete the live plan ...\n'
sleep 60
still=$(jp "$PLAN_RES" "$APP_NS" "$DEV_PLAN" '{.metadata.name}')
assert_eq "plan still present ~60s after creation (ownerRef alive, no cascade)" "$still" "$DEV_PLAN"
printf '  ok: plan shape + ownerRef alive\n'

# ===============================================================
# Phase 4: self-heal (R3-M2) — deleting the plan while the change persists
#          re-creates it; the app stays paused
# ===============================================================

phase "Phase 4: self-heal — delete the plan; the operator recreates it, app stays paused"

kubectl -n "$APP_NS" delete "$PLAN_RES" "$DEV_PLAN" --wait=false
printf '  deleted plan %s; waiting for the operator to recreate one ...\n' "$DEV_PLAN"

# A fresh app-scope plan for mig-dev must reappear (name may differ — the
# operator regenerates a timestamped name). The app stays paused throughout.
DEV_PLAN2="$(wait_plan_appears "$APP_DEV" 90)"
[ -n "$DEV_PLAN2" ] || { printf 'FAILED: plan did not self-heal\n' >&2; exit 1; }
wait_jsonpath "$APP_RES" "$APP_NS" "$APP_DEV" '{.status.phase}' AwaitingMigrationApproval 60
printf '  ok: plan self-heals on delete (recreated as %s; mig-dev still paused)\n' "$DEV_PLAN2"

# ===============================================================
# Phase 5: CLI list + approve + consume + anti-loop
# ===============================================================

phase "Phase 5: apprafter migration list + approve — renders, consumes, un-freezes, no loop"

# `apprafter migration list` (Task 13) must show the app-scope plan in its
# mig-walk namespace. Re-resolve the current plan name (self-heal may have
# renamed it).
CUR_PLAN="$(app_scope_plan_name "$APP_DEV")"
[ -n "$CUR_PLAN" ] || { printf 'FAILED: no current dev plan to approve\n' >&2; exit 1; }
list_out="$(apprafter migration list)"
printf '%s\n' "$list_out"
if printf '%s' "$list_out" | grep -q "$APP_NS" && printf '%s' "$list_out" | grep -q "$CUR_PLAN"; then
    printf '  ok: apprafter migration list shows %s in namespace %s\n' "$CUR_PLAN" "$APP_NS"
else
    printf 'FAILED: apprafter migration list did not show plan %s in namespace %s\n%s\n' \
        "$CUR_PLAN" "$APP_NS" "$list_out" >&2
    exit 1
fi

# Approve WITHOUT -n to exercise the CLI's namespace auto-resolution (the plan
# lives in mig-walk, not apprafter-system — the resolver must find it).
printf '  approving %s via apprafter migration approve (auto-resolving the namespace) ...\n' "$CUR_PLAN"
apprafter migration approve "$CUR_PLAN"

# The operator renders the new spec, stamps the baseline, consumes (deletes)
# the plan, and un-freezes. mig-dev leaves AwaitingMigrationApproval.
wait_non_paused "$APP_DEV" 120

# The plan is consumed (deleted) within ~60s.
wait_gone "$PLAN_RES" "$APP_NS" "$CUR_PLAN" 90

# Anti-loop: NO new app-scope plan reappears over ~30s (the baseline was
# re-stamped to the new spec, so the next reconcile detects no change).
printf '  watching ~30s for an anti-loop regression (no new plan should appear) ...\n'
sleep 30
loop_count="$(plan_count_for "$APP_DEV")"
assert_eq "anti-loop: no new plan after approve" "$loop_count" "0"

# The approved scale-to-zero was applied: the Deployment's replicas == 0.
wait_jsonpath deployment "$APP_NS" "$APP_DEV" '{.spec.replicas}' 0 120
printf '  ok: approve consumes plan, applies spec (replicas=0), no loop\n'

# The baseline was re-stamped to reflect the approved spec — its effective dev
# replicas is now 0. Read the RAW stamped baseline's environments.dev.replicas.
new_baseline_dev_replicas=$(jp "$APP_RES" "$APP_NS" "$APP_DEV" '{.status.lastAppliedSpec.environments.dev.replicas}')
assert_eq "re-stamped baseline environments.dev.replicas" "$new_baseline_dev_replicas" "0"

# ===============================================================
# Phase 6: soft change (image TAG only) is ungated + emits an Event
# ===============================================================

phase "Phase 6: soft change (image tag) on mig-prod — NOT gated + SoftDestructiveChange Event"

# An image TAG change (same repo, different tag) is SOFT-destructive: it rolls
# through un-gated but earns a k8s Event so operators notice. Edit mig-prod's
# base.image tag only (prod replicas stay 2, network public).
apply_app "$APP_PROD" prod 0 2 public "$APP_IMAGE_TAG2"

# mig-prod must NOT gate (no pause, no plan).
sleep 10
assert_non_paused "$APP_PROD"
assert_eq "no plan for mig-prod on a soft image-tag change" "$(plan_count_for "$APP_PROD")" "0"

# A SoftDestructiveChange Event appears on the mig-prod Application. The
# operator publishes it best-effort after the apply succeeds; poll for it.
printf '  waiting for a SoftDestructiveChange Event on mig-prod ...\n'
_ev_deadline=$(( $(date +%s) + 60 ))
soft_event=""
while [ "$(date +%s)" -lt "$_ev_deadline" ]; do
    soft_event=$(kubectl -n "$APP_NS" get events \
        --field-selector "involvedObject.name=${APP_PROD}" \
        -o jsonpath='{range .items[*]}{.reason}{"\n"}{end}' 2>/dev/null \
        | grep -x SoftDestructiveChange || true)
    [ -n "$soft_event" ] && break
    sleep 5
done
if [ -n "$soft_event" ]; then
    printf '  ok: soft change ungated + Event (reason=SoftDestructiveChange on mig-prod)\n'
else
    printf 'FAILED: no SoftDestructiveChange Event on mig-prod after 60s\n' >&2
    kubectl -n "$APP_NS" get events --field-selector "involvedObject.name=${APP_PROD}" >&2 2>&1 || true
    exit 1
fi

# ===============================================================
# Phase 7: revert = reject-via-Git (DeleteThenRender + network-visibility op)
# ===============================================================

phase "Phase 7: revert = reject-via-Git — gate mig-prod on network public->internal, then REVERT"

# network public->internal is HARD-destructive (network-visibility-change,
# requires-restart). Keep the tag from Phase 6 so ONLY the network flips.
apply_app "$APP_PROD" prod 0 2 internal "$APP_IMAGE_TAG2"

wait_jsonpath "$APP_RES" "$APP_NS" "$APP_PROD" '{.status.phase}' AwaitingMigrationApproval 180
PROD_PLAN="$(wait_plan_appears "$APP_PROD" 120)"
[ -n "$PROD_PLAN" ] || { printf 'FAILED: prod plan name empty\n' >&2; exit 1; }
prod_trigger=$(jp "$PLAN_RES" "$APP_NS" "$PROD_PLAN" '{.spec.trigger.type}')
assert_eq "prod plan trigger.type" "$prod_trigger" "network-visibility-change"
printf '  ok: mig-prod gated on network-visibility-change (plan %s)\n' "$PROD_PLAN"

# REVERT the change in-place (network back to public) — the analogue of
# reverting the edit in Git. The operator sees no destructive delta vs the
# baseline (still public), so it self-deletes the now-stale plan and un-freezes.
printf '  reverting mig-prod network back to public (reject-via-Git) ...\n'
apply_app "$APP_PROD" prod 0 2 public "$APP_IMAGE_TAG2"

# The plan self-deletes + mig-prod un-freezes.
wait_gone "$PLAN_RES" "$APP_NS" "$PROD_PLAN" 90
wait_non_paused "$APP_PROD" 120
assert_eq "no residual plan for mig-prod after revert" "$(plan_count_for "$APP_PROD")" "0"
printf '  ok: revert deletes plan + un-freezes (reject-via-Git)\n'

# ===============================================================
# Phase 8: Argo surface (SOFT)
# ===============================================================

phase "Phase 8: (soft) Argo Application health Lua present for AwaitingMigrationApproval"

# The argocd-cm carries the `apprafter.io_Application` health customization
# that surfaces the paused phase as the Approve node in the Argo UI. Assert its
# presence but NEVER fail the walk on Argo — the full Argo-UI Approve-node click
# is covered by the ADR 0048 platform-approval walk + the manual walk.
if kubectl -n argocd get cm argocd-cm >/dev/null 2>&1; then
    if kubectl -n argocd get cm argocd-cm -o yaml 2>/dev/null \
        | grep -q "AwaitingMigrationApproval"; then
        printf '  ok: (soft) Argo Application health Lua present (argocd-cm maps AwaitingMigrationApproval)\n'
    else
        printf '  note: (soft) argocd-cm does not (yet) map AwaitingMigrationApproval — not failing (Argo surface rides the ADR 0048 walk).\n'
    fi
else
    printf '  note: (soft) argocd-cm not present — not failing (Argo not up).\n'
fi

# ===============================================================
# Done — tear down on the success path
# ===============================================================

# Remove the EXIT trap so cleanup() does not fire again — we own the tear-down
# inline here on the success path.
trap - EXIT

if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    k3d_down "$CLUSTER_NAME" || true
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' "$CLUSTER_NAME"
fi

rm -rf "$TMPDIR_WORK"

printf '\napp-migration-walk GREEN in %s\n' "$(elapsed)"
printf 'Chain proven: deploy two-env -> baseline stamped -> effective-per-env gate (dev gated, prod untouched) -> plan shape + ownerRef -> self-heal -> CLI list + approve (consume + apply + anti-loop) -> soft change ungated + Event -> revert = reject-via-Git\n'
