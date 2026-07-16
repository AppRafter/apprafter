#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter backup/restore DISASTER-RECOVERY walk — REAL Hetzner, TWO clusters,
# the REAL landing CMS app (2.6d T13-adjacent live e2e).
#
# This is the real-hardware counterpart to `e2e/backup-restore-walk.sh` (the
# kind/k3d single-runtime walk that SOFT-skips the two-cluster restore under
# resource pressure). Here we provision TWO genuinely-separate Hetzner clusters
# in TWO separate Hetzner PROJECTS (two tokens), deploy the actual
# `landing/cms/apprafter/Application.cue` (Payload + Next, `needs.pg` + a user
# `secret:` ref), seed known data, `apprafter export` + `apprafter backup`,
# then `apprafter restore --target <new>` (restore-into-running, mode b — the
# SHIPPED path; NOT `--reprovision`/T13) into the SECOND cluster, and prove the
# two clusters are ~identical (pg data + re-sealed secret). It also does the
# capacity live-read the kind walk SOFT-skips (kubelet Summary API on a real
# node), and tears BOTH clusters down with an API-verified zero-server check.
#
#   provision OLD (token1, dr-old) -> bootstrap -> platform Ready ->
#   seal FAKE PAYLOAD_SECRET into ns apprafter ->
#   app add the CMS (public image + public repo, --env dev, cluster-internal) ->
#   CMS Ready (needs.pg claim bound + Payload prodMigrations on boot) ->
#   seed a deterministic marker row into the CMS pg ->
#   `apprafter export`  -> assert openable pg dump + manifest.json ->
#   `apprafter backup`  -> encrypted restic repo + `backup list` ->
#   provision NEW (token2, dr-new) -> bootstrap ->
#   `apprafter restore <repo> --target dr-new` (mode b, restore-into-running) ->
#   CMS auto-registers, needs.pg re-provisioned + data loaded, secret re-sealed ->
#   verify: CMS Ready (NOT EnvSecretMissing), marker row present + MATCHES OLD ->
#   `apprafter export` on NEW -> equivalence (query-result compare, NOT raw dump) ->
#   capacity live-read (kubelet /stats/summary on a real node) ->
#   TEARDOWN: destroy BOTH + API-verify zero servers per project (LEAK WARNING).
#
# ============================ SAFETY (READ THIS) ============================
# This walk provisions REAL, PAID Hetzner infrastructure and drives TWO live
# API tokens. Four invariants keep it safe:
#
#   1. TWO tokens, TWO projects. HCLOUD_TOKEN_OLD + HCLOUD_TOKEN_NEW are read
#      from the ENV (never hard-coded, never echoed — not even masked). Each is
#      stored into its OWN target's credentials.yaml via `target add --token`.
#      The bare `HCLOUD_TOKEN` env var is DELIBERATELY UNSET for the whole run:
#      the CLI's credential resolver puts `HCLOUD_TOKEN` ABOVE the per-target
#      stored token (cli-core/credentials.rs §7), so exporting it would make
#      BOTH targets resolve to the SAME project — a cross-project leak. We keep
#      it unset so each `--target` resolves to its own stored token.
#
#   2. ISOLATION from the operator's real config. APPRAFTER_CONFIG_DIR is a
#      fresh mktemp -d, so the DR targets NEVER touch the user's real
#      ~/.config/apprafter (they run a real prod cluster). Two distinct target
#      names — `dr-old` and `dr-new` — each bound to its own token/project.
#
#   3. TEARDOWN-SAFE (the load-bearing requirement). A `trap ... EXIT` destroys
#      BOTH clusters regardless of pass/fail, THEN verifies via the Hetzner
#      Cloud API (GET /v1/servers with each token) that each project has ZERO
#      servers left, printing a loud `LEAK WARNING` otherwise. Per-cluster
#      OLD_CREATED / NEW_CREATED guards (mirroring K3D_CREATED in the kind
#      walks) mean the trap only destroys what was actually created.
#      APPRAFTER_HETZNER_SKIP_DESTROY=1 is an explicit opt-out (default = always
#      destroy).
#
#   4. COST-BOUNDED. Small server type (cpx22-class shared vCPU, the mvp.sh
#      default), single node each, short-lived. A cost-awareness note prints at
#      start. Two clusters up for ~20-40 min ≈ single-digit cents.
#
# Required env:
#   HCLOUD_TOKEN_OLD             — Hetzner Cloud API token for the OLD project
#   HCLOUD_TOKEN_NEW             — Hetzner Cloud API token for the NEW project
#   APPRAFTER_DR_SSH_PUBLIC_KEY_PATH — path to an SSH public key file used to
#                                   provision both nodes (same key, both
#                                   projects — passed as `target add --ssh-key`)
#
# Optional env:
#   APPRAFTER_HETZNER_SKIP_DESTROY=1 — keep BOTH clusters up at the end (debug).
#                                   You then MUST destroy them by hand (the trap
#                                   prints the exact commands).
#   APPRAFTER_DR_REGION=nbg1     — Hetzner region for both clusters; default nbg1
#   APPRAFTER_DR_FAKE_SECRET=…   — the fake PAYLOAD_SECRET value; default is a
#                                   deterministic in-script literal.
#
# Exit codes:
#   0 — DR chain green
#   1 — assertion failure (the trap tears both clusters down first)
#   2 — env precondition missing (checked BEFORE arming the destroy trap)
#
# PASS/FAIL is judged by READING THE LOG: every phase prints `ok:` lines, the
# final `backup-restore-hetzner GREEN` banner prints only on the success path,
# and any failure prints `FAILED:` before teardown. Do NOT trust the exit code
# alone — and the teardown ALWAYS runs, so a leaked server surfaces as a loud
# `LEAK WARNING` in the trap output even on the happy path.

set -euo pipefail

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------

OLD_TARGET="dr-old"                 # OLD cluster target (token1 / project1)
NEW_TARGET="dr-new"                 # NEW cluster target (token2 / project2)

# The REAL landing CMS. We deploy it VERBATIM from the public repo path via the
# `dev` environment (cluster-internal, NO public expose / real domain), and
# reach it through kubectl port-forward for seeding + verify.
CMS_REPO="https://github.com/AppRafter/apprafter"
CMS_PATH="landing/cms/apprafter"    # holds Application.cue (metadata.name: landing-cms)
CMS_APP="landing-cms"               # metadata.name
CMS_NS="apprafter"                  # metadata.namespace
CMS_ENV="dev"                       # cluster-internal environment (no real domain)
CMS_PG_CLAIM="${CMS_APP}-pg"        # generated needs.pg ResourceClaim
CMS_PG_CONN="${CMS_APP}-pg-conn"    # pg connection Secret (app ns), key `url`

# The user SealedSecret the CMS manifest references via
# `secret:"apprafter-landing-cms-secrets/PAYLOAD_SECRET"` (2.12 env value ref).
# It MUST be sealed into the APP namespace (metadata.namespace = apprafter), NOT
# apprafter-system, or the operator surfaces EnvSecretMissing (the secret-seal
# namespace footgun). We seal a FAKE value — the DR test never needs the real
# production PAYLOAD_SECRET.
CMS_SECRET_NAME="apprafter-landing-cms-secrets"
CMS_SECRET_KEY="PAYLOAD_SECRET"
CMS_SECRET_VALUE="${APPRAFTER_DR_FAKE_SECRET:-dr-walk-fake-payload-secret-0123456789abcdef}"

# The deterministic known data: a marker table + row we write into the CMS's pg
# BEFORE export and read back on the restored cluster. A dedicated table (not a
# Payload-owned one) keeps the equivalence anchor independent of Payload's
# migration schema — it survives pg_dump/pg_restore and is the compare key.
DR_MARKER_TABLE="dr_walk_marker"
DR_MARKER_VALUE="dr-known-payload-$(date +%Y%m%d)"

# Group-qualify the collision-prone apprafter.io kinds (bare `application` also
# matches Argo CD's argoproj.io Application; bare `resourceclaim` matches the
# k8s DRA resource.k8s.io ResourceClaim).
APP_RES="application.apprafter.io"
CLAIM_RES="resourceclaim.apprafter.io"

PROVIDER_NS="apprafter-system"

# Backup artifacts (host paths, under the temp workspace).
RESTIC_PASS="dr-walk-restic-passphrase-2026"

# A throwaway psql helper image (carries psql + pg_restore). We only use it as a
# short-lived pod to seed/verify pg over the connection Secret DSN — the CMS
# image is Next/Payload and ships no psql. Public, anon-pullable.
PSQL_IMAGE="postgres:18-alpine"

REGION="${APPRAFTER_DR_REGION:-nbg1}"

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------

for tool in cargo kubectl curl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done

# restic must be resolvable by the CLI subprocess (Command::new("restic")).
# lib.sh installs a `nix run nixpkgs#restic` wrapper on $PATH when absent.
ensure_restic_on_path
command -v restic >/dev/null 2>&1 || {
    printf 'ERROR: restic not resolvable even after the nix wrapper install\n' >&2
    exit 2
}
printf 'restic: %s\n' "$(command -v restic)"

# ---------------------------------------------------------------
# Preconditions — BEFORE the trap is armed so a missing token exits cleanly
# with code 2 (never triggers a destroy on a cluster that was never made).
# NB: we require the TWO project tokens + the ssh key path. We DELIBERATELY do
# NOT require (and will UNSET) the bare HCLOUD_TOKEN — see the SAFETY block.
# ---------------------------------------------------------------

require_env HCLOUD_TOKEN_OLD HCLOUD_TOKEN_NEW APPRAFTER_DR_SSH_PUBLIC_KEY_PATH

if [ ! -r "$APPRAFTER_DR_SSH_PUBLIC_KEY_PATH" ]; then
    printf 'ERROR: SSH public key not readable at %s\n' "$APPRAFTER_DR_SSH_PUBLIC_KEY_PATH" >&2
    exit 2
fi

# CRITICAL isolation: never let a stray HCLOUD_TOKEN in the caller's env
# override the per-target stored token (that would point BOTH targets at ONE
# project). Unset it for the whole run so every `--target` resolves to its own
# credentials.yaml. Also unset APPRAFTER_SSH_PUBLIC_KEY (the inline-body env)
# so the per-target `--ssh-key` path is the single source of truth.
unset HCLOUD_TOKEN || true
unset APPRAFTER_SSH_PUBLIC_KEY || true

# ---------------------------------------------------------------
# Temp workspace + fresh CONFIG DIR (isolates from ~/.config/apprafter)
# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
mkdir -p "$APPRAFTER_CONFIG_DIR"
export APPRAFTER_CONFIG_DIR

EXPORT_DIR_OLD="${TMPDIR_WORK}/export-old"
EXPORT_DIR_NEW="${TMPDIR_WORK}/export-new"
RESTIC_REPO="${TMPDIR_WORK}/restic-repo"
KC_FILE="${TMPDIR_WORK}/kubeconfig"     # scratch kubeconfig for kubectl

# Per-cluster created flags — the cleanup trap only destroys what we made.
OLD_CREATED=0
NEW_CREATED=0

# ---------------------------------------------------------------
# Hetzner API helper: count servers in a project (by token).
#   hetzner_server_count <token>  — echoes the number of servers, or `?` on a
#   failed API call. Token is used ONLY as a bearer header; `set +x` guards it
#   so it never lands in an xtrace log, and it is never printed.
# ---------------------------------------------------------------
hetzner_server_count() {
    local token="$1" body count
    { set +x; } 2>/dev/null
    body=$(curl -fsS -H "Authorization: Bearer ${token}" \
        "https://api.hetzner.cloud/v1/servers" 2>/dev/null || true)
    if [ -z "$body" ]; then
        printf '?'
        return 0
    fi
    # Prefer jq; fall back to a grep count of `"id":` under the servers array.
    if command -v jq >/dev/null 2>&1; then
        count=$(printf '%s' "$body" | jq -r '.servers | length' 2>/dev/null || printf '?')
    else
        count=$(printf '%s' "$body" | grep -o '"status"' | wc -l | tr -d '[:space:]')
    fi
    printf '%s' "${count:-?}"
}

# ---------------------------------------------------------------
# Cleanup trap — DESTROY both clusters (regardless of pass/fail) then
# API-verify ZERO servers per project. This is the load-bearing safety net.
# ---------------------------------------------------------------
cleanup() {
    local exit_code=$?

    if [ "$exit_code" -ne 0 ]; then
        printf '\nFAILED: backup-restore-hetzner FAILED at %s (exit %d)\n' \
            "$(elapsed)" "$exit_code" >&2
        # Best-effort diagnostics on whichever cluster(s) we created.
        if [ "$OLD_CREATED" -eq 1 ]; then
            printf '\n=== OLD cluster diagnostics (%s) ===\n' "$OLD_TARGET" >&2
            if apprafter kubeconfig --target "$OLD_TARGET" >"$KC_FILE" 2>/dev/null; then
                KUBECONFIG="$KC_FILE" dump_diagnostics || true
            fi
        fi
        if [ "$NEW_CREATED" -eq 1 ]; then
            printf '\n=== NEW cluster diagnostics (%s) ===\n' "$NEW_TARGET" >&2
            if apprafter kubeconfig --target "$NEW_TARGET" >"$KC_FILE" 2>/dev/null; then
                KUBECONFIG="$KC_FILE" dump_diagnostics || true
            fi
        fi
    fi

    if [ -z "${APPRAFTER_HETZNER_SKIP_DESTROY:-}" ]; then
        # Destroy each cluster we created via its OWN target (per-target token).
        if [ "$OLD_CREATED" -eq 1 ]; then
            printf '\n=== destroying OLD cluster (%s) ===\n' "$OLD_TARGET" >&2
            apprafter destroy --yes --target "$OLD_TARGET" || \
                printf 'WARN: `apprafter destroy --target %s` returned non-zero\n' "$OLD_TARGET" >&2
        fi
        if [ "$NEW_CREATED" -eq 1 ]; then
            printf '\n=== destroying NEW cluster (%s) ===\n' "$NEW_TARGET" >&2
            apprafter destroy --yes --target "$NEW_TARGET" || \
                printf 'WARN: `apprafter destroy --target %s` returned non-zero\n' "$NEW_TARGET" >&2
        fi

        # API-verify ZERO servers per project. A non-zero (or unknown) count is
        # a LEAK WARNING — the operator MUST inspect the project by hand.
        if [ "$OLD_CREATED" -eq 1 ]; then
            local old_n; old_n=$(hetzner_server_count "$HCLOUD_TOKEN_OLD")
            if [ "$old_n" = "0" ]; then
                printf 'ok: OLD project has ZERO servers after destroy (API-verified)\n' >&2
            else
                printf 'LEAK WARNING: OLD project reports %s server(s) after destroy — INSPECT https://console.hetzner.cloud (token1 project) AND DELETE ANY STRAGGLERS BY HAND\n' \
                    "$old_n" >&2
            fi
        fi
        if [ "$NEW_CREATED" -eq 1 ]; then
            local new_n; new_n=$(hetzner_server_count "$HCLOUD_TOKEN_NEW")
            if [ "$new_n" = "0" ]; then
                printf 'ok: NEW project has ZERO servers after destroy (API-verified)\n' >&2
            else
                printf 'LEAK WARNING: NEW project reports %s server(s) after destroy — INSPECT https://console.hetzner.cloud (token2 project) AND DELETE ANY STRAGGLERS BY HAND\n' \
                    "$new_n" >&2
            fi
        fi
    else
        printf '\nAPPRAFTER_HETZNER_SKIP_DESTROY set — leaving BOTH clusters UP.\n' >&2
        [ "$OLD_CREATED" -eq 1 ] && printf 'Destroy OLD by hand: APPRAFTER_CONFIG_DIR=%s apprafter destroy --yes --target %s\n' "$APPRAFTER_CONFIG_DIR" "$OLD_TARGET" >&2
        [ "$NEW_CREATED" -eq 1 ] && printf 'Destroy NEW by hand: APPRAFTER_CONFIG_DIR=%s apprafter destroy --yes --target %s\n' "$APPRAFTER_CONFIG_DIR" "$NEW_TARGET" >&2
        # Do NOT rm the temp config dir here — the operator needs it to destroy.
        exit "$exit_code"
    fi

    rm -rf "$TMPDIR_WORK"
    [ -n "${RESTIC_WRAPPER_BIN_DIR:-}" ] && rm -rf "$RESTIC_WRAPPER_BIN_DIR" || true
    exit "$exit_code"
}
trap cleanup EXIT
# A SIGINT/SIGTERM (e.g. an outer `timeout`, or a manual Ctrl-C) must NOT bypass
# teardown — convert each into an `exit`, which runs the EXIT trap exactly once
# and so ALWAYS destroys + API-verifies both projects. Without this, a signal
# would kill the script mid-provision and leak paid servers.
trap 'exit 130' INT
trap 'exit 143' TERM

# ---------------------------------------------------------------
# Local assertion helpers (mirror the kind walks — operate on $KUBECONFIG)
# ---------------------------------------------------------------

# wait_jsonpath <kind> <ns> <name> <jsonpath> <want> [timeout]
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

# jp <kind> <ns> <name> <jsonpath>  — read one value (uses $KUBECONFIG)
jp() { kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true; }

# cms_pod  — a Running CMS pod name (uses $KUBECONFIG). The operator labels the
# workload `app.kubernetes.io/name=<inner-app-name>`; for the CMS that is the
# manifest name landing-cms.
cms_pod() {
    local running
    running=$(kubectl -n "$CMS_NS" get pod -l "app.kubernetes.io/name=$CMS_APP" \
        --field-selector=status.phase=Running \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
    if [ -n "$running" ]; then printf '%s' "$running"; return; fi
    kubectl -n "$CMS_NS" get pod -l "app.kubernetes.io/name=$CMS_APP" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# Read the pg DSN from the connection Secret (key `url`, decomposed DSN — 2.12).
cms_pg_dsn() {
    kubectl -n "$CMS_NS" get secret "$CMS_PG_CONN" \
        -o jsonpath='{.data.url}' 2>/dev/null | base64 -d 2>/dev/null || true
}

# Run a one-shot psql against the CMS pg over its DSN, via a throwaway
# postgres:18-alpine pod (the CMS image ships no psql). Echoes psql stdout.
#   psql_oneshot <dsn> <sql>
psql_oneshot() {
    local dsn="$1" sql="$2" pod
    pod="dr-psql-$(date +%s%N | tail -c 8)"
    kubectl -n "$CMS_NS" run "$pod" --rm -i --restart=Never \
        --image="$PSQL_IMAGE" --command -- \
        psql "$dsn" -v ON_ERROR_STOP=1 -tAc "$sql" 2>/dev/null || true
}

# Provision + bootstrap ONE cluster on a target that already carries a stored
# token + ssh key. Sets the active target to it, runs `up`, waits platform
# Ready, and pulls the kubeconfig into $KC_FILE / exports $KUBECONFIG.
#   provision_and_bootstrap <target>
provision_and_bootstrap() {
    local target="$1"
    # `up` (bootstrap-all) chains apply -> k3s-ready poll -> cluster-bootstrap;
    # the inner cluster-bootstrap acts on the ACTIVE target, so make it active
    # first (then also pass --target to apply's resolution for good measure).
    apprafter target use "$target"
    apprafter up --target "$target"
    # Pull the kubeconfig into a scratch file so our kubectl asserts work.
    apprafter kubeconfig --target "$target" >"$KC_FILE"
    export KUBECONFIG="$KC_FILE"
}

# Wait for the platform to be self-managing-Ready on $KUBECONFIG: the Argo CD
# platform Applications Synced + CNPG operator up (needs.pg) + sealed-secrets
# controller serving (secret seal / restore re-seal).
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

    # CNPG operator (needs.pg backend).
    printf '  waiting for the CNPG operator ...\n'
    retry 30 10 -- kubectl -n cnpg-system rollout status \
        deploy -l app.kubernetes.io/name=cloudnative-pg --timeout=60s

    # sealed-secrets controller must be serving (secret seal fetches its cert;
    # restore re-seals through it). Deployment Available + Endpoints present.
    printf '  waiting for the sealed-secrets controller ...\n'
    retry 30 10 -- kubectl -n "$PROVIDER_NS" rollout status \
        deploy sealed-secrets-controller --timeout=60s
    retry 30 5 -- sh -c "kubectl -n '$PROVIDER_NS' get endpoints sealed-secrets-controller -o jsonpath='{.subsets[0].addresses[0].ip}' 2>/dev/null | grep -q ."
    printf '  ok: platform ready (Argo apps Synced + CNPG + sealed-secrets)\n'
}

# Wait for the CMS Application to reach Ready: needs.pg claim bound, phase
# Ready, Deployment Available, a Running pod. On a real node the CMS boots
# Payload + Next and runs prodMigrations against the freshly-provisioned pg.
wait_cms_ready() {
    wait_jsonpath "$CLAIM_RES" "$CMS_NS" "$CMS_PG_CLAIM" '{.status.ready}' true 480
    wait_jsonpath "$APP_RES" "$CMS_NS" "$CMS_APP" '{.status.phase}' Ready 480
    # The CMS is deployed via --env dev, so the operator renders one Deployment.
    kubectl -n "$CMS_NS" wait --for=condition=Available "deployment/${CMS_APP}" --timeout=480s
    retry 40 8 -- kubectl -n "$CMS_NS" wait --for=condition=Ready \
        pod -l "app.kubernetes.io/name=${CMS_APP}" --timeout=20s
}

# =====================================================================
# Phase 0: setup
# =====================================================================

phase "Phase 0: setup — fresh config dir, two tokens, cost note, teardown armed"

cat <<'COST_NOTE'
  COST AWARENESS: this walk provisions TWO real Hetzner servers (one per
  project, cpx22-class shared vCPU, single node each) for the duration of the
  run (~20-40 min). Two clusters up for that long costs single-digit cents. The
  teardown trap ALWAYS destroys both and API-verifies zero servers per project;
  a leaked server prints a loud LEAK WARNING. Set APPRAFTER_HETZNER_SKIP_DESTROY=1
  ONLY to keep them alive for debugging (you then destroy by hand).
COST_NOTE
printf '  config dir (isolated from ~/.config/apprafter): %s\n' "$APPRAFTER_CONFIG_DIR"
printf '  region: %s | old target: %s | new target: %s\n' "$REGION" "$OLD_TARGET" "$NEW_TARGET"

# Register BOTH targets up front, each with its OWN token (per-target
# credentials.yaml) + the shared ssh key path. `--no-interactive` keeps it
# flag-driven; `--no-ping` avoids a token-authenticating API round-trip here
# (provisioning will authenticate anyway). Token passed via --token (NOT env)
# so the two projects stay isolated. `set +x` guards the token args.
{ set +x; } 2>/dev/null
apprafter target add "$OLD_TARGET" \
    --provider hetzner-cloud \
    --tier     solo \
    --region   "$REGION" \
    --token    "$HCLOUD_TOKEN_OLD" \
    --ssh-key  "$APPRAFTER_DR_SSH_PUBLIC_KEY_PATH" \
    --no-interactive --no-ping --force
apprafter target add "$NEW_TARGET" \
    --provider hetzner-cloud \
    --tier     solo \
    --region   "$REGION" \
    --token    "$HCLOUD_TOKEN_NEW" \
    --ssh-key  "$APPRAFTER_DR_SSH_PUBLIC_KEY_PATH" \
    --no-interactive --no-ping --force
printf '  ok: targets %s + %s registered (each with its own token, HCLOUD_TOKEN env unset)\n' \
    "$OLD_TARGET" "$NEW_TARGET"

# =====================================================================
# Phase 1: provision OLD (token1, dr-old)
# =====================================================================

phase "Phase 1: provision OLD cluster (${OLD_TARGET}, project1) + bootstrap"

provision_and_bootstrap "$OLD_TARGET"
OLD_CREATED=1
printf '  ok: OLD cluster provisioned + bootstrapped\n'

wait_platform_ready

# =====================================================================
# Phase 2: deploy the CMS on OLD + seal secret + seed known data
# =====================================================================

phase "Phase 2: deploy the REAL CMS on OLD (needs.pg + secret ref, --env dev); seed known data"

# Seal the FAKE PAYLOAD_SECRET into the APP namespace (metadata.namespace =
# apprafter). MUST be the app ns, not apprafter-system, or the operator marks
# the CMS EnvSecretMissing (the secret-seal namespace footgun). Creating the ns
# first so the seal + unseal land somewhere; Argo CreateNamespace also makes it,
# but the seal runs before the first app sync.
kubectl create namespace "$CMS_NS" 2>/dev/null || true
{ set +x; } 2>/dev/null
apprafter secret seal "$CMS_SECRET_NAME" \
    --from-literal "${CMS_SECRET_KEY}=${CMS_SECRET_VALUE}" \
    --namespace "$CMS_NS"
wait_jsonpath secret "$CMS_NS" "$CMS_SECRET_NAME" '{.metadata.name}' "$CMS_SECRET_NAME" 180
printf '  ok: FAKE PAYLOAD_SECRET sealed + unsealed into ns %s\n' "$CMS_NS"

# Register the CMS from the PUBLIC repo + path, deploying the `dev` environment
# (cluster-internal — no public expose, no real cms.apprafter.dev). Public
# image (ghcr.io/apprafter/landing-cms) is anon-pullable, so NO SourceCredential
# and NO pull secret are needed. --no-ping skips the git ls-remote check (CI
# network), --no-interactive keeps it flag-driven, --coverage-gate present (the
# default, warn-only) since the repo is public.
# `app add` reads the LOCAL `<cwd>/apprafter/Application.cue` (post-2.12 env
# validation + derived name), even with an explicit git_url. The CMS manifest
# lives at `landing/cms/apprafter/`, so run from `landing/cms` — exactly as a
# user sitting in their repo checkout would. The apprafter() wrapper forces
# cwd=cli (which breaks the `<cwd>/apprafter/Application.cue` lookup), so invoke
# the BUILT binary directly from the manifest dir. The env (APPRAFTER_CONFIG_DIR
# + the active target dr-old) is inherited by the subshell.
APPRAFTER_BIN="${REPO_ROOT}/cli/target/debug/apprafter"
[ -x "$APPRAFTER_BIN" ] || ( cd "${REPO_ROOT}/cli" && cargo build --quiet --bin apprafter )
# --branch master: `app add` with an EXPLICIT git_url defaults targetRevision to
# `main`, but the AppRafter repo's default branch is `master` — without this,
# Argo fails with `unable to resolve 'main' to a commit SHA` (ComparisonError),
# never renders the AppRafter Application CR, and no ResourceClaim is generated.
# The CMS manifest lives on origin/master (verified).
( cd "${REPO_ROOT}/landing/cms" && "$APPRAFTER_BIN" app add "$CMS_REPO" \
    --name "$CMS_APP" \
    --branch master \
    --path "$CMS_PATH" \
    --namespace "$CMS_NS" \
    --env "$CMS_ENV" \
    --no-ping \
    --no-interactive )
printf '  ok: CMS registered (public repo %s path %s, --env %s)\n' "$CMS_REPO" "$CMS_PATH" "$CMS_ENV"

wait_cms_ready
CMS_POD_OLD="$(cms_pod)"
[ -n "$CMS_POD_OLD" ] || { printf 'FAILED: no CMS pod on OLD\n' >&2; exit 1; }
printf '  ok: CMS Ready on OLD (pod %s); Payload prodMigrations ran on boot\n' "$CMS_POD_OLD"

# The CMS must NOT be EnvSecretMissing — proves the sealed PAYLOAD_SECRET reached
# the container (Ready=True already implies the env ref resolved, but assert the
# absence of the failure condition explicitly).
cms_ready_cond=$(jp "$APP_RES" "$CMS_NS" "$CMS_APP" \
    '{.status.conditions[?(@.type=="Ready")].status}')
assert_eq "CMS Ready condition on OLD (secret resolved, not EnvSecretMissing)" "$cms_ready_cond" "True"

# Seed the deterministic KNOWN data into the CMS pg (a dedicated marker table,
# independent of Payload's schema). This is the equivalence anchor we read back
# on the restored cluster.
DSN_OLD="$(cms_pg_dsn)"
[ -n "$DSN_OLD" ] || { printf 'FAILED: could not read the CMS pg connection url on OLD\n' >&2; exit 1; }
psql_oneshot "$DSN_OLD" \
    "CREATE TABLE IF NOT EXISTS ${DR_MARKER_TABLE} (id INT PRIMARY KEY, note TEXT NOT NULL);" >/dev/null
psql_oneshot "$DSN_OLD" \
    "INSERT INTO ${DR_MARKER_TABLE} (id, note) VALUES (1, '${DR_MARKER_VALUE}') ON CONFLICT (id) DO UPDATE SET note = EXCLUDED.note;" >/dev/null
old_marker=$(psql_oneshot "$DSN_OLD" "SELECT note FROM ${DR_MARKER_TABLE} WHERE id=1;" | tr -d '[:space:]')
assert_eq "known marker row present on OLD pg" "$old_marker" "$DR_MARKER_VALUE"

# =====================================================================
# Phase 3: export OLD
# =====================================================================

phase "Phase 3: apprafter export OLD -> pg dump + manifest.json (openable)"

apprafter target use "$OLD_TARGET"
apprafter export --out "$EXPORT_DIR_OLD"

# manifest.json present + carries the app namespace (BackupManifest is
# camelCase: the version key is `platformVersion`).
[ -f "${EXPORT_DIR_OLD}/manifest.json" ] || { printf 'FAILED: OLD export manifest.json missing\n' >&2; exit 1; }
grep -q '"platformVersion"' "${EXPORT_DIR_OLD}/manifest.json" || {
    printf 'FAILED: OLD manifest.json has no platformVersion\n' >&2; exit 1; }
grep -q "\"${CMS_NS}\"" "${EXPORT_DIR_OLD}/manifest.json" || {
    printf 'FAILED: OLD manifest.json does not list the %s namespace\n' "$CMS_NS" >&2; exit 1; }
printf '  ok: OLD manifest.json present with platformVersion + %s namespace\n' "$CMS_NS"

# pg dump present + a valid custom-format archive (pg_restore -l lists it). Run
# pg_restore inside a throwaway postgres:18-alpine pod (major-matches CNPG 18).
PG_DUMP_OLD="${EXPORT_DIR_OLD}/pg/${CMS_NS}/${CMS_PG_CLAIM}.dump"
[ -f "$PG_DUMP_OLD" ] || { printf 'FAILED: expected OLD pg dump %s missing\n' "$PG_DUMP_OLD" >&2; ls -R "$EXPORT_DIR_OLD" >&2; exit 1; }
toc_pod="dr-toc-$(date +%s%N | tail -c 8)"
if kubectl -n "$CMS_NS" run "$toc_pod" --restart=Never --image="$PSQL_IMAGE" \
        --command -- sleep 300 >/dev/null 2>&1 \
   && kubectl -n "$CMS_NS" wait --for=condition=Ready "pod/$toc_pod" --timeout=120s >/dev/null 2>&1 \
   && kubectl -n "$CMS_NS" cp "$PG_DUMP_OLD" "${toc_pod}:/tmp/old.dump" ; then
    toc=$(kubectl -n "$CMS_NS" exec "$toc_pod" -- pg_restore -l /tmp/old.dump 2>/dev/null || true)
    kubectl -n "$CMS_NS" delete pod "$toc_pod" --wait=false >/dev/null 2>&1 || true
    if printf '%s' "$toc" | grep -qiE "${DR_MARKER_TABLE}|TABLE DATA"; then
        printf '  ok: OLD pg dump is a valid custom-format archive (pg_restore -l lists app objects)\n'
    else
        printf 'FAILED: pg_restore -l did not list expected objects\n%s\n' "$toc" >&2
        exit 1
    fi
else
    kubectl -n "$CMS_NS" delete pod "$toc_pod" --wait=false >/dev/null 2>&1 || true
    printf 'FAILED: could not verify the OLD pg dump with pg_restore -l\n' >&2
    exit 1
fi

# =====================================================================
# Phase 4: backup OLD
# =====================================================================

phase "Phase 4: apprafter backup create OLD -> encrypted restic repo + backup list"

{ set +x; } 2>/dev/null
apprafter backup create --repo "$RESTIC_REPO" --passphrase "$RESTIC_PASS"

# A real encrypted restic repo: the config file is present + NOT plaintext, a
# wrong passphrase is rejected, the right one is accepted.
[ -f "${RESTIC_REPO}/config" ] || { printf 'FAILED: restic repo config missing at %s\n' "$RESTIC_REPO" >&2; ls -la "$RESTIC_REPO" >&2 2>&1 || true; exit 1; }
if grep -aq '"version"' "${RESTIC_REPO}/config" 2>/dev/null; then
    printf 'FAILED: restic repo config looks like plaintext — not encrypted\n' >&2
    exit 1
fi
if RESTIC_PASSWORD=dr-wrong-pass restic -r "$RESTIC_REPO" snapshots >/dev/null 2>&1; then
    printf 'FAILED: restic accepted a WRONG passphrase — repo not encrypted\n' >&2
    exit 1
fi
RESTIC_PASSWORD="$RESTIC_PASS" restic -r "$RESTIC_REPO" cat config >/dev/null 2>&1 || {
    printf 'FAILED: restic could not read config with the correct passphrase\n' >&2; exit 1; }
printf '  ok: restic repo is encrypted (wrong passphrase rejected, right one accepted)\n'

# `apprafter backup list` shows the snapshot.
list_out="$(apprafter backup list --repo "$RESTIC_REPO" --passphrase "$RESTIC_PASS")"
printf '%s\n' "$list_out"
if printf '%s' "$list_out" | grep -q "${OLD_TARGET}"; then
    printf '  ok: backup list shows a snapshot tagged with the source cluster id\n'
elif printf '%s' "$list_out" | grep -qiE 'ID +TIME'; then
    printf '  ok: backup list shows a snapshot (header + row)\n'
else
    printf 'FAILED: backup list did not list a snapshot\n%s\n' "$list_out" >&2
    exit 1
fi

# =====================================================================
# Phase 5: provision NEW (token2, dr-new)
# =====================================================================

phase "Phase 5: provision NEW cluster (${NEW_TARGET}, project2) + bootstrap"

provision_and_bootstrap "$NEW_TARGET"
NEW_CREATED=1
printf '  ok: NEW cluster provisioned + bootstrapped\n'

wait_platform_ready

# =====================================================================
# Phase 6: restore into NEW (restore-into-running / mode b)
# =====================================================================

phase "Phase 6: apprafter restore <repo> --target ${NEW_TARGET} (mode b, restore-into-running)"

# Restore the OLD backup INTO the already-bootstrapped NEW cluster. This is the
# SHIPPED restore-into-running path (NOT --reprovision/T13). The CMS
# auto-registers (Application CR replayed, gated to replicas=0), needs.pg is
# re-provisioned, the pg dump is loaded via pg_restore, and the PAYLOAD_SECRET
# is re-sealed for the NEW cluster's controller key.
{ set +x; } 2>/dev/null
apprafter restore "$RESTIC_REPO" --target "$NEW_TARGET" --passphrase "$RESTIC_PASS"

# Point kubectl at the NEW cluster for verification.
apprafter kubeconfig --target "$NEW_TARGET" >"$KC_FILE"
export KUBECONFIG="$KC_FILE"

# The app auto-registered + reached Ready (claims re-provisioned without
# deadlock, workloads resumed on the restored data).
wait_cms_ready
CMS_POD_NEW="$(cms_pod)"
[ -n "$CMS_POD_NEW" ] || { printf 'FAILED: no restored CMS pod on NEW\n' >&2; exit 1; }
printf '  ok: restore did not hang — needs.pg re-provisioned, CMS auto-registered + Ready (pod %s)\n' "$CMS_POD_NEW"

# =====================================================================
# Phase 7: verify NEW — secret re-sealed + read; marker row present
# =====================================================================

phase "Phase 7: verify NEW — PAYLOAD_SECRET re-sealed + read; known marker row present"

# The SealedSecret is present in ns apprafter on NEW + the CMS reads it (Ready,
# not EnvSecretMissing). Ready=True was asserted by wait_cms_ready; assert the
# secret object + the Ready condition explicitly.
wait_jsonpath secret "$CMS_NS" "$CMS_SECRET_NAME" '{.metadata.name}' "$CMS_SECRET_NAME" 180
new_ready_cond=$(jp "$APP_RES" "$CMS_NS" "$CMS_APP" \
    '{.status.conditions[?(@.type=="Ready")].status}')
assert_eq "CMS Ready on NEW (re-sealed PAYLOAD_SECRET resolved, not EnvSecretMissing)" \
    "$new_ready_cond" "True"
# The re-sealed secret decrypts to the SAME fake value we sealed on OLD.
new_secret_val=$(kubectl -n "$CMS_NS" get secret "$CMS_SECRET_NAME" \
    -o jsonpath="{.data.${CMS_SECRET_KEY}}" 2>/dev/null | base64 -d 2>/dev/null || true)
assert_eq "re-sealed PAYLOAD_SECRET decrypts to the OLD value on NEW" \
    "$new_secret_val" "$CMS_SECRET_VALUE"

# The KNOWN marker row is present in NEW's pg (loaded via pg_restore from the
# backup). Read it over the NEW connection Secret DSN.
DSN_NEW="$(cms_pg_dsn)"
[ -n "$DSN_NEW" ] || { printf 'FAILED: could not read the CMS pg connection url on NEW\n' >&2; exit 1; }
new_marker=$(psql_oneshot "$DSN_NEW" "SELECT note FROM ${DR_MARKER_TABLE} WHERE id=1;" | tr -d '[:space:]')
assert_eq "known marker row restored into NEW pg" "$new_marker" "$DR_MARKER_VALUE"

# =====================================================================
# Phase 8: export NEW + equivalence (query-result compare, not raw dump)
# =====================================================================

phase "Phase 8: apprafter export NEW + equivalence (query-result compare, NOT raw pg_dump bytes)"

apprafter target use "$NEW_TARGET"
apprafter export --out "$EXPORT_DIR_NEW"

# The NEW export produced a pg dump for the same claim.
PG_DUMP_NEW="${EXPORT_DIR_NEW}/pg/${CMS_NS}/${CMS_PG_CLAIM}.dump"
[ -f "$PG_DUMP_NEW" ] || { printf 'FAILED: expected NEW pg dump %s missing\n' "$PG_DUMP_NEW" >&2; ls -R "$EXPORT_DIR_NEW" >&2; exit 1; }
printf '  ok: NEW export produced a pg dump for %s/%s\n' "$CMS_NS" "$CMS_PG_CLAIM"

# EQUIVALENCE: compare the canonical QUERY RESULT between OLD and NEW, NOT the
# raw pg_dump bytes (dumps differ by oid/timestamp/order). The marker row's
# value is the canonical anchor; we already read old_marker on OLD (Phase 2) and
# new_marker on NEW (Phase 7). Assert they match — the app's data survived the
# full export -> backup -> restore chain identically across the two clusters.
assert_eq "OLD/NEW equivalence — marker query result identical across clusters" \
    "$new_marker" "$old_marker"
# Row COUNT of the marker table also matches (single row, no dupes/loss).
new_marker_count=$(psql_oneshot "$DSN_NEW" "SELECT count(*) FROM ${DR_MARKER_TABLE};" | tr -d '[:space:]')
assert_eq "OLD/NEW equivalence — marker table row count preserved" "$new_marker_count" "1"
printf '  ok: the two clusters are ~identical for the app data (query-result compare)\n'

# =====================================================================
# Phase 9: capacity live-read (kubelet Summary API on a real node)
# =====================================================================

phase "Phase 9: capacity live-read — kubelet /stats/summary on a real node (SOFT-skipped on kind)"

# On a real Hetzner node the kubelet Summary API IS reachable through the
# apiserver's nodes/proxy subresource — the very read that SOFT-skips on kind
# (kind's kubelet stats proxy is unreliable there). Assert a live
# GET /api/v1/nodes/<node>/proxy/stats/summary returns node-level capacity data.
# This is the node-capacity signal the operator's SharedVolume/capacity path
# reads; the CMS carries no SharedVolume, so we assert the underlying
# nodes/proxy stats read directly (documented as the capacity-live proxy).
NODE=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
[ -n "$NODE" ] || { printf 'FAILED: could not resolve a node name for the capacity read\n' >&2; exit 1; }
summary=$(kubectl get --raw "/api/v1/nodes/${NODE}/proxy/stats/summary" 2>/dev/null || true)
if printf '%s' "$summary" | grep -q '"nodeName"' && printf '%s' "$summary" | grep -q '"fs"'; then
    printf '  ok: kubelet Summary API reachable on node %s (nodeName + fs capacity present)\n' "$NODE"
else
    printf 'FAILED: kubelet /stats/summary did not return node capacity data on %s\n' "$NODE" >&2
    printf '  raw (first 400 chars): %s\n' "$(printf '%s' "$summary" | head -c 400)" >&2
    exit 1
fi

# =====================================================================
# Done — success path teardown runs in the trap (destroys BOTH + API-verify)
# =====================================================================

printf '\nbackup-restore-hetzner GREEN in %s\n' "$(elapsed)"
printf 'DR chain proven on REAL Hetzner (two projects, two clusters): provision OLD -> deploy the REAL CMS (needs.pg + user secret, --env dev) -> seed known data -> export (openable pg dump + manifest) -> backup (encrypted restic + list) -> provision NEW -> restore-into-running (auto-register, needs.pg re-provisioned, secret re-sealed) -> verify (marker row + secret) -> export NEW + equivalence (query-result compare) -> capacity live-read (kubelet Summary API).\n'
printf 'Teardown (destroy BOTH + API-verify zero servers per project) runs now in the EXIT trap.\n'
# The trap does the destroy + leak-verify + tmp cleanup and preserves exit 0.
