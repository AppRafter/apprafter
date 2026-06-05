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
#   resume + DSN inject (2.4d/2.4e/2.6-6) -> isolation proof ($N ACL) ->
#   delete + snapshot (2.4f/2.6-4) -> force-GC (2.6-7) ->
#   redis-cli ACL/FLUSHDB proof
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
#      ACL user, and writes a connection Secret carrying REDIS_URL +
#      REDIS_CHANNEL_PREFIX — and that the scheduler's Scheduled=True
#      survives the provisioner's status write (the SSA field-manager
#      split guard).
#   5. ASSERT the Application resumes to Ready and the rendered
#      Deployment injects REDIS_URL + REDIS_CHANNEL_PREFIX from the Secret.
#   6. Isolation proof (ADR 0042 Pre-merge #3): a SECOND claim's ACL user
#      gets NOPERM when it tries to SELECT the first claim's DB.
#   7. Delete the ResourceClaim; ASSERT the finalizer snapshots a
#      RetainedClaim (backend=dragonfly, 7-day grace) and the connection
#      Secret cascades.
#   8. Force GC by deleting + re-creating the RetainedClaim with a past
#      retainUntil; ASSERT FLUSHDB + ACL DELUSER ran (the ACL user is
#      gone, the DB is empty), and the snapshot is removed.
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

# Group-qualify the two collision-prone kinds so kubectl never resolves
# to the wrong API group: bare `application` also matches Argo CD's
# argoproj.io Application, and bare `resourceclaim` matches the k8s 1.32+
# DRA resource.k8s.io ResourceClaim. Always address the apprafter.io CRs.
APP_RES="application.apprafter.io"
CLAIM_RES="resourceclaim.apprafter.io"

# acl_user(demo, web-redis) / acl_user(demo, api-redis).
ACL_USER="claim_demo_web-redis_redis"
ACL_USER2="claim_demo_api-redis_redis"
# RetainedClaim name = cnpg::k8s_name(demo, web-redis).
RETAINED="claim-demo-web-redis"

DF_NS="dragonfly-system"            # shared Dragonfly instance namespace
# The ephemeral pool instance (index 0) the first non-persistent claim
# lands on (pool_instance_name(false, 0)).
DF_INSTANCE="platform-redis-ephemeral-000"
DF_ADMIN_SECRET="platform-redis-ephemeral-000-admin"
RETAINED_NS="apprafter-system"      # RetainedClaim namespace
PROVIDER="redis-integrated"         # seeded ServiceProvider

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------

for tool in cargo kubectl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done

# docker or podman (podman is typically aliased to docker in CI)
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
# Local helper: redis_admin <args...>  — run redis-cli against the
# shared Dragonfly instance AS THE ADMIN (default) user, reading the
# admin password from the per-instance admin Secret. Used for the
# isolation proof and the post-GC ACL/keyspace assertions.
#
# Echoes redis-cli stdout; returns redis-cli's exit code.
# ---------------------------------------------------------------
redis_admin() {
    local admin_pw
    admin_pw=$(kubectl -n "$DF_NS" get secret "$DF_ADMIN_SECRET" \
        -o jsonpath='{.data.password}' 2>/dev/null | base64 -d)
    kubectl -n "$DF_NS" exec "deploy/${DF_INSTANCE}" -- \
        redis-cli -a "$admin_pw" --no-auth-warning "$@" 2>/dev/null
}

# redis_as <user> <password> <args...> — run redis-cli AS a per-claim
# ACL user (proves the user's grants directly).
redis_as() {
    local user="$1" pw="$2"; shift 2
    kubectl -n "$DF_NS" exec "deploy/${DF_INSTANCE}" -- \
        redis-cli -u "redis://${user}:${pw}@127.0.0.1:6379" \
        --no-auth-warning "$@" 2>&1 || true
}

# Recover a claim's ACL password from its connection Secret's REDIS_URL
# (redis://user:PASSWORD@host:port/db). The provisioner stores no
# separate password — the DSN is the source of truth.
claim_password() {
    local conn_ns="$1" conn_name="$2"
    kubectl -n "$conn_ns" get secret "$conn_name" \
        -o jsonpath='{.data.REDIS_URL}' 2>/dev/null | base64 -d \
        | sed -E 's#^redis://[^:]+:([^@]+)@.*$#\1#'
}

# ===============================================================
# Phase 0: bring up the k3d cluster
# ===============================================================

phase "Phase 0: k3d_up ${CLUSTER_NAME}"

k3d_up "$CLUSTER_NAME"

k3d_bin="k3d"
command -v k3d >/dev/null 2>&1 || k3d_bin="nix run nixpkgs#k3d --"
# shellcheck disable=SC2086
$k3d_bin kubeconfig write "$CLUSTER_NAME" --output "$KUBECONFIG_FILE"
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

phase "Phase 3: apply Applications with spec.base.needs.redis"

kubectl create namespace "$APP_NS" 2>/dev/null || true

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
      public: false
    needs:
      redis:
        selector:
          tier: integrated
YAML

# A second app — same ephemeral pool instance, a DIFFERENT numbered DB.
# Used in Phase 6 for the cross-DB isolation proof (ADR 0042 #3).
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
      public: false
    needs:
      redis:
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

# The Application is paused awaiting the claim (2.4d gate).
wait_jsonpath "$APP_RES" "$APP_NS" "$APP" '{.status.phase}' \
    AwaitingResourceClaim 120
gate_cond=$(cond_status "$APP_RES" "$APP_NS" "$APP" ResourceClaimPending)
assert_eq "Application ResourceClaimPending condition" "$gate_cond" "True"

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

# The $N ACL user exists on the instance with the right DB selector.
acl_line=$(redis_admin ACL GETUSER "$ACL_USER" || true)
if [ -z "$acl_line" ]; then
    printf 'ERROR: ACL user %s not found on %s\n' "$ACL_USER" "$DF_INSTANCE" >&2
    redis_admin ACL LIST >&2 || true
    exit 1
fi
printf '  ok: ACL user %s exists on %s\n' "$ACL_USER" "$DF_INSTANCE"

# Connection Secret: status ref, BOTH keys, ownerRef cascade.
conn_ref=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.connectionSecretRef}')
assert_eq "status.connectionSecretRef" "$conn_ref" "$CONN_SECRET"
conn_url=$(jp secret "$APP_NS" "$CONN_SECRET" '{.data.REDIS_URL}')
if [ -z "$conn_url" ]; then
    printf 'ERROR: connection Secret %s missing REDIS_URL key\n' "$CONN_SECRET" >&2
    exit 1
fi
conn_pfx=$(jp secret "$APP_NS" "$CONN_SECRET" '{.data.REDIS_CHANNEL_PREFIX}')
if [ -z "$conn_pfx" ]; then
    printf 'ERROR: connection Secret %s missing REDIS_CHANNEL_PREFIX key\n' "$CONN_SECRET" >&2
    exit 1
fi
printf '  ok: connection Secret %s carries REDIS_URL + REDIS_CHANNEL_PREFIX\n' "$CONN_SECRET"
conn_owner_kind=$(jp secret "$APP_NS" "$CONN_SECRET" \
    '{.metadata.ownerReferences[0].kind}')
assert_eq "connection Secret ownerRef Kind" "$conn_owner_kind" "ResourceClaim"

# SSA-split guard: the provisioner's status write (ready /
# connectionSecretRef / Ready / instance / dbnum) must NOT have clobbered
# the scheduler's Scheduled=True. Re-assert it after provisioning.
sched_after=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM" Scheduled)
assert_eq "Scheduled still True after provision (SSA split)" "$sched_after" "True"

# ===============================================================
# Phase 7: resume + DSN (2.4d/2.4e/2.6-6) — Application Ready, env injected
# ===============================================================

phase "Phase 7: resume — Application Ready, REDIS_URL + REDIS_CHANNEL_PREFIX injected"

wait_jsonpath "$APP_RES" "$APP_NS" "$APP" '{.status.phase}' Ready 180

env_secret=$(kubectl -n "$APP_NS" get deployment "$APP" \
    -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"REDIS_URL\")].valueFrom.secretKeyRef.name}" \
    2>/dev/null || true)
assert_eq "Deployment env REDIS_URL secretKeyRef.name" "$env_secret" "$CONN_SECRET"
env_pfx_secret=$(kubectl -n "$APP_NS" get deployment "$APP" \
    -o jsonpath="{.spec.template.spec.containers[0].env[?(@.name==\"REDIS_CHANNEL_PREFIX\")].valueFrom.secretKeyRef.name}" \
    2>/dev/null || true)
assert_eq "Deployment env REDIS_CHANNEL_PREFIX secretKeyRef.name" "$env_pfx_secret" "$CONN_SECRET"

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

# ===============================================================
# Phase 9: delete + snapshot (2.4f/2.6-4) — RetainedClaim (dragonfly), cascade
# ===============================================================

phase "Phase 9: delete claim — RetainedClaim snapshot + cascade"

kubectl delete "$CLAIM_RES" "$CLAIM" -n "$APP_NS" --wait=true

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
# Phase 10: force GC (2.6-7) — re-create RetainedClaim with a past
#           retainUntil; the GC runs FLUSHDB + ACL DELUSER
# ===============================================================

phase "Phase 10: force GC — past retainUntil drops the ACL user + flushes the DB"

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
printf 'Chain proven: generate -> schedule -> provision -> resume + DSN -> isolation -> delete + snapshot -> GC -> FLUSHDB/DELUSER\n'
