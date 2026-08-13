#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# 2.6d backup-UX bugfixes — real-Hetzner + real-S3 walk (branch fix/backup-ux).
# Validates the NEW backup flow end-to-end against a live cluster + a real
# S3-compatible bucket:
#   - `apprafter secret seal` REFUSES to silently replace (warn/`--yes`, #4);
#   - `apprafter backup enable` is ONE-INPUT (`--credential-file` auto-seals +
#     probes + patches — no separate `secret seal`, #3);
#   - `--endpoint <host> --bucket <name>` builds the `s3:https://…` repo URL
#     for you (no hand-assembly);
#   - the credential is NEUTRAL `S3_*` (translated to restic AWS_* under the
#     hood, #2). The enable PROBE (`restic cat config → init`) hits the REAL S3,
#     so a green probe proves creds + URL + translation all work end-to-end.
#
# COST-BOUNDED + TEARDOWN-SAFE: one small server, `trap … EXIT` destroys it +
# API-verifies the project is back to zero; the restic repo in S3 is best-effort
# pruned.
#
# Required env (the caller exports these; the S3 creds come from backup-test.env):
#   HCLOUD_TOKEN            — Hetzner project token (baseline empty).
#   S3_ACCESS_KEY_ID        — S3 access key.
#   S3_SECRET_ACCESS_KEY    — S3 secret key.
#   RESTIC_PASSWORD         — restic repo passphrase.
#   APPRAFTER_S3_ENDPOINT   — e.g. nbg1.your-objectstorage.com
#   APPRAFTER_S3_BUCKET     — bucket name, e.g. ar-test
# Optional: APPRAFTER_S3_REGION, APPRAFTER_SSH_PUBLIC_KEY_PATH (default id_ed25519).
#
# Exit 0 = GREEN (all legs ok + project=0). Non-zero = a leg FAILED (grep FAILED)
# or cleanup could not reach zero. Judge by READING THE LOG.

set -euo pipefail
# shellcheck source=e2e/lib.sh
source "$(cd "$(dirname "$0")" && pwd)/lib.sh"

require_env HCLOUD_TOKEN S3_ACCESS_KEY_ID S3_SECRET_ACCESS_KEY RESTIC_PASSWORD \
    APPRAFTER_S3_ENDPOINT APPRAFTER_S3_BUCKET

ENDPOINT="$APPRAFTER_S3_ENDPOINT"
BUCKET="$APPRAFTER_S3_BUCKET"
REGION="${APPRAFTER_S3_REGION:-}"
SSH_PUB="${APPRAFTER_SSH_PUBLIC_KEY_PATH:-$HOME/.ssh/id_ed25519.pub}"
SKU="${APPRAFTER_E2E_SERVER_TYPE:-cpx22}"
PROV_REGION="${APPRAFTER_E2E_REGION:-hel1}"
TARGET="bux-e2e"
REPO="s3:https://${ENDPOINT}/${BUCKET}"           # the URL enable will construct
HZ="python3 ${REPO_ROOT}/e2e/hz.py"

[ -r "$SSH_PUB" ] || { printf 'ERROR: SSH pub key not readable at %s\n' "$SSH_PUB" >&2; exit 2; }
ensure_restic_on_path
command -v restic >/dev/null 2>&1 || { printf 'ERROR: restic unavailable\n' >&2; exit 2; }

# Isolated CLI state (never touch the operator's real targets).
WORK="$(mktemp -d -t bux-e2e.XXXXXX)"
export APPRAFTER_CONFIG_DIR="${WORK}/config"
export APPRAFTER_AGE_KEY="${WORK}/age.key"
mkdir -p "$APPRAFTER_CONFIG_DIR"

# The operator-side dotenv --credential-file consumes: NEUTRAL S3_* keys.
DOTENV="${WORK}/s3.env"
( umask 077
  printf 'S3_ACCESS_KEY_ID=%s\n'     "$S3_ACCESS_KEY_ID"
  printf 'S3_SECRET_ACCESS_KEY=%s\n' "$S3_SECRET_ACCESS_KEY"
  printf 'RESTIC_PASSWORD=%s\n'      "$RESTIC_PASSWORD"
  if [ -n "$REGION" ]; then printf 'S3_REGION=%s\n' "$REGION"; fi
) > "$DOTENV"

FAILED=0
fail() { printf '  FAILED: %s\n' "$1" >&2; FAILED=1; }
ok()   { printf '  ok: %s\n' "$1"; }

cleanup() {
    local rc=$?
    phase "cleanup: destroy cluster + sweep project + prune S3 repo"
    dump_diagnostics 2>/dev/null || true
    apprafter destroy --yes 2>/dev/null || true
    $HZ sweep "$HCLOUD_TOKEN" || true
    # best-effort prune the restic repo we init'd in S3 (test data only)
    AWS_ACCESS_KEY_ID="$S3_ACCESS_KEY_ID" AWS_SECRET_ACCESS_KEY="$S3_SECRET_ACCESS_KEY" \
    RESTIC_PASSWORD="$RESTIC_PASSWORD" \
        restic -r "$REPO" forget --keep-last 0 --prune >/dev/null 2>&1 || \
        printf '  note: S3 restic repo %s left as-is (manual prune if desired)\n' "$REPO" >&2
    if $HZ verify "$HCLOUD_TOKEN"; then ok "project swept to zero"; else printf '  FAILED: project NOT empty after sweep\n' >&2; rc=1; fi
    rm -rf "$WORK"
    if [ "$FAILED" -ne 0 ] || [ "$rc" -ne 0 ]; then
        printf '\n===== backup-ux walk: RED =====\n' >&2; exit 1
    fi
    printf '\n===== backup-ux walk: GREEN =====\n'; exit 0
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

phase "pre-flight: project must start empty"
$HZ verify "$HCLOUD_TOKEN" || { printf 'ERROR: test project not empty — refusing\n' >&2; exit 2; }

phase "provision + bootstrap the cluster (${SKU} @ ${PROV_REGION}) — ~6-9 min"
apprafter target add "$TARGET" \
    --provider hetzner-cloud --token "$HCLOUD_TOKEN" \
    --tier solo --region "$PROV_REGION" --server-type "$SKU" --ssh-key "$SSH_PUB" \
    --no-interactive
apprafter up --target "$TARGET"
ok "cluster provisioned + platform bootstrapped"
# KUBECONFIG for the raw-kubectl assertions (`apprafter kubeconfig` prints the
# cached YAML to stdout; the active target is $TARGET after `target add`).
KUBE="${WORK}/kubeconfig"
apprafter kubeconfig > "$KUBE" || true
export KUBECONFIG="$KUBE"
if kubectl get nodes >/dev/null 2>&1; then
    ok "kubeconfig usable (kubectl reaches the apiserver)"
    KUBE_OK=1
else
    KUBE_OK=0
    printf '  note: raw kubectl unusable — CRD asserts fall back to apprafter backup status\n' >&2
    printf '  --- kubeconfig head ---\n'; head -5 "$KUBE" | sed 's/^/    | /' >&2
fi

# ---------------------------------------------------------------------------
phase "Leg S1: secret seal REFUSES silent replace (#4)"
if apprafter secret seal bux-warn --from-literal K1=v1 --namespace apprafter-system >/dev/null; then ok "Leg S1: first seal ok"; else fail "Leg S1: first seal failed"; fi
# re-seal same name, non-interactive, NO --yes → must ERROR (not silently replace)
if apprafter secret seal bux-warn --from-literal K2=v2 --namespace apprafter-system 2> "${WORK}/s1.err"; then
    fail "Leg S1: re-seal SUCCEEDED without --yes (expected refuse)"
else
    if grep -qiE "already exists|--yes|replace" "${WORK}/s1.err"; then
        ok "Leg S1: re-seal refused without --yes (warns replace-not-merge)"
    else
        fail "Leg S1: re-seal failed but not with the overwrite guard:"; sed 's/^/    | /' "${WORK}/s1.err" >&2
    fi
fi
# with --yes → replaces
if apprafter secret seal bux-warn --from-literal K2=v2 --yes --namespace apprafter-system >/dev/null; then ok "Leg S1: --yes replaces"; else fail "Leg S1: --yes re-seal failed"; fi
apprafter secret remove bux-warn --yes --namespace apprafter-system >/dev/null 2>&1 || true

# ---------------------------------------------------------------------------
phase "Leg E: backup enable ONE-INPUT — builds URL, auto-seals, probes REAL S3 (#1/#2/#3)"
# ONE command: --endpoint + bare --bucket + --credential-file (neutral S3_ dotenv).
# No separate `secret seal`. The probe hits the real S3 → proves creds+URL+xlate.
if apprafter backup enable \
        --endpoint "$ENDPOINT" --bucket "$BUCKET" \
        --credential-file "$DOTENV" --i-have-saved-credentials \
        2> "${WORK}/enable.err"; then
    ok "Leg E: backup enable succeeded (real-S3 probe passed → creds+URL+translation OK)"
else
    fail "Leg E: backup enable FAILED:"; sed 's/^/    | /' "${WORK}/enable.err" >&2
fi
# assert the CRD stored the CONSTRUCTED full URL — via `apprafter backup status`
# (the CLI's own kubeconfig resolution, robust) rather than raw kubectl.
apprafter backup status > "${WORK}/status.out" 2>&1 || true
if grep -qF "$REPO" "${WORK}/status.out"; then
    ok "Leg E: backup status shows bucket ${REPO} (CRD patched + URL constructed)"
else
    fail "Leg E: backup status does not show the constructed bucket:"; sed 's/^/    | /' "${WORK}/status.out" >&2
fi
# credentialRef + NEUTRAL S3_ keys (not AWS_) — via raw kubectl when usable.
# PlatformStack is Namespaced and the singleton lives in apprafter-system, so
# every raw read MUST be `-n apprafter-system` (and never abort under set -e).
if [ "${KUBE_OK:-0}" = "1" ]; then
    CRED_NAME="$(kubectl -n apprafter-system get platformstack default -o jsonpath='{.spec.backup.credentialRef.name}' 2>/dev/null || true)"
    if [ -n "$CRED_NAME" ]; then ok "Leg E: credentialRef.name = ${CRED_NAME}"; else fail "Leg E: no credentialRef.name (raw kubectl)"; fi
    # the sealed-secrets controller unseals into a plain Secret of that name
    KEYS="$(kubectl -n apprafter-system get secret "$CRED_NAME" -o jsonpath='{.data}' 2>/dev/null || true)"
    if printf '%s' "$KEYS" | grep -q "S3_ACCESS_KEY_ID" && ! printf '%s' "$KEYS" | grep -q "AWS_ACCESS_KEY_ID"; then
        ok "Leg E: auto-sealed Secret holds NEUTRAL S3_ keys (no AWS_)"
    else
        fail "Leg E: sealed Secret keys wrong (want S3_*, not AWS_*): ${KEYS}"
    fi
else
    printf '  note: skipping raw-kubectl secret-key assert (kubeconfig unusable); enable-success + unit tests cover the S3_ key naming\n' >&2
fi

# ---------------------------------------------------------------------------
phase "Leg L: restic repo initialised in S3 — snapshots list cleanly"
# The enable probe ran `restic init`; confirm the repo is a valid restic repo in S3.
if AWS_ACCESS_KEY_ID="$S3_ACCESS_KEY_ID" AWS_SECRET_ACCESS_KEY="$S3_SECRET_ACCESS_KEY" \
   RESTIC_PASSWORD="$RESTIC_PASSWORD" \
   restic -r "$REPO" snapshots >/dev/null 2> "${WORK}/snap.err"; then
    ok "Leg L: restic repo in S3 is valid + listable (init by enable worked)"
else
    fail "Leg L: restic repo not usable in S3:"; sed 's/^/    | /' "${WORK}/snap.err" >&2
fi

phase "all legs done — cleanup runs on exit"
