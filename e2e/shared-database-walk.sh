#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter SHARED DATABASE e2e — the 2.29 chain on a local k3d/kind cluster
# (ADR 0066).
#
# What this proves that nothing else can
# --------------------------------------
# The role model is already proven on a live PostgreSQL by
# `e2e/shared-pg-sql-check.sh`, which executes the statement builders' own
# output against a real 18.4 server and ends on the property — a table created
# by one rw consumer is owned by the GROUP, a second rw consumer reads and
# writes it, an ro consumer reads and its INSERT is refused. That is the SQL
# settled. What it cannot touch is everything between a manifest and those
# statements:
#
#   1. That a `SharedDatabase` CR becomes a real database at all — the
#      controller creating the groups over SQL, CNPG creating the database
#      owned by one of them, and the operator having the RBAC to watch the
#      Kind in the first place. A missing rbac.yaml line 403s on the first
#      watch and the controller stalls with nothing in the object saying why;
#      only a cluster catches that.
#   2. That `needs.pg.ref` generates a claim which BINDS rather than
#      provisions, and that the consumer's connection Secret is the same shape
#      an owned claim's is — which is what lets `claim.pg.*`, the egress rule
#      and the readiness gate stay unaware the database is shared.
#   3. That the ADR 0052 gate fires on the FIRST bind of a pair and not on the
#      second reconcile of the same one. A gate that re-fires is a gate nobody
#      can leave switched on.
#   4. That `refCount` is derived and the `db rm` guard reads it — including
#      the half no unit test reaches, that the refusal NAMES the binders.
#   5. That a shared Redis keyspace's consumers can publish to each other.
#      The per-database channel prefix exists for exactly this, and a
#      per-user prefix would pass every unit test while leaving the two
#      unable to talk.
#   6. `pgvector` in a SHARED database, end to end from the manifest.
#
# Judge this walk by READING THE LOG, not by its exit code — a sandboxed
# runner can mask the inner status. Every phase prints `ok:` lines; the last
# line is GREEN, or the script has printed FAILED and exited non-zero.
#
# Usage:
#   APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/shared-database-walk.sh
#
# APPRAFTER_E2E_LOCAL_OPERATOR=1 is MANDATORY: every line under test is on the
# branch, and the published operator knows nothing about SharedDatabase.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export REPO_ROOT
# shellcheck source=/dev/null
source "$(dirname "$0")/lib.sh"

if [ -z "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
    cat >&2 <<'MSG'
ERROR: shared-database-walk requires APPRAFTER_E2E_LOCAL_OPERATOR=1.

Every behaviour under test — the SharedDatabase controller, the bind path,
the ADR 0052 trigger — exists only on this branch. Against the published
operator the walk would report a CRD that nothing reconciles.

  APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/shared-database-walk.sh
MSG
    exit 2
fi

CLUSTER_NAME="${APPRAFTER_E2E_CLUSTER:-apprafter-shdb-walk}"
export CLUSTER_NAME

PLATFORM_NS="apprafter-system"
CNPG_NS="cnpg-system"
DF_NS="dragonfly-system"
APP_NS="shop"

PG_DB="orders"                       # the shared pg SharedDatabase
CACHE_DB="events"                    # the shared redis SharedDatabase
APP_RW="web"                         # binds orders rw
APP_RO="reporter"                    # binds orders ro
APP_CACHE_A="pub"                    # binds events rw
APP_CACHE_B="sub"                    # binds events rw

# Derived by the operator — asserted rather than assumed, because these are
# exactly the names an operator has to find on the server.
#   shared_group("shop","orders")  -> shd_shop_orders  (group AND database)
#   pg_identifier("shop","web-pg") -> claim_shop_web_pg
PG_DB_NAME="shd_shop_orders"
PG_GROUP="shd_shop_orders"
PG_READER_GROUP="shd_shop_orders_ro"
RW_ROLE="claim_shop_web_pg"
RO_ROLE="claim_shop_reporter_pg"

AR_APP="application.apprafter.io"
CLAIM_RES="resourceclaim.apprafter.io"
SHDB_RES="shareddatabase.apprafter.io"
PLAN_RES="migrationplan.apprafter.io"

K3D_CREATED=0
FAILED=0

for tool in cargo kubectl helm; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    }
done
command -v docker >/dev/null 2>&1 || command -v podman >/dev/null 2>&1 || {
    printf 'ERROR: neither "docker" nor "podman" found on PATH\n' >&2
    exit 2
}

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"

cleanup() {
    if [ "$FAILED" = "1" ]; then
        printf '\n----- diagnostics -----\n' >&2
        kubectl -n "$APP_NS" get "$SHDB_RES" -o yaml >&2 2>&1 || true
        kubectl -n "$APP_NS" get "$CLAIM_RES" -o wide >&2 2>&1 || true
        kubectl -n "$PLATFORM_NS" logs deploy/apprafter-operator --tail=120 >&2 2>&1 || true
        kubectl -n "$CNPG_NS" get cluster,database >&2 2>&1 || true
        printf '----- end diagnostics -----\n' >&2
    fi
    if [ "$K3D_CREATED" = "1" ]; then
        if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
            printf '\nTearing down cluster (APPRAFTER_E2E_SKIP_DESTROY=1 keeps it).\n' >&2
            k3d_down "$CLUSTER_NAME" >/dev/null 2>&1 || true
        else
            printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving %s up.\n' "$CLUSTER_NAME" >&2
        fi
    fi
    rm -rf "$TMPDIR_WORK"
}
trap cleanup EXIT

die() {
    FAILED=1
    printf '\n!!! shared-database-walk FAILED: %s !!!\n' "$1" >&2
    exit 1
}

# ---------------------------------------------------------------
# Helpers (same shapes the other walks use; see env-and-secrets-walk.sh)
# ---------------------------------------------------------------
jp() { kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true; }

cond_reason() {
    kubectl -n "$2" get "$1" "$3" \
        -o jsonpath="{.status.conditions[?(@.type==\"$4\")].reason}" 2>/dev/null || true
}
cond_message() {
    kubectl -n "$2" get "$1" "$3" \
        -o jsonpath="{.status.conditions[?(@.type==\"$4\")].message}" 2>/dev/null || true
}
cond_status() {
    kubectl -n "$2" get "$1" "$3" \
        -o jsonpath="{.status.conditions[?(@.type==\"$4\")].status}" 2>/dev/null || true
}

wait_jsonpath() { # <kind> <ns> <name> <jsonpath> <want> [timeout]
    local kind="$1" ns="$2" name="$3" path="$4" want="$5" timeout="${6:-180}"
    local deadline got
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait %s/%s [%s] == %q (timeout %ss) ...\n' "$kind" "$name" "$path" "$want" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        got=$(kubectl -n "$ns" get "$kind" "$name" -o jsonpath="$path" 2>/dev/null || true)
        if [ "$got" = "$want" ]; then
            printf '  ok: %s/%s [%s] = %q\n' "$kind" "$name" "$path" "$got"
            return 0
        fi
        sleep 5
    done
    printf 'ERROR: %s/%s [%s] never became %q (last=%q)\n' \
        "$kind" "$name" "$path" "$want" "${got:-}" >&2
    kubectl -n "$ns" describe "$kind" "$name" >&2 2>&1 || true
    return 1
}

check() { # <description> <actual> <expected>
    if [ "$2" = "$3" ]; then
        printf '  ok: %s\n' "$1"
    else
        printf '  FAIL: %s — got %q, wanted %q\n' "$1" "$2" "$3" >&2
        die "$1"
    fi
}

contains() { # <description> <haystack> <needle>
    case "$2" in
        *"$3"*) printf '  ok: %s\n' "$1" ;;
        *)
            printf '  FAIL: %s — %q does not contain %q\n' "$1" "$2" "$3" >&2
            die "$1"
            ;;
    esac
}

seed_apprafter_state() {
    local kc="$1" kc_escaped
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/k3d/.apprafter"
    cat >"${APPRAFTER_CONFIG_DIR}/config.yaml" <<'YAML'
active_target: k3d
version: 1
YAML
    kc_escaped=$(printf '%s' "$kc" | sed 's/\\/\\\\/g' | sed 's/"/\\"/g' | awk '{printf "%s\\n", $0}')
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

# Run one SQL statement inside the CNPG primary as the given role.
#
# Executed through the POD rather than through a port-forward, because a
# forward adds a failure mode that looks exactly like an auth failure and this
# walk's whole subject is whether auth is right.
psql_as() { # <role> <password> <database> <sql>
    kubectl -n "$CNPG_NS" exec -i "${PG_CLUSTER}-1" -c postgres -- \
        env PGPASSWORD="$2" psql -U "$1" -d "$3" -h 127.0.0.1 -tAc "$4" 2>&1
}

# The value of one key in a claim's connection Secret.
conn_key() { # <claim> <key>
    kubectl -n "$APP_NS" get secret "${1}-conn" \
        -o jsonpath="{.data.$2}" 2>/dev/null | base64 -d 2>/dev/null || true
}

# ===============================================================
phase "Phase 0: cluster up ${CLUSTER_NAME}"
# ===============================================================
k3d_up "$CLUSTER_NAME"
cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
K3D_CREATED=1
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

# ===============================================================
phase "Phase 1: cluster-bootstrap (published platform stack)"
# ===============================================================
seed_apprafter_state "$(cat "$KUBECONFIG_FILE")"
export APPRAFTER_CONFIG_DIR
bootstrap_with_retry
printf '  cluster-bootstrap complete\n'

# ===============================================================
phase "Phase 1b: build + load the working-tree operator + webhook"
# ===============================================================
build_load_restart apprafter-operator apprafter-operator
build_load_restart admission-webhook admission-webhook

# Argo CD owns the operator CRDs, so pause its sync before applying the
# branch ones — otherwise it reverts them to the published set, which has no
# SharedDatabase at all and the walk fails on a CRD that briefly existed.
for _app in platform apprafter-operator; do
    kubectl -n argocd patch application.argoproj.io "$_app" --type=merge \
        -p '{"spec":{"syncPolicy":{"automated":null}}}' >/dev/null 2>&1 || true
done
apply_branch_operator_crds
for _crd in applications serviceproviders resourceclaims shareddatabases; do
    retry 12 5 -- kubectl wait --for=condition=Established \
        "crd/${_crd}.apprafter.io" --timeout=30s
done
printf '  branch CRDs applied + Established (including shareddatabases)\n'
apply_branch_operator_rbac

# ===============================================================
phase "Phase 2: platform readiness (CNPG, dragonfly, providers)"
# ===============================================================
kubectl create namespace "$APP_NS" >/dev/null 2>&1 || true
# By LABEL, not by a guessed Deployment name. The first version of this walk
# named `cnpg-cloudnative-pg` and then `cnpg-controller-manager`, and spent
# five minutes retrying a 404 — the chart's release name is not a thing to
# guess at. The label is what env-and-secrets-walk has always used.
retry 30 10 -- kubectl -n "$CNPG_NS" rollout status \
    deploy -l app.kubernetes.io/name=cloudnative-pg --timeout=60s \
    || die "the CNPG operator never became Available"
printf '  ok: CNPG operator Available\n'
retry 30 10 -- kubectl get serviceprovider pg-integrated -n "$PLATFORM_NS" >/dev/null \
    || die "the pg-integrated ServiceProvider was never seeded"
printf '  ok: pg-integrated ServiceProvider present\n'
PG_CLUSTER=$(jp serviceprovider "$PLATFORM_NS" pg-integrated '{.spec.config.cluster}')
[ -n "$PG_CLUSTER" ] || PG_CLUSTER="platform-postgres"
printf '  shared CNPG cluster: %s\n' "$PG_CLUSTER"

# ===============================================================
phase "Phase 3: apprafter db create — a shared pg database with pgvector"
# ===============================================================
# Through the CLI rather than kubectl, because `apprafter db` is part of the
# deliverable and a walk that only ever applies YAML proves the CRD and not
# the product.
apprafter db create "$PG_DB" --type pg --extension vector --extension pg_trgm \
    -n "$APP_NS" || die "apprafter db create"

wait_jsonpath "$SHDB_RES" "$APP_NS" "$PG_DB" '{.status.ready}' true 420 \
    || die "the shared pg database never became ready: $(cond_reason "$SHDB_RES" "$APP_NS" "$PG_DB" Ready) / $(cond_message "$SHDB_RES" "$APP_NS" "$PG_DB" Ready)"

check "status.database names the derived database" \
    "$(jp "$SHDB_RES" "$APP_NS" "$PG_DB" '{.status.database}')" "$PG_DB_NAME"
check "an unbound database reports refCount 0" \
    "$(jp "$SHDB_RES" "$APP_NS" "$PG_DB" '{.status.refCount}')" "0"
# The absence is the design (ADR 0066 §1) and the reason the CRD exists: one
# shared Secret here would be the thing per-consumer credentials replace.
check "the status publishes no shared connection Secret" \
    "$(jp "$SHDB_RES" "$APP_NS" "$PG_DB" '{.status.connectionSecretRef}')" ""

# The groups exist ON THE SERVER, created by the controller over SQL because
# CNPG cannot create them (a CREATEROLE non-superuser administers only what it
# created — measured, ADR 0066 §3.1).
PG_ADMIN_PW=$(kubectl -n "$CNPG_NS" get secret "${PG_CLUSTER}-apprafter-admin" \
    -o jsonpath='{.data.password}' 2>/dev/null | base64 -d 2>/dev/null || true)
[ -n "$PG_ADMIN_PW" ] || die "the platform role's password Secret was never created"
GROUPS=$(psql_as apprafter_admin "$PG_ADMIN_PW" postgres \
    "SELECT rolname FROM pg_roles WHERE rolname IN ('${PG_GROUP}','${PG_READER_GROUP}') ORDER BY 1;")
contains "the owning group exists on the server" "$GROUPS" "$PG_GROUP"
contains "the reader group exists on the server" "$GROUPS" "$PG_READER_GROUP"

DB_OWNER=$(psql_as apprafter_admin "$PG_ADMIN_PW" postgres \
    "SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname='${PG_DB_NAME}';")
check "the database is owned by the GROUP, not by a consumer" "$DB_OWNER" "$PG_GROUP"

# The platform role must NOT be a superuser: it creates roles, and that is all
# it is allowed to do. A superuser here would also mean CREATE EXTENSION ran
# for anything a manifest named rather than only the allow list.
IS_SUPER=$(psql_as apprafter_admin "$PG_ADMIN_PW" postgres \
    "SELECT rolsuper FROM pg_roles WHERE rolname='apprafter_admin';")
check "the platform role is not a superuser" "$IS_SUPER" "f"

# ===============================================================
phase "Phase 4: the FIRST bind is gated (ADR 0052 trigger #17)"
# ===============================================================
kubectl apply -f - >/dev/null <<YAML || die "applying ${APP_RW}"
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: {name: ${APP_RW}, namespace: ${APP_NS}}
spec:
  base:
    image: nginxdemos/hello:plain-text
    needs:
      pg:
        ref: ${PG_DB}
YAML

# The gate is on the FIRST bind of the pair, so it must fire here — and the
# application must NOT generate a claim while it waits, or the binding would
# have happened and the approval would be decoration.
printf '  waiting for the bind MigrationPlan ...\n'
PLAN=""
_deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$_deadline" ]; do
    PLAN=$(kubectl -n "$APP_NS" get "$PLAN_RES" -o name 2>/dev/null | head -1 || true)
    [ -n "$PLAN" ] && break
    sleep 5
done
[ -n "$PLAN" ] || die "no MigrationPlan was created for the first bind — the gate did not fire"
PLAN_NAME="${PLAN#*/}"
check "the plan's trigger names the bind" \
    "$(jp "$PLAN_RES" "$APP_NS" "$PLAN_NAME" '{.spec.trigger.type}')" "shared-database-bind"
check "the bind is classified as a security boundary" \
    "$(jp "$PLAN_RES" "$APP_NS" "$PLAN_NAME" '{.spec.classification}')" "security-boundary"
contains "the plan names the database and the access level" \
    "$(jp "$PLAN_RES" "$APP_NS" "$PLAN_NAME" '{.spec.trigger.to}')" "$PG_DB"

wait_jsonpath "$AR_APP" "$APP_NS" "$APP_RW" '{.status.phase}' AwaitingMigrationApproval 120 \
    || die "the application did not pause behind the gate"
CLAIMS_WHILE_GATED=$(kubectl -n "$APP_NS" get "$CLAIM_RES" -o name 2>/dev/null || true)
check "no claim is generated while the gate is pending" "$CLAIMS_WHILE_GATED" ""
# ...and the refCount must still read 0, because nothing is bound yet. A gate
# that let the count move would mean the exposure existed before approval.
check "refCount stays 0 while the bind waits for approval" \
    "$(jp "$SHDB_RES" "$APP_NS" "$PG_DB" '{.status.refCount}')" "0"

printf '  approving the plan ...\n'
kubectl -n "$APP_NS" patch "$PLAN_RES" "$PLAN_NAME" --type=merge --subresource=status \
    -p '{"status":{"phase":"Approved"}}' >/dev/null 2>&1 \
    || kubectl -n "$APP_NS" patch "$PLAN_RES" "$PLAN_NAME" --type=merge \
        -p '{"spec":{"approved":true}}' >/dev/null 2>&1 \
        || die "could not approve the MigrationPlan"

# ===============================================================
phase "Phase 5: the rw consumer binds — claim, role, Secret"
# ===============================================================
wait_jsonpath "$CLAIM_RES" "$APP_NS" "${APP_RW}-pg" '{.status.ready}' true 300 \
    || die "the rw claim never bound: $(cond_reason "$CLAIM_RES" "$APP_NS" "${APP_RW}-pg" Ready) / $(cond_message "$CLAIM_RES" "$APP_NS" "${APP_RW}-pg" Ready)"

contains "the claim's Ready message says it BOUND rather than provisioned" \
    "$(cond_message "$CLAIM_RES" "$APP_NS" "${APP_RW}-pg" Ready)" "bound to shared database"
check "the claim carries the sharedRef it was generated from" \
    "$(jp "$CLAIM_RES" "$APP_NS" "${APP_RW}-pg" '{.spec.sharedRef}')" "$PG_DB"

# The Secret shape is the contract every downstream piece reads. Assert the
# database is the SHARED one and the user is this consumer's own — the two
# facts that together say "shared data, private credential".
check "the connection Secret points at the shared database" \
    "$(conn_key "${APP_RW}-pg" db)" "$PG_DB_NAME"
check "the connection Secret carries this consumer's own role" \
    "$(conn_key "${APP_RW}-pg" user)" "$RW_ROLE"
RW_PW=$(conn_key "${APP_RW}-pg" pass)
[ -n "$RW_PW" ] || die "the connection Secret has no password"

wait_jsonpath "$SHDB_RES" "$APP_NS" "$PG_DB" '{.status.refCount}' 1 120 \
    || die "refCount did not rise to 1 after the first bind"

# ===============================================================
phase "Phase 6: a second bind of the SAME pair does not re-gate"
# ===============================================================
# Re-applying an unchanged spec must not produce a second plan. A gate that
# re-fires on every reconcile is one nobody can leave switched on.
PLANS_BEFORE=$(kubectl -n "$APP_NS" get "$PLAN_RES" -o name 2>/dev/null | wc -l | tr -d ' ')
kubectl -n "$APP_NS" annotate "$AR_APP" "$APP_RW" \
    "walk.apprafter.io/touch=$(date +%s)" --overwrite >/dev/null
sleep 20
PLANS_AFTER=$(kubectl -n "$APP_NS" get "$PLAN_RES" -o name 2>/dev/null | wc -l | tr -d ' ')
check "no new MigrationPlan on a re-reconcile of an existing binding" \
    "$PLANS_AFTER" "$PLANS_BEFORE"

# ===============================================================
phase "Phase 7: a second application binds ro — and the data is SHARED"
# ===============================================================
kubectl apply -f - >/dev/null <<YAML || die "applying ${APP_RO}"
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: {name: ${APP_RO}, namespace: ${APP_NS}}
spec:
  base:
    image: nginxdemos/hello:plain-text
    needs:
      pg:
        ref: ${PG_DB}
        access: ro
YAML

printf '  approving the second bind ...\n'
_deadline=$(( $(date +%s) + 180 ))
RO_PLAN=""
while [ "$(date +%s)" -lt "$_deadline" ]; do
    RO_PLAN=$(kubectl -n "$APP_NS" get "$PLAN_RES" -o name 2>/dev/null \
        | grep -v "$PLAN_NAME" | head -1 || true)
    [ -n "$RO_PLAN" ] && break
    sleep 5
done
[ -n "$RO_PLAN" ] || die "the second application's first bind was not gated"
RO_PLAN_NAME="${RO_PLAN#*/}"
contains "the ro bind's plan records the access level" \
    "$(jp "$PLAN_RES" "$APP_NS" "$RO_PLAN_NAME" '{.spec.trigger.to}')" "ro"
kubectl -n "$APP_NS" patch "$PLAN_RES" "$RO_PLAN_NAME" --type=merge --subresource=status \
    -p '{"status":{"phase":"Approved"}}' >/dev/null 2>&1 \
    || kubectl -n "$APP_NS" patch "$PLAN_RES" "$RO_PLAN_NAME" --type=merge \
        -p '{"spec":{"approved":true}}' >/dev/null 2>&1 \
        || die "could not approve the ro MigrationPlan"

wait_jsonpath "$CLAIM_RES" "$APP_NS" "${APP_RO}-pg" '{.status.ready}' true 300 \
    || die "the ro claim never bound: $(cond_message "$CLAIM_RES" "$APP_NS" "${APP_RO}-pg" Ready)"
RO_PW=$(conn_key "${APP_RO}-pg" pass)
[ -n "$RO_PW" ] || die "the ro connection Secret has no password"

# THE PROPERTY. Not "the roles exist" — that a table one consumer creates is
# the other's to read, which is the whole reason for the feature.
psql_as "$RW_ROLE" "$RW_PW" "$PG_DB_NAME" \
    "CREATE TABLE IF NOT EXISTS shared_orders(id int primary key, note text);" >/dev/null \
    || die "the rw consumer could not create a table"
psql_as "$RW_ROLE" "$RW_PW" "$PG_DB_NAME" \
    "INSERT INTO shared_orders VALUES (1,'from-web') ON CONFLICT DO NOTHING;" >/dev/null \
    || die "the rw consumer could not insert"

TBL_OWNER=$(psql_as "$RW_ROLE" "$RW_PW" "$PG_DB_NAME" \
    "SELECT tableowner FROM pg_tables WHERE tablename='shared_orders';")
check "a table created by a consumer is owned by the GROUP" "$TBL_OWNER" "$PG_GROUP"

RO_READ=$(psql_as "$RO_ROLE" "$RO_PW" "$PG_DB_NAME" "SELECT note FROM shared_orders WHERE id=1;")
check "the ro consumer READS the rw consumer's row" "$RO_READ" "from-web"

RO_WRITE=$(psql_as "$RO_ROLE" "$RO_PW" "$PG_DB_NAME" \
    "INSERT INTO shared_orders VALUES (2,'from-reporter');" || true)
contains "the ro consumer's INSERT is refused" "$RO_WRITE" "permission denied"

# ===============================================================
phase "Phase 8: pgvector in the SHARED database"
# ===============================================================
VEC=$(psql_as "$RW_ROLE" "$RW_PW" "$PG_DB_NAME" \
    "SELECT extname FROM pg_extension WHERE extname IN ('vector','pg_trgm') ORDER BY 1;")
contains "vector is installed in the shared database" "$VEC" "vector"
contains "pg_trgm is installed in the shared database" "$VEC" "pg_trgm"
# Usable, not merely present: `CREATE EXTENSION` can succeed and the type
# still be unreachable from a consumer's search_path.
VEC_USE=$(psql_as "$RW_ROLE" "$RW_PW" "$PG_DB_NAME" \
    "SELECT '[1,2,3]'::vector <-> '[1,2,4]'::vector;" || true)
case "$VEC_USE" in
    "" | *ERROR*) die "the vector type is present but unusable: ${VEC_USE}" ;;
    *) printf '  ok: a consumer can actually USE the vector type (distance = %s)\n' "$VEC_USE" ;;
esac

# ===============================================================
phase "Phase 9: db rm is refused while bound, and names the binders"
# ===============================================================
check "refCount counts both consumers" \
    "$(jp "$SHDB_RES" "$APP_NS" "$PG_DB" '{.status.refCount}')" "2"

# The APISERVER refuses it too, not only the CLI. Without a webhook rule the
# delete is admitted, the finalizer holds the object in Terminating, and an
# operator sees a command that succeeded followed by an object that never goes
# away — a worse outcome than a refusal, because nothing says why. `kubectl`
# is the only way to reach that path; the CLI's own guard runs first.
KUBECTL_RM=$(kubectl -n "$APP_NS" delete "$SHDB_RES" "$PG_DB" 2>&1 || true)
contains "a raw kubectl delete is refused by the apiserver" "$KUBECTL_RM" "bound application"
contains "the apiserver's refusal points at the command that names them" \
    "$KUBECTL_RM" "apprafter db status"
STILL_THERE=$(kubectl -n "$APP_NS" get "$SHDB_RES" "$PG_DB" \
    -o jsonpath='{.metadata.deletionTimestamp}' 2>/dev/null || true)
check "the refused delete left no deletionTimestamp behind" "$STILL_THERE" ""

RM_OUT=$(apprafter db rm "$PG_DB" -n "$APP_NS" --yes 2>&1 || true)
contains "the refusal says the database is still bound" "$RM_OUT" "still has 2 bound"
# The half no unit test reaches: an operator told only a COUNT knows they are
# blocked and not by whom, and for a database the binding is one line inside a
# `needs` block in somebody's manifest.
contains "the refusal names the rw binder" "$RM_OUT" "${APP_RW}-pg"
contains "the refusal names the ro binder" "$RM_OUT" "${APP_RO}-pg"

printf '  removing both applications, then retrying the delete ...\n'
kubectl -n "$APP_NS" delete "$AR_APP" "$APP_RW" "$APP_RO" --wait=true --timeout=180s >/dev/null
wait_jsonpath "$SHDB_RES" "$APP_NS" "$PG_DB" '{.status.refCount}' 0 240 \
    || die "refCount never fell back to 0 after the consumers left"
# ...and the DATA is still there. This is the property the whole CRD exists
# for: a consumer's lifecycle never touches the shared database.
STILL=$(psql_as apprafter_admin "$PG_ADMIN_PW" "$PG_DB_NAME" \
    "SELECT note FROM shared_orders WHERE id=1;")
check "the shared data survived both consumers being deleted" "$STILL" "from-web"

# REVOCATION, which is the guide's own promise and the reason a consumer gets
# its own login at all. The connection Secret cascades with the claim either
# way, so its disappearance proves nothing — what has to be gone is the ROLE
# on the server. Without this assertion a leaked role is invisible: the
# database keeps working, refCount falls to 0, and a credential somebody may
# have captured goes on working against the shared data forever.
printf '  waiting for the consumer roles to be revoked ...\n'
_deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$_deadline" ]; do
    LEFT=$(psql_as apprafter_admin "$PG_ADMIN_PW" postgres \
        "SELECT count(*) FROM pg_roles WHERE rolname IN ('${RW_ROLE}','${RO_ROLE}');")
    [ "$LEFT" = "0" ] && break
    sleep 5
done
check "both consumer roles are gone from the server" "$LEFT" "0"
# ...and the GROUPS are still there, because the database is. Dropping a
# consumer must not take the shared structure with it.
GROUPS_LEFT=$(psql_as apprafter_admin "$PG_ADMIN_PW" postgres \
    "SELECT count(*) FROM pg_roles WHERE rolname IN ('${PG_GROUP}','${PG_READER_GROUP}');")
check "the shared database's groups survive a consumer leaving" "$GROUPS_LEFT" "2"

apprafter db rm "$PG_DB" -n "$APP_NS" --yes >/dev/null || die "db rm at refCount 0"

# The delete drops the groups too — they exist for this database and nothing
# else. Asserted because `drop_backing`'s own doc comment claimed to do this
# for a while before any builder existed to do it with.
printf '  waiting for the groups to be dropped ...\n'
_deadline=$(( $(date +%s) + 240 ))
while [ "$(date +%s)" -lt "$_deadline" ]; do
    GROUPS_NOW=$(psql_as apprafter_admin "$PG_ADMIN_PW" postgres \
        "SELECT count(*) FROM pg_roles WHERE rolname IN ('${PG_GROUP}','${PG_READER_GROUP}');")
    [ "$GROUPS_NOW" = "0" ] && break
    sleep 5
done
check "the groups are dropped with the database" "$GROUPS_NOW" "0"
_deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$_deadline" ]; do
    kubectl -n "$APP_NS" get "$SHDB_RES" "$PG_DB" >/dev/null 2>&1 || break
    sleep 5
done
kubectl -n "$APP_NS" get "$SHDB_RES" "$PG_DB" >/dev/null 2>&1 \
    && die "the SharedDatabase was never actually deleted (finalizer stuck?)"
printf '  ok: the database deletes once nothing is bound\n'

# ===============================================================
phase "Phase 10: a shared Redis keyspace — two consumers, one channel space"
# ===============================================================
apprafter db create "$CACHE_DB" --type redis -n "$APP_NS" \
    || die "apprafter db create (redis)"
wait_jsonpath "$SHDB_RES" "$APP_NS" "$CACHE_DB" '{.status.ready}' true 420 \
    || die "the shared cache never became ready: $(cond_message "$SHDB_RES" "$APP_NS" "$CACHE_DB" Ready)"
CACHE_INSTANCE=$(jp "$SHDB_RES" "$APP_NS" "$CACHE_DB" '{.status.instance}')
CACHE_DBNUM=$(jp "$SHDB_RES" "$APP_NS" "$CACHE_DB" '{.status.dbnum}')
[ -n "$CACHE_INSTANCE" ] || die "the shared cache published no instance"
[ -n "$CACHE_DBNUM" ] || die "the shared cache published no dbnum"
printf '  shared cache: %s $%s\n' "$CACHE_INSTANCE" "$CACHE_DBNUM"

for _app in "$APP_CACHE_A" "$APP_CACHE_B"; do
    kubectl apply -f - >/dev/null <<YAML || die "applying ${_app}"
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: {name: ${_app}, namespace: ${APP_NS}}
spec:
  base:
    image: nginxdemos/hello:plain-text
    needs:
      redis:
        ref: ${CACHE_DB}
YAML
done

printf '  approving both cache binds ...\n'
_deadline=$(( $(date +%s) + 240 ))
while [ "$(date +%s)" -lt "$_deadline" ]; do
    _pending=$(kubectl -n "$APP_NS" get "$PLAN_RES" -o name 2>/dev/null || true)
    [ -z "$_pending" ] && { sleep 5; continue; }
    for _p in $_pending; do
        kubectl -n "$APP_NS" patch "$PLAN_RES" "${_p#*/}" --type=merge --subresource=status \
            -p '{"status":{"phase":"Approved"}}' >/dev/null 2>&1 \
            || kubectl -n "$APP_NS" patch "$PLAN_RES" "${_p#*/}" --type=merge \
                -p '{"spec":{"approved":true}}' >/dev/null 2>&1 || true
    done
    _a=$(jp "$CLAIM_RES" "$APP_NS" "${APP_CACHE_A}-redis" '{.status.ready}')
    _b=$(jp "$CLAIM_RES" "$APP_NS" "${APP_CACHE_B}-redis" '{.status.ready}')
    [ "$_a" = "true" ] && [ "$_b" = "true" ] && break
    sleep 5
done
check "the first cache consumer bound" \
    "$(jp "$CLAIM_RES" "$APP_NS" "${APP_CACHE_A}-redis" '{.status.ready}')" "true"
check "the second cache consumer bound" \
    "$(jp "$CLAIM_RES" "$APP_NS" "${APP_CACHE_B}-redis" '{.status.ready}')" "true"

# Both must be pinned to the DATABASE's `$N`, not to one of their own.
A_URL=$(conn_key "${APP_CACHE_A}-redis" url)
B_URL=$(conn_key "${APP_CACHE_B}-redis" url)
contains "the first consumer is pinned to the shared keyspace" "$A_URL" "/${CACHE_DBNUM}"
contains "the second consumer is pinned to the same keyspace" "$B_URL" "/${CACHE_DBNUM}"

# THE channel-prefix property. A per-user prefix passes every unit test and
# leaves these two unable to hear each other, which is most of what sharing a
# cache is for.
A_PREFIX=$(conn_key "${APP_CACHE_A}-redis" channelPrefix)
B_PREFIX=$(conn_key "${APP_CACHE_B}-redis" channelPrefix)
check "both consumers are given the SAME channel prefix" "$A_PREFIX" "$B_PREFIX"
contains "the prefix is the database's, not a user's" "$A_PREFIX" "shd_${APP_NS}_${CACHE_DB}"

# And a write by one is readable by the other — the keyspace really is one.
A_USER=$(conn_key "${APP_CACHE_A}-redis" user)
A_PASS=$(conn_key "${APP_CACHE_A}-redis" pass)
B_USER=$(conn_key "${APP_CACHE_B}-redis" user)
B_PASS=$(conn_key "${APP_CACHE_B}-redis" pass)
DF_POD=$(kubectl -n "$DF_NS" get pods -l app="$CACHE_INSTANCE" \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
[ -n "$DF_POD" ] || DF_POD=$(kubectl -n "$DF_NS" get pods \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
[ -n "$DF_POD" ] || die "could not find the Dragonfly pod"
kubectl -n "$DF_NS" exec -i "$DF_POD" -- \
    redis-cli --user "$A_USER" --pass "$A_PASS" --no-auth-warning \
    -n "$CACHE_DBNUM" SET walk:key from-pub >/dev/null 2>&1 \
    || die "the first consumer could not write to the shared keyspace"
GOT=$(kubectl -n "$DF_NS" exec -i "$DF_POD" -- \
    redis-cli --user "$B_USER" --pass "$B_PASS" --no-auth-warning \
    -n "$CACHE_DBNUM" GET walk:key 2>/dev/null | tr -d '\r' || true)
check "the second consumer reads what the first wrote" "$GOT" "from-pub"

# ===============================================================
phase "Summary"
# ===============================================================
printf '\n=== shared-database-walk: ALL PHASES GREEN ===\n'
