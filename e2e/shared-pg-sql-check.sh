#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Do the shared-database SQL builders actually work? (2.29 / ADR 0066 §3.1)
#
# WHY THIS EXISTS
# ---------------
# `shared_pg.rs` builds statement STRINGS, and its unit tests assert what those
# strings contain. That is necessary and not sufficient: a string can contain
# exactly what you meant and still be rejected by the server. It already was —
# the first version of the module emitted `PASSWORD $1` and documented that the
# caller would bind it, and BOTH halves of that are impossible (a DO block takes
# no parameters; a utility statement cannot be prepared). No assertion about
# string content would have caught it.
#
# So this executes the builders' OWN output — taken from the module, never
# retyped — against a real PostgreSQL of the operand major, and then asserts the
# property the whole role model exists for:
#
#   * a table created by one rw consumer is owned by the GROUP, not by it;
#   * a SECOND rw consumer reads and writes that table;
#   * the ro consumer reads it and its INSERT is refused;
#   * the platform role is not a superuser at any point.
#
# The password used carries a single quote and a `$$` pair, because those are
# the two characters that could end a statement early.
#
# Usage:  bash e2e/shared-pg-sql-check.sh
# Judge it by READING ITS LOG: every assertion prints `ok:`, last line GREEN.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="docker.io/library/postgres:18-alpine"
CTR="apprafter-shared-pg-check"
PGPORT_HOST="${APPRAFTER_PG_CHECK_PORT:-15433}"
PW="p'w\$\$q"

BUILDER=podman
command -v podman >/dev/null 2>&1 || BUILDER=docker

fail() { printf 'FAILED: %s\n' "$1" >&2; exit 1; }
cleanup() { "$BUILDER" rm -f "$CTR" >/dev/null 2>&1 || true; }
trap cleanup EXIT

# NOTE the `< /dev/null`, and the absent `-i`. These calls run inside
# `while read` loops whose stdin is the list of statements; a container exec
# that inherits that stdin SWALLOWS the remaining lines, and the loop silently
# runs only its first iteration. Observed exactly that: one statement executed,
# four vanished, and the failure surfaced later as "database does not exist".
psql_as() { # <role> <db> <sql>
    "$BUILDER" exec "$CTR" psql -U "$1" -d "$2" -h 127.0.0.1 -v ON_ERROR_STOP=1 -tAc "$3" \
        < /dev/null 2>&1
}

printf '=== 1/6  the statements, from the module itself ===\n'
SQL=$(cd "${REPO_ROOT}/operator" && cargo test -q -p operator-controllers-resourceclaim-provisioner \
    print_statements_for_the_live_sql_check -- --ignored --nocapture 2>/dev/null \
    | sed -n '/-- @@SETUP/,/-- @@END/p')
[ -n "$SQL" ] || fail "the builder test printed nothing — the statement source is missing"
SETUP=$(printf '%s\n' "$SQL" | sed -n '/-- @@SETUP/,/-- @@INDB/p' | grep -v '^-- @@')
INDB=$(printf '%s\n' "$SQL" | sed -n '/-- @@INDB/,/-- @@END/p' | grep -v '^-- @@')
printf '  ok: %s setup statement(s), %s in-database statement(s)\n' \
    "$(printf '%s\n' "$SETUP" | grep -c .)" "$(printf '%s\n' "$INDB" | grep -c .)"

printf '\n=== 2/6  a PostgreSQL of the operand major ===\n'
"$BUILDER" rm -f "$CTR" >/dev/null 2>&1 || true
# The port is published for step 6: rootless podman gives the container no
# address of its own that the host can dial, so `inspect .IPAddress` is empty
# and a published port is the only route in.
"$BUILDER" run -d --name "$CTR" -p "${PGPORT_HOST}:5432" -e POSTGRES_PASSWORD=x "$IMAGE" >/dev/null
for _ in $(seq 1 30); do
    "$BUILDER" exec "$CTR" pg_isready -q 2>/dev/null && break
    sleep 2
done
VER=$(psql_as postgres postgres "SELECT version()" | head -1)
printf '  ok: %s\n' "${VER:0:40}"

printf '\n=== 3/6  the platform role, as CNPG managed.roles would create it ===\n'
# The ONE role CNPG creates. Not a superuser — that is the point of the whole
# arrangement, and step 5 asserts it again at the end.
psql_as postgres postgres "CREATE ROLE apprafter_admin LOGIN PASSWORD 'a' CREATEROLE CREATEDB;" >/dev/null \
    || fail "creating the platform role"
printf '  ok: apprafter_admin created with CREATEROLE CREATEDB, no superuser\n'

printf '\n=== 4/6  the builders run, as apprafter_admin ===\n'
printf '%s\n' "$SETUP" | while IFS= read -r stmt; do
    [ -n "$stmt" ] || continue
    out=$(psql_as apprafter_admin postgres "$stmt") || {
        # Never echo the statement: a bind statement carries the password.
        printf 'FAILED: a setup statement was rejected: %s\n' "$out" >&2
        exit 1
    }
done || fail "setup statements"
printf '  ok: groups created, membership granted, database created\n'
printf '%s\n' "$INDB" | while IFS= read -r stmt; do
    [ -n "$stmt" ] || continue
    out=$(psql_as apprafter_admin shd_apps_orders "$stmt") || {
        printf 'FAILED: an in-database statement was rejected: %s\n' "$out" >&2
        exit 1
    }
done || fail "in-database statements"
printf '  ok: reader grants applied, three consumers bound\n'

printf '\n=== 5/6  the property the role model exists for ===\n'
OWNER=$("$BUILDER" exec -e PGPASSWORD="$PW" "$CTR" psql -U claim_apps_web_pg -d shd_apps_orders \
    -h 127.0.0.1 -v ON_ERROR_STOP=1 -tAc \
    "CREATE TABLE orders(id int primary key, note text); INSERT INTO orders VALUES (1,'from-web'); \
     SELECT tableowner FROM pg_tables WHERE tablename='orders';" 2>&1 | tail -1)
[ "$OWNER" = "shd_apps_orders" ] \
    || fail "a table created by a consumer is owned by '${OWNER}', not the group — SET ROLE did not take, and the next consumer will not see this table"
printf '  ok: a consumer-created table is owned by the GROUP (%s)\n' "$OWNER"

SECOND=$("$BUILDER" exec -e PGPASSWORD="$PW" "$CTR" psql -U claim_apps_api_pg -d shd_apps_orders \
    -h 127.0.0.1 -v ON_ERROR_STOP=1 -tAc \
    "SELECT note FROM orders; INSERT INTO orders VALUES (2,'from-api'); SELECT 'wrote';" 2>&1 | tail -1)
[ "$SECOND" = "wrote" ] || fail "a second rw consumer could not read/write the first's table: ${SECOND}"
printf '  ok: a SECOND rw consumer reads and writes it\n'

RO_READ=$("$BUILDER" exec -e PGPASSWORD="$PW" "$CTR" psql -U claim_apps_rep_pg -d shd_apps_orders \
    -h 127.0.0.1 -tAc "SELECT count(*) FROM orders;" 2>&1 | tail -1 || true)
[ "$RO_READ" = "2" ] || fail "the ro consumer could not read: ${RO_READ}"
# `|| true`: this INSERT is SUPPOSED to fail, and under `set -e` a failing
# command substitution kills the script before the `case` below can read it —
# which is how an assertion that the platform REFUSES something turns into a
# silent early exit that looks like a pass to anyone reading the exit code.
RO_WRITE=$("$BUILDER" exec -e PGPASSWORD="$PW" "$CTR" psql -U claim_apps_rep_pg -d shd_apps_orders \
    -h 127.0.0.1 -tAc "INSERT INTO orders VALUES (3,'nope');" 2>&1 | tail -1 || true)
case "$RO_WRITE" in
    *"permission denied"*) printf '  ok: the ro consumer reads (%s rows) and its INSERT is refused\n' "$RO_READ" ;;
    *) fail "the ro consumer was ALLOWED to write: ${RO_WRITE}" ;;
esac

SUPER=$(psql_as postgres postgres "SELECT rolsuper FROM pg_roles WHERE rolname='apprafter_admin'" | tail -1)
[ "$SUPER" = "f" ] || fail "the platform role ended up a superuser (${SUPER})"
printf '  ok: the platform role is still not a superuser\n'

printf '\n=== 6/6  the Rust client drives the same sequence ===\n'
# The steps above prove the STATEMENTS, by piping them through psql. This
# proves the CLIENT: that execute_all sequences a batch the way psql does, that
# a failure is attributed to the right statement INDEX, and that the error
# carries no statement text — which matters because a bind statement in that
# same batch shape carries a password. psql cannot observe any of those.
CLIENT_OUT=$(cd "${REPO_ROOT}/operator" \
    && APPRAFTER_PG_SMOKE_DSN="postgresql://postgres:x@127.0.0.1:${PGPORT_HOST}/postgres" \
       cargo test -q -p operator-controllers-resourceclaim-provisioner \
       --test pg_client_smoke_test -- --ignored 2>&1 || true)
case "$CLIENT_OUT" in
    *"1 passed"*) printf '  ok: execute_all, extension_available and the indexed failure all behave\n' ;;
    *) printf '%s\n' "$CLIENT_OUT" | tail -20 >&2
       fail "the Postgres client smoke test did not pass" ;;
esac

printf '\nGREEN — the shared-database SQL builders and the client both execute.\n'
