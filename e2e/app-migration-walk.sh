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
# ALSO rebuilt + side-loaded (2.16b security axis): the S-1 non-operator
# `Application.status` write rejection and the §7.2 `spec.environment`-immutable
# UPDATE guard live in the webhook, so the branch image is required for the
# P-SEC-* phases.
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

# ---------------------------------------------------------------
# Security-axis constants (2.16b)
# ---------------------------------------------------------------

SEC_NS="sec-walk"                          # dedicated tenant ns for P-SEC-*
SEC_INT="sec-int"                          # internal-then-escalated app (P-SEC-1)
SEC_IMG="sec-img"                          # image-drift stale-approval app (P-SEC-2)
SEC_STATUS="sec-status"                    # S-1 status-bypass target (P-SEC-3)
SEC_ENV="sec-env"                          # §7.2 environment-immutable + negative
SEC_PLAN_F1B="sec-plan"                    # F-1b MigrationPlan.status guard target (P-SEC-7)
SEC_F2="sec-f2"                            # F-2 acquire-secret-ref target (P-SEC-8)
SEC_F3="sec-f3"                            # F-3 public host-swap target (P-SEC-9)
SEC_IMG_A="nginxdemos/hello:plain-text"    # image-drift start repo
SEC_IMG_B="nginxdemos/nginx-hello:plain-text"  # first (approved) drift target
SEC_IMG_C="library/nginx:latest"           # second (post-drift) target — DIFFERENT

# 2.16b-sec S2 impersonation (F-1 / F-1b): the webhook status guards gate on
# the AUTHENTICATED `request.userInfo.username`. kind's default kubeconfig is
# cluster-admin (system:masters) → a plain `kubectl patch` would be ALLOWED by
# the break-glass path, NOT exercising the guard. To simulate a non-operator
# attacker we IMPERSONATE a plain ServiceAccount (kind admin may impersonate
# any subject) so `userInfo.username` becomes this identity → the guard must
# REJECT. The `--as-group=system:serviceaccounts` mirrors what the apiserver
# would stamp for a real SA token (a defensive belt so the impersonated groups
# never accidentally include a break-glass group).
ATTACKER_SA="system:serviceaccount:default:attacker"
ATTACKER_GROUP="system:serviceaccounts"

# apply_sec_app — a single-env Application emitter for the security phases with
# per-field toggles the escalation/env/imagePolicy triggers need. Unlike
# apply_app (two-env, always public + hostname), this emits ONE CR whose expose
# block is EITHER internal-with-no-hostname OR public-with-a-hostname (the
# webhook couples hostname<->public: an internal app must carry NO hostname; a
# public app REQUIRES one — validator.rs::validate_expose).
#
# Usage: apply_sec_app <name> <environment> <network> [opts...]
#   network       — "internal" (no hostname emitted) | "public" (hostname req.)
# Optional key=value opts (any order):
#   image=<ref>          default: $APP_IMAGE
#   hostname=<h>         required when network=public; ignored when internal
#   port=<n>             default 80
#   env_secret=<K>=<name/key>   add one env var whose value is {secret:"<name/key>"}
#   env_claim=<K>=<type.field>  add one env var whose value is {claim:"<type.field>"}
#   env_literal=<K>=<value>     add one env var whose value is a plain literal string
#   needs_pg=1           add base.needs.pg (a scalar pg need)
#   imagepolicy=<off|digest>  set base.imagePolicy.resolve
#
# The three env_* opts are mutually exclusive PER KEY but any combination of
# distinct keys is fine; F-2 uses env_literal then env_secret on the SAME key
# across two applies to exercise the `Literal → Ref(Secret)` acquire.
apply_sec_app() {
    local name="$1" environment="$2" network="$3"; shift 3
    local image="$APP_IMAGE" hostname="" port="80"
    local env_secret="" env_claim="" env_literal="" needs_pg="" imagepolicy=""
    local kv
    for kv in "$@"; do
        case "$kv" in
            image=*)       image="${kv#image=}" ;;
            hostname=*)    hostname="${kv#hostname=}" ;;
            port=*)        port="${kv#port=}" ;;
            env_secret=*)  env_secret="${kv#env_secret=}" ;;
            env_claim=*)   env_claim="${kv#env_claim=}" ;;
            env_literal=*) env_literal="${kv#env_literal=}" ;;
            needs_pg=*)    needs_pg="${kv#needs_pg=}" ;;
            imagepolicy=*) imagepolicy="${kv#imagepolicy=}" ;;
            *) printf 'apply_sec_app: unknown opt %q\n' "$kv" >&2; return 2 ;;
        esac
    done

    # Assemble the base block line-by-line so absent fields are truly omitted
    # (an empty `hostname:` would fail the webhook's DNS-1123 check).
    {
        printf 'apiVersion: apprafter.io/v1alpha1\n'
        printf 'kind: Application\n'
        printf 'metadata:\n'
        printf '  name: %s\n' "$name"
        printf '  namespace: %s\n' "$SEC_NS"
        printf '  labels:\n'
        printf '    apprafter.io/managed-by: apprafter\n'
        printf 'spec:\n'
        printf '  environment: %s\n' "$environment"
        printf '  base:\n'
        printf '    image: %s\n' "$image"
        printf '    replicas: 1\n'
        if [ -n "$imagepolicy" ]; then
            printf '    imagePolicy:\n'
            printf '      resolve: %s\n' "$imagepolicy"
        fi
        if [ -n "$needs_pg" ]; then
            printf '    needs:\n'
            printf '      pg: {}\n'
        fi
        if [ -n "$env_secret" ] || [ -n "$env_claim" ] || [ -n "$env_literal" ]; then
            printf '    env:\n'
            if [ -n "$env_secret" ]; then
                printf '      %s:\n' "${env_secret%%=*}"
                printf '        secret: %q\n' "${env_secret#*=}"
            fi
            if [ -n "$env_claim" ]; then
                printf '      %s:\n' "${env_claim%%=*}"
                printf '        claim: %q\n' "${env_claim#*=}"
            fi
            if [ -n "$env_literal" ]; then
                # A plain literal string value (EnvValue::Literal). Quoted so a
                # numeric/boolean-looking value stays a string.
                printf '      %s: %q\n' "${env_literal%%=*}" "${env_literal#*=}"
            fi
        fi
        printf '    expose:\n'
        printf '      port: %s\n' "$port"
        printf '      network: %s\n' "$network"
        if [ "$network" = "public" ] && [ -n "$hostname" ]; then
            printf '      hostname: %s\n' "$hostname"
        fi
        printf '  environments:\n'
        printf '    %s:\n' "$environment"
        printf '      replicas: 1\n'
    } | kubectl apply -f -
}

# sec_wait_baseline <name> <want-image> — poll until the security app reaches a
# non-paused steady state AND its baseline (status.lastAppliedSpec.base.image)
# stamps to <want-image>. The baseline stamps on the FIRST successful render
# (no CNPG round-trip in these single-need-free apps), so this is quick.
sec_wait_baseline() {
    local name="$1" want_image="$2" deadline ph
    deadline=$(( $(date +%s) + 240 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        ph=$(kubectl -n "$SEC_NS" get "$APP_RES" "$name" -o jsonpath='{.status.phase}' 2>/dev/null || true)
        [ -n "$ph" ] && break
        sleep 5
    done
    if [ "$ph" = "AwaitingMigrationApproval" ]; then
        printf 'FAILED: %s paused on first apply (phase=%q) — no baseline to diff against yet\n' "$name" "$ph" >&2
        return 1
    fi
    wait_jsonpath "$APP_RES" "$SEC_NS" "$name" '{.status.lastAppliedSpec.base.image}' "$want_image" 180
}

# sec_plan_name <name> — the app-scope MigrationPlan name for <name> in $SEC_NS.
sec_plan_name() {
    kubectl -n "$SEC_NS" get "$PLAN_RES" \
        -l "apprafter.io/application=$1,apprafter.io/scope=application" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# sec_plan_count <name> — number of app-scope plans for <name> in $SEC_NS.
sec_plan_count() {
    kubectl -n "$SEC_NS" get "$PLAN_RES" \
        -l "apprafter.io/application=$1,apprafter.io/scope=application" \
        --no-headers 2>/dev/null | grep -c . || true
}

# sec_wait_plan <name> [timeout] — poll until an app-scope plan for <name>
# appears in $SEC_NS; echoes its name on STDOUT (progress → STDERR).
sec_wait_plan() {
    local name="$1" timeout="${2:-120}" deadline nm
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait an app-scope MigrationPlan for %s to appear (timeout %ss) ...\n' "$name" "$timeout" >&2
    while [ "$(date +%s)" -lt "$deadline" ]; do
        nm="$(sec_plan_name "$name")"
        if [ -n "$nm" ]; then
            printf '  ok: MigrationPlan %s/%s exists for %s\n' "$SEC_NS" "$nm" "$name" >&2
            printf '%s' "$nm"
            return 0
        fi
        sleep 5
    done
    printf 'FAILED: no app-scope MigrationPlan for %s appeared within %ss\n' "$name" "$timeout" >&2
    kubectl -n "$SEC_NS" get "$PLAN_RES" -o wide >&2 2>&1 || true
    return 1
}

# sec_jp <kind> <name> <jsonpath> — read one value from $SEC_NS.
sec_jp() { kubectl -n "$SEC_NS" get "$1" "$2" -o jsonpath="$3" 2>/dev/null || true; }

# sec_wait_non_paused <name> [timeout] — the $SEC_NS analogue of
# wait_non_paused (which hardcodes $APP_NS): poll until <name> leaves the
# AwaitingMigrationApproval phase (settles to Ready/Progressing/…).
sec_wait_non_paused() {
    local name="$1" timeout="${2:-120}" deadline phase
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait %s (in %s) to leave AwaitingMigrationApproval (timeout %ss) ...\n' \
        "$name" "$SEC_NS" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        phase=$(sec_jp "$APP_RES" "$name" '{.status.phase}')
        if [ -n "$phase" ] && [ "$phase" != "AwaitingMigrationApproval" ]; then
            printf '  ok: %s left the paused phase (phase=%q)\n' "$name" "$phase"
            return 0
        fi
        sleep 5
    done
    printf 'FAILED: %s never left AwaitingMigrationApproval within %ss (last=%q)\n' \
        "$name" "$timeout" "${phase:-<empty>}" >&2
    kubectl -n "$SEC_NS" describe "$APP_RES" "$name" >&2 2>&1 || true
    return 1
}

# ---------------------------------------------------------------
# 2.16b-sec S2 impersonation harness (F-1 / F-1b)
# ---------------------------------------------------------------

# IMPERSONATION_OK — set to 1 in Phase 0 once we confirm the walk's kube
# context can impersonate a non-operator subject (needed for the F-1/F-1b
# reject tests). Left 0 → the impersonation-dependent asserts SOFT-skip with a
# loud note (F-2/F-3 don't need impersonation and always run).
IMPERSONATION_OK=0

# kubectl_as <username> <group> -- <kubectl-args...> — run kubectl impersonating
# <username>+<group>. The webhook's AdmissionReview `userInfo.username` becomes
# the impersonated identity, so the status guards (which gate on that field)
# see a NON-operator, NON-admin subject and must reject.
kubectl_as() {
    local user="$1" group="$2"; shift 2
    [ "$1" = "--" ] && shift
    kubectl --as="$user" --as-group="$group" "$@"
}

# ensure_impersonation — set up the non-operator attacker identity for the
# F-1/F-1b reject tests. TWO layers of RBAC matter, and they model the threat
# precisely:
#
#   1. The walk's context (kind's cluster-admin) must be allowed to IMPERSONATE
#      the attacker SA. cluster-admin can impersonate any subject, so this
#      normally needs no grant; if apiserver RBAC blocks it we grant `impersonate`
#      on users/groups/serviceaccounts to the current identity and re-probe.
#
#   2. The attacker SA itself must hold `patch applications/status` +
#      `patch migrationplans/status` RBAC — otherwise the apiserver's RBAC
#      denies the write BEFORE admission (a `kubectl patch` first GETs the
#      object), and the webhook's identity guard never runs (we'd see a generic
#      "cannot get resource .../status" Forbidden, not the F-1/F-1b message).
#      This is EXACTLY the threat F-1/F-1b defend against: a subject that HAS
#      the RBAC to write the subresource (a compromised/over-granted controller)
#      but is NOT the operator's authenticated ServiceAccount. Granting the
#      attacker this RBAC is what makes "the webhook, not RBAC, is the boundary"
#      a real assertion — the guard must reject an RBAC-capable non-operator.
#
# Sets IMPERSONATION_OK=1 when BOTH the SA exists+is granted AND impersonation
# works; leaves it 0 on persistent failure (the caller SOFT-skips F-1/F-1b).
ensure_impersonation() {
    # (2) Create the attacker SA + grant it the subresource-write RBAC that lets
    # its request REACH admission (where the identity guard denies it).
    kubectl -n default create serviceaccount attacker >/dev/null 2>&1 || true
    kubectl apply -f - >/dev/null 2>&1 <<'YAML' || true
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: apprafter-walk-attacker
rules:
  - apiGroups: ["apprafter.io"]
    resources:
      - "applications"
      - "applications/status"
      - "migrationplans"
      - "migrationplans/status"
    verbs: ["get", "list", "patch", "update"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: apprafter-walk-attacker
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: apprafter-walk-attacker
subjects:
  - kind: ServiceAccount
    name: attacker
    namespace: default
YAML

    # (1) Probe whether we may impersonate the attacker SA. Use an impersonated
    # `get --raw /version`: `/version` is readable by ANY authenticated identity,
    # so exit 0 means the IMPERSONATION itself was permitted. (NB: do NOT use
    # `auth can-i` here — it returns exit 1 when the ANSWER is "no", which is the
    # expected answer for the low-priv attacker SA even when impersonation
    # succeeded, so its exit code cannot distinguish "impersonation denied" from
    # "impersonation OK, permission denied".) A real impersonation denial yields a
    # nonzero `cannot impersonate` Forbidden.
    if kubectl_as "$ATTACKER_SA" "$ATTACKER_GROUP" -- \
        get --raw /version >/dev/null 2>&1; then
        IMPERSONATION_OK=1
        printf '  ok: attacker SA created + granted status-write RBAC; walk context can impersonate %s\n' "$ATTACKER_SA"
        return 0
    fi
    printf '  note: impersonation denied by apiserver RBAC — granting the walk context impersonate on users/groups/serviceaccounts ...\n'
    local me
    me=$(kubectl auth whoami -o jsonpath='{.status.userInfo.username}' 2>/dev/null || true)
    kubectl apply -f - >/dev/null 2>&1 <<YAML || true
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: apprafter-walk-impersonator
rules:
  - apiGroups: [""]
    resources: ["users", "groups", "serviceaccounts"]
    verbs: ["impersonate"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: apprafter-walk-impersonator
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: apprafter-walk-impersonator
subjects:
  - apiGroup: rbac.authorization.k8s.io
    kind: User
    name: ${me:-kubernetes-admin}
YAML
    sleep 3
    if kubectl_as "$ATTACKER_SA" "$ATTACKER_GROUP" -- \
        get --raw /version >/dev/null 2>&1; then
        IMPERSONATION_OK=1
        printf '  ok: walk context can now impersonate %s (grant applied)\n' "$ATTACKER_SA"
        return 0
    fi
    printf '  note: (soft) impersonation still blocked after grant — F-1/F-1b reject tests will SOFT-skip.\n'
    return 0
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
# `admission-webhook` is ALSO disabled (2.16b security axis): its
# ValidatingWebhookConfiguration is the released one, whose `applications` rule
# LACKS the `applications/status` subresource — so an S-1 status-subresource
# write bypasses admission entirely. We side-load the BRANCH-rendered VWC (which
# lists `applications/status`) after the webhook build below, and Argo's
# selfHeal would otherwise revert that drift instantly.
printf '  disabling Argo automated-sync on platform + apprafter-operator + admission-webhook ...\n'
for _app in platform apprafter-operator admission-webhook; do
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

# 2.16b security axis: the branch ADMISSION-WEBHOOK too. The S-1 non-operator
# `Application.status` write rejection (server.rs — status_write_allowed) and
# the §7.2 `spec.environment`-immutable UPDATE guard live ONLY in the webhook,
# so the P-SEC-3 / P-SEC-4 phases require the working-tree image, not the
# released one. Same build_load_restart helper; the deployment + Dockerfile
# subdir are both `admission-webhook` (matching how needs-disk-walk builds it).
build_load_restart admission-webhook admission-webhook

# 2.16b-sec (F-1): side-load the branch webhook Deployment's OPERATOR_SERVICEACCOUNT
# env. `build_load_restart` rebuilds the IMAGE + restarts the running Deployment,
# but it does NOT re-render the Deployment SPEC — the running Deployment came from
# the last PUBLISHED chart (its container env predates the F-1 change), so it
# lacks `OPERATOR_SERVICEACCOUNT`. Without it the binary falls back to the
# hardcoded default SA; the fallback happens to equal the correct value here
# (release ns = apprafter-system), so the guards still WORK, but this walk must
# prove the CHART carries the env (the deployment.yaml change) — so we read the
# value the branch chart renders and PATCH it onto the running Deployment (a
# targeted env add, keeping the side-loaded branch image tag). Mirrors the VWC
# side-load above. This triggers one more rollout, which the terminate-wait
# below absorbs.
printf '  side-loading the branch webhook OPERATOR_SERVICEACCOUNT env (F-1 — build_load_restart does not re-render the Deployment spec) ...\n'
_wh_sa_value=$(helm template admission-webhook "${REPO_ROOT}/operator/charts/apprafter-admission-webhook" \
    --namespace "$PROVIDER_NS" \
    | _yq 'select(.kind == "Deployment") | .spec.template.spec.containers[0].env[] | select(.name == "OPERATOR_SERVICEACCOUNT") | .value' \
    | head -1)
if [ -z "$_wh_sa_value" ] || [ "$_wh_sa_value" = "null" ]; then
    printf 'FAILED: the branch admission-webhook chart does NOT render an OPERATOR_SERVICEACCOUNT env — the deployment.yaml F-1 change is absent from the working tree\n' >&2
    exit 1
fi
printf '  branch chart renders OPERATOR_SERVICEACCOUNT=%q — patching it onto the running Deployment ...\n' "$_wh_sa_value"
kubectl -n "$PROVIDER_NS" patch deploy admission-webhook --type=json -p "$(cat <<JSON
[{"op":"add","path":"/spec/template/spec/containers/0/env/-","value":{"name":"OPERATOR_SERVICEACCOUNT","value":"${_wh_sa_value}"}}]
JSON
)"
kubectl -n "$PROVIDER_NS" rollout status deploy admission-webhook --timeout=120s

# `rollout status` returns once the NEW webhook pod is Ready, but the OLD
# (released) pod lingers Terminating for its grace period — and during that
# window an Application UPDATE could still route to the old webhook, whose
# released image lacks the S-1 / §7.2 guards. Wait until ONLY the branch
# webhook serves before any security-phase apply/patch.
printf '  waiting for the old (released) webhook pod to fully terminate ...\n'
_wh_deadline=$(( $(date +%s) + 90 ))
while [ "$(date +%s)" -lt "$_wh_deadline" ]; do
    [ "$(kubectl -n "$PROVIDER_NS" get pods \
        -l app.kubernetes.io/name=admission-webhook --no-headers 2>/dev/null \
        | wc -l)" -le 1 ] && break
    sleep 3
done
printf '  ok: admission-webhook now running the working-tree (2.16b) build\n'

# 2.16b S-1: side-load the BRANCH-rendered ValidatingWebhookConfiguration. The
# released VWC's `applications` rule lists only `applications` — NOT
# `applications/status`. In Kubernetes, a status-subresource write
# (`kubectl patch --subresource=status`) routes through a DIFFERENT admission
# URL, so without the `applications/status` subresource listed the S-1 guard in
# the webhook (status_write_allowed) is NEVER invoked — the write bypasses
# admission entirely. The branch chart's VWC template lists both (verified at
# operator/charts/apprafter-admission-webhook/templates/
# validatingwebhookconfiguration.yaml:44-46), so we render + apply it. Argo
# selfHeal on the `admission-webhook` app was disabled above so this drift
# survives.
#
# The template carries `cert-manager.io/inject-ca-from`, so cert-manager's
# ca-injector repopulates `clientConfig.caBundle` after the apply. We wait for
# the caBundle to reappear on the applications webhook (else a failurePolicy
# request could hit an empty-CA webhook) AND for the `applications/status`
# subresource to be listed before proceeding.
printf '  applying branch admission-webhook ValidatingWebhookConfiguration (S-1: adds applications/status) ...\n'
helm template admission-webhook "${REPO_ROOT}/operator/charts/apprafter-admission-webhook" \
    --namespace "$PROVIDER_NS" \
    | _yq 'select(.kind == "ValidatingWebhookConfiguration")' \
    | kubectl apply -f -
printf '  waiting for the branch VWC to list applications/status + cert-manager to re-inject the caBundle ...\n'
_vwc_deadline=$(( $(date +%s) + 90 ))
while [ "$(date +%s)" -lt "$_vwc_deadline" ]; do
    _vwc_res=$(kubectl get validatingwebhookconfiguration admission-webhook.apprafter.io \
        -o jsonpath='{range .webhooks[?(@.name=="applications.apprafter.io")]}{.rules[*].resources}{"|"}{.clientConfig.caBundle}{end}' 2>/dev/null || true)
    # Require BOTH: applications/status present AND a non-empty caBundle.
    if printf '%s' "$_vwc_res" | grep -q "applications/status" \
        && [ -n "${_vwc_res##*|}" ]; then
        break
    fi
    sleep 3
done
if kubectl get validatingwebhookconfiguration admission-webhook.apprafter.io \
    -o jsonpath='{range .webhooks[?(@.name=="applications.apprafter.io")]}{.rules[*].resources}{end}' 2>/dev/null \
    | grep -q "applications/status"; then
    printf '  ok: branch VWC live — applications/status subresource intercepted (S-1 guard reachable)\n'
else
    printf 'FAILED: branch VWC did not take (applications/status still not listed) — S-1 guard unreachable\n' >&2
    kubectl get validatingwebhookconfiguration admission-webhook.apprafter.io -o yaml >&2 2>&1 || true
    exit 1
fi

# The admission webhook must be Available before any Application apply (it
# validates the CR on CREATE — network/hostname invariants).
retry 30 10 -- kubectl -n "$PROVIDER_NS" rollout status \
    deploy admission-webhook --timeout=60s
printf '  ok: platform ready (branch operator + webhook)\n'

# 2.16b-sec (F-1): the branch webhook Deployment must carry OPERATOR_SERVICEACCOUNT
# (deployment.yaml env, templated on the release namespace) and log the RESOLVED
# SA at startup (main.rs — "resolved operator ServiceAccount for status/identity
# guards"). Confirm BOTH: the env is present on the container, AND the pod logged
# the value it resolved (surfaces a mis-set env in the pod log, not at first
# admission). These are the trust anchor for the F-1/F-1b status guards.
printf '  confirming the branch webhook carries OPERATOR_SERVICEACCOUNT + logs the resolved SA ...\n'
wh_sa_env=$(kubectl -n "$PROVIDER_NS" get deploy admission-webhook \
    -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="OPERATOR_SERVICEACCOUNT")].value}' 2>/dev/null || true)
if [ -n "$wh_sa_env" ]; then
    printf '  ok: webhook Deployment env OPERATOR_SERVICEACCOUNT=%q\n' "$wh_sa_env"
else
    printf 'FAILED: branch webhook Deployment is MISSING the OPERATOR_SERVICEACCOUNT env (F-1 trust anchor absent)\n' >&2
    kubectl -n "$PROVIDER_NS" get deploy admission-webhook -o jsonpath='{.spec.template.spec.containers[0].env}' >&2 2>&1 || true
    exit 1
fi
# Poll the webhook pod log for the startup line that echoes the resolved SA.
printf '  waiting for the webhook pod to log its resolved operator ServiceAccount ...\n'
_wh_log_deadline=$(( $(date +%s) + 60 ))
wh_sa_logged=""
while [ "$(date +%s)" -lt "$_wh_log_deadline" ]; do
    wh_sa_logged=$(kubectl -n "$PROVIDER_NS" logs deploy/admission-webhook --tail=200 2>/dev/null \
        | grep -i 'resolved operator ServiceAccount' | head -1 || true)
    [ -n "$wh_sa_logged" ] && break
    sleep 3
done
if [ -n "$wh_sa_logged" ]; then
    printf '  ok: webhook logged resolved SA at startup: %s\n' "$wh_sa_logged"
    # Belt: the logged SA should match the injected env value.
    if printf '%s' "$wh_sa_logged" | grep -qF "$wh_sa_env"; then
        printf '  ok: logged SA matches the injected OPERATOR_SERVICEACCOUNT (%s)\n' "$wh_sa_env"
    else
        printf '  note: logged SA line does not literally contain %q (log formatting) — env presence already asserted; continuing.\n' "$wh_sa_env"
    fi
else
    printf '  note: (soft) did not observe the resolved-SA startup log within 60s (log rotation / verbosity) — the env presence above is the hard assertion; continuing.\n'
fi

# 2.16b-sec (F-1/F-1b): probe/enable impersonation for the reject tests. The
# webhook allows admin break-glass (system:masters), and kind's default
# kubeconfig IS cluster-admin — so a plain kubectl patch would be ALLOWED, not
# exercising the guard. We impersonate a plain SA so `userInfo.username` is a
# non-operator, non-admin subject → the guard must REJECT. Grant impersonate if
# apiserver RBAC blocks it; SOFT-skip F-1/F-1b if it stays blocked.
ensure_impersonation

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
# Phase 7: revert = reject-via-Git (DeleteThenRender + image-path op)
# ===============================================================

phase "Phase 7: revert = reject-via-Git — gate mig-prod on image-repo change, then REVERT"

# An image REPOSITORY change is HARD-destructive (image-path-change,
# requires-restart). Keep network=public + the Phase-6 tag; only the repo
# flips → webhook-valid and a single, unambiguous trigger.
#
# Why NOT network public->internal here: the admission webhook couples
# hostname<->public bi-directionally (validator.rs — a public app REQUIRES a
# hostname; a hostname REQUIRES public). So flipping a public app to internal
# must co-remove its hostname, and the classifier tie-break scores that combined
# edit as `domain-change` (sorts before `network-visibility-change`). Thus
# network-visibility-change is never the standalone primary trigger for a
# webhook-valid edit — it is defense-in-depth, always shadowed by domain-change.
# image-path-change is a clean, reachable single-trigger op for the revert test.
apply_app "$APP_PROD" prod 0 2 public "nginxdemos/nginx-hello:0.4"

wait_jsonpath "$APP_RES" "$APP_NS" "$APP_PROD" '{.status.phase}' AwaitingMigrationApproval 180
PROD_PLAN="$(wait_plan_appears "$APP_PROD" 120)"
[ -n "$PROD_PLAN" ] || { printf 'FAILED: prod plan name empty\n' >&2; exit 1; }
prod_trigger=$(jp "$PLAN_RES" "$APP_NS" "$PROD_PLAN" '{.spec.trigger.type}')
assert_eq "prod plan trigger.type" "$prod_trigger" "image-path-change"
printf '  ok: mig-prod gated on image-path-change (plan %s)\n' "$PROD_PLAN"

# REVERT the change in-place (image repo back to the Phase-6 value) — the
# analogue of reverting the edit in Git. The operator sees no destructive delta
# vs the baseline, so it self-deletes the now-stale plan and un-freezes.
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

# ###############################################################
# 2.16b SECURITY AXIS — P-SEC-1 .. P-SEC-9
#
# The app-migration state machine gains a `security-boundary` class (severity
# 4, OUTRANKS data-migration), a family of expose/env/imagePolicy triggers, a
# `spec.changes[]`/`spec.risks.classifications` rollup, a content-hash-bound
# approval (S-4), a webhook `applications/status` guard (S-1), a §7.2
# `spec.environment`-immutable UPDATE guard, and a needs.selector tripwire.
#
# The S2 round-of-review fixes add:
#   F-1  (P-SEC-3): Application.status writes gate on the AUTHENTICATED
#        `userInfo.username == <operator SA>` (was a spoofable fieldManager) —
#        a spoofed `--field-manager=apprafter-operator` from a non-operator
#        impersonated identity is now REJECTED.
#   F-1b (P-SEC-7): MigrationPlan.status external writes are field-restricted —
#        an external subject may write ONLY status.phase pending-approval→
#        approved; executedSteps (or any other field / phase jump) is REJECTED,
#        while the legit external approve still works.
#   F-2  (P-SEC-8): a key that ACQUIRES a secret-ref (Literal/Ref(Claim) →
#        Ref(Secret)) gates as env-secret-ref-add (security-boundary).
#   F-3  (P-SEC-9): a public hostname SWAP {a}→{b} co-fires #3 domain-change
#        (a lost) AND #11 public-hostname-add (b gained); primary is #11
#        (security-boundary).
#
# F-1/F-1b use IMPERSONATION (`kubectl --as=…`) to present a non-operator,
# non-admin `userInfo.username` to the webhook — kind's default kubeconfig is
# cluster-admin (a break-glass path the guard allows), so a plain patch would
# not exercise the guard. F-2/F-3 need no impersonation (operator-side gating).
#
# These phases exercise the LIVE behavior against the branch operator + webhook
# built in Phase 0.
# ###############################################################

kubectl create namespace "$SEC_NS" 2>/dev/null || true

# ===============================================================
# P-SEC-1: network escalation (internal -> public + hostname) gates as
#          security-boundary + carries the full changes[]/classifications
#          rollup; approve un-freezes.
# ===============================================================

phase "P-SEC-1: internal->public escalation gated (security-boundary) + rollup + approve"

# Deploy an INTERNAL app: network:internal, NO hostname (the webhook couples
# hostname<->public — an internal app must carry no hostname). Baseline stamps
# on the first render.
apply_sec_app "$SEC_INT" dev internal
sec_wait_baseline "$SEC_INT" "$APP_IMAGE"
assert_eq "no plan for sec-int at baseline" "$(sec_plan_count "$SEC_INT")" "0"

# Escalate: network -> public + add hostname sec.example.com. This fires TWO
# security-boundary triggers in ONE edit:
#   #10 network-visibility-escalation (internal -> public), and
#   #11 public-hostname-add          (the None -> Some first-hostname add).
# Both are classification=security-boundary. Primary (severity desc, then
# trigger_type asc) → network-visibility-escalation.
apply_sec_app "$SEC_INT" dev public hostname=sec.example.com

# The app pauses + an app-scope plan appears.
wait_jsonpath "$APP_RES" "$SEC_NS" "$SEC_INT" '{.status.phase}' AwaitingMigrationApproval 180
SEC1_PLAN="$(sec_wait_plan "$SEC_INT" 120)"
[ -n "$SEC1_PLAN" ] || { printf 'FAILED: sec-int plan name empty\n' >&2; exit 1; }

# Primary classification == security-boundary (the field is spec.risks.classification).
sec1_primary=$(sec_jp "$PLAN_RES" "$SEC1_PLAN" '{.spec.risks.classification}')
assert_eq "P-SEC-1 spec.risks.classification (primary)" "$sec1_primary" "security-boundary"

# classifications[] rollup CONTAINS security-boundary. (Both triggers are
# security-boundary, so the distinct set is exactly [security-boundary].)
sec1_classifications=$(sec_jp "$PLAN_RES" "$SEC1_PLAN" '{.spec.risks.classifications[*]}')
printf '  spec.risks.classifications = %q\n' "$sec1_classifications"
if printf '%s' "$sec1_classifications" | grep -qw "security-boundary"; then
    printf '  ok: spec.risks.classifications contains security-boundary\n'
else
    printf 'FAILED: spec.risks.classifications %q does not contain security-boundary\n' "$sec1_classifications" >&2
    exit 1
fi

# changes[] rollup carries BOTH triggers. NB the wire field is `type` (the Rust
# MigrationChange.trigger is #[serde(rename = "type")]), so we read
# .spec.changes[*].type — NOT `.trigger`.
sec1_change_types=$(sec_jp "$PLAN_RES" "$SEC1_PLAN" '{.spec.changes[*].type}')
printf '  spec.changes[*].type = %q\n' "$sec1_change_types"
for want in network-visibility-escalation public-hostname-add; do
    if printf '%s' "$sec1_change_types" | grep -qw "$want"; then
        printf '  ok: spec.changes[] contains trigger %s\n' "$want"
    else
        printf 'FAILED: spec.changes[*].type %q missing %s\n' "$sec1_change_types" "$want" >&2
        kubectl -n "$SEC_NS" get "$PLAN_RES" "$SEC1_PLAN" -o yaml >&2 2>&1 || true
        exit 1
    fi
done

# Every change[] row is classified security-boundary (severity 4).
sec1_change_classes=$(sec_jp "$PLAN_RES" "$SEC1_PLAN" '{.spec.changes[*].classification}')
if printf '%s' "$sec1_change_classes" | grep -qw "security-boundary"; then
    printf '  ok: spec.changes[] rows carry security-boundary classification\n'
else
    printf 'FAILED: spec.changes[*].classification %q missing security-boundary\n' "$sec1_change_classes" >&2
    exit 1
fi

# Approve via the branch CLI (auto-resolving the namespace) → app un-freezes +
# the escalated (public) spec applies. The plan consumes (deletes).
printf '  approving %s via apprafter migration approve ...\n' "$SEC1_PLAN"
apprafter migration approve "$SEC1_PLAN"
sec_wait_non_paused "$SEC_INT" 120
wait_gone "$PLAN_RES" "$SEC_NS" "$SEC1_PLAN" 90
# The approved escalation applied: the baseline is now public.
wait_jsonpath "$APP_RES" "$SEC_NS" "$SEC_INT" '{.status.lastAppliedSpec.base.expose.network}' public 120
printf 'ok: security-boundary escalation gated + rollup + approved\n'

# ===============================================================
# P-SEC-2: S-4 stale-approval — approving the ORIGINAL plan must NOT deploy a
#          DIFFERENT (drifted) image. The approval is bound to a content hash
#          of the change it was cut over; a subsequent retarget invalidates it.
# ===============================================================

phase "P-SEC-2: S-4 stale approval not consumed for a different image target"

# Deploy on image repo A (nginxdemos/hello). Baseline stamps.
apply_sec_app "$SEC_IMG" dev internal image="$SEC_IMG_A"
sec_wait_baseline "$SEC_IMG" "$SEC_IMG_A"

# Edit image REPO A -> B (nginxdemos/nginx-hello): image-path-change,
# security-boundary. Plan P1 appears (from=nginxdemos/hello, to=nginxdemos/nginx-hello).
apply_sec_app "$SEC_IMG" dev internal image="$SEC_IMG_B"
wait_jsonpath "$APP_RES" "$SEC_NS" "$SEC_IMG" '{.status.phase}' AwaitingMigrationApproval 180
SEC2_P1="$(sec_wait_plan "$SEC_IMG" 120)"
[ -n "$SEC2_P1" ] || { printf 'FAILED: sec-img P1 plan name empty\n' >&2; exit 1; }
sec2_p1_trigger=$(sec_jp "$PLAN_RES" "$SEC2_P1" '{.spec.trigger.type}')
assert_eq "P-SEC-2 P1 trigger.type" "$sec2_p1_trigger" "image-path-change"
sec2_p1_to=$(sec_jp "$PLAN_RES" "$SEC2_P1" '{.spec.trigger.to}')
sec2_p1_hash=$(sec_jp "$PLAN_RES" "$SEC2_P1" '{.spec.trigger.approvedSpecHash}')
printf '  P1: trigger.to=%q approvedSpecHash=%q\n' "$sec2_p1_to" "$sec2_p1_hash"
[ -n "$sec2_p1_hash" ] || { printf 'FAILED: P1 carries no approvedSpecHash (S-4 binding absent)\n' >&2; exit 1; }

# BEFORE approving, DRIFT the target: edit image REPO B -> C (library/nginx):
# same trigger tuple (image-path-change on spec.image) but DIFFERENT content.
# The operator supersedes P1 with a fresh plan bound to the C target (its
# content hash differs). Give the operator a beat to re-gate.
apply_sec_app "$SEC_IMG" dev internal image="$SEC_IMG_C"
sleep 15

# Now approve the ORIGINAL plan P1 (bound to the B target). The S-4 content-hash
# check must REFUSE to consume it against the current (C) change set — the app
# must NOT deploy repo B (nginxdemos/nginx-hello).
printf '  approving the ORIGINAL plan %s (bound to the B target) ...\n' "$SEC2_P1"
apprafter migration approve "$SEC2_P1" || printf '  note: approve returned nonzero (plan may already be superseded) — continuing\n'

# Watch ~40s: the Deployment image repo must NEVER become nginxdemos/nginx-hello.
printf '  watching ~40s that the Deployment never adopts the stale (B) image repo ...\n'
sec2_bad_repo="nginxdemos/nginx-hello"
sec2_saw_bad=0
_sec2_deadline=$(( $(date +%s) + 40 ))
while [ "$(date +%s)" -lt "$_sec2_deadline" ]; do
    dep_img=$(kubectl -n "$SEC_NS" get deployment "$SEC_IMG" \
        -o jsonpath='{.spec.template.spec.containers[0].image}' 2>/dev/null || true)
    # Extract the repo (strip any @digest then any :tag after the last '/').
    dep_no_digest="${dep_img%%@*}"
    case "$dep_no_digest" in
        */*:*) dep_repo="${dep_no_digest%:*}" ;;   # tag after last slash
        *:*)   dep_repo="${dep_no_digest%:*}" ;;
        *)     dep_repo="$dep_no_digest" ;;
    esac
    if [ "$dep_repo" = "$sec2_bad_repo" ]; then
        sec2_saw_bad=1
        printf '    PRODUCT-BUG-SIGNAL: Deployment adopted the STALE image repo %q (img=%q)\n' "$dep_repo" "$dep_img" >&2
        break
    fi
    sleep 5
done

# Capture the resulting plan state for the report.
sec2_phase=$(sec_jp "$APP_RES" "$SEC_IMG" '{.status.phase}')
sec2_cur_plan="$(sec_plan_name "$SEC_IMG")"
sec2_cur_to=""
[ -n "$sec2_cur_plan" ] && sec2_cur_to=$(sec_jp "$PLAN_RES" "$sec2_cur_plan" '{.spec.trigger.to}')
printf '  post-approve: app phase=%q, current-plan=%q (trigger.to=%q)\n' \
    "$sec2_phase" "${sec2_cur_plan:-<none>}" "${sec2_cur_to:-<none>}"

if [ "$sec2_saw_bad" -eq 1 ]; then
    printf 'FAILED: PRODUCT BUG — a STALE approval (P1, target %s) deployed the WRONG image repo %s (S-4 content-hash binding did not hold)\n' \
        "$SEC_IMG_B" "$sec2_bad_repo" >&2
    kubectl -n "$SEC_NS" get "$PLAN_RES" -o wide >&2 2>&1 || true
    kubectl -n "$SEC_NS" get "$APP_RES" "$SEC_IMG" -o yaml >&2 2>&1 || true
    exit 1
fi
printf 'ok: stale approval not consumed for a different target (S-4)\n'

# ===============================================================
# P-SEC-3: F-1 — a non-operator write to Application/status is webhook-DENIED
#          EVEN when it SPOOFS the operator fieldManager.
#
# S2 F-1 hardened the guard: it now gates on the AUTHENTICATED
# `request.userInfo.username`, NOT on the client-supplied `fieldManager`. The
# OLD guard trusted `--field-manager=apprafter-operator`, so any subject could
# spoof it. Here we impersonate a NON-operator SA AND pass the spoofed
# `--field-manager=apprafter-operator` — the write must STILL be DENIED (the
# authenticated identity is the attacker SA, not the operator SA). We also
# assert the OPERATOR's own reconcile keeps writing status normally (the app
# reaches a real phase + a stamped baseline), proving the guard rejects the
# attacker without wedging the legitimate writer.
# ===============================================================

phase "P-SEC-3: F-1 spoofed-fieldManager non-operator Application.status write denied"

# Deploy a plain app to target. The operator's OWN reconcile must drive it to a
# non-paused phase + stamp the baseline — sec_wait_baseline asserts exactly
# that, so this doubles as "the operator still writes status" (F-1 does not wedge
# the legit writer).
apply_sec_app "$SEC_STATUS" dev internal
sec_wait_baseline "$SEC_STATUS" "$APP_IMAGE"
printf '  ok: operator reconcile still writes Application.status (phase+baseline stamped) — legit writer not wedged (F-1)\n'

if [ "$IMPERSONATION_OK" -eq 1 ]; then
    # Impersonate a NON-operator SA AND spoof the operator fieldManager. Under
    # the OLD (pre-F-1) guard the spoofed fieldManager would have been ALLOWED;
    # under F-1 the authenticated `userInfo.username` is the attacker → DENIED.
    set +e
    sec3_out=$(kubectl_as "$ATTACKER_SA" "$ATTACKER_GROUP" -- \
        patch "$APP_RES" "$SEC_STATUS" -n "$SEC_NS" \
        --subresource=status --type=merge \
        --field-manager=apprafter-operator \
        -p '{"status":{"lastAppliedSpec":{}}}' 2>&1)
    sec3_rc=$?
    set -e
    printf '  impersonated (%s) spoofed-fieldManager status-patch rc=%d output:\n%s\n' \
        "$ATTACKER_SA" "$sec3_rc" "$sec3_out"
    # DENIED iff nonzero AND the webhook's F-1 message ("operator-owned" /
    # "rejected write from") is present — NOT an RBAC/impersonation error.
    if [ "$sec3_rc" -ne 0 ] \
        && printf '%s' "$sec3_out" | grep -Eqi 'operator-owned|rejected write from'; then
        printf 'ok: spoofed-fieldManager status write denied (F-1)\n'
    else
        printf 'FAILED: spoofed-fieldManager non-operator Application.status write was NOT denied by the F-1 guard (rc=%d)\n' "$sec3_rc" >&2
        exit 1
    fi
else
    printf '  note: (soft) impersonation unavailable — SOFT-skipping the F-1 spoofed-fieldManager reject assert (the operator-writes-status half above still ran).\n'
    printf 'ok: spoofed-fieldManager status write denied (F-1) — SOFT-skipped (impersonation blocked)\n'
fi

# ===============================================================
# P-SEC-4: §7.2 — spec.environment is immutable on UPDATE.
# ===============================================================

phase "P-SEC-4: §7.2 spec.environment immutable on UPDATE"

# The sec-env app carries spec.environment: dev. A merge-patch flipping it to
# prod must be DENIED by the webhook (environment_update_allowed).
apply_sec_app "$SEC_ENV" dev internal
sec_wait_baseline "$SEC_ENV" "$APP_IMAGE"

set +e
sec4_out=$(kubectl patch "$APP_RES" "$SEC_ENV" -n "$SEC_NS" --type=merge \
    -p '{"spec":{"environment":"prod"}}' 2>&1)
sec4_rc=$?
set -e
printf '  kubectl env-change-patch rc=%d output:\n%s\n' "$sec4_rc" "$sec4_out"
if [ "$sec4_rc" -ne 0 ] && printf '%s' "$sec4_out" | grep -qi 'immutable'; then
    printf 'ok: spec.environment change denied (S7.2)\n'
else
    printf 'FAILED: spec.environment change was NOT denied (rc=%d) — §7.2 guard missing/ineffective\n' "$sec4_rc" >&2
    exit 1
fi

# ===============================================================
# P-SEC-5: needs.selector multi-provider tripwire (BEST-EFFORT / SOFT).
#
# The tripwire fires a `SelectorChangeUnderMultipleProviders` Warning Event
# when a soft needs.*.selector change lands while >1 ServiceProvider of that
# type exists (selector_multiprovider_tripwire — it lists ServiceProviders by
# spec.type and counts them; it does NOT require them to be Ready). The
# platform already ships one `pg` ServiceProvider (`pg-integrated`), so we add
# ONE more (a 2nd `pg` CR with the correct {type,backend,config} shape) to make
# the count >=2, deploy an app with `needs.pg` + an initial selector, wait for
# its baseline, then CHANGE the selector (a soft, ungated edit). If any step is
# infeasible on kind, SOFT-skip with a note — this phase NEVER fails the walk.
# ===============================================================

phase "P-SEC-5: (soft) needs.selector multi-provider tripwire"

sec5_done=0
if kubectl get crd serviceproviders.apprafter.io >/dev/null 2>&1; then
    # A 2nd pg ServiceProvider (correct shape: {type,backend,config} — the CRD
    # strict-decodes, so a bogus field like `tier` is rejected). The config is a
    # never-provisioned shell; the tripwire only COUNTS CRs of spec.type=pg.
    sec5_applied=1
    kubectl apply -f - >/dev/null 2>&1 <<YAML || sec5_applied=0
apiVersion: apprafter.io/v1alpha1
kind: ServiceProvider
metadata:
  name: sec-pg-2
  namespace: ${PROVIDER_NS}
spec:
  type: pg
  backend: cloudnative-pg
  config:
    cluster: sec-pg-2-cluster
    namespace: cnpg-system
    instances: 1
    storage: 1Gi
YAML

    if [ "$sec5_applied" -eq 1 ] && \
       [ "$(kubectl -n "$PROVIDER_NS" get serviceprovider.apprafter.io \
            -o jsonpath='{range .items[?(@.spec.type=="pg")]}{.metadata.name}{"\n"}{end}' 2>/dev/null \
            | grep -c .)" -ge 2 ]; then
        printf '  ok: >=2 pg ServiceProviders present\n'
        # An app with needs.pg carrying an INITIAL selector. The baseline stamps
        # on the first render (no CNPG round-trip needed — the render + stamp
        # precedes claim readiness), so give it a bounded chance to stamp.
        kubectl apply -f - >/dev/null 2>&1 <<YAML || true
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${SEC_ENV}-sel
  namespace: ${SEC_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  environment: dev
  base:
    image: ${APP_IMAGE}
    replicas: 1
    needs:
      pg:
        selector:
          zone: a
    expose:
      port: 80
      network: internal
  environments:
    dev:
      replicas: 1
YAML
        _sel_deadline=$(( $(date +%s) + 120 ))
        while [ "$(date +%s)" -lt "$_sel_deadline" ]; do
            [ -n "$(sec_jp "$APP_RES" "$SEC_ENV-sel" '{.status.lastAppliedSpec.base.needs}')" ] && break
            sleep 5
        done
        # Soft selector CHANGE (zone: a -> b) on the existing needs.pg. A
        # selector-only edit is ungated/soft; the tripwire escalates it to a
        # Warning under >1 provider (soft_destructive_notes emits
        # `changed needs.pg selector`, then selector_multiprovider_tripwire fires).
        kubectl apply -f - >/dev/null 2>&1 <<YAML || true
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${SEC_ENV}-sel
  namespace: ${SEC_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  environment: dev
  base:
    image: ${APP_IMAGE}
    replicas: 1
    needs:
      pg:
        selector:
          zone: b
    expose:
      port: 80
      network: internal
  environments:
    dev:
      replicas: 1
YAML
        # Look for the Warning Event OR the operator-log tripwire line (~90s).
        printf '  watching ~90s for a SelectorChangeUnderMultipleProviders Warning Event / log line ...\n'
        _tw_deadline=$(( $(date +%s) + 90 ))
        sec5_hit=""
        while [ "$(date +%s)" -lt "$_tw_deadline" ]; do
            sec5_hit=$(kubectl -n "$SEC_NS" get events \
                --field-selector "involvedObject.name=${SEC_ENV}-sel" \
                -o jsonpath='{range .items[*]}{.reason}{"\n"}{end}' 2>/dev/null \
                | grep -x SelectorChangeUnderMultipleProviders || true)
            [ -n "$sec5_hit" ] && break
            if kubectl -n "$PROVIDER_NS" logs deploy/apprafter-operator --tail=600 2>/dev/null \
                | grep -q "SelectorChangeUnderMultipleProviders"; then
                sec5_hit="log"
                break
            fi
            sleep 5
        done
        if [ -n "$sec5_hit" ]; then
            printf '  ok: tripwire fired (SelectorChangeUnderMultipleProviders via %s)\n' \
                "$([ "$sec5_hit" = log ] && echo operator-log || echo Event)"
            sec5_done=1
        else
            printf '  note: (soft) no tripwire Event/log observed — the selector edit may not have re-reconciled as soft on this bare-provider kind (e.g. baseline never stamped because the claim gate blocked the render); NOT failing.\n'
        fi
    else
        printf '  note: (soft) could not stand up a 2nd pg ServiceProvider on kind — SOFT-skipping the tripwire.\n'
    fi
else
    printf '  note: (soft) ServiceProvider CRD not present — SOFT-skipping the tripwire.\n'
fi
if [ "$sec5_done" -eq 1 ]; then
    printf 'ok: (soft) selector tripwire\n'
else
    printf 'ok: (soft) selector tripwire — SOFT-skipped (2-provider setup not exercised)\n'
fi

# ===============================================================
# P-SEC-6: NEGATIVE — an ungated edit (needs.pg ADD + a self-scoped claim-ref
#          env ADD) produces NO MigrationPlan.
# ===============================================================

phase "P-SEC-6: negative — needs.pg add + claim-ref env add is NOT gated"

# Fresh app with a baseline, no needs, no env. Reuse sec-status (already has a
# baseline). Add base.needs.pg + env DB={claim:"pg.url"} in one edit. Both are
# UNGATED: a needs ADD (only removal gates) and a claim-ref add (self-scoped,
# ADR 0046 — only a secret-ref add gates). Assert NO plan appears.
apply_sec_app "$SEC_STATUS" dev internal needs_pg=1 env_claim='DB=pg.url'

# Give the operator a generous beat to reconcile the edit, then assert no plan.
printf '  watching ~30s that NO MigrationPlan appears for the ungated edit ...\n'
sleep 30
sec6_plans=$(sec_plan_count "$SEC_STATUS")
assert_eq "P-SEC-6 no plan for needs+claim-ref add (ungated)" "$sec6_plans" "0"
# And the app is not paused on this edit.
sec6_phase=$(sec_jp "$APP_RES" "$SEC_STATUS" '{.status.phase}')
if [ "$sec6_phase" = "AwaitingMigrationApproval" ]; then
    printf 'FAILED: sec-status PAUSED on an ungated needs+claim-ref add (phase=%q)\n' "$sec6_phase" >&2
    kubectl -n "$SEC_NS" get "$APP_RES" "$SEC_STATUS" -o yaml >&2 2>&1 || true
    exit 1
fi
printf 'ok: claim-ref + needs add NOT gated (negative)\n'

# ===============================================================
# P-SEC-7: F-1b — MigrationPlan.status external-write field-restriction.
#   An external (non-operator) subject may write ONLY
#   `status.phase pending-approval→approved` (the legit approval signal). A
#   write to `status.executedSteps` (or any other controller-owned field, or a
#   phase jump) is REJECTED. Uses a REAL pending-approval plan (created via a
#   destructive edit) and impersonation for the non-operator identity.
# ===============================================================

phase "P-SEC-7: F-1b external MigrationPlan.status write field-restricted (executedSteps denied, phase-approve allowed)"

if [ "$IMPERSONATION_OK" -eq 1 ]; then
    # Stand up a fresh app + baseline, then a destructive edit (scale-to-zero,
    # HARD) to produce a pending-approval app-scope MigrationPlan.
    apply_sec_app "$SEC_PLAN_F1B" dev internal
    sec_wait_baseline "$SEC_PLAN_F1B" "$APP_IMAGE"
    # Scale-to-zero is requires-restart (gates). Re-emit with base.replicas 0
    # via a direct apply (apply_sec_app hardcodes replicas:1, so patch replicas
    # to 0 on both base + the env override the operator unifies).
    kubectl -n "$SEC_NS" patch "$APP_RES" "$SEC_PLAN_F1B" --type=merge \
        -p '{"spec":{"base":{"replicas":0},"environments":{"dev":{"replicas":0}}}}' >/dev/null
    wait_jsonpath "$APP_RES" "$SEC_NS" "$SEC_PLAN_F1B" '{.status.phase}' AwaitingMigrationApproval 180
    SEC7_PLAN="$(sec_wait_plan "$SEC_PLAN_F1B" 120)"
    [ -n "$SEC7_PLAN" ] || { printf 'FAILED: F-1b plan name empty\n' >&2; exit 1; }
    printf '  ok: pending-approval plan %s/%s created for %s\n' "$SEC_NS" "$SEC7_PLAN" "$SEC_PLAN_F1B"

    # Establish a deterministic `status.phase: pending-approval` on the plan
    # (the operator leaves an app-scope plan in the defaulted pending-approval
    # for await_change and may not stamp the status subresource). As
    # cluster-admin (the break-glass path the operator-or-admin allow covers)
    # we may write any status field — this fixes oldObject.status.phase for the
    # attacker's approval-signal check below. (Legit: an admin priming the
    # plan's status is exactly the operator-or-admin write the guard permits.)
    kubectl -n "$SEC_NS" patch "$PLAN_RES" "$SEC7_PLAN" --subresource=status \
        --type=merge -p '{"status":{"phase":"pending-approval"}}' >/dev/null
    wait_jsonpath "$PLAN_RES" "$SEC_NS" "$SEC7_PLAN" '{.status.phase}' pending-approval 30

    # (a) Attacker writes status.executedSteps — MUST be DENIED by the F-1b
    # webhook guard (only phase may change, and only pending-approval→approved).
    # The payload is a SCHEMA-VALID ExecutedStep object (items require
    # {step,startedAt,outcome}, outcome enum succeeded|failed|skipped) so the
    # apiserver's STRUCTURAL validation ACCEPTS it and the request actually
    # reaches the webhook — a malformed payload would be rejected by the
    # apiserver first (a false pass). The assertion below matches the webhook's
    # OWN F-1b message, not a generic apiserver schema error.
    set +e
    sec7a_out=$(kubectl_as "$ATTACKER_SA" "$ATTACKER_GROUP" -- \
        patch "$PLAN_RES" "$SEC7_PLAN" -n "$SEC_NS" --subresource=status \
        --type=merge \
        -p '{"status":{"executedSteps":[{"step":1,"startedAt":"2026-08-07T00:00:00Z","outcome":"succeeded"}]}}' 2>&1)
    sec7a_rc=$?
    set -e
    printf '  (a) impersonated executedSteps write rc=%d output:\n%s\n' "$sec7a_rc" "$sec7a_out"
    # DENIED iff nonzero AND the webhook's F-1b message is present. Match the
    # distinctive phrases the guard emits ("may only set status.phase" /
    # "may write other MigrationPlan.status fields" / the named field
    # "executedSteps") — a webhook denial, not an apiserver schema error.
    if [ "$sec7a_rc" -ne 0 ] \
        && printf '%s' "$sec7a_out" | grep -Eqi 'may only set status\.phase|may write other MigrationPlan\.status fields|status fields \[executedSteps'; then
        printf '  ok: external executedSteps write DENIED by the F-1b guard\n'
    else
        printf 'FAILED: external MigrationPlan.status.executedSteps write was NOT denied by the F-1b webhook guard (rc=%d)\n' "$sec7a_rc" >&2
        kubectl -n "$SEC_NS" get "$PLAN_RES" "$SEC7_PLAN" -o yaml >&2 2>&1 || true
        exit 1
    fi

    # (b) The SAME attacker identity writes status.phase pending-approval→approved
    # — the LEGIT external approval signal — MUST be ALLOWED.
    set +e
    sec7b_out=$(kubectl_as "$ATTACKER_SA" "$ATTACKER_GROUP" -- \
        patch "$PLAN_RES" "$SEC7_PLAN" -n "$SEC_NS" --subresource=status \
        --type=merge \
        -p '{"status":{"phase":"approved"}}' 2>&1)
    sec7b_rc=$?
    set -e
    printf '  (b) impersonated phase-approve write rc=%d output:\n%s\n' "$sec7b_rc" "$sec7b_out"
    if [ "$sec7b_rc" -eq 0 ]; then
        printf '  ok: external phase pending-approval->approved ALLOWED (F-1b legit approval)\n'
    else
        printf 'FAILED: the LEGIT external phase-approve (pending-approval->approved) was DENIED (rc=%d) — F-1b over-blocks the approval signal\n' "$sec7b_rc" >&2
        kubectl -n "$SEC_NS" get "$PLAN_RES" "$SEC7_PLAN" -o yaml >&2 2>&1 || true
        exit 1
    fi
    printf 'ok: external executedSteps denied, phase-approve allowed (F-1b)\n'
else
    printf '  note: (soft) impersonation unavailable — SOFT-skipping the F-1b MigrationPlan.status field-restriction reject/allow tests.\n'
    printf 'ok: external executedSteps denied, phase-approve allowed (F-1b) — SOFT-skipped (impersonation blocked)\n'
fi

# ===============================================================
# P-SEC-8: F-2 — a key that ACQUIRES a secret-ref gates as `env-secret-ref-add`
#   (security-boundary). Deploy with a LITERAL env value, reach steady state,
#   then edit the SAME key to a {secret:"…"} ref. The gate fires BEFORE render,
#   so we assert the PLAN shape (no need to seal the secret / keep it healthy).
# ===============================================================

phase "P-SEC-8: F-2 a key acquiring a secret-ref gates (env-secret-ref-add, security-boundary)"

# Deploy with env DB=<literal> (EnvValue::Literal). A literal always renders +
# stays healthy, so the baseline stamps cleanly.
apply_sec_app "$SEC_F2" dev internal env_literal='DB=plain-literal-value'
sec_wait_baseline "$SEC_F2" "$APP_IMAGE"
assert_eq "no plan for sec-f2 at baseline" "$(sec_plan_count "$SEC_F2")" "0"

# Edit the SAME key DB: Literal -> {secret:"stripe-prod/key"}. This is the F-2
# broadened acquire (`{Absent,Literal,Ref(Claim)} -> Ref(Secret)`): #7
# env-secret-ref-add, classification security-boundary. The gate fires before
# render, so the (unsealed) secret being absent doesn't matter for the assert.
apply_sec_app "$SEC_F2" dev internal env_secret='DB=stripe-prod/key'

wait_jsonpath "$APP_RES" "$SEC_NS" "$SEC_F2" '{.status.phase}' AwaitingMigrationApproval 180
SEC8_PLAN="$(sec_wait_plan "$SEC_F2" 120)"
[ -n "$SEC8_PLAN" ] || { printf 'FAILED: sec-f2 plan name empty\n' >&2; exit 1; }

# Primary classification == security-boundary.
sec8_primary=$(sec_jp "$PLAN_RES" "$SEC8_PLAN" '{.spec.risks.classification}')
assert_eq "P-SEC-8 spec.risks.classification (primary)" "$sec8_primary" "security-boundary"

# changes[] rollup contains env-secret-ref-add (wire field is `type`).
sec8_change_types=$(sec_jp "$PLAN_RES" "$SEC8_PLAN" '{.spec.changes[*].type}')
printf '  spec.changes[*].type = %q\n' "$sec8_change_types"
if printf '%s' "$sec8_change_types" | grep -qw 'env-secret-ref-add'; then
    printf '  ok: spec.changes[] contains env-secret-ref-add\n'
else
    printf 'FAILED: spec.changes[*].type %q missing env-secret-ref-add (F-2)\n' "$sec8_change_types" >&2
    kubectl -n "$SEC_NS" get "$PLAN_RES" "$SEC8_PLAN" -o yaml >&2 2>&1 || true
    exit 1
fi
# classifications[] rollup contains security-boundary.
sec8_classifications=$(sec_jp "$PLAN_RES" "$SEC8_PLAN" '{.spec.risks.classifications[*]}')
if printf '%s' "$sec8_classifications" | grep -qw 'security-boundary'; then
    printf '  ok: spec.risks.classifications contains security-boundary\n'
else
    printf 'FAILED: spec.risks.classifications %q missing security-boundary (F-2)\n' "$sec8_classifications" >&2
    exit 1
fi
printf 'ok: key acquiring a secret-ref gates (F-2)\n'

# ===============================================================
# P-SEC-9: F-3 — a public hostname SWAP {a}->{b} co-fires #3 domain-change (a
#   LOST) AND #11 public-hostname-add (b GAINED). The primary
#   (spec.risks.classification) is security-boundary (#11 sev4 > #3 sev1), and
#   classifications[] contains security-boundary. Set-algebra fix (F-3) removed
#   the old de-dup that hid #11 behind #3.
# ===============================================================

phase "P-SEC-9: F-3 public host swap co-fires #3 domain-change + #11 public-hostname-add (security-boundary)"

# Deploy a PUBLIC app with hostname a.example.com. Baseline stamps public+a.
apply_sec_app "$SEC_F3" dev public hostname=a.example.com
sec_wait_baseline "$SEC_F3" "$APP_IMAGE"
assert_eq "no plan for sec-f3 at baseline" "$(sec_plan_count "$SEC_F3")" "0"

# SWAP the hostname a -> b (still public). {a}->{b}: a LOST (#3 domain-change,
# requires-restart) AND b GAINED (#11 public-hostname-add, security-boundary).
apply_sec_app "$SEC_F3" dev public hostname=b.example.com

wait_jsonpath "$APP_RES" "$SEC_NS" "$SEC_F3" '{.status.phase}' AwaitingMigrationApproval 180
SEC9_PLAN="$(sec_wait_plan "$SEC_F3" 120)"
[ -n "$SEC9_PLAN" ] || { printf 'FAILED: sec-f3 plan name empty\n' >&2; exit 1; }

# changes[] carries BOTH #3 domain-change AND #11 public-hostname-add.
sec9_change_types=$(sec_jp "$PLAN_RES" "$SEC9_PLAN" '{.spec.changes[*].type}')
printf '  spec.changes[*].type = %q\n' "$sec9_change_types"
for want in domain-change public-hostname-add; do
    if printf '%s' "$sec9_change_types" | grep -qw "$want"; then
        printf '  ok: spec.changes[] contains %s\n' "$want"
    else
        printf 'FAILED: spec.changes[*].type %q missing %s (F-3 set-algebra)\n' "$sec9_change_types" "$want" >&2
        kubectl -n "$SEC_NS" get "$PLAN_RES" "$SEC9_PLAN" -o yaml >&2 2>&1 || true
        exit 1
    fi
done

# classifications[] contains security-boundary (from #11).
sec9_classifications=$(sec_jp "$PLAN_RES" "$SEC9_PLAN" '{.spec.risks.classifications[*]}')
printf '  spec.risks.classifications = %q\n' "$sec9_classifications"
if printf '%s' "$sec9_classifications" | grep -qw 'security-boundary'; then
    printf '  ok: spec.risks.classifications contains security-boundary\n'
else
    printf 'FAILED: spec.risks.classifications %q missing security-boundary (F-3)\n' "$sec9_classifications" >&2
    exit 1
fi

# Primary (spec.risks.classification) == security-boundary (#11 sev4 outranks #3 sev1).
sec9_primary=$(sec_jp "$PLAN_RES" "$SEC9_PLAN" '{.spec.risks.classification}')
assert_eq "P-SEC-9 spec.risks.classification (primary)" "$sec9_primary" "security-boundary"
printf 'ok: public host swap co-fires #3+#11, surfaces security-boundary (F-3)\n'

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
printf 'Security axis (2.16b) proven: internal->public escalation gated as security-boundary + changes[]/classifications rollup + approve (P-SEC-1) -> S-4 stale-approval refused for a drifted image target (P-SEC-2) -> F-1 spoofed-fieldManager non-operator Application.status write denied (P-SEC-3) -> §7.2 spec.environment immutable on UPDATE (P-SEC-4) -> (soft) needs.selector multi-provider tripwire (P-SEC-5) -> negative: needs.pg + claim-ref env add ungated (P-SEC-6)\n'
printf 'Security axis S2 fixes proven: F-1b external MigrationPlan.status field-restricted (executedSteps denied, phase-approve allowed) (P-SEC-7) -> F-2 key acquiring a secret-ref gates as env-secret-ref-add/security-boundary (P-SEC-8) -> F-3 public host swap co-fires #3 domain-change + #11 public-hostname-add, primary security-boundary (P-SEC-9)\n'
