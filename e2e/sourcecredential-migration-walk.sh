#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter sourcecredential-migration-walk e2e — the operator-side
# 2.16b-sc SourceCredential-scope coverage-narrowing migration gate on a
# local kind/k3d cluster (plan item 2.16b-sc, spec.md §3.8, ADR 0039 +
# the app-scope migration ADR). This walk is the empirical gate for
# 2.16b-sc and closes 1.79c acceptance #4.
#
# What it validates (the shipped gate)
# ------------------------------------
# A destructive coverage-NARROWING of a SourceCredential — removing a
# `spec.git.repoPrefixes` OR `spec.registry.hosts` entry — auto-creates a
# `sourcecredential`-scope MigrationPlan in the credential's namespace,
# PAUSES BOTH derived-Secret derivations (the git `repo-creds` Secret(s)
# in the `argocd` ns AND the registry `dockerconfigjson` pull-secret in
# the credential's own ns), and LEAVES the old wider-coverage derived
# Secrets untouched so in-flight apps keep git-clone / image-pull access.
# The gate consumes on approve (re-derive with the narrowed coverage +
# stamp the new baseline) and is approve-only (reject == re-widen the
# spec in Git → the stale plan self-deletes). The gate is
# ACTOR-AGNOSTIC: a raw `kubectl edit`/`patch` (not the CLI) trips it too.
#
# The chain:
#
#   seed a SourceCredential covering TWO registry hosts + TWO git repo
#     prefixes, with sealed material -> the controller derives BOTH halves
#     (2 argocd repo-creds Secrets + 1 dockerconfigjson pull-secret) and
#     stamps status.lastAppliedSpec (baseline) + coveredHosts /
#     coveredRepoPrefixes list all four ->
#   REMOVE one covering registry host (coverage narrowing) ->
#     a sourcecredential-scope MigrationPlan appears in the cred ns
#       (scope.type=sourcecredential, scope.sourcecredential.ref.name,
#        ownerRef->SourceCredential), the SC phase=AwaitingMigrationApproval,
#       and BOTH derived Secrets are UNCHANGED (captured resourceVersion +
#       content byte-identical before/after — the pause left them put) ->
#   plan shape: classification=breaking, changes[].type=coverage-removal,
#     approvedSpecHash present, controller ownerRef (controller=true+uid) ->
#   `apprafter migration list` shows the plan with SCOPE=sourcecredential ->
#   `apprafter migration approve <plan>` (auto-resolving the cred ns) ->
#     the plan is consumed (deleted), the SC leaves AwaitingMigrationApproval,
#     the derivation RE-RUNS with the narrowed coverage (the removed host's
#     `auths` entry is GONE from the dockerconfigjson; coveredHosts no longer
#     lists it), status.lastAppliedSpec re-stamped ->
#   ACTOR-AGNOSTIC: remove ANOTHER covering entry via RAW kubectl patch (not
#     the CLI) -> the SAME gate trips (a fresh sourcecredential plan appears,
#     the derivation pauses) ->
#   WIDEN-BACK: re-ADD the removed entry on a pending plan -> the stale plan
#     self-deletes (DeleteThenRender) + the SC un-pauses + derivation resumes.
#
# Local-operator mode (REQUIRED — the released chart predates 2.16b-sc)
# --------------------------------------------------------------------
# The released platform-stack chart's SourceCredential controller cannot
# detect/gate/consume a coverage-narrowing change (no lastAppliedSpec
# baseline, no sourcecredential MigrationPlan scope), and its MigrationPlan
# CRD may lack the `sourcecredential` scope shape. So this walk ALWAYS
# builds + side-loads the working-tree apprafter-operator (which carries the
# SourceCredential controller + the MigrationController) + the branch
# admission-webhook (whose MigrationPlan validator carries the
# sourcecredential-scope approve-only status guard) + the BRANCH-rendered
# CRDs + the branch ValidatingWebhookConfiguration. Mirrors the P0 of
# e2e/app-migration-walk.sh exactly.
#
# NO Cilium (bootstrap_with_retry sets APPRAFTER_BOOTSTRAP_SKIP_CILIUM=1) —
# SourceCredential derivation does NOT need a CNI dataplane, so NO
# sandbox-run microVM is needed. Run it plainly:
#   export KIND_EXPERIMENTAL_PROVIDER=podman   # (or docker)
#   bash e2e/sourcecredential-migration-walk.sh
#
# Infeasible-on-kind SOFT-skip: the git/registry VALIDITY probe (S5) needs
# real egress to github.com / ghcr.io, which kind lacks — so GitValid /
# RegistryValid land Unverified. The gate mechanics (plan created, both
# Secrets untouched on pause, re-derived narrowed on approve, GC on widen)
# are the HARD assertions and never depend on the probe verdict.
#
# Exit codes: 0 chain green / 1 assertion failure / 2 precondition missing.
# PASS/FAIL is judged by READING THE LOG: every phase prints `ok:` lines,
# the final `sourcecredential-migration-walk GREEN` banner prints only on
# the success path, and any failure prints `FAILED:` before teardown. Do
# NOT trust the exit code alone.

set -euo pipefail

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-sc-mig-walk"

SC_NS="sc-walk"                     # the credential's namespace (SC-scope plans live here)
SC_NAME="walkcreds"                 # the SourceCredential CR name
SC_MATERIAL="srccred-${SC_NAME}-material"   # unsealed material Secret (backend sealedSecretRef)

# Coverage: TWO registry hosts + TWO git repo prefixes. The narrowing edits
# drop one at a time; the survivor must stay covered throughout.
HOST_KEEP="ghcr.io/walkorg/"
HOST_DROP="registry.example.com/walkorg/"
HOST_KEEP_HN="ghcr.io"                     # registry_hostname(HOST_KEEP) — the dockerconfigjson auths key
HOST_DROP_HN="registry.example.com"        # registry_hostname(HOST_DROP)
PREFIX_KEEP="github.com/walkorg/"
PREFIX_DROP="gitlab.example.com/walkorg/"  # dropped in P6 (actor-agnostic, via raw kubectl)

# Derived-Secret names the controller writes (deterministic — see
# operator-controllers/sourcecredential: pull_secret_name / repo_cred_secret_name).
PULL_SECRET="srccred-${SC_NAME}-dockercfg"   # dockerconfigjson, in $SC_NS
REPO_SECRET_0="srccred-${SC_NAME}-repo-0"    # repo-creds for repoPrefixes[0], in argocd
REPO_SECRET_1="srccred-${SC_NAME}-repo-1"    # repo-creds for repoPrefixes[1], in argocd
ARGOCD_NS="argocd"

PROVIDER_NS="apprafter-system"      # operator + webhook + sealed-secrets ns

# The MigrationPlan CRD kind. Address the full name for clarity.
PLAN_RES="migrationplan"

# Group-qualified: a bare `application` also resolves to Argo CD's
# argoproj.io Application, and Argo CD is installed here.
APP_RES="application.apprafter.io"

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

# yq (or the nix fallback) selects CRD/ClusterRole/VWC objects out of the
# branch `helm template` in Phase 0.
_yq() { if command -v yq >/dev/null 2>&1; then yq "$@"; else nix run nixpkgs#yq-go -- "$@"; fi; }

# ---------------------------------------------------------------
# Temp workspace + CLI state seed
# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"

# Set to 1 only after the cluster is up AND $KUBECONFIG points at it (Phase 0).
# Until then dump_diagnostics / k3d_down must NOT run — on a cluster-up failure
# $KUBECONFIG still points at the ambient cluster, and an e2e must never touch
# a non-test cluster.
K3D_CREATED=0

cleanup() {
    local exit_code=$?
    if [ "$exit_code" -ne 0 ]; then
        printf '\nFAILED: sourcecredential-migration-walk FAILED at %s (exit %d)\n' \
            "$(elapsed)" "$exit_code" >&2
        if [ "$K3D_CREATED" -eq 1 ]; then
            dump_diagnostics || true
            # SC-specific diagnostics: the SC, its derived Secrets, and any plan.
            printf '\n----- sourcecredential diagnostics -----\n' >&2
            kubectl -n "$SC_NS" get sourcecredential "$SC_NAME" -o yaml >&2 2>&1 || true
            kubectl -n "$SC_NS" get "$PLAN_RES" -o wide >&2 2>&1 || true
            kubectl -n "$SC_NS" get secret "$PULL_SECRET" -o yaml >&2 2>&1 || true
            kubectl -n "$ARGOCD_NS" get secret -l "apprafter.io/source-credential=${SC_NAME}" >&2 2>&1 || true
            kubectl -n "$PROVIDER_NS" logs deploy/apprafter-operator --tail=120 >&2 2>&1 || true
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
            printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' "$CLUSTER_NAME"
            printf 'Run: <k3d|kind> cluster delete %s\n' "$CLUSTER_NAME"
        fi
    fi
    rm -rf "$TMPDIR_WORK"
    exit "$exit_code"
}
trap cleanup EXIT

# Seed the CLI state store (mirrors app-migration-walk / needs-disk-walk).
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

# wait_jsonpath <kind> <ns> <name> <jsonpath> <want> [timeout]
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

# wait_secret_appears <ns> <name> [timeout]
wait_secret_appears() {
    local ns="$1" name="$2" timeout="${3:-180}" deadline
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait secret %s/%s to appear (timeout %ss) ...\n' "$ns" "$name" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if kubectl -n "$ns" get secret "$name" >/dev/null 2>&1; then
            printf '  ok: secret %s/%s exists\n' "$ns" "$name"
            return 0
        fi
        sleep 5
    done
    printf 'FAILED: secret %s/%s never appeared within %ss\n' "$ns" "$name" "$timeout" >&2
    return 1
}

# wait_gone <kind> <ns> <name> [timeout]
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

# ---- SC-scope MigrationPlan helpers (label-selected in the cred ns) ----

# sc_plan_name — the (<=1) sourcecredential-scope plan name for $SC_NAME in $SC_NS.
sc_plan_name() {
    kubectl -n "$SC_NS" get "$PLAN_RES" \
        -l "apprafter.io/scope=sourcecredential,apprafter.io/source-credential=${SC_NAME}" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# sc_plan_count — number of sourcecredential-scope plans for $SC_NAME in $SC_NS.
sc_plan_count() {
    kubectl -n "$SC_NS" get "$PLAN_RES" \
        -l "apprafter.io/scope=sourcecredential,apprafter.io/source-credential=${SC_NAME}" \
        --no-headers 2>/dev/null | grep -c . || true
}

# sc_wait_plan [timeout] — poll until a sourcecredential-scope plan appears;
# echoes its name on STDOUT (progress -> STDERR so it never pollutes the name).
sc_wait_plan() {
    local timeout="${1:-120}" deadline nm
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait a sourcecredential-scope MigrationPlan for %s to appear (timeout %ss) ...\n' "$SC_NAME" "$timeout" >&2
    while [ "$(date +%s)" -lt "$deadline" ]; do
        nm="$(sc_plan_name)"
        if [ -n "$nm" ]; then
            printf '  ok: MigrationPlan %s/%s exists for %s\n' "$SC_NS" "$nm" "$SC_NAME" >&2
            printf '%s' "$nm"
            return 0
        fi
        sleep 5
    done
    printf 'FAILED: no sourcecredential-scope MigrationPlan for %s appeared within %ss\n' "$SC_NAME" "$timeout" >&2
    kubectl -n "$SC_NS" get "$PLAN_RES" -o wide >&2 2>&1 || true
    return 1
}

# sc_wait_non_paused [timeout] — poll until the SC leaves AwaitingMigrationApproval.
# The render path PRUNES status.phase (build_status emits phase:None), so "not
# paused" means phase is empty OR anything != AwaitingMigrationApproval.
sc_wait_non_paused() {
    local timeout="${1:-120}" deadline phase
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait %s to leave AwaitingMigrationApproval (timeout %ss) ...\n' "$SC_NAME" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        phase=$(jp sourcecredential "$SC_NS" "$SC_NAME" '{.status.phase}')
        if [ "$phase" != "AwaitingMigrationApproval" ]; then
            printf '  ok: %s left the paused phase (phase=%q)\n' "$SC_NAME" "${phase:-<empty>}"
            return 0
        fi
        sleep 5
    done
    printf 'FAILED: %s never left AwaitingMigrationApproval within %ss (last=%q)\n' \
        "$SC_NAME" "$timeout" "${phase:-<empty>}" >&2
    kubectl -n "$SC_NS" describe sourcecredential "$SC_NAME" >&2 2>&1 || true
    return 1
}

# secret_rv <ns> <name> — the Secret's resourceVersion (empty if absent). A
# server-side change to a Secret ALWAYS bumps resourceVersion, so an unchanged
# rv across the coverage-narrowing edit proves the pause did NOT re-derive it.
secret_rv() { jp secret "$1" "$2" '{.metadata.resourceVersion}'; }

# dockercfg_has_host <hostname> — 0 (true) iff the derived dockerconfigjson's
# `.auths` map carries <hostname>. The pull-secret's `.dockerconfigjson` is a
# base64 JSON blob; decode + grep for the auths key.
dockercfg_has_host() {
    local hostname="$1" blob
    blob=$(kubectl -n "$SC_NS" get secret "$PULL_SECRET" \
        -o jsonpath='{.data.\.dockerconfigjson}' 2>/dev/null | base64 -d 2>/dev/null || true)
    printf '%s' "$blob" | grep -q "\"${hostname}\""
}

# apply_sc <hosts-yaml-list> <prefixes-yaml-list> — (re)apply the SourceCredential
# CR with the given registry hosts + git repoPrefixes. Both halves share the one
# sealed material Secret ($SC_MATERIAL) in $SC_NS. Applied via kubectl (raw CR,
# actor-agnostic — no CLI in the write path), so ANY edit path trips the same
# operator gate. The `hosts` / `repoPrefixes` args are the YAML inline-list
# bodies (e.g. '"a/", "b/"').
apply_sc() {
    local hosts="$1" prefixes="$2"
    kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: SourceCredential
metadata:
  name: ${SC_NAME}
  namespace: ${SC_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  git:
    backend:
      sealedSecretRef:
        name: ${SC_MATERIAL}
    repoPrefixes: [${prefixes}]
  registry:
    backend:
      sealedSecretRef:
        name: ${SC_MATERIAL}
    hosts: [${hosts}]
YAML
}

# ===============================================================
# Phase 0: cluster up + bootstrap + branch CRD/RBAC + local operator + webhook
# ===============================================================

phase "Phase 0: kind up + bootstrap + branch CRDs/RBAC + local operator + branch webhook (${CLUSTER_NAME})"

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

# The released chart predates 2.16b-sc. Argo CD owns the operator/webhook CRDs
# + VWC via its Applications, so disable automated sync on them (else Argo
# reverts the branch drift), then apply the BRANCH-rendered CRDs + RBAC + VWC.
printf '  disabling Argo automated-sync on platform + apprafter-operator + admission-webhook ...\n'
for _app in platform apprafter-operator admission-webhook; do
    kubectl -n argocd patch applications.argoproj.io "$_app" --type=merge \
        -p '{"spec":{"syncPolicy":{"automated":null}}}' >/dev/null 2>&1 || true
done

printf '  applying branch operator CRDs + ClusterRole/ClusterRoleBinding ...\n'
# `--namespace apprafter-system` is LOAD-BEARING: without it `helm template`
# renders the ClusterRoleBinding subject namespace as `default`, and a
# --force-conflicts apply would then overwrite the umbrella-installed binding
# that correctly targets the operator SA in apprafter-system (the operator SA
# would lose every cluster-scoped permission). See app-migration-walk P0.
helm template apprafter-operator "${REPO_ROOT}/operator/charts/apprafter-operator" \
    --namespace apprafter-system \
    | _yq 'select(.kind == "CustomResourceDefinition" or .kind == "ClusterRole" or .kind == "ClusterRoleBinding")' \
    | kubectl apply --server-side --force-conflicts -f -
retry 12 5 -- kubectl wait --for=condition=Established \
    crd/sourcecredentials.apprafter.io --timeout=30s
retry 12 5 -- kubectl wait --for=condition=Established \
    crd/migrationplans.apprafter.io --timeout=30s
printf '  branch CRDs Established (sourcecredentials + migrationplans) + operator RBAC applied\n'

# Build + side-load the working-tree operator, then restart it so the branch
# SourceCredential controller + MigrationController (both in the one
# apprafter-operator binary) run against the 2.16b-sc schema.
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
printf '  ok: apprafter-operator now running the working-tree (2.16b-sc) build\n'

# The branch admission-webhook carries the MigrationPlan validator's
# sourcecredential-scope handling + the approve-only status guard
# (validator_migrationplan.rs — validate_sourcecredential_scope +
# status-write allowlist). Its VWC's `migrationplans` rule intercepts
# `migrationplans/status`, so an external `phase→approved` write is validated
# (approve-only). Rebuild + side-load the branch image + VWC.
build_load_restart admission-webhook admission-webhook
printf '  ok: admission-webhook now running the working-tree (2.16b-sc) build\n'

# Side-load the BRANCH-rendered ValidatingWebhookConfiguration so the
# migrationplans/status interception (approve-only guard) is the branch one.
# The template carries cert-manager.io/inject-ca-from, so ca-injector
# repopulates clientConfig.caBundle after the apply — wait for a non-empty
# caBundle on the migrationplans webhook before proceeding.
printf '  applying branch admission-webhook ValidatingWebhookConfiguration ...\n'
helm template admission-webhook "${REPO_ROOT}/operator/charts/apprafter-admission-webhook" \
    --namespace "$PROVIDER_NS" \
    | _yq 'select(.kind == "ValidatingWebhookConfiguration")' \
    | kubectl apply -f -
printf '  waiting for cert-manager to re-inject the caBundle on the migrationplans webhook ...\n'
_vwc_deadline=$(( $(date +%s) + 90 ))
while [ "$(date +%s)" -lt "$_vwc_deadline" ]; do
    _cab=$(kubectl get validatingwebhookconfiguration admission-webhook.apprafter.io \
        -o jsonpath='{range .webhooks[?(@.name=="migrationplans.apprafter.io")]}{.clientConfig.caBundle}{end}' 2>/dev/null || true)
    [ -n "$_cab" ] && break
    sleep 3
done
# Confirm the migrationplans rule lists the status subresource (the branch VWC
# intercepts migrationplans/status → the approve-only guard is reachable).
if kubectl get validatingwebhookconfiguration admission-webhook.apprafter.io \
    -o jsonpath='{range .webhooks[?(@.name=="migrationplans.apprafter.io")]}{.rules[*].resources}{end}' 2>/dev/null \
    | grep -q "migrationplans/status"; then
    printf '  ok: branch VWC live — migrationplans/status intercepted (approve-only guard reachable)\n'
else
    printf 'FAILED: branch VWC did not take (migrationplans/status not listed) — approve-only guard unreachable\n' >&2
    kubectl get validatingwebhookconfiguration admission-webhook.apprafter.io -o yaml >&2 2>&1 || true
    exit 1
fi

retry 30 10 -- kubectl -n "$PROVIDER_NS" rollout status \
    deploy admission-webhook --timeout=60s
retry 30 10 -- kubectl -n "$PROVIDER_NS" rollout status \
    deploy apprafter-operator --timeout=60s
printf '  ok: platform ready (branch operator + webhook)\n'

# The sealed-secrets controller must be Ready before `apprafter secret seal`
# (which fetches its cert via the Service) and before the SC controller can
# unseal the material. Argo may sync it a beat after the webhook.
printf '  waiting for the sealed-secrets controller ...\n'
_ss_deadline=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$_ss_deadline" ]; do
    kubectl -n "$PROVIDER_NS" get deploy sealed-secrets-controller >/dev/null 2>&1 && break
    sleep 10
done
retry 30 10 -- kubectl -n "$PROVIDER_NS" rollout status \
    deploy sealed-secrets-controller --timeout=60s
retry 30 5 -- sh -c "kubectl -n '$PROVIDER_NS' get endpoints sealed-secrets-controller -o jsonpath='{.subsets[0].addresses[0].ip}' 2>/dev/null | grep -q ."
printf '  ok: sealed-secrets controller Ready\n'

# ===============================================================
# Phase 1: seed the SourceCredential + baseline (both halves derived)
# ===============================================================

phase "Phase 1: seed SourceCredential (2 hosts + 2 prefixes) + sealed material -> derive BOTH halves + baseline"

kubectl create namespace "$SC_NS" 2>/dev/null || true

# Seal the material (username/password) into the material Secret the SC's
# backend.sealedSecretRef names — the real seal flow (apprafter secret seal
# fetches the controller cert; the controller unseals it to $SC_MATERIAL).
apprafter secret seal "$SC_MATERIAL" \
    --namespace "$SC_NS" \
    --from-literal "username=walkbot" \
    --from-literal "password=ghp_walk_dummy_token"
# The sealed-secrets controller unseals it to a plain Secret of the same name.
wait_secret_appears "$SC_NS" "$SC_MATERIAL" 120
printf '  ok: sealed material %s/%s unsealed\n' "$SC_NS" "$SC_MATERIAL"

# Create the SourceCredential covering BOTH registry hosts + BOTH git prefixes.
apply_sc "\"${HOST_KEEP}\", \"${HOST_DROP}\"" "\"${PREFIX_KEEP}\", \"${PREFIX_DROP}\""
wait_jsonpath sourcecredential "$SC_NS" "$SC_NAME" '{.metadata.name}' "$SC_NAME" 60

# The controller derives BOTH halves: the registry dockerconfigjson pull-secret
# in $SC_NS, and one repo-creds Secret per git prefix in argocd.
wait_secret_appears "$SC_NS" "$PULL_SECRET" 180
wait_secret_appears "$ARGOCD_NS" "$REPO_SECRET_0" 180
wait_secret_appears "$ARGOCD_NS" "$REPO_SECRET_1" 180
printf '  ok: BOTH halves derived (pull-secret + 2 repo-creds)\n'

# The baseline (status.lastAppliedSpec) stamps after a successful derive — this
# is what every later coverage diff is taken against. Poll for it.
wait_jsonpath sourcecredential "$SC_NS" "$SC_NAME" \
    '{.status.lastAppliedSpec.registry.hosts[0]}' "$HOST_KEEP" 180
printf '  ok: baseline stamped (status.lastAppliedSpec present)\n'

# coveredHosts / coveredRepoPrefixes list all four. The order is the spec order.
covered_hosts=$(jp sourcecredential "$SC_NS" "$SC_NAME" '{.status.coveredHosts[*]}')
printf '  status.coveredHosts = %q\n' "$covered_hosts"
for h in "$HOST_KEEP" "$HOST_DROP"; do
    printf '%s' "$covered_hosts" | grep -qF "$h" || {
        printf 'FAILED: status.coveredHosts %q missing %q\n' "$covered_hosts" "$h" >&2; exit 1; }
done
printf '  ok: coveredHosts lists BOTH registry hosts\n'
covered_prefixes=$(jp sourcecredential "$SC_NS" "$SC_NAME" '{.status.coveredRepoPrefixes[*]}')
printf '  status.coveredRepoPrefixes = %q\n' "$covered_prefixes"
for p in "$PREFIX_KEEP" "$PREFIX_DROP"; do
    printf '%s' "$covered_prefixes" | grep -qF "$p" || {
        printf 'FAILED: status.coveredRepoPrefixes %q missing %q\n' "$covered_prefixes" "$p" >&2; exit 1; }
done
printf '  ok: coveredRepoPrefixes lists BOTH git prefixes\n'

# The derived dockerconfigjson carries an auths entry for BOTH hosts at baseline.
dockercfg_has_host "$HOST_KEEP_HN" || { printf 'FAILED: dockerconfigjson missing %s at baseline\n' "$HOST_KEEP_HN" >&2; exit 1; }
dockercfg_has_host "$HOST_DROP_HN" || { printf 'FAILED: dockerconfigjson missing %s at baseline\n' "$HOST_DROP_HN" >&2; exit 1; }
printf '  ok: dockerconfigjson has auths for BOTH hosts at baseline\n'

# Sanity: no plans yet (nothing destructive happened).
assert_eq "no SC-scope plans yet" "$(sc_plan_count)" "0"
printf 'ok: SC derived + baseline stamped\n'

# ===============================================================
# Phase 2: coverage-removal gates + PAUSES BOTH halves (old Secrets intact)
# ===============================================================

phase "Phase 2: remove one covering host -> gate + PAUSE both derivations (old Secrets untouched)"

# Capture the derived Secrets' state BEFORE the narrowing edit. A server-side
# change to a Secret ALWAYS bumps its resourceVersion — so identical rv AFTER
# proves the pause did NOT re-derive it (both wider-coverage Secrets stayed put).
rv_pull_before=$(secret_rv "$SC_NS" "$PULL_SECRET")
rv_repo0_before=$(secret_rv "$ARGOCD_NS" "$REPO_SECRET_0")
rv_repo1_before=$(secret_rv "$ARGOCD_NS" "$REPO_SECRET_1")
dockercfg_before=$(kubectl -n "$SC_NS" get secret "$PULL_SECRET" \
    -o jsonpath='{.data.\.dockerconfigjson}' 2>/dev/null || true)
printf '  captured pre-edit rv: pull=%s repo0=%s repo1=%s\n' \
    "$rv_pull_before" "$rv_repo0_before" "$rv_repo1_before"

# REMOVE one covering registry host (drop $HOST_DROP; keep $HOST_KEEP + both
# git prefixes). Raw kubectl apply of the narrowed CR.
apply_sc "\"${HOST_KEEP}\"" "\"${PREFIX_KEEP}\", \"${PREFIX_DROP}\""

# (a) A sourcecredential-scope MigrationPlan appears in the cred ns.
P2_PLAN="$(sc_wait_plan 120)"
[ -n "$P2_PLAN" ] || { printf 'FAILED: P2 plan name empty\n' >&2; exit 1; }
p2_scope_type=$(jp "$PLAN_RES" "$SC_NS" "$P2_PLAN" '{.spec.scope.type}')
assert_eq "plan .spec.scope.type" "$p2_scope_type" "sourcecredential"
p2_ref_name=$(jp "$PLAN_RES" "$SC_NS" "$P2_PLAN" '{.spec.scope.sourcecredential.ref.name}')
assert_eq "plan .spec.scope.sourcecredential.ref.name" "$p2_ref_name" "$SC_NAME"
p2_ref_ns=$(jp "$PLAN_RES" "$SC_NS" "$P2_PLAN" '{.spec.scope.sourcecredential.ref.namespace}')
assert_eq "plan .spec.scope.sourcecredential.ref.namespace" "$p2_ref_ns" "$SC_NS"

# ownerRef -> SourceCredential (cascade on SC delete).
p2_owner_kind=$(jp "$PLAN_RES" "$SC_NS" "$P2_PLAN" '{.metadata.ownerReferences[0].kind}')
assert_eq "plan ownerRef[0].kind" "$p2_owner_kind" "SourceCredential"

# (b) The SC pauses.
wait_jsonpath sourcecredential "$SC_NS" "$SC_NAME" '{.status.phase}' AwaitingMigrationApproval 120
printf '  ok: SC paused (phase=AwaitingMigrationApproval) with plan %s/%s\n' "$SC_NS" "$P2_PLAN"

# (c) BOTH derived Secrets are UNCHANGED — the pause left them put. Give the
# operator a few reconcile beats to (wrongly) touch them, then assert the rv +
# content are byte-identical to the pre-edit capture.
sleep 20
rv_pull_after=$(secret_rv "$SC_NS" "$PULL_SECRET")
rv_repo0_after=$(secret_rv "$ARGOCD_NS" "$REPO_SECRET_0")
rv_repo1_after=$(secret_rv "$ARGOCD_NS" "$REPO_SECRET_1")
dockercfg_after=$(kubectl -n "$SC_NS" get secret "$PULL_SECRET" \
    -o jsonpath='{.data.\.dockerconfigjson}' 2>/dev/null || true)
assert_eq "pull-secret resourceVersion unchanged (pause did NOT re-derive)" "$rv_pull_after" "$rv_pull_before"
assert_eq "repo-creds[0] resourceVersion unchanged (pause did NOT re-derive)" "$rv_repo0_after" "$rv_repo0_before"
assert_eq "repo-creds[1] resourceVersion unchanged (pause did NOT re-derive)" "$rv_repo1_after" "$rv_repo1_before"
assert_eq "pull-secret dockerconfigjson byte-identical (still covers the removed host)" "$dockercfg_after" "$dockercfg_before"
# Belt: the old wider-coverage dockerconfigjson STILL carries the removed host
# (in-flight apps keep image-pull access during the pause).
dockercfg_has_host "$HOST_DROP_HN" || {
    printf 'FAILED: paused pull-secret lost the removed host %s (pause should keep wider coverage)\n' "$HOST_DROP_HN" >&2; exit 1; }
printf 'ok: coverage-removal gated + BOTH derivations paused (old Secrets intact)\n'

# ===============================================================
# Phase 3: plan shape (classification / changes / hash / ownerRef)
# ===============================================================

phase "Phase 3: sourcecredential plan shape — classification, changes[], approvedSpecHash, ownerRef"

# risks.classification == breaking (coverage-removal is classified breaking).
p3_class=$(jp "$PLAN_RES" "$SC_NS" "$P2_PLAN" '{.spec.risks.classification}')
assert_eq "plan .spec.risks.classification" "$p3_class" "breaking"

# changes[].type contains coverage-removal (wire field is `type`).
p3_change_types=$(jp "$PLAN_RES" "$SC_NS" "$P2_PLAN" '{.spec.changes[*].type}')
printf '  spec.changes[*].type = %q\n' "$p3_change_types"
if printf '%s' "$p3_change_types" | grep -qw 'coverage-removal'; then
    printf '  ok: spec.changes[] contains coverage-removal\n'
else
    printf 'FAILED: spec.changes[*].type %q missing coverage-removal\n' "$p3_change_types" >&2
    kubectl -n "$SC_NS" get "$PLAN_RES" "$P2_PLAN" -o yaml >&2 2>&1 || true
    exit 1
fi

# approvedSpecHash present on the trigger (S-4 binds approval to the full change set).
p3_hash=$(jp "$PLAN_RES" "$SC_NS" "$P2_PLAN" '{.spec.trigger.approvedSpecHash}')
if [ -n "$p3_hash" ]; then
    printf '  ok: spec.trigger.approvedSpecHash present (%s...)\n' "${p3_hash:0:12}"
else
    printf 'FAILED: spec.trigger.approvedSpecHash is empty\n' >&2; exit 1
fi

# ownerRef controller=true + uid == the SC uid.
p3_owner_ctrl=$(jp "$PLAN_RES" "$SC_NS" "$P2_PLAN" '{.metadata.ownerReferences[0].controller}')
assert_eq "plan ownerRef[0].controller" "$p3_owner_ctrl" "true"
p3_owner_uid=$(jp "$PLAN_RES" "$SC_NS" "$P2_PLAN" '{.metadata.ownerReferences[0].uid}')
sc_uid=$(jp sourcecredential "$SC_NS" "$SC_NAME" '{.metadata.uid}')
assert_eq "plan ownerRef[0].uid == SourceCredential uid" "$p3_owner_uid" "$sc_uid"
printf 'ok: sourcecredential plan shape\n'

# ===============================================================
# Phase 4: CLI list shows the plan with SCOPE=sourcecredential
# ===============================================================

phase "Phase 4: apprafter migration list shows the plan with SCOPE=sourcecredential"

list_out="$(apprafter migration list)"
printf '%s\n' "$list_out"
# The row must carry the plan name, its ns, and SCOPE=sourcecredential.
if printf '%s' "$list_out" | grep -q "$P2_PLAN" \
    && printf '%s' "$list_out" | grep -q "$SC_NS" \
    && printf '%s' "$list_out" | grep -q "sourcecredential"; then
    printf '  ok: migration list shows %s in %s with SCOPE=sourcecredential\n' "$P2_PLAN" "$SC_NS"
else
    printf 'FAILED: migration list did not show plan %s / ns %s / SCOPE sourcecredential\n%s\n' \
        "$P2_PLAN" "$SC_NS" "$list_out" >&2
    exit 1
fi
printf 'ok: migration list shows sourcecredential plan\n'

# ===============================================================
# Phase 5: approve -> consume + re-derive narrowed + GC the plan
# ===============================================================

phase "Phase 5: apprafter migration approve -> consume + re-derive narrowed + GC"

# Approve WITHOUT -n to exercise the CLI's namespace auto-resolution (the plan
# lives in $SC_NS, not apprafter-system — the resolver must find it).
printf '  approving %s via apprafter migration approve (auto-resolving the namespace) ...\n' "$P2_PLAN"
apprafter migration approve "$P2_PLAN"

# The SC leaves AwaitingMigrationApproval (consume -> re-derive -> stamp).
sc_wait_non_paused 120

# The plan is consumed (deleted) within ~90s.
wait_gone "$PLAN_RES" "$SC_NS" "$P2_PLAN" 90

# The derivation RE-RAN with the narrowed coverage: the dockerconfigjson now
# carries ONLY the surviving host; the removed host's auths entry is GONE.
_narrow_deadline=$(( $(date +%s) + 120 ))
while [ "$(date +%s)" -lt "$_narrow_deadline" ]; do
    if dockercfg_has_host "$HOST_KEEP_HN" && ! dockercfg_has_host "$HOST_DROP_HN"; then
        break
    fi
    sleep 5
done
dockercfg_has_host "$HOST_KEEP_HN" || {
    printf 'FAILED: re-derived pull-secret lost the surviving host %s\n' "$HOST_KEEP_HN" >&2; exit 1; }
if dockercfg_has_host "$HOST_DROP_HN"; then
    printf 'FAILED: re-derived pull-secret STILL carries the removed host %s (narrowing did not apply)\n' "$HOST_DROP_HN" >&2
    kubectl -n "$SC_NS" get secret "$PULL_SECRET" -o jsonpath='{.data.\.dockerconfigjson}' | base64 -d >&2 2>&1 || true
    exit 1
fi
printf '  ok: re-derived dockerconfigjson covers ONLY the surviving host\n'

# coveredHosts no longer lists the removed host; baseline re-stamped to narrowed.
wait_jsonpath sourcecredential "$SC_NS" "$SC_NAME" \
    '{.status.lastAppliedSpec.registry.hosts[0]}' "$HOST_KEEP" 120
covered_after=$(jp sourcecredential "$SC_NS" "$SC_NAME" '{.status.coveredHosts[*]}')
printf '  status.coveredHosts after approve = %q\n' "$covered_after"
if printf '%s' "$covered_after" | grep -qF "$HOST_DROP"; then
    printf 'FAILED: status.coveredHosts still lists the removed host %q after approve\n' "$HOST_DROP" >&2; exit 1
fi
printf '%s' "$covered_after" | grep -qF "$HOST_KEEP" || {
    printf 'FAILED: status.coveredHosts dropped the surviving host %q\n' "$HOST_KEEP" >&2; exit 1; }
# Baseline no longer carries the removed host (only one host remains).
base_hosts_after=$(jp sourcecredential "$SC_NS" "$SC_NAME" '{.status.lastAppliedSpec.registry.hosts[*]}')
if printf '%s' "$base_hosts_after" | grep -qF "$HOST_DROP"; then
    printf 'FAILED: re-stamped baseline still carries the removed host %q\n' "$HOST_DROP" >&2; exit 1
fi
printf '  ok: coveredHosts + baseline re-stamped to the narrowed coverage\n'

# Anti-loop: no NEW plan reappears (the baseline was re-stamped to the narrowed
# spec, so the next reconcile detects no change).
printf '  watching ~25s for an anti-loop regression (no new plan should appear) ...\n'
sleep 25
assert_eq "anti-loop: no new plan after approve" "$(sc_plan_count)" "0"
printf 'ok: approve consumes + re-derives narrowed + GC\n'

# ===============================================================
# Phase 6: actor-agnostic — a RAW kubectl edit trips the SAME gate
# ===============================================================

phase "Phase 6: raw kubectl patch (not the CLI) removing a git prefix trips the SAME gate"

# Remove a covering GIT prefix ($PREFIX_DROP) via a RAW kubectl patch — NOT the
# CLI. The gate keys on the spec diff vs baseline, not on the actor, so this
# must gate identically. Use a strategic-merge that overwrites repoPrefixes to
# the single surviving prefix.
kubectl -n "$SC_NS" patch sourcecredential "$SC_NAME" --type=merge \
    -p "{\"spec\":{\"git\":{\"repoPrefixes\":[\"${PREFIX_KEEP}\"]}}}"

# A FRESH sourcecredential-scope plan appears + the SC pauses.
P6_PLAN="$(sc_wait_plan 120)"
[ -n "$P6_PLAN" ] || { printf 'FAILED: P6 plan name empty\n' >&2; exit 1; }
p6_scope=$(jp "$PLAN_RES" "$SC_NS" "$P6_PLAN" '{.spec.scope.type}')
assert_eq "P6 plan scope.type" "$p6_scope" "sourcecredential"
p6_field=$(jp "$PLAN_RES" "$SC_NS" "$P6_PLAN" '{.spec.trigger.field}')
printf '  P6 plan trigger.field = %q\n' "$p6_field"
if printf '%s' "$p6_field" | grep -q 'repoPrefixes'; then
    printf '  ok: P6 plan trigger.field names the git repoPrefixes change\n'
else
    printf '  note: P6 trigger.field=%q (field label may combine halves) — the gate tripped, which is the assertion\n' "$p6_field"
fi
wait_jsonpath sourcecredential "$SC_NS" "$SC_NAME" '{.status.phase}' AwaitingMigrationApproval 120

# The pause left the surviving repo-creds Secret in place (repo-0 for the kept
# prefix). It must still exist while gated.
kubectl -n "$ARGOCD_NS" get secret "$REPO_SECRET_0" >/dev/null 2>&1 || {
    printf 'FAILED: surviving repo-creds %s vanished during the git-prefix pause\n' "$REPO_SECRET_0" >&2; exit 1; }
printf 'ok: raw kubectl edit trips the gate (actor-agnostic)\n'

# ===============================================================
# Phase 7: widen-back GCs the stale plan + un-pauses
# ===============================================================

phase "Phase 7: re-add the removed prefix (widen back) -> stale plan self-deletes + un-pauses"

# On the still-pending P6 plan, re-ADD the dropped git prefix (widen back to the
# baseline coverage). The destructive delta vanishes -> DeleteThenRender: the
# operator GCs the stale plan, derives normally, and un-pauses.
kubectl -n "$SC_NS" patch sourcecredential "$SC_NAME" --type=merge \
    -p "{\"spec\":{\"git\":{\"repoPrefixes\":[\"${PREFIX_KEEP}\",\"${PREFIX_DROP}\"]}}}"

# The stale plan self-deletes.
wait_gone "$PLAN_RES" "$SC_NS" "$P6_PLAN" 90

# The SC un-pauses (leaves AwaitingMigrationApproval) and derivation resumes:
# the re-widened second repo-creds Secret is (re)derived.
sc_wait_non_paused 120
wait_secret_appears "$ARGOCD_NS" "$REPO_SECRET_1" 120
# And no plan lingers (widen-back left a clean state).
assert_eq "no SC-scope plan after widen-back" "$(sc_plan_count)" "0"
printf 'ok: widen-back GCs the stale plan + un-pauses\n'

# ===============================================================
# Phase 8: the pull-secret copy is reference-counted, not orphaned (D13)
# ===============================================================
phase "Phase 8: two apps share one pull-secret copy -> it dies only when the LAST one does"

# D13: the copy the Application controller projects into the app namespace is
# named after the CREDENTIAL, so every app in that namespace pulling through it
# shares one object. Before 2.22b it had no owner at all: a controlling
# ownerRef from any single Application would cascade-delete a Secret its
# neighbours still need, that was correctly avoided, and then nobody assigned
# an owner instead. The Secret outlived every consumer forever.
#
# The shipped fix uses the fact that `ownerReferences` is a LIST: one
# NON-controller entry per consuming Application, and the apiserver's garbage
# collector removes a dependent only when ALL its owners are gone. Reference
# counting, executed by the apiserver.
#
# ONE app cannot show that. With a single owner, "dies with its owner" and
# "reference counted" are the same observation — so this phase runs two, and
# the load-bearing assertion is the MIDDLE one: after the first deletion the
# Secret must still be there.
D13_NS="pullref"
D13_APP_A="puller-a"
D13_APP_B="puller-b"
D13_CRED="pullcreds"                       # lives in apprafter-system, see below
D13_MATERIAL="srccred-${D13_CRED}-material"
D13_DERIVED="srccred-${D13_CRED}-dockercfg"  # the canonical derived pull-secret
D13_COPY="srccred-${D13_CRED}-pull"          # app_pull_secret_name(cred_name)

# THE CREDENTIAL MUST LIVE IN apprafter-system, AND THE ONE THIS WALK HAS DOES
# NOT. `list_source_credentials` reads exactly one namespace —
# `SOURCECRED_NAMESPACE = "apprafter-system"` (application/src/lib.rs:2111,
# :2167-2169) — while this walk deliberately keeps its credential in $SC_NS to
# prove that SC-scope MigrationPlans land in the credential's own namespace.
#
# So the Application controller cannot see $SC_NAME at all, and a phase built
# on it would assert a copy the product has no path to create: green never,
# and for a reason having nothing to do with reference counting. Found by
# running it — the apps came up Ready with zero owners on a Secret that was
# never written.
#
# Hence a second, self-contained credential in the namespace the controller
# actually reads. It shares nothing with the migration-gate fixture above.
kubectl create namespace "$D13_NS" --dry-run=client -o yaml | kubectl apply -f -

apprafter secret seal "$D13_MATERIAL" \
    --namespace "$PROVIDER_NS" \
    --from-literal "username=pullbot" \
    --from-literal "password=ghp_pull_dummy_token"
wait_secret_appears "$PROVIDER_NS" "$D13_MATERIAL" 120
printf '  ok: sealed material %s/%s unsealed\n' "$PROVIDER_NS" "$D13_MATERIAL"

kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: SourceCredential
metadata:
  name: ${D13_CRED}
  namespace: ${PROVIDER_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  registry:
    backend:
      sealedSecretRef:
        name: ${D13_MATERIAL}
    hosts: ["${HOST_KEEP}"]
YAML

# The controller derives the canonical pull-secret before any app can consume
# it; `attach_pull_secret` defers (and logs) while it is absent, so waiting
# here is what separates "not derived yet" from "not covered at all".
wait_secret_appears "$PROVIDER_NS" "$D13_DERIVED" 180
printf '  ok: derived pull-secret %s/%s present\n' "$PROVIDER_NS" "$D13_DERIVED"

# Both images sit under the host the credential still covers after Phase 5's
# narrowing ($HOST_KEEP), so the controller finds a covering credential. They
# never need to PULL — the copy is written during reconcile, long before the
# kubelet tries — so an unpullable tag is fine and keeps the walk offline.
for _app in "$D13_APP_A" "$D13_APP_B"; do
    kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${_app}
  namespace: ${D13_NS}
spec:
  base:
    image: ${HOST_KEEP}${_app}:v1
    replicas: 1
YAML
done
printf '  applied %s + %s in %s (images under %s)\n' \
    "$D13_APP_A" "$D13_APP_B" "$D13_NS" "$HOST_KEEP"

# Wait for the copy to appear AND to carry both owners.
#
# Read the names FIRST and count second. `kubectl | wc -w` looks harmless and
# is not: under `set -o pipefail` a kubectl that exits non-zero (which it does
# for every poll before the Secret exists) makes the whole pipeline non-zero,
# the assignment inherits that status, and `set -e` kills the walk on the first
# iteration — one second in, with no error of its own. `2>/dev/null` hides the
# message and not the status. Same family as the `grep -q` SIGPIPE trap.
owner_names() {
    kubectl -n "$D13_NS" get secret "$D13_COPY" \
        -o jsonpath='{.metadata.ownerReferences[*].name}' 2>/dev/null || true
}
_d13_deadline=$(( $(date +%s) + 180 ))
owners=0
while [ "$(date +%s)" -lt "$_d13_deadline" ]; do
    owners=$(owner_names | wc -w | tr -d ' ')
    [ "${owners:-0}" -ge 2 ] && break
    sleep 5
done
if [ "$owners" -lt 2 ]; then
    printf 'ERROR: pull-secret copy %s/%s never gathered both owners (saw %s)\n' \
        "$D13_NS" "$D13_COPY" "$owners" >&2
    kubectl -n "$D13_NS" get secret "$D13_COPY" -o yaml >&2 2>&1 || true
    kubectl -n "$D13_NS" get "$APP_RES" -o wide >&2 2>&1 || true
    exit 1
fi
printf '  ok: the copy carries %s ownerReferences, one per consuming Application\n' "$owners"

# Not one of them may be the controller — a controlling ref is exactly the
# cascade this design refuses.
controllers=$( { kubectl -n "$D13_NS" get secret "$D13_COPY" \
    -o jsonpath='{.metadata.ownerReferences[?(@.controller==true)].name}' 2>/dev/null || true; } | wc -w | tr -d ' ')
assert_eq "no owner is the CONTROLLER (a cascade would evict the neighbour)" "$controllers" "0"

# THE ASSERTION THIS PHASE EXISTS FOR. Delete one app; the Secret must survive,
# because the other app is still pulling through it.
kubectl -n "$D13_NS" delete "$APP_RES" "$D13_APP_A" --wait=true
sleep 10
if ! kubectl -n "$D13_NS" get secret "$D13_COPY" >/dev/null 2>&1; then
    printf 'ERROR: the pull-secret copy was deleted with the FIRST app — %s can no longer pull\n' \
        "$D13_APP_B" >&2
    printf '  that is the cascade the non-controller ownerRef design exists to prevent\n' >&2
    exit 1
fi
owners_after=$(owner_names | wc -w | tr -d ' ')
assert_eq "one owner released, the copy survives for the remaining consumer" "$owners_after" "1"

# Delete the last consumer; the apiserver's GC must now reclaim it. This is the
# half D13 opened on: before the fix it survived here forever, in every
# namespace that had ever pulled through the credential.
kubectl -n "$D13_NS" delete "$APP_RES" "$D13_APP_B" --wait=true
_gc_deadline=$(( $(date +%s) + 120 ))
gone=0
while [ "$(date +%s)" -lt "$_gc_deadline" ]; do
    if ! kubectl -n "$D13_NS" get secret "$D13_COPY" >/dev/null 2>&1; then gone=1; break; fi
    sleep 5
done
if [ "$gone" -ne 1 ]; then
    printf 'ERROR: the pull-secret copy outlived its LAST consumer — the D13 orphan is back\n' >&2
    kubectl -n "$D13_NS" get secret "$D13_COPY" -o yaml >&2 2>&1 || true
    exit 1
fi
printf '  ok: the last deletion reclaimed the copy — no dockerconfigjson orphan in %s\n' "$D13_NS"

# The generalised form the ledger asked for: nothing of that type survives.
leftover=$(kubectl -n "$D13_NS" get secrets \
    --field-selector type=kubernetes.io/dockerconfigjson \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || true)
assert_eq "no dockerconfigjson Secret survives in the namespace" "$leftover" ""

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

printf '\nsourcecredential-migration-walk GREEN in %s\n' "$(elapsed)"
printf 'Chain proven: seed SC (2 hosts + 2 prefixes) + sealed material -> derive BOTH halves + baseline stamped -> remove a host -> sourcecredential-scope plan gated + BOTH derivations paused (old Secrets byte-identical) -> plan shape (breaking / coverage-removal / approvedSpecHash / SC ownerRef) -> migration list SCOPE=sourcecredential -> approve (consume + re-derive narrowed + GC + anti-loop) -> raw kubectl edit trips the SAME gate (actor-agnostic) -> widen-back GCs the stale plan + un-pauses -> two apps share one pull-secret copy, it survives the first deletion and is reclaimed on the last (D13)\n'
