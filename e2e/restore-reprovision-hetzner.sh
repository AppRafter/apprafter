#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# 2.6d T13 — `apprafter restore --reprovision` (clone-to-new) on REAL Hetzner.
#
# The "source cluster is DEAD, rebuild from nothing" DR path. kind has no cloud
# provider, so --reprovision (which drives real provisioning) is real-Hetzner
# only. ONE cluster, ONE token. Flow:
#
#   provision + bootstrap  ->  deploy the REAL landing CMS (needs.pg + a
#   `secret:` ref, --env dev) + seed a known marker  ->  `apprafter backup
#   create` (encrypted restic repo ON THE HOST)  ->  `apprafter destroy` (the
#   source dies; the host backup survives; the TARGET REGISTRATION is kept)  ->
#   `apprafter restore <repo> --reprovision --target <same>` (provisions a FRESH
#   cluster in the emptied target, then replays)  ->  verify the reprovisioned
#   cluster: CMS auto-Ready, re-sealed secret decrypts, marker restored, and the
#   Hetzner server id CHANGED (genuinely a new box, not the old one).
#
# TEARDOWN-SAFE: a `trap ... EXIT` destroys whatever cluster is up + API-verifies
# ZERO servers (LEAK WARNING otherwise). Helpers mirror e2e/backup-restore-
# hetzner.sh (kept self-contained so this walk runs + reads independently).
#
# Env:
#   HCLOUD_TOKEN_OLD                 — Hetzner token (project). REQUIRED.
#   APPRAFTER_DR_SSH_PUBLIC_KEY_PATH — ssh public key path. REQUIRED.
#   APPRAFTER_DR_REGION              — region (default nbg1).
#   APPRAFTER_HETZNER_SKIP_DESTROY=1 — keep the cluster up (debug; destroy by hand).
#
# Exit: 0 = GREEN; 1 = assertion failure (trap tears the cluster down first);
#       2 = env precondition missing (checked BEFORE the destroy trap is armed).

set -euo pipefail

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------
RR_TARGET="rr-cap"
REGION="${APPRAFTER_DR_REGION:-nbg1}"

CMS_REPO="https://github.com/AppRafter/apprafter"
CMS_PATH="landing/cms/apprafter"
CMS_APP="landing-cms"
CMS_NS="apprafter"
CMS_ENV="dev"
CMS_PG_CLAIM="${CMS_APP}-pg"
CMS_PG_CONN="${CMS_APP}-pg-conn"

CMS_SECRET_NAME="apprafter-landing-cms-secrets"
CMS_SECRET_KEY="PAYLOAD_SECRET"
CMS_SECRET_VALUE="${APPRAFTER_DR_FAKE_SECRET:-dr-walk-fake-payload-secret-0123456789abcdef}"

DR_MARKER_TABLE="dr_walk_marker"
DR_MARKER_VALUE="dr-known-payload-$(date +%Y%m%d)"

APP_RES="application.apprafter.io"
RESTIC_PASS="dr-walk-restic-passphrase-2026"

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------
for t in kubectl curl python3; do
    command -v "$t" >/dev/null 2>&1 || { printf 'missing tool: %s\n' "$t" >&2; exit 2; }
done
# restic must be resolvable by the CLI subprocess (Command::new("restic")).
# lib.sh installs a `nix run nixpkgs#restic` wrapper on $PATH when absent.
ensure_restic_on_path
command -v restic >/dev/null 2>&1 || { printf 'ERROR: restic not resolvable even after the nix wrapper install\n' >&2; exit 2; }
printf 'restic: %s\n' "$(command -v restic)"

# ---------------------------------------------------------------
# Assertion helpers (operate on $KUBECONFIG; mirror the DR walk).
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

# hetzner_first_server_id <token> — the first server's numeric id (or '' if none).
hetzner_first_server_id() {
    local token="$1" body
    { set +x; } 2>/dev/null
    body=$(curl -fsS -H "Authorization: Bearer ${token}" \
        "https://api.hetzner.cloud/v1/servers" 2>/dev/null || true)
    [ -n "$body" ] || { printf ''; return 0; }
    printf '%s' "$body" | jq -r '.servers[0].id // empty' 2>/dev/null || printf ''
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
#     bearing). See e2e/backup-restore-hetzner.sh for the full rationale. ---
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
# so `app add` finds the LOCAL Application.cue; --branch master for the repo's
# default branch). Seals the FAKE secret into the app ns first.
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
# Preconditions — BEFORE the trap is armed (exit 2, never destroys).
# ---------------------------------------------------------------
: "${HCLOUD_TOKEN_OLD:?set HCLOUD_TOKEN_OLD (exit 2 before any provision)}"
: "${APPRAFTER_DR_SSH_PUBLIC_KEY_PATH:?set APPRAFTER_DR_SSH_PUBLIC_KEY_PATH (exit 2)}"
[ -r "$APPRAFTER_DR_SSH_PUBLIC_KEY_PATH" ] || { printf 'ssh key not readable: %s\n' "$APPRAFTER_DR_SSH_PUBLIC_KEY_PATH" >&2; exit 2; }
unset HCLOUD_TOKEN || true
unset APPRAFTER_SSH_PUBLIC_KEY || true

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"; mkdir -p "$APPRAFTER_CONFIG_DIR"
export APPRAFTER_CONFIG_DIR
RESTIC_REPO="${TMPDIR_WORK}/restic-repo"   # ON THE HOST — survives the destroy
KC_FILE="${TMPDIR_WORK}/kubeconfig"
RR_CREATED=0

# ---------------------------------------------------------------
# Cleanup trap — destroy whatever is up + API-verify ZERO servers.
# ---------------------------------------------------------------
cleanup() {
    local exit_code=$?
    if [ "$exit_code" -ne 0 ]; then
        printf '\nFAILED: restore-reprovision-hetzner FAILED at %s (exit %d)\n' "$(elapsed)" "$exit_code" >&2
        if [ "$RR_CREATED" -eq 1 ] && apprafter kubeconfig --target "$RR_TARGET" >"$KC_FILE" 2>/dev/null; then
            KUBECONFIG="$KC_FILE" dump_diagnostics || true
        fi
    fi
    if [ -z "${APPRAFTER_HETZNER_SKIP_DESTROY:-}" ]; then
        if [ "$RR_CREATED" -eq 1 ]; then
            printf '\n=== destroying cluster (%s) ===\n' "$RR_TARGET" >&2
            apprafter destroy --yes --target "$RR_TARGET" || \
                printf 'WARN: destroy --target %s returned non-zero\n' "$RR_TARGET" >&2
            local n; n=$(hetzner_server_count "$HCLOUD_TOKEN_OLD")
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
        [ "$RR_CREATED" -eq 1 ] && printf 'Destroy by hand: APPRAFTER_CONFIG_DIR=%s apprafter destroy --yes --target %s\n' "$APPRAFTER_CONFIG_DIR" "$RR_TARGET" >&2
    fi
    exit "$exit_code"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# ---------------------------------------------------------------
# Phase 0: register the target (its OWN token) — one project, one cluster.
# ---------------------------------------------------------------
phase "Phase 0: setup — fresh config dir, one token, teardown armed"
cat <<'COST_NOTE'
  COST AWARENESS: this walk provisions ONE real Hetzner server, destroys it, then
  re-provisions ANOTHER (the --reprovision path) — two short-lived boxes total,
  single-digit cents. The teardown trap ALWAYS destroys + API-verifies zero.
COST_NOTE
{ set +x; } 2>/dev/null
apprafter target add "$RR_TARGET" \
    --provider hetzner-cloud --tier solo --region "$REGION" \
    --server-type "${APPRAFTER_E2E_SERVER_TYPE:-cpx22}" \
    --token "$HCLOUD_TOKEN_OLD" --ssh-key "$APPRAFTER_DR_SSH_PUBLIC_KEY_PATH" \
    --no-interactive --no-ping --force
printf '  ok: target %s registered\n' "$RR_TARGET"

# ---------------------------------------------------------------
# Phase 1: provision + bootstrap.
# ---------------------------------------------------------------
phase "Phase 1: provision + bootstrap the SOURCE cluster ($RR_TARGET)"
provision_and_bootstrap "$RR_TARGET"
RR_CREATED=1
wait_platform_ready
SRC_SERVER_ID="$(hetzner_first_server_id "$HCLOUD_TOKEN_OLD")"
printf '  ok: source cluster up (hetzner server id %s)\n' "${SRC_SERVER_ID:-?}"

# ---------------------------------------------------------------
# Phase 2: deploy CMS + seed the known marker.
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
# Phase 3: backup create -> encrypted restic repo ON THE HOST.
# ---------------------------------------------------------------
phase "Phase 3: apprafter backup create -> encrypted restic repo (host, survives destroy)"
{ set +x; } 2>/dev/null
apprafter backup create --repo "$RESTIC_REPO" --passphrase "$RESTIC_PASS"
[ -f "${RESTIC_REPO}/config" ] || { printf 'FAILED: restic repo config missing at %s\n' "$RESTIC_REPO" >&2; exit 1; }
if RESTIC_PASSWORD=dr-wrong restic -r "$RESTIC_REPO" snapshots >/dev/null 2>&1; then
    printf 'FAILED: restic accepted a WRONG passphrase\n' >&2; exit 1
fi
printf '  ok: encrypted restic repo created on the host\n'

# ---------------------------------------------------------------
# Phase 4: DESTROY the source cluster — the "source is dead" event.
# ---------------------------------------------------------------
phase "Phase 4: apprafter destroy (source dies) — keep the target registration + host backup"
{ set +x; } 2>/dev/null
apprafter destroy --yes --target "$RR_TARGET"
dead_n=$(hetzner_server_count "$HCLOUD_TOKEN_OLD")
assert_eq "source project has ZERO servers after destroy (source is dead)" "$dead_n" "0"
[ -f "${RESTIC_REPO}/config" ] || { printf 'FAILED: host restic repo vanished with the cluster\n' >&2; exit 1; }
printf '  ok: source destroyed; host backup survived; target %s still registered\n' "$RR_TARGET"

# ---------------------------------------------------------------
# Phase 5: restore --reprovision — THE T13 command.
# ---------------------------------------------------------------
phase "Phase 5: apprafter restore <repo> --reprovision --target $RR_TARGET (clone-to-new)"
{ set +x; } 2>/dev/null
restore_log="${TMPDIR_WORK}/restore.log"
apprafter restore "$RESTIC_REPO" --reprovision --target "$RR_TARGET" --passphrase "$RESTIC_PASS" \
    > "$restore_log" 2>&1 || { printf 'FAILED: restore --reprovision returned non-zero:\n'; sed 's/^/    /' "$restore_log" >&2; exit 1; }
# RR_CREATED stays 1 — a FRESH cluster now exists in the target; teardown destroys it.
grep -qE 'provisioning a fresh cluster' "$restore_log" || { printf 'FAILED: restore did not run the reprovision step\n'; sed 's/^/    /' "$restore_log" >&2; exit 1; }
grep -qE 'namespaces ensured' "$restore_log" || { printf 'FAILED: restore did not ensure namespaces (the EnsureNamespaces fix)\n'; sed 's/^/    /' "$restore_log" >&2; exit 1; }
grep -qE 'Restored backup' "$restore_log" || { printf 'FAILED: restore did not report completion\n'; sed 's/^/    /' "$restore_log" >&2; exit 1; }
printf '  ok: restore --reprovision provisioned a fresh cluster + replayed (reprovision + namespaces + restore markers present)\n'

# ---------------------------------------------------------------
# Phase 6: verify the reprovisioned cluster.
# ---------------------------------------------------------------
phase "Phase 6: verify reprovisioned cluster — CMS auto-Ready, secret + marker restored, NEW box"
apprafter kubeconfig --target "$RR_TARGET" >"$KC_FILE"
export KUBECONFIG="$KC_FILE"

NEW_SERVER_ID="$(hetzner_first_server_id "$HCLOUD_TOKEN_OLD")"
[ -n "$NEW_SERVER_ID" ] || { printf 'FAILED: no server in the project after reprovision\n' >&2; exit 1; }
if [ -n "$SRC_SERVER_ID" ] && [ "$NEW_SERVER_ID" = "$SRC_SERVER_ID" ]; then
    printf 'FAILED: server id unchanged (%s) — not a genuine re-provision\n' "$NEW_SERVER_ID" >&2; exit 1
fi
printf '  ok: genuinely re-provisioned (source id %s -> new id %s)\n' "${SRC_SERVER_ID:-?}" "$NEW_SERVER_ID"

wait_cms_ready
CMS_POD_NEW="$(cms_pod)"
[ -n "$CMS_POD_NEW" ] || { printf 'FAILED: no restored CMS pod on the reprovisioned cluster\n' >&2; exit 1; }
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
assert_eq "known marker row restored into the reprovisioned cluster's pg" "$new_marker" "$DR_MARKER_VALUE"
assert_eq "source/reprovisioned equivalence — marker identical" "$new_marker" "$old_marker"

printf '\n=== GREEN: 2.6d T13 restore --reprovision (clone-to-new) validated end-to-end on real Hetzner ===\n'
printf 'provision -> CMS+seed -> backup -> DESTROY (source dead) -> restore --reprovision (fresh box %s) -> data+secret intact\n' "$NEW_SERVER_ID"
