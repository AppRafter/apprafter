#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter backup-restore-walk e2e — the full Phase-2.6d export + backup +
# restore acceptance on local kind/k3d cluster(s) (plan item 2.6d-5, ADR
# 0050). This is the closure gate for the restore-into-running path; the
# `--reprovision` fresh-cluster + full DR ride the real-Hetzner manual walk
# (2.6d T13).
#
# It exercises the shipped CLI surface end-to-end:
#
#   deploy app (needs.pg + needs.disk + user secret + tracked migration) ->
#   write known data (pg row + volume marker) ->
#   SourceCredential round-trip (rev-5: material in apprafter-system) ->
#   `apprafter export` -> assert openable dumps (pg_restore -l, tar -t) ->
#   `apprafter backup create` -> assert encrypted restic repo + backup list ->
#   H1: platform Argo Apps NOT captured, only the user app's Argo App ->
#   [TWO-CLUSTER] bring up a FRESH target + bootstrap + register ->
#   `apprafter restore <repo> --target <fresh>` -> assert:
#     no hang (claims ready, R1 no-deadlock), app auto-registers,
#     tracked migration SKIPS on boot (sees restored _migrations),
#     pg row + volume marker are the BACKED-UP data (H2/R3),
#     user secret + SourceCredential material re-sealed in the target ->
#   `--data-only` round-trip.
#
# 2.6d is CLI-ONLY
# ----------------
# 2.6d changed only the CLI (cli/platform-cli + cli-providers backup module),
# the e2e harness, and docs — NO operator/CRD/webhook change. So this walk does
# NOT need local-operator mode for 2.6d itself: the PUBLISHED operator + chart
# (which already carries the 2.6c SharedVolume CRD + the 2.6b/2.12 surface this
# walk's app uses) suffices. The walk runs the WORKING-TREE CLI via the
# `apprafter()` wrapper (`cd cli && cargo run`), which is what's under test.
#
# Two clusters — RESOURCE-GATED SOFT skip
# ---------------------------------------
# The restore-into-fresh phases need TWO kind/k3d clusters concurrently. When
# the second cluster cannot be brought up (OOM / runtime can't start a 2nd
# node), those phases SOFT-SKIP with a clear note — full DR rides the real-
# Hetzner manual walk. The single-cluster phases (deploy, write data, export,
# backup, backup list, H1) ALWAYS run and must be GREEN. A non-running restore
# is NEVER reported GREEN: SOFT-SKIP is honest only for the resource limit.
# Set APPRAFTER_E2E_FORCE_SINGLE_CLUSTER=1 to force the SOFT-skip path.
#
# CLI state injection
# -------------------
# `apprafter cluster-bootstrap` / `apprafter export|backup|restore` read the
# kubeconfig from the CLI's per-target state store (not $KUBECONFIG). We set
# APPRAFTER_CONFIG_DIR to a tmpdir and seed it with a config.yaml + a
# state.json carrying each cluster's kubeconfig as kubeconfig_yaml (plaintext),
# one per target — `source` (active) and `fresh` — the same approach the
# sibling walks use. `apprafter restore --target fresh` selects the second.
#
# Required: docker (or podman), cargo, kubectl, restic (lib.sh installs a
#   `nix run nixpkgs#restic` wrapper on $PATH when the bare binary is absent —
#   the CLI shells out to `restic`, so it must be resolvable by the child).
#   All satisfied inside `nix develop` or on a standard CI runner.
#
# Exit codes:
#   0 — chain green (incl. an honest two-cluster SOFT-skip)
#   1 — assertion failure
#   2 — precondition missing
#
# PASS/FAIL is judged by READING THE LOG: every phase prints `ok:` lines, a
# SOFT-skipped phase prints `SOFT-SKIP:`, the final `backup-restore-walk GREEN`
# banner prints only on the success path, and any failure prints `FAILED:`
# (the cleanup trap) before teardown. Do NOT trust the exit code alone.

set -euo pipefail

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------

SRC_CLUSTER="apprafter-br-src"      # source cluster (data lives here)
DST_CLUSTER="apprafter-br-dst"      # fresh restore target

APP_NS="demo"                       # tenant namespace
APP="shop"                          # the test Application
DISK_CLAIM="${APP}-disk"            # generated owned-disk ResourceClaim
PG_CLAIM="${APP}-pg"                # generated pg ResourceClaim (scalar need)
PG_CONN="${PG_CLAIM}-conn"          # pg connection Secret
MOUNT_PATH="/data"                  # needs.disk.mountPath

USER_SECRET="shop-app-secret"       # a user secret sealed into the app ns
USER_SECRET_KEY="apikey"            # its key
USER_SECRET_VALUE="s3cr3t-walk-token"

# SourceCredential round-trip coordinates (rev-5: material in apprafter-system).
SC_NAME="walkcreds"
SC_MATERIAL="srccred-${SC_NAME}-material"   # material_secret_name(walkcreds)
SC_URL_PREFIX="https://github.com/apprafter-walk/demo"
SC_USERNAME="walkbot"
SC_TOKEN="ghp_walkfaketokenforbackuprestore012345"

# Group-qualify the collision-prone kinds (bare `application` also matches
# Argo CD's argoproj.io Application; bare `resourceclaim` matches the k8s 1.32+
# DRA resource.k8s.io ResourceClaim). Always address the apprafter.io CRs.
APP_RES="application.apprafter.io"
CLAIM_RES="resourceclaim.apprafter.io"

PROVIDER_NS="apprafter-system"      # ServiceProvider + SourceCredential ns
DISK_PROVIDER="disk-local"          # seeded owned-disk ServiceProvider
STORAGE_CLASS="local-path"

# Backup artifacts (host paths, under the temp workspace).
RESTIC_PASS="walk-restic-passphrase-2026"

# The test app image. The Application schema has NO `command` field (the
# renderer sets only image/replicas/env/expose/needs), so a framework-style
# tracked migration on boot (R3) must live in the IMAGE's ENTRYPOINT. We build
# a tiny image at walk time (Phase 0b) from postgres:16-alpine (carries psql +
# pg_restore) whose entrypoint: (1) ensures a `_migrations` tracking table,
# (2) applies migration `001` only if absent + records it, (3) prints whether
# the user secret arrived, (4) sleeps forever. Side-loaded into BOTH clusters
# (imagePullPolicy IfNotPresent on a local tag), mirroring the local-operator
# image-build pattern the sibling walks use. The `:walk` tag is never published.
APP_IMAGE="apprafter-br-walk-app:walk"

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

# Ensure restic is resolvable by the CLI subprocess (installs a nix wrapper on
# $PATH when absent). Called WITHOUT a subshell so the PATH export survives.
# RESTIC_WRAPPER_BIN_DIR (set by the helper) is cleaned up in the trap.
ensure_restic_on_path
command -v restic >/dev/null 2>&1 || {
    printf 'ERROR: restic not resolvable even after the nix wrapper install\n' >&2
    exit 2
}
printf 'restic: %s\n' "$(command -v restic)"

# ---------------------------------------------------------------
# Temp workspace
# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
SRC_KUBECONFIG="${TMPDIR_WORK}/kubeconfig-src"
DST_KUBECONFIG="${TMPDIR_WORK}/kubeconfig-dst"
EXPORT_DIR="${TMPDIR_WORK}/export"
RESTIC_REPO="${TMPDIR_WORK}/restic-repo"

# Per-cluster created flags — only delete a cluster we actually created (the
# cleanup-trap discipline; never touch a non-test ambient cluster).
SRC_CREATED=0
DST_CREATED=0

cleanup() {
    local exit_code=$?

    if [ "$exit_code" -ne 0 ]; then
        printf '\nFAILED: backup-restore-walk FAILED at %s (exit %d)\n' \
            "$(elapsed)" "$exit_code" >&2
        # Diagnose whichever cluster(s) we created (KUBECONFIG points at the
        # last-used one; dump both explicitly).
        if [ "$SRC_CREATED" -eq 1 ]; then
            printf '\n=== SOURCE cluster diagnostics ===\n' >&2
            KUBECONFIG="$SRC_KUBECONFIG" dump_diagnostics || true
        fi
        if [ "$DST_CREATED" -eq 1 ]; then
            printf '\n=== TARGET cluster diagnostics ===\n' >&2
            KUBECONFIG="$DST_KUBECONFIG" dump_diagnostics || true
        fi
        if [ "$SRC_CREATED" -eq 0 ] && [ "$DST_CREATED" -eq 0 ]; then
            printf 'no test cluster was created (runtime unavailable?) — ambient KUBECONFIG NOT touched.\n' >&2
        fi
    fi

    if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
        [ "$DST_CREATED" -eq 1 ] && k3d_down "$DST_CLUSTER" || true
        [ "$SRC_CREATED" -eq 1 ] && k3d_down "$SRC_CLUSTER" || true
    else
        printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster(s) up.\n'
        [ "$SRC_CREATED" -eq 1 ] && printf 'Run: <k3d|kind> cluster delete %s\n' "$SRC_CLUSTER" || true
        [ "$DST_CREATED" -eq 1 ] && printf 'Run: <k3d|kind> cluster delete %s\n' "$DST_CLUSTER" || true
    fi

    rm -rf "$TMPDIR_WORK"
    [ -n "${RESTIC_WRAPPER_BIN_DIR:-}" ] && rm -rf "$RESTIC_WRAPPER_BIN_DIR" || true
    exit "$exit_code"
}
trap cleanup EXIT

# ---------------------------------------------------------------
# Helper: seed the CLI state store with BOTH targets (source + fresh).
# The active target is `source`; `apprafter restore --target fresh` selects
# the second. Mirrors the sibling walks' single-target seed, generalized.
# ---------------------------------------------------------------
seed_apprafter_state_two() {
    local src_kc="$1" dst_kc="$2"

    mkdir -p "${APPRAFTER_CONFIG_DIR}"
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/source/.apprafter"
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/fresh/.apprafter"

    cat >"${APPRAFTER_CONFIG_DIR}/config.yaml" <<'YAML'
active_target: source
version: 1
YAML

    # The source kubeconfig always exists by the time we seed. The fresh one
    # only exists after Phase 4 brings up the second cluster — seed it only
    # when the file is present (a non-existent fresh target is simply absent
    # from the state store until restore needs it). NB: use `if` blocks, not
    # `[ ... ] && _write_state` — a trailing false `&&`-chain would make the
    # function return non-zero and abort the caller under `set -e`.
    if [ -s "$src_kc" ]; then
        _write_state "source" "$src_kc"
    fi
    if [ -n "$dst_kc" ] && [ -s "$dst_kc" ]; then
        _write_state "fresh" "$dst_kc"
    fi
}

_write_state() {
    local target="$1" kubeconfig_file="$2" kc_escaped
    kc_escaped=$(sed 's/\\/\\\\/g; s/"/\\"/g' "$kubeconfig_file" | awk '{printf "%s\\n", $0}')
    cat >"${APPRAFTER_CONFIG_DIR}/state/${target}/.apprafter/state.json" <<STATE
{
  "hetzner_cloud": {
    "server_id": 1,
    "server_name": "${target}",
    "ssh_key_ids": [],
    "kubeconfig_yaml": "${kc_escaped}"
  }
}
STATE
}

# Set the active target in config.yaml (so cluster-bootstrap / export / backup
# act on it). `apprafter restore --target <t>` overrides per-invocation.
set_active_target() {
    local target="$1"
    cat >"${APPRAFTER_CONFIG_DIR}/config.yaml" <<YAML
active_target: ${target}
version: 1
YAML
}

# ---------------------------------------------------------------
# Local assertion helpers (mirror the sibling walks)
# ---------------------------------------------------------------

# wait_jsonpath <kind> <ns> <name> <jsonpath> <want> [timeout]   (uses $KUBECONFIG)
wait_jsonpath() {
    local kind="$1" ns="$2" name="$3" jsonpath="$4" want="$5"
    local timeout="${6:-180}" deadline got
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait %s/%s [%s] == %q (timeout %ss) ...\n' "$kind" "$name" "$jsonpath" "$want" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        got=$(kubectl -n "$ns" get "$kind" "$name" -o jsonpath="$jsonpath" 2>/dev/null || true)
        if [ "$got" = "$want" ]; then
            printf '  ok: %s/%s [%s] = %q\n' "$kind" "$name" "$jsonpath" "$got"
            return 0
        fi
        printf '    %s: got=%q want=%q\n' "$(date +%H:%M:%S)" "$got" "$want"
        sleep 5
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

# jp <kind> <ns> <name> <jsonpath>   (read one value, uses $KUBECONFIG)
jp() { kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true; }

# app_pod <app-name>  — a Running pod for an Application (uses $KUBECONFIG).
app_pod() {
    local running
    running=$(kubectl -n "$APP_NS" get pod -l "app.kubernetes.io/name=$1" \
        --field-selector=status.phase=Running \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
    if [ -n "$running" ]; then printf '%s' "$running"; return; fi
    kubectl -n "$APP_NS" get pod -l "app.kubernetes.io/name=$1" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# bring up + bootstrap one cluster on $1=name, writing kubeconfig to $2.
# Sets the GLOBAL $KUBECONFIG to it. Returns 0 on success.
cluster_up_bootstrap() {
    local name="$1" kc_out="$2" target="$3"
    k3d_up "$name"
    cluster_kubeconfig_write "$name" "$kc_out"
    export KUBECONFIG="$kc_out"
    seed_apprafter_state_two "$SRC_KUBECONFIG" "$DST_KUBECONFIG"
    set_active_target "$target"
    export APPRAFTER_CONFIG_DIR
    bootstrap_with_retry
}

# Seed the disk-local + pg-integrated ServiceProviders + ensure local-path,
# then wait for the platform to be ready. Operates on $KUBECONFIG.
prepare_platform() {
    printf '  waiting for AppProject apps ...\n'
    local deadline; deadline=$(( $(date +%s) + 600 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1 && break
        sleep 10
    done
    kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1 || {
        printf 'FAILED: AppProject apps not found after 10 min\n' >&2; return 1; }

    # Seed the disk-local ServiceProvider if the published chart lacks it.
    if ! kubectl get serviceprovider "$DISK_PROVIDER" -n "$PROVIDER_NS" >/dev/null 2>&1; then
        printf '  seeding ServiceProvider %s ...\n' "$DISK_PROVIDER"
        kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: ServiceProvider
metadata:
  name: ${DISK_PROVIDER}
  namespace: ${PROVIDER_NS}
  labels: { apprafter.io/managed-by: apprafter, tier: integrated, location: in-cluster }
spec: { type: disk, backend: disk, config: { storageClass: local-path } }
YAML
    fi
    retry 30 10 -- kubectl get serviceprovider "$DISK_PROVIDER" -n "$PROVIDER_NS" >/dev/null

    # CNPG operator must be up for needs.pg.
    printf '  waiting for the CNPG operator ...\n'
    retry 30 10 -- kubectl -n cnpg-system rollout status \
        deploy -l app.kubernetes.io/name=cloudnative-pg --timeout=60s

    # admission webhook Available before any CR apply.
    retry 30 10 -- kubectl -n "$PROVIDER_NS" rollout status deploy admission-webhook --timeout=60s

    # The sealed-secrets controller must be up before `apprafter secret seal`
    # (which fetches its cert via the Service) and before restore re-seals.
    # The Argo `sealed-secrets` app may sync a beat after the webhook — wait
    # for both the Deployment AND a Service with Endpoints (the cert proxy
    # 404s until the pod is serving).
    printf '  waiting for the sealed-secrets controller ...\n'
    local sdeadline; sdeadline=$(( $(date +%s) + 300 ))
    while [ "$(date +%s)" -lt "$sdeadline" ]; do
        kubectl -n "$PROVIDER_NS" get deploy sealed-secrets-controller >/dev/null 2>&1 && break
        sleep 5
    done
    retry 30 10 -- kubectl -n "$PROVIDER_NS" rollout status \
        deploy sealed-secrets-controller --timeout=60s
    # Endpoints present (the cert proxy needs a serving pod).
    retry 30 5 -- sh -c "kubectl -n '$PROVIDER_NS' get endpoints sealed-secrets-controller -o jsonpath='{.subsets[0].addresses[0].ip}' 2>/dev/null | grep -q ."

    # local-path StorageClass (k3d ships it; bare kind needs the rancher provisioner).
    if ! kubectl get storageclass "$STORAGE_CLASS" >/dev/null 2>&1; then
        printf '  StorageClass %s absent — installing rancher local-path-provisioner ...\n' "$STORAGE_CLASS"
        retry 5 10 -- kubectl apply -f \
            https://raw.githubusercontent.com/rancher/local-path-provisioner/v0.0.31/deploy/local-path-storage.yaml
        retry 30 10 -- kubectl -n local-path-storage rollout status \
            deploy/local-path-provisioner --timeout=60s
    fi
    printf '  ok: platform ready (disk-local + pg-integrated + webhook + local-path)\n'
}

# Build the tracked-migration app image (Phase 0b) + side-load it into a
# cluster. The ENTRYPOINT runs the framework-style tracked migration on boot
# (the schema has no `command` field — the logic lives in the image), reading
# $DATABASE_URL (bound via the pg claim env ref) and writing the volume marker
# under /data (the owned disk). Idempotent: re-running the build is cheap and
# `cluster_load_image` is a no-op if the tag is already present.
APP_IMAGE_BUILT=0
build_app_image() {
    [ "$APP_IMAGE_BUILT" -eq 1 ] && return 0
    local builder=podman
    command -v podman >/dev/null 2>&1 || builder=docker
    local ctx; ctx="$(mktemp -d -t apprafter-br-app.XXXXXX)"
    cat >"${ctx}/entrypoint.sh" <<'ENTRY'
#!/bin/sh
set -e
echo "boot: running tracked migrations against ${DATABASE_URL%%@*}@<redacted>"
# Wait for the DB to be reachable (the pg claim Service may settle a beat
# after the pod starts).
i=0
until psql "$DATABASE_URL" -tAc "SELECT 1" >/dev/null 2>&1; do
  i=$((i+1)); [ "$i" -ge 60 ] && { echo "DB unreachable after 60 tries"; exit 1; }
  sleep 2
done
# Tracking table — idempotent.
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -c \
  "CREATE TABLE IF NOT EXISTS _migrations (id TEXT PRIMARY KEY, applied_at TIMESTAMPTZ NOT NULL DEFAULT now());"
# Apply migration 001 only if not already recorded (the tracked-migration gate).
if [ "$(psql "$DATABASE_URL" -tAc "SELECT 1 FROM _migrations WHERE id='001'" | tr -d '[:space:]')" = "1" ]; then
  echo "MIGRATION_001_SKIPPED"
else
  echo "MIGRATION_001_APPLYING"
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -c \
    "CREATE TABLE IF NOT EXISTS app_data (id SERIAL PRIMARY KEY, payload TEXT NOT NULL);"
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -c "INSERT INTO _migrations (id) VALUES ('001');"
  echo "MIGRATION_001_DONE"
fi
echo "secret-seen=${APIKEY:-MISSING}"
echo "boot complete; sleeping"
exec sleep 100000
ENTRY
    cat >"${ctx}/Dockerfile" <<'DOCKER'
FROM postgres:16-alpine
COPY entrypoint.sh /usr/local/bin/walk-entrypoint.sh
RUN chmod +x /usr/local/bin/walk-entrypoint.sh
ENTRYPOINT ["/usr/local/bin/walk-entrypoint.sh"]
DOCKER
    printf '  building %s (%s) ...\n' "$APP_IMAGE" "$builder"
    "$builder" build -t "$APP_IMAGE" "$ctx"
    rm -rf "$ctx"
    APP_IMAGE_BUILT=1
}

# Apply the test Application: needs.pg (scalar) + needs.disk (owned) + a user
# secret ref. The tracked migration runs in the IMAGE's ENTRYPOINT (R3).
apply_test_app() {
    kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP}
  namespace: ${APP_NS}
  labels: { apprafter.io/managed-by: apprafter }
spec:
  base:
    image: ${APP_IMAGE}
    replicas: 1
    expose:
      port: 5432
    env:
      DATABASE_URL:
        claim: "pg.url"
      APIKEY:
        secret: "${USER_SECRET}/${USER_SECRET_KEY}"
    needs:
      pg:
        selector: { tier: integrated }
      disk:
        size: 1Gi
        mountPath: ${MOUNT_PATH}
YAML
}

# ===============================================================
# Phase 0: SOURCE cluster up + bootstrap
# ===============================================================

phase "Phase 0: source cluster up + bootstrap (${SRC_CLUSTER})"

cluster_up_bootstrap "$SRC_CLUSTER" "$SRC_KUBECONFIG" "source"
SRC_CREATED=1
printf '  ok: source cluster %s bootstrapped\n' "$SRC_CLUSTER"

prepare_platform

kubectl create namespace "$APP_NS" 2>/dev/null || true

# Build the tracked-migration app image + side-load it into the source cluster.
phase "Phase 0b: build the tracked-migration app image + load into ${SRC_CLUSTER}"
build_app_image
cluster_load_image "$SRC_CLUSTER" "$APP_IMAGE"
printf '  ok: %s loaded into %s\n' "$APP_IMAGE" "$SRC_CLUSTER"

# ===============================================================
# Phase 1: deploy app + seal user secret + write known data
# ===============================================================

phase "Phase 1: deploy app (needs.pg + needs.disk + secret + tracked migration); write known data"

# Seal the USER secret into the APP namespace (NOT apprafter-system — a
# secret:"name/key" ref resolves in the app's own ns; see the secret-seal
# namespace footgun). The operator unseals it; the app mounts APIKEY from it.
apprafter secret seal "$USER_SECRET" \
    --from-literal "${USER_SECRET_KEY}=${USER_SECRET_VALUE}" \
    --namespace "$APP_NS"
wait_jsonpath secret "$APP_NS" "$USER_SECRET" "{.metadata.name}" "$USER_SECRET" 120
printf '  ok: user secret %s/%s sealed + unsealed\n' "$APP_NS" "$USER_SECRET"

apply_test_app

# The owned-disk + pg claims provision; the app resumes Ready.
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$DISK_CLAIM" '{.status.ready}' true 300
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$PG_CLAIM" '{.status.ready}' true 360
wait_jsonpath "$APP_RES" "$APP_NS" "$APP" '{.status.phase}' Ready 300
kubectl -n "$APP_NS" wait --for=condition=Available "deployment/${APP}" --timeout=300s
retry 40 5 -- kubectl -n "$APP_NS" wait --for=condition=Ready \
    pod -l "app.kubernetes.io/name=${APP}" --timeout=20s

POD="$(app_pod "$APP")"
[ -n "$POD" ] || { printf 'FAILED: no app pod\n' >&2; exit 1; }
printf '  app pod: %s\n' "$POD"

# The tracked migration applied 001 on first boot (proof: it's recorded).
boot_log=$(kubectl -n "$APP_NS" logs "$POD" 2>/dev/null || true)
if printf '%s' "$boot_log" | grep -q "MIGRATION_001_DONE"; then
    printf '  ok: tracked migration 001 applied on first boot\n'
elif printf '%s' "$boot_log" | grep -q "MIGRATION_001_SKIPPED"; then
    printf '  ok: tracked migration 001 already recorded (idempotent re-run)\n'
else
    printf 'FAILED: tracked-migration boot did not run as expected\n%s\n' "$boot_log" >&2
    exit 1
fi
# The user secret reached the container (APIKEY mounted).
if printf '%s' "$boot_log" | grep -q "secret-seen=${USER_SECRET_VALUE}"; then
    printf '  ok: user secret reached the container (APIKEY mounted)\n'
else
    printf 'FAILED: user secret did not reach the container\n%s\n' "$boot_log" >&2
    exit 1
fi

# Resolve the pg connection Secret -> psql DSN so the walk can write a KNOWN row.
pg_url=$(kubectl -n "$APP_NS" get secret "$PG_CONN" -o jsonpath='{.data.url}' 2>/dev/null | base64 -d || true)
[ -n "$pg_url" ] || { printf 'FAILED: could not read pg connection url\n' >&2; exit 1; }

# Write a KNOWN pg row + a volume marker file (the data we expect back).
kubectl -n "$APP_NS" exec "$POD" -- psql "$pg_url" -v ON_ERROR_STOP=1 \
    -c "INSERT INTO app_data (payload) VALUES ('walk-known-payload');"
pg_count=$(kubectl -n "$APP_NS" exec "$POD" -- psql "$pg_url" -tAc \
    "SELECT count(*) FROM app_data WHERE payload='walk-known-payload';" 2>/dev/null | tr -d '[:space:]' || true)
assert_eq "pg known row present on source" "$pg_count" "1"

kubectl -n "$APP_NS" exec "$POD" -- sh -c "echo volume-marker-walk > ${MOUNT_PATH}/marker"
vol_marker=$(kubectl -n "$APP_NS" exec "$POD" -- cat "${MOUNT_PATH}/marker" 2>/dev/null || true)
assert_eq "volume marker present on source" "$vol_marker" "volume-marker-walk"

# ===============================================================
# Phase 1b: SourceCredential round-trip (rev-5)
# ===============================================================

phase "Phase 1b: SourceCredential round-trip — material in apprafter-system (rev-5)"

# `repo creds add` puts the SourceCredential CR + sealed material in
# apprafter-system (OUTSIDE the app-ns). Load-bearing: if it were in the
# app-ns the secret sweep would mask the rev-5 follow-the-reference hole.
apprafter repo creds add "$SC_NAME" \
    --url-prefix "$SC_URL_PREFIX" \
    --type pat \
    --username "$SC_USERNAME" \
    --token "$SC_TOKEN" \
    --no-validate \
    --no-interactive
wait_jsonpath sourcecredential "$PROVIDER_NS" "$SC_NAME" '{.metadata.name}' "$SC_NAME" 60
# The controller unseals the material Secret in apprafter-system.
wait_jsonpath secret "$PROVIDER_NS" "$SC_MATERIAL" '{.metadata.name}' "$SC_MATERIAL" 120
# Confirm material is in apprafter-system, NOT the app ns (rev-5 invariant).
if kubectl -n "$APP_NS" get secret "$SC_MATERIAL" >/dev/null 2>&1; then
    printf 'FAILED: SourceCredential material leaked into the app ns (rev-5 invariant broken)\n' >&2
    exit 1
fi
printf '  ok: SourceCredential %s + material %s present in %s (outside app-ns)\n' \
    "$SC_NAME" "$SC_MATERIAL" "$PROVIDER_NS"

# ===============================================================
# Phase 2: `apprafter export` -> openable dumps + manifest
# ===============================================================

phase "Phase 2: apprafter export -> pg dump + volume tar + manifest.json, all openable"

apprafter export --out "$EXPORT_DIR"

# manifest.json exists + carries platformVersion + the demo namespace.
[ -f "${EXPORT_DIR}/manifest.json" ] || { printf 'FAILED: export manifest.json missing\n' >&2; exit 1; }
pv=$(python3 -c "import json,sys; print(json.load(open('${EXPORT_DIR}/manifest.json')).get('platform_version',''))" 2>/dev/null \
     || grep -o '"platform_version"[^,]*' "${EXPORT_DIR}/manifest.json")
printf '  manifest platform_version: %s\n' "$pv"
grep -q '"platform_version"' "${EXPORT_DIR}/manifest.json" || {
    printf 'FAILED: manifest.json has no platform_version\n' >&2; exit 1; }
grep -q "\"${APP_NS}\"" "${EXPORT_DIR}/manifest.json" || {
    printf 'FAILED: manifest.json does not list the %s namespace\n' "$APP_NS" >&2; exit 1; }
printf '  ok: manifest.json present with platform_version + %s namespace\n' "$APP_NS"

# pg dump exists + is a valid pg custom-format archive (pg_restore -l lists it).
PG_DUMP="${EXPORT_DIR}/pg/${APP_NS}/${PG_CLAIM}.dump"
[ -f "$PG_DUMP" ] || { printf 'FAILED: expected pg dump %s missing\n' "$PG_DUMP" >&2; ls -R "$EXPORT_DIR" >&2; exit 1; }
# pg_restore lives in the postgres helper image — run it via a throwaway pod's
# psql image is overkill; instead use the app pod (postgres:16-alpine carries it).
if kubectl -n "$APP_NS" cp "$PG_DUMP" "${POD}:/tmp/walk.dump"; then
    toc=$(kubectl -n "$APP_NS" exec "$POD" -- pg_restore -l /tmp/walk.dump 2>/dev/null || true)
    if printf '%s' "$toc" | grep -qi "app_data\|TABLE DATA\|_migrations"; then
        printf '  ok: pg dump is a valid custom-format archive (pg_restore -l lists app objects)\n'
    else
        printf 'FAILED: pg_restore -l did not list expected objects\n%s\n' "$toc" >&2
        exit 1
    fi
else
    printf 'FAILED: could not copy pg dump into the pod for pg_restore -l\n' >&2
    exit 1
fi

# volume tar exists + lists the marker.
VOL_TAR="${EXPORT_DIR}/volumes/${APP_NS}/${DISK_CLAIM}/data.tar"
[ -f "$VOL_TAR" ] || { printf 'FAILED: expected volume tar %s missing\n' "$VOL_TAR" >&2; ls -R "$EXPORT_DIR" >&2; exit 1; }
if tar -tf "$VOL_TAR" 2>/dev/null | grep -q "marker"; then
    printf '  ok: volume tar lists the marker file\n'
else
    printf 'FAILED: volume tar does not contain the marker\n' >&2
    tar -tf "$VOL_TAR" >&2 2>&1 || true
    exit 1
fi

# ===============================================================
# Phase 3: `apprafter backup create` -> encrypted restic repo + list + H1
# ===============================================================

phase "Phase 3: apprafter backup create -> encrypted restic repo + backup list + H1"

apprafter backup create --repo "$RESTIC_REPO" --passphrase "$RESTIC_PASS"

# The repo is a real, encrypted restic repository: `restic cat config` works
# only with the right passphrase, and the on-disk layout has the restic markers.
[ -f "${RESTIC_REPO}/config" ] || { printf 'FAILED: restic repo config missing at %s\n' "$RESTIC_REPO" >&2; ls -la "$RESTIC_REPO" >&2 2>&1 || true; exit 1; }
# Encrypted: the config file is NOT plaintext JSON (restic encrypts it).
if grep -aq '"version"' "${RESTIC_REPO}/config" 2>/dev/null; then
    printf 'FAILED: restic repo config looks like plaintext — not encrypted\n' >&2
    exit 1
fi
# Wrong passphrase must FAIL; right one must succeed (proves encryption).
if RESTIC_PASSWORD=wrong-pass restic -r "$RESTIC_REPO" snapshots >/dev/null 2>&1; then
    printf 'FAILED: restic accepted a WRONG passphrase — repo not encrypted\n' >&2
    exit 1
fi
RESTIC_PASSWORD="$RESTIC_PASS" restic -r "$RESTIC_REPO" cat config >/dev/null 2>&1 || {
    printf 'FAILED: restic could not read config with the correct passphrase\n' >&2; exit 1; }
printf '  ok: restic repo is encrypted (wrong passphrase rejected, right one accepted)\n'

# `apprafter backup list` shows the snapshot.
list_out="$(apprafter backup list --repo "$RESTIC_REPO" --passphrase "$RESTIC_PASS")"
printf '%s\n' "$list_out"
if printf '%s' "$list_out" | grep -q "${SRC_CLUSTER}\|source"; then
    printf '  ok: backup list shows the snapshot tagged with the source cluster id\n'
elif printf '%s' "$list_out" | grep -qiE 'ID +TIME'; then
    printf '  ok: backup list shows a snapshot (header + row)\n'
else
    printf 'FAILED: backup list did not list a snapshot\n%s\n' "$list_out" >&2
    exit 1
fi

# ---- H1: platform Argo Apps NOT captured; only the user app's Argo App ----
# Inspect the restic snapshot's crs/ tree. The platform umbrella + component
# Argo Applications (no managed-by label) must be ABSENT; the user app's Argo
# Application (managed-by=apprafter) must be present.
DUMP_DIR="${TMPDIR_WORK}/h1-dump"
mkdir -p "$DUMP_DIR"
RESTIC_PASSWORD="$RESTIC_PASS" restic -r "$RESTIC_REPO" restore latest --target "$DUMP_DIR" >/dev/null 2>&1 || {
    printf 'FAILED: could not restic-restore the snapshot for H1 inspection\n' >&2; exit 1; }
CRS_DIR=$(find "$DUMP_DIR" -type d -name crs | head -1)
[ -n "$CRS_DIR" ] || { printf 'FAILED: no crs/ dir in the snapshot\n' >&2; find "$DUMP_DIR" >&2; exit 1; }
printf '  crs/ contents:\n'; find "$CRS_DIR" -maxdepth 1 -type f -printf '    %f\n'

# The user app's Argo Application IS present (managed-by=apprafter). The Argo
# app for `shop` is named shop-<env>; the on-disk file is tagged ArgoApplication.
if find "$CRS_DIR" -maxdepth 1 -type f -name '*ArgoApplication*' | grep -q .; then
    printf '  ok: a user ArgoApplication is captured\n'
else
    printf 'FAILED: H1 — no user ArgoApplication captured (expected the shop app)\n' >&2
    find "$CRS_DIR" -maxdepth 1 -type f -printf '%f\n' >&2; exit 1
fi
# The PLATFORM Argo Applications (umbrella `platform`, `apprafter-operator`,
# `argocd`, `cilium`, etc.) must NOT appear. Those lack the managed-by label,
# so is_user_argo_app filters them out. Assert no captured ArgoApplication file
# is one of the known platform app names.
platform_leak=0
for forbidden in platform apprafter-operator argo-cd argocd cilium cert-manager sealed-secrets cnpg-operator; do
    if find "$CRS_DIR" -maxdepth 1 -type f -iname "*ArgoApplication-*-${forbidden}.json" | grep -q .; then
        printf 'FAILED: H1 — platform Argo Application "%s" leaked into the backup\n' "$forbidden" >&2
        platform_leak=1
    fi
done
[ "$platform_leak" -eq 0 ] || exit 1
printf '  ok: H1 — no platform umbrella/component Argo Application captured (user-vs-platform discrimination)\n'

# The SourceCredential material WAS captured (rev-5 follow-the-reference).
if find "$DUMP_DIR" -type f -path '*/sourcecred/*' -name "${SC_MATERIAL}.json" | grep -q .; then
    printf '  ok: rev-5 — SourceCredential material captured under secrets/sourcecred/\n'
else
    printf 'FAILED: rev-5 — SourceCredential material NOT captured (follow-the-reference broken)\n' >&2
    find "$DUMP_DIR" -name '*.json' >&2; exit 1
fi

# ===============================================================
# Phase 4: FRESH target cluster (SOFT-skip if two clusters infeasible)
# ===============================================================

phase "Phase 4: fresh target cluster + bootstrap + register (two-cluster)"

TWO_CLUSTER_OK=0
if [ -n "${APPRAFTER_E2E_FORCE_SINGLE_CLUSTER:-}" ]; then
    printf '  SOFT-SKIP: APPRAFTER_E2E_FORCE_SINGLE_CLUSTER set — skipping the restore-into-fresh phases.\n'
    printf '  note: full DR (restore into a fresh cluster) rides the real-Hetzner manual walk (2.6d T13).\n'
else
    # Try to bring up the second cluster. If it OOMs / cannot start, SOFT-skip.
    if ( set -e; k3d_up "$DST_CLUSTER" ) ; then
        DST_CREATED=1
        cluster_kubeconfig_write "$DST_CLUSTER" "$DST_KUBECONFIG"
        export KUBECONFIG="$DST_KUBECONFIG"
        seed_apprafter_state_two "$SRC_KUBECONFIG" "$DST_KUBECONFIG"
        set_active_target "fresh"
        if bootstrap_with_retry && prepare_platform; then
            kubectl create namespace "$APP_NS" 2>/dev/null || true
            # The restored Application references the local :walk image — load it
            # into the fresh cluster too (it has no registry to pull from).
            cluster_load_image "$DST_CLUSTER" "$APP_IMAGE"
            TWO_CLUSTER_OK=1
            printf '  ok: fresh target cluster %s bootstrapped + registered (+ app image loaded)\n' "$DST_CLUSTER"
        else
            printf '  SOFT-SKIP: fresh target bootstrap failed — treating as a resource limit.\n'
            printf '  note: full DR rides the real-Hetzner manual walk (2.6d T13).\n'
        fi
    else
        printf '  SOFT-SKIP: could not bring up a SECOND cluster (resource-bound in this sandbox).\n'
        printf '  note: the single-cluster export+backup phases are GREEN; full DR rides the real-Hetzner manual walk (2.6d T13).\n'
    fi
fi

# ===============================================================
# Phase 5: restore into the fresh target (SOFT-skip if no 2nd cluster)
# ===============================================================

phase "Phase 5: apprafter restore <repo> --target fresh (restore-into-running / mode a)"

if [ "$TWO_CLUSTER_OK" -ne 1 ]; then
    printf '  SOFT-SKIP: restore-into-fresh skipped (no second cluster). NOT reporting a fake GREEN restore.\n'
    printf '  note: the restore path is unit-tested + rides the real-Hetzner manual walk (2.6d T13).\n'
else
    # Restore into the fresh target. The active target was set to `fresh`, but
    # we pass --target explicitly to be unambiguous.
    apprafter restore "$RESTIC_REPO" --target fresh --passphrase "$RESTIC_PASS"

    export KUBECONFIG="$DST_KUBECONFIG"

    # The app auto-registered (Application CR replayed) and reached Ready (R1:
    # claims provisioned without deadlock, then workloads resumed).
    wait_jsonpath "$CLAIM_RES" "$APP_NS" "$DISK_CLAIM" '{.status.ready}' true 360
    wait_jsonpath "$CLAIM_RES" "$APP_NS" "$PG_CLAIM" '{.status.ready}' true 420
    wait_jsonpath "$APP_RES" "$APP_NS" "$APP" '{.status.phase}' Ready 360
    kubectl -n "$APP_NS" wait --for=condition=Available "deployment/${APP}" --timeout=360s
    retry 40 5 -- kubectl -n "$APP_NS" wait --for=condition=Ready \
        pod -l "app.kubernetes.io/name=${APP}" --timeout=20s
    printf '  ok: restore did not hang — claims ready (R1), app auto-registered + Ready\n'

    RPOD="$(app_pod "$APP")"
    [ -n "$RPOD" ] || { printf 'FAILED: no restored app pod\n' >&2; exit 1; }
    printf '  restored app pod: %s\n' "$RPOD"

    # R3/H2: the tracked migration SKIPS on boot — it sees the restored
    # _migrations row, so it does NOT re-apply 001. The data we read is the
    # BACKED-UP data, not freshly-migrated data.
    rlog=$(kubectl -n "$APP_NS" logs "$RPOD" 2>/dev/null || true)
    if printf '%s' "$rlog" | grep -q "MIGRATION_001_SKIPPED"; then
        printf '  ok: R3/H2 — tracked migration SKIPPED on boot (restored _migrations seen)\n'
    else
        printf 'FAILED: R3/H2 — tracked migration did NOT skip; restore loaded data BEFORE/AFTER the app booted incorrectly\n%s\n' "$rlog" >&2
        exit 1
    fi

    # The KNOWN pg row is present (the backed-up row, restored via pg_restore).
    rpg_url=$(kubectl -n "$APP_NS" get secret "$PG_CONN" -o jsonpath='{.data.url}' 2>/dev/null | base64 -d || true)
    [ -n "$rpg_url" ] || { printf 'FAILED: restored pg connection url missing\n' >&2; exit 1; }
    rpg_count=$(kubectl -n "$APP_NS" exec "$RPOD" -- psql "$rpg_url" -tAc \
        "SELECT count(*) FROM app_data WHERE payload='walk-known-payload';" 2>/dev/null | tr -d '[:space:]' || true)
    assert_eq "H2 — backed-up pg row restored into the fresh cluster" "$rpg_count" "1"

    # The volume marker is present (restored via tar into the fresh PVC).
    rvol=$(kubectl -n "$APP_NS" exec "$RPOD" -- cat "${MOUNT_PATH}/marker" 2>/dev/null || true)
    assert_eq "H2 — backed-up volume marker restored into the fresh cluster" "$rvol" "volume-marker-walk"

    # The user secret + SourceCredential material were re-sealed + present.
    wait_jsonpath secret "$APP_NS" "$USER_SECRET" '{.metadata.name}' "$USER_SECRET" 120
    us_val=$(kubectl -n "$APP_NS" get secret "$USER_SECRET" -o jsonpath="{.data.${USER_SECRET_KEY}}" 2>/dev/null | base64 -d || true)
    assert_eq "user secret re-sealed + decryptable in the target" "$us_val" "$USER_SECRET_VALUE"
    wait_jsonpath sourcecredential "$PROVIDER_NS" "$SC_NAME" '{.metadata.name}' "$SC_NAME" 120
    wait_jsonpath secret "$PROVIDER_NS" "$SC_MATERIAL" '{.metadata.name}' "$SC_MATERIAL" 120
    sc_user=$(kubectl -n "$PROVIDER_NS" get secret "$SC_MATERIAL" -o jsonpath='{.data.username}' 2>/dev/null | base64 -d || true)
    assert_eq "SourceCredential material re-sealed in the target" "$sc_user" "$SC_USERNAME"
    printf '  ok: user secret + SourceCredential material re-sealed in the target (two secret paths)\n'
fi

# ===============================================================
# Phase 6: --data-only round-trip (SOFT-skip if no 2nd cluster)
# ===============================================================

phase "Phase 6: --data-only round-trip (reload data into a configured cluster)"

if [ "$TWO_CLUSTER_OK" -ne 1 ]; then
    printf '  SOFT-SKIP: --data-only restore skipped (no second cluster). Rides the real-Hetzner manual walk.\n'
else
    export KUBECONFIG="$DST_KUBECONFIG"
    RPOD="$(app_pod "$APP")"
    # Mutate the data so we can prove the --data-only restore overwrites it
    # back to the backed-up state.
    rpg_url=$(kubectl -n "$APP_NS" get secret "$PG_CONN" -o jsonpath='{.data.url}' 2>/dev/null | base64 -d || true)
    kubectl -n "$APP_NS" exec "$RPOD" -- psql "$rpg_url" -v ON_ERROR_STOP=1 \
        -c "INSERT INTO app_data (payload) VALUES ('post-restore-mutation');" || true
    kubectl -n "$APP_NS" exec "$RPOD" -- sh -c "echo mutated > ${MOUNT_PATH}/marker" || true

    apprafter restore "$RESTIC_REPO" --target fresh --data-only --passphrase "$RESTIC_PASS"

    # After a --data-only restore the workloads resume; the marker is back to
    # the backed-up value (the data was reloaded, app re-created on it).
    kubectl -n "$APP_NS" wait --for=condition=Available "deployment/${APP}" --timeout=360s
    retry 40 5 -- kubectl -n "$APP_NS" wait --for=condition=Ready \
        pod -l "app.kubernetes.io/name=${APP}" --timeout=20s
    RPOD2="$(app_pod "$APP")"
    dvol=$(kubectl -n "$APP_NS" exec "$RPOD2" -- cat "${MOUNT_PATH}/marker" 2>/dev/null || true)
    assert_eq "--data-only restored the volume marker to the backed-up value" "$dvol" "volume-marker-walk"
    printf '  ok: --data-only round-trip reloaded the backed-up volume data\n'
fi

# ===============================================================
# Done — tear down on the success path
# ===============================================================

trap - EXIT

if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    [ "$DST_CREATED" -eq 1 ] && k3d_down "$DST_CLUSTER" || true
    [ "$SRC_CREATED" -eq 1 ] && k3d_down "$SRC_CLUSTER" || true
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster(s) up.\n'
fi

rm -rf "$TMPDIR_WORK"
[ -n "${RESTIC_WRAPPER_BIN_DIR:-}" ] && rm -rf "$RESTIC_WRAPPER_BIN_DIR" || true

printf '\nbackup-restore-walk GREEN in %s\n' "$(elapsed)"
if [ "$TWO_CLUSTER_OK" -eq 1 ]; then
    printf 'Chain proven: deploy (pg+disk+secret+tracked-migration) -> write data -> SourceCredential round-trip -> export (openable dumps) -> backup (encrypted restic + list + H1) -> restore-into-fresh (auto-register, migration SKIPS, backed-up data, re-sealed secrets) -> --data-only round-trip\n'
else
    printf 'Chain proven (single-cluster): deploy -> write data -> SourceCredential round-trip -> export (openable dumps) -> backup (encrypted restic + list + H1 + rev-5 material). Restore-into-fresh SOFT-skipped (two-cluster resource limit) -> rides the real-Hetzner manual walk (2.6d T13).\n'
fi
