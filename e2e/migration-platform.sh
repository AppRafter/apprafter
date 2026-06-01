#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter migration-platform e2e — platform pin -> breaking ->
# MigrationPlan -> approve/reject (plan item 1.81).
#
# This script proves the full platform-scope MigrationPlan gate:
#
#   1. k3d_up (trap k3d_down on EXIT)
#   2. apprafter cluster-bootstrap (seeds the CLI state store the
#      same way gitops-walk.sh does — APPRAFTER_CONFIG_DIR + a
#      minimal state.json carrying the k3d kubeconfig).
#   3. Stand up a local OCI registry via k3d's --registry-create
#      flag (no external service required). Push two minimal
#      platform-stack Helm charts to it:
#        - 0.2.0 carrying change:"safe"   (baseline pin)
#        - 0.3.0 carrying change:"breaking" (the trigger)
#      The fixture charts are built in-script from the rendered
#      dist/ tree (platform-stack-0.1.50) with version + Chart.yaml
#      annotations patched in-place.
#   4. Patch PlatformStack/default:
#        spec.source.repoURL  -> local registry
#        spec.source.upstream -> local registry (so availability
#                                polling also stays local)
#        spec.pin             -> "0.2.0"
#   5. PlatformController sees 0.3.0 available + breaking ->
#      creates a MigrationPlan. ASSERT the plan appears with
#      spec.scope.type == "platform".
#   6. APPROVE path: apprafter migration approve <plan> ->
#      ASSERT PlatformStack.spec.pin becomes "0.3.0".
#   7. REJECT path: create a second plan manually by patching pin
#      to "0.3.0" first and then back to "0.2.0" (or by re-running
#      the detection cycle after resetting pin), then
#      apprafter migration reject <plan> -> ASSERT spec.pin reverts
#      to the snapshot value.
#
# Local OCI registry approach
# ---------------------------
# k3d cluster create --registry-create apprafter-e2e-reg:5000
# publishes a registry container whose name is
# "apprafter-e2e-reg" reachable inside k3d pods as
# "apprafter-e2e-reg:5000" (k3d injects a /etc/hosts entry on
# every node). The host can push to it as
# "localhost:5000" (k3d maps the registry port to the host).
#
# Chart packaging uses `helm package` + `helm push`.
# If helm is not available the script exits with a clear error.
#
# CLI state injection
# -------------------
# Mirrors gitops-walk.sh: APPRAFTER_CONFIG_DIR is set to a tmpdir;
# a minimal config.yaml (active_target: k3d) and a state.json
# carrying the kubeconfig as plaintext kubeconfig_yaml are written
# before cluster-bootstrap. The apprafter() helper in lib.sh reads
# APPRAFTER_CONFIG_DIR from the environment.
#
# Required: docker (or podman aliased to docker), helm, cargo,
#   kubectl — all satisfied inside `nix develop` or a standard CI
#   runner.
#
# Exit codes:
#   0 — MigrationPlan gate proven (approve + reject paths)
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

CLUSTER_NAME="apprafter-e2e-migration"
REGISTRY_NAME="apprafter-e2e-reg"
REGISTRY_HOST_PORT="5000"
# Inside k3d pods the registry is reachable as <name>:<port>
REGISTRY_IN_CLUSTER="${REGISTRY_NAME}:${REGISTRY_HOST_PORT}"
# On the host we push to localhost:<port>
REGISTRY_HOST="localhost:${REGISTRY_HOST_PORT}"
CHART_NAME="platform-stack"

# We use 0.2.0 (safe, baseline) and 0.3.0 (breaking, trigger).
BASELINE_VERSION="0.2.0"
BREAKING_VERSION="0.3.0"

# Namespace where PlatformStack/default and MigrationPlans live.
PLATFORM_NS="apprafter-system"

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------

for tool in helm cargo kubectl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        printf "       Install it or run inside 'nix develop'.\n" >&2
        exit 2
    fi
done

if ! command -v docker >/dev/null 2>&1; then
    printf 'ERROR: "docker" (or podman aliased as docker) not found on PATH\n' >&2
    exit 2
fi

# ---------------------------------------------------------------
# Temp workspace
# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"
CHART_BUILD_DIR="${TMPDIR_WORK}/charts"

cleanup() {
    local exit_code=$?
    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! migration-platform FAILED at %s (exit %d) !!!\n' \
            "$(elapsed)" "$exit_code" >&2
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
# Helper: resolve k3d binary (same pattern as lib.sh k3d_up)
# ---------------------------------------------------------------

k3d_bin() {
    if command -v k3d >/dev/null 2>&1; then
        printf 'k3d'
    else
        printf 'nix run nixpkgs#k3d --'
    fi
}

# ---------------------------------------------------------------
# Helper: seed the CLI state store — mirrors gitops-walk.sh
# exactly.
#
# Writes:
#   $APPRAFTER_CONFIG_DIR/config.yaml
#   $APPRAFTER_CONFIG_DIR/state/k3d/.apprafter/state.json
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
# Helper: build a minimal fixture Helm chart from the dist tree.
#
# Takes the 0.1.50 dist/ chart as a template, patches Chart.yaml
# (version + appVersion + change-class annotation) and
# compatibility.yaml, then packages and pushes it to the local
# OCI registry.
#
# Arguments: <version> <change_class>
# ---------------------------------------------------------------

build_and_push_fixture_chart() {
    local version="$1"
    local change_class="$2"
    local src_chart="${REPO_ROOT}/platform-stack/dist/platform-stack-0.1.50"
    local build_dir="${CHART_BUILD_DIR}/${version}"

    if [ ! -d "$src_chart" ]; then
        printf 'ERROR: source chart not found at %s\n' "$src_chart" >&2
        printf "       Run 'make render' in platform-stack/ to produce it.\n" >&2
        exit 1
    fi

    printf '  building fixture chart %s (change: %s) ...\n' "$version" "$change_class"

    cp -r "$src_chart" "$build_dir"

    # Patch Chart.yaml: version, appVersion, and the
    # apprafter.io/change-class annotation. We use sed to stay
    # POSIX-portable without requiring yq.
    sed -i \
        -e "s/^version: .*/version: \"${version}\"/" \
        -e "s/^appVersion: .*/appVersion: \"${version}\"/" \
        -e "s|apprafter.io/change-class: .*|apprafter.io/change-class: \"${change_class}\"|" \
        "${build_dir}/Chart.yaml"

    # Replace compatibility.yaml with a minimal file that only
    # contains the entry for this version. PlatformController
    # reads the compatibility metadata to determine the
    # change-class for the target version.
    cat >"${build_dir}/compatibility.yaml" <<COMPAT
${version}:
  version: ${version}
  change: ${change_class}
  operatorVersion: v0.1.136
  notes: "e2e fixture chart for migration-platform test"
  references: []
  yanked: false
COMPAT

    # Package the chart into a .tgz in the build dir.
    helm package "$build_dir" --destination "$CHART_BUILD_DIR" >/dev/null

    # Push the packaged chart to the local OCI registry.
    # helm push requires the oci:// scheme prefix.
    local tgz_path="${CHART_BUILD_DIR}/${CHART_NAME}-${version}.tgz"
    if [ ! -f "$tgz_path" ]; then
        printf 'ERROR: helm package did not produce %s\n' "$tgz_path" >&2
        exit 1
    fi
    helm push "$tgz_path" "oci://${REGISTRY_HOST}" 2>&1 \
        | grep -v "^Pushed\|^Digest" || true
    printf '  pushed %s/%s:%s\n' "$REGISTRY_HOST" "$CHART_NAME" "$version"
}

# ---------------------------------------------------------------
# Helper: patch PlatformStack/default source + pin.
#
# Arguments: <repoURL> <pin>
# ---------------------------------------------------------------

patch_platformstack() {
    local repo_url="$1"
    local pin="$2"

    printf '  patching PlatformStack/default: repoURL=%s pin=%s\n' \
        "$repo_url" "$pin"

    # Strategic merge patch via kubectl; SSA is also supported but
    # strategic-merge is more portable across k3s versions.
    kubectl -n "$PLATFORM_NS" patch platformstacks.apprafter.io default \
        --type=merge \
        --patch "$(printf '{"spec":{"pin":"%s","source":{"repoURL":"%s","upstream":"%s","checkInterval":"1m"}}}' \
            "$pin" "$repo_url" "$repo_url")"
}

# ---------------------------------------------------------------
# Helper: wait until a MigrationPlan with scope.type=platform
# appears in the platform namespace.
#
# Returns the plan name via stdout so the caller can capture it.
# ---------------------------------------------------------------

wait_for_platform_migration_plan() {
    local baseline="$1"   # the "from" pin that was set
    local target="$2"     # the "to" version that triggered the plan
    local deadline=$(( $(date +%s) + 300 ))  # 5 min max

    printf '  waiting for platform-scope MigrationPlan (pin %s -> %s) ...\n' \
        "$baseline" "$target"

    while [ "$(date +%s)" -lt "$deadline" ]; do
        local plan_name
        plan_name=$(kubectl get migrationplans.apprafter.io \
            -n "$PLATFORM_NS" \
            -o jsonpath='{range .items[?(@.spec.scope.type=="platform")]}{.metadata.name}{"\n"}{end}' \
            2>/dev/null | head -1 || true)
        if [ -n "$plan_name" ]; then
            printf '  MigrationPlan %s -> found\n' "$plan_name"
            printf '%s' "$plan_name"
            return 0
        fi
        printf '    %s: no platform MigrationPlan yet, retrying...\n' \
            "$(date +%H:%M:%S)"
        sleep 10
    done

    printf 'ERROR: no platform-scope MigrationPlan appeared within 5 min\n' >&2
    printf 'Existing MigrationPlans:\n' >&2
    kubectl get migrationplans.apprafter.io -n "$PLATFORM_NS" >&2 || true
    printf 'PlatformStack status:\n' >&2
    kubectl -n "$PLATFORM_NS" describe platformstacks.apprafter.io default >&2 || true
    return 1
}

# ---------------------------------------------------------------
# Helper: assert that PlatformStack.spec.pin equals a value.
# ---------------------------------------------------------------

assert_platform_pin() {
    local expected="$1"
    local actual
    actual=$(kubectl -n "$PLATFORM_NS" get platformstacks.apprafter.io default \
        -o jsonpath='{.spec.pin}' 2>/dev/null || true)
    if [ "$actual" = "$expected" ]; then
        printf '  ASSERT PASS: PlatformStack.spec.pin == "%s"\n' "$expected"
    else
        printf 'ASSERT FAIL: PlatformStack.spec.pin expected "%s", got "%s"\n' \
            "$expected" "$actual" >&2
        kubectl -n "$PLATFORM_NS" describe platformstacks.apprafter.io default >&2 || true
        exit 1
    fi
}

# ---------------------------------------------------------------
# Helper: delete a MigrationPlan (for cleanup between paths).
# ---------------------------------------------------------------

delete_migration_plan() {
    local plan_name="$1"
    kubectl delete migrationplans.apprafter.io "$plan_name" \
        -n "$PLATFORM_NS" --ignore-not-found >/dev/null
    printf '  deleted MigrationPlan %s\n' "$plan_name"
}

# ---------------------------------------------------------------
# Phase 0: bring up the k3d cluster with a local OCI registry.
#
# k3d's --registry-create flag creates a Docker container named
# <registry-name> running a distribution/registry image, then
# injects the registry's hostname into /etc/hosts on every k3d
# node so that containerd inside k3d can pull from it without
# TLS (plain HTTP, allowed via k3d's automatic registries.yaml
# ConfigMap). The host pushes to localhost:<port>.
# ---------------------------------------------------------------

phase "Phase 0: k3d_up ${CLUSTER_NAME} + local OCI registry"

K3D="$(k3d_bin)"

# shellcheck disable=SC2086
$K3D cluster create "$CLUSTER_NAME" \
    --servers 1 --agents 0 \
    --port "8080:80@loadbalancer" \
    --port "8443:443@loadbalancer" \
    --k3s-arg "--disable=traefik@server:0" \
    --k3s-arg "--disable=servicelb@server:0" \
    --registry-create "${REGISTRY_NAME}:0.0.0.0:${REGISTRY_HOST_PORT}"

printf '  k3d cluster %s is ready with registry %s\n' \
    "$CLUSTER_NAME" "$REGISTRY_NAME"

# Write out the kubeconfig.
# shellcheck disable=SC2086
$K3D kubeconfig write "$CLUSTER_NAME" --output "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

# ---------------------------------------------------------------
# Phase 1: seed CLI state and run cluster-bootstrap
# ---------------------------------------------------------------

phase "Phase 1: cluster-bootstrap (Cilium -> Argo CD -> platform-stack)"

kubeconfig_content=$(cat "$KUBECONFIG_FILE")
seed_apprafter_state "$kubeconfig_content"
export APPRAFTER_CONFIG_DIR

printf '  APPRAFTER_CONFIG_DIR=%s\n' "$APPRAFTER_CONFIG_DIR"
printf '  Running cluster-bootstrap...\n'

apprafter cluster-bootstrap

printf '  cluster-bootstrap complete\n'

# ---------------------------------------------------------------
# Phase 2: wait for PlatformStack/default to exist and for the
# PlatformController to populate its status.
# ---------------------------------------------------------------

phase "Phase 2: wait for PlatformStack/default + controller active"

printf '  waiting for PlatformStack/default ...\n'
deadline=$(( $(date +%s) + 600 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    if kubectl -n "$PLATFORM_NS" get platformstacks.apprafter.io default \
            >/dev/null 2>&1; then
        printf '  PlatformStack/default -> found\n'
        break
    fi
    sleep 10
done
kubectl -n "$PLATFORM_NS" get platformstacks.apprafter.io default >/dev/null 2>&1 || {
    printf 'ERROR: PlatformStack/default not found after 10 min\n' >&2
    exit 1
}

# Wait for currentVersion to be populated (signals controller is up).
printf '  waiting for PlatformController to populate status.currentVersion ...\n'
deadline=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    local_cv=$(kubectl -n "$PLATFORM_NS" get platformstacks.apprafter.io default \
        -o jsonpath='{.status.currentVersion}' 2>/dev/null || true)
    if [ -n "$local_cv" ]; then
        printf '  status.currentVersion = %s\n' "$local_cv"
        break
    fi
    sleep 10
done
cv=$(kubectl -n "$PLATFORM_NS" get platformstacks.apprafter.io default \
    -o jsonpath='{.status.currentVersion}' 2>/dev/null || true)
if [ -z "$cv" ]; then
    printf 'ERROR: status.currentVersion never populated (PlatformController not running?)\n' >&2
    kubectl -n "$PLATFORM_NS" describe platformstacks.apprafter.io default >&2 || true
    kubectl -n "$PLATFORM_NS" logs -l app.kubernetes.io/name=apprafter-operator \
        --tail=50 2>/dev/null >&2 || true
    exit 1
fi

# ---------------------------------------------------------------
# Phase 3: build + push fixture charts to the local OCI registry
# ---------------------------------------------------------------

phase "Phase 3: build + push fixture charts (0.2.0 safe, 0.3.0 breaking)"

mkdir -p "$CHART_BUILD_DIR"

build_and_push_fixture_chart "$BASELINE_VERSION" "safe"
build_and_push_fixture_chart "$BREAKING_VERSION" "breaking"

printf '  both fixture charts available in local registry\n'

# ---------------------------------------------------------------
# Phase 4: point PlatformStack at local registry, pin to 0.2.0
#
# We redirect source.repoURL to the in-cluster registry name so
# PlatformController's availability poll uses the local registry.
# Setting checkInterval to "1m" allows the controller to detect
# 0.3.0 quickly without waiting the default 6h.
# ---------------------------------------------------------------

phase "Phase 4: redirect PlatformStack to local registry, pin 0.2.0"

LOCAL_REPO_URL="oci://${REGISTRY_IN_CLUSTER}/${CHART_NAME}"
patch_platformstack "$LOCAL_REPO_URL" "$BASELINE_VERSION"

# Brief pause to allow the controller to reconcile the new spec.
printf '  waiting 15s for controller to reconcile pin ...\n'
sleep 15

assert_platform_pin "$BASELINE_VERSION"

# ---------------------------------------------------------------
# Phase 5: push 0.3.0 is already done in Phase 3. Wait for
# PlatformController to detect it and create a MigrationPlan.
#
# The controller polls source.repoURL with the cadence from
# checkInterval (we set 1m). With a 5-min window and a 1m poll
# interval, we have at least 4 poll cycles to catch the plan.
# ---------------------------------------------------------------

phase "Phase 5: wait for PlatformController to create MigrationPlan"

printf '  Waiting up to 5 min for MigrationPlan to appear...\n'
printf '  (controller polls local registry every 1 min)\n'

PLAN_NAME="$(wait_for_platform_migration_plan "$BASELINE_VERSION" "$BREAKING_VERSION")"

# ASSERT: plan has scope.type == "platform"
SCOPE_TYPE=$(kubectl -n "$PLATFORM_NS" get migrationplans.apprafter.io "$PLAN_NAME" \
    -o jsonpath='{.spec.scope.type}' 2>/dev/null || true)
if [ "$SCOPE_TYPE" = "platform" ]; then
    printf '  ASSERT PASS: MigrationPlan %s scope.type == "platform"\n' "$PLAN_NAME"
else
    printf 'ASSERT FAIL: MigrationPlan %s scope.type expected "platform", got "%s"\n' \
        "$PLAN_NAME" "$SCOPE_TYPE" >&2
    kubectl -n "$PLATFORM_NS" describe migrationplans.apprafter.io "$PLAN_NAME" >&2 || true
    exit 1
fi

# ASSERT: plan is in pending-approval phase
PLAN_PHASE=$(kubectl -n "$PLATFORM_NS" get migrationplans.apprafter.io "$PLAN_NAME" \
    -o jsonpath='{.status.phase}' 2>/dev/null || true)
printf '  MigrationPlan %s phase: %s\n' "$PLAN_NAME" "${PLAN_PHASE:-<empty>}"
# pending-approval may be empty if the controller hasn't written status yet;
# anything other than "rejected" or "completed" is acceptable here.
if [ "$PLAN_PHASE" = "rejected" ] || [ "$PLAN_PHASE" = "completed" ]; then
    printf 'ASSERT FAIL: MigrationPlan %s phase is already "%s" at creation\n' \
        "$PLAN_NAME" "$PLAN_PHASE" >&2
    exit 1
fi

# ---------------------------------------------------------------
# Phase 6: APPROVE path
#
# apprafter migration approve <plan> transitions phase to
# "approved" -> "executing" -> "completed". The PlatformController
# then patches spec.pin to 0.3.0 (the MigrationPlan's trigger.to).
# We wait up to 5 min for the pin to settle.
# ---------------------------------------------------------------

phase "Phase 6: APPROVE path — apprafter migration approve ${PLAN_NAME}"

apprafter migration approve "$PLAN_NAME"
printf '  approve command issued\n'

printf '  waiting for PlatformStack.spec.pin to advance to %s ...\n' \
    "$BREAKING_VERSION"
deadline=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    new_pin=$(kubectl -n "$PLATFORM_NS" get platformstacks.apprafter.io default \
        -o jsonpath='{.spec.pin}' 2>/dev/null || true)
    if [ "$new_pin" = "$BREAKING_VERSION" ]; then
        printf '  spec.pin -> %s (upgrade accepted)\n' "$BREAKING_VERSION"
        break
    fi
    printf '    %s: spec.pin=%s, waiting...\n' "$(date +%H:%M:%S)" "${new_pin:-<unset>}"
    sleep 10
done
assert_platform_pin "$BREAKING_VERSION"

# Clean up the approved plan before the reject path.
delete_migration_plan "$PLAN_NAME"

# ---------------------------------------------------------------
# Phase 7: REJECT path
#
# Reset pin to 0.2.0 so the controller detects 0.3.0 as available
# again and creates a new MigrationPlan. Then reject it and assert
# that spec.pin reverts to the snapshot (0.2.0).
# ---------------------------------------------------------------

phase "Phase 7: REJECT path — reset pin, wait for new MigrationPlan, reject"

printf '  resetting spec.pin to %s to re-trigger detection...\n' "$BASELINE_VERSION"
patch_platformstack "$LOCAL_REPO_URL" "$BASELINE_VERSION"
sleep 15
assert_platform_pin "$BASELINE_VERSION"

printf '  waiting for second MigrationPlan ...\n'
PLAN_NAME2="$(wait_for_platform_migration_plan "$BASELINE_VERSION" "$BREAKING_VERSION")"

printf '  issuing: apprafter migration reject %s\n' "$PLAN_NAME2"
apprafter migration reject "$PLAN_NAME2"
printf '  reject command issued\n'

# The PlatformMigrationStrategy.reject() reverts spec.pin to the
# previousSpecSnapshot.pin value. Since we set pin=0.2.0 before
# the plan was created, the snapshot should be "0.2.0" and the
# controller should leave (or restore) pin at 0.2.0.
printf '  waiting for PlatformStack.spec.pin to stay/revert to %s ...\n' \
    "$BASELINE_VERSION"
# Allow 2 min for the reject to process.
deadline=$(( $(date +%s) + 120 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    reject_phase=$(kubectl -n "$PLATFORM_NS" \
        get migrationplans.apprafter.io "$PLAN_NAME2" \
        -o jsonpath='{.status.phase}' 2>/dev/null || true)
    if [ "$reject_phase" = "rejected" ]; then
        printf '  MigrationPlan %s -> phase=rejected\n' "$PLAN_NAME2"
        break
    fi
    printf '    %s: plan phase=%s, waiting...\n' "$(date +%H:%M:%S)" \
        "${reject_phase:-<empty>}"
    sleep 10
done

# Assert plan is rejected.
final_phase=$(kubectl -n "$PLATFORM_NS" \
    get migrationplans.apprafter.io "$PLAN_NAME2" \
    -o jsonpath='{.status.phase}' 2>/dev/null || true)
if [ "$final_phase" = "rejected" ]; then
    printf '  ASSERT PASS: MigrationPlan %s phase == "rejected"\n' "$PLAN_NAME2"
else
    printf 'ASSERT FAIL: MigrationPlan %s phase expected "rejected", got "%s"\n' \
        "$PLAN_NAME2" "${final_phase:-<empty>}" >&2
    kubectl -n "$PLATFORM_NS" describe migrationplans.apprafter.io "$PLAN_NAME2" >&2 || true
    exit 1
fi

# Assert pin was restored to the snapshot value.
assert_platform_pin "$BASELINE_VERSION"

# ---------------------------------------------------------------
# Done — tear down on success path
# ---------------------------------------------------------------

trap - EXIT

if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    k3d_down "$CLUSTER_NAME" || true
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving k3d cluster %s up.\n' \
        "$CLUSTER_NAME"
fi

rm -rf "$TMPDIR_WORK"

printf '\nmigration-platform GREEN in %s\n' "$(elapsed)"
printf 'Gate proven: pin -> breaking -> MigrationPlan -> approve + reject\n'
