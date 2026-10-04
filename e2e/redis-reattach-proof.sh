#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# redis-reattach-proof.sh — a persistent needs.redis claim that is deleted and
# re-created within its grace period reattaches to its retained database, and
# a reattach whose first pass fails AFTER it recorded its allocation keeps
# that data on the retry (WI-402).
#
# Before operator v0.2.53 the retry took the "existing allocation" short cut,
# treated "not ready" as "nothing stored yet" and ran FLUSHDB on the very
# database the reattach was recovering. No walk could see it: every walk's
# reattach succeeds on its first pass.
#
# What it does, on one private kind cluster that it deletes on exit:
#
#   0. a kind cluster and `apprafter cluster-bootstrap` (the published platform);
#   1. with APPRAFTER_E2E_LOCAL_OPERATOR=1, the working tree's operator and
#      admission webhook, CRDs and RBAC; without it the published operator runs;
#   2. an Application with `needs.redis: {persistent: true}`; its claim goes
#      ready on the persistent pool instance;
#   3. PROOF_KEYS keys written into the claim's database as the claim's user;
#   4. the Application deleted: the claim cascades and its finalizer writes a
#      RetainedClaim naming the same instance and database number;
#   5. ACL SETUSER taken away from the instance's admin user (`default`, the
#      identity the provisioner uses), with FLUSHDB and DBSIZE still allowed.
#      A second admin user, created first, gives it back in step 7;
#   6. the Application re-applied: the new claim reattaches, records its
#      allocation, and fails at ACL SETUSER. The script waits for
#      PROOF_FAILURES such failures;
#   7. the keys counted while the failure stands, ACL SETUSER given back, the
#      claim ready again on the same database number, the keys counted again.
#
# The verdict is the count after the claim recovered: every recovering pass
# after the first one is a retry on top of the recorded allocation.
#
#   PROOF_EXPECT=survive|loss   default: survive with APPRAFTER_E2E_LOCAL_OPERATOR=1,
#                               loss without it (the published v0.2.52 operator
#                               flushes; running it shows the proof sees the defect)
#   PROOF_KEYS=50               keys written before the delete
#   PROOF_FAILURES=1            failed passes to wait for before the failure is lifted
#   APPRAFTER_E2E_SKIP_DESTROY=1  keep the cluster after the run
#
# KUBECONFIG is replaced by the private cluster's before the first kubectl, and
# the exit trap diagnoses and deletes only that cluster.
#
# Exit codes: 0 GREEN, 1 an assertion failed, 2 a precondition is missing or
# the failure could not be injected on this Dragonfly: no rule refused
# ACL SETUSER, the rule that did also took FLUSHDB or DBSIZE away from the
# admin user (the probe-database checks of step 5), or the claim went ready
# while ACL SETUSER was refused.

set -euo pipefail

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

CLUSTER_NAME="apprafter-redis-reattach"

APP_NS="demo"
APP="keeper"
CLAIM="keeper-redis"                     # generated claim: <app>-redis
CONN_SECRET="keeper-redis-conn"          # connection Secret: <claim>-conn
ACL_USER="claim_demo_keeper-redis_redis" # dragonfly::acl_user(demo, keeper-redis)
RETAINED="claim-demo-keeper-redis"       # cnpg::k8s_name(demo, keeper-redis)
RETAINED_NS="apprafter-system"
PROVIDER="redis-integrated"

DF_NS="dragonfly-system"
DF_INSTANCE="platform-redis-persistent-000"       # pool_instance_name(true, 0)
DF_ADMIN_SECRET="platform-redis-persistent-000-admin"

# Group-qualified: bare `application` also matches Argo CD's, and bare
# `resourceclaim` matches the DRA kind on Kubernetes 1.32+.
APP_RES="application.apprafter.io"
CLAIM_RES="resourceclaim.apprafter.io"
RETAINED_RES="retainedclaim.apprafter.io"

PROOF_KEYS="${PROOF_KEYS:-50}"
PROOF_FAILURES="${PROOF_FAILURES:-1}"
if [ -n "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
    PROOF_EXPECT="${PROOF_EXPECT:-survive}"
else
    PROOF_EXPECT="${PROOF_EXPECT:-loss}"
fi

case "$PROOF_EXPECT" in
    survive | loss) ;;
    *) printf 'ERROR: PROOF_EXPECT must be survive or loss, not %q\n' "$PROOF_EXPECT" >&2; exit 2 ;;
esac
case "$PROOF_KEYS" in
    '' | *[!0-9]* | 0) printf 'ERROR: PROOF_KEYS must be a positive integer, not %q\n' "$PROOF_KEYS" >&2; exit 2 ;;
esac
case "$PROOF_FAILURES" in
    '' | *[!0-9]* | 0) printf 'ERROR: PROOF_FAILURES must be a positive integer, not %q\n' "$PROOF_FAILURES" >&2; exit 2 ;;
esac

for tool in cargo kubectl helm; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done
if ! command -v docker >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
    printf 'ERROR: neither "docker" nor "podman" found on PATH\n' >&2
    exit 2
fi

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"
PROOF_ADMIN_PW="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"

# 1 once the private cluster exists and KUBECONFIG names it. Until then the
# trap must neither diagnose nor delete anything.
CLUSTER_CREATED=0
# 1 while the admin user lacks ACL SETUSER.
INJECTED=0

# shellcheck disable=SC2329  # invoked by the EXIT trap
cleanup() {
    local exit_code=$?
    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! redis-reattach-proof FAILED at %s (exit %d) !!!\n' "$(elapsed)" "$exit_code" >&2
        if [ "$CLUSTER_CREATED" -eq 1 ]; then
            dump_diagnostics || true
        fi
    fi
    if [ "$CLUSTER_CREATED" -eq 1 ]; then
        if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
            k3d_down "$CLUSTER_NAME" || true
        else
            printf '\nAPPRAFTER_E2E_SKIP_DESTROY set: cluster %s is left running.\n' "$CLUSTER_NAME"
            if [ "$INJECTED" -eq 1 ]; then
                printf 'Its %s admin user still lacks ACL SETUSER; user proofadmin can give it back.\n' "$DF_INSTANCE"
            fi
        fi
    fi
    rm -rf "$TMPDIR_WORK"
    exit "$exit_code"
}
trap cleanup EXIT

# ---------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------

# The CLI reads its kubeconfig from its own state store (as every walk does).
seed_apprafter_state() {
    local kubeconfig_content="$1" kc_escaped
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/k3d/.apprafter"
    printf 'active_target: k3d\nversion: 1\n' >"${APPRAFTER_CONFIG_DIR}/config.yaml"
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

strip_ansi() {
    sed $'s/\033\\[[0-9;]*[a-zA-Z]//g'
}

jp() { # <resource> <ns> <name> <jsonpath>
    kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true
}

ready_reason() { # the claim's Ready condition reason
    jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.conditions[?(@.type=="Ready")].reason}'
}

wait_jsonpath() { # <resource> <ns> <name> <jsonpath> <want> [timeout]
    local res="$1" ns="$2" name="$3" path="$4" want="$5" timeout="${6:-180}" got=""
    local deadline=$(( $(date +%s) + timeout ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        got="$(jp "$res" "$ns" "$name" "$path")"
        if [ "$got" = "$want" ]; then
            printf '  ok: %s/%s %s = %s\n' "$res" "$name" "$path" "$got"
            return 0
        fi
        sleep 5
    done
    printf 'ERROR: %s/%s %s never became %q (last %q)\n' "$res" "$name" "$path" "$want" "$got" >&2
    kubectl -n "$ns" get "$res" "$name" -o yaml >&2 2>&1 || true
    return 1
}

wait_gone() { # <resource> <ns> <name> [timeout]
    local res="$1" ns="$2" name="$3" timeout="${4:-180}"
    local deadline=$(( $(date +%s) + timeout ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if ! kubectl -n "$ns" get "$res" "$name" >/dev/null 2>&1; then
            printf '  ok: %s/%s is gone\n' "$res" "$name"
            return 0
        fi
        sleep 5
    done
    printf 'ERROR: %s/%s still present after %ss\n' "$res" "$name" "$timeout" >&2
    return 1
}

assert_eq() { # <what> <got> <want>
    if [ "$2" = "$3" ]; then
        printf '  ok: %s = %s\n' "$1" "$2"
        return 0
    fi
    printf 'ERROR: %s: got %q, want %q\n' "$1" "$2" "$3" >&2
    return 1
}

admin_pw() {
    kubectl -n "$DF_NS" get secret "$DF_ADMIN_SECRET" -o jsonpath='{.data.password}' 2>/dev/null | base64 -d
}

# redis-cli on the persistent instance as `default`, the admin identity the
# provisioner uses. stderr is folded in so an error reply can be matched.
redis_admin() {
    local pw
    pw="$(admin_pw)"
    kubectl -n "$DF_NS" exec "${DF_INSTANCE}-0" -- \
        redis-cli -a "$pw" --no-auth-warning "$@" 2>&1 || true
}

# redis-cli as a named user. `--user/--pass`, not a URL: the bundled
# redis-cli 6.0 ignores the URL's username and authenticates as `default`.
redis_as() { # <user> <password> <args...>
    local user="$1" pw="$2"
    shift 2
    kubectl -n "$DF_NS" exec "${DF_INSTANCE}-0" -- \
        redis-cli --user "$user" --pass "$pw" --no-auth-warning "$@" 2>&1 || true
}

# Dragonfly (v1.37.0) answers ACL WHOAMI with `User is <name>`, Redis with `<name>`.
whoami_name() { tr -d '\r' | sed -E 's/^User is //'; }

keys_in() { # <dbnum>: DBSIZE as the admin user; a non-number is a failure
    local n
    n="$(redis_admin -n "$1" DBSIZE | tr -d '\r')"
    case "$n" in
        '' | *[!0-9]*)
            printf 'ERROR: DBSIZE of database %s answered %q\n' "$1" "$n" >&2
            return 1 ;;
    esac
    printf '%s' "$n"
}

operator_pod() {
    kubectl -n "$RETAINED_NS" get pods -l app.kubernetes.io/name=apprafter-operator \
        --field-selector=status.phase=Running -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# Lines of the operator log that name this claim and contain <text>.
claim_log_count() { # <text>
    kubectl -n "$RETAINED_NS" logs deploy/apprafter-operator --tail=-1 2>/dev/null \
        | strip_ansi | grep -F -- "$CLAIM" | grep -c -F -- "$1" || true
}

apply_app() {
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
        persistent: true
        selector:
          tier: integrated
YAML
}

# ===============================================================
phase "Phase 0: kind cluster ${CLUSTER_NAME} and cluster-bootstrap (expect: ${PROOF_EXPECT})"
# ===============================================================

k3d_up "$CLUSTER_NAME"
cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
CLUSTER_CREATED=1
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

seed_apprafter_state "$(cat "$KUBECONFIG_FILE")"
export APPRAFTER_CONFIG_DIR
bootstrap_with_retry
printf '  cluster-bootstrap complete\n'

if [ -n "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
    # =========================================================
    phase "Phase 1: the working tree's operator, webhook, CRDs and RBAC"
    # =========================================================
    # Argo CD would put the published CRDs back; stop it syncing the two
    # Applications that own them first (as every local-operator walk does).
    for _app in platform apprafter-operator; do
        kubectl -n argocd patch applications.argoproj.io "$_app" --type=merge \
            -p '{"spec":{"syncPolicy":{"automated":null}}}' >/dev/null 2>&1 || true
    done
    build_load_restart apprafter-operator apprafter-operator
    build_load_restart admission-webhook admission-webhook
    _wh_deadline=$(( $(date +%s) + 90 ))
    while [ "$(date +%s)" -lt "$_wh_deadline" ]; do
        [ "$(kubectl -n "$RETAINED_NS" get pods -l app.kubernetes.io/name=admission-webhook \
            --no-headers 2>/dev/null | wc -l)" -le 1 ] && break
        sleep 3
    done
    apply_branch_operator_crds
    for _crd in applications serviceproviders resourceclaims retainedclaims; do
        retry 12 5 -- kubectl wait --for=condition=Established "crd/${_crd}.apprafter.io" --timeout=30s
    done
    apply_branch_operator_rbac
    printf "  the working tree's operator and webhook are running\n"
else
    printf '\n  APPRAFTER_E2E_LOCAL_OPERATOR unset: the published operator runs\n'
fi

# ===============================================================
phase "Phase 2: platform readiness"
# ===============================================================

retry 30 10 -- kubectl -n "$DF_NS" rollout status deploy -l app.kubernetes.io/name=dragonfly-operator --timeout=60s
retry 30 10 -- kubectl -n "$RETAINED_NS" get serviceprovider.apprafter.io "$PROVIDER"
retry 30 10 -- kubectl -n "$RETAINED_NS" rollout status deploy admission-webhook --timeout=60s

# ===============================================================
phase "Phase 3: a persistent needs.redis claim goes ready"
# ===============================================================

kubectl create namespace "$APP_NS" 2>/dev/null || true
apply_app
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.ready}' true 600
assert_eq "the claim's instance" "$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.instance}')" "$DF_INSTANCE"
DBNUM="$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.dbnum}')"
case "$DBNUM" in
    '' | *[!0-9]*) printf "ERROR: the claim's status.dbnum is not a number: %q\n" "$DBNUM" >&2; exit 1 ;;
esac
printf '  ok: the claim holds database %s\n' "$DBNUM"

# ===============================================================
phase "Phase 4: ${PROOF_KEYS} keys written as the claim's user"
# ===============================================================

CLAIM_PW="$(kubectl -n "$APP_NS" get secret "$CONN_SECRET" -o jsonpath='{.data.pass}' | base64 -d)"
[ -n "$CLAIM_PW" ] || { printf 'ERROR: %s carries no pass key\n' "$CONN_SECRET" >&2; exit 1; }
mset_args=()
for i in $(seq 1 "$PROOF_KEYS"); do
    mset_args+=("proof:key:${i}" "value-${i}")
done
assert_eq "MSET as ${ACL_USER}" "$(redis_as "$ACL_USER" "$CLAIM_PW" -n "$DBNUM" MSET "${mset_args[@]}" | tr -d '\r')" "OK"
BEFORE="$(keys_in "$DBNUM")"
assert_eq "keys in database ${DBNUM}" "$BEFORE" "$PROOF_KEYS"

# ===============================================================
phase "Phase 5: delete the Application; the claim's snapshot keeps the database"
# ===============================================================

# The Application, not the claim: a claim deleted under a live Application is
# regenerated at once and reattaches before anything can be observed.
kubectl -n "$APP_NS" delete "$APP_RES" "$APP" --wait=true
wait_gone "$CLAIM_RES" "$APP_NS" "$CLAIM" 180
wait_jsonpath "$RETAINED_RES" "$RETAINED_NS" "$RETAINED" '{.spec.claimRef.name}' "$CLAIM" 180
assert_eq "RetainedClaim backend" "$(jp "$RETAINED_RES" "$RETAINED_NS" "$RETAINED" '{.spec.backend}')" "dragonfly"
assert_eq "RetainedClaim instance" "$(jp "$RETAINED_RES" "$RETAINED_NS" "$RETAINED" '{.spec.instance}')" "$DF_INSTANCE"
assert_eq "RetainedClaim dbnum" "$(jp "$RETAINED_RES" "$RETAINED_NS" "$RETAINED" '{.spec.dbnum}')" "$DBNUM"
assert_eq "RetainedClaim aclUser" "$(jp "$RETAINED_RES" "$RETAINED_NS" "$RETAINED" '{.spec.aclUser}')" "$ACL_USER"
assert_eq "keys in database ${DBNUM} within grace" "$(keys_in "$DBNUM")" "$PROOF_KEYS"

# ===============================================================
phase "Phase 6: take ACL SETUSER away from the instance's admin user"
# ===============================================================

assert_eq "proofadmin created" \
    "$(redis_admin ACL SETUSER proofadmin on ">${PROOF_ADMIN_PW}" '~*' '&*' '+@all' | tr -d '\r')" "OK"
assert_eq "proofadmin authenticates" \
    "$(redis_as proofadmin "$PROOF_ADMIN_PW" ACL WHOAMI | whoami_name)" "proofadmin"

# The narrowest rule this Dragonfly accepts that refuses ACL SETUSER to
# `default`. A rule it rejects, or one that leaves ACL SETUSER allowed, is
# undone and the next one tried; none working is exit 2, never a pass.
# `-acl|setuser` is tried first but is expected to be rejected: Dragonfly's
# ACL parser refuses the command|subcommand form ("Unrecognized parameter
# +CLIENT|SETNAME", recorded in operator/operator-controllers/
# resourceclaim-provisioner/src/dragonfly.rs, acl_setuser_args_scoped, on
# v1.37.0). It stays first in case a later Dragonfly accepts it.
INJECT_RULE=""
for rule in '-acl|setuser' '-acl' '-@admin'; do
    reply="$(redis_admin ACL SETUSER default "$rule" | tr -d '\r')"
    probe="$(redis_admin ACL SETUSER proofprobe off | tr -d '\r')"
    case "$probe" in
        *NOPERM*)
            INJECT_RULE="$rule"
            INJECTED=1
            printf '  ok: rule %s refuses ACL SETUSER to default (%s)\n' "$rule" "$probe"
            break ;;
    esac
    printf '  rule %s did not refuse ACL SETUSER (setuser: %s, probe: %s); undoing it\n' "$rule" "$reply" "$probe"
    redis_admin ACL DELUSER proofprobe >/dev/null
    redis_admin ACL SETUSER default '+@all' >/dev/null
done
if [ -z "$INJECT_RULE" ]; then
    printf 'ERROR: no rule refused ACL SETUSER to default on this Dragonfly; the failure cannot be injected\n' >&2
    exit 2
fi

# FLUSHDB and DBSIZE must still work, or the injected failure would not be
# the one this proof is about: the published operator could not flush and
# the "during" count would mean nothing. Checked on an empty database. A
# miss here is an injection precondition (the rule took more than ACL
# SETUSER, e.g. `-@admin` taking FLUSHDB or DBSIZE), so it exits 2, not 1.
PROBE_DB=$(( DBNUM + 1 ))
assert_eq "probe database ${PROBE_DB} starts empty" "$(keys_in "$PROBE_DB")" "0" || exit 2
assert_eq "SET in the probe database as default" "$(redis_admin -n "$PROBE_DB" SET proof:probe 1 | tr -d '\r')" "OK" || exit 2
assert_eq "FLUSHDB as default while ACL SETUSER is refused" "$(redis_admin -n "$PROBE_DB" FLUSHDB | tr -d '\r')" "OK" || exit 2
assert_eq "probe database ${PROBE_DB} after FLUSHDB" "$(keys_in "$PROBE_DB")" "0" || exit 2

# ===============================================================
phase "Phase 7: re-create the Application; the reattach fails after its checkpoint"
# ===============================================================

OP_POD="$(operator_pod)"
[ -n "$OP_POD" ] || { printf 'ERROR: no running apprafter-operator pod\n' >&2; exit 1; }
FAIL_BASE="$(claim_log_count 'dragonfly ACL SETUSER')"
SKIP_BASE="$(claim_log_count 'skipping FLUSHDB')"
apply_app

fail_deadline=$(( $(date +%s) + 300 ))
failures=0
while :; do
    failures=$(( $(claim_log_count 'dragonfly ACL SETUSER') - FAIL_BASE ))
    checkpoint="$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.dbnum}')"
    if [ "$failures" -ge "$PROOF_FAILURES" ] && [ "$checkpoint" = "$DBNUM" ]; then
        break
    fi
    if [ "$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.ready}')" = "true" ]; then
        printf 'ERROR: the claim went ready while ACL SETUSER was refused; the injection did not hold\n' >&2
        exit 2
    fi
    if [ "$(date +%s)" -ge "$fail_deadline" ]; then
        printf 'ERROR: after 300s, %d failed ACL SETUSER pass(es) (want %d) and status.dbnum=%q (want %s)\n' \
            "$failures" "$PROOF_FAILURES" "$checkpoint" "$DBNUM" >&2
        exit 1
    fi
    sleep 5
done
printf '  ok: %d reattach pass(es) failed at ACL SETUSER after recording database %s\n' "$failures" "$DBNUM"
reason="$(ready_reason)"
case "$reason" in
    DbnumConflict | AwaitingKeyspace)
        printf 'ERROR: the reattach reads %s: it conflicts with its own snapshot\n' "$reason" >&2
        exit 1 ;;
esac
printf '  ok: the claim is not ready (Ready reason %q)\n' "$reason"

DURING="$(keys_in "$DBNUM")"
printf '  keys in database %s while ACL SETUSER is refused: %s\n' "$DBNUM" "$DURING"

# ===============================================================
phase "Phase 8: give ACL SETUSER back; the claim recovers"
# ===============================================================

assert_eq "default gets ACL SETUSER back" \
    "$(redis_as proofadmin "$PROOF_ADMIN_PW" ACL SETUSER default '+@all' | tr -d '\r')" "OK"
assert_eq "default runs ACL commands again" "$(redis_admin ACL WHOAMI | whoami_name)" "default"
INJECTED=0
redis_admin ACL DELUSER proofadmin proofprobe >/dev/null

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.ready}' true 300
assert_eq "the claim's database after the reattach" "$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.dbnum}')" "$DBNUM"
assert_eq "the claim's Ready reason" "$(ready_reason)" "Provisioned"
wait_gone "$RETAINED_RES" "$RETAINED_NS" "$RETAINED" 180
assert_eq "the operator pod did not restart during the proof" "$(operator_pod)" "$OP_POD"

AFTER="$(keys_in "$DBNUM")"
SKIPS=$(( $(claim_log_count 'skipping FLUSHDB') - SKIP_BASE ))
printf '  keys in database %s after the claim recovered: %s\n' "$DBNUM" "$AFTER"
printf '  passes that skipped FLUSHDB as a reattach: %d\n' "$SKIPS"

# ===============================================================
phase "Verdict (expect: ${PROOF_EXPECT})"
# ===============================================================

if [ "$PROOF_EXPECT" = survive ]; then
    assert_eq "keys while the reattach was failing" "$DURING" "$PROOF_KEYS"
    assert_eq "keys after the reattach recovered" "$AFTER" "$PROOF_KEYS"
    if [ "$SKIPS" -lt 2 ]; then
        printf 'ERROR: only %d pass(es) skipped FLUSHDB as a reattach; the retry did not take the reattach path\n' "$SKIPS" >&2
        exit 1
    fi
    printf '  ok: %d passes skipped FLUSHDB, the retries included\n' "$SKIPS"
    NEW_PW="$(kubectl -n "$APP_NS" get secret "$CONN_SECRET" -o jsonpath='{.data.pass}' | base64 -d)"
    assert_eq "the claim's user reads its retained data" \
        "$(redis_as "$ACL_USER" "$NEW_PW" -n "$DBNUM" GET "proof:key:${PROOF_KEYS}" | tr -d '\r')" "value-${PROOF_KEYS}"
    printf '\nredis-reattach-proof GREEN in %s (expect survive, rule %s): %s keys before the delete, %s while the reattach failed, %s after it recovered on database %s\n' \
        "$(elapsed)" "$INJECT_RULE" "$BEFORE" "$DURING" "$AFTER" "$DBNUM"
else
    assert_eq "keys after the reattach recovered" "$AFTER" "0"
    printf '\nredis-reattach-proof GREEN in %s (expect loss, rule %s): the operator flushed the retained data on the retry: %s keys before the delete, %s while the reattach failed, %s after it recovered on database %s\n' \
        "$(elapsed)" "$INJECT_RULE" "$BEFORE" "$DURING" "$AFTER" "$DBNUM"
fi
