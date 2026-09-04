#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter needs-removal walk e2e — the operator deletes the children it
# no longer declares (plan item 2.22b; D4 / D12 in the day-2 ledger).
#
# WHY THIS WALK EXISTS
#
# Every existing needs walk deletes the WHOLE Application to reach the
# retention path. None of them removes a single `needs.*` entry from an
# app that keeps running — which is exactly why the defect survived every
# gate for months: four documents said the backing claim is
# garbage-collected, nothing deleted it, and the operator's RBAC carried
# no `delete` verb on `resourceclaims` at all, so no code path could have.
#
# The claim shape is deliberately TWO NAMED pg claims on ONE app rather
# than pg + redis. It exercises the desired-set diff with a single
# provider, and it makes the sibling assertion sharp: the claim that is
# still declared must be untouched, by uid, not merely present.
#
# WHAT IT ASSERTS
#
#   1. An app with two named pg claims provisions both and goes Ready.
#   2. Removing ONE need creates a MigrationPlan (`needs-removal`,
#      classified `data-migration`) and gates the change — nothing is
#      destroyed before approval.
#   3. On approval the removed claim is deleted, its finalizer writes a
#      RetainedClaim, and its connection Secret cascades.
#   4. The SIBLING claim is untouched — same uid, still ready, secret
#      intact — and the Application returns to Ready.
#   5. Removing `expose` prunes the Service (D12), while the Deployment
#      survives. That arm never existed: 1.83b pruned the HTTPRoute for
#      the same transition and recorded the Service's omission in a code
#      comment rather than fixing it.
#   6. NEGATIVE CONTROL: a second Application's claim in the same
#      namespace is never touched. `claims_to_prune` filters on the
#      controller ownerRef uid, and an empty uid prunes nothing; this
#      proves the filter live rather than in a unit test.
#
# Usage:
#   APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/needs-removal-walk.sh
#
# Judge the outcome by READING THE LOG — every phase prints `ok:` lines
# and the run ends with a GREEN banner. Under `sandbox-run` the reported
# exit code is not trustworthy.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "${SCRIPT_DIR}/lib.sh"

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-removal-walk"

APP_NS="demo"
APP="parser"                          # the app that loses a need
APP_RES="application.apprafter.io"
CLAIM_RES="resourceclaim.apprafter.io"

# 2.6b named claims: `needs.pg` as an ARRAY yields `<app>-pg-<name>`.
CLAIM_KEEP="parser-pg-primary"
CLAIM_DROP="parser-pg-analytics"
SECRET_KEEP="parser-pg-primary-conn"
SECRET_DROP="parser-pg-analytics-conn"
# RetainedClaim name = k8s_name(namespace, claim)
RETAINED_DROP="claim-demo-parser-pg-analytics"

# The negative control: a different Application, same namespace.
APP2="ledger"
CLAIM2="ledger-pg"

CNPG_NS="cnpg-system"
CNPG_CLUSTER="platform-postgres"
RETAINED_NS="apprafter-system"
PROVIDER="pg-integrated"

# The reaper must not fire during this walk: it would delete the shared
# Cluster mid-assertion. Production default is 600s; pinned long here.
REAP_DWELL_SECS=3600

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

# ---------------------------------------------------------------
# Precondition: the prune ships in 2.22b and is UNRELEASED, so the
# published operator image cannot delete anything. Without the local
# build this walk would assert against the very behaviour it is meant
# to prove was fixed, and pass by describing the defect.
# ---------------------------------------------------------------
if [ -z "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
    cat >&2 <<'EOF'
ERROR: needs-removal-walk requires APPRAFTER_E2E_LOCAL_OPERATOR=1.

The claim/Service prune ships in 2.22b and is not published yet. Running
against the released image would exercise the OLD behaviour — no delete,
no RetainedClaim — and the walk would be asserting the defect.

  APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/needs-removal-walk.sh
EOF
    exit 2
fi

# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"

# Set to 1 only after the k3d cluster is up AND $KUBECONFIG points at it
# (Phase 0). Until then, dump_diagnostics / k3d_down must NOT run — on a
# k3d-up failure (e.g. no docker in this shell) $KUBECONFIG still points
# at the operator's ambient cluster, and an e2e must never touch a
# non-test cluster.
K3D_CREATED=0

cleanup() {
    local exit_code=$?

    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! needs-pg-walk FAILED at %s (exit %d) !!!\n' \
            "$(elapsed)" "$exit_code" >&2
        if [ "$K3D_CREATED" -eq 1 ]; then
            dump_diagnostics
            printf 'Tearing down k3d cluster (set APPRAFTER_E2E_SKIP_DESTROY=1 to keep).\n' >&2
        else
            printf 'k3d cluster %s was never created (k3d/docker unavailable in this shell?) — skipping diagnostics + teardown; your ambient KUBECONFIG was NOT touched.\n' \
                "$CLUSTER_NAME" >&2
        fi
    fi

    if [ "$K3D_CREATED" -eq 1 ]; then
        if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
            k3d_down "$CLUSTER_NAME" || true
        else
            printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving k3d cluster %s up.\n' \
                "$CLUSTER_NAME"
            printf 'Run: k3d cluster delete %s\n' "$CLUSTER_NAME"
        fi
    fi

    rm -rf "$TMPDIR_WORK"
    exit "$exit_code"
}
trap cleanup EXIT

# ---------------------------------------------------------------
# Helper: seed the CLI state store (mirrors gitops-walk.sh).
#
# Writes:
#   $APPRAFTER_CONFIG_DIR/config.yaml      — active_target: k3d
#   $APPRAFTER_CONFIG_DIR/state/k3d/.apprafter/state.json
#     hetzner_cloud.kubeconfig_yaml = <k3d kubeconfig plaintext>
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
# Local helper: wait_gone <kind> <ns> <name> [timeout]
#   Poll until `kubectl get` 404s (the object is gone). Prints an
#   ERROR + returns 1 on timeout.
# ---------------------------------------------------------------
wait_gone() {
    local kind="$1" ns="$2" name="$3"
    local timeout="${4:-120}"
    local deadline
    deadline=$(( $(date +%s) + timeout ))

    printf '  wait %s/%s gone (timeout %ss) ...\n' "$kind" "$name" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if ! kubectl -n "$ns" get "$kind" "$name" >/dev/null 2>&1; then
            printf '  ok: %s/%s is gone\n' "$kind" "$name"
            return 0
        fi
        sleep 5
    done
    printf 'ERROR: %s/%s still present after %ss\n' "$kind" "$name" "$timeout" >&2
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

# Condition-status reader: the `.status` of a condition selected by
# `type`. kubectl jsonpath has no boolean filter for the operator's
# verbs, so we pull the conditions array as JSON and grep it via a
# small python-free jsonpath the kubectl client supports.
cond_status() {
    # $1 kind, $2 ns, $3 name, $4 condition-type
    kubectl -n "$2" get "$1" "$3" -o \
        jsonpath="{.status.conditions[?(@.type==\"$4\")].status}" \
        2>/dev/null || true
}

# ---------------------------------------------------------------
# Local helper: strip_ansi — drop SGR escape sequences from stdin.
#
# LOAD-BEARING for every log-based assertion below. `tracing_subscriber::
# fmt()` colourises by default (it does not check for a TTY), and it wraps
# each structured FIELD separately — so a line that reads
# `... loop starting dwell_secs=30` on screen is actually
# `dwell_secs<ESC>[0m<ESC>[2m=<ESC>[0m30` in the bytes, and a grep for the
# literal `dwell_secs=30` matches NOTHING. Found by running this walk: the
# operator had logged exactly the expected line and the assertion still
# failed.
# ---------------------------------------------------------------
strip_ansi() {
    sed $'s/\033\\[[0-9;]*[a-zA-Z]//g'
}

# ---------------------------------------------------------------
# Local helper: operator_pod — the name of the Running apprafter-operator
# pod (replicaCount is 1, so this is the leader).
#
# Prometheus counters are PER-POD and reset to zero on restart, so every
# counter comparison in the §9 phases re-reads this and FAILS if the pod
# changed underneath it — otherwise a mid-test operator restart would make
# "the counter did not increase" trivially true and turn the negative test
# into a false green.
# ---------------------------------------------------------------
operator_pod() {
    kubectl -n "$RETAINED_NS" get pods \
        -l app.kubernetes.io/name=apprafter-operator \
        --field-selector=status.phase=Running \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# ---------------------------------------------------------------
# Local helper: operator_metrics — the operator's /metrics text.
#
# Scraped through the apiserver's pod-proxy subresource rather than a
# port-forward or a curl sidecar: no background process to reap, no image
# to pull, and the walk's kubeconfig is system:masters so pods/proxy is
# already permitted.
# ---------------------------------------------------------------
operator_metrics() {
    local pod port
    pod="$(operator_pod)"
    [ -n "$pod" ] || return 1
    port=$(kubectl -n "$RETAINED_NS" get deploy apprafter-operator \
        -o jsonpath='{.spec.template.spec.containers[0].ports[0].containerPort}' \
        2>/dev/null || true)
    kubectl get --raw \
        "/api/v1/namespaces/${RETAINED_NS}/pods/${pod}:${port:-8080}/proxy/metrics" \
        2>/dev/null
}

# ---------------------------------------------------------------
# Local helper: metric_value <series> — the current value of an exact
# Prometheus series (name + rendered label set), or 0 when the series is
# absent (a CounterVec child that has never been incremented emits no
# line at all, so "absent" and "zero" are the same statement).
#
# A FAILED SCRAPE IS NOT ZERO. It returns non-zero so `set -e` aborts the
# walk at the call site: silently reading 0 would make every "did not
# increase" assertion in the §9 phases pass for the wrong reason.
# ---------------------------------------------------------------
metric_value() {
    local series="$1" body
    body="$(operator_metrics || true)"
    if [ -z "$body" ]; then
        # Empty covers BOTH a failed scrape and a genuinely empty exposition
        # (a freshly-restarted operator that has not touched a single metric
        # yet emits a zero-byte body — observed on this walk's own cluster).
        # Neither is a number to reason from: the second means the counters
        # this phase is comparing against have been reset, so treating it as
        # 0 would make a "did not increase" assertion pass on a restart.
        printf 'ERROR: the operator /metrics endpoint returned nothing (pod %q) — either the scrape failed or the operator restarted and has emitted no metric yet; refusing to read a counter delta from it\n' \
            "$(operator_pod)" >&2
        return 1
    fi
    printf '%s\n' "$body" | awk -v want="$series" \
        '$1 == want { printf "%d", $2 + 0; found = 1 }
         END { if (!found) printf "0" }'
}

# reap_metric <backend> <result> — apprafter_shared_backend_reap_total
# for one (backend, result) pair. The label ORDER is the CounterVec's
# declaration order in operator-core/src/metrics.rs (`&["backend",
# "result"]`), which is what the exposition renders.
reap_metric() {
    metric_value "apprafter_shared_backend_reap_total{backend=\"$1\",result=\"$2\"}"
}

# ===============================================================
# Phase 0: bring up the k3d cluster
# ===============================================================

phase "Phase 0: k3d_up ${CLUSTER_NAME}"

k3d_up "$CLUSTER_NAME"

cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
# The k3d cluster exists and $KUBECONFIG now points at it — cleanup may
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
# the branch CRDs. 2.12 (ADR 0046) drops the 2.4e auto-inject + the composed
# `DATABASE_URL` connection-Secret key and adds the `env` value node + webhook
# env-ref rules — all UNRELEASED, so the published image + CRD this cluster
# bootstrapped from cannot render/validate the explicit DSN ref in Phase 3.
# Mirrors needs-disk-walk Phase 1b (no extra RBAC — env refs add no new k8s
# verb; resolve_env only reads Secrets the operator already watches).
# NOTE: the if-body below is intentionally NOT indented; `fi` closes it just
# before Phase 2.
# ---------------------------------------------------------------
if [ -n "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
phase "Phase 1b: build + load local operator + webhook (APPRAFTER_E2E_LOCAL_OPERATOR)"
builder=podman; command -v podman >/dev/null 2>&1 || builder=docker
# `build_load_restart` now lives in e2e/lib.sh — ONE implementation, and it
# CACHES the built image by the content of operator/ + schemas/v1alpha1/.
# Thirteen walks carried a private copy that SHADOWED the shared one, so the
# cache benefited nobody: each still rebuilt the same image (3m04 measured).
build_load_restart apprafter-operator apprafter-operator
build_load_restart admission-webhook admission-webhook
# Wait until ONLY the branch webhook serves before any branch-typed apply: the
# OLD (released) webhook pod lingers Terminating for its grace period, and
# during that window the Phase 3 env-ref apply could route to it — whose
# released validator lacks the 2.12 env-ref rules.
printf '  waiting for the old (released) webhook pod to fully terminate ...\n'
_wh_deadline=$(( $(date +%s) + 90 ))
while [ "$(date +%s)" -lt "$_wh_deadline" ]; do
    [ "$(kubectl -n apprafter-system get pods \
        -l app.kubernetes.io/name=admission-webhook --no-headers 2>/dev/null \
        | wc -l)" -le 1 ] && break
    sleep 3
done
printf '  apprafter-operator + admission-webhook now running the working-tree build\n'

# The released chart predates the 2.12 CRD change (Application.spec.base.env /
# environments[*].env now accept a string-OR-object #EnvValue). Argo CD owns
# those CRDs via the apprafter-operator Application, so disable automated sync
# on the parent + operator apps (else Argo reverts the drift) then apply the
# BRANCH-rendered CRDs server-side. Mirrors needs-disk-walk Phase 1b.
# 2.22b HARNESS LESSON: the branch RBAC must ship with the branch IMAGE.
# This phase used to render only CustomResourceDefinitions, so the cluster kept
# the PUBLISHED ClusterRole while running the working-tree operator. 2.22b adds
# `delete` on resourceclaims, and without it the prune 403s on every reconcile —
# which is exactly what the third run of this walk showed. The operator survives
# it (the delete warns and retries rather than failing the reconcile, per the
# ADR 0048 anchor-403 lesson), so the symptom is a silent no-op rather than a
# crash, and only reading the log finds it.
printf '  applying branch operator CRDs + RBAC (released chart predates this branch) ...\n'
for _app in platform apprafter-operator; do
    kubectl -n argocd patch applications.argoproj.io "$_app" --type=merge \
        -p '{"spec":{"syncPolicy":{"automated":null}}}' >/dev/null 2>&1 || true
done
_yq() { if command -v yq >/dev/null 2>&1; then yq "$@"; else nix run nixpkgs#yq-go -- "$@"; fi; }
helm template apprafter-operator "${REPO_ROOT}/operator/charts/apprafter-operator" \
    | _yq 'select(.kind == "CustomResourceDefinition" or .kind == "ClusterRole")' \
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

# ADR 0042 §9 — pin a SHORT reaper dwell so the §9 phases assert a TERMINAL
# outcome (reaped / still there) instead of racing the 600s production
# default. This is the operator's own env seam (main.rs reads
# APPRAFTER_REAP_DWELL_SECS); it is set HERE, after the automated-sync
# disable above, because Argo CD would otherwise revert the Deployment env
# on its next sync.
printf '  pinning APPRAFTER_REAP_DWELL_SECS=%s on the operator (ADR 0042 §9) ...\n' \
    "$REAP_DWELL_SECS"
kubectl -n apprafter-system set env deploy/apprafter-operator \
    "APPRAFTER_REAP_DWELL_SECS=${REAP_DWELL_SECS}"
kubectl -n apprafter-system rollout status deploy/apprafter-operator --timeout=180s

# PROVE the pin took effect. The reaper logs its dwell ONCE, at loop start,
# after leader election — so this also proves the loop is actually running.
# Without this assertion a walk whose env never landed would run at the
# 600s default and every §9 phase would time out for a reason nothing in
# the log explains.
printf '  waiting for the reaper loop to report the pinned dwell ...\n'
_dwell_seen=""
_dwell_deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$_dwell_deadline" ]; do
    # Capture, THEN match — same SIGPIPE-under-pipefail hazard as the TTL probe
    # in §9. Survivable here only because the inner grep narrows the stream to
    # one line before `grep -q` sees it; that is luck, not design.
    _dwell_logs="$(kubectl -n apprafter-system logs deploy/apprafter-operator --tail=-1 2>/dev/null | strip_ansi || true)"
    if printf '%s\n' "$_dwell_logs" | grep 'SharedBackendReaper loop starting' \
        | grep -c "dwell_secs=${REAP_DWELL_SECS}" >/dev/null 2>&1; then
        _dwell_seen="ok"
        break
    fi
    sleep 5
done
if [ "$_dwell_seen" != "ok" ]; then
    printf 'ERROR: the operator never logged "SharedBackendReaper loop starting" with dwell_secs=%s — APPRAFTER_REAP_DWELL_SECS did not take effect, or the reaper never became leader\n' \
        "$REAP_DWELL_SECS" >&2
    kubectl -n apprafter-system logs deploy/apprafter-operator --tail=80 >&2 2>&1 || true
    exit 1
fi
printf '  ok: SharedBackendReaper running with dwell_secs=%s\n' "$REAP_DWELL_SECS"
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
# needs.pg Application (it validates the CR on CREATE).
printf '  waiting for the admission-webhook Deployment ...\n'
retry 30 10 -- kubectl -n "$RETAINED_NS" rollout status \
    deploy admission-webhook --timeout=60s


PLAN_RES="migrationplan.apprafter.io"

# app_scope_plan_name <app> — the app-scope MigrationPlan for <app>, if any.
app_scope_plan_name() {
    kubectl -n "$APP_NS" get "$PLAN_RES" \
        -l "apprafter.io/application=$1,apprafter.io/scope=application" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# wait_plan_appears <app> <timeout> — poll until an app-scope plan exists.
wait_plan_appears() {
    local app="$1" timeout="${2:-180}" deadline name
    deadline=$(( $(date +%s) + timeout ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        name="$(app_scope_plan_name "$app")"
        [ -n "$name" ] && { printf '%s' "$name"; return 0; }
        sleep 5
    done
    return 1
}

# apply_parser <needs-block> <expose-block> — re-apply the Application with a
# given needs/expose shape. Everything else is held constant so a diff between
# two calls is exactly the change under test.
apply_parser() {
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
${2}
${1}
YAML
}

NEEDS_BOTH='    needs:
      pg:
        - name: primary
          selector:
            tier: integrated
        - name: analytics
          selector:
            tier: integrated'
NEEDS_ONE='    needs:
      pg:
        - name: primary
          selector:
            tier: integrated'
EXPOSE_ON='    expose:
      port: 80'
EXPOSE_OFF=''

# ===============================================================
# Phase 3: an Application with TWO named pg claims
# ===============================================================

phase "Phase 3: apply Application with two named needs.pg entries"

kubectl create namespace "$APP_NS" 2>/dev/null || true

apply_parser "$NEEDS_BOTH" "$EXPOSE_ON"

# The negative control, applied now so it is provisioned and steady long
# before the removal under test. Its claim must survive everything below.
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP2}
  namespace: ${APP_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    needs:
      pg:
        selector:
          tier: integrated
YAML

# ===============================================================
# Phase 4: both claims provision, the app goes Ready
# ===============================================================

phase "Phase 4: both claims provisioned, Application Ready, Service present"

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM_KEEP" '{.status.ready}' "true" 600
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM_DROP" '{.status.ready}' "true" 600
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM2"     '{.status.ready}' "true" 600

# Record the sibling's uid NOW. Asserting it is merely "still present" after
# the removal would pass on a delete-and-recreate, which is the failure this
# walk most needs to exclude: a diff that prunes the wrong member and lets
# the controller regenerate it looks identical by name.
KEEP_UID="$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM_KEEP" '{.metadata.uid}')"
CTRL_UID="$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM2" '{.metadata.uid}')"
[ -n "$KEEP_UID" ] || { printf 'FAILED: no uid on %s\n' "$CLAIM_KEEP" >&2; exit 1; }
printf '  ok: sibling uid recorded (%s)\n' "$KEEP_UID"

wait_jsonpath "$APP_RES" "$APP_NS" "$APP" '{.status.phase}' "Ready" 300
retry 30 5 -- kubectl -n "$APP_NS" get secret "$SECRET_KEEP" >/dev/null
retry 30 5 -- kubectl -n "$APP_NS" get secret "$SECRET_DROP" >/dev/null
retry 30 5 -- kubectl -n "$APP_NS" get service "$APP" >/dev/null
printf '  ok: two claims ready, both connection Secrets present, Service present\n'

# ===============================================================
# Phase 5: remove ONE need — the change is GATED, nothing destroyed
# ===============================================================

phase "Phase 5: drop needs.pg[analytics] — MigrationPlan gates it"

apply_parser "$NEEDS_ONE" "$EXPOSE_ON"

PLAN="$(wait_plan_appears "$APP" 240)" || {
    printf 'FAILED: no MigrationPlan for the needs removal after 4 min\n' >&2
    exit 1
}
printf '  plan: %s\n' "$PLAN"

plan_class=$(jp "$PLAN_RES" "$APP_NS" "$PLAN" '{.spec.risks.classification}')
assert_eq "removal classified" "$plan_class" "data-migration"

# THE GATE ITSELF. Before approval nothing may be destroyed — this is the
# assertion that keeps the prune from becoming a way to lose data on an
# unapproved edit. The claim, its Secret and its role must all still be here.
sleep 30
retry 3 5 -- kubectl -n "$APP_NS" get "$CLAIM_RES" "$CLAIM_DROP" >/dev/null
retry 3 5 -- kubectl -n "$APP_NS" get secret "$SECRET_DROP" >/dev/null
dropped_dts=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM_DROP" '{.metadata.deletionTimestamp}')
assert_eq "claim NOT deleting while the plan is pending" "$dropped_dts" ""
printf '  ok: 30s past the plan and the undeclared claim is untouched\n'

# ===============================================================
# Phase 6: approve — the claim is released, the sibling is not
# ===============================================================

phase "Phase 6: approve — removed claim deleted + RetainedClaim, sibling untouched"

apprafter migration approve "$PLAN"

# Assert the GATE opens before asserting what the gate releases. The app-scope
# machine proceeds only on `CompletedMatch` (operator-core/migration_state.rs):
# a plan that is approved but never executed reads as `BlockingMatch`, so the
# reconcile keeps gating, the removed claim is never pruned, and the symptom is
# an absent RetainedClaim 300s later — which blames the retention path for a
# stall in the migration machine. Name the stall where it happens.
#
# "completed OR gone", never "== completed": the whole machine ran in 5ms on
# the run that produced this assertion, and the GC then deleted the terminal
# plan, so a poll for the phase saw a 404 and read it as `last=''`. That is
# the 2.6b lesson again — a fast backend cannot be caught in a transient
# state, so assert the OUTCOME. A stall still fails here, and still names the
# phase it stalled in.
_plan_deadline=$(( $(date +%s) + 180 ))
_plan_phase=""
while [ "$(date +%s)" -lt "$_plan_deadline" ]; do
    if ! kubectl -n "$APP_NS" get migrationplan "$PLAN" >/dev/null 2>&1; then
        _plan_phase="gone"; break
    fi
    _plan_phase=$(jp migrationplan "$APP_NS" "$PLAN" '{.status.phase}')
    [ "$_plan_phase" = "completed" ] && break
    sleep 5
done
case "$_plan_phase" in
    completed|gone) printf '  ok: gate opened (plan %s)\n' "$_plan_phase" ;;
    *) printf 'FAILED: plan %s stalled in phase %q — the gate never opened, so nothing below can pass\n' \
        "$PLAN" "$_plan_phase" >&2; exit 1 ;;
esac

# The delete is what starts the documented retention path. Until 2.22b it
# never ran for a removed need: nothing deleted, so no deletionTimestamp, so
# the finalizer never fired and no snapshot was ever written.
wait_jsonpath retainedclaim "$RETAINED_NS" "$RETAINED_DROP" \
    '{.spec.claimRef.name}' "$CLAIM_DROP" 300
rc_retain=$(jp retainedclaim "$RETAINED_NS" "$RETAINED_DROP" '{.spec.retainUntil}')
case "$rc_retain" in
    [0-9][0-9][0-9][0-9]-*) printf '  ok: RetainedClaim retainUntil = %s\n' "$rc_retain" ;;
    *) printf 'FAILED: retainUntil not RFC3339: %q\n' "$rc_retain" >&2; exit 1 ;;
esac

wait_gone "$CLAIM_RES" "$APP_NS" "$CLAIM_DROP" 300
wait_gone secret "$APP_NS" "$SECRET_DROP" 300
printf '  ok: removed claim and its connection Secret are gone\n'

# The sibling, by uid. Present-by-name would pass a delete-and-recreate.
keep_uid_after="$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM_KEEP" '{.metadata.uid}')"
assert_eq "sibling claim uid unchanged" "$keep_uid_after" "$KEEP_UID"
keep_ready=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM_KEEP" '{.status.ready}')
assert_eq "sibling claim still ready" "$keep_ready" "true"
retry 3 5 -- kubectl -n "$APP_NS" get secret "$SECRET_KEEP" >/dev/null
printf '  ok: sibling claim untouched (same uid, ready, Secret intact)\n'

# NEGATIVE CONTROL: the other Application's claim. `claims_to_prune` filters
# on the controller ownerRef uid; this proves that live rather than in a unit
# test, where a filter bug and a passing test can coexist.
ctrl_uid_after="$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM2" '{.metadata.uid}')"
assert_eq "other app's claim uid unchanged" "$ctrl_uid_after" "$CTRL_UID"
printf '  ok: a different Application'"'"'s claim was never a candidate\n'

wait_jsonpath "$APP_RES" "$APP_NS" "$APP" '{.status.phase}' "Ready" 300
printf '  ok: Application back to Ready on one need\n'

# ===============================================================
# Phase 7: D12 — dropping `expose` prunes the Service
# ===============================================================

phase "Phase 7: drop expose — Service pruned, Deployment survives"

# Assert the Service is STILL THERE before removing what creates it. Without
# this the `wait_gone` below passes vacuously if anything pruned it earlier —
# Phase 4's presence check is six minutes and two reconcile storms away, and
# an assertion satisfied by the null case is the exact failure this whole
# subphase was opened to stop repeating.
retry 3 5 -- kubectl -n "$APP_NS" get service "$APP" >/dev/null
printf '  ok: Service still present immediately before expose is removed\n'

apply_parser "$NEEDS_ONE" "$EXPOSE_OFF"

wait_gone service "$APP_NS" "$APP" 300
printf '  ok: Service pruned after expose was removed\n'

# The workload must NOT have gone with it. A prune that took the Deployment
# too would satisfy the assertion above and be catastrophic.
retry 10 5 -- kubectl -n "$APP_NS" get deployment "$APP" >/dev/null
printf '  ok: Deployment survives the Service prune\n'

# ===============================================================
# Phase 8: idempotence — a second reconcile changes nothing
# ===============================================================

phase "Phase 8: steady state — re-reconcile prunes nothing further"

# A prune that is not idempotent would eventually take the sibling too. Give
# the controller several reconciles (60s requeue) and re-assert.
sleep 90
keep_uid_final="$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM_KEEP" '{.metadata.uid}')"
assert_eq "sibling survives repeated reconciles" "$keep_uid_final" "$KEEP_UID"
ctrl_uid_final="$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM2" '{.metadata.uid}')"
assert_eq "control app claim survives repeated reconciles" "$ctrl_uid_final" "$CTRL_UID"
retry 3 5 -- kubectl -n "$APP_NS" get deployment "$APP" >/dev/null
printf '  ok: steady after ~90s of reconciles\n'

# ===============================================================
# Phase 9 (2.22h / D16): a reconcile failure is visible WITHOUT a log read
# ===============================================================
# The fixture reproduces the exact incident that opened D16: during the 2.22b
# walk the operator 403'd on every claim delete, every thirty seconds, and the
# only trace was `kubectl logs`. `app status` said Ready and the claim simply
# never went away — three runs and a log read to find it.
#
# It is reproduced by WITHHOLDING ONE VERB. `delete` specifically: the list at
# the top of `prune_orphaned_claims` propagates normally, so withholding the
# whole rule would produce a DIFFERENT failure (one that reaches error_policy)
# and the walk would pass without exercising the swallow path this subphase is
# about.

phase "Phase 9: an undesigned failure surfaces in app status (2.22h / D16)"

# Pin a short retention so the ageing assertion does not need an hour. The
# operator LOGS the value at startup, so "aged out" and "was never written"
# stay distinguishable.
# Two knobs, both for the same reason: the shipped values (3600s TTL, 900s
# refresh floor) make the ageing and accumulation assertions below unrunnable
# inside a walk, and an assertion that cannot run is one that cannot fail.
printf '  pinning APPRAFTER_PROBLEM_TTL_SECS=120 + APPRAFTER_PROBLEM_REFRESH_FLOOR_SECS=30 and confirming it took ...\n'
_ttl_pod_before="$(operator_pod)"
kubectl -n apprafter-system set env deploy/apprafter-operator \
    APPRAFTER_PROBLEM_TTL_SECS=120 APPRAFTER_PROBLEM_REFRESH_FLOOR_SECS=30
kubectl -n apprafter-system rollout status deploy/apprafter-operator --timeout=180s

# Read the NEW pod by name, never `logs deploy/...`. A Deployment reference
# resolves through the selector, which during a roll still matches the old
# Running-but-terminating pod — and the old pod, by construction, never logged
# the pinned value. Two runs failed here against a pod that had already logged
# it correctly; the deployment-level read was the whole defect.
_ttl_pod=""
_pod_deadline=$(( $(date +%s) + 120 ))
while [ "$(date +%s)" -lt "$_pod_deadline" ]; do
    # Newest Running pod, sorted in the shell: `--sort-by` is not reliably
    # honoured alongside `-o jsonpath` across kubectl versions.
    _ttl_pod="$(kubectl -n apprafter-system get pods \
        -l app.kubernetes.io/name=apprafter-operator \
        --field-selector=status.phase=Running \
        -o jsonpath='{range .items[*]}{.metadata.creationTimestamp} {.metadata.name}{"\n"}{end}' \
        2>/dev/null | sort | tail -1 | awk '{print $2}')"
    [ -n "$_ttl_pod" ] && [ "$_ttl_pod" != "$_ttl_pod_before" ] && break
    sleep 5
done
[ -n "$_ttl_pod" ] && [ "$_ttl_pod" != "$_ttl_pod_before" ] || {
    printf 'ERROR: no new operator pod after the env pin (before=%q now=%q)\n' \
        "$_ttl_pod_before" "$_ttl_pod" >&2; exit 1; }

# 300s: the new pod waits out the previous holder's 30s Lease before it
# reaches this line, on top of the roll itself.
_ttl_seen=""
_ttl_deadline=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$_ttl_deadline" ]; do
    # Capture, THEN match. `kubectl logs | grep -q` cannot work under
    # `set -o pipefail` on a large stream: `grep -q` exits on the first match
    # and closes the pipe, `kubectl` dies of SIGPIPE, and pipefail reports the
    # pipeline as failed — so a MATCH reads as a miss. Three runs timed out
    # here against a pod that had logged the value correctly all along; the
    # failure diagnostic printing `what it logged: problem_ttl_secs=120` is
    # what finally named it.
    _ttl_logs="$(kubectl -n apprafter-system logs "$_ttl_pod" --tail=-1 2>/dev/null | strip_ansi || true)"
    # Two independent substring checks, not one ordered pattern: the field
    # order in tracing's output is not a contract worth depending on.
    case "$_ttl_logs" in *problem_ttl_secs=120*)
        case "$_ttl_logs" in *problem_refresh_floor_secs=30*) _ttl_seen=ok; break ;; esac ;;
    esac
    sleep 5
done
[ "$_ttl_seen" = ok ] || {
    printf 'ERROR: pod %s never logged problem_ttl_secs=120 — the pin did not land, so an absent problem below would be unattributable\n' "$_ttl_pod" >&2
    # Say WHAT it logged instead. "Pinned to another value" and "never got the
    # env at all" need different fixes, and a bare timeout distinguishes neither.
    printf '  what it logged: %s\n' \
        "$(kubectl -n apprafter-system logs "$_ttl_pod" --tail=-1 2>/dev/null | strip_ansi \
            | grep -o 'problem_ttl_secs=[0-9]*' | tail -1)" >&2
    printf '  env on the pod: %s\n' \
        "$(kubectl -n apprafter-system get pod "$_ttl_pod" \
            -o jsonpath='{range .spec.containers[*].env[?(@.name=="APPRAFTER_PROBLEM_TTL_SECS")]}{.name}={.value}{end}' 2>/dev/null)" >&2
    exit 1; }
printf '  ok: operator pod %s running with problem_ttl_secs=120\n' "$_ttl_pod"

# Withhold `delete` on resourceclaims, exactly as the 2.22b harness gap did by
# accident. Argo owns this ClusterRole, and automated sync was disabled in
# Phase 1b, so the edit holds.
#
# The verb is toggled by re-reading the LIVE role and rewriting its rules, in
# both directions — never by re-applying a snapshot taken before the change.
# A snapshot carries the `resourceVersion` it was read at, and `kubectl apply`
# turns that into an optimistic-concurrency precondition, so the restore fails
# with a Conflict the moment anything else touches the object. That is exactly
# how run 8 died, three assertions after the part under test had passed.
claims_delete_verb() {   # $1 = grant | revoke
    kubectl get clusterrole apprafter-operator -o json | python3 -c "
import json,sys
mode=sys.argv[1]
r=json.load(sys.stdin)
for rule in r['rules']:
    if 'apprafter.io' in rule.get('apiGroups',[]) and 'resourceclaims' in rule.get('resources',[]):
        verbs=[v for v in rule['verbs'] if v!='delete']
        if mode=='grant':
            verbs.append('delete')
        rule['verbs']=verbs
# Volatile server-owned metadata: harmless to send, but there is no reason to.
for k in ('resourceVersion','uid','creationTimestamp','managedFields'):
    r['metadata'].pop(k, None)
json.dump(r,sys.stdout)" "$1" | kubectl apply -f - >/dev/null
}

printf '  withholding the resourceclaims `delete` verb ...\n'
claims_delete_verb revoke
retry 12 5 -- sh -c '[ "$(kubectl auth can-i delete resourceclaims.apprafter.io \
    --as=system:serviceaccount:apprafter-system:apprafter-operator -n demo)" = "no" ]'
printf '  ok: the operator can no longer delete resourceclaims\n'

# Drop the remaining need, approve its plan, and let the prune fail.
kubectl apply -f - >/dev/null <<EOF
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: { name: $APP, namespace: $APP_NS }
spec:
  base:
    image: nginxdemos/hello:plain-text
EOF
_plan=""; _pd=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$_pd" ]; do
    _plan=$(kubectl -n "$APP_NS" get migrationplan.apprafter.io -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
    [ -n "$_plan" ] && break
    sleep 10
done
if [ -n "$_plan" ]; then
    apprafter migration approve "$_plan" --namespace "$APP_NS" >/dev/null 2>&1 || true
fi

printf '  waiting for the failure to reach app status (one requeue) ...\n'
_seen=""; _sd=$(( $(date +%s) + 240 ))
while [ "$(date +%s)" -lt "$_sd" ]; do
    _reason=$(jp "$APP_RES" "$APP_NS" "$APP" '{.status.recentProblems[0].reason}')
    [ "$_reason" = "ClaimPruneFailed" ] && { _seen=ok; break; }
    sleep 10
done
if [ "$_seen" != ok ]; then
    printf 'ERROR: the prune 403 never reached status.recentProblems — this is exactly the invisibility D16 is about\n' >&2
    kubectl -n "$APP_NS" get "$APP_RES" "$APP" -o jsonpath='{.status}' >&2; echo >&2
    claims_delete_verb grant >/dev/null 2>&1 || true
    exit 1
fi

# NOT just "a problems section exists" — that would pass for any failure, and
# for none. The message must name what actually went wrong.
_msg=$(jp "$APP_RES" "$APP_NS" "$APP" '{.status.recentProblems[0].message}')
case "$_msg" in
    *forbidden*resourceclaims*|*resourceclaims*forbidden*)
        printf '  ok: the recorded message names the forbidden resourceclaims delete\n' ;;
    *) printf 'ERROR: the message does not name the failure: %q\n' "$_msg" >&2
       claims_delete_verb grant >/dev/null 2>&1 || true
       exit 1 ;;
esac

# THE CLI hop is NOT covered here, and this says so out loud rather than
# letting a green run imply it was. `apprafter app status` resolves an app
# through its Argo CD registration — `find_apprafter_app_name` reads the inner
# CR name out of the Argo Application's `status.resources[]`, which only a real
# sync populates. This walk builds bare Application CRs with kubectl, so there
# is no Argo app to resolve and the command correctly reports the app as
# unregistered. Covering it needs the gitops walk's git-daemon + `app add`
# fixture (~60 lines, host-networking-sensitive) inside a walk whose fixture is
# a cluster-wide RBAC withdrawal. Fabricating `status.resources` to satisfy the
# assertion was rejected: Argo owns that field and overwrites it, and an
# assertion propped up by a forged fixture is the null-case pass this file
# already warns about twice.
#
# What IS covered: the operator half, above and below — the entry reaches the
# CR, names the real failure, deduplicates, and ages out. The rendering half is
# unit-covered (`format_problem_lines`, 6 cases incl. the 24h horizon and the
# empty list).
printf '  NOT COVERED HERE: `apprafter app status` rendering of these entries — needs an Argo-registered app (owed; see the 2.22h closure note)\n'

# THE CLUSTER ROLL-UP, which — unlike `app status` — IS reachable from this
# fixture. `app status` cannot render an application at all without an Argo CD
# registration; the roll-up takes its PROBLEM data from a cluster-wide (`-A`)
# read of the AppRafter CRs and uses Argo CD only to put a friendlier NAME on
# each row, degrading to the CR's own name when that lookup is unavailable.
# `--cached` skips the 30s upstream re-check; the roll-up does not depend on it.
#
# This app is a bare CR, so it exercises the UNRESOLVABLE arm on purpose: the
# roll-up cannot map it to a logical name, and must therefore list it as
# `<ns>/<name>` and say so, rather than drop it. Dropping unregistered
# applications would make the surface quietest exactly where it should be
# loudest.
printf '  checking the cluster roll-up names the broken application ...\n'
_ps_out=$(apprafter platform status --cached 2>&1 || true)
case "$_ps_out" in
    *"reporting problems"*"$APP_NS/$APP"*)
        printf '  ok: platform status names %s/%s in the problem roll-up\n' "$APP_NS" "$APP" ;;
    *) printf 'ERROR: platform status did not name the broken application:\n%s\n' "$_ps_out" >&2
       claims_delete_verb grant >/dev/null 2>&1 || true
       exit 1 ;;
esac

# DEDUP. A 30s retry over three minutes must produce ONE entry with a rising
# count — not one entry per tick, which would be the firehose this replaces.
printf '  sampling for 180s to prove it deduplicates rather than spams ...\n'
_first_seen_a=$(jp "$APP_RES" "$APP_NS" "$APP" '{.status.recentProblems[0].firstSeen}')
sleep 180
_len=$(kubectl -n "$APP_NS" get "$APP_RES" "$APP" -o jsonpath='{.status.recentProblems[*].reason}' | wc -w | tr -d ' ')
_first_seen_b=$(jp "$APP_RES" "$APP_NS" "$APP" '{.status.recentProblems[0].firstSeen}')
_count=$(jp "$APP_RES" "$APP_NS" "$APP" '{.status.recentProblems[0].count}')
assert_eq "one entry, not one per retry" "$_len" "1"
assert_eq "firstSeen is stable across retries" "$_first_seen_b" "$_first_seen_a"
# `a && b || c` cannot fail a script under `set -e` — it always exits 0. The
# original form printed ERROR and carried on to a GREEN banner, which is the
# silent pass this suite forbids.
if [ "${_count:-0}" -ge 2 ]; then
    printf '  ok: count rose to %s (the retries accumulate)\n' "$_count"
else
    printf 'ERROR: count did not rise (%s) — the retries are not being recorded\n' "${_count:-unset}" >&2
    claims_delete_verb grant >/dev/null 2>&1 || true
    exit 1
fi

# AGEING. Restore the verb; the prune succeeds, the problem stops recurring,
# and the entry must disappear on its own within the pinned 120s TTL.
printf '  restoring the verb; the entry must age out on its own ...\n'
claims_delete_verb grant
retry 12 5 -- sh -c '[ "$(kubectl auth can-i delete resourceclaims.apprafter.io \
    --as=system:serviceaccount:apprafter-system:apprafter-operator -n demo)" = "yes" ]'
_gone=""; _gd=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$_gd" ]; do
    _n=$(kubectl -n "$APP_NS" get "$APP_RES" "$APP" -o jsonpath='{.status.recentProblems[*].reason}' | wc -w | tr -d ' ')
    [ "${_n:-0}" -eq 0 ] && { _gone=ok; break; }
    sleep 10
done
[ "$_gone" = ok ] \
    && printf '  ok: the entry aged out once the failure stopped — not a permanent scar\n' \
    || { printf 'ERROR: the problem never aged out; a surface that lists what was once broken is one people learn not to read\n' >&2
         kubectl -n "$APP_NS" get "$APP_RES" "$APP" -o jsonpath='{.status.recentProblems}' >&2; echo >&2; exit 1; }

# THE NEGATIVE CONTROL for the roll-up. Without it the positive assertion above
# would pass for a section that simply prints the application unconditionally.
# A healthy cluster must ALSO say so out loud — silence would be
# indistinguishable from "the check did not run", which is the whole reason
# this section prints when there is nothing to report.
printf '  checking the roll-up clears and states the healthy case ...\n'
_ps_clean=$(apprafter platform status --cached 2>&1 || true)
case "$_ps_clean" in
    *"$APP_NS/$APP"*)
        printf 'ERROR: platform status still names %s/%s after the problem aged out:\n%s\n' \
            "$APP_NS" "$APP" "$_ps_clean" >&2; exit 1 ;;
esac
case "$_ps_clean" in
    *"none reporting problems"*)
        printf '  ok: platform status states the healthy case explicitly\n' ;;
    *) printf 'ERROR: platform status did not state the healthy case — silence reads as "the check did not run":\n%s\n' \
        "$_ps_clean" >&2; exit 1 ;;
esac

printf '\n'
printf '===============================================================\n'
printf '  GREEN — needs-removal walk passed (2.22b / D4 + D12, 2.22h / D16)\n'
printf '===============================================================\n'
