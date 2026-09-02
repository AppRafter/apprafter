#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter needs-env-refs-walk e2e — the Phase-2.12 `Application.env`
# value-reference chain on a local kind/k3d cluster (ADR 0046).
#
# This script exercises the shipped 2.12 env-reference pipeline end-to-end,
# proving the OPERATOR side of ADR 0046 — the marker → secretKeyRef
# resolution for all three env sources, plus the EnvSecretMissing NotReady
# path:
#
#   provision (2.4c) -> decomposed connection-Secret keys (2.12, ADR 0046 #3)
#   -> resolve_env: literal | claim ref | secret ref -> EnvVar / secretKeyRef
#   -> EnvSecretMissing NotReady when the external Secret is missing -> recover
#
# Path taken — DIRECT-CR MARKER FORM (not the gitops bare-selector path).
# The MUST-HAVE for this live walk is the operator side (ADR 0046 #8): the
# Application CR carries the resolved markers `{claim: "pg.url"}` /
# `{secret: "appsecret/token"}`, and the operator's `resolve_env` expands
# them into container `EnvVar{valueFrom: secretKeyRef}`. The cue-cmp
# bare-selector rendering (`claim.pg.url` → `{claim: "pg.url"}`) is already
# covered by `argocd-cue-cmp/test-inject.sh` (host CMP test), so this walk
# applies the marker form directly and skips the git-daemon + Argo + cue-cmp
# machinery — the lighter path that proves the operator chain.
#
# Concretely, on a cluster bootstrapped with the platform stack
# (operator + admission-webhook + the always-on CNPG operator + the seeded
# `pg-integrated` ServiceProvider), with the WORKING-TREE 2.12 operator +
# webhook side-loaded (Phase 1b — 2.12 is UNRELEASED, the published chart
# predates the `env` value node, the webhook rules, and the decomposed
# connection-Secret keys):
#
#   1. Create the app namespace + an external `appsecret` Secret
#      (token=s3cr3t).
#   2. Apply an AppRafter Application with `spec.base.needs.pg` and an
#      `env` map carrying ALL THREE sources:
#        LOG_LEVEL: "info"                      (literal)
#        DATABASE_URL: {claim: "pg.url"}        (claim ref, composed DSN)
#        DB_USER:      {claim: "pg.user"}       (claim ref, decomposed)
#        DB_PASS:      {claim: "pg.pass"}       (claim ref, decomposed)
#        STRIPE_KEY:   {secret: "appsecret/token"}  (external secret ref)
#      Wait for status.phase == Ready.
#   3. ASSERT the rendered Deployment's container env:
#        - LOG_LEVEL is a LITERAL ("info"), NOT a secretKeyRef.
#        - DATABASE_URL/DB_USER/DB_PASS are secretKeyRefs into the pg
#          connection Secret with keys url/user/pass respectively.
#        - STRIPE_KEY is a secretKeyRef → appsecret/token.
#        - There is NO auto-injected DATABASE_URL beyond the explicit ref
#          (the only env entry whose name is DATABASE_URL is the one we
#          declared; 2.4e auto-inject is removed in ADR 0046 #5).
#   4. exec the pod and ASSERT the RESOLVED VALUES are present:
#        - DATABASE_URL is a postgres:// DSN.
#        - DB_USER is the managed role; DB_PASS is the role password.
#        - STRIPE_KEY == s3cr3t (the external Secret value).
#        - LOG_LEVEL == info.
#   5. Delete the `appsecret` Secret; ASSERT the Application flips to
#      Ready=False reason=EnvSecretMissing (phase EnvSecretMissing).
#      Re-create it under the WRONG KEY; ASSERT the message distinguishes
#      that from absence and names the namespace, the Secret and the keys it
#      DOES carry (2.22c / D7 — the error has to answer the question it
#      raises, or the reader goes to kubectl for all three candidate causes).
#      Re-create it correctly; ASSERT the Application recovers to Ready.
#   6. Rotate the VALUE under the same key; ASSERT `status.envConfig.digest`
#      and `changedAt` both move, that `changedAt` lands NEWER than the
#      pre-rotation pod's startTime (the comparison `apprafter app status`
#      renders as `← old config`), and — just as hard — that NOTHING rolled:
#      same pod, same restart count, same Deployment generation (2.22c / D6).
#      The automatic roll was explicitly rejected as a Tier-1 default, so a
#      walk asserting the value reached the pod would be testing the
#      behaviour that was turned down.
#
# CLI state injection
# -------------------
# `apprafter cluster-bootstrap` reads the kubeconfig from the CLI's
# per-target state store (not from $KUBECONFIG). We set
# APPRAFTER_CONFIG_DIR to a tmpdir and seed it with a minimal
# config.yaml (active_target: k3d) and a state.json carrying the
# kubeconfig as kubeconfig_yaml (plaintext) — the same approach
# needs-pg-walk.sh / needs-disk-walk.sh use.
#
# Local-operator mode (REQUIRED here — 2.12 is UNRELEASED)
# -------------------------------------------------------
# Unlike the shipped needs.pg / needs.disk walks, this walk REQUIRES
# APPRAFTER_E2E_LOCAL_OPERATOR=1: the published operator + webhook + CRD
# predate the 2.12 `env` value node, so the default published path cannot
# render/validate the env refs. Phase 1b builds the operator + webhook from
# THIS branch, side-loads them, and applies the branch-rendered CRDs (the
# `env` value node, webhook rules) — mirrors needs-disk-walk Phase 1b. The
# walk hard-fails (exit 2) if the flag is unset.
#
# Required: docker (or podman), cargo, kubectl
#   — all satisfied inside `nix develop` or on a standard CI runner.
#
# Exit codes:
#   0 — chain green
#   1 — assertion failure
#   2 — precondition missing

set -euo pipefail

# ---------------------------------------------------------------
# Source shared helpers
# ---------------------------------------------------------------

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# ---------------------------------------------------------------
# Constants — the shipped needs.pg coordinates (derived from the
# claim's (namespace, name) by the 2.4c provisioner). See
# operator/operator-controllers/resourceclaim-provisioner/src/reconcile.rs
# (connection_secret_object) + the Application controller's claim_name.
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-env-refs-walk"

APP_NS="demo"                       # tenant namespace
APP="web"                           # Application name
CLAIM="web-pg"                      # generated ResourceClaim name
CONN_SECRET="web-pg-conn"           # connection Secret (app ns)

EXT_SECRET="appsecret"              # external (user-managed) Secret name
EXT_KEY="token"                     # external Secret key
EXT_VAL="s3cr3t"                    # external Secret value

# Group-qualify the two collision-prone kinds so kubectl never resolves
# to the wrong API group: bare `application` also matches Argo CD's
# argoproj.io Application, and bare `resourceclaim` matches the k8s 1.32+
# DRA resource.k8s.io ResourceClaim. Always address the apprafter.io CRs.
APP_RES="application.apprafter.io"
CLAIM_RES="resourceclaim.apprafter.io"

PG_ROLE="claim_demo_web_pg"         # pg_identifier(demo, web-pg)
CNPG_NS="cnpg-system"               # shared CNPG Cluster namespace
RETAINED_NS="apprafter-system"      # ServiceProvider namespace
PROVIDER="pg-integrated"            # seeded ServiceProvider

# ---------------------------------------------------------------
# Precondition: this walk REQUIRES local-operator mode (2.12 unreleased).
# ---------------------------------------------------------------

if [ -z "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
    cat >&2 <<'EOF'
ERROR: needs-env-refs-walk requires APPRAFTER_E2E_LOCAL_OPERATOR=1.

Phase 2.12 (Application.env value references) is UNRELEASED — the published
operator + admission-webhook + CRD predate the `env` value node, the webhook
env-ref rules, and the decomposed connection-Secret keys. Run:

  APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/needs-env-refs-walk.sh

so the walk builds + side-loads the working-tree operator/webhook and applies
the branch CRDs (Phase 1b).
EOF
    exit 2
fi

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------

for tool in cargo kubectl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done

# A container runtime: docker (→ k3d) or podman (→ kind). cluster_runtime
# in lib.sh picks the backend; here we only assert one of them exists.
if ! command -v docker >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
    printf 'ERROR: neither "docker" nor "podman" found on PATH\n' >&2
    exit 2
fi

# ---------------------------------------------------------------
# Temp workspace
# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"

# Set to 1 only after the cluster is up AND $KUBECONFIG points at it
# (Phase 0). Until then, dump_diagnostics / k3d_down must NOT run — on a
# cluster-up failure $KUBECONFIG still points at the operator's ambient
# cluster, and an e2e must never touch a non-test cluster.
K3D_CREATED=0

cleanup() {
    local exit_code=$?

    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! needs-env-refs-walk FAILED at %s (exit %d) !!!\n' \
            "$(elapsed)" "$exit_code" >&2
        if [ "$K3D_CREATED" -eq 1 ]; then
            dump_diagnostics
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
# Helper: seed the CLI state store (mirrors needs-pg-walk.sh).
#
# Writes:
#   $APPRAFTER_CONFIG_DIR/config.yaml      — active_target: k3d
#   $APPRAFTER_CONFIG_DIR/state/k3d/.apprafter/state.json
#     hetzner_cloud.kubeconfig_yaml = <kubeconfig plaintext>
# ---------------------------------------------------------------
seed_apprafter_state() {
    local kubeconfig_content="$1"

    mkdir -p "${APPRAFTER_CONFIG_DIR}"
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/k3d/.apprafter"

    cat >"${APPRAFTER_CONFIG_DIR}/config.yaml" <<'YAML'
active_target: k3d
version: 1
YAML

    # Escape the kubeconfig for JSON embedding (replace newlines with
    # \n, escape double-quotes and backslashes).
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
# Local helper: wait_jsonpath <kind> <ns> <name> <jsonpath> <want> [timeout]
#   Poll `kubectl get -o jsonpath` every 5s until the rendered value
#   equals <want> or the deadline passes. Prints an ERROR + returns 1
#   on timeout (with a describe for diagnostics).
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
# Local helper: assert_nonempty <description> <got>
# ---------------------------------------------------------------
assert_nonempty() {
    local desc="$1" got="$2"
    if [ -n "$got" ]; then
        printf '  ok: %s = %q\n' "$desc" "$got"
        return 0
    fi
    printf 'ERROR: %s — got empty value\n' "$desc" >&2
    return 1
}

# ---------------------------------------------------------------
# Local helper: jp <kind> <ns> <name> <jsonpath>  (read one value)
# ---------------------------------------------------------------
jp() {
    kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true
}

# Condition-status reader: the `.status` of a condition selected by
# `type`. Pulls the conditions array via a jsonpath filter.
cond_status() {
    # $1 kind, $2 ns, $3 name, $4 condition-type
    kubectl -n "$2" get "$1" "$3" -o \
        jsonpath="{.status.conditions[?(@.type==\"$4\")].status}" \
        2>/dev/null || true
}

cond_reason() {
    # $1 kind, $2 ns, $3 name, $4 condition-type
    kubectl -n "$2" get "$1" "$3" -o \
        jsonpath="{.status.conditions[?(@.type==\"$4\")].reason}" \
        2>/dev/null || true
}

cond_message() {
    # $1 kind, $2 ns, $3 name, $4 condition-type
    kubectl -n "$2" get "$1" "$3" -o \
        jsonpath="{.status.conditions[?(@.type==\"$4\")].message}" \
        2>/dev/null || true
}

# ---------------------------------------------------------------
# Local helper: assert_contains <description> <haystack> <needle>
# ---------------------------------------------------------------
assert_contains() {
    local desc="$1" hay="$2" needle="$3"
    case "$hay" in
        *"$needle"*)
            printf '  ok: %s (found %q)\n' "$desc" "$needle"
            return 0
            ;;
    esac
    printf 'ERROR: %s — %q not found in:\n%s\n' "$desc" "$needle" "$hay" >&2
    return 1
}

# Wait until a condition's message contains a substring, or time out.
wait_cond_message() {
    # $1 kind, $2 ns, $3 name, $4 type, $5 needle, $6 timeout-seconds
    local deadline msg
    deadline=$(( $(date +%s) + $6 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        msg="$(cond_message "$1" "$2" "$3" "$4")"
        case "$msg" in
            *"$5"*) printf '%s' "$msg"; return 0 ;;
        esac
        sleep 3
    done
    printf '%s' "$msg"
    return 1
}

# ---------------------------------------------------------------
# Local helper: env_ref_secret_name / env_ref_secret_key — read the
# secretKeyRef name/key for a named env var off the Deployment's
# container[0].env, and env_literal — read a literal value.
# ---------------------------------------------------------------
env_ref_secret_name() {
    kubectl -n "$APP_NS" get deployment "$APP" \
        -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"$1\")].valueFrom.secretKeyRef.name}" \
        2>/dev/null || true
}
env_ref_secret_key() {
    kubectl -n "$APP_NS" get deployment "$APP" \
        -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"$1\")].valueFrom.secretKeyRef.key}" \
        2>/dev/null || true
}
env_literal() {
    kubectl -n "$APP_NS" get deployment "$APP" \
        -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"$1\")].value}" \
        2>/dev/null || true
}
# Count how many env entries on container[0] are named $1 — a jsonpath
# filter selects all matches; printing their `.name` and counting words
# yields the multiplicity (proves there is exactly ONE DATABASE_URL, i.e.
# no auto-inject duplicate beyond the explicit ref).
env_name_count() {
    local names
    names=$(kubectl -n "$APP_NS" get deployment "$APP" \
        -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"$1\")].name}" \
        2>/dev/null || true)
    # jsonpath joins multiple matches with a space; count the tokens.
    printf '%s' "$names" | wc -w | tr -d ' '
}

# ---------------------------------------------------------------
# Local helper: web_pod — the current running web pod name.
# ---------------------------------------------------------------
web_pod() {
    kubectl -n "$APP_NS" get pod -l app.kubernetes.io/name="$APP" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# ---------------------------------------------------------------
# Local helper: pod_env <pod> <var> — read a resolved env var off a
# RUNNING pod via `kubectl exec ... printenv`. secretKeyRef values are
# materialised into the container's env at start, so this proves the
# resolution end-to-end (not just the spec wiring).
# ---------------------------------------------------------------
pod_env() {
    local pod="$1" var="$2"
    kubectl -n "$APP_NS" exec "$pod" -- printenv "$var" 2>/dev/null | tr -d '\r' || true
}

# ===============================================================
# Phase 0: bring up the cluster
# ===============================================================

phase "Phase 0: k3d_up ${CLUSTER_NAME}"

k3d_up "$CLUSTER_NAME"

cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
# The cluster exists and $KUBECONFIG now points at it — cleanup may
# safely diagnose/tear it down (and only it).
K3D_CREATED=1
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

# ===============================================================
# Phase 1: seed CLI state and run cluster-bootstrap
# ===============================================================

phase "Phase 1: cluster-bootstrap (platform stack + CNPG + pg-integrated)"

kubeconfig_content=$(cat "$KUBECONFIG_FILE")
seed_apprafter_state "$kubeconfig_content"
export APPRAFTER_CONFIG_DIR
printf '  APPRAFTER_CONFIG_DIR=%s\n' "$APPRAFTER_CONFIG_DIR"

bootstrap_with_retry

printf '  cluster-bootstrap complete\n'

# ---------------------------------------------------------------
# Phase 1b (REQUIRED — APPRAFTER_E2E_LOCAL_OPERATOR): build + side-load the
# working-tree operator + webhook instead of the published image, then apply
# the branch CRDs. 2.12 (the `env` value node + webhook env-ref rules +
# decomposed connection-Secret keys) is UNRELEASED, so the published image +
# CRD this cluster bootstrapped from cannot render/validate env refs. Mirrors
# needs-disk-walk Phase 1b (no extra RBAC — env refs add no new k8s verb;
# resolve_env only reads Secrets the operator already watches).
# NOTE: the if-body below is intentionally NOT indented — it carries a
# column-0 heredoc-free apply pipeline; `fi` closes it just before Phase 2.
# ---------------------------------------------------------------
if [ -n "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
phase "Phase 1b: build + load local operator + webhook (APPRAFTER_E2E_LOCAL_OPERATOR)"
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
# (released) pod lingers Terminating for its grace period — and during that
# window a branch CR apply (the Phase 3 env-ref Application) can still route
# to the old webhook, whose released validator lacks the 2.12 env-ref rules
# (and would reject the `env` claim/secret objects). Wait until ONLY the
# branch webhook serves before any branch-typed apply.
printf '  waiting for the old (released) webhook pod to fully terminate ...\n'
_wh_deadline=$(( $(date +%s) + 90 ))
while [ "$(date +%s)" -lt "$_wh_deadline" ]; do
    [ "$(kubectl -n apprafter-system get pods \
        -l app.kubernetes.io/name=admission-webhook --no-headers 2>/dev/null \
        | wc -l)" -le 1 ] && break
    sleep 3
done
printf '  apprafter-operator + admission-webhook now running the working-tree build\n'

# The released platform-stack chart this cluster bootstrapped from predates
# the 2.12 CRD change (Application.spec.base.env / environments[*].env now
# accept a string-OR-object #EnvValue via x-kubernetes-preserve-unknown-fields).
# Argo CD owns those CRDs via the apprafter-operator Application, so: disable
# automated sync on the parent + operator apps (else Argo reverts the drift),
# then apply the BRANCH-rendered CRDs server-side. Same rationale as
# side-loading the operator image — both are unpublished 2.12 artifacts; this
# whole walk is LOCAL_OPERATOR-gated (pre-release). Mirrors
# scripts/validate-crds.sh's render + needs-disk-walk Phase 1b.
printf '  applying branch operator CRDs (released chart predates 2.12 env node) ...\n'
for _app in platform apprafter-operator; do
    kubectl -n argocd patch applications.argoproj.io "$_app" --type=merge \
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
# Phase 2: readiness — CNPG operator, the seeded provider, the webhook
# ===============================================================

phase "Phase 2: platform readiness (AppProject, CNPG operator, provider, webhook)"

# The `apps` AppProject is created by the platform-stack chart.
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
    exit 1
}

# CNPG operator Deployment must be Available before any claim can be
# provisioned (the provisioner SSA-applies CNPG Cluster/Database CRs).
printf '  waiting for the CNPG operator Deployment ...\n'
retry 30 10 -- kubectl -n "$CNPG_NS" rollout status \
    deploy -l app.kubernetes.io/name=cloudnative-pg --timeout=60s

# ASSERT the `pg-integrated` ServiceProvider is seeded with the
# tier=integrated label the needs.pg selector matches.
retry 30 10 -- kubectl get serviceprovider "$PROVIDER" \
    -n "$RETAINED_NS" >/dev/null
sp_tier=$(jp serviceprovider "$RETAINED_NS" "$PROVIDER" '{.metadata.labels.tier}')
assert_eq "ServiceProvider ${PROVIDER} label tier" "$sp_tier" "integrated"

# The admission webhook must be Available before applying the
# env-ref Application (it validates the CR on CREATE).
printf '  waiting for the admission-webhook Deployment ...\n'
retry 30 10 -- kubectl -n "$RETAINED_NS" rollout status \
    deploy admission-webhook --timeout=60s

# ===============================================================
# Phase 3: create the namespace + external Secret, apply the env-ref App
# ===============================================================

phase "Phase 3: namespace + appsecret Secret + env-ref Application"

kubectl create namespace "$APP_NS" 2>/dev/null || true

# The external (user-managed) Secret the `{secret: "appsecret/token"}` ref
# resolves to. Created directly — the operator only READS a Secret (no full
# SealedSecrets seal flow needed for this walk).
kubectl -n "$APP_NS" create secret generic "$EXT_SECRET" \
    --from-literal="${EXT_KEY}=${EXT_VAL}" \
    --dry-run=client -o yaml | kubectl apply -f -
printf '  created Secret %s/%s with key %s\n' "$APP_NS" "$EXT_SECRET" "$EXT_KEY"

# The env map carries ALL THREE sources in MARKER form (the cue-cmp would
# render bare `claim.pg.url` → `{claim: "pg.url"}`; we apply that resolved
# form directly — see the path note in the header).
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP}
  namespace: ${APP_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    expose:
      port: 80
    needs:
      pg:
        selector:
          tier: integrated
        size: small
    env:
      LOG_LEVEL: "info"
      DATABASE_URL:
        claim: "pg.url"
      DB_USER:
        claim: "pg.user"
      DB_PASS:
        claim: "pg.pass"
      STRIPE_KEY:
        secret: "${EXT_SECRET}/${EXT_KEY}"
YAML

# Sanity: the operator generated the ResourceClaim (the 2.4d gate's
# load-bearing action — it emits a claim instead of rendering immediately).
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.spec.type}' pg 180

# The claim provisions (lazy CNPG Cluster boot is the slow step).
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.ready}' true 300

# The connection Secret carries the DECOMPOSED keys (ADR 0046 #3): url,
# user, pass, host, port, db — and NO `DATABASE_URL` key (dropped with the
# 2.4e auto-inject). Prove the keys resolve_env reads exist.
for _k in url user pass; do
    _v=$(jp secret "$APP_NS" "$CONN_SECRET" "{.data.${_k}}")
    assert_nonempty "connection Secret ${CONN_SECRET} key ${_k}" "$_v"
done
# The old composed key must be GONE (the 2.4e DATABASE_URL key is removed).
_old=$(jp secret "$APP_NS" "$CONN_SECRET" '{.data.DATABASE_URL}')
assert_eq "connection Secret has NO DATABASE_URL key (2.4e dropped)" "$_old" ""

# Application resumes to Ready (every env secret ref resolves: appsecret
# exists; claim refs are gated-ready).
wait_jsonpath "$APP_RES" "$APP_NS" "$APP" '{.status.phase}' Ready 180

# ===============================================================
# Phase 4: assert the rendered env — wiring + resolved values
# ===============================================================

phase "Phase 4: rendered env wiring + resolved pod values (all three sources)"

# ---- (a) the rendered Deployment container env wiring ----

# LOG_LEVEL is a LITERAL, NOT a secretKeyRef.
log_literal=$(env_literal LOG_LEVEL)
assert_eq "LOG_LEVEL literal value" "$log_literal" "info"
log_ref=$(env_ref_secret_name LOG_LEVEL)
assert_eq "LOG_LEVEL is NOT a secretKeyRef" "$log_ref" ""

# DATABASE_URL → secretKeyRef into the pg connection Secret, key `url`.
dburl_name=$(env_ref_secret_name DATABASE_URL)
assert_eq "DATABASE_URL secretKeyRef.name" "$dburl_name" "$CONN_SECRET"
dburl_key=$(env_ref_secret_key DATABASE_URL)
assert_eq "DATABASE_URL secretKeyRef.key" "$dburl_key" "url"

# DB_USER → key `user`; DB_PASS → key `pass`.
dbuser_name=$(env_ref_secret_name DB_USER)
assert_eq "DB_USER secretKeyRef.name" "$dbuser_name" "$CONN_SECRET"
dbuser_key=$(env_ref_secret_key DB_USER)
assert_eq "DB_USER secretKeyRef.key" "$dbuser_key" "user"
dbpass_name=$(env_ref_secret_name DB_PASS)
assert_eq "DB_PASS secretKeyRef.name" "$dbpass_name" "$CONN_SECRET"
dbpass_key=$(env_ref_secret_key DB_PASS)
assert_eq "DB_PASS secretKeyRef.key" "$dbpass_key" "pass"

# STRIPE_KEY → secretKeyRef into the EXTERNAL Secret, key `token`.
stripe_name=$(env_ref_secret_name STRIPE_KEY)
assert_eq "STRIPE_KEY secretKeyRef.name" "$stripe_name" "$EXT_SECRET"
stripe_key=$(env_ref_secret_key STRIPE_KEY)
assert_eq "STRIPE_KEY secretKeyRef.key" "$stripe_key" "$EXT_KEY"

# NO auto-injected DATABASE_URL beyond the explicit ref: exactly ONE env
# entry is named DATABASE_URL (the one we declared). 2.4e auto-inject is
# removed (ADR 0046 #5).
dburl_count=$(env_name_count DATABASE_URL)
assert_eq "exactly one DATABASE_URL env entry (no auto-inject)" "$dburl_count" "1"

# ---- (b) the RESOLVED VALUES on the running pod ----

kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP}" --timeout=300s
printf '  Deployment %s -> Available\n' "$APP"

POD=$(web_pod)
assert_nonempty "running web pod" "$POD"
kubectl -n "$APP_NS" wait --for=condition=Ready "pod/${POD}" --timeout=120s

# LOG_LEVEL resolves to the literal.
got=$(pod_env "$POD" LOG_LEVEL)
assert_eq "pod LOG_LEVEL resolved" "$got" "info"

# DATABASE_URL resolves to a postgres DSN.
got=$(pod_env "$POD" DATABASE_URL)
case "$got" in
    postgres://*|postgresql://*) printf '  ok: pod DATABASE_URL is a DSN: %s\n' "$got" ;;
    *) printf 'ERROR: pod DATABASE_URL not a postgres DSN: %q\n' "$got" >&2; exit 1 ;;
esac

# DB_USER resolves to the managed role; DB_PASS is the non-empty password.
got=$(pod_env "$POD" DB_USER)
assert_eq "pod DB_USER resolved (managed role)" "$got" "$PG_ROLE"
got=$(pod_env "$POD" DB_PASS)
assert_nonempty "pod DB_PASS resolved (role password)" "$got"

# STRIPE_KEY resolves to the EXTERNAL Secret value.
got=$(pod_env "$POD" STRIPE_KEY)
assert_eq "pod STRIPE_KEY resolved (external secret value)" "$got" "$EXT_VAL"

printf '  all three env sources resolved on the pod: literal + claim(url/user/pass) + secret\n'

# ===============================================================
# Phase 5: EnvSecretMissing NotReady path + recovery
# ===============================================================

phase "Phase 5: delete appsecret -> Ready=False/EnvSecretMissing -> recover"

# Delete the external Secret the STRIPE_KEY ref depends on. The operator
# re-reconciles (it watches Secrets in the app namespace) and, per ADR 0046
# Decision #4 (runtime existence check), sets Ready=False/EnvSecretMissing
# WITHOUT rendering — the missing Secret may never come back.
kubectl -n "$APP_NS" delete secret "$EXT_SECRET" --wait=true

# Application flips to Ready=False, reason EnvSecretMissing (phase mirrors
# the reason).
wait_jsonpath "$APP_RES" "$APP_NS" "$APP" '{.status.phase}' EnvSecretMissing 120
ready=$(cond_status "$APP_RES" "$APP_NS" "$APP" Ready)
assert_eq "Ready condition status after secret delete" "$ready" "False"
reason=$(cond_reason "$APP_RES" "$APP_NS" "$APP" Ready)
assert_eq "Ready condition reason" "$reason" "EnvSecretMissing"

# --- D7: the error answers the question it raises ------------------------
#
# Re-create the Secret, but under the WRONG KEY — the `stripe_api_key` versus
# `stripe-api-key` slip the fix was written for. Before 2.22c the operator said
# only that a ref was unresolved, which sends the reader to kubectl to find out
# whether the Secret is missing, in the wrong namespace, or simply spelled
# differently. Those are three different problems with one message.
#
# The Secret now EXISTS, so this also separates the two failures: "no Secret"
# and "Secret without that key" must not read identically.
kubectl -n "$APP_NS" create secret generic "$EXT_SECRET" \
    --from-literal="wrong_key_name=${EXT_VAL}" \
    --dry-run=client -o yaml | kubectl apply -f -
printf '  re-created Secret %s/%s under the WRONG key\n' "$APP_NS" "$EXT_SECRET"

msg="$(wait_cond_message "$APP_RES" "$APP_NS" "$APP" Ready "carries no key" 120)" || {
    printf 'ERROR: the Ready message never named the missing key. Got: %s\n' "$msg" >&2
    exit 1; }
assert_contains "the message distinguishes present-but-wrong-key from absent" \
    "$msg" "carries no key"
assert_contains "the message names the namespace, so it need not be guessed" \
    "$msg" "namespace \"${APP_NS}\""
assert_contains "the message names the Secret" "$msg" "\"${EXT_SECRET}\""
# The half that turns the message into an answer: the keys that ARE there.
# Without this line the reader still has to go and look.
assert_contains "the message lists the key the Secret actually carries" \
    "$msg" "wrong_key_name"
reason=$(cond_reason "$APP_RES" "$APP_NS" "$APP" Ready)
assert_eq "wrong key is still EnvSecretMissing, not a new reason" "$reason" "EnvSecretMissing"

# Re-create the Secret correctly; the operator recovers the Application to Ready.
kubectl -n "$APP_NS" create secret generic "$EXT_SECRET" \
    --from-literal="${EXT_KEY}=${EXT_VAL}" \
    --dry-run=client -o yaml | kubectl apply -f -
printf '  re-created Secret %s/%s\n' "$APP_NS" "$EXT_SECRET"

wait_jsonpath "$APP_RES" "$APP_NS" "$APP" '{.status.phase}' Ready 180
ready=$(cond_status "$APP_RES" "$APP_NS" "$APP" Ready)
assert_eq "Ready condition status after secret recreate" "$ready" "True"

# ===============================================================
# Phase 6: rotating a secret is VISIBLE without being ACTED on (D6)
# ===============================================================
phase "Phase 6: rotate the secret value -> envConfig digest + changedAt move, pods do NOT roll"

# D6's decision, and the reason this phase asserts a negative as hard as it
# asserts a positive: an env var sourced from a Secret is resolved once at pod
# start and never re-read, so a rotated secret silently does nothing until
# something restarts the workload. The fix makes that VISIBLE
# (`status.envConfig.changedAt` versus each pod's startTime) and deliberately
# does NOT make it ACT — an automatic roll was rejected as a Tier-1 default,
# because a Secret is not owned by one Application and the blast radius of
# rolling every consumer is unknowable to whoever sealed it.
#
# So a walk that only checked "the value eventually reaches the pod" would be
# asserting the behaviour that was explicitly turned down.

digest_before=$(jp "$APP_RES" "$APP_NS" "$APP" '{.status.envConfig.digest}')
changed_before=$(jp "$APP_RES" "$APP_NS" "$APP" '{.status.envConfig.changedAt}')
assert_nonempty "envConfig.digest is recorded while Ready" "$digest_before"
assert_nonempty "envConfig.changedAt is recorded while Ready" "$changed_before"

pod_before=$(kubectl -n "$APP_NS" get pods -l "app.kubernetes.io/name=${APP}" \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
restarts_before=$(kubectl -n "$APP_NS" get pods -l "app.kubernetes.io/name=${APP}" \
    -o jsonpath='{.items[0].status.containerStatuses[0].restartCount}' 2>/dev/null || true)
gen_before=$(jp deployment "$APP_NS" "$APP" '{.metadata.generation}')
assert_nonempty "a workload pod exists before the rotation" "$pod_before"

# Rotate the VALUE under the same key — the ordinary credential rotation.
kubectl -n "$APP_NS" create secret generic "$EXT_SECRET" \
    --from-literal="${EXT_KEY}=rotated-${EXT_VAL}" \
    --dry-run=client -o yaml | kubectl apply -f -
printf '  rotated the value of %s/%s under key %s\n' "$APP_NS" "$EXT_SECRET" "$EXT_KEY"

# The digest is over the RESOLVED values, so a new value must move it.
_d_deadline=$(( $(date +%s) + 180 ))
digest_after="$digest_before"
while [ "$(date +%s)" -lt "$_d_deadline" ]; do
    digest_after=$(jp "$APP_RES" "$APP_NS" "$APP" '{.status.envConfig.digest}')
    [ -n "$digest_after" ] && [ "$digest_after" != "$digest_before" ] && break
    sleep 3
done
if [ "$digest_after" = "$digest_before" ]; then
    printf 'ERROR: envConfig.digest did not move after the value rotation (still %q)\n' \
        "$digest_before" >&2
    printf '  the drift signal is the whole of D6 — if it does not move, nothing downstream can see the rotation\n' >&2
    exit 1
fi
printf '  ok: envConfig.digest moved on rotation (%.12s… -> %.12s…)\n' "$digest_before" "$digest_after"

changed_after=$(jp "$APP_RES" "$APP_NS" "$APP" '{.status.envConfig.changedAt}')
if [ "$changed_after" = "$changed_before" ]; then
    printf 'ERROR: envConfig.changedAt did not move although the digest did (%s)\n' "$changed_after" >&2
    exit 1
fi
printf '  ok: envConfig.changedAt moved with it (%s -> %s)\n' "$changed_before" "$changed_after"

# changedAt is only usable as a drift boundary if it is NEWER than the pod that
# predates the change. This is the comparison `apprafter app status` renders as
# `← old config`.
pod_started=$(kubectl -n "$APP_NS" get pod "$pod_before" \
    -o jsonpath='{.status.startTime}' 2>/dev/null || true)
assert_nonempty "the pre-rotation pod reports a startTime" "$pod_started"
newer=$(python3 - "$pod_started" "$changed_after" <<'PY'
import sys
from datetime import datetime
def p(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00"))
print("yes" if p(sys.argv[2]) > p(sys.argv[1]) else "no")
PY
)
assert_eq "changedAt is newer than the pod that predates the rotation" "$newer" "yes"

# AND THE NEGATIVE. Nothing rolled: same pod, same restart count, same
# Deployment generation. If a future change starts stamping the digest onto the
# pod template, every one of these flips and this phase is what says so.
pod_after=$(kubectl -n "$APP_NS" get pods -l "app.kubernetes.io/name=${APP}" \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
restarts_after=$(kubectl -n "$APP_NS" get pods -l "app.kubernetes.io/name=${APP}" \
    -o jsonpath='{.items[0].status.containerStatuses[0].restartCount}' 2>/dev/null || true)
gen_after=$(jp deployment "$APP_NS" "$APP" '{.metadata.generation}')
assert_eq "the rotation did NOT replace the pod" "$pod_after" "$pod_before"
assert_eq "the rotation did NOT restart the container" "$restarts_after" "$restarts_before"
assert_eq "the rotation did NOT bump the Deployment generation" "$gen_after" "$gen_before"

# ===============================================================
# Done — tear down on success path
# ===============================================================

# Remove the EXIT trap so cleanup() does not fire again — we own the
# tear-down inline here on the success path.
trap - EXIT

if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    k3d_down "$CLUSTER_NAME" || true
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' \
        "$CLUSTER_NAME"
fi

rm -rf "$TMPDIR_WORK"

printf '\nneeds-env-refs-walk GREEN in %s\n' "$(elapsed)"
printf 'Chain proven: provision -> decomposed conn-Secret keys -> resolve_env (literal + claim url/user/pass + external secret) -> resolved pod values -> EnvSecretMissing NotReady (absent AND wrong-key, message names ns + available keys, D7) -> recover -> rotation moves envConfig digest+changedAt without rolling the pod (D6)\n'
