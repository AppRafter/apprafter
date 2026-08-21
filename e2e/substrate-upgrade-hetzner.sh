#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# SUBSTRATE UPGRADE on REAL Hetzner — move a live cluster onto a BIGGER machine
# via backup → destroy → `restore --reprovision --server-type`, and prove the
# workload AND every byte of its Postgres data came across.
#
# This is a PLANNED substrate upgrade, not disaster recovery: same target, same
# Hetzner project, same region, bigger box. It is exactly the move this repo's
# own dogfood cluster needs (its 4 GB node hit 99 % of memory requests on
# 2026-08-21).
#
#     cx23  (2 cores, 4.0 GB, 40 GB)  ->  cx33  (4 cores, 8.0 GB, 80 GB)   @ hel1
#
# The capability is SHIPPED (`apprafter restore --reprovision --server-type`);
# this walk PROVES it, it does not build it.
#
# Flow (phases mirror the log banners):
#   0  setup — fresh config dir, one token, teardown trap armed BEFORE any spend
#   1  provision + bootstrap on the SMALL SKU; assert the landed SKU from the
#      HETZNER API (not local state); record server id + type
#   2  deploy the REAL landing CMS (needs.pg + a `secret:` ref, --env dev) and
#      assert Ready=True (proves the secret resolved, not EnvSecretMissing)
#   3  THE DATA FINGERPRINT — wake the CMS so it creates its REAL schema (its
#      Payload adapter migrates + seeds LAZILY, on the first request to reach a
#      payload route, so a pod that is merely Ready has written nothing), seed a
#      human-readable marker + 500 rows of varied types, wait for the database
#      to go quiescent, then build a deterministic digest of EVERY user table in
#      the CMS application database (row count + a content hash over the rows
#      with an explicit deterministic ORDER BY). Computed TWICE and asserted
#      identical, because a digest that is not reproducible turns this walk into
#      either a false alarm or a false pass and you cannot tell which.
#   4  back up (see BACKENDS below)
#   5  `apprafter destroy` — API-assert the project is back to ZERO servers and
#      that the backup artifact survived the cluster
#   6  `apprafter restore <repo> --reprovision --server-type cx33 --target <same>`
#   7  VERIFY THE UPGRADE, not just the restore: type is now the BIG SKU (from
#      the API), server id CHANGED, node allocatable memory GREW, CMS reaches
#      Ready on its own, the `secret:` ref resolves (re-sealed), the marker row
#      reads back, and the FULL DIGEST IS BYTE-IDENTICAL to phase 3 with the row
#      counts matching table for table
#   8  teardown, with an API-verified zero-server check
#
# BACKENDS — one switch, `APPRAFTER_SUBSTRATE_BACKEND`, shared by every other
# phase (the two legs do NOT fork the script):
#   local  (default, "Leg A") — `apprafter backup create` into a host-local
#          restic repo that survives the destroy. Needs only a Hetzner token.
#   s3     ("Leg B")          — `apprafter backup enable` against a real S3
#          bucket, trigger the in-cluster CronJob runner, assert a NEW snapshot
#          lands off-site, then restore from the `s3:` URL. Needs the S3 vars.
#
# TEARDOWN-SAFE: the `trap ... EXIT` destroys whatever cluster is up, sweeps the
# project, and API-verifies ZERO resources (LEAK WARNING + non-zero exit
# otherwise). The trap is armed BEFORE the first provision.
#
# COST AWARENESS: two short-lived boxes (one cx23, one cx33) in hel1 for well
# under an hour — around EUR 0.02. What matters is not leaking a server.
#
# NO SILENT SKIPS: a missing precondition EXITS 2 before anything is provisioned.
# This walk never returns green for a gate that did not run.
#
# Env — REQUIRED:
#   HCLOUD_TOKEN | OLD_CLUSTER_TOKEN  Hetzner project token. A substrate upgrade
#                                     is ONE project, so the "old cluster" token
#                                     is the only one needed. Sourcing the repo's
#                                     gitignored backup-test.env exports the
#                                     OLD_CLUSTER_TOKEN spelling directly:
#                                       set -a; . ./backup-test.env; set +a
# Env — REQUIRED for BACKEND=s3 only:
#   S3_ACCESS_KEY_ID     | S3_BUCKET_ACCESS_KEY   S3 access key.
#   S3_SECRET_ACCESS_KEY | S3_BUCKET_SECRET_KEY   S3 secret key.
#   S3_ENDPOINT          | S3_BUCKET_BASE_URL     endpoint HOST (no scheme),
#                                                 e.g. hel1.your-objectstorage.com
#   S3_BUCKET            | S3_BUCKET_NAME         bucket name.
#   RESTIC_PASSWORD                               restic repo passphrase.
# Env — OPTIONAL:
#   APPRAFTER_SUBSTRATE_BACKEND=local|s3   backup backend (default local).
#   APPRAFTER_SUBSTRATE_SKU_SMALL=cx23     the "too small" SKU to start on.
#   APPRAFTER_SUBSTRATE_SKU_BIG=cx33       the SKU to upgrade onto.
#   APPRAFTER_SUBSTRATE_REGION=hel1        Hetzner region (both SKUs must exist).
#   APPRAFTER_SUBSTRATE_TARGET=subup-e2e   target name.
#   APPRAFTER_SUBSTRATE_S3_PREFIX=...      path prefix inside the bucket.
#   S3_REGION                              S3 region, when the provider needs it.
#   APPRAFTER_SSH_PUBLIC_KEY_PATH          ssh public key (default ~/.ssh/id_ed25519.pub).
#   RESTIC_PASSWORD                        also used for the BACKEND=local repo.
#   APPRAFTER_HETZNER_SKIP_DESTROY=1       leave the cluster UP for debugging
#                                          (destroy it by hand afterwards).
#
# Exit: 0 = GREEN; 1 = an assertion failed (the trap tears the cluster down
#       first) or teardown could not reach zero; 2 = a precondition is missing
#       (checked BEFORE the destroy trap is armed, so nothing was ever spent).
#       PASS/FAIL is judged by READING THE LOG — the `FAILED:` marker, every
#       phase's `ok:` lines, and the final GREEN banner — because these walks
#       are often run under wrappers that mask the inner exit code.

set -euo pipefail

# shellcheck source=e2e/lib.sh
source "$(cd "$(dirname "$0")" && pwd)/lib.sh"

# ---------------------------------------------------------------
# The single switch: which backup backend both legs share.
# ---------------------------------------------------------------
BACKEND="${APPRAFTER_SUBSTRATE_BACKEND:-local}"
case "$BACKEND" in
    local|s3) : ;;
    *) printf 'ERROR: APPRAFTER_SUBSTRATE_BACKEND must be "local" or "s3" (got %q)\n' "$BACKEND" >&2; exit 2 ;;
esac

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------
TARGET="${APPRAFTER_SUBSTRATE_TARGET:-subup-e2e}"
REGION="${APPRAFTER_SUBSTRATE_REGION:-hel1}"
SKU_SMALL="${APPRAFTER_SUBSTRATE_SKU_SMALL:-cx23}"
SKU_BIG="${APPRAFTER_SUBSTRATE_SKU_BIG:-cx33}"
SSH_PUB="${APPRAFTER_SSH_PUBLIC_KEY_PATH:-$HOME/.ssh/id_ed25519.pub}"

CMS_REPO="https://github.com/AppRafter/apprafter"
CMS_PATH="landing/cms/apprafter"
CMS_APP="landing-cms"
CMS_NS="apprafter"
CMS_ENV="dev"

CMS_SECRET_NAME="apprafter-landing-cms-secrets"
CMS_SECRET_KEY="PAYLOAD_SECRET"
CMS_SECRET_VALUE="${APPRAFTER_SUBSTRATE_FAKE_SECRET:-subup-walk-fake-payload-secret-0123456789abcdef}"

MARKER_TABLE="substrate_walk_marker"
MARKER_VALUE="substrate-known-payload-$(date +%Y%m%d%H%M%S)"
DATA_TABLE="substrate_walk_data"
DATA_ROWS=500

APP_RES="application.apprafter.io"
BACKUP_NS="apprafter-system"
BACKUP_CRONJOB="apprafter-backup"

# Hetzner token — one project, so the "old cluster" token is the only one.
HZ_TOKEN="${HCLOUD_TOKEN:-${OLD_CLUSTER_TOKEN:-}}"

# ---------------------------------------------------------------
# Preconditions — checked BEFORE the trap is armed (exit 2, never destroys,
# never silently skips).
# ---------------------------------------------------------------
for t in kubectl curl python3 md5sum diff cmp; do
    command -v "$t" >/dev/null 2>&1 || { printf 'ERROR: missing tool: %s\n' "$t" >&2; exit 2; }
done
# restic must be resolvable by the CLI SUBPROCESS (Command::new("restic")), not
# just by this shell; lib.sh installs a `nix run nixpkgs#restic` wrapper on PATH.
ensure_restic_on_path
command -v restic >/dev/null 2>&1 || { printf 'ERROR: restic not resolvable even after the nix wrapper install\n' >&2; exit 2; }
printf 'restic: %s\n' "$(command -v restic)"

if [ -z "$HZ_TOKEN" ]; then
    printf 'ERROR: no Hetzner token. Set HCLOUD_TOKEN, or source the gitignored creds file:\n' >&2
    printf '         set -a; . ./backup-test.env; set +a   # exports OLD_CLUSTER_TOKEN\n' >&2
    exit 2
fi
[ -r "$SSH_PUB" ] || { printf 'ERROR: ssh public key not readable: %s (set APPRAFTER_SSH_PUBLIC_KEY_PATH)\n' "$SSH_PUB" >&2; exit 2; }

# restic passphrase for the BACKEND=local repo — the real one when the creds
# file is sourced, else a walk-local constant so Leg A needs only the token.
RESTIC_PASS="${RESTIC_PASSWORD:-substrate-upgrade-walk-restic-2026}"

# S3 knobs — accept both the neutral spellings and the backup-test.env ones.
S3_KEY="${S3_ACCESS_KEY_ID:-${S3_BUCKET_ACCESS_KEY:-}}"
S3_SECRET="${S3_SECRET_ACCESS_KEY:-${S3_BUCKET_SECRET_KEY:-}}"
S3_ENDPOINT_RAW="${S3_ENDPOINT:-${S3_BUCKET_BASE_URL:-}}"
S3_BUCKET_NAME_R="${S3_BUCKET:-${S3_BUCKET_NAME:-}}"
S3_REGION_V="${S3_REGION:-}"
S3_PREFIX="${APPRAFTER_SUBSTRATE_S3_PREFIX:-substrate-upgrade-e2e}"
# `--endpoint` takes a bare host; strip a scheme if the creds file carries one.
S3_HOST="${S3_ENDPOINT_RAW#http://}"
S3_HOST="${S3_HOST#https://}"
S3_HOST="${S3_HOST%/}"
S3_REPO="s3:https://${S3_HOST}/${S3_BUCKET_NAME_R}/${S3_PREFIX}"

if [ "$BACKEND" = "s3" ]; then
    _missing=""
    [ -n "$S3_KEY" ]            || _missing="${_missing} S3_ACCESS_KEY_ID|S3_BUCKET_ACCESS_KEY"
    [ -n "$S3_SECRET" ]         || _missing="${_missing} S3_SECRET_ACCESS_KEY|S3_BUCKET_SECRET_KEY"
    [ -n "$S3_HOST" ]           || _missing="${_missing} S3_ENDPOINT|S3_BUCKET_BASE_URL"
    [ -n "$S3_BUCKET_NAME_R" ]  || _missing="${_missing} S3_BUCKET|S3_BUCKET_NAME"
    [ -n "${RESTIC_PASSWORD:-}" ] || _missing="${_missing} RESTIC_PASSWORD"
    if [ -n "$_missing" ]; then
        printf 'ERROR: APPRAFTER_SUBSTRATE_BACKEND=s3 needs the off-site credentials. Missing:%s\n' "$_missing" >&2
        printf '       Source the gitignored creds file:  set -a; . ./backup-test.env; set +a\n' >&2
        exit 2
    fi
fi

HZ="python3 ${REPO_ROOT}/e2e/hz.py"

# BASELINE: the project must START empty. This is checked HERE, before the
# teardown trap is armed, and not one line later — the trap's backstop SWEEPS
# the project, so arming it over somebody else's resources would delete them.
# shellcheck disable=SC2086 # $HZ is "python3 <path>" — word-splitting is intended
$HZ verify "$HZ_TOKEN" || {
    printf 'ERROR: the Hetzner project is NOT empty — refusing to run.\n' >&2
    printf '       This walk sweeps the project on teardown, so its baseline MUST be zero resources.\n' >&2
    exit 2
}
printf 'ok: Hetzner project baseline is empty (teardown may safely sweep)\n'

# ---------------------------------------------------------------
# Assertion + readout helpers. Style matches e2e/restore-reprovision-hetzner.sh
# so the two walks read the same.
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
assert_ne() {
    local desc="$1" got="$2" forbidden="$3"
    if [ "$got" != "$forbidden" ]; then
        printf '  ok: %s (%q != %q)\n' "$desc" "$got" "$forbidden"
        return 0
    fi
    printf 'FAILED: %s — value is unchanged (%q)\n' "$desc" "$got" >&2
    return 1
}
assert_contains() {
    local desc="$1" haystack="$2" needle="$3"
    if printf '%s' "$haystack" | grep -qF -- "$needle"; then
        printf '  ok: %s (found %q)\n' "$desc" "$needle"
        return 0
    fi
    printf 'FAILED: %s — %q not found in:\n%s\n' "$desc" "$needle" "$haystack" >&2
    return 1
}
wait_jsonpath() {
    local kind="$1" ns="$2" name="$3" jsonpath="$4" want="$5"
    local timeout="${6:-300}" deadline got
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait %s/%s [%s] == %q (timeout %ss) ...\n' "$kind" "$name" "$jsonpath" "$want" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        got=$(kubectl -n "$ns" get "$kind" "$name" -o jsonpath="$jsonpath" 2>/dev/null || true)
        if [ "$got" = "$want" ]; then
            printf '  ok: %s/%s [%s] = %q\n' "$kind" "$name" "$jsonpath" "$got"
            return 0
        fi
        printf '    %s: got=%q want=%q\n' "$(date +%H:%M:%S)" "$got" "$want"
        sleep 8
    done
    printf 'FAILED: %s/%s [%s] never became %q (last=%q)\n' "$kind" "$name" "$jsonpath" "$want" "${got:-}" >&2
    kubectl -n "$ns" describe "$kind" "$name" >&2 2>&1 || true
    return 1
}

# jp <kind> <ns> <name> <jsonpath> — read one value (uses $KUBECONFIG)
jp() { kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true; }

# ---------------------------------------------------------------
# Hetzner API helpers — the walk reads the SUBSTRATE FACTS (id, SKU, count)
# from the provider, never from local state, because local state is exactly
# what an upgrade could be lying about.
# ---------------------------------------------------------------
_hz_servers_json() {
    { set +x; } 2>/dev/null
    curl -fsS -H "Authorization: Bearer ${HZ_TOKEN}" \
        "https://api.hetzner.cloud/v1/servers" 2>/dev/null || true
}
hetzner_server_count() {
    local body; body="$(_hz_servers_json)"
    [ -n "$body" ] || { printf '?'; return 0; }
    printf '%s' "$body" | python3 -c 'import json,sys
d=sys.stdin.read().strip()
try: print(len(json.loads(d).get("servers", [])) if d else "?")
except Exception: print("?")'
}
hetzner_first_server_id() {
    local body; body="$(_hz_servers_json)"
    [ -n "$body" ] || { printf ''; return 0; }
    printf '%s' "$body" | python3 -c 'import json,sys
d=sys.stdin.read().strip()
try:
    s=json.loads(d).get("servers", [])
    print(s[0]["id"] if s else "")
except Exception: print("")'
}
hetzner_first_server_type() {
    local body; body="$(_hz_servers_json)"
    [ -n "$body" ] || { printf ''; return 0; }
    printf '%s' "$body" | python3 -c 'import json,sys
d=sys.stdin.read().strip()
try:
    s=json.loads(d).get("servers", [])
    print((s[0].get("server_type") or {}).get("name", "") if s else "")
except Exception: print("")'
}

# ---------------------------------------------------------------
# Node capacity — the number the SCHEDULER sees, which is the whole point of
# moving to a bigger box. Kubernetes reports quantities like `7838532Ki`.
# ---------------------------------------------------------------
mem_to_bytes() {
    local v="$1"
    case "$v" in
        *Ki) printf '%s' "$(( ${v%Ki} * 1024 ))" ;;
        *Mi) printf '%s' "$(( ${v%Mi} * 1048576 ))" ;;
        *Gi) printf '%s' "$(( ${v%Gi} * 1073741824 ))" ;;
        *[0-9]) printf '%s' "$v" ;;
        *) printf '0' ;;
    esac
}
node_allocatable_memory() {
    kubectl get nodes -o jsonpath='{.items[0].status.allocatable.memory}' 2>/dev/null || true
}
mib() { printf '%s' "$(( $1 / 1048576 ))"; }

# ---------------------------------------------------------------
# CMS + Postgres helpers (mirror e2e/restore-reprovision-hetzner.sh, but the
# claim / connection-Secret names are DISCOVERED from the CR rather than
# hardcoded, so a naming change surfaces as a readout instead of a 480 s stall).
# ---------------------------------------------------------------
cms_pod() {
    local running
    running=$(kubectl -n "$CMS_NS" get pod -l "app.kubernetes.io/name=$CMS_APP" \
        --field-selector=status.phase=Running \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
    if [ -n "$running" ]; then printf '%s' "$running"; return; fi
    kubectl -n "$CMS_NS" get pod -l "app.kubernetes.io/name=$CMS_APP" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}
pg_claim_name() {
    kubectl -n "$CMS_NS" get resourceclaim.apprafter.io \
        -o jsonpath='{.items[?(@.spec.type=="pg")].metadata.name}' 2>/dev/null \
        | awk '{print $1}'
}
# wait_pg_claim_ready — returns the claim name on stdout once status.ready=true.
wait_pg_claim_ready() {
    local timeout="${1:-600}" deadline name ready
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait: a pg ResourceClaim in ns %s to report status.ready=true (timeout %ss) ...\n' \
        "$CMS_NS" "$timeout" >&2
    while [ "$(date +%s)" -lt "$deadline" ]; do
        name="$(pg_claim_name)"
        if [ -n "$name" ]; then
            ready=$(jp resourceclaim.apprafter.io "$CMS_NS" "$name" '{.status.ready}')
            if [ "$ready" = "true" ]; then
                printf '  ok: pg ResourceClaim %s is ready\n' "$name" >&2
                printf '%s' "$name"
                return 0
            fi
        fi
        printf '    %s: claim=%q ready=%q\n' "$(date +%H:%M:%S)" "${name:-}" "${ready:-}" >&2
        sleep 8
    done
    printf 'FAILED: no pg ResourceClaim became ready in %s within %ss\n' "$CMS_NS" "$timeout" >&2
    kubectl -n "$CMS_NS" get resourceclaim.apprafter.io -o wide >&2 2>&1 || true
    return 1
}
pg_conn_secret() {
    jp resourceclaim.apprafter.io "$CMS_NS" "$1" '{.status.connectionSecretRef}'
}
conn_field() {
    kubectl -n "$CMS_NS" get secret "$1" -o jsonpath="{.data.$2}" 2>/dev/null | base64 -d 2>/dev/null || true
}

# --- pg access: exec into the CNPG PRIMARY as the postgres superuser over the
#     LOCAL unix socket (peer auth — no password, no TCP; `-c postgres` is
#     load-bearing). Same rationale as e2e/backup-restore-hetzner.sh. ---
cnpg_primary() {
    local p
    p=$(kubectl -n cnpg-system get pods -l 'cnpg.io/instanceRole=primary' -o name 2>/dev/null | head -1) || true
    [ -n "$p" ] || p=$(kubectl -n cnpg-system get pods -l 'role=primary' -o name 2>/dev/null | head -1) || true
    printf '%s' "$p"
}
# psql_super <db> <sql> — best-effort read (empty stdout + a stderr note on
# error). Used for polls where an error is an expected transient.
psql_super() {
    local db="$1" sql="$2" primary out rc errf
    primary="$(cnpg_primary)"
    if [ -z "$primary" ] || [ -z "$db" ]; then
        printf 'psql_super: no primary (got "%s") or no db (got "%s")\n' "$primary" "$db" >&2
        printf ''; return 0
    fi
    { set +x; } 2>/dev/null
    errf="${TMPDIR_WORK:-${TMPDIR:-/tmp}}/psql_super.$$"
    set +e
    out=$(kubectl -n cnpg-system exec "$primary" -c postgres -- \
        psql -U postgres -d "$db" -v ON_ERROR_STOP=1 -tAc "$sql" 2>"$errf")
    rc=$?
    set -e
    if [ "$rc" -ne 0 ]; then
        printf 'psql_super: rc=%s db=%s primary=%s :: %s\n' \
            "$rc" "$db" "$primary" "$(tr '\n' ' ' < "$errf" 2>/dev/null)" >&2
        rm -f "$errf"; printf ''; return 0
    fi
    rm -f "$errf"
    printf '%s' "$out"
}
# psql_super_script <db> <sql-file> — run a multi-statement script and FAIL
# LOUDLY (non-zero + the server error) if anything goes wrong. The digest must
# never degrade into an empty string that silently compares equal.
psql_super_script() {
    local db="$1" file="$2" primary rc errf
    primary="$(cnpg_primary)"
    if [ -z "$primary" ] || [ -z "$db" ]; then
        printf 'FAILED: psql_super_script: no CNPG primary (got %q) or no db (got %q)\n' "$primary" "$db" >&2
        return 1
    fi
    errf="${TMPDIR_WORK:-${TMPDIR:-/tmp}}/psql_script.$$"
    set +e
    kubectl -n cnpg-system exec -i "$primary" -c postgres -- \
        psql -U postgres -d "$db" -v ON_ERROR_STOP=1 -tA -f - <"$file" 2>"$errf"
    rc=$?
    set -e
    if [ "$rc" -ne 0 ]; then
        printf 'FAILED: psql script rc=%s db=%s primary=%s\n' "$rc" "$db" "$primary" >&2
        sed 's/^/    | /' "$errf" >&2 2>/dev/null || true
        rm -f "$errf"
        return 1
    fi
    rm -f "$errf"
    return 0
}
wait_for_pg_primary() {
    local timeout="${1:-420}" waited=0 name
    printf '  wait: CNPG primary pod Ready in cnpg-system (timeout %ss) ...\n' "$timeout"
    while [ "$waited" -lt "$timeout" ]; do
        name="$(cnpg_primary)"
        if [ -n "$name" ] && kubectl -n cnpg-system wait "$name" --for=condition=Ready --timeout=5s >/dev/null 2>&1; then
            printf '  ok: CNPG primary ready (%s)\n' "$name"; return 0
        fi
        sleep 5; waited=$((waited + 5))
    done
    printf 'FAILED: no CNPG primary became Ready in cnpg-system within %ss\n' "$timeout" >&2
    return 1
}
wait_for_appdb() {
    local db="$1" timeout="${2:-300}" waited=0 got
    printf '  wait: app database %s to exist (timeout %ss) ...\n' "$db" "$timeout"
    while [ "$waited" -lt "$timeout" ]; do
        got=$(psql_super postgres "SELECT 1 FROM pg_database WHERE datname = '${db}';")
        [ "$got" = "1" ] && { printf '  ok: app database %s exists\n' "$db"; return 0; }
        sleep 5; waited=$((waited + 5))
    done
    printf 'FAILED: app database %s never appeared in %ss. pg_database has: [%s]\n' \
        "$db" "$timeout" "$(psql_super postgres "SELECT string_agg(datname, ' ') FROM pg_database;")" >&2
    return 1
}

# ---------------------------------------------------------------
# THE DATA FINGERPRINT
#
# `pg_dump` output is NOT stable (unordered heap scans reorder rows freely), so
# hashing a dump produces false failures. Instead: for EVERY user table, emit
#
#     <schema>.<table>|<row count>|<md5 of the rows>
#
# where the row hash is md5 over the whole-row text renderings joined in an
# explicit `ORDER BY ... COLLATE "C"` — a byte-wise total order that does not
# depend on the database's collation, and a per-row rendering (`x::text`) that
# works for every column type without needing a btree operator class.
#
# The session pins every output-formatting GUC that could differ between two
# clusters (datestyle, timezone, float digits, bytea output), so the digest is a
# property of the DATA, not of the server it happens to be sitting on.
#
# plpgsql (always present) rather than `query_to_xml` (needs a libxml build) —
# the digest must not depend on an optional server feature.
# ---------------------------------------------------------------
write_digest_sql() {
    cat >"$1" <<'DIGEST_SQL'
SET client_encoding TO 'UTF8';
SET datestyle TO 'ISO, MDY';
SET intervalstyle TO 'postgres';
SET extra_float_digits TO 3;
SET timezone TO 'UTC';
SET bytea_output TO 'hex';
CREATE TEMP TABLE _ar_digest(line text);
DO $do$
DECLARE
    rec record;
    cnt bigint;
    hsh text;
BEGIN
    FOR rec IN
        SELECT ns.nspname AS sch, cl.relname AS tbl
        FROM pg_class cl
        JOIN pg_namespace ns ON ns.oid = cl.relnamespace
        WHERE cl.relkind = 'r'
          AND cl.relpersistence = 'p'
          AND ns.nspname NOT IN ('pg_catalog', 'information_schema')
          AND ns.nspname !~ '^pg_(toast|temp)'
        ORDER BY 1, 2
    LOOP
        EXECUTE format(
            'SELECT count(*), coalesce(md5(string_agg(v, chr(10) ORDER BY v COLLATE "C")), ''-'') '
            'FROM (SELECT x::text AS v FROM %I.%I x) z',
            rec.sch, rec.tbl)
        INTO cnt, hsh;
        INSERT INTO _ar_digest VALUES (format('%s.%s|%s|%s', rec.sch, rec.tbl, cnt, hsh));
    END LOOP;
END
$do$;
SELECT line FROM _ar_digest ORDER BY line COLLATE "C";
DIGEST_SQL
}

# compute_digest <db> <outfile> — writes the sorted digest lines. Fails when the
# script errors OR when it produced nothing (an empty digest would compare equal
# to another empty digest and silently pass).
compute_digest() {
    local db="$1" out="$2" raw="${2}.raw"
    psql_super_script "$db" "$DIGEST_SQL_FILE" >"$raw" || return 1
    grep -E '\|' "$raw" | LC_ALL=C sort >"$out" || true
    rm -f "$raw"
    if [ ! -s "$out" ]; then
        printf 'FAILED: digest of db %q is EMPTY — the fingerprint found no user tables\n' "$db" >&2
        return 1
    fi
    return 0
}
# wake_cms <db> [timeout] — Payload initialises LAZILY. The adapter runs
# `prodMigrations` and the onInit global seed on the first request that reaches
# a payload route, NOT at container start: the image is a Next standalone
# server, and the pod reaches Ready without anything ever touching payload.
# Until something asks, the application database holds NO CMS tables at all —
# measured on the first run of this walk, where the "CMS database" fingerprint
# turned out to be nothing but the walk's own seed data. So ask, from inside the
# pod, and wait for the real schema to materialise.
wake_cms() {
    local db="$1" timeout="${2:-480}" waited=0 pod have tables
    pod="$(cms_pod)"
    [ -n "$pod" ] || { printf 'FAILED: no CMS pod to wake\n' >&2; return 1; }
    printf '  waking Payload via an in-pod request to /admin (lazy init; timeout %ss) ...\n' "$timeout"
    while [ "$waited" -lt "$timeout" ]; do
        # wget ships in the image (its own HEALTHCHECK uses it). The exit code
        # is ignored on purpose — a redirect or a 401 still initialises payload;
        # the DATABASE is the thing being asserted, not the HTTP status.
        kubectl -n "$CMS_NS" exec "$pod" -- \
            wget -q -T 25 -O /dev/null 'http://127.0.0.1:3000/admin' >/dev/null 2>&1 || true
        have="$(psql_super "$db" "SELECT count(*) FROM pg_class cl JOIN pg_namespace ns ON ns.oid = cl.relnamespace WHERE cl.relkind = 'r' AND ns.nspname = 'public' AND cl.relname = 'payload_migrations';" | tr -d '[:space:]')"
        if [ "$have" = "1" ]; then
            tables="$(psql_super "$db" "SELECT count(*) FROM pg_class cl JOIN pg_namespace ns ON ns.oid = cl.relnamespace WHERE cl.relkind = 'r' AND ns.nspname = 'public';" | tr -d '[:space:]')"
            printf '  ok: Payload initialised — prodMigrations applied, %s public table(s) in %s\n' "$tables" "$db"
            return 0
        fi
        printf '    %s: payload_migrations absent — CMS schema not created yet ...\n' "$(date +%H:%M:%S)"
        sleep 10; waited=$((waited + 10))
    done
    printf 'FAILED: the CMS never created its schema in %s within %ss (payload_migrations absent)\n' "$db" "$timeout" >&2
    printf '  public tables present: [%s]\n' \
        "$(psql_super "$db" "SELECT coalesce(string_agg(cl.relname, ' ' ORDER BY cl.relname), '<none>') FROM pg_class cl JOIN pg_namespace ns ON ns.oid = cl.relnamespace WHERE cl.relkind = 'r' AND ns.nspname = 'public';")" >&2
    kubectl -n "$CMS_NS" logs "$pod" --all-containers --tail=150 >&2 2>&1 || true
    return 1
}

# wait_db_quiescent <db> [timeout] — a fingerprint is only a BASELINE if the
# application has stopped writing. Poll until two consecutive digests agree.
#
# This is a different question from the reproducibility assertion that follows
# it: "has the workload gone quiet" vs "is the digest FUNCTION deterministic".
# Both are real risks, so both get their own named failure — otherwise a
# background write shows up much later as a mystery diff in phase 7 and gets
# blamed on the restore.
wait_db_quiescent() {
    local db="$1" timeout="${2:-300}" waited=0
    local a="${TMPDIR_WORK}/quiesce.a" b="${TMPDIR_WORK}/quiesce.b"
    printf '  wait: %s to go quiescent (two consecutive identical digests, timeout %ss) ...\n' "$db" "$timeout"
    compute_digest "$db" "$a" || return 1
    while [ "$waited" -lt "$timeout" ]; do
        sleep 15; waited=$((waited + 15))
        compute_digest "$db" "$b" || return 1
        if cmp -s "$a" "$b"; then
            printf '  ok: %s is quiescent after %ss (digest md5 %s)\n' "$db" "$waited" "$(digest_md5 "$b")"
            return 0
        fi
        printf '    %s: still moving (%s -> %s) ...\n' \
            "$(date +%H:%M:%S)" "$(digest_md5 "$a")" "$(digest_md5 "$b")"
        mv "$b" "$a"
    done
    printf 'FAILED: %s never went quiescent within %ss — the workload keeps writing, so there is no stable fingerprint to compare against\n' "$db" "$timeout" >&2
    diff -u "$a" "$b" >&2 || true
    return 1
}

digest_md5()    { md5sum <"$1" | awk '{print $1}'; }
digest_tables() { wc -l <"$1" | tr -d ' '; }
digest_rows()   { awk -F'|' '{s+=$2} END {print s+0}' "$1"; }
digest_counts() { cut -d'|' -f1,2 "$1"; }
# assert_same_digest <desc> <fileA> <fileB> — byte-identity, with the unified
# diff on failure so the offending TABLE is named, not just "they differ".
assert_same_digest() {
    local desc="$1" a="$2" b="$3"
    if diff -u "$a" "$b" >"${TMPDIR_WORK}/digest.diff" 2>&1; then
        printf '  ok: %s (md5 %s, %s table[s], %s row[s])\n' \
            "$desc" "$(digest_md5 "$a")" "$(digest_tables "$a")" "$(digest_rows "$a")"
        return 0
    fi
    printf 'FAILED: %s — digests differ (%s vs %s):\n' "$desc" "$(digest_md5 "$a")" "$(digest_md5 "$b")" >&2
    sed 's/^/    | /' "${TMPDIR_WORK}/digest.diff" >&2
    return 1
}

# ---------------------------------------------------------------
# Cluster bring-up + CMS deploy
# ---------------------------------------------------------------
provision_and_bootstrap() {
    local target="$1" sku="$2"
    apprafter target use "$target"
    apprafter up --target "$target" --server-type "$sku"
    apprafter kubeconfig --target "$target" >"$KC_FILE"
    export KUBECONFIG="$KC_FILE"
}
wait_platform_ready() {
    printf '  waiting for Argo CD platform Applications to Sync ...\n'
    local apps=(cilium argocd cert-manager apprafter-operator admission-webhook)
    local app deadline sync
    for app in "${apps[@]}"; do
        sync=""
        deadline=$(( $(date +%s) + 900 ))
        while [ "$(date +%s)" -lt "$deadline" ]; do
            sync=$(kubectl -n argocd get applications.argoproj.io "$app" \
                -o jsonpath='{.status.sync.status}' 2>/dev/null || true)
            [ "$sync" = "Synced" ] && break
            sleep 10
        done
        [ "$sync" = "Synced" ] || {
            printf 'FAILED: Argo CD Application %s not Synced within 15 min (got %q)\n' "$app" "$sync" >&2
            return 1
        }
        printf '  ok: Argo CD Application %s -> Synced\n' "$app"
    done
    printf '  ok: platform ready (Argo apps Synced)\n'
}
wait_cms_ready() {
    local claim
    claim="$(wait_pg_claim_ready 720)"
    printf '  ok: pg claim resolved to %s\n' "$claim"
    wait_jsonpath "$APP_RES" "$CMS_NS" "$CMS_APP" '{.status.phase}' Ready 600
    kubectl -n "$CMS_NS" wait --for=condition=Available "deployment/${CMS_APP}" --timeout=600s
    retry 40 8 -- kubectl -n "$CMS_NS" wait --for=condition=Ready \
        pod -l "app.kubernetes.io/name=${CMS_APP}" --timeout=20s
}
# Deploy the CMS from its PUBLIC repo path via --env dev (cwd = the manifest dir
# so `app add` finds the local Application.cue; --branch master). Seals the FAKE
# PAYLOAD_SECRET into the APP namespace first — sealed secrets are
# namespace-bound, and a platform-namespace seal would surface as
# EnvSecretMissing.
deploy_cms() {
    kubectl create namespace "$CMS_NS" 2>/dev/null || true
    { set +x; } 2>/dev/null
    apprafter secret seal "$CMS_SECRET_NAME" \
        --from-literal "${CMS_SECRET_KEY}=${CMS_SECRET_VALUE}" --namespace "$CMS_NS"
    wait_jsonpath secret "$CMS_NS" "$CMS_SECRET_NAME" '{.metadata.name}' "$CMS_SECRET_NAME" 180
    printf '  ok: FAKE PAYLOAD_SECRET sealed into ns %s\n' "$CMS_NS"
    local bin="${REPO_ROOT}/cli/target/debug/apprafter"
    [ -x "$bin" ] || ( cd "${REPO_ROOT}/cli" && cargo build --quiet --bin apprafter )
    ( cd "${REPO_ROOT}/landing/cms" && "$bin" app add "$CMS_REPO" \
        --name "$CMS_APP" --branch master --path "$CMS_PATH" \
        --namespace "$CMS_NS" --env "$CMS_ENV" --no-ping --no-interactive )
    printf '  ok: CMS registered (public repo, --env %s)\n' "$CMS_ENV"
    wait_cms_ready
}

# ---------------------------------------------------------------
# S3 helpers (BACKEND=s3 only)
# ---------------------------------------------------------------
restic_s3() {
    { set +x; } 2>/dev/null
    local -a envs=(
        "AWS_ACCESS_KEY_ID=${S3_KEY}"
        "AWS_SECRET_ACCESS_KEY=${S3_SECRET}"
        "RESTIC_PASSWORD=${RESTIC_PASS}"
    )
    [ -n "$S3_REGION_V" ] && envs+=( "AWS_DEFAULT_REGION=${S3_REGION_V}" )
    env "${envs[@]}" restic "$@"
}
s3_snapshot_count() {
    local out
    out="$(restic_s3 -r "$S3_REPO" snapshots --json 2>/dev/null || true)"
    printf '%s' "$out" | python3 -c 'import json,sys
d=sys.stdin.read().strip()
try: print(len(json.loads(d)) if d else 0)
except Exception: print(0)'
}
build_cred_file() {
    { set +x; } 2>/dev/null
    ( umask 077
      printf 'S3_ACCESS_KEY_ID=%s\n'     "$S3_KEY"
      printf 'S3_SECRET_ACCESS_KEY=%s\n' "$S3_SECRET"
      printf 'RESTIC_PASSWORD=%s\n'      "$RESTIC_PASS"
      [ -n "$S3_REGION_V" ] && printf 'S3_REGION=%s\n' "$S3_REGION_V"
      true
    ) >"$CRED_FILE"
    chmod 0600 "$CRED_FILE"
}
wait_job_complete() {
    local job="$1" ns="$2" timeout="${3:-900}" deadline done_c failed_c
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait job/%s -n %s to Complete (timeout %ss) ...\n' "$job" "$ns" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        done_c=$(kubectl -n "$ns" get job "$job" \
            -o jsonpath='{.status.conditions[?(@.type=="Complete")].status}' 2>/dev/null || true)
        failed_c=$(kubectl -n "$ns" get job "$job" \
            -o jsonpath='{.status.conditions[?(@.type=="Failed")].status}' 2>/dev/null || true)
        if [ "$done_c" = "True" ]; then printf '  ok: job/%s Completed\n' "$job"; return 0; fi
        if [ "$failed_c" = "True" ]; then
            printf 'FAILED: job/%s reported Failed\n' "$job" >&2
            kubectl -n "$ns" logs "job/$job" --all-containers --tail=150 >&2 2>&1 || true
            return 1
        fi
        sleep 8
    done
    printf 'FAILED: job/%s did not Complete within %ss\n' "$job" "$timeout" >&2
    kubectl -n "$ns" describe job "$job" >&2 2>&1 || true
    kubectl -n "$ns" logs "job/$job" --all-containers --tail=150 >&2 2>&1 || true
    return 1
}

# ---------------------------------------------------------------
# Workspace (isolated CLI state — never touches the operator's real targets)
# ---------------------------------------------------------------
TMPDIR_WORK="$(mktemp -d -t subup-e2e.XXXXXX)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"; mkdir -p "$APPRAFTER_CONFIG_DIR"
export APPRAFTER_CONFIG_DIR
export APPRAFTER_AGE_KEY="${TMPDIR_WORK}/age.key"
KC_FILE="${TMPDIR_WORK}/kubeconfig"
CRED_FILE="${TMPDIR_WORK}/backup-creds.env"
DIGEST_SQL_FILE="${TMPDIR_WORK}/digest.sql"
LOCAL_REPO="${TMPDIR_WORK}/restic-repo"   # ON THE HOST — survives the destroy
CLUSTER_CREATED=0

# ---------------------------------------------------------------
# Teardown trap — armed BEFORE the first provision. Destroys whatever is up,
# sweeps the project, and API-verifies ZERO. A leak is a FAILURE, not a note.
# ---------------------------------------------------------------
cleanup() {
    local exit_code=$?
    { set +x; } 2>/dev/null
    if [ "$exit_code" -ne 0 ]; then
        printf '\nFAILED: substrate-upgrade-hetzner FAILED at %s (exit %d)\n' "$(elapsed)" "$exit_code" >&2
        if [ "$CLUSTER_CREATED" -eq 1 ] && apprafter kubeconfig --target "$TARGET" >"$KC_FILE" 2>/dev/null; then
            KUBECONFIG="$KC_FILE" dump_diagnostics || true
        fi
    fi

    if [ -n "${APPRAFTER_HETZNER_SKIP_DESTROY:-}" ]; then
        printf '\nAPPRAFTER_HETZNER_SKIP_DESTROY set — leaving the cluster UP (it is COSTING MONEY).\n' >&2
        [ "$CLUSTER_CREATED" -eq 1 ] && printf 'Destroy by hand: APPRAFTER_CONFIG_DIR=%s apprafter destroy --yes --target %s\n' \
            "$APPRAFTER_CONFIG_DIR" "$TARGET" >&2
        exit "$exit_code"
    fi

    phase "Phase 8: teardown — destroy, sweep, API-verify ZERO servers"
    if [ "$CLUSTER_CREATED" -eq 1 ]; then
        apprafter destroy --yes --target "$TARGET" || \
            printf 'WARN: destroy --target %s returned non-zero; the sweep below is the backstop\n' "$TARGET" >&2
    fi
    # Backstop: the walk's baseline is an EMPTY project (verified BEFORE this
    # trap was armed), so anything left is ours.
    # shellcheck disable=SC2086 # $HZ is "python3 <path>" — word-splitting is intended
    $HZ sweep "$HZ_TOKEN" >&2 || true
    local n; n="$(hetzner_server_count)"
    if [ "$n" = "0" ]; then
        printf '  ok: project has ZERO servers after teardown (Hetzner API-verified)\n' >&2
    else
        printf 'FAILED: LEAK WARNING — project reports %s server(s) after teardown. INSPECT https://console.hetzner.cloud AND DELETE STRAGGLERS BY HAND\n' "$n" >&2
        exit_code=1
    fi
    # shellcheck disable=SC2086 # $HZ is "python3 <path>" — word-splitting is intended
    if $HZ verify "$HZ_TOKEN" >&2; then
        printf '  ok: project swept back to zero resources\n' >&2
    else
        printf 'FAILED: project NOT empty after sweep — inspect it by hand\n' >&2
        exit_code=1
    fi

    # BACKEND=s3: best-effort prune of OUR prefix. The BUCKET is never touched.
    if [ "$BACKEND" = "s3" ]; then
        restic_s3 -r "$S3_REPO" forget --keep-last 0 --prune >/dev/null 2>&1 || \
            printf '  note: restic prefix %s left as-is (prune by hand if desired)\n' "$S3_REPO" >&2
    fi

    rm -rf "$TMPDIR_WORK"
    [ -n "${RESTIC_WRAPPER_BIN_DIR:-}" ] && rm -rf "$RESTIC_WRAPPER_BIN_DIR" || true

    if [ "$exit_code" -eq 0 ]; then
        printf '\n=== GREEN: substrate upgrade %s -> %s validated end-to-end on real Hetzner (backend=%s, %s) ===\n' \
            "$SKU_SMALL" "$SKU_BIG" "$BACKEND" "$(elapsed)"
    fi
    exit "$exit_code"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# ===============================================================
# Phase 0: setup
# ===============================================================
phase "Phase 0: setup — fresh config dir, one token, teardown armed BEFORE any spend"
cat <<COST_NOTE
  COST AWARENESS: this walk provisions ONE ${SKU_SMALL}, destroys it, then
  re-provisions ONE ${SKU_BIG} (the --reprovision path) in ${REGION} — two
  short-lived boxes, around EUR 0.02. The teardown trap ALWAYS destroys,
  sweeps and API-verifies zero.
  backend: ${BACKEND}   target: ${TARGET}   ${SKU_SMALL} -> ${SKU_BIG}
COST_NOTE
printf '  ok: Hetzner project baseline verified empty above (pre-trap)\n'

if [ "$BACKEND" = "s3" ]; then
    build_cred_file
    printf '  ok: operator S3 credential dotenv written 0600 (%s)\n' "$CRED_FILE"
    printf '  ok: off-site repo target: %s\n' "$S3_REPO"
fi

{ set +x; } 2>/dev/null
apprafter target add "$TARGET" \
    --provider hetzner-cloud --tier solo --region "$REGION" \
    --server-type "$SKU_SMALL" \
    --token "$HZ_TOKEN" --ssh-key "$SSH_PUB" \
    --no-interactive --force
printf '  ok: target %s registered (region %s, server type %s — SKU validated against the live Hetzner API)\n' \
    "$TARGET" "$REGION" "$SKU_SMALL"

# ===============================================================
# Phase 1: provision + bootstrap on the SMALL SKU
# ===============================================================
phase "Phase 1: provision + bootstrap the cluster on the SMALL SKU ($SKU_SMALL)"
provision_and_bootstrap "$TARGET" "$SKU_SMALL"
CLUSTER_CREATED=1
wait_platform_ready

# The substrate facts come from the PROVIDER, not from local state — local
# state is exactly what an upgrade could be lying about.
OLD_SERVER_ID="$(hetzner_first_server_id)"
OLD_SERVER_TYPE="$(hetzner_first_server_type)"
[ -n "$OLD_SERVER_ID" ] || { printf 'FAILED: no server in the project after provisioning\n' >&2; exit 1; }
assert_eq "landed SKU (read from the Hetzner API, not local state)" "$OLD_SERVER_TYPE" "$SKU_SMALL"
printf '  ok: source cluster up — hetzner server id %s, type %s\n' "$OLD_SERVER_ID" "$OLD_SERVER_TYPE"

OLD_ALLOC_RAW="$(node_allocatable_memory)"
OLD_ALLOC="$(mem_to_bytes "$OLD_ALLOC_RAW")"
[ "${OLD_ALLOC:-0}" -gt 0 ] || { printf 'FAILED: could not read node allocatable memory (got %q)\n' "$OLD_ALLOC_RAW" >&2; exit 1; }
printf '  ok: node allocatable memory BEFORE = %s (%s MiB)\n' "$OLD_ALLOC_RAW" "$(mib "$OLD_ALLOC")"

# ===============================================================
# Phase 2: deploy the REAL CMS
# ===============================================================
phase "Phase 2: deploy the REAL landing CMS (needs.pg + a secret: ref, --env $CMS_ENV)"
deploy_cms
CMS_POD_SRC="$(cms_pod)"
[ -n "$CMS_POD_SRC" ] || { printf 'FAILED: no CMS pod on the source cluster\n' >&2; exit 1; }
printf '  ok: CMS pod on source: %s\n' "$CMS_POD_SRC"
src_ready=$(jp "$APP_RES" "$CMS_NS" "$CMS_APP" '{.status.conditions[?(@.type=="Ready")].status}')
assert_eq "CMS Ready=True on source (the secret: ref resolved, not EnvSecretMissing)" "$src_ready" "True"

# ===============================================================
# Phase 3: THE DATA FINGERPRINT
# ===============================================================
phase "Phase 3: the data fingerprint — wake the CMS, seed known data, build a REPRODUCIBLE digest"
write_digest_sql "$DIGEST_SQL_FILE"
wait_for_pg_primary
SRC_CLAIM="$(wait_pg_claim_ready 600)"
SRC_CONN="$(pg_conn_secret "$SRC_CLAIM")"
[ -n "$SRC_CONN" ] || { printf 'FAILED: pg claim %s has no status.connectionSecretRef\n' "$SRC_CLAIM" >&2; exit 1; }
SRC_APPDB="$(conn_field "$SRC_CONN" db)"
SRC_APPROLE="$(conn_field "$SRC_CONN" user)"
[ -n "$SRC_APPDB" ] || { printf 'FAILED: could not resolve the CMS app database from secret %s/%s\n' "$CMS_NS" "$SRC_CONN" >&2; exit 1; }
printf '  ok: CMS pg claim=%s conn-secret=%s db=%s role=%s\n' "$SRC_CLAIM" "$SRC_CONN" "$SRC_APPDB" "${SRC_APPROLE:-<none>}"
wait_for_appdb "$SRC_APPDB"

# Make the CMS create its REAL schema + seed its globals before fingerprinting,
# so the digest covers the actual workload's data and not just the walk's props.
wake_cms "$SRC_APPDB"

# Seed 1: a human-readable marker so a data failure is legible at a glance.
# Seed 2: DATA_ROWS rows of varied types (text / numeric / float / bool /
# timestamptz / jsonb / bytea / NULL) so the digest is a real fingerprint and
# not "three empty tables hashed the same". Both tables are chowned to the APP
# ROLE: the backup runs `pg_dump -U <approle>`, so superuser-owned tables would
# not survive the round trip and the walk would blame the wrong thing.
printf '  seeding %s + %s (%s rows) ...\n' "$MARKER_TABLE" "$DATA_TABLE" "$DATA_ROWS"
seed_sql="CREATE TABLE IF NOT EXISTS ${MARKER_TABLE} (id INT PRIMARY KEY, note TEXT NOT NULL);"
seed_sql="${seed_sql} INSERT INTO ${MARKER_TABLE} (id, note) VALUES (1, '${MARKER_VALUE}') ON CONFLICT (id) DO UPDATE SET note = EXCLUDED.note;"
seed_sql="${seed_sql} DROP TABLE IF EXISTS ${DATA_TABLE};"
seed_sql="${seed_sql} CREATE TABLE ${DATA_TABLE} (id BIGINT PRIMARY KEY, label TEXT NOT NULL, amount NUMERIC(18,6) NOT NULL, ratio DOUBLE PRECISION NOT NULL, flag BOOLEAN NOT NULL, created TIMESTAMPTZ NOT NULL, payload JSONB NOT NULL, blob BYTEA NOT NULL, maybe_null TEXT);"
seed_sql="${seed_sql} INSERT INTO ${DATA_TABLE} SELECT g, 'row-' || g, (g * 1234.567891)::numeric(18,6), sqrt(g::double precision), (g % 3 = 0), timestamptz '2026-01-01 00:00:00+00' + (g || ' minutes')::interval, jsonb_build_object('i', g, 'sq', g*g, 's', md5(g::text)), decode(md5(g::text), 'hex'), CASE WHEN g % 7 = 0 THEN NULL ELSE 'v' || g END FROM generate_series(1, ${DATA_ROWS}) g;"
if [ -n "$SRC_APPROLE" ]; then
    seed_sql="${seed_sql} ALTER TABLE ${MARKER_TABLE} OWNER TO \"${SRC_APPROLE}\";"
    seed_sql="${seed_sql} ALTER TABLE ${DATA_TABLE} OWNER TO \"${SRC_APPROLE}\";"
fi
psql_super "$SRC_APPDB" "$seed_sql" >/dev/null
old_marker=$(psql_super "$SRC_APPDB" "SELECT note FROM ${MARKER_TABLE} WHERE id=1;" | tr -d '[:space:]')
assert_eq "known marker row present on the source pg" "$old_marker" "$MARKER_VALUE"
seeded_rows=$(psql_super "$SRC_APPDB" "SELECT count(*) FROM ${DATA_TABLE};" | tr -d '[:space:]')
assert_eq "seeded payload table row count" "$seeded_rows" "$DATA_ROWS"

# Let the CMS finish whatever the wake-up started before taking the baseline.
wait_db_quiescent "$SRC_APPDB"

# The digest, computed TWICE on the (now unchanging) source DB. A digest that is
# not reproducible turns this walk into either a false alarm or a false pass,
# and you cannot tell which — so prove reproducibility BEFORE relying on it.
compute_digest "$SRC_APPDB" "${TMPDIR_WORK}/digest.src.1"
compute_digest "$SRC_APPDB" "${TMPDIR_WORK}/digest.src.2"
assert_same_digest "digest is REPRODUCIBLE on the unchanged source DB (double compute)" \
    "${TMPDIR_WORK}/digest.src.1" "${TMPDIR_WORK}/digest.src.2"

SRC_DIGEST_MD5="$(digest_md5 "${TMPDIR_WORK}/digest.src.1")"
SRC_TABLES="$(digest_tables "${TMPDIR_WORK}/digest.src.1")"
SRC_ROWS="$(digest_rows "${TMPDIR_WORK}/digest.src.1")"
printf '\n  --- source data fingerprint (db %s) ---\n' "$SRC_APPDB"
sed 's/^/    | /' "${TMPDIR_WORK}/digest.src.1"
printf '    | TOTAL %s table(s), %s row(s), digest md5 %s\n\n' "$SRC_TABLES" "$SRC_ROWS" "$SRC_DIGEST_MD5"
# Sanity: the fingerprint must cover the REAL CMS schema, not just the walk's
# two seeded tables. Asserted by NAME (payload_migrations is created by the
# CMS's own prodMigrations) rather than by a row count, so it cannot be
# satisfied by accident.
grep -q '^public\.payload_migrations|' "${TMPDIR_WORK}/digest.src.1" || {
    printf 'FAILED: the fingerprint does not cover the CMS schema — public.payload_migrations is absent, so this is a digest of the walk props only\n' >&2
    exit 1
}
printf '  ok: fingerprint covers the CMS schema itself (public.payload_migrations present)\n'
[ "$SRC_TABLES" -ge 10 ] || { printf 'FAILED: digest covers only %s table(s) — expected the CMS schema (30+ tables) plus the 2 seeded tables\n' "$SRC_TABLES" >&2; exit 1; }
[ "$SRC_ROWS" -ge "$((DATA_ROWS + 1))" ] || { printf 'FAILED: digest covers only %s row(s) — expected at least %s\n' "$SRC_ROWS" "$((DATA_ROWS + 1))" >&2; exit 1; }
printf '  ok: fingerprint recorded — %s table(s), %s row(s), md5 %s\n' "$SRC_TABLES" "$SRC_ROWS" "$SRC_DIGEST_MD5"
printf '  ok: substrate facts recorded — server id %s, type %s, node allocatable %s\n' \
    "$OLD_SERVER_ID" "$OLD_SERVER_TYPE" "$OLD_ALLOC_RAW"

# ===============================================================
# Phase 4: back up (the one switch)
# ===============================================================
if [ "$BACKEND" = "local" ]; then
    phase "Phase 4 (Leg A, backend=local): apprafter backup create -> encrypted restic repo on the HOST"
    { set +x; } 2>/dev/null
    apprafter backup create --repo "$LOCAL_REPO" --passphrase "$RESTIC_PASS"
    [ -f "${LOCAL_REPO}/config" ] || { printf 'FAILED: restic repo config missing at %s\n' "$LOCAL_REPO" >&2; exit 1; }
    if RESTIC_PASSWORD=subup-deliberately-wrong restic -r "$LOCAL_REPO" snapshots >/dev/null 2>&1; then
        printf 'FAILED: restic accepted a WRONG passphrase — the repo is not encrypted as claimed\n' >&2; exit 1
    fi
    printf '  ok: encrypted restic repo created on the host (%s); a wrong passphrase is rejected\n' "$LOCAL_REPO"
    RESTORE_REPO="$LOCAL_REPO"
else
    phase "Phase 4 (Leg B, backend=s3): apprafter backup enable -> trigger the runner -> assert a NEW off-site snapshot"
    { set +x; } 2>/dev/null
    SNAPS_BEFORE="$(s3_snapshot_count)"
    printf '  note: repo %s currently holds %s snapshot(s) — the assertion is that this GROWS\n' "$S3_REPO" "$SNAPS_BEFORE"
    # ONE-INPUT enable: --endpoint + bare --bucket build the s3: URL, and
    # --credential-file auto-seals the neutral S3_* creds into the cluster.
    # The enable PROBE hits the real bucket, so a green enable already proves
    # creds + URL + translation.
    apprafter backup enable \
        --endpoint "$S3_HOST" --bucket "$S3_BUCKET_NAME_R" --prefix "$S3_PREFIX" \
        --credential-file "$CRED_FILE" \
        --cron '*/5 * * * *' \
        --i-have-saved-credentials
    status_out="$(apprafter backup status)"
    printf '%s\n' "$status_out"
    assert_contains "backup status reports ENABLED" "$status_out" "Backup: ENABLED"
    assert_contains "backup status reports the constructed repo URL" "$status_out" "$S3_REPO"
    enabled_cr=$(jp platformstack "$BACKUP_NS" default '{.spec.backup.enabled}')
    assert_eq "PlatformStack spec.backup.enabled" "$enabled_cr" "true"
    wait_jsonpath cronjob "$BACKUP_NS" "$BACKUP_CRONJOB" '{.metadata.name}' "$BACKUP_CRONJOB" 420
    printf '  ok: the in-cluster backup runner (CronJob %s) is wired\n' "$BACKUP_CRONJOB"

    # --- tier 1 of the credential model: what the CLUSTER gets ---------------
    # `enable --credential-file` auto-seals the operator's dotenv into a
    # SealedSecret in apprafter-system; the runner mounts it via envFrom. Read
    # the name off the CR rather than assuming the default, then assert the
    # sealed material uses the NEUTRAL S3_* keys (the CLI translates to restic's
    # AWS_* only at the point of use — AWS_* landing in the cluster would mean
    # the translation leaked into stored state).
    CRED_SECRET="$(jp platformstack "$BACKUP_NS" default '{.spec.backup.credentialRef.name}')"
    [ -n "$CRED_SECRET" ] || { printf 'FAILED: PlatformStack has no spec.backup.credentialRef.name after enable\n' >&2; exit 1; }
    printf '  ok: cluster credential is a SealedSecret named %s in %s\n' "$CRED_SECRET" "$BACKUP_NS"
    wait_jsonpath secret "$BACKUP_NS" "$CRED_SECRET" '{.metadata.name}' "$CRED_SECRET" 240
    cred_keys="$(kubectl -n "$BACKUP_NS" get secret "$CRED_SECRET" -o jsonpath='{.data}' 2>/dev/null || true)"
    printf '%s' "$cred_keys" | grep -q 'S3_ACCESS_KEY_ID' || {
        printf 'FAILED: the cluster credential Secret %s does not carry the neutral S3_ACCESS_KEY_ID key\n' "$CRED_SECRET" >&2; exit 1; }
    if printf '%s' "$cred_keys" | grep -q 'AWS_'; then
        printf 'FAILED: the cluster credential Secret %s carries AWS_* keys — the restic translation leaked into stored cluster state\n' "$CRED_SECRET" >&2; exit 1
    fi
    printf '  ok: cluster credential holds NEUTRAL S3_* keys and no AWS_* keys\n'

    # Trigger NOW rather than waiting for the cron — the assertion is about the
    # runner's OUTCOME, not about cron firing on time. The runner has NO
    # credential source other than the sealed Secret above, so a Completed Job
    # plus a new off-site snapshot is the end-to-end proof that tier 1 works.
    MANUAL_JOB="apprafter-backup-manual-$(date +%s)"
    kubectl -n "$BACKUP_NS" create job --from="cronjob/${BACKUP_CRONJOB}" "$MANUAL_JOB"
    wait_job_complete "$MANUAL_JOB" "$BACKUP_NS" 1200
    status_out="$(apprafter backup status)"
    printf '%s\n' "$status_out"
    last_success=$(printf '%s' "$status_out" | awk -F': *' '/lastSuccess:/ {print $2; exit}')
    if [ -z "$last_success" ] || [ "$last_success" = "never" ]; then
        printf 'FAILED: backup status lastSuccess is empty/never after a Completed run (got %q)\n' "${last_success:-}" >&2
        exit 1
    fi
    printf '  ok: backup status reports lastSuccess=%s\n' "$last_success"
    SNAPS_AFTER="$(s3_snapshot_count)"
    if [ "${SNAPS_AFTER:-0}" -le "${SNAPS_BEFORE:-0}" ]; then
        printf 'FAILED: no NEW snapshot landed off-site (%s before, %s after) in %s\n' \
            "$SNAPS_BEFORE" "$SNAPS_AFTER" "$S3_REPO" >&2
        exit 1
    fi
    printf '  ok: a NEW snapshot landed OFF-SITE (%s -> %s snapshot[s] in %s), written by the in-cluster runner using ONLY the sealed credential\n' \
        "$SNAPS_BEFORE" "$SNAPS_AFTER" "$S3_REPO"
    apprafter backup check --credential-file "$CRED_FILE"
    printf '  ok: apprafter backup check passed (off-site repo is structurally sound)\n'
    RESTORE_REPO="$S3_REPO"
    # The artifact must live somewhere the cluster does NOT control. Leg B never
    # creates a host-local repo, so prove there is no local fallback that a
    # later restore could silently read instead of the bucket.
    [ ! -e "$LOCAL_REPO" ] || {
        printf 'FAILED: a host-local restic repo exists at %s in the s3 leg — a restore could read it instead of the bucket\n' "$LOCAL_REPO" >&2; exit 1; }
    printf '  ok: no host-local restic repo exists — the only artifact is the off-site one\n'
fi
case "$RESTORE_REPO" in
    s3:*) [ "$BACKEND" = "s3" ] || { printf 'FAILED: backend=%s resolved an s3 repo\n' "$BACKEND" >&2; exit 1; } ;;
    *)    [ "$BACKEND" = "local" ] || { printf 'FAILED: backend=s3 did not resolve an s3: repo (got %q)\n' "$RESTORE_REPO" >&2; exit 1; } ;;
esac
printf '  ok: restore source resolved for backend=%s: %s\n' "$BACKEND" "$RESTORE_REPO"

# The comparison in phase 7 is only meaningful if the source DB did not move
# underneath the backup. Re-fingerprint the SOURCE now: if this trips, the
# mismatch is the workload writing during the backup window (a walk-harness
# problem), NOT the restore losing data (a product problem) — and the two must
# never be confused for one another.
compute_digest "$SRC_APPDB" "${TMPDIR_WORK}/digest.src.post"
assert_same_digest "source digest UNCHANGED across the backup window (so a phase-7 mismatch can only be the restore)" \
    "${TMPDIR_WORK}/digest.src.1" "${TMPDIR_WORK}/digest.src.post"

# ===============================================================
# Phase 5: destroy
# ===============================================================
phase "Phase 5: apprafter destroy — the small box goes away, the backup does not"
{ set +x; } 2>/dev/null
apprafter destroy --yes --target "$TARGET"
dead_n="$(hetzner_server_count)"
assert_eq "project has ZERO servers after destroy (Hetzner API-verified)" "$dead_n" "0"
if [ "$BACKEND" = "local" ]; then
    [ -f "${LOCAL_REPO}/config" ] || { printf 'FAILED: the host restic repo vanished with the cluster\n' >&2; exit 1; }
    printf '  ok: backup artifact survived the destroy (host restic repo %s)\n' "$LOCAL_REPO"
else
    surviving="$(s3_snapshot_count)"
    [ "${surviving:-0}" -ge 1 ] || { printf 'FAILED: the off-site snapshots vanished with the cluster (got %s)\n' "${surviving:-0}" >&2; exit 1; }
    printf '  ok: backup artifact survived the destroy (%s off-site snapshot[s] in %s)\n' "$surviving" "$S3_REPO"
fi
printf '  ok: target %s is still registered — this is an UPGRADE of the same target, not a new one\n' "$TARGET"

if [ "$BACKEND" = "s3" ]; then
    # --- tier 2 of the credential model: what the OPERATOR keeps -------------
    # The cluster is GONE. Anything that still works against the bucket from
    # here can only be using the operator's local dotenv — there is no cluster
    # left to read a credential from. That is the sharpest available proof that
    # the two tiers are genuinely separate, so make it explicit before the
    # restore relies on it.
    postmortem_snaps="$(s3_snapshot_count)"
    [ "${postmortem_snaps:-0}" -ge 1 ] || {
        printf 'FAILED: cannot list the off-site repo with the operator dotenv after the cluster died (got %s snapshot[s])\n' "${postmortem_snaps:-0}" >&2; exit 1; }
    printf '  ok: the off-site repo is still readable with the OPERATOR credentials alone (%s snapshot[s]) — the cluster is gone, so these creds came from the local dotenv, never from the cluster\n' \
        "$postmortem_snaps"

    # OBSERVATION, not a gate. `backup check` is documented as an operator-side
    # verb to "Run OUTSIDE the cluster with the operator's full S3 credentials",
    # and with an explicit --repo + --credential-file it needs neither the
    # cluster nor the CR. Record what it ACTUALLY does with the cluster gone —
    # this is the moment an operator most wants to verify a backup. Printed as a
    # finding either way; it never passes or fails the walk.
    probe_log="${TMPDIR_WORK}/check-no-cluster.log"
    if apprafter backup check --repo "$S3_REPO" --credential-file "$CRED_FILE" >"$probe_log" 2>&1; then
        printf '  observation: "backup check --repo --credential-file" WORKS with the cluster destroyed (cluster-independent, as documented)\n'
    else
        printf '  observation/FINDING: "backup check --repo --credential-file" FAILS with the cluster destroyed, though it needs neither the cluster nor the CR:\n'
        sed 's/^/      | /' "$probe_log"
        printf '      (reported as a finding — NOT treated as a walk failure; the restore path below is unaffected)\n'
    fi
fi

# ===============================================================
# Phase 6: restore --reprovision onto the BIG SKU
# ===============================================================
phase "Phase 6: apprafter restore <repo> --reprovision --server-type $SKU_BIG --target $TARGET"
{ set +x; } 2>/dev/null
restore_log="${TMPDIR_WORK}/restore.log"
restore_args=( "$RESTORE_REPO" --reprovision --server-type "$SKU_BIG" --target "$TARGET" )
if [ "$BACKEND" = "local" ]; then
    restore_args+=( --passphrase "$RESTIC_PASS" )
else
    restore_args+=( --credential-file "$CRED_FILE" )
fi
apprafter restore "${restore_args[@]}" >"$restore_log" 2>&1 || {
    printf 'FAILED: restore --reprovision --server-type %s returned non-zero:\n' "$SKU_BIG" >&2
    sed 's/^/    | /' "$restore_log" >&2
    exit 1
}
# CLUSTER_CREATED stays 1 — a FRESH cluster now exists in the target.
grep -qE 'provisioning a fresh cluster' "$restore_log" || {
    printf 'FAILED: restore did not run the reprovision step\n' >&2; sed 's/^/    | /' "$restore_log" >&2; exit 1; }
grep -qE 'Restored backup' "$restore_log" || {
    printf 'FAILED: restore did not report completion\n' >&2; sed 's/^/    | /' "$restore_log" >&2; exit 1; }
printf '  ok: restore --reprovision provisioned a fresh cluster and replayed the backup\n'
if [ "$BACKEND" = "s3" ]; then
    printf '  ok: the replay source was the OFF-SITE repo %s, read with the operator dotenv while no cluster existed\n' "$RESTORE_REPO"
fi
sed 's/^/    | /' "$restore_log"

# ===============================================================
# Phase 7: verify the UPGRADE, not just the restore
# ===============================================================
phase "Phase 7: verify the UPGRADE — bigger box, same workload, byte-identical data"
apprafter kubeconfig --target "$TARGET" >"$KC_FILE"
export KUBECONFIG="$KC_FILE"

# --- the substrate actually got bigger (all three facts from the API) ---
NEW_SERVER_ID="$(hetzner_first_server_id)"
NEW_SERVER_TYPE="$(hetzner_first_server_type)"
[ -n "$NEW_SERVER_ID" ] || { printf 'FAILED: no server in the project after reprovision\n' >&2; exit 1; }
assert_eq "server type is now the BIG SKU (read from the Hetzner API)" "$NEW_SERVER_TYPE" "$SKU_BIG"
assert_ne "server id changed — genuinely a NEW box, not a resized ghost" "$NEW_SERVER_ID" "$OLD_SERVER_ID"

NEW_ALLOC_RAW="$(node_allocatable_memory)"
NEW_ALLOC="$(mem_to_bytes "$NEW_ALLOC_RAW")"
[ "${NEW_ALLOC:-0}" -gt 0 ] || { printf 'FAILED: could not read node allocatable memory after the upgrade (got %q)\n' "$NEW_ALLOC_RAW" >&2; exit 1; }
if [ "$NEW_ALLOC" -le "$OLD_ALLOC" ]; then
    printf 'FAILED: node allocatable memory did NOT grow — %s (%s MiB) -> %s (%s MiB). The bigger box is not real in the number the scheduler sees.\n' \
        "$OLD_ALLOC_RAW" "$(mib "$OLD_ALLOC")" "$NEW_ALLOC_RAW" "$(mib "$NEW_ALLOC")" >&2
    exit 1
fi
printf '  ok: node allocatable memory GREW — %s (%s MiB) -> %s (%s MiB)\n' \
    "$OLD_ALLOC_RAW" "$(mib "$OLD_ALLOC")" "$NEW_ALLOC_RAW" "$(mib "$NEW_ALLOC")"

# --- the workload came back by itself ---
wait_cms_ready
CMS_POD_NEW="$(cms_pod)"
[ -n "$CMS_POD_NEW" ] || { printf 'FAILED: no CMS pod on the upgraded cluster\n' >&2; exit 1; }
printf '  ok: CMS pod on the upgraded cluster: %s\n' "$CMS_POD_NEW"
new_ready=$(jp "$APP_RES" "$CMS_NS" "$CMS_APP" '{.status.conditions[?(@.type=="Ready")].status}')
assert_eq "CMS Ready=True on the upgraded cluster (reached on its own)" "$new_ready" "True"
new_secret_val=$(kubectl -n "$CMS_NS" get secret "$CMS_SECRET_NAME" \
    -o jsonpath="{.data.${CMS_SECRET_KEY}}" 2>/dev/null | base64 -d 2>/dev/null || true)
assert_eq "the secret: ref resolves — re-sealed PAYLOAD_SECRET decrypts to the original value" \
    "$new_secret_val" "$CMS_SECRET_VALUE"

# --- the data ---
wait_for_pg_primary
NEW_CLAIM="$(wait_pg_claim_ready 600)"
NEW_CONN="$(pg_conn_secret "$NEW_CLAIM")"
[ -n "$NEW_CONN" ] || { printf 'FAILED: pg claim %s has no status.connectionSecretRef after the upgrade\n' "$NEW_CLAIM" >&2; exit 1; }
NEW_APPDB="$(conn_field "$NEW_CONN" db)"
[ -n "$NEW_APPDB" ] || { printf 'FAILED: could not resolve the CMS app database on the upgraded cluster\n' >&2; exit 1; }
printf '  ok: upgraded-cluster pg claim=%s conn-secret=%s db=%s\n' "$NEW_CLAIM" "$NEW_CONN" "$NEW_APPDB"
wait_for_appdb "$NEW_APPDB"

new_marker=$(psql_super "$NEW_APPDB" "SELECT note FROM ${MARKER_TABLE} WHERE id=1;" | tr -d '[:space:]')
assert_eq "known marker row reads back on the upgraded cluster" "$new_marker" "$MARKER_VALUE"
assert_eq "marker is identical to the source's" "$new_marker" "$old_marker"

# Double-compute on the TARGET too: if these two differ, the restored DB is
# still being written to and any source/target comparison would be noise — a
# distinguishable failure, reported as such.
compute_digest "$NEW_APPDB" "${TMPDIR_WORK}/digest.new.1"
compute_digest "$NEW_APPDB" "${TMPDIR_WORK}/digest.new.2"
assert_same_digest "digest is reproducible on the upgraded cluster (the restored DB is quiescent)" \
    "${TMPDIR_WORK}/digest.new.1" "${TMPDIR_WORK}/digest.new.2"

NEW_DIGEST_MD5="$(digest_md5 "${TMPDIR_WORK}/digest.new.1")"
printf '\n  --- upgraded-cluster data fingerprint (db %s) ---\n' "$NEW_APPDB"
sed 's/^/    | /' "${TMPDIR_WORK}/digest.new.1"
printf '    | TOTAL %s table(s), %s row(s), digest md5 %s\n\n' \
    "$(digest_tables "${TMPDIR_WORK}/digest.new.1")" \
    "$(digest_rows "${TMPDIR_WORK}/digest.new.1")" "$NEW_DIGEST_MD5"

# Row counts, table for table — asserted separately from the content hash so a
# "same rows, different bytes" failure reads differently from a "rows went
# missing" failure.
digest_counts "${TMPDIR_WORK}/digest.src.1" >"${TMPDIR_WORK}/counts.src"
digest_counts "${TMPDIR_WORK}/digest.new.1" >"${TMPDIR_WORK}/counts.new"
assert_same_digest "row counts match TABLE FOR TABLE across the upgrade" \
    "${TMPDIR_WORK}/counts.src" "${TMPDIR_WORK}/counts.new"

# THE assertion the whole walk exists for.
assert_same_digest "FULL DATA DIGEST is byte-identical across the substrate upgrade" \
    "${TMPDIR_WORK}/digest.src.1" "${TMPDIR_WORK}/digest.new.1"
assert_eq "digest md5 before == digest md5 after" "$NEW_DIGEST_MD5" "$SRC_DIGEST_MD5"

if [ "$BACKEND" = "s3" ]; then
    # The backup configuration is part of the cluster's state, so it should have
    # come across too — otherwise the upgraded cluster is silently unprotected.
    status_out="$(apprafter backup status)"
    assert_contains "scheduled off-site backup is still ENABLED after the upgrade" "$status_out" "Backup: ENABLED"
fi

# Informational (NOT an assertion): the target's recorded server-type
# PREFERENCE is a separate rung from the live fact, and `restore
# --server-type` upgrades the box without rewriting the preference.
printf '\n  --- target record vs live substrate (informational) ---\n'
apprafter target show "$TARGET" 2>/dev/null | sed 's/^/    | /' || true
printf '    | live server: id %s, type %s (Hetzner API)\n\n' "$NEW_SERVER_ID" "$NEW_SERVER_TYPE"

printf '=== substrate upgrade VERIFIED ===\n'
printf '  server:            id %s (%s)  ->  id %s (%s)\n' \
    "$OLD_SERVER_ID" "$OLD_SERVER_TYPE" "$NEW_SERVER_ID" "$NEW_SERVER_TYPE"
printf '  node allocatable:  %s (%s MiB)  ->  %s (%s MiB)\n' \
    "$OLD_ALLOC_RAW" "$(mib "$OLD_ALLOC")" "$NEW_ALLOC_RAW" "$(mib "$NEW_ALLOC")"
printf '  data digest:       %s  ==  %s  (%s table[s], %s row[s])\n' \
    "$SRC_DIGEST_MD5" "$NEW_DIGEST_MD5" "$SRC_TABLES" "$SRC_ROWS"
printf '  workload:          %s Ready=True on its own, secret: ref resolved, marker %s\n' \
    "$CMS_APP" "$MARKER_VALUE"
printf '  backend:           %s (%s)\n' "$BACKEND" "$RESTORE_REPO"

# Phase 8 (teardown + the API-verified zero-server check) runs in the EXIT trap,
# so it happens on every path — including a failure two phases ago.
