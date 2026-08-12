#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# 2.16h / 2.16h-a machine-picker + no-default REAL-HETZNER walk
# (non-interactive legs; the interactive matrix is verified manually).
#
# Exercises the shipped provisioning behaviour on a real (backup/test)
# Hetzner project. COST-BOUNDED: provisions ONE small server, reuses it
# across legs, and sweeps the project to zero on exit (trap). Uses
# `apprafter apply` (infra only, no cluster-bootstrap) — the machine-type
# assertions do not need k3s, so we skip the ~6-9 min bootstrap.
#
# Legs (non-interactive):
#   Leg 2  no server type set  → `apply` fails ServerTypeNotSelected and
#          creates ZERO resources (pre-flight fires before any CREATE).
#   Leg 1  explicit --server-type → `apply` provisions EXACTLY that SKU in
#          that region; state.json records the type as a fact.
#   Leg 3  legacy self-heal: blank the type in config+state on the running
#          box → `apply` succeeds (no error) AND backfills the live type
#          into the target (the dogfood non-regression, H8/backfill).
#   Leg 4  `target machine --server-type <other>` patches the target
#          preference without re-provisioning.
#   (DR `restore --reprovision` reproduction is inherited + unit-tested;
#    it needs a backup repo and is out of scope for this cost-bounded run.)
#
# Required env:
#   HCLOUD_TOKEN  — a BACKUP/TEST Hetzner project token (baseline: empty).
# Optional env:
#   APPRAFTER_E2E_REGION       — default hel1
#   APPRAFTER_E2E_SERVER_TYPE  — default cpx22 (a live shared-vCPU class;
#                                cx11/cpx11 were retired in early 2026)
#   APPRAFTER_E2E_SERVER_TYPE2 — default cx23 (the "other" live type for Leg 4)
#   APPRAFTER_SSH_PUBLIC_KEY_PATH — default ~/.ssh/id_ed25519.pub
#
# Exit: 0 = GREEN (all legs ok + project swept to zero). Non-zero = a leg
# FAILED (grep the log for 'FAILED') or cleanup could not reach zero.

set -euo pipefail
# shellcheck source=e2e/lib.sh
source "$(cd "$(dirname "$0")" && pwd)/lib.sh"

require_env HCLOUD_TOKEN

REGION="${APPRAFTER_E2E_REGION:-hel1}"
SKU="${APPRAFTER_E2E_SERVER_TYPE:-cpx22}"
SKU2="${APPRAFTER_E2E_SERVER_TYPE2:-cx23}"
SSH_PUB="${APPRAFTER_SSH_PUBLIC_KEY_PATH:-$HOME/.ssh/id_ed25519.pub}"
CLUSTER="mp-e2e"
HZ="python3 ${REPO_ROOT}/e2e/hz.py"

if [ ! -r "$SSH_PUB" ]; then
    printf 'ERROR: SSH public key not readable at %s (set APPRAFTER_SSH_PUBLIC_KEY_PATH)\n' "$SSH_PUB" >&2
    exit 2
fi

# Isolate ALL CLI state from the operator's real targets: a throwaway config
# dir + age key, cleaned up on exit. The CLI writes targets + per-target state
# under here (APPRAFTER_CONFIG_DIR), so we can read state.json directly.
WORK="$(mktemp -d -t mp-e2e.XXXXXX)"
export APPRAFTER_CONFIG_DIR="${WORK}/config"
export APPRAFTER_AGE_KEY="${WORK}/age.key"
mkdir -p "$APPRAFTER_CONFIG_DIR"

FAILED=0
fail() { printf '  FAILED: %s\n' "$1" >&2; FAILED=1; }
ok()   { printf '  ok: %s\n' "$1"; }

# --- server introspection via the Hetzner API (never prints the token) ----
# echo "<server_type_name> <location_name> <count>" for apprafter-labelled servers.
hz_server_facts() {
    python3 - "$HCLOUD_TOKEN" <<'PY'
import json, sys, urllib.request
tok = sys.argv[1]
r = urllib.request.Request("https://api.hetzner.cloud/v1/servers?per_page=50",
                           headers={"Authorization": "Bearer " + tok})
with urllib.request.urlopen(r, timeout=30) as resp:
    servers = json.load(resp).get("servers", [])
if not servers:
    print("NONE NONE 0"); sys.exit(0)
s = servers[0]
st = (s.get("server_type") or {}).get("name", "?")
# location lives under datacenter.location.name (and, on newer API, top-level location)
loc = ((s.get("datacenter") or {}).get("location") or {}).get("name") \
      or (s.get("location") or {}).get("name") or "?"
print("%s %s %d" % (st, loc, len(servers)))
PY
}

state_json_path() {
    # cli-state writes per-target state under the config dir; find it.
    find "$APPRAFTER_CONFIG_DIR" -name state.json 2>/dev/null | head -1
}

cleanup() {
    local rc=$?
    phase "cleanup: sweep the test project to zero"
    dump_diagnostics 2>/dev/null || true
    # best-effort CLI destroy first (graceful), then the API sweep (authoritative)
    apprafter destroy --yes 2>/dev/null || true
    $HZ sweep "$HCLOUD_TOKEN" || true
    # verify zero — a non-empty project after sweep is a hard failure (leaked spend)
    if $HZ verify "$HCLOUD_TOKEN"; then
        ok "project swept to zero"
    else
        printf '  FAILED: project NOT empty after sweep — MANUAL CLEANUP NEEDED\n' >&2
        rc=1
    fi
    rm -rf "$WORK"
    if [ "$FAILED" -ne 0 ] || [ "$rc" -ne 0 ]; then
        printf '\n===== machine-picker walk: RED (see FAILED lines above) =====\n' >&2
        exit 1
    fi
    printf '\n===== machine-picker walk: GREEN (all legs ok, project=0) =====\n'
    exit 0
}
trap cleanup EXIT

phase "pre-flight: project must start empty"
$HZ verify "$HCLOUD_TOKEN" || { printf 'ERROR: test project is NOT empty at start — refusing to run\n' >&2; exit 2; }

# ---------------------------------------------------------------------------
phase "Leg 2: no server type → ServerTypeNotSelected, ZERO resources created"
# A target with NO --server-type. `apply` must fail on the create pre-flight
# BEFORE any SSH-key/network/firewall/server is created.
apprafter target add "$CLUSTER" \
    --provider hetzner-cloud --token "$HCLOUD_TOKEN" \
    --tier solo --region "$REGION" --ssh-key "$SSH_PUB" \
    --no-interactive --no-ping
if apprafter apply 2> "${WORK}/leg2.err"; then
    fail "Leg 2: apply SUCCEEDED with no server type (expected ServerTypeNotSelected)"
else
    if grep -qiE "server.type.not.selected|no server type selected" "${WORK}/leg2.err"; then
        ok "Leg 2: apply refused with server_type_not_selected"
    else
        fail "Leg 2: apply failed but NOT with the server-type error:"; sed 's/^/    | /' "${WORK}/leg2.err" >&2
    fi
fi
read -r _t _l cnt < <(hz_server_facts)
if [ "$cnt" = "0" ]; then ok "Leg 2: zero servers created"; else fail "Leg 2: $cnt server(s) leaked on the no-type path"; fi
# also assert NO ancillary resources (pre-flight is before any CREATE)
if $HZ verify "$HCLOUD_TOKEN" >/dev/null; then ok "Leg 2: zero resources of any kind"; else fail "Leg 2: pre-flight created ancillary resources (ssh/network/firewall) before erroring"; fi

# ---------------------------------------------------------------------------
phase "Leg 1: explicit --server-type ${SKU} provisions that exact SKU in ${REGION}"
apprafter target machine --server-type "$SKU" || fail "Leg 1: target machine --server-type failed"
retry 3 20 -- apprafter apply || fail "Leg 1: apply (provision) failed"
sleep 5
read -r stype sloc cnt < <(hz_server_facts)
if [ "$cnt" = "1" ]; then ok "Leg 1: exactly one server provisioned"; else fail "Leg 1: expected 1 server, got $cnt"; fi
if [ "$stype" = "$SKU" ]; then ok "Leg 1: server type == ${SKU}"; else fail "Leg 1: server type is '${stype}', expected ${SKU}"; fi
if [ "$sloc" = "$REGION" ]; then ok "Leg 1: server location == ${REGION}"; else fail "Leg 1: server location is '${sloc}', expected ${REGION}"; fi
SJ="$(state_json_path)"
if [ -n "$SJ" ] && grep -q "\"server_type\"" "$SJ" && grep -q "$SKU" "$SJ"; then
    ok "Leg 1: state.json records server_type=${SKU} (fact)"
else
    fail "Leg 1: state.json did not record server_type (path='${SJ:-none}')"
fi

# ---------------------------------------------------------------------------
phase "Leg 3: legacy self-heal — blank the type, re-apply on the running box → backfill"
# Simulate a pre-2.16h target: strip server_type from BOTH stores.
CFG="$(find "$APPRAFTER_CONFIG_DIR" -path '*targets*/config.yaml' | head -1)"
[ -n "$CFG" ] || fail "Leg 3: could not find target config.yaml"
# remove any server_type: line from the target config (preference) + state (fact)
[ -n "$CFG" ] && sed -i '/^server_type:/d' "$CFG" || true
[ -n "$SJ" ]  && python3 - "$SJ" <<'PY' || true
import json, sys
p = sys.argv[1]
d = json.load(open(p))
hc = d.get("hetzner_cloud")
if isinstance(hc, dict):
    hc.pop("server_type", None)
json.dump(d, open(p, "w"))
PY
ok "Leg 3: blanked server_type in config + state (legacy target simulated)"
# re-apply: the server EXISTS, so no type is required; it must succeed + backfill.
if apprafter apply 2> "${WORK}/leg3.err"; then
    ok "Leg 3: apply on the existing box SUCCEEDED without a type (no-default fires only on create)"
else
    fail "Leg 3: apply FAILED on an existing box with no type (H8 regression!):"; sed 's/^/    | /' "${WORK}/leg3.err" >&2
fi
# the backfill must have re-adopted the live type into the target config
CFG="$(find "$APPRAFTER_CONFIG_DIR" -path '*targets*/config.yaml' | head -1)"
if grep -qE "^server_type: *\"?${SKU}\"?" "$CFG"; then
    ok "Leg 3: backfill re-adopted server_type=${SKU} into the target"
else
    printf '  config after re-apply:\n'; sed 's/^/    | /' "$CFG"
    fail "Leg 3: backfill did NOT restore server_type into the target"
fi

# ---------------------------------------------------------------------------
phase "Leg 4: target machine --server-type ${SKU2} patches the preference (no re-provision)"
apprafter target machine --server-type "$SKU2" --no-ping || fail "Leg 4: target machine --server-type ${SKU2} failed"
CFG="$(find "$APPRAFTER_CONFIG_DIR" -path '*targets*/config.yaml' | head -1)"
if grep -qE "^server_type: *\"?${SKU2}\"?" "$CFG"; then ok "Leg 4: target preference now ${SKU2}"; else fail "Leg 4: preference not updated to ${SKU2}"; fi
# the running server must be UNCHANGED (patch is a preference, not a resize)
read -r stype _ cnt < <(hz_server_facts)
if [ "$stype" = "$SKU" ] && [ "$cnt" = "1" ]; then
    ok "Leg 4: running server still ${SKU} (preference change did not touch the box)"
else
    fail "Leg 4: running server changed unexpectedly (type='${stype}', count=${cnt})"
fi

phase "all legs done — cleanup runs on exit"
