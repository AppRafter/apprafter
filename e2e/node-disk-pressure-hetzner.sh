#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# D8 + the disk half of D22 — on a REAL Hetzner node, because kind cannot.
#
# WHAT THIS PROVES, AND WHY IT NEEDS HARDWARE
# -------------------------------------------
# Two signals in the 2.22d capacity work have never been observed working:
#
#   D22 (disk half) — the provisioner samples an owned disk's OWN PVC through
#     the kubelet Summary API and writes `status.capacity.{usedBytes,
#     capacityBytes}` onto the ResourceClaim. On kind the kubelet reports no
#     stats for a hostPath-backed local-path volume, so the figure is always
#     absent and every local walk SOFT-SKIPS it. The number has therefore never
#     been seen to appear at all.
#
#   D8 (node half) — a node running out of disk is published as a
#     `NodeDiskPressure` condition on the PlatformStack singleton, and every
#     CLI command warns off it. Firing it needs a node filesystem that is
#     genuinely more than 85% full: `DEFAULT_NODE_FREE_THRESHOLD = 0.15` is a
#     hard-coded constant (operator-core/src/capacity.rs), with no CR field and
#     no env override, so there is no way to trigger it except to fill a disk.
#
# So this walk provisions one real node, fills it on purpose, and asserts the
# whole chain — sample -> condition -> CLI banner -> recovery.
#
# THE FILL IS `fallocate`, NOT `dd` AND NOT `truncate`
# ----------------------------------------------------
# `truncate` makes a SPARSE file: the size is metadata, no blocks are consumed,
# and the kubelet's availableBytes does not move — the walk would fill nothing
# and prove nothing. `dd` works but writes every byte. `fallocate` allocates
# the blocks without writing them, which moves the figure immediately. busybox
# does not always ship the applet, so there is a `dd` fallback.
#
# The target is ~10% free, not 1%: below the threshold with room to spare, and
# still enough headroom that k3s, etcd and containerd keep running. A walk that
# wedges the cluster it is measuring proves the wrong thing.
#
# THE CLI CACHE IS LOAD-BEARING AND MUST BE CLEARED
# --------------------------------------------------
# `node_disk_check` caches its verdict for five minutes under
# `~/.cache/apprafter/node-disk-check.json` so that every command does not pay
# an apiserver round trip. That cache is exactly long enough to span this
# walk's fill and recovery, so a stale "no pressure" entry would mask the
# banner and a stale "pressure" entry would fake it. Both directions are
# cleared explicitly before the assertion that depends on them.
#
# Required env:
#   HCLOUD_TOKEN_OLD (or HCLOUD_TOKEN)  — Hetzner project token, baseline empty.
#   APPRAFTER_DR_SSH_PUBLIC_KEY_PATH    — ssh public key path.
# Optional:
#   APPRAFTER_DR_REGION                 — default nbg1.
#   APPRAFTER_E2E_SERVER_TYPE           — default cpx22. MANDATORY since 2.16h-a
#                                         removed the implicit default; two
#                                         walks silently could not provision at
#                                         all for weeks because of it.
#   APPRAFTER_HETZNER_SKIP_DESTROY=1    — keep the cluster (debug).
#
# COST-BOUNDED + TEARDOWN-SAFE: one small server, and the EXIT trap always
# destroys it and API-verifies the project is back to zero.
#
# Exit: 0 green / 1 assertion failure / 2 precondition missing.
# PASS/FAIL IS JUDGED BY READING THE LOG — the `ok:` lines and the final GREEN
# banner — never by the exit code.

set -euo pipefail

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

TARGET="d8-disk"
REGION="${APPRAFTER_DR_REGION:-nbg1}"
SERVER_TYPE="${APPRAFTER_E2E_SERVER_TYPE:-cpx22}"
APP_NS="diskdemo"
APP="disky"
APP_RES="application.apprafter.io"
CLAIM_RES="resourceclaim.apprafter.io"
BALLAST_POD="d8-ballast"
BALLAST_PATH="/var/lib/apprafter-d8-ballast"

TOKEN="${HCLOUD_TOKEN_OLD:-${HCLOUD_TOKEN:-}}"

# ---------------------------------------------------------------
# Preconditions — BEFORE the destroy trap is armed (exit 2, never provisions).
# ---------------------------------------------------------------
[ -n "$TOKEN" ] || { printf 'ERROR: set HCLOUD_TOKEN_OLD (or HCLOUD_TOKEN)\n' >&2; exit 2; }
: "${APPRAFTER_DR_SSH_PUBLIC_KEY_PATH:?set APPRAFTER_DR_SSH_PUBLIC_KEY_PATH (exit 2)}"
[ -r "$APPRAFTER_DR_SSH_PUBLIC_KEY_PATH" ] || {
    printf 'ERROR: ssh key not readable: %s\n' "$APPRAFTER_DR_SSH_PUBLIC_KEY_PATH" >&2; exit 2; }
for t in kubectl curl python3 cargo; do
    command -v "$t" >/dev/null 2>&1 || { printf 'ERROR: missing tool: %s\n' "$t" >&2; exit 2; }
done

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"; mkdir -p "$APPRAFTER_CONFIG_DIR"
export APPRAFTER_CONFIG_DIR
KC_FILE="${TMPDIR_WORK}/kubeconfig"
CREATED=0

hetzner_server_count() {
    local body
    { set +x; } 2>/dev/null
    body=$(curl -fsS -H "Authorization: Bearer ${TOKEN}" \
        "https://api.hetzner.cloud/v1/servers" 2>/dev/null || true)
    [ -n "$body" ] || { printf '?'; return 0; }
    printf '%s' "$body" | python3 -c 'import json,sys
try: print(len(json.load(sys.stdin).get("servers", [])))
except Exception: print("?")'
}

cleanup() {
    local exit_code=$?
    # Best-effort: drop the ballast before teardown so a SKIP_DESTROY debug
    # session is not left on a nearly full disk.
    kubectl delete pod "$BALLAST_POD" --ignore-not-found --wait=false >/dev/null 2>&1 || true
    if [ -z "${APPRAFTER_HETZNER_SKIP_DESTROY:-}" ]; then
        if [ "$CREATED" -eq 1 ]; then
            printf '\n=== destroying %s ===\n' "$TARGET" >&2
            apprafter destroy --yes --target "$TARGET" || \
                printf 'WARN: destroy --target %s returned non-zero\n' "$TARGET" >&2
            local n; n=$(hetzner_server_count)
            if [ "$n" = "0" ]; then
                printf 'ok: project has ZERO servers after destroy (API-verified)\n' >&2
            else
                printf 'LEAK WARNING: project reports %s server(s) — INSPECT https://console.hetzner.cloud AND DELETE BY HAND\n' "$n" >&2
            fi
        fi
        rm -rf "$TMPDIR_WORK"
    else
        printf '\nAPPRAFTER_HETZNER_SKIP_DESTROY set — cluster left UP.\n' >&2
        [ "$CREATED" -eq 1 ] && printf 'Destroy by hand: APPRAFTER_CONFIG_DIR=%s apprafter destroy --yes --target %s\n' "$APPRAFTER_CONFIG_DIR" "$TARGET" >&2
    fi
    exit "$exit_code"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# ---------------------------------------------------------------
# Local helpers. Every read that may legitimately fail while polling carries
# `|| true`: under `set -o pipefail` a failing kubectl inside a pipeline makes
# the assignment non-zero and `set -e` kills the walk on the first poll, one
# second in, with no error of its own.
# ---------------------------------------------------------------
jp() { kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true; }

assert_eq() {
    local desc="$1" got="$2" want="$3"
    if [ "$got" = "$want" ]; then printf '  ok: %s = %q\n' "$desc" "$got"; return 0; fi
    printf 'ERROR: %s — got %q, want %q\n' "$desc" "$got" "$want" >&2
    return 1
}

assert_contains() {
    local desc="$1" hay="$2" needle="$3"
    case "$hay" in
        *"$needle"*) printf '  ok: %s (found %q)\n' "$desc" "$needle"; return 0 ;;
    esac
    printf 'ERROR: %s — %q not found in:\n%s\n' "$desc" "$needle" "$hay" >&2
    return 1
}

# The kubelet Summary node filesystem figures, as "<available> <capacity>".
node_fs() {
    kubectl get --raw "/api/v1/nodes/${NODE}/proxy/stats/summary" 2>/dev/null \
        | python3 -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: sys.exit(0)
fs=(d.get("node") or {}).get("fs") or {}
a,c=fs.get("availableBytes"),fs.get("capacityBytes")
if a is None or c is None: sys.exit(0)
print(a,c)' 2>/dev/null || true
}

# The CLI caches its disk verdict for five minutes; both directions of this
# walk cross that window, so the cache is dropped before every assertion that
# reads it.
drop_cli_cache() {
    rm -f "${XDG_CACHE_HOME:-$HOME/.cache}/apprafter/node-disk-check.json" 2>/dev/null || true
}

# ===============================================================
# Phase 1: provision + bootstrap ONE real node
# ===============================================================
phase "Phase 1: provision + bootstrap one Hetzner node (${SERVER_TYPE}, ${REGION})"
{ set +x; } 2>/dev/null
apprafter target add "$TARGET" \
    --provider hetzner-cloud --tier solo --region "$REGION" \
    --server-type "$SERVER_TYPE" \
    --token "$TOKEN" --ssh-key "$APPRAFTER_DR_SSH_PUBLIC_KEY_PATH" \
    --no-interactive --no-ping --force
CREATED=1
apprafter target use "$TARGET"
apprafter up --target "$TARGET"
apprafter kubeconfig --target "$TARGET" >"$KC_FILE"
export KUBECONFIG="$KC_FILE"

NODE=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
[ -n "$NODE" ] || { printf 'FAILED: no node name\n' >&2; exit 1; }
printf '  ok: node = %s (%s)\n' "$NODE" \
    "$(kubectl get node "$NODE" -o jsonpath='{.status.nodeInfo.kubeletVersion}' 2>/dev/null || true)"

# The whole walk rests on the kubelet reporting node.fs. On k3s it does; the
# 2.6c probe was opened precisely because one earlier run suggested otherwise.
# Assert it up front so a missing field fails HERE, naming itself, instead of
# surfacing three phases later as an empty capacity figure.
read -r AVAIL CAP <<<"$(node_fs)"
[ -n "${CAP:-}" ] && [ "${CAP:-0}" -gt 0 ] || {
    printf 'FAILED: kubelet Summary reports no node.fs on this node — the capacity chain cannot work\n' >&2
    kubectl get --raw "/api/v1/nodes/${NODE}/proxy/stats/summary" 2>&1 | head -40 >&2 || true
    exit 1; }
printf '  ok: kubelet node.fs present — %s free of %s bytes (%.1f%% free)\n' \
    "$AVAIL" "$CAP" "$(python3 -c "print(100*$AVAIL/$CAP)")"

# ===============================================================
# Phase 2: a needs.disk claim gets a REAL capacity figure (D22, disk half)
# ===============================================================
phase "Phase 2: needs.disk claim carries status.capacity — the figure kind cannot produce"

kubectl create namespace "$APP_NS" --dry-run=client -o yaml | kubectl apply -f -
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP}
  namespace: ${APP_NS}
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    needs:
      # `mountPath` is REQUIRED and the claim name is derived from its last
      # segment when `name` is omitted (schemas/v1alpha1/application.cue
      # #DiskClaim). A first draft of this walk declared `size` alone and the
      # admission webhook rejected it — correctly, and on real hardware five
      # minutes into a paid run. Worth the note: the webhook is the only layer
      # that enforces this, so `cue vet` would not have caught it either.
      disk:
        size: 1Gi
        mountPath: /data
YAML

# The claim is generated by the operator; poll for it, then for Ready.
_deadline=$(( $(date +%s) + 300 ))
CLAIM=""
while [ "$(date +%s)" -lt "$_deadline" ]; do
    CLAIM=$(kubectl -n "$APP_NS" get "$CLAIM_RES" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
    [ -n "$CLAIM" ] && break
    sleep 5
done
[ -n "$CLAIM" ] || { printf 'FAILED: no ResourceClaim was generated for %s\n' "$APP" >&2; exit 1; }
printf '  claim = %s\n' "$CLAIM"

_deadline=$(( $(date +%s) + 420 ))
CAP_BYTES=""
while [ "$(date +%s)" -lt "$_deadline" ]; do
    CAP_BYTES=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.capacity.capacityBytes}')
    [ -n "$CAP_BYTES" ] && [ "$CAP_BYTES" != "0" ] && break
    sleep 10
done
if [ -z "$CAP_BYTES" ] || [ "$CAP_BYTES" = "0" ]; then
    printf 'FAILED: the disk claim never carried status.capacity.capacityBytes\n' >&2
    printf '  THIS IS THE D22 DISK HALF. It is the assertion every kind walk skips,\n' >&2
    printf '  and this walk exists because only real hardware can make it.\n' >&2
    kubectl -n "$APP_NS" get "$CLAIM_RES" "$CLAIM" -o yaml >&2 2>&1 || true
    exit 1
fi
USED_BYTES=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.capacity.usedBytes}')
printf '  ok: disk claim capacity sampled — usedBytes=%s capacityBytes=%s\n' \
    "${USED_BYTES:-<absent>}" "$CAP_BYTES"
[ -n "$USED_BYTES" ] || { printf 'FAILED: capacityBytes present but usedBytes absent — a half-written figure\n' >&2; exit 1; }
# A 1Gi PVC cannot report a capacity of zero or a used figure above it.
python3 -c "
import sys
used, cap = int('$USED_BYTES'), int('$CAP_BYTES')
if cap <= 0 or used < 0 or used > cap:
    print(f'FAILED: implausible figure used={used} cap={cap}', file=sys.stderr); sys.exit(1)
print(f'  ok: the figure is internally consistent ({100*used/cap:.1f}% used)')
"

# D29 — THE FIGURE MUST SAY WHICH THING IT MEASURED.
#
# The first run of this walk found the claim reporting capacityBytes identical
# to the NODE filesystem: a 1Gi claim presented as an 80GB one. Nothing in the
# sampling is wrong — the kubelet reports the BACKING FILESYSTEM for a
# local-path PV, because a directory on a shared disk has no quota to report
# against — but presenting it as the CLAIM's capacity is the node's fact
# wearing the claim's name, which is D8's own title one layer down.
#
# The fix records the scope rather than discarding a genuinely useful number
# ("this volume shares an 80GB disk that is 12% full" is the actionable fact on
# a single-node tier). So on THIS backend the assertion is exact: the figures
# equal the node's, and the operator must say so.
assert_eq "the sampled figure equals the node disk (local-path has no quota)" \
    "$CAP_BYTES" "$CAP"
SCOPE=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM" '{.status.capacity.scope}')
if [ "$SCOPE" != "host" ]; then
    printf 'FAILED: the claim reports the node disk but labels it %q, not "host"\n' \
        "${SCOPE:-<absent>}" >&2
    printf '  An unlabelled figure tells the reader their 1Gi volume holds 80GB (D29).\n' >&2
    kubectl -n "$APP_NS" get "$CLAIM_RES" "$CLAIM" -o jsonpath='{.status.capacity}' >&2 2>&1 || true
    echo >&2
    exit 1
fi
printf '  ok: the figure is labelled scope=host — the node disk, named as such\n'
# The CLI side (`1Gi · host disk 12%% full` instead of `9.0 GB / 74.8 GB`) is
# unit-covered in app.rs; it is not asserted here because this walk applies the
# Application CR directly and `app status` resolves through an Argo CD
# Application, which a direct CR does not have.

# ===============================================================
# Phase 3: fill the node past the threshold -> NodeDiskPressure (D8)
# ===============================================================
phase "Phase 3: fill the node below 15% free -> PlatformStack NodeDiskPressure"

read -r AVAIL CAP <<<"$(node_fs)"
# Leave ~10% free: under the 0.15 threshold, above the point where k3s suffers.
BALLAST_BYTES=$(python3 -c "
avail, cap = int('$AVAIL'), int('$CAP')
target_free = int(cap * 0.10)
print(max(0, avail - target_free))")
if [ "$BALLAST_BYTES" -le 0 ]; then
    printf 'FAILED: node is already below 10%% free before the fill — refusing to make it worse\n' >&2
    exit 1
fi
printf '  allocating %s bytes of ballast at %s (leaving ~10%%%% free)\n' "$BALLAST_BYTES" "$BALLAST_PATH"

kubectl apply -f - <<YAML
apiVersion: v1
kind: Pod
metadata:
  name: ${BALLAST_POD}
spec:
  restartPolicy: Never
  nodeName: ${NODE}
  tolerations:
    - operator: Exists
  containers:
    - name: fill
      image: busybox:1.36
      securityContext: { privileged: true }
      command: ["sh", "-c"]
      args:
        - |
          set -e
          # fallocate ALLOCATES blocks without writing them, so the kubelet's
          # availableBytes moves at once. truncate would create a sparse file
          # and move nothing. dd is the fallback when the applet is absent.
          if fallocate -l ${BALLAST_BYTES} /host${BALLAST_PATH} 2>/dev/null; then
            echo "fallocate ok"
          else
            echo "fallocate unavailable — falling back to dd"
            dd if=/dev/zero of=/host${BALLAST_PATH} bs=1M \
               count=\$(( ${BALLAST_BYTES} / 1048576 )) status=none
          fi
          ls -l /host${BALLAST_PATH}
      volumeMounts:
        - { name: host, mountPath: /host }
  volumes:
    - name: host
      hostPath: { path: / }
YAML

_deadline=$(( $(date +%s) + 600 ))
BALLAST_PHASE=""
while [ "$(date +%s)" -lt "$_deadline" ]; do
    BALLAST_PHASE=$(kubectl get pod "$BALLAST_POD" -o jsonpath='{.status.phase}' 2>/dev/null || true)
    case "$BALLAST_PHASE" in Succeeded|Failed) break ;; esac
    sleep 10
done
assert_eq "the ballast pod completed" "$BALLAST_PHASE" "Succeeded"

read -r AVAIL2 CAP2 <<<"$(node_fs)"
FREE_PCT=$(python3 -c "print(f'{100*int('$AVAIL2')/int('$CAP2'):.1f}')")
printf '  node is now %s%% free (was %s%%)\n' "$FREE_PCT" \
    "$(python3 -c "print(f'{100*int('$AVAIL')/int('$CAP'):.1f}')")"
python3 -c "
import sys
if 100*int('$AVAIL2')/int('$CAP2') >= 15.0:
    print('FAILED: the fill did not push the node under the 15% threshold — nothing below can fire', file=sys.stderr)
    sys.exit(1)
"

# The operator caches a kubelet sample for 30s and the PlatformStack controller
# writes the condition on its own tick; budget generously.
_deadline=$(( $(date +%s) + 300 ))
PRESSURE=""
while [ "$(date +%s)" -lt "$_deadline" ]; do
    PRESSURE=$(kubectl -n apprafter-system get platformstack default \
        -o jsonpath='{.status.conditions[?(@.type=="NodeDiskPressure")].status}' 2>/dev/null || true)
    [ "$PRESSURE" = "True" ] && break
    sleep 10
done
if [ "$PRESSURE" != "True" ]; then
    printf 'FAILED: node is %s%% full but PlatformStack carries no NodeDiskPressure=True (got %q)\n' \
        "$FREE_PCT" "${PRESSURE:-<absent>}" >&2
    kubectl -n apprafter-system get platformstack default -o yaml >&2 2>&1 || true
    kubectl -n apprafter-system logs deploy/apprafter-operator --tail=60 >&2 2>&1 || true
    exit 1
fi
PRESSURE_MSG=$(kubectl -n apprafter-system get platformstack default \
    -o jsonpath='{.status.conditions[?(@.type=="NodeDiskPressure")].message}' 2>/dev/null || true)
printf '  ok: NodeDiskPressure=True — %s\n' "$PRESSURE_MSG"
[ -n "$PRESSURE_MSG" ] || { printf 'FAILED: the condition fired with an EMPTY message — nothing for a CLI to print\n' >&2; exit 1; }

# ===============================================================
# Phase 4: every command warns (D8's second half)
# ===============================================================
phase "Phase 4: the CLI warns off that condition, without being asked"

# Drop the five-minute verdict cache first: a pre-fill "no pressure" entry
# would mask the banner and read as a product failure.
#
# ASSERT THE BANNER'S OWN PREFIX, NOT THE WORD "disk". This walk's app is
# called `disky` and declares `needs.disk`, so "disk" appears in `app status`
# output whether or not the node is under pressure — an assertion that cannot
# fail is not a test. The banner is `Node disk: <operator message>`
# (node_disk_check.rs:62-65), and that prefix appears nowhere else.
# `whoami` ON PURPOSE. D8's second half is "every command warns", and the hook
# sits in the shared pre-dispatch path (lib.rs:97) precisely because a full
# node stops every workload at once, not just the one somebody happens to be
# asking about. Proving it with a disk-shaped command would prove the weaker
# thing. `whoami` has nothing to do with storage — and it does not need an Argo
# Application, which `app status` does (resolve_app_for_command), and which
# this walk's directly-applied CR does not have.
drop_cli_cache
STATUS_OUT="$(NO_COLOR=1 apprafter whoami 2>&1 || true)"
printf '%s\n' "$STATUS_OUT"
assert_contains "an ordinary command surfaces the node's disk pressure unasked" \
    "$STATUS_OUT" "Node disk:"
# And it carries the operator's own words, not a generic string the CLI made up.
assert_contains "the banner relays the condition's message" \
    "$STATUS_OUT" "$(printf '%s' "$PRESSURE_MSG" | cut -c1-24)"

# ===============================================================
# Phase 5: recovery — the warning is a state, not a latch
# ===============================================================
phase "Phase 5: drop the ballast -> the condition clears"

kubectl delete pod "$BALLAST_POD" --ignore-not-found --wait=true >/dev/null 2>&1 || true
kubectl apply -f - <<YAML
apiVersion: v1
kind: Pod
metadata:
  name: ${BALLAST_POD}-rm
spec:
  restartPolicy: Never
  nodeName: ${NODE}
  tolerations:
    - operator: Exists
  containers:
    - name: rm
      image: busybox:1.36
      securityContext: { privileged: true }
      command: ["sh", "-c", "rm -f /host${BALLAST_PATH}; echo removed"]
      volumeMounts:
        - { name: host, mountPath: /host }
  volumes:
    - name: host
      hostPath: { path: / }
YAML
_deadline=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$_deadline" ]; do
    p=$(kubectl get pod "${BALLAST_POD}-rm" -o jsonpath='{.status.phase}' 2>/dev/null || true)
    case "$p" in Succeeded|Failed) break ;; esac
    sleep 5
done
printf '  ballast removed (pod phase %s)\n' "${p:-<unknown>}"

_deadline=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$_deadline" ]; do
    PRESSURE=$(kubectl -n apprafter-system get platformstack default \
        -o jsonpath='{.status.conditions[?(@.type=="NodeDiskPressure")].status}' 2>/dev/null || true)
    [ "$PRESSURE" != "True" ] && break
    sleep 10
done
if [ "$PRESSURE" = "True" ]; then
    read -r AVAIL3 CAP3 <<<"$(node_fs)"
    printf 'FAILED: NodeDiskPressure stayed True after the disk was freed (%s%% free) — a latch, not a state\n' \
        "$(python3 -c "print(f'{100*int('$AVAIL3')/int('$CAP3'):.1f}')")" >&2
    exit 1
fi
printf '  ok: NodeDiskPressure cleared once the disk was freed (now %q)\n' "${PRESSURE:-<absent>}"

drop_cli_cache
STATUS_OUT2="$(NO_COLOR=1 apprafter whoami 2>&1 || true)"
case "$STATUS_OUT2" in
    *"Node disk:"*)
        printf 'FAILED: the CLI still warns after recovery — a banner that never clears is one people learn to scroll past\n' >&2
        printf '%s\n' "$STATUS_OUT2" >&2
        exit 1 ;;
esac
printf '  ok: the CLI stopped warning once the condition cleared\n'

# ===============================================================
# Done
# ===============================================================
printf '\nnode-disk-pressure-hetzner GREEN in %s\n' "$(elapsed)"
printf 'Chain proven: real node -> needs.disk claim carries a sampled status.capacity (D22 disk half, impossible on kind) -> fill below 15%% free -> PlatformStack NodeDiskPressure=True with a message -> the CLI warns unasked (D8) -> free the disk -> the condition and the banner both clear\n'
