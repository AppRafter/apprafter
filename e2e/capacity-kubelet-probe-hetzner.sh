#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# 2.6c capacity live-diagnostic on a REAL Hetzner k3s node.
#
# The real-Hetzner DR walk (backup-restore-hetzner.sh Phase 9) found the kubelet
# Summary API returned node-level cpu/psi but NO `node.fs` — the exact field the
# operator's capacity signal reads (`resourceclaim-provisioner/src/capacity.rs`
# → `summary.pointer("/node/fs/capacityBytes")`). This probe provisions ONE real
# node and answers, with evidence:
#   1. Is `/node/fs` genuinely absent from the kubelet Summary API on real k3s?
#   2. Is it absent ALWAYS, or a boot-time lag (poll over ~8 min)?
#   3. What node-level fs-ish data IS present (imageFs? node.status.capacity /
#      allocatable ephemeral-storage — the always-present fallback candidate)?
#
# Single cluster, single token (OLD). Teardown-safe: a trap ALWAYS destroys the
# cluster and API-verifies zero servers (LEAK WARNING otherwise).
#
# Env (same contract as the DR walk):
#   HCLOUD_TOKEN_OLD                 — Hetzner token (project). REQUIRED.
#   APPRAFTER_DR_SSH_PUBLIC_KEY_PATH — ssh public key path. REQUIRED.
#   APPRAFTER_DR_REGION              — region (default nbg1).
#   APPRAFTER_HETZNER_SKIP_DESTROY=1 — keep the cluster up (debug; destroy by hand).
#
# Exit: 0 = probe ran + reported (regardless of the finding); 2 = env missing
# (checked BEFORE the destroy trap is armed).

set -euo pipefail

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

CAP_TARGET="dr-cap"
REGION="${APPRAFTER_DR_REGION:-nbg1}"

# ---------------------------------------------------------------
# Hetzner server count (API-verify teardown) — copied from the DR walk.
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

# ---------------------------------------------------------------
# Preconditions — BEFORE the trap is armed (exit 2, never destroys).
# ---------------------------------------------------------------
: "${HCLOUD_TOKEN_OLD:?set HCLOUD_TOKEN_OLD (exit 2 before any provision)}"
: "${APPRAFTER_DR_SSH_PUBLIC_KEY_PATH:?set APPRAFTER_DR_SSH_PUBLIC_KEY_PATH (exit 2)}"
[ -r "$APPRAFTER_DR_SSH_PUBLIC_KEY_PATH" ] || { printf 'ssh key not readable: %s\n' "$APPRAFTER_DR_SSH_PUBLIC_KEY_PATH" >&2; exit 2; }
for t in kubectl curl python3; do command -v "$t" >/dev/null 2>&1 || { printf 'missing tool: %s\n' "$t" >&2; exit 2; }; done

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"; mkdir -p "$APPRAFTER_CONFIG_DIR"
export APPRAFTER_CONFIG_DIR
KC_FILE="${TMPDIR_WORK}/kubeconfig"
CAP_CREATED=0

cleanup() {
    local exit_code=$?
    if [ -z "${APPRAFTER_HETZNER_SKIP_DESTROY:-}" ]; then
        if [ "$CAP_CREATED" -eq 1 ]; then
            printf '\n=== destroying capacity-probe cluster (%s) ===\n' "$CAP_TARGET" >&2
            apprafter destroy --yes --target "$CAP_TARGET" || \
                printf 'WARN: destroy --target %s returned non-zero\n' "$CAP_TARGET" >&2
            local n; n=$(hetzner_server_count "$HCLOUD_TOKEN_OLD")
            if [ "$n" = "0" ]; then
                printf 'ok: project has ZERO servers after destroy (API-verified)\n' >&2
            else
                printf 'LEAK WARNING: project reports %s server(s) after destroy — INSPECT https://console.hetzner.cloud AND DELETE STRAGGLERS BY HAND\n' "$n" >&2
            fi
        fi
        rm -rf "$TMPDIR_WORK"
    else
        printf '\nAPPRAFTER_HETZNER_SKIP_DESTROY set — leaving the cluster UP.\n' >&2
        [ "$CAP_CREATED" -eq 1 ] && printf 'Destroy by hand: APPRAFTER_CONFIG_DIR=%s apprafter destroy --yes --target %s\n' "$APPRAFTER_CONFIG_DIR" "$CAP_TARGET" >&2
    fi
    exit "$exit_code"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# ---------------------------------------------------------------
# Provision ONE cluster + bootstrap (faithful to what the operator runs on).
# ---------------------------------------------------------------
phase "Provision + bootstrap ONE real Hetzner node ($CAP_TARGET, region $REGION)"
{ set +x; } 2>/dev/null
apprafter target add "$CAP_TARGET" \
    --provider hetzner-cloud --tier solo --region "$REGION" \
    --server-type "${APPRAFTER_E2E_SERVER_TYPE:-cpx22}" \
    --token "$HCLOUD_TOKEN_OLD" --ssh-key "$APPRAFTER_DR_SSH_PUBLIC_KEY_PATH" \
    --no-interactive --no-ping --force
CAP_CREATED=1
apprafter target use "$CAP_TARGET"
apprafter up --target "$CAP_TARGET"
apprafter kubeconfig --target "$CAP_TARGET" >"$KC_FILE"
export KUBECONFIG="$KC_FILE"

NODE=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
[ -n "$NODE" ] || { printf 'FAILED: no node name\n' >&2; exit 1; }
printf '  ok: node = %s (k3s %s)\n' "$NODE" "$(kubectl get node "$NODE" -o jsonpath='{.status.nodeInfo.kubeletVersion}' 2>/dev/null)"

# ---------------------------------------------------------------
# The DIAGNOSTIC: kubelet Summary node.fs presence over time + node.status.
# ---------------------------------------------------------------
# Python extractor: given the raw Summary JSON on stdin, print what fs-ish data
# is present at the node level. Uses `python3 -c` (code as an ARG) so stdin
# stays free for the piped JSON — a heredoc `python3 - <<PY` would hijack stdin
# and SIGPIPE the feeding printf. Emits a machine line `FS_PRESENT=yes|no`.
summary_verdict() {
    python3 -c '
import sys, json
try:
    s = json.load(sys.stdin)
except Exception as e:
    print(f"PARSE_ERROR: {e}"); print("FS_PRESENT=no"); sys.exit(0)
node = s.get("node", {})
keys = ",".join(sorted(node.keys()))
fs = node.get("fs")
imgfs = (node.get("runtime") or {}).get("imageFs")
def cap(d):
    if not isinstance(d, dict): return "absent"
    a, c = d.get("availableBytes"), d.get("capacityBytes")
    if a is None and c is None: return "present-but-no-bytes"
    return f"avail={a} cap={c}"
print(f"node-keys=[{keys}]")
print(f"node.fs               -> {cap(fs)}")
print(f"node.runtime.imageFs  -> {cap(imgfs)}")
op = None
if isinstance(fs, dict) and isinstance(fs.get("availableBytes"), (int,float)) and isinstance(fs.get("capacityBytes"), (int,float)) and fs["capacityBytes"] > 0:
    op = fs["availableBytes"] / fs["capacityBytes"]
frac = "None (capacity signal INERT)" if op is None else round(op, 4)
print(f"operator node_free_fraction(/node/fs) -> {frac}")
print("FS_PRESENT=" + ("yes" if op is not None else "no"))
'
}
# smoke-test the extractor locally with a synthetic summary so a quoting/heredoc
# bug can NEVER again cost a live provision (this is the class of bug that just
# burned one). Runs at script start; aborts before provisioning if broken.
_summary_verdict_selftest() {
    local out
    out=$(printf '%s' '{"node":{"fs":{"availableBytes":30,"capacityBytes":100},"cpu":{}}}' | summary_verdict)
    case "$out" in
        *"FS_PRESENT=yes"*) : ;;
        *) printf 'SELFTEST FAILED: summary_verdict did not parse a known-good summary:\n%s\n' "$out" >&2; exit 2 ;;
    esac
    out=$(printf '%s' '{"node":{"cpu":{}}}' | summary_verdict)
    case "$out" in
        *"FS_PRESENT=no"*) : ;;
        *) printf 'SELFTEST FAILED: summary_verdict did not flag missing fs:\n%s\n' "$out" >&2; exit 2 ;;
    esac
    printf '  ok: summary_verdict self-test passed (parses fs-present + fs-absent)\n'
}

_summary_verdict_selftest
FOUND_FS=0
for i in 1 2 3 4 5; do
    printf '\n--- kubelet Summary probe #%d (node up ~%dm) ---\n' "$i" "$(( (i-1)*2 ))"
    raw=$(kubectl get --raw "/api/v1/nodes/${NODE}/proxy/stats/summary" 2>/dev/null || true)
    if [ -z "$raw" ]; then
        printf '  kubelet Summary fetch returned EMPTY (proxy/kubelet not answering)\n'
    else
        verdict=$(printf '%s' "$raw" | summary_verdict)
        printf '%s\n' "$verdict" | sed 's/^/  /'
        case "$verdict" in *FS_PRESENT=yes*) FOUND_FS=1 ;; esac
    fi
    # node.status — the always-present fallback candidate.
    printf '  node.status.capacity.ephemeral-storage   = %s\n' \
        "$(kubectl get node "$NODE" -o jsonpath='{.status.capacity.ephemeral-storage}' 2>/dev/null)"
    printf '  node.status.allocatable.ephemeral-storage= %s\n' \
        "$(kubectl get node "$NODE" -o jsonpath='{.status.allocatable.ephemeral-storage}' 2>/dev/null)"
    [ "$i" -lt 5 ] && sleep 120
done

# One raw dump of the node{} subtree for the record (first 1200 chars).
printf '\n--- raw node{} subtree (first 1200 chars) ---\n'
kubectl get --raw "/api/v1/nodes/${NODE}/proxy/stats/summary" 2>/dev/null \
    | python3 -c 'import sys,json; print(json.dumps(json.load(sys.stdin).get("node",{}),indent=1)[:1200])' 2>/dev/null || true

printf '\n=== VERDICT ===\n'
if [ "$FOUND_FS" -eq 1 ]; then
    printf 'node.fs APPEARED at least once → capacity signal works on real k3s (was a boot-time LAG).\n'
else
    printf 'node.fs NEVER appeared across ~8 min → capacity signal is INERT on real k3s; a fallback to\n'
    printf 'node.status.(capacity|allocatable).ephemeral-storage is the fix candidate.\n'
fi
printf 'DONE\n'
