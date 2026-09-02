#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# 2.6d-4 — off-site S3 backup end-to-end on REAL Hetzner + a REAL S3 bucket
# (the operator's own, USER-GATED). Monolithic DR walk. This is the closure
# gate for the scheduled off-site backup surface: it exercises the actual
# `apprafter backup enable` / `status` / `check` / `prune` verbs against a
# genuine `s3:` repo, triggers the in-cluster CronJob runner, verifies a
# snapshot lands off-site, DESTROYS the source cluster, and restores it into a
# fresh box with `restore --reprovision` — proving the S3 artifact is a real
# disaster-recovery artifact, not just a host-local restic repo (that path is
# already covered by e2e/restore-reprovision-hetzner.sh).
#
# Flow:
#   provision + bootstrap  ->  deploy the REAL landing CMS (needs.pg + a
#   `secret:` ref, --env dev) + seed a known pg marker + a known sealed secret
#   value  ->  seal the CLUSTER S3 credential Secret into apprafter-system  ->
#   `apprafter backup enable --bucket <s3:> --credential <sealed> --credential-
#   file <dotenv> --at ... --i-have-saved-credentials`  ->  assert `backup
#   status` ENABLED + bucket  ->  trigger the CronJob NOW (kubectl create job
#   --from=cronjob/apprafter-backup), wait Complete  ->  assert a snapshot
#   exists off-site (backup status lastSuccess + `restic snapshots` >=1)  ->
#   `backup check` passes  ->  `backup prune` runs clean + stamps last-prune  ->
#   `apprafter destroy` (source dies; the S3 repo survives)  ->  `apprafter
#   restore <s3:> --reprovision --credential-file <dotenv>` into a fresh cluster
#   ->  verify the known pg marker + sealed secret value survive.
#
# TEARDOWN-SAFE: a `trap ... EXIT` destroys whatever cluster is up + API-verifies
# ZERO servers (LEAK WARNING otherwise). It NEVER deletes the user's bucket; the
# restic prefix is wiped best-effort ONLY when APPRAFTER_BACKUP_S3_E2E_PURGE=1.
# Helpers mirror e2e/restore-reprovision-hetzner.sh + e2e/backup-restore-
# hetzner.sh (kept self-contained so this walk reads independently).
#
# COST AWARENESS: provisions ONE real Hetzner server, destroys it, then
# re-provisions ANOTHER (the --reprovision path) — two short-lived boxes,
# single-digit cents — plus a handful of S3 PUT/GET on the operator's own
# bucket. The teardown trap ALWAYS destroys + API-verifies zero servers.
#
# Env — REQUIRED (all must be set or the walk SKIPS with exit 0):
#   APPRAFTER_BACKUP_S3_E2E=1        — master gate (opt-in).
#   HCLOUD_TOKEN                     — Hetzner token (project). REQUIRED.
#   APPRAFTER_S3_BUCKET              — restic repo URL, e.g.
#                                      s3:s3.eu-central-1.amazonaws.com/mybucket/apprafter-e2e
#   AWS_ACCESS_KEY_ID               — S3 access key.
#   AWS_SECRET_ACCESS_KEY           — S3 secret key.
#   RESTIC_PASSWORD                 — restic repo passphrase.
# Env — OPTIONAL:
#   AWS_DEFAULT_REGION              — S3 region (some providers need it).
#   APPRAFTER_DR_SSH_PUBLIC_KEY_PATH — ssh public key path (default ~/.ssh/id_ed25519.pub).
#   APPRAFTER_DR_REGION             — Hetzner region (default nbg1).
#   APPRAFTER_BACKUP_S3_E2E_PURGE=1 — after the walk, best-effort wipe of the
#                                     restic prefix (`restic forget --keep-last 1
#                                     --prune`); the BUCKET itself is never touched.
#   APPRAFTER_HETZNER_SKIP_DESTROY=1 — keep the cluster up (debug; destroy by hand).
#
# Exit: 0 = GREEN or SKIP (env precondition missing); 1 = assertion failure
#       (the trap tears the cluster down first). PASS/FAIL is judged by READING
#       THE LOG (a `FAILED:` marker on any assertion + the final `GREEN` banner),
#       since these walks are often run under wrappers that mask exit codes.

set -euo pipefail

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# ---------------------------------------------------------------
# Gate — skip (exit 0), not fail, when env preconditions are missing.
# Checked BEFORE anything is provisioned or the destroy trap is armed.
# ---------------------------------------------------------------
_missing=""
[ "${APPRAFTER_BACKUP_S3_E2E:-}" = "1" ] || _missing="${_missing} APPRAFTER_BACKUP_S3_E2E=1"
[ -n "${HCLOUD_TOKEN:-}" ]            || _missing="${_missing} HCLOUD_TOKEN"
[ -n "${APPRAFTER_S3_BUCKET:-}" ]     || _missing="${_missing} APPRAFTER_S3_BUCKET"
[ -n "${AWS_ACCESS_KEY_ID:-}" ]       || _missing="${_missing} AWS_ACCESS_KEY_ID"
[ -n "${AWS_SECRET_ACCESS_KEY:-}" ]   || _missing="${_missing} AWS_SECRET_ACCESS_KEY"
[ -n "${RESTIC_PASSWORD:-}" ]         || _missing="${_missing} RESTIC_PASSWORD"
if [ -n "$_missing" ]; then
    cat <<EOF
SKIP: 2.6d-4 off-site S3 backup e2e is USER-GATED — it needs a real Hetzner
project AND a real S3 bucket you own. Missing:${_missing}

Set these to run (the bucket + creds are YOURS; the walk never deletes the bucket):
  export APPRAFTER_BACKUP_S3_E2E=1
  export HCLOUD_TOKEN=<hetzner project token>
  export APPRAFTER_S3_BUCKET='s3:s3.eu-central-1.amazonaws.com/<bucket>/apprafter-e2e'
  export AWS_ACCESS_KEY_ID=<s3 access key>
  export AWS_SECRET_ACCESS_KEY=<s3 secret key>
  export RESTIC_PASSWORD=<restic passphrase>
  # optional:
  export AWS_DEFAULT_REGION=eu-central-1
  export APPRAFTER_DR_SSH_PUBLIC_KEY_PATH=~/.ssh/id_ed25519.pub   # default
  export APPRAFTER_BACKUP_S3_E2E_PURGE=1   # wipe the restic prefix at the end

Then: e2e/backup-s3-hetzner.sh
EOF
    exit 0
fi

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------
BR_TARGET="s3bak-cap"
REGION="${APPRAFTER_DR_REGION:-nbg1}"
SSH_KEY_PATH="${APPRAFTER_DR_SSH_PUBLIC_KEY_PATH:-$HOME/.ssh/id_ed25519.pub}"

S3_REPO="$APPRAFTER_S3_BUCKET"          # the restic s3: URL (the operator's own bucket)
CLUSTER_CRED_SECRET="backup-s3-creds"   # sealed cluster credential Secret name

CMS_REPO="https://github.com/AppRafter/apprafter"
CMS_PATH="landing/cms/apprafter"
CMS_APP="landing-cms"
CMS_NS="apprafter"
CMS_ENV="dev"
CMS_PG_CLAIM="${CMS_APP}-pg"
CMS_PG_CONN="${CMS_APP}-pg-conn"

CMS_SECRET_NAME="apprafter-landing-cms-secrets"
CMS_SECRET_KEY="PAYLOAD_SECRET"
CMS_SECRET_VALUE="${APPRAFTER_DR_FAKE_SECRET:-s3bak-walk-fake-payload-secret-0123456789abcdef}"

DR_MARKER_TABLE="s3bak_walk_marker"
DR_MARKER_VALUE="s3bak-known-payload-$(date +%Y%m%d%H%M%S)"

APP_RES="application.apprafter.io"
BACKUP_NS="apprafter-system"
BACKUP_CRONJOB="apprafter-backup"

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------
for t in kubectl curl python3; do
    command -v "$t" >/dev/null 2>&1 || { printf 'missing tool: %s\n' "$t" >&2; exit 2; }
done
# restic must be resolvable both here (snapshot assertion + optional purge) and
# by the CLI subprocess (backup enable/check/prune shell out to `restic`).
# lib.sh installs a `nix run nixpkgs#restic` wrapper on $PATH when absent.
ensure_restic_on_path
command -v restic >/dev/null 2>&1 || { printf 'ERROR: restic not resolvable even after the nix wrapper install\n' >&2; exit 2; }
printf 'restic: %s\n' "$(command -v restic)"

[ -r "$SSH_KEY_PATH" ] || { printf 'ssh public key not readable: %s (set APPRAFTER_DR_SSH_PUBLIC_KEY_PATH)\n' "$SSH_KEY_PATH" >&2; exit 2; }

# ---------------------------------------------------------------
# Assertion helpers (operate on $KUBECONFIG; mirror the DR walks).
# ---------------------------------------------------------------
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
assert_eq() {
    local desc="$1" got="$2" want="$3"
    if [ "$got" = "$want" ]; then
        printf '  ok: %s = %q\n' "$desc" "$got"
        return 0
    fi
    printf 'FAILED: %s — got %q, want %q\n' "$desc" "$got" "$want" >&2
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

# ---------------------------------------------------------------
# Hetzner API helpers (teardown verify + genuine-reprovision proof).
# ---------------------------------------------------------------
hetzner_server_count() {
    local token="$1" body
    { set +x; } 2>/dev/null
    body=$(curl -fsS -H "Authorization: Bearer ${token}" \
        "https://api.hetzner.cloud/v1/servers" 2>/dev/null || true)
    if [ -z "$body" ]; then printf '?'; return 0; fi
    if command -v jq >/dev/null 2>&1; then
        printf '%s' "$body" | jq '.servers | length' 2>/dev/null || printf '?'
    else
        printf '%s' "$body" | grep -o '"id":' | wc -l | tr -d ' '
    fi
}
hetzner_first_server_id() {
    local token="$1" body
    { set +x; } 2>/dev/null
    body=$(curl -fsS -H "Authorization: Bearer ${token}" \
        "https://api.hetzner.cloud/v1/servers" 2>/dev/null || true)
    [ -n "$body" ] || { printf ''; return 0; }
    if command -v jq >/dev/null 2>&1; then
        printf '%s' "$body" | jq -r '.servers[0].id // empty' 2>/dev/null || printf ''
    else
        printf '%s' "$body" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2
    fi
}

# jp <kind> <ns> <name> <jsonpath> — read one value (uses $KUBECONFIG)
jp() { kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true; }

# cms_pod — a CMS pod name (Running preferred).
cms_pod() {
    local running
    running=$(kubectl -n "$CMS_NS" get pod -l "app.kubernetes.io/name=$CMS_APP" \
        --field-selector=status.phase=Running \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
    if [ -n "$running" ]; then printf '%s' "$running"; return; fi
    kubectl -n "$CMS_NS" get pod -l "app.kubernetes.io/name=$CMS_APP" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# --- pg seed/read via exec into the CNPG PRIMARY as the postgres superuser over
#     the LOCAL unix socket (peer auth — no password/TCP; `-c postgres` is load-
#     bearing). Mirrors e2e/restore-reprovision-hetzner.sh. ---
cms_appdb() {
    kubectl -n "$CMS_NS" get secret "$CMS_PG_CONN" -o jsonpath='{.data.db}' 2>/dev/null | base64 -d 2>/dev/null || true
}
cms_approle() {
    kubectl -n "$CMS_NS" get secret "$CMS_PG_CONN" -o jsonpath='{.data.user}' 2>/dev/null | base64 -d 2>/dev/null || true
}
psql_super() {
    local db="$1" sql="$2" primary out rc errf
    primary=$(kubectl -n cnpg-system get pods -l 'cnpg.io/instanceRole=primary' -o name 2>/dev/null | head -1) || true
    [ -n "$primary" ] || primary=$(kubectl -n cnpg-system get pods -l 'role=primary' -o name 2>/dev/null | head -1) || true
    if [ -z "$primary" ] || [ -z "$db" ]; then
        printf 'psql_super: no primary (got "%s") or no db (got "%s")\n' "$primary" "$db" >&2
        printf ''; return 0
    fi
    { set +x; } 2>/dev/null
    errf="${TMPDIR:-/tmp}/psql_super.$$"
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
wait_for_pg_primary() {
    local ns=cnpg-system timeout="${1:-300}" waited=0 name
    printf '  wait: CNPG primary pod Ready in %s (timeout %ss) ...\n' "$ns" "$timeout"
    while [ "$waited" -lt "$timeout" ]; do
        name=$(kubectl -n "$ns" get pods -l 'cnpg.io/instanceRole=primary' -o name 2>/dev/null | head -1) || true
        [ -n "$name" ] || name=$(kubectl -n "$ns" get pods -l 'role=primary' -o name 2>/dev/null | head -1) || true
        if [ -n "$name" ] && kubectl -n "$ns" wait "$name" --for=condition=Ready --timeout=5s >/dev/null 2>&1; then
            printf '  ok: CNPG primary ready (%s)\n' "$name"; return 0
        fi
        sleep 5; waited=$((waited + 5))
    done
    printf 'FAILED: no CNPG primary became Ready in %s within %ss\n' "$ns" "$timeout" >&2
    return 1
}
wait_for_appdb() {
    local db="$1" timeout="${2:-240}" waited=0 got
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

# provision + bootstrap the (already-registered) target; sets $KUBECONFIG.
provision_and_bootstrap() {
    local target="$1"
    apprafter target use "$target"
    apprafter up --target "$target"
    apprafter kubeconfig --target "$target" >"$KC_FILE"
    export KUBECONFIG="$KC_FILE"
}
wait_platform_ready() {
    printf '  waiting for Argo CD platform Applications to Sync ...\n'
    local apps=(cilium argocd cert-manager apprafter-operator admission-webhook)
    local app deadline sync
    for app in "${apps[@]}"; do
        deadline=$(( $(date +%s) + 600 ))
        while [ "$(date +%s)" -lt "$deadline" ]; do
            sync=$(kubectl -n argocd get applications.argoproj.io "$app" \
                -o jsonpath='{.status.sync.status}' 2>/dev/null || true)
            [ "$sync" = "Synced" ] && break
            sleep 10
        done
        [ "$sync" = "Synced" ] || {
            printf 'FAILED: Argo CD Application %s not Synced within 10 min (got %q)\n' "$app" "$sync" >&2
            return 1
        }
        printf '  ok: Argo CD Application %s -> Synced\n' "$app"
    done
    printf '  ok: platform ready (Argo apps Synced)\n'
}
wait_cms_ready() {
    wait_jsonpath resourceclaim.apprafter.io "$CMS_NS" "$CMS_PG_CLAIM" '{.status.ready}' true 480
    wait_jsonpath "$APP_RES" "$CMS_NS" "$CMS_APP" '{.status.phase}' Ready 480
    kubectl -n "$CMS_NS" wait --for=condition=Available "deployment/${CMS_APP}" --timeout=480s
    retry 40 8 -- kubectl -n "$CMS_NS" wait --for=condition=Ready \
        pod -l "app.kubernetes.io/name=${CMS_APP}" --timeout=20s
}

# Deploy the CMS from its public repo path via --env dev (cwd = the manifest dir
# so `app add` finds the LOCAL Application.cue; --branch master). Seals the FAKE
# secret into the app ns first. Mirrors e2e/restore-reprovision-hetzner.sh.
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

# Wait for a Job to reach Complete (or Failed) — polls .status.conditions.
# On failure dumps the runner Job's pod logs (the restic/S3 error lives there).
wait_job_complete() {
    local job="$1" ns="$2" timeout="${3:-600}" deadline done_c failed_c
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait job/%s -n %s to Complete (timeout %ss) ...\n' "$job" "$ns" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        done_c=$(kubectl -n "$ns" get job "$job" \
            -o jsonpath='{.status.conditions[?(@.type=="Complete")].status}' 2>/dev/null || true)
        failed_c=$(kubectl -n "$ns" get job "$job" \
            -o jsonpath='{.status.conditions[?(@.type=="Failed")].status}' 2>/dev/null || true)
        if [ "$done_c" = "True" ]; then
            printf '  ok: job/%s Completed\n' "$job"
            return 0
        fi
        if [ "$failed_c" = "True" ]; then
            printf 'FAILED: job/%s reported Failed\n' "$job" >&2
            kubectl -n "$ns" logs "job/$job" --all-containers --tail=120 >&2 2>&1 || true
            return 1
        fi
        sleep 8
    done
    printf 'FAILED: job/%s did not Complete within %ss\n' "$job" "$timeout" >&2
    kubectl -n "$ns" describe job "$job" >&2 2>&1 || true
    kubectl -n "$ns" logs "job/$job" --all-containers --tail=120 >&2 2>&1 || true
    return 1
}

# ---------------------------------------------------------------
# Preconditions — BEFORE the trap is armed (exit 2, never destroys).
# The operator's own S3 creds MUST NOT leak into the provisioned cluster via
# ambient env — the CLI reads them from the --credential-file we write, and the
# cluster gets them ONLY through the sealed Secret. But the CLI's `backup
# enable`/`check`/`prune` preflight + this walk's own `restic` calls DO read
# AWS_*/RESTIC_PASSWORD from env, so we keep them exported (they're the
# operator's laptop creds, not the cluster's).
# ---------------------------------------------------------------
TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"; mkdir -p "$APPRAFTER_CONFIG_DIR"
export APPRAFTER_CONFIG_DIR
KC_FILE="${TMPDIR_WORK}/kubeconfig"
CRED_FILE="${TMPDIR_WORK}/backup-creds.env"
BR_CREATED=0

# The operator-side dotenv the --credential-file paths consume (AWS_* +
# RESTIC_PASSWORD [+ region]). 0600 — it holds live S3 creds.
build_cred_file() {
    { set +x; } 2>/dev/null
    umask 077
    {
        printf 'AWS_ACCESS_KEY_ID=%s\n'     "$AWS_ACCESS_KEY_ID"
        printf 'AWS_SECRET_ACCESS_KEY=%s\n' "$AWS_SECRET_ACCESS_KEY"
        printf 'RESTIC_PASSWORD=%s\n'       "$RESTIC_PASSWORD"
        [ -n "${AWS_DEFAULT_REGION:-}" ] && printf 'AWS_DEFAULT_REGION=%s\n' "$AWS_DEFAULT_REGION"
    } >"$CRED_FILE"
    chmod 0600 "$CRED_FILE"
}

# ---------------------------------------------------------------
# Cleanup trap — destroy whatever is up + API-verify ZERO servers.
# NEVER deletes the user's bucket. Optionally wipes the restic PREFIX.
# ---------------------------------------------------------------
cleanup() {
    local exit_code=$?
    { set +x; } 2>/dev/null
    if [ "$exit_code" -ne 0 ]; then
        printf '\nFAILED: backup-s3-hetzner FAILED at %s (exit %d)\n' "$(elapsed)" "$exit_code" >&2
        if [ "$BR_CREATED" -eq 1 ] && apprafter kubeconfig --target "$BR_TARGET" >"$KC_FILE" 2>/dev/null; then
            KUBECONFIG="$KC_FILE" dump_diagnostics || true
        fi
    fi

    # Optional restic-prefix purge (the operator owns the bucket; we own the
    # prefix). NEVER deletes the bucket. `forget --keep-last 1 --prune` reaps
    # this walk's snapshots; keep-last-1 leaves a valid repo behind.
    if [ "${APPRAFTER_BACKUP_S3_E2E_PURGE:-}" = "1" ]; then
        printf '\n=== APPRAFTER_BACKUP_S3_E2E_PURGE=1 — best-effort wipe of the restic prefix %s ===\n' "$S3_REPO" >&2
        restic -r "$S3_REPO" forget --keep-last 1 --prune >&2 2>&1 \
            || printf 'WARN: restic prefix purge returned non-zero (left as-is; bucket untouched)\n' >&2
    fi

    if [ -z "${APPRAFTER_HETZNER_SKIP_DESTROY:-}" ]; then
        if [ "$BR_CREATED" -eq 1 ]; then
            printf '\n=== destroying cluster (%s) ===\n' "$BR_TARGET" >&2
            apprafter destroy --yes --target "$BR_TARGET" || \
                printf 'WARN: destroy --target %s returned non-zero\n' "$BR_TARGET" >&2
            local n; n=$(hetzner_server_count "$HCLOUD_TOKEN")
            if [ "$n" = "0" ]; then
                printf 'ok: project has ZERO servers after destroy (API-verified)\n' >&2
            else
                printf 'LEAK WARNING: project reports %s server(s) after destroy — INSPECT https://console.hetzner.cloud AND DELETE STRAGGLERS BY HAND\n' "$n" >&2
            fi
        fi
        rm -rf "$TMPDIR_WORK"
        [ -n "${RESTIC_WRAPPER_BIN_DIR:-}" ] && rm -rf "$RESTIC_WRAPPER_BIN_DIR" || true
    else
        printf '\nAPPRAFTER_HETZNER_SKIP_DESTROY set — leaving the cluster UP.\n' >&2
        [ "$BR_CREATED" -eq 1 ] && printf 'Destroy by hand: APPRAFTER_CONFIG_DIR=%s apprafter destroy --yes --target %s\n' "$APPRAFTER_CONFIG_DIR" "$BR_TARGET" >&2
    fi
    exit "$exit_code"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# ---------------------------------------------------------------
# Phase 0: register the target + build the operator credential dotenv.
# ---------------------------------------------------------------
phase "Phase 0: setup — register target, build operator S3 credential file"
build_cred_file
printf '  ok: operator S3 credential file written (0600): %s\n' "$CRED_FILE"
{ set +x; } 2>/dev/null
# `--server-type` is REQUIRED since 2.16h-a removed the implicit default
# (Decision 0): `apply` refuses with `server_type_not_selected` rather than
# picking a SKU. This walk predates that change, so it could not provision at
# all. Same default and override as mvp.sh / machine-picker-walk.
apprafter target add "$BR_TARGET" \
    --provider hetzner-cloud --tier solo --region "$REGION" \
    --server-type "${APPRAFTER_E2E_SERVER_TYPE:-cpx22}" \
    --token "$HCLOUD_TOKEN" --ssh-key "$SSH_KEY_PATH" \
    --no-interactive --no-ping --force
printf '  ok: target %s registered\n' "$BR_TARGET"

# ---------------------------------------------------------------
# Phase 1: provision + bootstrap.
# ---------------------------------------------------------------
phase "Phase 1: provision + bootstrap the SOURCE cluster ($BR_TARGET)"
provision_and_bootstrap "$BR_TARGET"
BR_CREATED=1
wait_platform_ready

# ---------------------------------------------------------------
# Phase 2: deploy CMS + seed the known marker + secret.
# ---------------------------------------------------------------
phase "Phase 2: deploy the REAL CMS (needs.pg + secret ref) + seed known data"
deploy_cms
CMS_POD_SRC="$(cms_pod)"
[ -n "$CMS_POD_SRC" ] || { printf 'FAILED: no CMS pod on source\n' >&2; exit 1; }
src_ready=$(jp "$APP_RES" "$CMS_NS" "$CMS_APP" '{.status.conditions[?(@.type=="Ready")].status}')
assert_eq "CMS Ready on source (secret resolved, not EnvSecretMissing)" "$src_ready" "True"
wait_for_pg_primary
SRC_APPDB="$(cms_appdb)"; SRC_APPROLE="$(cms_approle)"
[ -n "$SRC_APPDB" ] || { printf 'FAILED: could not resolve CMS app db from %s/%s\n' "$CMS_NS" "$CMS_PG_CONN" >&2; exit 1; }
wait_for_appdb "$SRC_APPDB"
seed_sql="CREATE TABLE IF NOT EXISTS ${DR_MARKER_TABLE} (id INT PRIMARY KEY, note TEXT NOT NULL);"
seed_sql="${seed_sql} INSERT INTO ${DR_MARKER_TABLE} (id, note) VALUES (1, '${DR_MARKER_VALUE}') ON CONFLICT (id) DO UPDATE SET note = EXCLUDED.note;"
[ -n "$SRC_APPROLE" ] && seed_sql="${seed_sql} ALTER TABLE ${DR_MARKER_TABLE} OWNER TO \"${SRC_APPROLE}\";"
psql_super "$SRC_APPDB" "$seed_sql" >/dev/null
old_marker=$(psql_super "$SRC_APPDB" "SELECT note FROM ${DR_MARKER_TABLE} WHERE id=1;" | tr -d '[:space:]')
assert_eq "known marker row present on source pg" "$old_marker" "$DR_MARKER_VALUE"

# ---------------------------------------------------------------
# Phase 3: seal the CLUSTER S3 credential Secret into apprafter-system.
# The CronJob runner mounts THIS via `envFrom: secretRef` — the cluster gets
# creds ONLY through the sealed Secret (H1: passphrase + S3 keys never in chart
# values). Keys mirror the operator dotenv.
# ---------------------------------------------------------------
phase "Phase 3: seal the cluster S3 credential Secret ($CLUSTER_CRED_SECRET) into $BACKUP_NS"
{ set +x; } 2>/dev/null
seal_args=(
    --from-literal "AWS_ACCESS_KEY_ID=${AWS_ACCESS_KEY_ID}"
    --from-literal "AWS_SECRET_ACCESS_KEY=${AWS_SECRET_ACCESS_KEY}"
    --from-literal "RESTIC_PASSWORD=${RESTIC_PASSWORD}"
)
[ -n "${AWS_DEFAULT_REGION:-}" ] && seal_args+=( --from-literal "AWS_DEFAULT_REGION=${AWS_DEFAULT_REGION}" )
apprafter secret seal "$CLUSTER_CRED_SECRET" "${seal_args[@]}" --namespace "$BACKUP_NS"
wait_jsonpath secret "$BACKUP_NS" "$CLUSTER_CRED_SECRET" '{.metadata.name}' "$CLUSTER_CRED_SECRET" 180
printf '  ok: cluster S3 credential Secret sealed + unsealed in %s\n' "$BACKUP_NS"

# ---------------------------------------------------------------
# Phase 4: apprafter backup enable → merge-patch spec.backup + assert status.
# 2.22g: the schedule surface is `--at HH:MM` + `--timezone`; a scheduled run is
# not waited for (Phase 5 triggers a manual Job), so the time is arbitrary — what
# is asserted is that the ZONE reached the CronJob. The preflight `restic init`
# uses the --credential-file creds to reach the real bucket.
# ---------------------------------------------------------------
phase "Phase 4: apprafter backup enable (--bucket $S3_REPO) + status ENABLED"
{ set +x; } 2>/dev/null
apprafter backup enable \
    --bucket "$S3_REPO" \
    --credential "$CLUSTER_CRED_SECRET" \
    --credential-file "$CRED_FILE" \
    --at 22:30 \
    --timezone Europe/Berlin \
    --i-have-saved-credentials
status_out="$(apprafter backup status)"
printf '%s\n' "$status_out"
assert_contains "backup status shows ENABLED" "$status_out" "Backup: ENABLED"
assert_contains "backup status shows the bucket" "$status_out" "$S3_REPO"
enabled_cr=$(jp platformstack "$BACKUP_NS" default '{.spec.backup.enabled}')
assert_eq "spec.backup.enabled is true on the PlatformStack" "$enabled_cr" "true"
# The chart-rendered CronJob exists (the runner is wired in-cluster).
wait_jsonpath cronjob "$BACKUP_NS" "$BACKUP_CRONJOB" '{.metadata.name}' "$BACKUP_CRONJOB" 300
printf '  ok: apprafter-backup CronJob present (runner wired)\n'

# ---------------------------------------------------------------
# Phase 5: trigger a backup run NOW (don't wait for the cron), wait Complete.
# ---------------------------------------------------------------
phase "Phase 5: trigger the backup CronJob manually + wait for the runner Job to Complete"
MANUAL_JOB="apprafter-backup-manual-$(date +%s)"
kubectl -n "$BACKUP_NS" create job --from="cronjob/${BACKUP_CRONJOB}" "$MANUAL_JOB"
wait_job_complete "$MANUAL_JOB" "$BACKUP_NS" 600
printf '  ok: manual backup Job %s Completed\n' "$MANUAL_JOB"

# ---------------------------------------------------------------
# Phase 6: assert a snapshot landed OFF-SITE (status lastSuccess + restic).
# ---------------------------------------------------------------
phase "Phase 6: assert a snapshot exists in the S3 bucket (backup status + restic snapshots)"
# The runner stamps apprafter-backup-status.lastSuccess after a good run.
wait_jsonpath configmap "$BACKUP_NS" apprafter-backup-status \
    '{.data.lastSuccess}' "$(jp configmap "$BACKUP_NS" apprafter-backup-status '{.data.lastSuccess}')" 5 || true
status_out="$(apprafter backup status)"
printf '%s\n' "$status_out"
# lastSuccess must be present + not "never".
last_success=$(printf '%s' "$status_out" | awk -F': *' '/lastSuccess:/ {print $2; exit}')
if [ -z "$last_success" ] || [ "$last_success" = "never" ]; then
    printf 'FAILED: backup status lastSuccess is empty/never after a completed run (got %q)\n' "${last_success:-}" >&2
    exit 1
fi
printf '  ok: backup status reports lastSuccess=%s\n' "$last_success"
# And the snapshot is genuinely in the bucket: restic sees >=1 snapshot.
snap_json="$(restic -r "$S3_REPO" snapshots --json 2>/dev/null || true)"
snap_count=$(printf '%s' "$snap_json" | python3 -c 'import json,sys;
d=sys.stdin.read().strip()
try: print(len(json.loads(d)) if d else 0)
except Exception: print(0)')
if [ "${snap_count:-0}" -lt 1 ]; then
    printf 'FAILED: restic reports %s snapshots in %s (expected >=1)\n' "${snap_count:-0}" "$S3_REPO" >&2
    exit 1
fi
printf '  ok: restic lists %s snapshot(s) in the bucket %s\n' "$snap_count" "$S3_REPO"

# ---------------------------------------------------------------
# Phase 7: apprafter backup check + prune (operator-side, full creds).
# ---------------------------------------------------------------
phase "Phase 7: apprafter backup check + prune (operator-side verbs)"
{ set +x; } 2>/dev/null
apprafter backup check --credential-file "$CRED_FILE"
printf '  ok: apprafter backup check passed (structural integrity)\n'
# Prune runs clean (nothing to forget yet is fine) and stamps last-prune.
apprafter backup prune --credential-file "$CRED_FILE"
status_out="$(apprafter backup status)"
printf '%s\n' "$status_out"
last_prune=$(printf '%s' "$status_out" | awk -F': *' '/Last prune:/ {print $2; exit}')
if [ -z "$last_prune" ] || [ "$last_prune" = "never" ]; then
    printf 'FAILED: backup status Last prune not stamped after prune (got %q)\n' "${last_prune:-}" >&2
    exit 1
fi
printf '  ok: apprafter.io/last-prune stamped (Last prune: %s)\n' "$last_prune"

# ---------------------------------------------------------------
# Phase 8: DESTROY the source cluster — the "source is dead" event.
# The S3 repo survives (it's off-site).
# ---------------------------------------------------------------
phase "Phase 8: apprafter destroy (source dies) — the off-site S3 repo survives"
SRC_SERVER_ID="$(hetzner_first_server_id "$HCLOUD_TOKEN")"
{ set +x; } 2>/dev/null
apprafter destroy --yes --target "$BR_TARGET"
dead_n=$(hetzner_server_count "$HCLOUD_TOKEN")
assert_eq "source project has ZERO servers after destroy (source is dead)" "$dead_n" "0"
# The off-site repo is untouched by a cluster destroy.
snap_json="$(restic -r "$S3_REPO" snapshots --json 2>/dev/null || true)"
snap_count=$(printf '%s' "$snap_json" | python3 -c 'import json,sys;
d=sys.stdin.read().strip()
try: print(len(json.loads(d)) if d else 0)
except Exception: print(0)')
[ "${snap_count:-0}" -ge 1 ] || { printf 'FAILED: S3 snapshots vanished with the cluster (got %s)\n' "${snap_count:-0}" >&2; exit 1; }
printf '  ok: source destroyed; off-site S3 repo survived (%s snapshot[s])\n' "$snap_count"

# ---------------------------------------------------------------
# Phase 9: restore --reprovision from the S3 repo into a fresh cluster.
# ---------------------------------------------------------------
phase "Phase 9: apprafter restore <s3:> --reprovision --target $BR_TARGET (clone-to-new from off-site)"
{ set +x; } 2>/dev/null
restore_log="${TMPDIR_WORK}/restore.log"
apprafter restore "$S3_REPO" --reprovision --target "$BR_TARGET" \
    --credential-file "$CRED_FILE" \
    > "$restore_log" 2>&1 || { printf 'FAILED: restore --reprovision returned non-zero:\n'; sed 's/^/    /' "$restore_log" >&2; exit 1; }
# BR_CREATED stays 1 — a FRESH cluster now exists in the target; teardown destroys it.
grep -qE 'provisioning a fresh cluster' "$restore_log" || { printf 'FAILED: restore did not run the reprovision step\n'; sed 's/^/    /' "$restore_log" >&2; exit 1; }
grep -qE 'Restored backup' "$restore_log" || { printf 'FAILED: restore did not report completion\n'; sed 's/^/    /' "$restore_log" >&2; exit 1; }
printf '  ok: restore --reprovision provisioned a fresh cluster + replayed from the S3 repo\n'

# ---------------------------------------------------------------
# Phase 10: verify the reprovisioned cluster — data + secret survive, NEW box.
# ---------------------------------------------------------------
phase "Phase 10: verify reprovisioned cluster — CMS Ready, secret + marker restored, NEW box"
apprafter kubeconfig --target "$BR_TARGET" >"$KC_FILE"
export KUBECONFIG="$KC_FILE"

NEW_SERVER_ID="$(hetzner_first_server_id "$HCLOUD_TOKEN")"
[ -n "$NEW_SERVER_ID" ] || { printf 'FAILED: no server in the project after reprovision\n' >&2; exit 1; }
if [ -n "$SRC_SERVER_ID" ] && [ "$NEW_SERVER_ID" = "$SRC_SERVER_ID" ]; then
    printf 'FAILED: server id unchanged (%s) — not a genuine re-provision\n' "$NEW_SERVER_ID" >&2; exit 1
fi
printf '  ok: genuinely re-provisioned (source id %s -> new id %s)\n' "${SRC_SERVER_ID:-?}" "$NEW_SERVER_ID"

wait_cms_ready
new_ready=$(jp "$APP_RES" "$CMS_NS" "$CMS_APP" '{.status.conditions[?(@.type=="Ready")].status}')
assert_eq "CMS Ready on reprovisioned cluster (re-sealed PAYLOAD_SECRET resolved)" "$new_ready" "True"
new_secret_val=$(kubectl -n "$CMS_NS" get secret "$CMS_SECRET_NAME" \
    -o jsonpath="{.data.${CMS_SECRET_KEY}}" 2>/dev/null | base64 -d 2>/dev/null || true)
assert_eq "re-sealed PAYLOAD_SECRET decrypts to the original value" "$new_secret_val" "$CMS_SECRET_VALUE"

wait_for_pg_primary
NEW_APPDB="$(cms_appdb)"
[ -n "$NEW_APPDB" ] || { printf 'FAILED: could not resolve CMS app db on reprovisioned cluster\n' >&2; exit 1; }
wait_for_appdb "$NEW_APPDB"
new_marker=$(psql_super "$NEW_APPDB" "SELECT note FROM ${DR_MARKER_TABLE} WHERE id=1;" | tr -d '[:space:]')
assert_eq "known marker row restored from the S3 backup into the reprovisioned cluster's pg" "$new_marker" "$DR_MARKER_VALUE"
assert_eq "source/reprovisioned equivalence — marker identical" "$new_marker" "$old_marker"

printf '\n=== GREEN: 2.6d-4 off-site S3 backup validated end-to-end on real Hetzner ===\n'
printf 'provision -> CMS+seed -> seal S3 creds -> backup enable -> manual Job -> off-site snapshot -> check+prune -> DESTROY (source dead) -> restore --reprovision (fresh box %s) from S3 -> data+secret intact\n' "$NEW_SERVER_ID"
