#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter needs.pg-walk e2e — the full Phase-2.4 ResourceClaim chain
# on a local k3d cluster (plan item 2.4g).
#
# This script exercises the shipped needs.pg pipeline end-to-end:
#
#   generate (2.4d) -> schedule (2.3) -> provision (2.4c) ->
#   resume + DSN inject (2.4d/2.4e) -> delete + snapshot (2.4f) ->
#   force-GC (2.4f) -> psql DROP proof ->
#   shared-backend reap round-trip (ADR 0042 §9)
#
# Concretely, on a k3d cluster bootstrapped with the platform stack
# (operator + admission-webhook + the always-on CNPG operator + the
# seeded `pg-integrated` ServiceProvider):
#
#   1. Apply an AppRafter Application with `spec.base.needs.pg`.
#   2. ASSERT the operator generates a ResourceClaim and HOLDS the
#      Application (status.phase=AwaitingResourceClaim).
#   3. ASSERT the scheduler matches it to `pg-integrated`
#      (status.provider, Scheduled=True).
#   4. ASSERT the provisioner lazily creates the shared CNPG Cluster,
#      a managed role, a Database, a password Secret, and a connection
#      Secret carrying the DECOMPOSED keys (2.12 / ADR 0046 #3: url user
#      pass host port db — the old composed DATABASE_URL key is dropped) —
#      and that the scheduler's Scheduled=True survives the provisioner's
#      status write (the SSA field-manager split guard).
#   5. ASSERT the Application resumes to Ready and the rendered Deployment
#      resolves the EXPLICIT `env: {DATABASE_URL: {claim: "pg.url"}}` ref
#      (2.12 / ADR 0046) into a secretKeyRef on the connection Secret's
#      `url` key (the 2.4e auto-inject is removed).
#   6. Delete the ResourceClaim; ASSERT the finalizer snapshots a
#      RetainedClaim (7-day grace) and the connection Secret cascades,
#      while the role + Database survive the grace floor.
#   7. Force GC by deleting + re-creating the RetainedClaim with a
#      past retainUntil; ASSERT the role + Database + password Secret +
#      RetainedClaim are all dropped.
#   8. psql DROP proof: exec the CNPG primary and ASSERT the database
#      and role are PHYSICALLY gone from Postgres (not merely the CR
#      reporting ensure:absent).
#   9. Shared-backend reaper round-trip (ADR 0042 §9) — THE ENFORCEMENT
#      §9.2 claims. A marker row is written into the shared cluster; the
#      last tenant is removed; the RetainedClaim is deleted to simulate
#      post-grace; and the walk ASSERTS the Cluster is reaped, no pods
#      remain (the memory genuinely comes back), and **the PVC is still
#      Bound**. Re-provisioning then ASSERTS the SAME PV is adopted and
#      the marker row still reads back. This is what turns §9.2's
#      "re-measure on a CNPG chart bump" from prose into a gate: a CNPG
#      version that re-adds the instance PVC's `controller: true`
#      ownerReference after the §9.3 strip makes the reap cascade, and
#      that fails HERE rather than in a user's cluster.
#  10. Negative test — with a tenant live, three dwells pass and the
#      Cluster's uid is unchanged while the `reaped` counter does not
#      move (and `veto_live` does, proving the reaper was running and
#      deciding rather than merely absent). An over-eager reaper is
#      worse than an absent one.
#  11. Sweep health — the reaper's own error bucket stays 0 across the
#      whole run, and the Dragonfly arm (no instances on this cluster)
#      is silent rather than erroring or reaping.
#
# A SECOND needs.pg Application (`ledger`) is applied alongside `parser`
# for §9's sake: it holds the ALLOCATED veto on the shared Cluster across
# Phases 8-10, so the short reaper dwell this walk pins cannot reap the
# cluster out from under the force-GC / psql-DROP assertions.
#
# CLI state injection
# -------------------
# `apprafter cluster-bootstrap` reads the kubeconfig from the CLI's
# per-target state store (not from $KUBECONFIG). We set
# APPRAFTER_CONFIG_DIR to a tmpdir and seed it with a minimal
# config.yaml (active_target: k3d) and a state.json carrying the k3d
# kubeconfig as kubeconfig_yaml (plaintext) — the same approach
# gitops-walk.sh uses.
#
# Required: docker (or podman aliased to docker), cargo, kubectl
#   — all satisfied inside `nix develop` or on a standard CI runner.
#
# NOTE: the GC step deletes + RE-CREATES the RetainedClaim with a past
# retainUntil rather than patching it in place — the RetainedClaim is
# immutable (CEL `self == oldSelf`), so an in-place patch is rejected;
# the e2e/walk admin kubeconfig is `system:masters`, which the
# operator-only webhook permits to CREATE.
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
# operator/operator-controllers/resourceclaim-provisioner/src/cnpg.rs
# (`pg_identifier` / `k8s_name`) for the derivation.
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-pg-walk"

APP_NS="demo"                       # tenant namespace
APP="parser"                        # Application name
CLAIM="parser-pg"                   # generated ResourceClaim name
CONN_SECRET="parser-pg-conn"        # connection Secret (app ns)

# Group-qualify the two collision-prone kinds so kubectl never resolves
# to the wrong API group: bare `application` also matches Argo CD's
# argoproj.io Application, and bare `resourceclaim` matches the k8s 1.32+
# DRA resource.k8s.io ResourceClaim. Always address the apprafter.io CRs.
APP_RES="application.apprafter.io"
CLAIM_RES="resourceclaim.apprafter.io"

PG_ROLE="claim_demo_parser_pg"      # pg_identifier(demo, parser-pg)
DB_OBJECT="claim-demo-parser-pg"    # k8s_name(demo, parser-pg)
PW_SECRET="claim-demo-parser-pg-pw" # {DB_OBJECT}-pw
RETAINED="claim-demo-parser-pg"     # RetainedClaim name = k8s_name(...)

# A SECOND needs.pg Application, for the ADR 0042 §9 reaper phases. It
# shares the same lazily-created Cluster, so from the moment its claim is
# allocated it holds the §9.1 ALLOCATED veto — which is what stops the
# short dwell pinned below from reaping the shared Cluster during the
# force-GC (Phase 9) and psql-DROP (Phase 10) assertions, both of which
# need a live Postgres. The §9 phases then delete it to make the cluster
# genuinely tenantless.
APP2="ledger"
CLAIM2="ledger-pg"
PG_ROLE2="claim_demo_ledger_pg"     # pg_identifier(demo, ledger-pg)
RETAINED2="claim-demo-ledger-pg"    # RetainedClaim name = k8s_name(...)

CNPG_NS="cnpg-system"               # shared CNPG Cluster namespace
CNPG_CLUSTER="platform-postgres"    # shared CNPG Cluster name
RETAINED_NS="apprafter-system"      # RetainedClaim namespace
PROVIDER="pg-integrated"            # seeded ServiceProvider

# ADR 0042 §9 — the shared-backend reaper's dwell, pinned SHORT on the
# operator Deployment (Phase 1b) via APPRAFTER_REAP_DWELL_SECS. Production
# runs unset, at the 600s default; a walk that raced a ten-minute timer
# could only ever assert a transient, so it pins the timer instead and
# asserts terminal outcomes.
REAP_DWELL_SECS=30

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
# Precondition: this walk REQUIRES local-operator mode while 2.12 is
# UNRELEASED. ADR 0046 removed the 2.4e implicit DATABASE_URL injection and
# the connection-Secret's composed `DATABASE_URL` key — the app now binds the
# DSN by an EXPLICIT `env: {DATABASE_URL: {claim: "pg.url"}}` ref (Phase 3),
# which only the branch operator + webhook + CRD render/validate. So this
# walk builds + side-loads the working-tree operator/webhook and applies the
# branch CRDs (Phase 1b). The flag is mandatory until 2.12 ships.
# ---------------------------------------------------------------
if [ -z "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
    cat >&2 <<'EOF'
ERROR: needs-pg-walk requires APPRAFTER_E2E_LOCAL_OPERATOR=1 while 2.12 is unreleased.

ADR 0046 (Phase 2.12) removed the 2.4e implicit DATABASE_URL injection and the
connection-Secret's composed `DATABASE_URL` key. This walk now declares the DSN
explicitly (`env: {DATABASE_URL: {claim: "pg.url"}}`), which only the branch
operator + webhook + CRD render/validate. Run:

  APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/needs-pg-walk.sh

so the walk builds + side-loads the working-tree operator/webhook and applies
the branch CRDs (Phase 1b). Drop this gate once 2.12 publishes.
EOF
    exit 2
fi

# ---------------------------------------------------------------
# Temp workspace
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
    if kubectl -n apprafter-system logs deploy/apprafter-operator --tail=-1 2>/dev/null \
        | strip_ansi \
        | grep 'SharedBackendReaper loop starting' \
        | grep -q "dwell_secs=${REAP_DWELL_SECS}"; then
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

# ===============================================================
# Phase 3: apply the needs.pg Application
# ===============================================================

phase "Phase 3: apply Application with spec.base.needs.pg + explicit DSN ref"

kubectl create namespace "$APP_NS" 2>/dev/null || true

# 2.12 (ADR 0046 #5): the 2.4e implicit DATABASE_URL injection is removed —
# the app binds the DSN by an EXPLICIT claim ref. The cue-cmp renders the bare
# selector `claim.pg.url` → the `{claim: "pg.url"}` marker; we apply that
# resolved marker form directly (this walk runs raw CRs, not the cue-cmp).
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
      DATABASE_URL:
        claim: "pg.url"
YAML

# A SECOND needs.pg Application sharing the same lazily-created Cluster. It
# exists for ADR 0042 §9: its claim holds the §9.1 ALLOCATED veto, so the
# short reaper dwell pinned in Phase 1b cannot reap the shared Cluster while
# Phases 8-10 still need a live Postgres to assert against. The §9 phases
# delete it to make the cluster genuinely tenantless. No env refs — this app
# never reads its DSN, it only has to exist.
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
    expose:
      port: 80
    needs:
      pg:
        selector:
          tier: integrated
        size: small
YAML

# ===============================================================
# Phase 4: generate (2.4d) — the operator creates the ResourceClaim
#          and HOLDS the Application
# ===============================================================

phase "Phase 4: generate — ResourceClaim created, Application gated"

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.spec.type}' pg 180

claim_tier=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.spec.selector.tier}')
assert_eq "ResourceClaim selector.tier" "$claim_tier" "integrated"
claim_size=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.spec.size}')
assert_eq "ResourceClaim size" "$claim_size" "small"
claim_owner_kind=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" \
    '{.metadata.ownerReferences[0].kind}')
assert_eq "ResourceClaim ownerRef Kind" "$claim_owner_kind" "Application"

# NOTE — the gate (status.phase=AwaitingResourceClaim, ResourceClaimPending=
# True until the claim is ready) is deliberately NOT polled here. The shared
# CNPG cluster is pre-warmed by bootstrap, so pg role/DB creation is near-
# instant and an Application transitions AwaitingResourceClaim -> Ready in
# well under a poll interval — making the transient gate phase a coin-flip to
# observe (a timing artifact of a fast cluster, not a product behaviour to
# assert). The gate is instead proven DETERMINISTICALLY by (a) claim
# GENERATION above — the operator emitted a ResourceClaim rather than
# rendering the Deployment immediately, the gate's load-bearing action — and
# (b) the Ready + DATABASE_URL injection (Phase 7) assertions: the workload
# only receives its DSN once the claim is ready. The pause-while-unready logic
# itself is unit-tested in the operator. (Mirrors the needs-disk-walk note;
# this poll flaked the e2e-pg nightly when CNPG was warm.)

# ===============================================================
# Phase 5: schedule (2.3) — the scheduler matches `pg-integrated`
# ===============================================================

phase "Phase 5: schedule — provider matched, Scheduled=True"

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.provider}' \
    "$PROVIDER" 120
sched_cond=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM" Scheduled)
assert_eq "ResourceClaim Scheduled condition" "$sched_cond" "True"

# ===============================================================
# Phase 6: provision (2.4c) — lazy CNPG Cluster + role + DB + Secrets
# ===============================================================

phase "Phase 6: provision — lazy CNPG Cluster, role, Database, Secrets"

# Lazy Cluster boot is the slow step (CNPG instance comes up).
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.ready}' true 300

# The `ledger` claim provisions onto the SAME shared Cluster. Waiting for it
# HERE is what makes its ADR 0042 §9.1 ALLOCATED veto load-bearing from this
# point on: an unallocated claim only carries the weaker INTENT veto, and
# Phases 8-10 must not depend on the weaker one.
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM2" '{.status.ready}' true 300

# Exactly ONE CNPG Cluster — the lazy-create is idempotent across claims.
cluster_count=$(kubectl -n "$CNPG_NS" get cluster.postgresql.cnpg.io \
    --no-headers 2>/dev/null | wc -l | tr -d ' ')
assert_eq "CNPG Cluster count (lazy-create idempotency)" "$cluster_count" "1"
cluster_name_got=$(jp cluster.postgresql.cnpg.io "$CNPG_NS" "$CNPG_CLUSTER" \
    '{.metadata.name}')
assert_eq "shared CNPG Cluster name" "$cluster_name_got" "$CNPG_CLUSTER"

# Database CR: ensure=present, owner=role.
db_ensure=$(jp database.postgresql.cnpg.io "$CNPG_NS" "$DB_OBJECT" '{.spec.ensure}')
assert_eq "Database ${DB_OBJECT} ensure" "$db_ensure" "present"
db_owner=$(jp database.postgresql.cnpg.io "$CNPG_NS" "$DB_OBJECT" '{.spec.owner}')
assert_eq "Database ${DB_OBJECT} owner" "$db_owner" "$PG_ROLE"

# Password Secret is a basic-auth Secret in the CNPG namespace.
pw_type=$(jp secret "$CNPG_NS" "$PW_SECRET" '{.type}')
assert_eq "password Secret ${PW_SECRET} type" "$pw_type" "kubernetes.io/basic-auth"

# The role is present in the shared Cluster's spec.managed.roles.
role_present=$(kubectl -n "$CNPG_NS" get cluster.postgresql.cnpg.io \
    "$CNPG_CLUSTER" \
    -o jsonpath="{.spec.managed.roles[?(@.name==\"${PG_ROLE}\")].name}" \
    2>/dev/null || true)
assert_eq "managed role in Cluster spec.managed.roles" "$role_present" "$PG_ROLE"

# Connection Secret: status ref, DECOMPOSED keys (2.12 / ADR 0046 #3:
# url user pass host port db — the composed `DATABASE_URL` key is dropped),
# ownerRef cascade.
conn_ref=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.connectionSecretRef}')
assert_eq "status.connectionSecretRef" "$conn_ref" "$CONN_SECRET"
conn_key=$(jp secret "$APP_NS" "$CONN_SECRET" '{.data.url}')
if [ -z "$conn_key" ]; then
    printf 'ERROR: connection Secret %s missing decomposed `url` key\n' "$CONN_SECRET" >&2
    exit 1
fi
printf '  ok: connection Secret %s carries the decomposed `url` key\n' "$CONN_SECRET"
# The old composed key must be GONE (2.4e DATABASE_URL key removed).
old_key=$(jp secret "$APP_NS" "$CONN_SECRET" '{.data.DATABASE_URL}')
assert_eq "connection Secret has NO DATABASE_URL key (2.4e dropped)" "$old_key" ""
conn_owner_kind=$(jp secret "$APP_NS" "$CONN_SECRET" \
    '{.metadata.ownerReferences[0].kind}')
assert_eq "connection Secret ownerRef Kind" "$conn_owner_kind" "ResourceClaim"

# SSA-split guard: the provisioner's status write (ready /
# connectionSecretRef / Ready) must NOT have clobbered the scheduler's
# Scheduled=True. Re-assert it after provisioning.
sched_after=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM" Scheduled)
assert_eq "Scheduled still True after provision (SSA split)" "$sched_after" "True"

# ===============================================================
# Phase 7: resume + DSN (2.4d / 2.12) — Application Ready, env ref resolved
# ===============================================================

phase "Phase 7: resume — Application Ready, DATABASE_URL claim ref resolved"

wait_jsonpath "$APP_RES" "$APP_NS" "$APP" '{.status.phase}' Ready 180

# 2.12 (ADR 0046 #8): the explicit `{claim: "pg.url"}` ref resolves to a
# secretKeyRef into the connection Secret, key `url` (the decomposed DSN key,
# NOT the old composed `DATABASE_URL` key).
env_secret=$(kubectl -n "$APP_NS" get deployment "$APP" \
    -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"DATABASE_URL\")].valueFrom.secretKeyRef.name}" \
    2>/dev/null || true)
assert_eq "Deployment env DATABASE_URL secretKeyRef.name" "$env_secret" "$CONN_SECRET"
env_key=$(kubectl -n "$APP_NS" get deployment "$APP" \
    -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"DATABASE_URL\")].valueFrom.secretKeyRef.key}" \
    2>/dev/null || true)
assert_eq "Deployment env DATABASE_URL secretKeyRef.key" "$env_key" "url"

kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP}" --timeout=300s
printf '  Deployment %s -> Available\n' "$APP"

# ===============================================================
# Phase 8: delete + snapshot (2.4f) — RetainedClaim, cascade, grace floor
# ===============================================================

phase "Phase 8: delete Application — RetainedClaim snapshot + 7-day grace floor"

# Delete the APPLICATION, not just the ResourceClaim: the claim is owned by
# the Application (ownerRef), so deleting the claim alone while the app still
# declares needs.pg makes the Application controller regenerate it, and the
# provisioner re-provisions onto the same role/DB — which (2.4f Fix A) CANCELS
# the just-written RetainedClaim (cancel-on-re-provision + the GC live-claim
# guard), so the snapshot never persists for the grace/GC assertions below.
# Deleting the app cascades to the claim with no regeneration. (Mirrors the
# needs-redis-walk Phase 11 pattern.)
kubectl delete "$APP_RES" "$APP" -n "$APP_NS" --wait=true

# The finalizer snapshots a RetainedClaim into apprafter-system.
wait_jsonpath retainedclaim "$RETAINED_NS" "$RETAINED" \
    '{.spec.claimRef.name}' "$CLAIM" 120
rc_role=$(jp retainedclaim "$RETAINED_NS" "$RETAINED" '{.spec.role}')
assert_eq "RetainedClaim spec.role" "$rc_role" "$PG_ROLE"
rc_backend=$(jp retainedclaim "$RETAINED_NS" "$RETAINED" '{.spec.backend}')
assert_eq "RetainedClaim spec.backend" "$rc_backend" "cloudnative-pg"
rc_cluster=$(jp retainedclaim "$RETAINED_NS" "$RETAINED" '{.spec.cnpgCluster}')
assert_eq "RetainedClaim spec.cnpgCluster" "$rc_cluster" "$CNPG_CLUSTER"
rc_retain=$(jp retainedclaim "$RETAINED_NS" "$RETAINED" '{.spec.retainUntil}')
# RFC3339: a non-empty timestamp starting with a 4-digit year.
case "$rc_retain" in
    [0-9][0-9][0-9][0-9]-*) printf '  ok: RetainedClaim retainUntil = %s\n' "$rc_retain" ;;
    *) printf 'ERROR: RetainedClaim retainUntil not RFC3339: %q\n' "$rc_retain" >&2; exit 1 ;;
esac

# The connection Secret cascades (ownerRef → ResourceClaim).
wait_gone secret "$APP_NS" "$CONN_SECRET" 120

# The 7-day grace floor holds: the role and Database survive the delete
# (GC has NOT fired — retainUntil is days away).
role_floor=$(kubectl -n "$CNPG_NS" get cluster.postgresql.cnpg.io \
    "$CNPG_CLUSTER" \
    -o jsonpath="{.spec.managed.roles[?(@.name==\"${PG_ROLE}\")].name}" \
    2>/dev/null || true)
assert_eq "role STILL present during grace" "$role_floor" "$PG_ROLE"
db_floor=$(jp database.postgresql.cnpg.io "$CNPG_NS" "$DB_OBJECT" '{.spec.ensure}')
assert_eq "Database STILL ensure=present during grace" "$db_floor" "present"

# ===============================================================
# Phase 9: force GC (2.4f) — re-create RetainedClaim with a past
#          retainUntil; the GC drops role + DB + password Secret
# ===============================================================

phase "Phase 9: force GC — past retainUntil drops role/DB/Secret"

# The RetainedClaim is immutable (CEL self==oldSelf); an in-place patch
# is rejected. Delete it and RE-CREATE with the same coordinates but a
# retainUntil in the past so the GC fires immediately.
kubectl delete retainedclaim "$RETAINED" -n "$RETAINED_NS" --wait=true

kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: RetainedClaim
metadata:
  name: ${RETAINED}
  namespace: ${RETAINED_NS}
spec:
  claimRef:
    name: ${CLAIM}
    namespace: ${APP_NS}
  provider: ${PROVIDER}
  backend: cloudnative-pg
  cnpgCluster: ${CNPG_CLUSTER}
  cnpgNamespace: ${CNPG_NS}
  role: ${PG_ROLE}
  database: ${PG_ROLE}
  databaseObjectName: ${DB_OBJECT}
  passwordSecretName: ${PW_SECRET}
  retainUntil: "2000-01-01T00:00:00Z"
YAML

# Role removed from the shared Cluster's spec.managed.roles.
printf '  waiting for the managed role to be dropped ...\n'
deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    role_left=$(kubectl -n "$CNPG_NS" get cluster.postgresql.cnpg.io \
        "$CNPG_CLUSTER" \
        -o jsonpath="{.spec.managed.roles[?(@.name==\"${PG_ROLE}\")].name}" \
        2>/dev/null || true)
    [ -z "$role_left" ] && break
    sleep 5
done
role_left=$(kubectl -n "$CNPG_NS" get cluster.postgresql.cnpg.io \
    "$CNPG_CLUSTER" \
    -o jsonpath="{.spec.managed.roles[?(@.name==\"${PG_ROLE}\")].name}" \
    2>/dev/null || true)
assert_eq "managed role removed by GC" "$role_left" ""

# Database flipped to ensure=absent (CNPG drops the DB).
wait_jsonpath database.postgresql.cnpg.io "$CNPG_NS" "$DB_OBJECT" \
    '{.spec.ensure}' absent 120

# Password Secret + the RetainedClaim itself are deleted.
wait_gone secret "$CNPG_NS" "$PW_SECRET" 120
wait_gone retainedclaim "$RETAINED_NS" "$RETAINED" 120

# ===============================================================
# Phase 10: psql DROP proof — the DB + role are PHYSICALLY gone from
#           Postgres, not merely the CR reporting ensure:absent.
# ===============================================================

phase "Phase 10: psql DROP proof — database + role physically gone"

PRIMARY=$(kubectl -n "$CNPG_NS" get pod \
    -l "cnpg.io/cluster=${CNPG_CLUSTER},role=primary" \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
if [ -z "$PRIMARY" ]; then
    printf 'ERROR: could not find the CNPG primary pod for %s\n' "$CNPG_CLUSTER" >&2
    kubectl -n "$CNPG_NS" get pod -l "cnpg.io/cluster=${CNPG_CLUSTER}" >&2 || true
    exit 1
fi
printf '  CNPG primary pod: %s\n' "$PRIMARY"

# CNPG's ensure:absent reconcile lags the CR patch, so wrap the proof
# in retry: the database row must disappear from pg_database.
assert_db_gone() {
    local row
    row=$(kubectl exec "$PRIMARY" -n "$CNPG_NS" -- \
        psql -U postgres -tAc \
        "SELECT 1 FROM pg_database WHERE datname='${PG_ROLE}'" 2>/dev/null || true)
    [ -z "$row" ]
}
assert_role_gone() {
    local row
    row=$(kubectl exec "$PRIMARY" -n "$CNPG_NS" -- \
        psql -U postgres -tAc \
        "SELECT 1 FROM pg_roles WHERE rolname='${PG_ROLE}'" 2>/dev/null || true)
    [ -z "$row" ]
}

retry 24 5 -- assert_db_gone || {
    printf 'ERROR: database %s STILL present in pg_database after GC — CNPG did NOT honor ensure:absent\n' \
        "$PG_ROLE" >&2
    exit 1
}
printf '  ok: database %s is physically gone (pg_database empty)\n' "$PG_ROLE"

retry 24 5 -- assert_role_gone || {
    printf 'ERROR: role %s STILL present in pg_roles after GC\n' "$PG_ROLE" >&2
    exit 1
}
printf '  ok: role %s is physically gone (pg_roles empty)\n' "$PG_ROLE"

# ===============================================================
# Phase 11: reaper baseline (ADR 0042 §9) — Cluster identity, the PVC
#           that must survive a reap, and a marker row that proves the
#           volume came back with its data on it
# ===============================================================

phase "Phase 11: reaper baseline — Cluster uid, instance PVC, marker row"

# pg_primary — the current primary pod. Re-resolved on every use because
# the reap round-trip DELETES the pod and the re-provisioned one has a
# different name; $PRIMARY from Phase 10 is stale the moment Phase 12 runs.
pg_primary() {
    kubectl -n "$CNPG_NS" get pod \
        -l "cnpg.io/cluster=${CNPG_CLUSTER},role=primary" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# psql_super <sql> — run one statement on the primary as the superuser,
# in the `postgres` database. The marker row lives there rather than in a
# claim's database so that it is independent of role/Database GC: the only
# thing under test here is whether the VOLUME survived.
psql_super() {
    local primary
    primary="$(pg_primary)"
    if [ -z "$primary" ]; then
        printf 'ERROR: no CNPG primary pod for %s\n' "$CNPG_CLUSTER" >&2
        return 1
    fi
    # stderr deliberately NOT swallowed (unlike the Phase-10 helpers, which
    # poll an expected-to-fail query): every use here is a statement that
    # must succeed, so a psql error has to be readable in the walk log.
    kubectl exec "$primary" -n "$CNPG_NS" -- psql -U postgres -tAc "$1"
}

CNPG_UID_1=$(jp cluster.postgresql.cnpg.io "$CNPG_NS" "$CNPG_CLUSTER" '{.metadata.uid}')
if [ -z "$CNPG_UID_1" ]; then
    printf 'ERROR: could not read the shared Cluster uid — the reap assertions below identify the cluster BY uid, not by name\n' >&2
    exit 1
fi
printf '  baseline: Cluster %s uid=%s\n' "$CNPG_CLUSTER" "$CNPG_UID_1"

# The instance PVC — discovered, not assumed: CNPG derives its name from the
# cluster + instance ordinal, and that derivation is CNPG's, not ours.
CNPG_PVC=$(kubectl -n "$CNPG_NS" get pvc --no-headers \
    -o custom-columns=NAME:.metadata.name 2>/dev/null \
    | grep "^${CNPG_CLUSTER}-" | head -1 || true)
if [ -z "$CNPG_PVC" ]; then
    printf 'ERROR: no PVC named %s-* in %s — the §9.2/§9.3 volume-preservation gate has nothing to assert on\n' \
        "$CNPG_CLUSTER" "$CNPG_NS" >&2
    kubectl -n "$CNPG_NS" get pvc >&2 2>&1 || true
    exit 1
fi
CNPG_PV_1=$(jp pvc "$CNPG_NS" "$CNPG_PVC" '{.spec.volumeName}')
pvc_phase=$(jp pvc "$CNPG_NS" "$CNPG_PVC" '{.status.phase}')
assert_eq "instance PVC ${CNPG_PVC} is Bound (baseline)" "$pvc_phase" "Bound"
printf '  baseline: PVC %s -> PV %s\n' "$CNPG_PVC" "$CNPG_PV_1"

# The `controller: true` ownerReference §9.3 strips. REPORTED, NOT ASSERTED,
# and deliberately so: its PRESENCE is what makes the strip necessary, but a
# future CNPG that stopped setting it would be a SAFE change (the strip
# becomes a no-op and the volume survives anyway). Asserting it here would
# fail the walk on a change in the harmless direction. The dangerous
# direction — CNPG re-adding it after the strip, so the delete cascades — is
# caught by the PVC-still-Bound assertion in Phase 12, which is the gate.
pvc_owner=$(jp pvc "$CNPG_NS" "$CNPG_PVC" '{.metadata.ownerReferences[0].uid}')
printf '  baseline: PVC ownerReferences[0].uid = %q (Cluster uid = %q)\n' \
    "$pvc_owner" "$CNPG_UID_1"

# The marker row. This is what makes "the volume survived" a statement about
# the DATA rather than about a PVC object: a cascade destroys it, and a
# re-provision onto a fresh volume comes back without it.
psql_super "CREATE TABLE IF NOT EXISTS reap_marker(id int)" >/dev/null
psql_super "INSERT INTO reap_marker VALUES (42)" >/dev/null
# Force it to disk so the assertion does not rest on how gracefully the
# reaped pod happened to shut down.
psql_super "CHECKPOINT" >/dev/null
marker_before=$(psql_super "SELECT id FROM reap_marker")
assert_eq "marker row readable before the reap" "$marker_before" "42"

# ===============================================================
# Phase 12: THE ADR 0042 §9.2/§9.3 GATE — reap the tenantless shared
#           Cluster and ASSERT the volume was preserved
# ===============================================================

phase "Phase 12: reap — RETAINED vetoes, then post-grace reap preserves the PVC"

REAPED_BEFORE=$(reap_metric cnpg reaped)
VETO_RETAINED_BEFORE=$(reap_metric cnpg veto_retained)
printf '  baseline counters: reaped=%s veto_retained=%s\n' \
    "$REAPED_BEFORE" "$VETO_RETAINED_BEFORE"

# Remove the last tenant. The claim's finalizer snapshots a RetainedClaim,
# which then holds the §9.1 RETAINED veto — the cluster must NOT be reaped
# while a snapshot can still reattach to its data.
kubectl delete "$APP_RES" "$APP2" -n "$APP_NS" --wait=true
wait_jsonpath retainedclaim "$RETAINED_NS" "$RETAINED2" \
    '{.spec.claimRef.name}' "$CLAIM2" 120

# Assert the VETO as a counter delta, never as a transient phase: a walk
# cannot reliably witness a decision that is re-taken every tick.
printf '  waiting for a veto_retained tick on the cnpg arm ...\n'
deadline=$(( $(date +%s) + 240 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    veto_now=$(reap_metric cnpg veto_retained)
    [ "$veto_now" -gt "$VETO_RETAINED_BEFORE" ] && break
    sleep 10
done
veto_now=$(reap_metric cnpg veto_retained)
if [ "$veto_now" -le "$VETO_RETAINED_BEFORE" ]; then
    printf 'ERROR: cnpg veto_retained never moved (%s -> %s) — the reaper is not evaluating the shared Cluster at all, so nothing below would mean anything\n' \
        "$VETO_RETAINED_BEFORE" "$veto_now" >&2
    exit 1
fi
printf '  ok: cnpg veto_retained %s -> %s (the snapshot held the cluster)\n' \
    "$VETO_RETAINED_BEFORE" "$veto_now"
uid_grace=$(jp cluster.postgresql.cnpg.io "$CNPG_NS" "$CNPG_CLUSTER" '{.metadata.uid}')
assert_eq "shared Cluster survived the grace window" "$uid_grace" "$CNPG_UID_1"

# Simulate post-grace: drop the snapshot outright (a plain delete, NOT the
# past-retainUntil GC dance of Phase 9 — reclaiming the role/database is not
# what is under test here, and leaving them on disk is what lets the marker
# row prove the volume came back).
kubectl delete retainedclaim "$RETAINED2" -n "$RETAINED_NS" --wait=true

# Now nothing points at the cluster. Timeout arithmetic: the sweep ticks
# every 60s, so the first tick after the veto clears only STARTS the dwell
# and the reap lands on the next one — up to 2 ticks (~120s) plus the dwell
# itself. dwell + 180 keeps a real margin over that; it is a timeout, not a
# threshold, so slack here weakens nothing.
wait_gone cluster.postgresql.cnpg.io "$CNPG_NS" "$CNPG_CLUSTER" \
    $(( REAP_DWELL_SECS + 180 ))

# The memory must ACTUALLY come back — a reaper that deletes the CR while
# the workload lingers has reclaimed nothing (§9.3: "pods remaining: none").
printf '  waiting for the CNPG instance pods to go ...\n'
deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    pods_left=$(kubectl -n "$CNPG_NS" get pod \
        -l "cnpg.io/cluster=${CNPG_CLUSTER}" --no-headers 2>/dev/null \
        | wc -l | tr -d ' ')
    [ "$pods_left" = "0" ] && break
    sleep 5
done
pods_left=$(kubectl -n "$CNPG_NS" get pod \
    -l "cnpg.io/cluster=${CNPG_CLUSTER}" --no-headers 2>/dev/null \
    | wc -l | tr -d ' ')
assert_eq "no CNPG instance pods remain after the reap" "$pods_left" "0"

# ---- THE GATE (ADR 0042 §9.2) -----------------------------------------
# The PVC is still Bound. If the reaper's ownerReference strip stopped
# holding — because a CNPG chart bump re-adds the `controller: true`
# reference after the strip and before the delete — the apiserver GC takes
# the volume with the Cluster and this fails. That is the whole point of
# this phase: §9.2 says "re-measure on a CNPG chart bump", and this is what
# makes that instruction enforced rather than written down.
#
# Held under observation rather than sampled once: a cascade deletes the
# PVC ASYNCHRONOUSLY, so a single read taken the instant the Cluster
# disappears can still see a doomed volume.
printf '  holding the PVC under observation for 30s (a cascade deletes it asynchronously) ...\n'
for _ in $(seq 1 6); do
    pvc_phase=$(jp pvc "$CNPG_NS" "$CNPG_PVC" '{.status.phase}')
    pvc_deleting=$(jp pvc "$CNPG_NS" "$CNPG_PVC" '{.metadata.deletionTimestamp}')
    if [ "$pvc_phase" != "Bound" ] || [ -n "$pvc_deleting" ]; then
        printf 'ERROR: PVC %s did NOT survive the reap (phase=%q deletionTimestamp=%q).\n' \
            "$CNPG_PVC" "$pvc_phase" "$pvc_deleting" >&2
        printf '       ADR 0042 §9.2/§9.3: the reaper strips the Cluster ownerReference from the\n' >&2
        printf '       instance PVC before deleting, so the volume is preserved instead of cascaded.\n' >&2
        printf '       A PVC that goes away here means that strip stopped holding on the CNPG chart\n' >&2
        printf '       this cluster runs — RE-MEASURE §9.2 against that chart before releasing.\n' >&2
        kubectl -n "$CNPG_NS" get pvc >&2 2>&1 || true
        kubectl -n "$CNPG_NS" get pv >&2 2>&1 || true
        exit 1
    fi
    sleep 5
done
printf '  ok: PVC %s still Bound after the reap (volume preserved, §9.2)\n' "$CNPG_PVC"

pv_after=$(jp pvc "$CNPG_NS" "$CNPG_PVC" '{.spec.volumeName}')
assert_eq "PVC still bound to the SAME PV after the reap" "$pv_after" "$CNPG_PV_1"

# The strip itself is visible: no ownerReference to the (now gone) Cluster.
owner_after=$(jp pvc "$CNPG_NS" "$CNPG_PVC" '{.metadata.ownerReferences[*].uid}')
case "$owner_after" in
    *"$CNPG_UID_1"*)
        printf 'ERROR: PVC %s still carries an ownerReference to the reaped Cluster uid %s — the §9.3 strip did not happen (the volume survived only because the GC has not caught up yet)\n' \
            "$CNPG_PVC" "$CNPG_UID_1" >&2
        exit 1 ;;
    *) printf '  ok: PVC carries no ownerReference to the reaped Cluster (§9.3 strip)\n' ;;
esac

reaped_after=$(reap_metric cnpg reaped)
if [ "$reaped_after" -le "$REAPED_BEFORE" ]; then
    printf 'ERROR: cnpg reaped counter did not move (%s -> %s) — the Cluster went away for some reason OTHER than the reaper\n' \
        "$REAPED_BEFORE" "$reaped_after" >&2
    exit 1
fi
printf '  ok: cnpg reaped %s -> %s (the reaper is what removed it)\n' \
    "$REAPED_BEFORE" "$reaped_after"

# ===============================================================
# Phase 13: re-provision (ADR 0042 §9.5) — the SAME PV is adopted and
#           the marker row reads back
# ===============================================================

phase "Phase 13: re-provision — same PV adopted, marker row intact"

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
    expose:
      port: 80
    needs:
      pg:
        selector:
          tier: integrated
        size: small
YAML

# A full lazy re-create (Cluster + instance boot + role/DB), so budget more
# than the first provision did.
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM2" '{.status.ready}' true 600

CNPG_UID_2=$(jp cluster.postgresql.cnpg.io "$CNPG_NS" "$CNPG_CLUSTER" '{.metadata.uid}')
if [ -z "$CNPG_UID_2" ] || [ "$CNPG_UID_2" = "$CNPG_UID_1" ]; then
    printf 'ERROR: the shared Cluster uid is %q, want a NEW uid different from the reaped %q — the cluster was never actually deleted, so this walk proved nothing about the reap\n' \
        "$CNPG_UID_2" "$CNPG_UID_1" >&2
    exit 1
fi
printf '  ok: re-provisioned Cluster is a NEW object (uid %s -> %s)\n' \
    "$CNPG_UID_1" "$CNPG_UID_2"

pv_readopted=$(jp pvc "$CNPG_NS" "$CNPG_PVC" '{.spec.volumeName}')
assert_eq "re-provisioned Cluster adopted the SAME PV (§9.5)" \
    "$pv_readopted" "$CNPG_PV_1"

# The re-provision is a REAL one, not a claim that merely reports ready: the
# tenant's managed role is back in the new Cluster's spec.
role_readopted=$(kubectl -n "$CNPG_NS" get cluster.postgresql.cnpg.io \
    "$CNPG_CLUSTER" \
    -o jsonpath="{.spec.managed.roles[?(@.name==\"${PG_ROLE2}\")].name}" \
    2>/dev/null || true)
assert_eq "re-provisioned Cluster carries the ledger managed role" \
    "$role_readopted" "$PG_ROLE2"

printf '  waiting for the re-provisioned CNPG primary to be Ready ...\n'
retry 60 10 -- kubectl -n "$CNPG_NS" wait --for=condition=Ready \
    pod -l "cnpg.io/cluster=${CNPG_CLUSTER},role=primary" --timeout=30s

# The round trip closes here: the row written before the reap is still
# readable after it. THIS is the safety argument, asserted rather than
# inferred from the PVC object surviving.
marker_after=$(psql_super "SELECT id FROM reap_marker")
assert_eq "marker row survived the reap round-trip (§9.5)" "$marker_after" "42"

# ===============================================================
# Phase 14: negative test — a shared backend with a LIVE tenant is
#           never reaped (ADR 0042 §9.1 ALLOCATED)
# ===============================================================

phase "Phase 14: negative test — 3x dwell with a live tenant, nothing is reaped"

OP_POD_BEFORE=$(operator_pod)
REAPED_BEFORE_NEG=$(reap_metric cnpg reaped)
VETO_LIVE_BEFORE_NEG=$(reap_metric cnpg veto_live)
UID_BEFORE_NEG="$CNPG_UID_2"

printf '  sleeping %ss (3x dwell) with the ledger claim live ...\n' \
    "$(( REAP_DWELL_SECS * 3 ))"
sleep $(( REAP_DWELL_SECS * 3 ))

# Counters are per-pod and reset on restart, so a restart here would make
# "did not increase" true for the wrong reason.
OP_POD_AFTER=$(operator_pod)
assert_eq "the operator pod did not restart during the negative test" \
    "$OP_POD_AFTER" "$OP_POD_BEFORE"

uid_after_neg=$(jp cluster.postgresql.cnpg.io "$CNPG_NS" "$CNPG_CLUSTER" '{.metadata.uid}')
assert_eq "the live shared Cluster kept its identity across 3x dwell" \
    "$uid_after_neg" "$UID_BEFORE_NEG"

reaped_after_neg=$(reap_metric cnpg reaped)
assert_eq "no cnpg reap fired while a tenant was live" \
    "$reaped_after_neg" "$REAPED_BEFORE_NEG"

# And it was VETOED, not merely skipped: without this, a reaper that had
# died would pass the assertion above for exactly the wrong reason.
veto_live_after_neg=$(reap_metric cnpg veto_live)
if [ "$veto_live_after_neg" -le "$VETO_LIVE_BEFORE_NEG" ]; then
    printf 'ERROR: cnpg veto_live did not move (%s -> %s) across 3x dwell — the reaper was not evaluating the cluster at all, so "nothing was reaped" says nothing\n' \
        "$VETO_LIVE_BEFORE_NEG" "$veto_live_after_neg" >&2
    exit 1
fi
printf '  ok: cnpg veto_live %s -> %s (evaluated every tick, vetoed every time)\n' \
    "$VETO_LIVE_BEFORE_NEG" "$veto_live_after_neg"

# ===============================================================
# Phase 15: sweep health — no per-tick error, and the arm with no
#           instances is silent rather than failing
# ===============================================================

phase "Phase 15: sweep health — the reaper errored on no tick of this run"

# The reaper reports a failed sweep on its OWN counter, under the synthetic
# backend label `unknown` (reaper.rs `run`), not on
# apprafter_reconcile_errors_total. Assert BOTH: the real bucket, and the
# reconcile-error series a future re-plumbing might move the signal to — so
# such a move fails this walk rather than silently passing it.
sweep_errors=$(reap_metric unknown error)
assert_eq 'apprafter_shared_backend_reap_total{backend="unknown",result="error"}' \
    "$sweep_errors" "0"
reconcile_errors=$(metric_value \
    'apprafter_reconcile_errors_total{kind="SharedBackendReaper"}')
assert_eq 'apprafter_reconcile_errors_total{kind="SharedBackendReaper"}' \
    "$reconcile_errors" "0"

if kubectl -n apprafter-system logs deploy/apprafter-operator --tail=-1 2>/dev/null \
    | strip_ansi | grep -q 'SharedBackendReaper sweep failed'; then
    printf 'ERROR: the operator log carries a "SharedBackendReaper sweep failed" line — a sweep aborted on this run\n' >&2
    kubectl -n apprafter-system logs deploy/apprafter-operator --tail=-1 2>/dev/null \
        | strip_ansi | grep 'SharedBackendReaper' >&2 || true
    exit 1
fi
printf '  ok: no sweep-failure line in the operator log\n'

# The Dragonfly arm ran on every tick of this walk with NOTHING to reap —
# the dragonfly-operator is always-on and its CRD is served, but this walk
# creates no redis claim, so the arm's candidate set is empty on every pass.
# An empty candidate set must be silent: no error (asserted above, the error
# bucket is shared) and no reap.
df_eph_reaped=$(reap_metric dragonfly-ephemeral reaped)
assert_eq "dragonfly-ephemeral reaps on a cluster with no Dragonfly instances" \
    "$df_eph_reaped" "0"
df_per_reaped=$(reap_metric dragonfly-persistent reaped)
assert_eq "dragonfly-persistent reaps on a cluster with no Dragonfly instances" \
    "$df_per_reaped" "0"

# ===============================================================
# Done — tear down on success path
# ===============================================================

# Remove the EXIT trap so cleanup() does not fire again — we own the
# tear-down inline here on the success path.
trap - EXIT

if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    k3d_down "$CLUSTER_NAME" || true
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving k3d cluster %s up.\n' \
        "$CLUSTER_NAME"
fi

rm -rf "$TMPDIR_WORK"

printf '\nneeds-pg-walk GREEN in %s\n' "$(elapsed)"
printf 'Chain proven: generate -> schedule -> provision -> resume + DSN -> delete + snapshot -> GC -> psql DROP\n'
printf 'ADR 0042 §9 proven: RETAINED veto -> post-grace reap (PVC still Bound, no pods) -> same PV re-adopted with the marker row intact -> live tenant never reaped -> no sweep error\n'
