#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter needs.redis-walk e2e — the full Phase-2.6 ResourceClaim
# chain on a local k3d cluster (plan item 2.6-8, ADR 0042).
#
# This script exercises the shipped needs.redis pipeline end-to-end,
# reusing the generic 2.4 machinery unchanged and adding the Dragonfly
# backend:
#
#   generate (2.4d) -> schedule (2.3) -> provision (2.6-3/2.6-4) ->
#   resume + explicit env refs (2.4d/2.12/2.6-6) -> isolation proof ($N ACL) ->
#   scripting/client-init/restart-repin (2.6-5, Pre-merge #4/#5/#6) ->
#   persistent variant + restart-durable data (2.6-2/2.6-3, §6) ->
#   delete + snapshot (2.4f/2.6-4) -> force-GC (2.6-7) ->
#   redis-cli ACL/FLUSHDB proof -> shared-instance reaping (ADR 0042 §9)
#
# Concretely, on a k3d cluster bootstrapped with the platform stack
# (operator + admission-webhook + the always-on dragonfly-operator + the
# seeded `redis-integrated` ServiceProvider):
#
#   1. Apply an AppRafter Application with `spec.base.needs.redis`.
#   2. ASSERT the operator generates a ResourceClaim and HOLDS the
#      Application (status.phase=AwaitingResourceClaim).
#   3. ASSERT the scheduler matches it to `redis-integrated`
#      (status.provider, Scheduled=True).
#   4. ASSERT the provisioner lazily creates a shared Dragonfly instance,
#      allocates a numbered logical DB (status.dbnum), creates the $N
#      ACL user, and writes a connection Secret carrying the DECOMPOSED keys
#      (2.12 / ADR 0046 #3: url user pass host port db channelPrefix — the
#      old composed REDIS_URL/REDIS_CHANNEL_PREFIX keys are dropped) — and
#      that the scheduler's Scheduled=True survives the provisioner's status
#      write (the SSA field-manager split guard).
#   5. ASSERT the Application resumes to Ready and the rendered Deployment
#      resolves the EXPLICIT `env: {REDIS_URL: {claim: "redis.url"},
#      REDIS_CHANNEL_PREFIX: {claim: "redis.channelPrefix"}}` refs
#      (2.12 / ADR 0046) into secretKeyRefs on the connection Secret's
#      `url` / `channelPrefix` keys (the 2.4e auto-inject is removed).
#   6. Isolation proof (ADR 0042 Pre-merge #3): a SECOND claim's ACL user
#      gets NOPERM when it tries to SELECT the first claim's DB, AND its
#      cross-DB escape attempts (MOVE / COPY ... DB / SWAPDB aimed at the
#      first claim's DB) are each DENIED.
#   7. Scripting + client-init + restart re-pin (ADR 0042 Pre-merge #4/#5/#6):
#      an in-script EVAL cannot escape DB N (declared-keys + $N pin); an
#      ioredis/BullMQ-style client init (CLIENT SETNAME/SETINFO, PING,
#      BLPOP) passes under the ACL while CONFIG GET stays denied; and after
#      a `kubectl delete pod` of the instance the reconcile loop re-pins the
#      user so the app reconnects without NOPERM.
#   8. Persistent variant (ADR 0042 §6): a `needs.redis: {persistent: true}`
#      claim routes to a SEPARATE persistent pool instance (snapshot->PVC),
#      and a key written there survives a pod restart.
#   9. Delete the ResourceClaim; ASSERT the finalizer snapshots a
#      RetainedClaim (backend=dragonfly, 7-day grace) and the connection
#      Secret cascades.
#  10. Force GC by deleting + re-creating the RetainedClaim with a past
#      retainUntil; ASSERT FLUSHDB + ACL DELUSER ran (the ACL user is
#      gone, the DB is empty), and the snapshot is removed.
#  11. Shared-instance reaping (ADR 0042 §9). The EPHEMERAL arm: remove
#      the last tenant and ASSERT the instance, its StatefulSet and its
#      pod are gone, while the `RetainedClaim` REMAINS (the §9.7
#      asymmetry — an ephemeral instance is not held for the 7-day
#      grace, because a snapshot naming it has no data to reattach to)
#      and the `-admin` Secret REMAINS (the deliberate keep). The memory
#      is then asserted NUMERICALLY: the node's allocated memory
#      requests must drop by at least the instance's own reservation,
#      which is what a reaper that deletes the CR while the StatefulSet
#      lingers would fail.
#  12. Clean re-create: the pool comes back as a NEW object (a different
#      uid — not a survivor), the admin password is byte-identical (the
#      Secret was kept, so nothing rotates), and the app's Deployment
#      reaches Available again.
#  13. NEGATIVE test: with the app live, three dwells pass and the
#      instance uid is unchanged while `reaped` does not move (and
#      `veto_live` does, proving the reaper was running and deciding).
#      An over-eager reaper is far worse than an absent one.
#  14. PERSISTENT arm: a deleted-but-in-grace claim VETOES the instance
#      (`veto_retained`); dropping the snapshot then lets it be reaped
#      within one dwell, with its snapshot PVC still Bound, and the
#      re-provision adopts the SAME PV.
#
# CLI state injection
# -------------------
# `apprafter cluster-bootstrap` reads the kubeconfig from the CLI's
# per-target state store (not from $KUBECONFIG). We set
# APPRAFTER_CONFIG_DIR to a tmpdir and seed it with a minimal
# config.yaml (active_target: k3d) and a state.json carrying the k3d
# kubeconfig as kubeconfig_yaml (plaintext) — the same approach
# needs-pg-walk.sh / gitops-walk.sh use.
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
# Constants — the shipped needs.redis coordinates. The claim name is
# derived from the Application name (`<app>-redis`); the per-claim ACL
# user / connection-Secret / RetainedClaim names are derived from the
# claim's (namespace, name) by the 2.6 provisioner. See
# operator/operator-controllers/resourceclaim-provisioner/src/dragonfly.rs
# (`acl_user` / `pool_instance_name`) and reconcile.rs
# (`connection_secret_name` / `cnpg::k8s_name`) for the derivation.
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-redis-walk"

APP_NS="demo"                       # tenant namespace
APP="web"                           # Application name
CLAIM="web-redis"                   # generated ResourceClaim name
CONN_SECRET="web-redis-conn"        # connection Secret (app ns)

# A second app proves cross-DB isolation (ADR 0042 Pre-merge #3).
APP2="api"
CLAIM2="api-redis"
CONN_SECRET2="api-redis-conn"

# A THIRD app declares `needs.redis: {persistent: true}` (ADR 0042 §6) —
# it routes to a SEPARATE persistent pool instance (snapshot->PVC), so its
# data survives a pod restart. Used by the persistence + restart-durability
# phase (Pre-merge #6's snapshot half).
APP3="worker"
CLAIM3="worker-redis"
CONN_SECRET3="worker-redis-conn"

# Group-qualify the two collision-prone kinds so kubectl never resolves
# to the wrong API group: bare `application` also matches Argo CD's
# argoproj.io Application, and bare `resourceclaim` matches the k8s 1.32+
# DRA resource.k8s.io ResourceClaim. Always address the apprafter.io CRs.
APP_RES="application.apprafter.io"
CLAIM_RES="resourceclaim.apprafter.io"

# acl_user(demo, web-redis) / acl_user(demo, api-redis) / acl_user(demo, worker-redis).
ACL_USER="claim_demo_web-redis_redis"
ACL_USER2="claim_demo_api-redis_redis"
ACL_USER3="claim_demo_worker-redis_redis"
# RetainedClaim name = cnpg::k8s_name(demo, web-redis).
RETAINED="claim-demo-web-redis"
# ... and the same derivation for the api / worker claims. Both matter to
# the ADR 0042 §9 phases: the api snapshot is the one that must NOT hold an
# ephemeral instance alive (§9.7), the worker snapshot is the one that MUST
# hold a persistent one.
RETAINED2="claim-demo-api-redis"
RETAINED3="claim-demo-worker-redis"

DF_NS="dragonfly-system"            # shared Dragonfly instance namespace
# The ephemeral pool instance (index 0) the first non-persistent claim
# lands on (pool_instance_name(false, 0)).
DF_INSTANCE="platform-redis-ephemeral-000"
DF_ADMIN_SECRET="platform-redis-ephemeral-000-admin"
# The persistent pool instance (index 0) the `persistent: true` claim
# lands on (pool_instance_name(true, 0)) — a SEPARATE Dragonfly CR with a
# snapshot->PVC block; its admin Secret is keyed by its own name.
DF_INSTANCE_P="platform-redis-persistent-000"
DF_ADMIN_SECRET_P="platform-redis-persistent-000-admin"
RETAINED_NS="apprafter-system"      # RetainedClaim namespace
PROVIDER="redis-integrated"         # seeded ServiceProvider

# ADR 0042 §9 — the shared-backend reaper's dwell, pinned SHORT on the
# operator Deployment (Phase 1b) via APPRAFTER_REAP_DWELL_SECS. Production
# runs unset, at the 600s default; a walk that raced a ten-minute timer
# could only ever assert a transient, so it pins the timer instead and
# asserts terminal outcomes.
REAP_DWELL_SECS=30

# The Guaranteed memory reservation a Dragonfly pool instance holds (ADR
# 0053 / platform-stack `service_providers.cue`, T1 seed 320Mi). Phase 6
# asserts the running instance really requests at least this much, and the
# ephemeral-reap phase asserts the node gives back at least this much — so
# the threshold is tied to the shipped sizing rather than picked.
DRAGONFLY_MEM_MI=320

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
# UNRELEASED. ADR 0046 removed the 2.4e implicit REDIS_URL/REDIS_CHANNEL_PREFIX
# injection and the connection-Secret's composed `REDIS_URL`/`REDIS_CHANNEL_PREFIX`
# keys — the app now binds them by EXPLICIT `env: {REDIS_URL: {claim: "redis.url"},
# REDIS_CHANNEL_PREFIX: {claim: "redis.channelPrefix"}}` refs (Phase 3), which
# only the branch operator + webhook + CRD render/validate. So this walk builds +
# side-loads the working-tree operator/webhook and applies the branch CRDs
# (Phase 1b). The flag is mandatory until 2.12 ships.
# ---------------------------------------------------------------
if [ -z "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
    cat >&2 <<'EOF'
ERROR: needs-redis-walk requires APPRAFTER_E2E_LOCAL_OPERATOR=1 while 2.12 is unreleased.

ADR 0046 (Phase 2.12) removed the 2.4e implicit REDIS_URL/REDIS_CHANNEL_PREFIX
injection and the connection-Secret's composed `REDIS_URL`/`REDIS_CHANNEL_PREFIX`
keys. This walk now declares them explicitly
(`env: {REDIS_URL: {claim: "redis.url"}, REDIS_CHANNEL_PREFIX: {claim: "redis.channelPrefix"}}`),
which only the branch operator + webhook + CRD render/validate. Run:

  APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/needs-redis-walk.sh

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
        printf '\n!!! needs-redis-walk FAILED at %s (exit %d) !!!\n' \
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
# Helper: seed the CLI state store (mirrors needs-pg-walk.sh).
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

# ---------------------------------------------------------------
# Local helper: _mem_sum_mi — sum whitespace-separated Kubernetes memory
# quantities read on stdin into a MiB total.
#
# Used by the two readers below so the ephemeral-reap phase can assert the
# memory NUMERICALLY. An unrecognised suffix is reported on stderr and
# counted as zero: it appears identically in the before and after readings
# of the same pod, so it cancels out of the delta the assertion is about,
# and it must not pass silently.
# ---------------------------------------------------------------
_mem_sum_mi() {
    awk '
        function to_mi(v,   n) {
            if (v == "") return 0
            n = v
            if (v ~ /^[0-9.]+Ki$/) { sub(/Ki$/, "", n); return n / 1024 }
            if (v ~ /^[0-9.]+Mi$/) { sub(/Mi$/, "", n); return n }
            if (v ~ /^[0-9.]+Gi$/) { sub(/Gi$/, "", n); return n * 1024 }
            if (v ~ /^[0-9.]+Ti$/) { sub(/Ti$/, "", n); return n * 1048576 }
            if (v ~ /^[0-9.]+[kK]$/) { sub(/[kK]$/, "", n); return n * 1000 / 1048576 }
            if (v ~ /^[0-9.]+M$/)  { sub(/M$/, "", n);  return n * 1000000 / 1048576 }
            if (v ~ /^[0-9.]+G$/)  { sub(/G$/, "", n);  return n * 1000000000 / 1048576 }
            if (v ~ /^[0-9]+$/) return v / 1048576
            printf "  WARN: unrecognised memory quantity %s — counted as 0\n", v > "/dev/stderr"
            return 0
        }
        { for (i = 1; i <= NF; i++) total += to_mi($i) }
        END { printf "%d", total + 0 }'
}

# pod_mem_requests_mi <ns> <pod> — the pod's total container memory
# requests, in MiB.
pod_mem_requests_mi() {
    kubectl -n "$1" get pod "$2" \
        -o jsonpath='{range .spec.containers[*]}{.resources.requests.memory}{" "}{end}' \
        2>/dev/null | _mem_sum_mi
}

# node_mem_requests_mi <node> — the sum of container memory REQUESTS over
# every non-terminated pod scheduled on <node>: the quantity `kubectl
# describe node` prints as "Allocated resources / memory requests",
# computed from the API so the unit suffix is parsed rather than
# screen-scraped out of a human-formatted table.
node_mem_requests_mi() {
    local node="$1"
    kubectl get pods -A -o jsonpath='{range .items[*]}{.spec.nodeName}{"|"}{.status.phase}{"|"}{range .spec.containers[*]}{.resources.requests.memory}{" "}{end}{"\n"}{end}' \
        2>/dev/null \
        | awk -F'|' -v node="$node" \
            '$1 == node && ($2 == "Running" || $2 == "Pending") { print $3 }' \
        | _mem_sum_mi
}

# ---------------------------------------------------------------
# Local helper: redis_admin <args...>  — run redis-cli against the
# shared Dragonfly instance AS THE ADMIN (default) user, reading the
# admin password from the per-instance admin Secret. Used for the
# isolation proof and the post-GC ACL/keyspace assertions.
#
# Echoes redis-cli stdout; returns redis-cli's exit code.
# ---------------------------------------------------------------
redis_admin() {
    redis_admin_on "$DF_INSTANCE" "$DF_ADMIN_SECRET" "$@"
}

# redis_admin_on <instance> <admin-secret> <args...> — the same, but
# against an EXPLICIT pool instance (used for the persistent-instance
# assertions, which run on $DF_INSTANCE_P, not the ephemeral default).
redis_admin_on() {
    local instance="$1" admin_secret="$2"; shift 2
    local admin_pw
    admin_pw=$(kubectl -n "$DF_NS" get secret "$admin_secret" \
        -o jsonpath='{.data.password}' 2>/dev/null | base64 -d)
    kubectl -n "$DF_NS" exec "${instance}-0" -- \
        redis-cli -a "$admin_pw" --no-auth-warning "$@" 2>/dev/null
}

# redis_as <user> <password> <args...> — run redis-cli AS a per-claim
# ACL user (proves the user's grants directly) against the ephemeral
# instance. redis_as_on parameterises the instance for the persistent path.
redis_as() {
    redis_as_on "$DF_INSTANCE" "$@"
}

# redis_as_on <instance> <user> <password> <args...> — as above, against
# an explicit pool instance. Output goes to stdout (stderr folded in) so a
# NOPERM/WRONGPASS reply is greppable; redis-cli's own exit is swallowed.
redis_as_on() {
    local instance="$1" user="$2" pw="$3"; shift 3
    # `--user/--pass` (not `-u redis://…`): the Dragonfly image bundles
    # redis-cli 6.0.16, whose URL parser ignores the userinfo username and
    # AUTHs as `default` → NOAUTH. The explicit ACL flags work on 6.0.x. The
    # caller passes the DB via `-n <dbnum>` (the user is $N-pinned, so it can
    # only SELECT its own N anyway).
    kubectl -n "$DF_NS" exec "${instance}-0" -- \
        redis-cli --user "$user" --pass "$pw" --no-auth-warning "$@" 2>&1 || true
}

# Recover a claim's ACL password from its connection Secret's `pass` key.
# 2.12 (ADR 0046 #3): the connection Secret carries decomposed keys
# (url user pass host port db channelPrefix); `pass` is the direct
# password without DSN parsing.
claim_password() {
    local conn_ns="$1" conn_name="$2"
    kubectl -n "$conn_ns" get secret "$conn_name" \
        -o jsonpath='{.data.pass}' 2>/dev/null | base64 -d
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

phase "Phase 1: cluster-bootstrap (platform stack + dragonfly-operator + redis-integrated)"

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
# `REDIS_URL`/`REDIS_CHANNEL_PREFIX` connection-Secret keys and adds the `env`
# value node + webhook env-ref rules — all UNRELEASED, so the published image +
# CRD this cluster bootstrapped from cannot render/validate the explicit DSN
# refs in Phase 3.
# Mirrors needs-pg-walk Phase 1b (no extra RBAC — env refs add no new k8s
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
# BRANCH-rendered CRDs server-side. Mirrors needs-pg-walk Phase 1b.
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
# Phase 2: readiness — dragonfly-operator, the seeded provider, the webhook
# ===============================================================

phase "Phase 2: platform readiness (AppProject, dragonfly-operator, provider, webhook)"

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

# dragonfly-operator Deployment must be Available before any claim can be
# provisioned (the provisioner SSA-applies the Dragonfly CR + drives ACL
# over the Redis protocol once the instance is up).
printf '  waiting for the dragonfly-operator Deployment ...\n'
retry 30 10 -- kubectl -n "$DF_NS" rollout status \
    deploy -l app.kubernetes.io/name=dragonfly-operator --timeout=60s

# ASSERT the `redis-integrated` ServiceProvider is seeded with the
# tier=integrated label the needs.redis selector matches.
retry 30 10 -- kubectl get serviceprovider "$PROVIDER" \
    -n "$RETAINED_NS" >/dev/null
sp_tier=$(jp serviceprovider "$RETAINED_NS" "$PROVIDER" '{.metadata.labels.tier}')
assert_eq "ServiceProvider ${PROVIDER} label tier" "$sp_tier" "integrated"
sp_backend=$(jp serviceprovider "$RETAINED_NS" "$PROVIDER" '{.spec.backend}')
assert_eq "ServiceProvider ${PROVIDER} backend" "$sp_backend" "dragonfly"

# The admission webhook must be Available before applying the
# needs.redis Application (it validates the CR on CREATE).
printf '  waiting for the admission-webhook Deployment ...\n'
retry 30 10 -- kubectl -n "$RETAINED_NS" rollout status \
    deploy admission-webhook --timeout=60s

# ===============================================================
# Phase 3: apply the needs.redis Application
# ===============================================================

phase "Phase 3: apply Applications with spec.base.needs.redis + explicit DSN refs"

kubectl create namespace "$APP_NS" 2>/dev/null || true

# 2.12 (ADR 0046 #5): the 2.4e implicit REDIS_URL/REDIS_CHANNEL_PREFIX injection
# is removed — the app binds them by EXPLICIT claim refs. The cue-cmp renders
# the bare selector `claim.redis.url` → the `{claim: "redis.url"}` marker; we
# apply that resolved marker form directly (this walk runs raw CRs, not the
# cue-cmp). The Phase 7 assertions verify the rendered Deployment carries
# secretKeyRefs into the connection Secret's `url`/`channelPrefix` keys.
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
      redis:
        selector:
          tier: integrated
    env:
      REDIS_URL:
        claim: "redis.url"
      REDIS_CHANNEL_PREFIX:
        claim: "redis.channelPrefix"
YAML

# A second app — same ephemeral pool instance, a DIFFERENT numbered DB.
# Used in Phase 8 for the cross-DB isolation proof (ADR 0042 #3).
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
      redis:
        selector:
          tier: integrated
YAML

# A third app declares the `persistent: true` variant (ADR 0042 §6). It
# routes to a SEPARATE persistent pool instance (snapshot->PVC), exercised
# in Phase 10 (placement + restart durability).
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP3}
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
      redis:
        persistent: true
        selector:
          tier: integrated
YAML

# ===============================================================
# Phase 4: generate (2.4d) — the operator creates the ResourceClaim
#          and HOLDS the Application
# ===============================================================

phase "Phase 4: generate — ResourceClaim created, Application gated"

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.spec.type}' redis 180

claim_tier=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.spec.selector.tier}')
assert_eq "ResourceClaim selector.tier" "$claim_tier" "integrated"
claim_owner_kind=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" \
    '{.metadata.ownerReferences[0].kind}')
assert_eq "ResourceClaim ownerRef Kind" "$claim_owner_kind" "Application"

# NOTE — the gate (status.phase=AwaitingResourceClaim, ResourceClaimPending=
# True until the claim is ready) is deliberately NOT polled here. The shared
# Dragonfly instance lazy-boots fast and the $N ACL user is created over the
# Redis protocol in well under a poll interval, so an Application transitions
# AwaitingResourceClaim -> Ready faster than the poll can observe (a timing
# artifact of a fast cluster, not a product behaviour to assert). The gate is
# instead proven DETERMINISTICALLY by (a) claim GENERATION above — the
# operator emitted a ResourceClaim rather than rendering the Deployment
# immediately, the gate's load-bearing action — and (b) the Ready + env-ref
# resolution (Phase 7) assertions: the workload only receives its DSN once the
# claim is ready. The pause-while-unready logic itself is unit-tested in the
# operator. (Mirrors the needs-disk-walk note; the equivalent pg poll flaked
# the e2e-pg nightly when the backend was warm.)

# ===============================================================
# Phase 5: schedule (2.3) — the scheduler matches `redis-integrated`
# ===============================================================

phase "Phase 5: schedule — provider matched, Scheduled=True"

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.provider}' \
    "$PROVIDER" 120
sched_cond=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM" Scheduled)
assert_eq "ResourceClaim Scheduled condition" "$sched_cond" "True"

# ===============================================================
# Phase 6: provision (2.6-3/2.6-4) — lazy Dragonfly instance + $N ACL
#          user + connection Secret
# ===============================================================

phase "Phase 6: provision — lazy Dragonfly, dbnum alloc, \$N ACL user, Secret"

# Lazy instance boot is the slow step (Dragonfly pod comes up).
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.ready}' true 300

# The claim recorded its allocation: the shared instance + a numbered DB.
claim_instance=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.instance}')
assert_eq "ResourceClaim status.instance" "$claim_instance" "$DF_INSTANCE"
claim_dbnum=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.dbnum}')
case "$claim_dbnum" in
    [0-9]*) printf '  ok: ResourceClaim status.dbnum = %s\n' "$claim_dbnum" ;;
    *) printf 'ERROR: ResourceClaim status.dbnum not an integer: %q\n' "$claim_dbnum" >&2; exit 1 ;;
esac

# Exactly ONE ephemeral Dragonfly instance — the lazy-create is
# idempotent across claims sharing a persistence class.
df_count=$(kubectl -n "$DF_NS" get dragonfly.dragonflydb.io \
    "$DF_INSTANCE" --no-headers 2>/dev/null | wc -l | tr -d ' ')
assert_eq "ephemeral Dragonfly instance present (lazy-create)" "$df_count" "1"

# The per-instance admin Secret exists (read-or-created by the provisioner).
admin_present=$(jp secret "$DF_NS" "$DF_ADMIN_SECRET" '{.metadata.name}')
assert_eq "Dragonfly admin Secret present" "$admin_present" "$DF_ADMIN_SECRET"

# ---- ADR 0042 §9 baseline ---------------------------------------------
# Recorded HERE, while the pool instance is freshly provisioned and known
# good, because the reap phases are all statements ABOUT this object: that
# the thing which comes back is a different one (uid), that nothing rotated
# under it (admin password), and that its reservation actually left the node
# (memory).
EPH_UID_1=$(jp dragonfly.dragonflydb.io "$DF_NS" "$DF_INSTANCE" '{.metadata.uid}')
if [ -z "$EPH_UID_1" ]; then
    printf 'ERROR: could not read the ephemeral pool instance uid — the reap assertions identify the instance BY uid, not by name\n' >&2
    exit 1
fi
# The admin Secret is deliberately NOT deleted by the reaper, so the
# password must be byte-identical after a reap/re-create cycle. Compared as
# the raw base64 from `.data` — no decode, so this is a bytewise statement.
EPH_ADMIN_PW_1=$(jp secret "$DF_NS" "$DF_ADMIN_SECRET" '{.data.password}')
if [ -z "$EPH_ADMIN_PW_1" ]; then
    printf 'ERROR: admin Secret %s has no `password` key\n' "$DF_ADMIN_SECRET" >&2
    exit 1
fi
printf '  baseline: %s uid=%s\n' "$DF_INSTANCE" "$EPH_UID_1"

# The instance's own Guaranteed reservation — the number the reap must give
# back. Asserted rather than assumed so the node-level threshold below is
# tied to what this cluster actually runs.
EPH_POD_MEM_MI=$(pod_mem_requests_mi "$DF_NS" "${DF_INSTANCE}-0")
if [ "$EPH_POD_MEM_MI" -lt "$DRAGONFLY_MEM_MI" ]; then
    printf 'ERROR: pool instance pod %s requests %sMi of memory, expected at least %sMi (ADR 0053 seed) — the reap memory assertion is calibrated on that number\n' \
        "${DF_INSTANCE}-0" "$EPH_POD_MEM_MI" "$DRAGONFLY_MEM_MI" >&2
    exit 1
fi
printf '  baseline: %s-0 requests %sMi of memory\n' "$DF_INSTANCE" "$EPH_POD_MEM_MI"

NODE_NAME=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}')
NODE_MEM_BASELINE_MI=$(node_mem_requests_mi "$NODE_NAME")
printf '  baseline: node %s allocated memory requests = %sMi\n' \
    "$NODE_NAME" "$NODE_MEM_BASELINE_MI"

# The $N ACL user exists on the instance with the right DB selector.
acl_line=$(redis_admin ACL GETUSER "$ACL_USER" || true)
if [ -z "$acl_line" ]; then
    printf 'ERROR: ACL user %s not found on %s\n' "$ACL_USER" "$DF_INSTANCE" >&2
    redis_admin ACL LIST >&2 || true
    exit 1
fi
printf '  ok: ACL user %s exists on %s\n' "$ACL_USER" "$DF_INSTANCE"

# Connection Secret: status ref, DECOMPOSED keys (2.12 / ADR 0046 #3:
# url user pass host port db channelPrefix — the old composed
# REDIS_URL/REDIS_CHANNEL_PREFIX keys are dropped), ownerRef cascade.
conn_ref=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.connectionSecretRef}')
assert_eq "status.connectionSecretRef" "$conn_ref" "$CONN_SECRET"
conn_url=$(jp secret "$APP_NS" "$CONN_SECRET" '{.data.url}')
if [ -z "$conn_url" ]; then
    printf 'ERROR: connection Secret %s missing decomposed `url` key\n' "$CONN_SECRET" >&2
    exit 1
fi
conn_pfx=$(jp secret "$APP_NS" "$CONN_SECRET" '{.data.channelPrefix}')
if [ -z "$conn_pfx" ]; then
    printf 'ERROR: connection Secret %s missing decomposed `channelPrefix` key\n' "$CONN_SECRET" >&2
    exit 1
fi
printf '  ok: connection Secret %s carries decomposed `url` + `channelPrefix` keys\n' "$CONN_SECRET"
# The old composed keys must be GONE (2.4e REDIS_URL/REDIS_CHANNEL_PREFIX removed).
old_url=$(jp secret "$APP_NS" "$CONN_SECRET" '{.data.REDIS_URL}')
assert_eq "connection Secret has NO REDIS_URL key (2.4e dropped)" "$old_url" ""
old_pfx=$(jp secret "$APP_NS" "$CONN_SECRET" '{.data.REDIS_CHANNEL_PREFIX}')
assert_eq "connection Secret has NO REDIS_CHANNEL_PREFIX key (2.4e dropped)" "$old_pfx" ""
conn_owner_kind=$(jp secret "$APP_NS" "$CONN_SECRET" \
    '{.metadata.ownerReferences[0].kind}')
assert_eq "connection Secret ownerRef Kind" "$conn_owner_kind" "ResourceClaim"

# SSA-split guard: the provisioner's status write (ready /
# connectionSecretRef / Ready / instance / dbnum) must NOT have clobbered
# the scheduler's Scheduled=True. Re-assert it after provisioning.
sched_after=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM" Scheduled)
assert_eq "Scheduled still True after provision (SSA split)" "$sched_after" "True"

# ===============================================================
# Phase 7: resume + DSN (2.4d / 2.12) — Application Ready, env refs resolved
# ===============================================================

phase "Phase 7: resume — Application Ready, REDIS_URL + REDIS_CHANNEL_PREFIX claim refs resolved"

wait_jsonpath "$APP_RES" "$APP_NS" "$APP" '{.status.phase}' Ready 180

# 2.12 (ADR 0046 #8): the explicit `{claim: "redis.url"}` ref resolves to a
# secretKeyRef into the connection Secret, key `url` (the decomposed URL key,
# NOT the old composed `REDIS_URL` key). Same for `channelPrefix`.
env_secret=$(kubectl -n "$APP_NS" get deployment "$APP" \
    -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"REDIS_URL\")].valueFrom.secretKeyRef.name}" \
    2>/dev/null || true)
assert_eq "Deployment env REDIS_URL secretKeyRef.name" "$env_secret" "$CONN_SECRET"
env_key=$(kubectl -n "$APP_NS" get deployment "$APP" \
    -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"REDIS_URL\")].valueFrom.secretKeyRef.key}" \
    2>/dev/null || true)
assert_eq "Deployment env REDIS_URL secretKeyRef.key" "$env_key" "url"
env_pfx_secret=$(kubectl -n "$APP_NS" get deployment "$APP" \
    -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"REDIS_CHANNEL_PREFIX\")].valueFrom.secretKeyRef.name}" \
    2>/dev/null || true)
assert_eq "Deployment env REDIS_CHANNEL_PREFIX secretKeyRef.name" "$env_pfx_secret" "$CONN_SECRET"
env_pfx_key=$(kubectl -n "$APP_NS" get deployment "$APP" \
    -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"REDIS_CHANNEL_PREFIX\")].valueFrom.secretKeyRef.key}" \
    2>/dev/null || true)
assert_eq "Deployment env REDIS_CHANNEL_PREFIX secretKeyRef.key" "$env_pfx_key" "channelPrefix"

kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP}" --timeout=300s
printf '  Deployment %s -> Available\n' "$APP"

# ===============================================================
# Phase 8: isolation proof (ADR 0042 Pre-merge #3) — a SECOND claim's
#          user gets NOPERM on the FIRST claim's DB
# ===============================================================

phase "Phase 8: isolation proof — \$N ACL pins each user to its own DB"

# Wait for the second claim to provision onto the SAME ephemeral instance
# but a DIFFERENT dbnum.
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM2" '{.status.ready}' true 300
claim2_instance=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM2" '{.status.instance}')
assert_eq "second claim shares the ephemeral instance" "$claim2_instance" "$DF_INSTANCE"
claim2_dbnum=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM2" '{.status.dbnum}')
if [ "$claim2_dbnum" = "$claim_dbnum" ]; then
    printf 'ERROR: second claim got the SAME dbnum (%s) — allocator collision\n' "$claim2_dbnum" >&2
    exit 1
fi
printf '  ok: distinct DBs (web=%s, api=%s) on one instance\n' "$claim_dbnum" "$claim2_dbnum"

PW2=$(claim_password "$APP_NS" "$CONN_SECRET2")
# The api user can SELECT its OWN db (it is $N-pinned there).
own=$(redis_as "$ACL_USER2" "$PW2" -n "$claim2_dbnum" PING)
assert_eq "api user PINGs its own DB" "$own" "PONG"
# The api user is DENIED the web user's DB — the $N pin is a hard wall.
cross=$(redis_as "$ACL_USER2" "$PW2" SELECT "$claim_dbnum")
case "$cross" in
    *NOPERM*) printf '  ok: api user NOPERM on web DB %s (%s)\n' "$claim_dbnum" "$cross" ;;
    *) printf 'ERROR: api user was NOT denied web DB %s — isolation breach: %q\n' "$claim_dbnum" "$cross" >&2; exit 1 ;;
esac

# ---- Cross-DB escape probe (ADR 0042 Pre-merge #3): MOVE / COPY ... DB /
#      SWAPDB name a DESTINATION DB index as an argument and would smuggle
#      data across the $N pin if granted. The ACL denies MOVE/COPY explicitly
#      (-move -copy) and SWAPDB via @dangerous, so EACH must return a NOPERM /
#      permission error. The api user, pinned to its own DB, aims them at the
#      web user's DB (claim_dbnum). Any SUCCESS is an isolation breach → fail.
# First, seed a key in the api user's OWN DB so MOVE/COPY have a real source
# (a denial must come from the ACL, not from a missing key).
seed=$(redis_as "$ACL_USER2" "$PW2" -n "$claim2_dbnum" SET escape-probe v)
assert_eq "api seeds a key in its own DB (escape-probe source)" "$seed" "OK"
# assert_denied <label> <command-output> — fail the walk unless the reply is a
# NOPERM / permission / generic error (the command was DENIED, not executed).
assert_denied() {
    local label="$1" out="$2"
    case "$out" in
        *NOPERM*|*"not allowed"*|*"has no permissions"*|*error*|*ERR*)
            printf '  ok: %s denied (%s)\n' "$label" "$out" ;;
        *)
            printf 'ERROR: %s was NOT denied — cross-DB escape breach: %q\n' "$label" "$out" >&2
            exit 1 ;;
    esac
}
move_out=$(redis_as "$ACL_USER2" "$PW2" -n "$claim2_dbnum" MOVE escape-probe "$claim_dbnum")
assert_denied "MOVE escape-probe -> web DB $claim_dbnum" "$move_out"
copy_out=$(redis_as "$ACL_USER2" "$PW2" -n "$claim2_dbnum" \
    COPY escape-probe escape-probe2 DB "$claim_dbnum")
assert_denied "COPY escape-probe DB $claim_dbnum" "$copy_out"
swapdb_out=$(redis_as "$ACL_USER2" "$PW2" -n "$claim2_dbnum" SWAPDB "$claim2_dbnum" "$claim_dbnum")
assert_denied "SWAPDB $claim2_dbnum <-> $claim_dbnum" "$swapdb_out"

# ===============================================================
# Phase 9: scripting + client-init + restart re-pin
#          (ADR 0042 Pre-merge #4 / #5 / #6) — all on the ephemeral
#          instance's `web` user (its DSN password recovered once).
# ===============================================================

phase "Phase 9: Pre-merge #4/#5/#6 — EVAL confinement, client init, restart re-pin"

PW=$(claim_password "$APP_NS" "$CONN_SECRET")
if [ -z "$PW" ]; then
    printf 'ERROR: could not recover web claim password from %s (decomposed `pass` key)\n' "$CONN_SECRET" >&2
    exit 1
fi

# ---- Pre-merge #4: in-script EVAL cannot escape DB N -----------------
# (a) A declared-key EVAL inside DB N works — scripting (@scripting) is
#     retained for the claim user, confined to its own DB by the $N pin.
eval_ok=$(redis_as "$ACL_USER" "$PW" -n "$claim_dbnum" \
    EVAL "redis.call('SET', KEYS[1], 'v'); return redis.call('GET', KEYS[1])" \
    1 eval-probe)
assert_eq "web EVAL SET/GET a DECLARED key in its own DB" "$eval_ok" "v"

# (b) A script that tries to SELECT another DB is denied: the $N pin makes
#     SELECT of any DB but N a NOPERM, and the embedded interpreter runs
#     under the SAME ACL. The whole EVAL must error (not silently cross over).
eval_cross=$(redis_as "$ACL_USER" "$PW" -n "$claim_dbnum" \
    EVAL "return redis.call('SELECT', ARGV[1])" 0 "$claim2_dbnum")
case "$eval_cross" in
    *NOPERM*|*"not allowed"*|*"has no permissions"*|*error*|*ERR*)
        printf '  ok: in-script SELECT of DB %s denied for web (%s)\n' "$claim2_dbnum" "$eval_cross" ;;
    *)
        printf 'ERROR: in-script SELECT of DB %s was NOT denied — EVAL escaped DB %s: %q\n' \
            "$claim2_dbnum" "$claim_dbnum" "$eval_cross" >&2
        exit 1 ;;
esac

# (c) A script that reaches a key OUTSIDE its declared KEYS[] is rejected
#     (declared-keys default; `allow-undeclared-keys` stays unset — §5
#     invariant). The EVAL declares zero keys but touches one → error.
eval_undeclared=$(redis_as "$ACL_USER" "$PW" -n "$claim_dbnum" \
    EVAL "return redis.call('GET', 'undeclared-key')" 0)
case "$eval_undeclared" in
    *error*|*ERR*|*"not been declared"*|*"undeclared"*|*"Lua redis lib"*)
        printf '  ok: in-script undeclared-key access rejected (%s)\n' "$eval_undeclared" ;;
    *)
        printf 'ERROR: in-script undeclared-key access was NOT rejected: %q\n' "$eval_undeclared" >&2
        exit 1 ;;
esac

# ---- Pre-merge #5: ioredis / BullMQ-style client init under the ACL ---
# A real client (ioredis/node-redis) on connect issues a CLIENT SETNAME /
# SETINFO handshake and a health PING, and a BullMQ queue leans on
# blocking-list + scripting primitives. All of these must pass under the
# per-claim ACL (the args re-grant the safe CLIENT subcommands + retain
# @scripting). We DO NOT assert `CONFIG GET maxmemory-policy` succeeds:
# CONFIG is under -@admin/-@dangerous by design (ADR 0042 §5 — do not widen
# to +@dangerous); a well-behaved client tolerates that probe failing.
init_setname=$(redis_as "$ACL_USER" "$PW" -n "$claim_dbnum" CLIENT SETNAME bullmq-probe)
assert_eq "client CLIENT SETNAME (handshake)" "$init_setname" "OK"
init_setinfo=$(redis_as "$ACL_USER" "$PW" -n "$claim_dbnum" \
    CLIENT SETINFO lib-name ioredis)
assert_eq "client CLIENT SETINFO (handshake)" "$init_setinfo" "OK"
init_ping=$(redis_as "$ACL_USER" "$PW" -n "$claim_dbnum" PING)
assert_eq "client health PING" "$init_ping" "PONG"
# BullMQ-style queue ops: a blocking pop with a 0.1s timeout returns nil on
# an empty list (not NOPERM) — proves @list (incl. blocking) is granted.
init_blpop=$(redis_as "$ACL_USER" "$PW" -n "$claim_dbnum" BLPOP bull:probe 0.1)
case "$init_blpop" in
    *NOPERM*)
        printf 'ERROR: BLPOP denied under the ACL — queue workloads broken: %q\n' "$init_blpop" >&2
        exit 1 ;;
    *) printf '  ok: BLPOP permitted (empty-list nil reply: %q)\n' "$init_blpop" ;;
esac
# CONFIG GET is intentionally NOT granted — assert it IS denied (proves we
# did not over-widen the ACL to satisfy a client probe).
init_config=$(redis_as "$ACL_USER" "$PW" -n "$claim_dbnum" CONFIG GET maxmemory-policy)
case "$init_config" in
    *NOPERM*) printf '  ok: CONFIG GET stays denied (ACL not widened — §5)\n' ;;
    *) printf 'ERROR: CONFIG GET was permitted — ACL over-widened past §5: %q\n' "$init_config" >&2; exit 1 ;;
esac

# ---- ADR 0042 §11 (D17): cross-tenant metadata is denied ---------------
# INFO KEYSPACE emits a line per NON-EMPTY database regardless of the DB the
# connection selected, so with `+info` a tenant could enumerate which other
# tenants hold data and how much. There is no section- or DB-scoped grant.
info_reply=$(redis_as "$ACL_USER" "$PW" -n "$claim_dbnum" INFO KEYSPACE)
case "$info_reply" in
    *NOPERM*) printf '  ok: INFO denied — a tenant cannot enumerate other tenants key counts\n' ;;
    *) printf 'ERROR: INFO was permitted — every tenant can read every other tenant %s keyspace stats: %q\n' "'" "$info_reply" >&2; exit 1 ;;
esac
# PUBSUB is the larger leak and was not in the original finding: its output
# is NOT filtered by the user's `&{user}:*` patterns, and channel names carry
# the Kubernetes namespace and application name.
pubsub_reply=$(redis_as "$ACL_USER" "$PW" -n "$claim_dbnum" PUBSUB CHANNELS '*')
case "$pubsub_reply" in
    *NOPERM*) printf '  ok: PUBSUB CHANNELS denied — tenant identities are not enumerable\n' ;;
    *) printf 'ERROR: PUBSUB CHANNELS was permitted — it returns every pub/sub tenant namespace and app name: %q\n' "$pubsub_reply" >&2; exit 1 ;;
esac
# And pub/sub ITSELF must still work: PUBSUB is a distinct command from
# PUBLISH/SUBSCRIBE, so revoking it must not cost the feature.
pub_ok=$(redis_as "$ACL_USER" "$PW" -n "$claim_dbnum" PUBLISH "${ACL_USER}:probe" hello)
case "$pub_ok" in
    *NOPERM*) printf 'ERROR: PUBLISH on the tenant own channel prefix was denied — -pubsub took the feature with it: %q\n' "$pub_ok" >&2; exit 1 ;;
    *) printf '  ok: PUBLISH on the tenant own channel prefix still works (%q)\n' "$pub_ok" ;;
esac

# ---- Pre-merge #6: kill the Dragonfly pod -> reconcile loop re-pins the
#      user -> the app reconnects without NOPERM ------------------------
# 2.22f (ADR 0042 §10) REWROTE THIS PHASE. It used to poll the tenant's
# login for up to 420 seconds and accept eventual recovery, under a comment
# saying the outage window "is racy, so we do not assert it". That did not
# leave the defect uncovered — it CODIFIED it as passing. The walk saw a
# cluster-wide authentication outage twice per run and wrote the decision to
# tolerate it into the acceptance criterion.
#
# The assertion is now that authentication was NEVER lost: the ACL set is
# persisted to a file the instance loads at startup, so the tenant's user
# exists from the first moment the pod serves.
#
# THE OPERATOR IS SCALED TO ZERO BEFORE THE KILL, and that is what makes the
# assertion mean something. With the operator running, a fast re-pin could
# make a polling assertion pass while the file did nothing — the degenerate
# case. With it stopped, NOTHING can create the user at runtime, so a
# successful single-shot login can only come from the file.
redis_admin -n "$claim_dbnum" SET repin-marker present >/dev/null || true

# The loop must have written the file before we can test that it works.
printf '  waiting for the durable ACL file to carry the web user ...\n'
acl_deadline=$(( $(date +%s) + 420 ))
acl_ok=""
while [ "$(date +%s)" -lt "$acl_deadline" ]; do
    body=$(kubectl -n "$DF_NS" get secret "${DF_INSTANCE}-acl" \
        -o jsonpath='{.data.acl}' 2>/dev/null | base64 -d 2>/dev/null || true)
    if printf '%s' "$body" | grep -q "^USER ${ACL_USER} "; then acl_ok="yes"; break; fi
    sleep 10
done
if [ "$acl_ok" != "yes" ]; then
    printf 'ERROR: the ACL file never carried USER %s — durability was never established, so the restart assertion below would prove nothing\n' "$ACL_USER" >&2
    kubectl -n "$DF_NS" get secret "${DF_INSTANCE}-acl" -o yaml >&2 2>&1 || true
    exit 1
fi
printf '  ok: durable ACL file carries USER %s\n' "$ACL_USER"

# 2.22d / D8: the SAMPLED figure for redis is a KEY COUNT, never bytes, and
# the CLI is required to say so. Per-database bytes are genuinely unreachable
# in Dragonfly (verified against v1.37.0 and `main`: the per-DB figures are
# summed away at every point they could reach a client), so the honest number
# is DBSIZE. Sampled on the 300s ACL resync loop, hence the wide window.
#
# Asserted here for the first time: until now nothing in any walk read
# `status.size`, so an inert sampler was indistinguishable from a working one.
printf '  waiting for the sampled key count to land (300s resync loop) ...\n'
_keys_deadline=$(( $(date +%s) + 420 ))
_keys=""; _kmeasured=""; _kbytes=""
while [ "$(date +%s)" -lt "$_keys_deadline" ]; do
    _keys=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.size.keys}')
    _kmeasured=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.size.measuredAt}')
    [ -n "$_keys" ] && break
    sleep 15
done
case "$_keys" in
    ''|*[!0-9]*) printf 'ERROR: status.size.keys never became a number (last=%q) — the D8 redis sampler is not reaching the claim\n' "$_keys" >&2; exit 1 ;;
esac
# NEGATIVE: bytes must stay unset for redis. A bytes figure here would be a
# fabricated number, which is worse than no number.
_kbytes=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.size.bytes}')
[ -z "$_kbytes" ] || { printf 'ERROR: status.size.bytes is set for a redis claim (%q) — per-database bytes are not obtainable from Dragonfly, so any value here is invented\n' "$_kbytes" >&2; exit 1; }
case "$_kmeasured" in
    [0-9][0-9][0-9][0-9]-*) printf '  ok: sampled key count = %s keys (bytes correctly unset), measured at %s\n' "$_keys" "$_kmeasured" ;;
    *) printf 'ERROR: status.size.measuredAt is not RFC3339 (%q)\n' "$_kmeasured" >&2; exit 1 ;;
esac

# The file is only loaded if the CR points at it.
acl_ref=$(kubectl -n "$DF_NS" get dragonfly "$DF_INSTANCE" \
    -o jsonpath='{.spec.aclFromSecret.name}' 2>/dev/null || true)
if [ "$acl_ref" = "${DF_INSTANCE}-acl" ]; then
    printf '  ok: Dragonfly CR loads %s at startup\n' "$acl_ref"
else
    printf 'ERROR: spec.aclFromSecret is %q, expected %s — the file exists but nothing reads it\n' "$acl_ref" "${DF_INSTANCE}-acl" >&2
    exit 1
fi

# The default line must be present. Its ABSENCE is not a lockout — it makes
# the loaded default `nopass +@all ~* &*` and turns authentication OFF on the
# instance serving every tenant. Assert it in the file before asserting it on
# the wire below.
if printf '%s' "$body" | grep -q '^USER default on >'; then
    printf '  ok: the ACL file carries a well-formed default line\n'
else
    printf 'ERROR: the ACL file has no `USER default on >` line — loading it would DISABLE authentication on this instance\n' >&2
    exit 1
fi

printf '  scaling the operator to 0 so nothing can re-pin at runtime ...\n'
kubectl -n apprafter-system scale deploy/apprafter-operator --replicas=0
kubectl -n apprafter-system wait --for=delete pod \
    -l app.kubernetes.io/name=apprafter-operator --timeout=120s 2>/dev/null || true
OLD_DF_UID=$(kubectl -n "$DF_NS" get pod "${DF_INSTANCE}-0" -o jsonpath='{.metadata.uid}' 2>/dev/null || true)

printf '  killing the ephemeral Dragonfly pod (wipes runtime ACL users) ...\n'
# Delete the StatefulSet pod by its deterministic name and WAIT for it to
# fully terminate (default --wait), so the readiness wait below targets the
# freshly recreated pod instead of racing the old pod's brief
# Ready-while-Terminating / Succeeded window (which yields "cannot exec into a
# completed pod" when the re-pin poll execs in).
kubectl -n "$DF_NS" delete pod "${DF_INSTANCE}-0" --ignore-not-found 2>/dev/null || true

# Wait for the instance to come back Ready (a fresh pod, empty ACL table).
printf '  waiting for the ephemeral instance to roll back to Ready ...\n'
# The Dragonfly StatefulSet uses the OnDelete update strategy, so
# `kubectl rollout status` is unavailable; wait on the pod readiness
# directly (`retry` rides out the gap while the killed pod recreates).
retry 40 10 -- kubectl -n "$DF_NS" wait --for=condition=Ready \
    "pod/${DF_INSTANCE}-0" --timeout=30s

# The pod really is a different one — otherwise everything below would be
# asserting against a process that never lost its in-memory ACL table.
NEW_DF_UID=$(kubectl -n "$DF_NS" get pod "${DF_INSTANCE}-0" -o jsonpath='{.metadata.uid}' 2>/dev/null || true)
if [ -n "$OLD_DF_UID" ] && [ "$OLD_DF_UID" = "$NEW_DF_UID" ]; then
    printf 'ERROR: the Dragonfly pod uid did not change (%s) — it never restarted, so nothing below is evidence\n' "$OLD_DF_UID" >&2
    kubectl -n apprafter-system scale deploy/apprafter-operator --replicas=1 || true
    exit 1
fi
printf '  ok: the instance is a fresh process (uid %s -> %s)\n' "${OLD_DF_UID:0:8}" "${NEW_DF_UID:0:8}"

# SINGLE SHOT, no poll. The operator is stopped, so nothing can create this
# user at runtime; a PONG here can only come from the file the instance
# loaded at startup. A poll would reintroduce exactly the "eventual recovery"
# criterion this phase was rewritten to remove.
got=$(redis_as "$ACL_USER" "$PW" -n "$claim_dbnum" PING)
if [ "$got" = "PONG" ]; then
    printf '  ok: the web user authenticated on the FIRST attempt after the restart — the ACL survived it\n'
else
    printf 'ERROR: the web user could not authenticate immediately after the restart (got=%q) — the ACL file did not survive the restart, which is the whole of D5\n' "$got" >&2
    kubectl -n "$DF_NS" exec "${DF_INSTANCE}-0" -- sh -c 'cat /var/lib/dragonfly/dragonfly.acl' >&2 2>&1 || true
    kubectl -n apprafter-system scale deploy/apprafter-operator --replicas=1 || true
    exit 1
fi

# THE DEFAULT-USER TRIAD. A file that parses but omits `USER default` yields
# an ACTIVE nopass superuser with authentication disabled — on the instance
# serving every tenant in the cluster. Assert the boundary in both
# directions, on the wire, not just in the file.
noauth=$(kubectl -n "$DF_NS" exec "${DF_INSTANCE}-0" -- \
    redis-cli --no-auth-warning PING 2>&1 || true)
case "$noauth" in
    *NOAUTH*|*Authentication*|*ERR*) printf '  ok: unauthenticated access is refused (%s)\n' "$(printf '%s' "$noauth" | head -c 40)" ;;
    *) printf 'ERROR: an UNAUTHENTICATED client got %q — the ACL file disabled authentication on a shared instance\n' "$noauth" >&2
       kubectl -n apprafter-system scale deploy/apprafter-operator --replicas=1 || true
       exit 1 ;;
esac
wrongpw=$(redis_as default "definitely-not-the-admin-password" PING)
case "$wrongpw" in
    PONG) printf 'ERROR: the default user accepted a WRONG password — nopass is in effect\n' >&2
          kubectl -n apprafter-system scale deploy/apprafter-operator --replicas=1 || true
          exit 1 ;;
    *) printf '  ok: the default user rejects a wrong password\n' ;;
esac
if [ "$(redis_admin PING)" = "PONG" ]; then
    printf '  ok: the admin password still works — the file did not lock the operator out\n'
else
    printf 'ERROR: the admin password no longer works — the default line is malformed (missing `on`?)\n' >&2
    kubectl -n apprafter-system scale deploy/apprafter-operator --replicas=1 || true
    exit 1
fi

printf '  restoring the operator ...\n'
kubectl -n apprafter-system scale deploy/apprafter-operator --replicas=1
kubectl -n apprafter-system rollout status deploy/apprafter-operator --timeout=180s

# And the re-pinned user is still confined to its own DB (the re-pin is the
# SAME $N-scoped grant, not a widened one).
recross=$(redis_as "$ACL_USER" "$PW" SELECT "$claim2_dbnum")
case "$recross" in
    *NOPERM*) printf '  ok: re-pinned web user still NOPERM on DB %s (pin preserved)\n' "$claim2_dbnum" ;;
    *) printf 'ERROR: re-pinned web user is NOT confined to its DB — re-pin widened the grant: %q\n' "$recross" >&2; exit 1 ;;
esac

# ===============================================================
# Phase 10: persistent variant (ADR 0042 §6) — separate persistent pool
#           instance + data survives a pod restart (snapshot->PVC)
# ===============================================================

phase "Phase 10: persistent variant — separate instance + restart-durable data"

# The `worker` claim (needs.redis: {persistent: true}) provisions onto the
# PERSISTENT pool instance, not the ephemeral one.
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM3" '{.status.ready}' true 300
claim3_instance=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM3" '{.status.instance}')
assert_eq "persistent claim lands on the persistent instance" \
    "$claim3_instance" "$DF_INSTANCE_P"
claim3_dbnum=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM3" '{.status.dbnum}')
case "$claim3_dbnum" in
    [0-9]*) printf '  ok: persistent claim status.dbnum = %s\n' "$claim3_dbnum" ;;
    *) printf 'ERROR: persistent claim status.dbnum not an integer: %q\n' "$claim3_dbnum" >&2; exit 1 ;;
esac

# The persistent Dragonfly CR exists AND carries a snapshot->PVC block
# (whole-instance durability) — the ephemeral one does not.
df_p_count=$(kubectl -n "$DF_NS" get dragonfly.dragonflydb.io \
    "$DF_INSTANCE_P" --no-headers 2>/dev/null | wc -l | tr -d ' ')
assert_eq "persistent Dragonfly instance present (separate from ephemeral)" \
    "$df_p_count" "1"
df_p_pvc=$(jp dragonfly.dragonflydb.io "$DF_NS" "$DF_INSTANCE_P" \
    '{.spec.snapshot.persistentVolumeClaimSpec.accessModes[0]}')
assert_eq "persistent Dragonfly CR has a snapshot->PVC block" "$df_p_pvc" "ReadWriteOnce"
df_e_pvc=$(jp dragonfly.dragonflydb.io "$DF_NS" "$DF_INSTANCE" \
    '{.spec.snapshot.persistentVolumeClaimSpec}')
if [ -n "$df_e_pvc" ]; then
    printf 'ERROR: the EPHEMERAL instance unexpectedly carries a snapshot->PVC block: %q\n' "$df_e_pvc" >&2
    exit 1
fi
printf '  ok: ephemeral instance has no snapshot block (class split holds)\n'

# Its own admin Secret + $N ACL user exist on the persistent instance.
admin_p_present=$(jp secret "$DF_NS" "$DF_ADMIN_SECRET_P" '{.metadata.name}')
assert_eq "persistent-instance admin Secret present" "$admin_p_present" "$DF_ADMIN_SECRET_P"
acl_p_line=$(redis_admin_on "$DF_INSTANCE_P" "$DF_ADMIN_SECRET_P" ACL GETUSER "$ACL_USER3" || true)
if [ -z "$acl_p_line" ]; then
    printf 'ERROR: ACL user %s not found on the persistent instance %s\n' "$ACL_USER3" "$DF_INSTANCE_P" >&2
    redis_admin_on "$DF_INSTANCE_P" "$DF_ADMIN_SECRET_P" ACL LIST >&2 || true
    exit 1
fi
printf '  ok: ACL user %s exists on the persistent instance\n' "$ACL_USER3"

# ---- restart-durability: persistent: true survives a pod restart -------
# Write a durable key into the worker DB, force a snapshot (SAVE), then kill
# the persistent pod. The dragonfly-operator restores from the snapshot
# PVC, so the key survives — the acceptance criterion for `persistent`.
PW3=$(claim_password "$APP_NS" "$CONN_SECRET3")
if [ -z "$PW3" ]; then
    printf 'ERROR: could not recover worker claim password from %s\n' "$CONN_SECRET3" >&2
    exit 1
fi
worker_set=$(redis_as_on "$DF_INSTANCE_P" "$ACL_USER3" "$PW3" -n "$claim3_dbnum" \
    SET durable-key survives-restart)
assert_eq "worker SET a durable key in its own DB" "$worker_set" "OK"
# Force a snapshot to disk so the restart restores it (admin SAVE — the
# claim user lacks @admin; this is the platform proving durability).
redis_admin_on "$DF_INSTANCE_P" "$DF_ADMIN_SECRET_P" SAVE >/dev/null 2>&1 || true

# Same control as the ephemeral phase: the loop must have persisted this
# instance's users first, and the operator must be STOPPED, or a runtime
# re-pin could make the assertion below pass while the file did nothing.
printf '  waiting for the persistent instance durable ACL file to carry the worker ...\n'
acl_p_deadline=$(( $(date +%s) + 420 ))
acl_p_ok=""
while [ "$(date +%s)" -lt "$acl_p_deadline" ]; do
    body_p=$(kubectl -n "$DF_NS" get secret "${DF_INSTANCE_P}-acl" \
        -o jsonpath='{.data.acl}' 2>/dev/null | base64 -d 2>/dev/null || true)
    if printf '%s' "$body_p" | grep -q "^USER ${ACL_USER3} "; then acl_p_ok="yes"; break; fi
    sleep 10
done
if [ "$acl_p_ok" != "yes" ]; then
    printf 'ERROR: the persistent instance ACL file never carried USER %s\n' "$ACL_USER3" >&2
    exit 1
fi
printf '  ok: persistent instance ACL file carries USER %s\n' "$ACL_USER3"

printf '  scaling the operator to 0 so nothing can re-pin at runtime ...\n'
kubectl -n apprafter-system scale deploy/apprafter-operator --replicas=0
kubectl -n apprafter-system wait --for=delete pod \
    -l app.kubernetes.io/name=apprafter-operator --timeout=120s 2>/dev/null || true

printf '  killing the persistent Dragonfly pod (data must survive) ...\n'
kubectl -n "$DF_NS" delete pod "${DF_INSTANCE_P}-0" --ignore-not-found 2>/dev/null || true

printf '  waiting for the persistent instance to roll back to Ready ...\n'
# OnDelete StatefulSet → wait on the pod directly (see the ephemeral path).
retry 40 10 -- kubectl -n "$DF_NS" wait --for=condition=Ready \
    "pod/${DF_INSTANCE_P}-0" --timeout=30s

# 2.22f: SINGLE SHOT, no poll — same rewrite as the ephemeral phase. The
# 420s poll here tolerated the same outage, and on a PERSISTENT instance it
# is the sharpest form of the defect: the keyspace comes back from the
# snapshot and the tenant is locked out of its own intact data. If the ACL
# file works, the user exists from the first moment the pod serves.
printf '  reading the durable key as the worker, immediately after the restart ...\n'
got=$(redis_as_on "$DF_INSTANCE_P" "$ACL_USER3" "$PW3" -n "$claim3_dbnum" GET durable-key)
case "$got" in
    survives-restart)
        printf '  ok: persistent data survived AND the worker authenticated on the first attempt\n' ;;
    *NOPERM*|*WRONGPASS*)
        printf 'ERROR: the worker could not authenticate immediately after the restart (got=%q) — the data survived and the tenant is locked out of it, which is the sharpest form of D5\n' "$got" >&2
        kubectl -n "$DF_NS" exec "${DF_INSTANCE_P}-0" -- sh -c 'cat /var/lib/dragonfly/dragonfly.acl' >&2 2>&1 || true
        kubectl -n apprafter-system scale deploy/apprafter-operator --replicas=1 || true
        exit 1 ;;
    *)
        printf 'ERROR: durable-key did NOT survive the persistent pod restart (got=%q) — persistence broken\n' "$got" >&2
        kubectl -n apprafter-system scale deploy/apprafter-operator --replicas=1 || true
        exit 1 ;;
esac

printf '  restoring the operator ...\n'
kubectl -n apprafter-system scale deploy/apprafter-operator --replicas=1
kubectl -n apprafter-system rollout status deploy/apprafter-operator --timeout=180s

# ===============================================================
# Phase 11: delete + snapshot (2.4f/2.6-4) — RetainedClaim (dragonfly), cascade
# ===============================================================

phase "Phase 11: delete claim — RetainedClaim snapshot + cascade"

# Delete the APPLICATION, not just the ResourceClaim: the claim is owned by
# the Application (ownerRef), so deleting the claim alone while the app still
# declares needs.redis makes the Application controller regenerate it, and the
# provisioner correctly REATTACHES it to its retained allocation (ADR 0042 §8),
# cancelling the RetainedClaim — so the snapshot never persists for the GC to
# act on. Deleting the app cascades to the claim with no regeneration.
kubectl delete "$APP_RES" "$APP" -n "$APP_NS" --wait=true

# The finalizer snapshots a dragonfly-shaped RetainedClaim.
wait_jsonpath retainedclaim "$RETAINED_NS" "$RETAINED" \
    '{.spec.claimRef.name}' "$CLAIM" 120
rc_backend=$(jp retainedclaim "$RETAINED_NS" "$RETAINED" '{.spec.backend}')
assert_eq "RetainedClaim spec.backend" "$rc_backend" "dragonfly"
rc_instance=$(jp retainedclaim "$RETAINED_NS" "$RETAINED" '{.spec.instance}')
assert_eq "RetainedClaim spec.instance" "$rc_instance" "$DF_INSTANCE"
rc_acl=$(jp retainedclaim "$RETAINED_NS" "$RETAINED" '{.spec.aclUser}')
assert_eq "RetainedClaim spec.aclUser" "$rc_acl" "$ACL_USER"
rc_retain=$(jp retainedclaim "$RETAINED_NS" "$RETAINED" '{.spec.retainUntil}')
# RFC3339: a non-empty timestamp starting with a 4-digit year.
case "$rc_retain" in
    [0-9][0-9][0-9][0-9]-*) printf '  ok: RetainedClaim retainUntil = %s\n' "$rc_retain" ;;
    *) printf 'ERROR: RetainedClaim retainUntil not RFC3339: %q\n' "$rc_retain" >&2; exit 1 ;;
esac

# The connection Secret cascades (ownerRef → ResourceClaim).
wait_gone secret "$APP_NS" "$CONN_SECRET" 120

# The grace floor holds: the ACL user survives the delete (GC has NOT
# fired — retainUntil is days away).
acl_floor=$(redis_admin ACL GETUSER "$ACL_USER" || true)
if [ -z "$acl_floor" ]; then
    printf 'ERROR: ACL user %s dropped DURING grace — GC fired early\n' "$ACL_USER" >&2
    exit 1
fi
printf '  ok: ACL user %s STILL present during grace\n' "$ACL_USER"

# ===============================================================
# Phase 12: force GC (2.6-7) — re-create RetainedClaim with a past
#           retainUntil; the GC runs FLUSHDB + ACL DELUSER
# ===============================================================

phase "Phase 12: force GC — past retainUntil drops the ACL user + flushes the DB"

# Seed a key into the web DB as the admin so we can prove FLUSHDB later.
redis_admin -n "$claim_dbnum" SET gc-probe present >/dev/null || true

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
  backend: dragonfly
  instance: ${DF_INSTANCE}
  dbnum: ${claim_dbnum}
  aclUser: ${ACL_USER}
  connectionSecretRef: ${CONN_SECRET}
  connectionSecretNamespace: ${APP_NS}
  retainUntil: "2000-01-01T00:00:00Z"
YAML

# The snapshot is deleted once the GC completes.
wait_gone retainedclaim "$RETAINED_NS" "$RETAINED" 180

# ACL DELUSER ran — the user is physically gone from the instance.
printf '  waiting for the ACL user to be dropped ...\n'
deadline=$(( $(date +%s) + 120 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    left=$(redis_admin ACL GETUSER "$ACL_USER" || true)
    [ -z "$left" ] && break
    sleep 5
done
left=$(redis_admin ACL GETUSER "$ACL_USER" || true)
if [ -n "$left" ]; then
    printf 'ERROR: ACL user %s STILL present after GC — DELUSER did not run\n' "$ACL_USER" >&2
    exit 1
fi
printf '  ok: ACL user %s is physically gone (ACL DELUSER ran)\n' "$ACL_USER"

# FLUSHDB ran — the web DB is empty (the gc-probe key is gone).
probe=$(redis_admin -n "$claim_dbnum" GET gc-probe || true)
if [ -n "$probe" ]; then
    printf 'ERROR: web DB %s NOT flushed after GC — gc-probe=%q\n' "$claim_dbnum" "$probe" >&2
    exit 1
fi
printf '  ok: web DB %s is empty (FLUSHDB ran)\n' "$claim_dbnum"

# The second claim's user is UNTOUCHED — the GC only reclaimed web's DB.
api_still=$(redis_admin ACL GETUSER "$ACL_USER2" || true)
if [ -z "$api_still" ]; then
    printf 'ERROR: GC of web also dropped api user %s — over-broad reclaim\n' "$ACL_USER2" >&2
    exit 1
fi
printf '  ok: api user %s untouched by web GC (scoped reclaim)\n' "$ACL_USER2"

# ===============================================================
# Phase 13: ephemeral reap (ADR 0042 §9.1/§9.7) — the last tenant leaves
#           and the pool instance is given back, snapshot notwithstanding
# ===============================================================

phase "Phase 13: ephemeral reap — instance gone, RetainedClaim + admin Secret kept"

EPH_REAPED_BEFORE=$(reap_metric dragonfly-ephemeral reaped)

# The node reading the assertion below subtracts from. NOT the Phase-6
# baseline, deliberately: the class-split phase brings up the PERSISTENT
# pool instance with its own 320Mi reservation somewhere between the two,
# so a delta measured from Phase 6 would be racing that phase rather than
# measuring this reap. The Phase-6 number is printed alongside for context.
NODE_MEM_PRE_REAP_MI=$(node_mem_requests_mi "$NODE_NAME")
printf '  node %s allocated memory requests: %sMi (Phase-6 baseline was %sMi)\n' \
    "$NODE_NAME" "$NODE_MEM_PRE_REAP_MI" "$NODE_MEM_BASELINE_MI"

# `api` is the ephemeral instance's last tenant (`web` went in Phase 11 and
# its slot was reclaimed in Phase 12). Deleting the APPLICATION cascades to
# the claim; the claim's finalizer snapshots a RetainedClaim.
kubectl delete "$APP_RES" "$APP2" -n "$APP_NS" --wait=true
wait_jsonpath retainedclaim "$RETAINED_NS" "$RETAINED2" \
    '{.spec.claimRef.name}' "$CLAIM2" 120

# Timeout arithmetic: the sweep ticks every 60s, so the first tick after the
# last claim goes only STARTS the dwell and the reap lands on the next one —
# up to 2 ticks (~120s) plus the dwell itself. dwell + 180 keeps a real
# margin over that; it is a timeout, not a threshold, so slack here weakens
# nothing.
wait_gone dragonfly.dragonflydb.io "$DF_NS" "$DF_INSTANCE" \
    $(( REAP_DWELL_SECS + 180 ))

# The WORKLOAD is gone too, not just the CR. A reaper that deleted the
# Dragonfly while the operator left the StatefulSet behind would reclaim
# nothing, and the CR-level assertion above would not notice.
wait_gone statefulset "$DF_NS" "$DF_INSTANCE" 180
wait_gone pod "$DF_NS" "${DF_INSTANCE}-0" 180

# §9.7 — the snapshot is STILL THERE, and the instance went anyway. This is
# the whole point of the ephemeral fast path: a RetainedClaim naming an
# ephemeral instance has no data to reattach to, so holding a 320Mi pod for
# the full 7-day grace would buy nothing. (The claim's DB NUMBER stays
# reserved for the grace either way — that is the slot, not the instance.)
retained_after=$(jp retainedclaim "$RETAINED_NS" "$RETAINED2" '{.metadata.name}')
assert_eq "RetainedClaim survives the ephemeral reap (§9.7 asymmetry)" \
    "$retained_after" "$RETAINED2"

# The admin Secret is a deliberate keep: deleting it would make the next
# provision mint a FRESH admin password while the old pod may still be
# terminating behind the Service.
admin_after=$(jp secret "$DF_NS" "$DF_ADMIN_SECRET" '{.metadata.name}')
assert_eq "admin Secret survives the ephemeral reap" \
    "$admin_after" "$DF_ADMIN_SECRET"

# ---- THE NUMERIC ASSERTION --------------------------------------------
# The reservation actually left the node. If this number does not move, the
# feature did not work regardless of what the CRs say — which is exactly the
# failure a reaper that deletes the CR while its StatefulSet lingers would
# produce.
NODE_MEM_POST_REAP_MI=$(node_mem_requests_mi "$NODE_NAME")
mem_drop=$(( NODE_MEM_PRE_REAP_MI - NODE_MEM_POST_REAP_MI ))
if [ "$mem_drop" -lt "$DRAGONFLY_MEM_MI" ]; then
    printf 'ERROR: node %s allocated memory requests fell only %sMi (%sMi -> %sMi), want at least %sMi — the reaped instance held %sMi, so its reservation did NOT come back\n' \
        "$NODE_NAME" "$mem_drop" "$NODE_MEM_PRE_REAP_MI" "$NODE_MEM_POST_REAP_MI" \
        "$DRAGONFLY_MEM_MI" "$EPH_POD_MEM_MI" >&2
    kubectl -n "$DF_NS" get pods -o wide >&2 2>&1 || true
    exit 1
fi
printf '  ok: node allocated memory requests fell %sMi (%sMi -> %sMi, >= %sMi)\n' \
    "$mem_drop" "$NODE_MEM_PRE_REAP_MI" "$NODE_MEM_POST_REAP_MI" "$DRAGONFLY_MEM_MI"

eph_reaped_after=$(reap_metric dragonfly-ephemeral reaped)
if [ "$eph_reaped_after" -le "$EPH_REAPED_BEFORE" ]; then
    printf 'ERROR: dragonfly-ephemeral reaped counter did not move (%s -> %s) — the instance went away for some reason OTHER than the reaper\n' \
        "$EPH_REAPED_BEFORE" "$eph_reaped_after" >&2
    exit 1
fi
printf '  ok: dragonfly-ephemeral reaped %s -> %s (the reaper is what removed it)\n' \
    "$EPH_REAPED_BEFORE" "$eph_reaped_after"

# ===============================================================
# Phase 14: clean re-create — a NEW instance, the SAME admin credential
# ===============================================================

phase "Phase 14: re-create — new pool instance, unrotated admin password"

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
      redis:
        selector:
          tier: integrated
YAML

# A full lazy re-create (Dragonfly CR + pod boot + ACL user), so budget more
# than the first provision did.
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM2" '{.status.ready}' true 600

EPH_UID_2=$(jp dragonfly.dragonflydb.io "$DF_NS" "$DF_INSTANCE" '{.metadata.uid}')
if [ -z "$EPH_UID_2" ] || [ "$EPH_UID_2" = "$EPH_UID_1" ]; then
    printf 'ERROR: the pool instance uid is %q, want a NEW uid different from the reaped %q — the instance was never actually deleted, so Phase 13 proved nothing\n' \
        "$EPH_UID_2" "$EPH_UID_1" >&2
    exit 1
fi
printf '  ok: re-created pool instance is a NEW object (uid %s -> %s)\n' \
    "$EPH_UID_1" "$EPH_UID_2"

# Nothing rotated: the admin Secret was kept, so the credential the
# provisioner drives ACL with is bit-for-bit the one from Phase 6.
admin_pw_2=$(jp secret "$DF_NS" "$DF_ADMIN_SECRET" '{.data.password}')
assert_eq "admin password is byte-identical after the reap round-trip" \
    "$admin_pw_2" "$EPH_ADMIN_PW_1"

kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP2}" --timeout=300s
printf '  Deployment %s -> Available on the re-created pool instance\n' "$APP2"

# ===============================================================
# Phase 15: negative test — a pool instance with a LIVE tenant is
#           never reaped (ADR 0042 §9.1 ALLOCATED)
# ===============================================================

phase "Phase 15: negative test — 3x dwell with a live tenant, nothing is reaped"

OP_POD_BEFORE=$(operator_pod)
EPH_REAPED_BEFORE_NEG=$(reap_metric dragonfly-ephemeral reaped)
EPH_VETO_LIVE_BEFORE_NEG=$(reap_metric dragonfly-ephemeral veto_live)

printf '  sleeping %ss (3x dwell) with the api claim live ...\n' \
    "$(( REAP_DWELL_SECS * 3 ))"
sleep $(( REAP_DWELL_SECS * 3 ))

# Counters are per-pod and reset on restart, so a restart here would make
# "did not increase" true for the wrong reason.
OP_POD_AFTER=$(operator_pod)
assert_eq "the operator pod did not restart during the negative test" \
    "$OP_POD_AFTER" "$OP_POD_BEFORE"

uid_after_neg=$(jp dragonfly.dragonflydb.io "$DF_NS" "$DF_INSTANCE" '{.metadata.uid}')
assert_eq "the live pool instance kept its identity across 3x dwell" \
    "$uid_after_neg" "$EPH_UID_2"

eph_reaped_after_neg=$(reap_metric dragonfly-ephemeral reaped)
assert_eq "no ephemeral reap fired while a tenant was live" \
    "$eph_reaped_after_neg" "$EPH_REAPED_BEFORE_NEG"

# And it was VETOED, not merely skipped: without this, a reaper that had
# died would pass the assertion above for exactly the wrong reason.
eph_veto_live_after_neg=$(reap_metric dragonfly-ephemeral veto_live)
if [ "$eph_veto_live_after_neg" -le "$EPH_VETO_LIVE_BEFORE_NEG" ]; then
    printf 'ERROR: dragonfly-ephemeral veto_live did not move (%s -> %s) across 3x dwell — the reaper was not evaluating the instance at all, so "nothing was reaped" says nothing\n' \
        "$EPH_VETO_LIVE_BEFORE_NEG" "$eph_veto_live_after_neg" >&2
    exit 1
fi
printf '  ok: dragonfly-ephemeral veto_live %s -> %s (evaluated every tick, vetoed every time)\n' \
    "$EPH_VETO_LIVE_BEFORE_NEG" "$eph_veto_live_after_neg"

# ===============================================================
# Phase 16: persistent arm (ADR 0042 §9.2/§9.5/§9.7) — a snapshot HOLDS
#           the instance; post-grace it is reaped with its PVC intact
#           and the re-provision adopts the SAME volume
# ===============================================================

phase "Phase 16: persistent arm — RETAINED holds, post-grace reap preserves the PVC"

PER_UID_1=$(jp dragonfly.dragonflydb.io "$DF_NS" "$DF_INSTANCE_P" '{.metadata.uid}')
if [ -z "$PER_UID_1" ]; then
    printf 'ERROR: could not read the persistent pool instance uid\n' >&2
    exit 1
fi

# The snapshot PVC — discovered, not assumed: the dragonfly-operator derives
# its name from its own volumeClaimTemplate plus the StatefulSet ordinal.
DF_PVC=$(kubectl -n "$DF_NS" get pvc --no-headers \
    -o custom-columns=NAME:.metadata.name 2>/dev/null \
    | grep -- "$DF_INSTANCE_P" | head -1 || true)
if [ -z "$DF_PVC" ]; then
    printf 'ERROR: no PVC matching %s in %s — the persistent arm has no volume to assert on\n' \
        "$DF_INSTANCE_P" "$DF_NS" >&2
    kubectl -n "$DF_NS" get pvc >&2 2>&1 || true
    exit 1
fi
DF_PV_1=$(jp pvc "$DF_NS" "$DF_PVC" '{.spec.volumeName}')
df_pvc_phase=$(jp pvc "$DF_NS" "$DF_PVC" '{.status.phase}')
assert_eq "snapshot PVC ${DF_PVC} is Bound (baseline)" "$df_pvc_phase" "Bound"
printf '  baseline: %s uid=%s, PVC %s -> PV %s\n' \
    "$DF_INSTANCE_P" "$PER_UID_1" "$DF_PVC" "$DF_PV_1"

PER_REAPED_BEFORE=$(reap_metric dragonfly-persistent reaped)
PER_VETO_RETAINED_BEFORE=$(reap_metric dragonfly-persistent veto_retained)

# Remove the tenant. Unlike the ephemeral arm, the snapshot it leaves behind
# DOES veto: a persistent instance holds data a recreate-within-grace can
# reattach to.
kubectl delete "$APP_RES" "$APP3" -n "$APP_NS" --wait=true
wait_jsonpath retainedclaim "$RETAINED_NS" "$RETAINED3" \
    '{.spec.claimRef.name}' "$CLAIM3" 120

printf '  waiting for a veto_retained tick on the dragonfly-persistent arm ...\n'
deadline=$(( $(date +%s) + 240 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    per_veto_now=$(reap_metric dragonfly-persistent veto_retained)
    [ "$per_veto_now" -gt "$PER_VETO_RETAINED_BEFORE" ] && break
    sleep 10
done
per_veto_now=$(reap_metric dragonfly-persistent veto_retained)
if [ "$per_veto_now" -le "$PER_VETO_RETAINED_BEFORE" ]; then
    printf 'ERROR: dragonfly-persistent veto_retained never moved (%s -> %s) — the reaper is not evaluating the persistent instance at all, so nothing below would mean anything\n' \
        "$PER_VETO_RETAINED_BEFORE" "$per_veto_now" >&2
    exit 1
fi
printf '  ok: dragonfly-persistent veto_retained %s -> %s (the snapshot held the instance)\n' \
    "$PER_VETO_RETAINED_BEFORE" "$per_veto_now"
per_uid_grace=$(jp dragonfly.dragonflydb.io "$DF_NS" "$DF_INSTANCE_P" '{.metadata.uid}')
assert_eq "persistent instance survived the grace window" "$per_uid_grace" "$PER_UID_1"

# Simulate post-grace: drop the snapshot outright (a plain delete, NOT the
# past-retainUntil GC dance of Phase 12 — reclaiming the tenant's DB is not
# what is under test here, and leaving the data on the volume is what makes
# the re-adoption assertion below mean something).
kubectl delete retainedclaim "$RETAINED3" -n "$RETAINED_NS" --wait=true

wait_gone dragonfly.dragonflydb.io "$DF_NS" "$DF_INSTANCE_P" \
    $(( REAP_DWELL_SECS + 180 ))
wait_gone pod "$DF_NS" "${DF_INSTANCE_P}-0" 180

# ---- the volume survives ----------------------------------------------
# ADR 0042 §9.2, measured on dragonfly-operator v1.5.0: the snapshot PVC is
# unowned and the StatefulSet ships persistentVolumeClaimRetentionPolicy
# Retain, so the delete leaves it Bound. Held under observation rather than
# sampled once — a cascade would delete it asynchronously.
printf '  holding the snapshot PVC under observation for 30s ...\n'
for _ in $(seq 1 6); do
    df_pvc_phase=$(jp pvc "$DF_NS" "$DF_PVC" '{.status.phase}')
    df_pvc_deleting=$(jp pvc "$DF_NS" "$DF_PVC" '{.metadata.deletionTimestamp}')
    if [ "$df_pvc_phase" != "Bound" ] || [ -n "$df_pvc_deleting" ]; then
        printf 'ERROR: snapshot PVC %s did NOT survive the reap (phase=%q deletionTimestamp=%q) — ADR 0042 §9.2 says the Dragonfly arm is just the delete because the PVC is unowned and retained; re-measure that against the dragonfly-operator this cluster runs\n' \
            "$DF_PVC" "$df_pvc_phase" "$df_pvc_deleting" >&2
        kubectl -n "$DF_NS" get pvc >&2 2>&1 || true
        exit 1
    fi
    sleep 5
done
df_pv_after=$(jp pvc "$DF_NS" "$DF_PVC" '{.spec.volumeName}')
assert_eq "snapshot PVC still bound to the SAME PV after the reap" \
    "$df_pv_after" "$DF_PV_1"

per_reaped_after=$(reap_metric dragonfly-persistent reaped)
if [ "$per_reaped_after" -le "$PER_REAPED_BEFORE" ]; then
    printf 'ERROR: dragonfly-persistent reaped counter did not move (%s -> %s) — the instance went away for some reason OTHER than the reaper\n' \
        "$PER_REAPED_BEFORE" "$per_reaped_after" >&2
    exit 1
fi
printf '  ok: dragonfly-persistent reaped %s -> %s\n' \
    "$PER_REAPED_BEFORE" "$per_reaped_after"

# ---- re-provision adopts the same volume (§9.5) ------------------------
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP3}
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
      redis:
        persistent: true
        selector:
          tier: integrated
YAML

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM3" '{.status.ready}' true 600

PER_UID_2=$(jp dragonfly.dragonflydb.io "$DF_NS" "$DF_INSTANCE_P" '{.metadata.uid}')
if [ -z "$PER_UID_2" ] || [ "$PER_UID_2" = "$PER_UID_1" ]; then
    printf 'ERROR: the persistent instance uid is %q, want a NEW uid different from the reaped %q\n' \
        "$PER_UID_2" "$PER_UID_1" >&2
    exit 1
fi
df_pv_readopted=$(jp pvc "$DF_NS" "$DF_PVC" '{.spec.volumeName}')
assert_eq "re-provisioned persistent instance adopted the SAME PV (§9.5)" \
    "$df_pv_readopted" "$DF_PV_1"
printf '  ok: persistent instance re-created (uid %s -> %s) on the original volume\n' \
    "$PER_UID_1" "$PER_UID_2"

# ===============================================================
# Phase 17: sweep health — no per-tick error, and the arm with no
#           instances is silent rather than failing
# ===============================================================

phase "Phase 17: sweep health — the reaper errored on no tick of this run"

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

# The CNPG arm ran on every tick of this walk with NOTHING to reap — the
# CNPG operator is always-on and its CRD is served, and the `pg-integrated`
# provider is seeded, but this walk creates no pg claim so the shared
# Cluster was never lazily created. An empty candidate set must be silent:
# no error (asserted above, the error bucket is shared) and no reap.
cnpg_reaped=$(reap_metric cnpg reaped)
assert_eq "cnpg reaps on a cluster with no shared CNPG Cluster" "$cnpg_reaped" "0"

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

printf '\nneeds-redis-walk GREEN in %s\n' "$(elapsed)"
printf 'Chain proven: generate -> schedule -> provision -> resume + explicit env refs (2.12) -> isolation -> EVAL-confinement/client-init/restart-repin -> persistent variant + restart-durable -> delete + snapshot -> GC -> FLUSHDB/DELUSER\n'
printf 'ADR 0042 §9 proven: ephemeral reap (instance+STS+pod gone, RetainedClaim + admin Secret kept, node memory returned) -> clean re-create (new uid, unrotated password) -> live tenant never reaped -> persistent RETAINED veto then post-grace reap with the PVC preserved and the same PV re-adopted -> no sweep error\n'
