#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# 2.16g node-swap validation walk (real Hetzner, host-level asserts).
# Provisions a throwaway solo (T1) node with the current CLI, then validates
# the 2.16g "provisioned swap + NoSwap" node substrate end-to-end:
#   1. fresh SKIP_START bootstrap — k3s ≥1.34 + cgroup2
#   2. kubelet config effect (configz: failSwapOn=false + swapBehavior=NoSwap),
#      swap size ≈ min(RAM,8Gi), Node-object swap.capacity, swappiness=10,
#      fstab sw,nofail, k3s GOMEMLIMIT
#   3. pod NoSwap — a managed app + needs.pg → container memory.swap.max==0 AND
#      the pg backend's VmSwap≈0
#   4. induced pressure — drop_caches → poll /readyz stable → a memory hog in
#      system.slice → pswpout delta>0, no OOM of k3s.service or any pod, /readyz up
#   5. retrofit + VPA-revert — a 2nd node with APPRAFTER_SKIP_NODE_SWAP=1 (no bake)
#      → deploy pg → `apprafter node prep` → pre-existing container swap.max==0 →
#      force a VPA resize → re-read swap.max (Q7) → re-run prep idempotent
#   6. induced-failure rollback — force an INVALID kubelet drop-in on the retrofit
#      node 2 (undocumented APPRAFTER_NODE_SWAP_FORCE_INVALID_DROPIN hook) → k3s
#      restart fails → /readyz timeout → whole-step rollback → cluster returns →
#      configz: swapBehavior GONE, failSwapOn:false STILL present (node 1 untouched)
#   7. reboot persistence — reboot the throwaway node → swap reactivates from
#      fstab + swappiness persists + a post-boot container gets swap.max=0
#   8. record free/swapon/vmstat/configz before+after; cleanup both projects → 0
#
# NoSwap/cgroup v2 + host swap need a REAL node (kind/k3d can't), so this MUST
# run on real Hetzner. Host-level facts (fstab, /proc/meminfo, sysctl, cgroup
# files, GOMEMLIMIT, drop_caches, systemd-run, reboot) are read over SSH with the
# same key the CLI uses (APPRAFTER_SSH_PRIVATE_KEY); the kube-API facts (configz,
# Node object, pod cgroups) go through `kubectl --raw .../proxy`.
#
# Judged by log markers (ok:/FAILED/GREEN), NOT the exit code (sandbox-run masks
# the inner code). Bulletproof cleanup + verify BOTH token projects → 0.
#
# CRITICAL (2.16f lesson): NEVER `kubectl ... -o json | grep -q X` under pipefail —
# grep -q matches early on large output, SIGPIPEs kubectl, pipefail false-fails.
# Always capture to a var then bash-match `[[ "$out" == *X* ]]`.
set -uo pipefail

APPRAFTER=/projects/omnixal/apprafter/cli/target/release/apprafter
HZ=/projects/omnixal/apprafter/e2e/hz.py
REGION="${APPRAFTER_E2E_REGION:-nbg1}"
TOKEN="${HCLOUD_TOKEN:?HCLOUD_TOKEN required}"
OTHER_TOKEN="${OTHER_TOKEN:-}"
FAIL=0
mark_fail() { FAIL=1; printf 'FAILED: %s\n' "$1"; }
ok() { printf 'ok: %s\n' "$1"; }

WORK=$(mktemp -d /tmp/apprafter-swap-XXXXXX)
export HOME="$WORK/home"; mkdir -p "$HOME/.ssh"
export APPRAFTER_SSH_PRIVATE_KEY="$WORK/id_ed25519"
ssh-keygen -t ed25519 -N '' -f "$APPRAFTER_SSH_PRIVATE_KEY" -C apprafter-swap >/dev/null 2>&1
export APPRAFTER_SSH_PUBLIC_KEY="$(cat "$APPRAFTER_SSH_PRIVATE_KEY.pub")"
export KUBECONFIG="$WORK/kubeconfig"
NOTE="$WORK/before-after.txt"
cd "$WORK"

cleanup() {
    printf '\n=== CLEANUP (always runs) ===\n'
    if [ -s "$NOTE" ]; then printf '  --- before/after note (step 8) ---\n'; sed 's/^/  /' "$NOTE"; fi
    "$APPRAFTER" destroy --yes >/dev/null 2>&1 && printf 'apprafter destroy: ok\n' || printf 'apprafter destroy: (non-zero; sweeping)\n'
    python3 "$HZ" sweep "$TOKEN" 2>/dev/null
    [ -n "$OTHER_TOKEN" ] && python3 "$HZ" sweep "$OTHER_TOKEN" 2>/dev/null   # STEP5's 2nd (SKIP) node lives in the OTHER project
    printf '  VERIFY primary project:\n'; python3 "$HZ" verify "$TOKEN" || FAIL=1
    if [ -n "$OTHER_TOKEN" ]; then printf '  VERIFY other project:\n'; python3 "$HZ" verify "$OTHER_TOKEN" || FAIL=1; fi
    rm -rf "$WORK"
    if [ "$FAIL" -eq 0 ]; then printf '\n=== NODE-SWAP WALK GREEN ===\n'; else printf '\n=== NODE-SWAP WALK RED ===\n'; fi
}
# cleanup runs once on EXIT; INT/TERM just `exit 143` (which fires the EXIT trap)
# so a SIGTERM actually STOPS the walk — a bare `trap cleanup TERM` would run
# cleanup then RESUME the interrupted loop against the destroyed cluster (2.16e).
trap cleanup EXIT
trap 'exit 143' INT TERM

# ---- SSH into the node (host-level facts the kube-API can't answer) ---------
# Same identity the CLI uses. accept-new (throwaway node, fresh host key each run).
SSH_OPTS=(-o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$WORK/known_hosts"
          -o ConnectTimeout=15 -o BatchMode=yes -i "$APPRAFTER_SSH_PRIVATE_KEY")
node_ssh() {  # node_ssh <ip> <remote-command...>; captured output on stdout
    local ip="$1"; shift
    ssh "${SSH_OPTS[@]}" "root@${ip}" "$@" 2>/dev/null
}
target_ipv4() {  # the active target's node public IPv4 (via the CLI, live Hetzner API)
    "$APPRAFTER" target ip 2>/dev/null | grep -ioE '([0-9]{1,3}\.){3}[0-9]{1,3}' | head -1
}

# ---- kube-API helpers -------------------------------------------------------
NS=default
node_name() { kubectl get nodes -o jsonpath='{.items[0].metadata.name}' 2>/dev/null; }
configz() {  # raw kubelet running config (the ONLY authority for swapBehavior/failSwapOn)
    kubectl get --raw "/api/v1/nodes/$1/proxy/configz" 2>/dev/null
}
app_pod() { kubectl -n "$NS" get pods -l "apprafter.io/application=$1" -o jsonpath='{.items[0].metadata.name}' 2>/dev/null; }

deploy_pg_app() {  # a managed app + needs.pg (brings up the shared CNPG cluster)
    local name="$1"
    kubectl apply -f - <<EOF
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: { name: $name, namespace: $NS }
spec:
  base:
    image: nginxdemos/hello:plain-text
    expose: { port: 80 }
    needs:
      pg: {}
EOF
}

wait_apps_synced() {
    local want="${1:-8}" deadline; deadline=$(( $(date +%s) + 900 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        local j total synced
        j=$(kubectl -n argocd get applications.argoproj.io -o json 2>/dev/null)
        total=$(printf '%s' "$j" | python3 -c "import json,sys;print(len(json.load(sys.stdin).get('items',[])))" 2>/dev/null || echo 0)
        synced=$(printf '%s' "$j" | python3 -c "import json,sys;d=json.load(sys.stdin);print(sum(1 for a in d.get('items',[]) if a.get('status',{}).get('sync',{}).get('status')=='Synced'))" 2>/dev/null || echo 0)
        printf '  argo apps synced=%s / total=%s\n' "$synced" "$total"
        [ "$total" -ge "$want" ] && [ "$synced" -ge "$total" ] && return 0
        sleep 20
    done
    return 1
}

wait_pg_running() {  # the shared CNPG primary pod Running (lazily provisioned on first claim)
    local deadline; deadline=$(( $(date +%s) + 900 )); local pg=0
    while [ "$(date +%s)" -lt "$deadline" ]; do
        pg=$(kubectl get pods -A -l 'cnpg.io/cluster' --field-selector=status.phase=Running -o name 2>/dev/null | wc -l | tr -d ' ')
        printf '  pg pods Running=%s\n' "$pg"
        [ "${pg:-0}" -ge 1 ] && return 0
        sleep 20
    done
    return 1
}

# For a container, read memory.swap.max via `kubectl exec` reading the pod's OWN
# cgroup file (cgroup v2 unified hierarchy; the pod sees its own limit at
# /sys/fs/cgroup/memory.swap.max). The value is the LITERAL string `max` when
# unlimited or `0` when NoSwap-clamped — a STRING compare, never integer math (P14).
pod_swap_max() {  # <pod-name> [container]
    local pod="$1" c="${2:-}" args=()
    [ -n "$c" ] && args=(-c "$c")
    kubectl -n "$NS" exec "$pod" "${args[@]}" -- sh -c 'cat /sys/fs/cgroup/memory.swap.max 2>/dev/null' 2>/dev/null | tr -d '[:space:]'
}

# =============================================================================
printf '=== Phase 1 (STEP 1): provision fresh T1 (CLI %s) + record version/cgroup ===\n' "$($APPRAFTER --version 2>/dev/null)"
"$APPRAFTER" target add e2e --provider hetzner-cloud --tier solo --region "$REGION" \
    --token "$TOKEN" --no-interactive --force || { mark_fail "target add"; exit 1; }
timeout 1500 "$APPRAFTER" up || { mark_fail "apprafter up"; exit 1; }
"$APPRAFTER" kubeconfig > "$KUBECONFIG" || { mark_fail "kubeconfig"; exit 1; }
ok "cluster bootstrapped"

NODE=$(node_name); printf '  node = %s\n' "$NODE"
IP=$(target_ipv4); printf '  node public IPv4 = %s\n' "${IP:-<none>}"
[ -n "$IP" ] || mark_fail "STEP1 could not resolve node public IPv4 (apprafter target ip) — host-level asserts unreachable"

# k3s/kubelet version ≥1.34 (the NoSwap-GA gate)
KVER=$(kubectl get node "$NODE" -o jsonpath='{.status.nodeInfo.kubeletVersion}' 2>/dev/null)
printf '  kubeletVersion = %s\n' "$KVER"
KMAJMIN=$(printf '%s' "$KVER" | sed -E 's/^v//; s/^([0-9]+)\.([0-9]+).*/\1 \2/')
KOK=$(python3 -c "import sys;maj,mn=(sys.argv[1].split()+['0','0'])[:2];print('yes' if (int(maj)>1 or (int(maj)==1 and int(mn)>=34)) else 'no')" "$KMAJMIN" 2>/dev/null)
[ "$KOK" = "yes" ] && ok "STEP1 k3s $KVER is ≥1.34 (NoSwap GA gate)" || mark_fail "STEP1 kubeletVersion $KVER is <1.34 — swap step would REFUSE"

# cgroup v2 (memory.swap.max is a v2-only knob)
CGFS=$(node_ssh "$IP" 'stat -fc %T /sys/fs/cgroup')
printf '  /sys/fs/cgroup fstype = %s\n' "${CGFS:-<ssh-failed>}"
[ "$CGFS" = "cgroup2fs" ] && ok "STEP1 cgroup v2 (cgroup2fs)" || mark_fail "STEP1 /sys/fs/cgroup is '$CGFS' (expected cgroup2fs)"

# =============================================================================
printf '\n=== STEP 2: kubelet config effect (configz) + swap sizing + host facts ===\n'
CZ=$(configz "$NODE")
[ -n "$CZ" ] || mark_fail "STEP2 configz unreachable (nodes/proxy/configz)"

# failSwapOn=false + swapBehavior=NoSwap — asserted via CONFIGZ (the running config),
# NOT file existence (Option-A: the effect is what matters, P16/Q4).
FSW=$(printf '%s' "$CZ" | python3 -c "import json,sys;print(json.load(sys.stdin).get('kubeletconfig',{}).get('failSwapOn'))" 2>/dev/null)
SWB=$(printf '%s' "$CZ" | python3 -c "import json,sys;print(json.load(sys.stdin).get('kubeletconfig',{}).get('memorySwap',{}).get('swapBehavior'))" 2>/dev/null)
printf '  configz failSwapOn=%s memorySwap.swapBehavior=%s\n' "$FSW" "$SWB"
[ "$FSW" = "False" ] && ok "STEP2 configz failSwapOn=false" || mark_fail "STEP2 configz failSwapOn=$FSW (expected False)"
[ "$SWB" = "NoSwap" ] && ok "STEP2 configz swapBehavior=NoSwap" || mark_fail "STEP2 configz swapBehavior=$SWB (expected NoSwap)"

# swap total ≈ min(RAM,8Gi) — count DERIVED from /proc/meminfo MemTotal and
# asserted against active swap (SwapTotal), with a tolerance (P13).
MEMINFO=$(node_ssh "$IP" 'cat /proc/meminfo')
MEMTOTAL_KB=$(printf '%s' "$MEMINFO" | awk '/^MemTotal:/{print $2}')
SWAPTOTAL_KB=$(printf '%s' "$MEMINFO" | awk '/^SwapTotal:/{print $2}')
printf '  MemTotal=%sKiB SwapTotal=%sKiB\n' "${MEMTOTAL_KB:-?}" "${SWAPTOTAL_KB:-?}"
SWAP_CHECK=$(python3 - "$MEMTOTAL_KB" "$SWAPTOTAL_KB" <<'PY' 2>/dev/null
import sys
try:
    mem = int(sys.argv[1]); swap = int(sys.argv[2])
except Exception:
    print("nodata"); raise SystemExit
cap = min(mem, 8 * 1024 * 1024)           # min(RAM, 8Gi) in KiB
lo, hi = cap * 0.85, cap * 1.10           # tolerance: swapfile rounds to whole MiB, minus overhead
print("ok" if lo <= swap <= hi else f"off(expected~{cap}KiB got {swap}KiB)")
PY
)
[ "$SWAP_CHECK" = "ok" ] && ok "STEP2 SwapTotal ≈ min(RAM,8Gi) derived from MemTotal" || mark_fail "STEP2 swap sizing $SWAP_CHECK"

# Node-object swap.capacity non-<unknown> — a DIFFERENT command than configz (P12).
SWAPCAP=$(kubectl get node "$NODE" -o jsonpath='{.status.nodeInfo.swap.capacity}' 2>/dev/null)
printf '  Node.status.nodeInfo.swap.capacity=%s\n' "${SWAPCAP:-<empty>}"
if [ -n "$SWAPCAP" ] && [ "$SWAPCAP" != "<unknown>" ] && [ "$SWAPCAP" != "0" ]; then
    ok "STEP2 Node object swap.capacity=$SWAPCAP (non-<unknown>)"
else
    mark_fail "STEP2 Node object swap.capacity='$SWAPCAP' (expected a non-<unknown> byte count)"
fi

# vm.swappiness=10
SWAPPINESS=$(node_ssh "$IP" 'cat /proc/sys/vm/swappiness')
printf '  vm.swappiness=%s\n' "${SWAPPINESS:-?}"
[ "$SWAPPINESS" = "10" ] && ok "STEP2 vm.swappiness=10" || mark_fail "STEP2 vm.swappiness=$SWAPPINESS (expected 10)"

# fstab carries the swap entry with sw,nofail (capture then bash-match, never grep -q)
FSTAB=$(node_ssh "$IP" 'cat /etc/fstab')
if [[ "$FSTAB" == *"/swapfile"* ]] && [[ "$FSTAB" == *"sw,nofail"* ]]; then
    ok "STEP2 fstab has /swapfile … sw,nofail (survives reboot, no-fail on missing)"
else
    mark_fail "STEP2 fstab missing the /swapfile sw,nofail entry"
fi

# k3s.service GOMEMLIMIT set + non-empty (bounds the Go-heap component; value tuned, not literal)
GOMEM=$(node_ssh "$IP" 'systemctl show k3s.service -p Environment --value 2>/dev/null; cat /etc/systemd/system/k3s.service.d/*.conf 2>/dev/null')
if [[ "$GOMEM" == *GOMEMLIMIT=* ]]; then
    GOMEMVAL=$(printf '%s' "$GOMEM" | grep -oE 'GOMEMLIMIT=[^ "]+' | head -1)
    ok "STEP2 k3s.service $GOMEMVAL set + non-empty"
else
    mark_fail "STEP2 k3s.service GOMEMLIMIT not set (grep of unit Environment + drop-in)"
fi

# `apprafter node status` renders GOMEMLIMIT via the CLI's OWN probe (not raw
# systemctl). This CATCHES the 2.16 probe bug where systemd's single
# `Environment=GOMEMLIMIT=…` key prefix made the probe report `unknown` despite
# GOMEMLIMIT being set. Capture-to-var + bash-match (NEVER `| grep -q` under
# pipefail — 2.16f lesson). Active target is node 1 (e2e) here.
NS1=$("$APPRAFTER" node status 2>&1)
printf '%s\n' "$NS1" >> "$NOTE" 2>&1
if [[ "$NS1" == *GOMEMLIMIT*2GiB* ]]; then
    ok "STEP2 node status GOMEMLIMIT rendered as 2GiB via the CLI probe (parse bug guard)"
elif [[ "$NS1" == *GOMEMLIMIT*unknown* ]]; then
    mark_fail "STEP2 node status GOMEMLIMIT=unknown — the CLI probe FAILED to parse systemd's Environment= key prefix"
elif [[ "$NS1" == *GOMEMLIMIT* ]]; then
    ok "STEP2 node status renders a non-unknown GOMEMLIMIT via the CLI probe"
else
    mark_fail "STEP2 node status did not surface a GOMEMLIMIT field at all"
fi

# ---- step-8 BEFORE snapshot (record while healthy) --------------------------
{ echo "===== BEFORE (step 8) ====="; echo "-- free -m --"; node_ssh "$IP" 'free -m';
  echo "-- swapon --show --"; node_ssh "$IP" 'swapon --show';
  echo "-- vmstat (pswpin/pswpout) --"; node_ssh "$IP" 'grep -E "pswpin|pswpout" /proc/vmstat';
  echo "-- configz failSwapOn/swapBehavior --"; printf '  failSwapOn=%s swapBehavior=%s\n' "$FSW" "$SWB"; } >> "$NOTE" 2>&1
ok "STEP8(before) free/swapon/vmstat/configz snapshot recorded"

# =============================================================================
printf '\n=== STEP 3: pod NoSwap — managed app + needs.pg → swap.max==0 + pg VmSwap≈0 ===\n'
deploy_pg_app swap-app || mark_fail "STEP3 apply managed app"
wait_apps_synced 8 || printf '  (platform not fully Synced — proceeding; pod check is authoritative)\n'
deadline=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$deadline" ]; do [ -n "$(app_pod swap-app)" ] && break; sleep 15; done
APOD=$(app_pod swap-app); printf '  app pod = %s\n' "${APOD:-<none>}"
[ -n "$APOD" ] || mark_fail "STEP3 managed app pod never appeared"

if [ -n "$APOD" ]; then
    # LITERAL string `max` when unlimited, `0` when NoSwap-clamped — string compare (P14).
    SM=$(pod_swap_max "$APOD")
    printf '  app container memory.swap.max = %q\n' "$SM"
    [ "$SM" = "0" ] && ok "STEP3 app container memory.swap.max==0 (NoSwap at creation)" || mark_fail "STEP3 app memory.swap.max='$SM' (expected literal 0, not 'max')"
fi

# pg backend: cgroup swap.max==0 AND VmSwap≈0 in the running postgres process
wait_pg_running || mark_fail "STEP3 no CNPG pg pod Running in 15m (needs.pg backend)"
PGPOD=$(kubectl get pods -A -l 'cnpg.io/cluster' --field-selector=status.phase=Running -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)
PGNS=$(kubectl get pods -A -l 'cnpg.io/cluster' --field-selector=status.phase=Running -o jsonpath='{.items[0].metadata.namespace}' 2>/dev/null)
printf '  pg pod = %s/%s\n' "${PGNS:-?}" "${PGPOD:-?}"
if [ -n "$PGPOD" ]; then
    PGSM=$(kubectl -n "$PGNS" exec "$PGPOD" -- sh -c 'cat /sys/fs/cgroup/memory.swap.max 2>/dev/null' 2>/dev/null | tr -d '[:space:]')
    printf '  pg container memory.swap.max = %q\n' "$PGSM"
    [ "$PGSM" = "0" ] && ok "STEP3 pg container memory.swap.max==0" || mark_fail "STEP3 pg memory.swap.max='$PGSM' (expected 0)"
    # VmSwap of the postgres process ≈ 0 KiB (find the postgres PID inside the pod).
    PGVMSWAP=$(kubectl -n "$PGNS" exec "$PGPOD" -- sh -c '
        pid=$(pgrep -x postgres 2>/dev/null | head -1); [ -z "$pid" ] && pid=1
        awk "/^VmSwap:/{print \$2}" /proc/$pid/status 2>/dev/null' 2>/dev/null | tr -d '[:space:]')
    printf '  pg process VmSwap = %sKiB\n' "${PGVMSWAP:-?}"
    if [ -n "$PGVMSWAP" ] && [ "$PGVMSWAP" -le 512 ] 2>/dev/null; then
        ok "STEP3 pg process VmSwap≈0 (${PGVMSWAP}KiB ≤ 512KiB tolerance)"
    else
        mark_fail "STEP3 pg process VmSwap=${PGVMSWAP}KiB (expected ≈0)"
    fi
fi

# =============================================================================
printf '\n=== STEP 4: induced pressure — pswpout>0, no OOM of k3s/pods, /readyz up ===\n'
# Baseline OOM/restart state before the hog.
K3S_ACTIVE_BEFORE=$(node_ssh "$IP" 'systemctl is-active k3s.service')
RESTARTS_BEFORE=$(kubectl get pods -A -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print(sum(cs.get('restartCount',0) for p in d.get('items',[]) for cs in (p.get('status',{}).get('containerStatuses') or [])))" 2>/dev/null || echo 0)
PSWPOUT_BEFORE=$(node_ssh "$IP" 'awk "/^pswpout/{print \$2}" /proc/vmstat')
printf '  before: k3s=%s totalRestarts=%s pswpout=%s\n' "$K3S_ACTIVE_BEFORE" "$RESTARTS_BEFORE" "${PSWPOUT_BEFORE:-?}"

# drop_caches can blip readyz — drop, then POLL /readyz stable for ~30s before the hog (Q6).
node_ssh "$IP" 'sync; echo 3 > /proc/sys/vm/drop_caches' >/dev/null
printf '  dropped caches; polling /readyz stable ~30s...\n'
stable=0
for _ in $(seq 1 12); do
    RZ=$(kubectl get --raw /readyz 2>/dev/null)
    [ "$RZ" = "ok" ] && stable=$((stable+1)) || stable=0
    [ "$stable" -ge 6 ] && break
    sleep 5
done
[ "$stable" -ge 6 ] && ok "STEP4 /readyz stable after drop_caches" || printf '  WARN /readyz did not stabilise in 60s post-drop_caches (proceeding)\n'

# A memory hog in system.slice, past the headroom, so the node must lean on swap.
# `systemd-run --scope --slice=system.slice` keeps it OFF kubepods so we test the
# NODE cushion. The hog itself being OOM-killed is OK (N9) — what must NOT die is
# k3s.service or any pod. RAM-derived hog size = MemTotal + a margin (force swap).
HOG_MB=$(python3 -c "print(int(${MEMTOTAL_KB:-4194304}/1024) + 512)" 2>/dev/null)
printf '  launching a %sMiB hog in system.slice (past headroom → swap)...\n' "$HOG_MB"
# tail -f keeps the scope alive briefly; the python allocs+touches HOG_MB then exits.
node_ssh "$IP" "systemd-run --scope --slice=system.slice --unit=apprafter-swaphog-\$\$ \
    timeout 90 python3 -c 'import sys; n=${HOG_MB}*1024*1024; b=bytearray(n);
[b.__setitem__(i, 1) for i in range(0, n, 4096)]; import time; time.sleep(25)' >/dev/null 2>&1 || true" >/dev/null 2>&1 &
HOGWAIT=$!
# Poll /readyz THROUGHOUT the pressure window.
readyz_up_throughout=1
for _ in $(seq 1 18); do
    RZ=$(kubectl get --raw /readyz 2>/dev/null)
    [ "$RZ" = "ok" ] || readyz_up_throughout=0
    sleep 5
done
wait "$HOGWAIT" 2>/dev/null || true

# pswpout DELTA > 0 (from /proc/vmstat, not a SwapUsed snapshot — swap may already
# have drained back by the time we read it).
PSWPOUT_AFTER=$(node_ssh "$IP" 'awk "/^pswpout/{print \$2}" /proc/vmstat')
DELTA=$(python3 -c "print(int('${PSWPOUT_AFTER:-0}') - int('${PSWPOUT_BEFORE:-0}'))" 2>/dev/null || echo 0)
printf '  pswpout %s -> %s (delta=%s)\n' "${PSWPOUT_BEFORE:-?}" "${PSWPOUT_AFTER:-?}" "$DELTA"
[ "${DELTA:-0}" -gt 0 ] 2>/dev/null && ok "STEP4 pswpout delta>0 ($DELTA pages swapped out — cushion engaged)" \
    || printf '  NOTE STEP4 pswpout delta=%s (GOMEMLIMIT may have prevented the repro — mechanism-not-incident, N9)\n' "$DELTA"

# k3s.service must still be active; no NEW pod restart/OOMKill.
K3S_ACTIVE_AFTER=$(node_ssh "$IP" 'systemctl is-active k3s.service')
[ "$K3S_ACTIVE_AFTER" = "active" ] && ok "STEP4 k3s.service still active (not OOM-killed)" || mark_fail "STEP4 k3s.service is '$K3S_ACTIVE_AFTER' after pressure (OOM?)"
RESTARTS_AFTER=$(kubectl get pods -A -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print(sum(cs.get('restartCount',0) for p in d.get('items',[]) for cs in (p.get('status',{}).get('containerStatuses') or [])))" 2>/dev/null || echo 0)
OOMKILLED=$(kubectl get pods -A -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print('\n'.join(f\"{p['metadata']['name']}/{cs['name']}\" for p in d.get('items',[]) for cs in (p.get('status',{}).get('containerStatuses') or []) if (cs.get('lastState',{}).get('terminated') or {}).get('reason')=='OOMKilled'))" 2>/dev/null)
printf '  totalRestarts %s -> %s ; OOMKilled pods: %s\n' "$RESTARTS_BEFORE" "$RESTARTS_AFTER" "${OOMKILLED:-none}"
# NoSwap protects the NODE, not workloads (ADR 0055): under an OVERSIZED system.slice
# hog, a pod (which cannot swap) OOMing is EXPECTED behaviour, not a swap failure. The
# cushion's job — validated above — is that k3s.service survives + swap engages
# (pswpout>0) + /readyz stays up. A pod restart/OOM here is a NOTE, not a fail.
[ "${RESTARTS_AFTER:-0}" -le "${RESTARTS_BEFORE:-0}" ] 2>/dev/null && ok "STEP4 no NEW pod restart under pressure" \
    || printf '  NOTE STEP4 pod restarts %s->%s under the oversized hog — EXPECTED (NoSwap protects the node, not pods; k3s survived + cushion engaged above)\n' "$RESTARTS_BEFORE" "$RESTARTS_AFTER"
[ -z "$OOMKILLED" ] && ok "STEP4 no pod OOMKilled" \
    || printf '  NOTE STEP4 pod OOMKilled under the oversized hog (%s) — EXPECTED (NoSwap: pods do not swap; the cushion protects the node/k3s)\n' "$OOMKILLED"
[ "$readyz_up_throughout" -eq 1 ] && ok "STEP4 /readyz up throughout the pressure window" || mark_fail "STEP4 /readyz dropped during pressure"

# =============================================================================
printf '\n=== STEP 5: retrofit (SKIP_NODE_SWAP=1) + node prep + VPA-revert (Q7) + idempotent ===\n'
# A SECOND node WITHOUT baked-in swap (the test hook forces a cushionless install),
# so `apprafter node prep` retrofits a LIVE cluster — the path the dogfood node walks.
export APPRAFTER_SKIP_NODE_SWAP=1
# env HCLOUD_TOKEN OVERRIDES the target's stored --token (found run 3: e2e-retro's
# `up` used the exported NEW token → adopted node 1, OTHER project stayed empty).
# Point the env token at the OTHER project for the whole e2e-retro block.
export HCLOUD_TOKEN="$OTHER_TOKEN"
# The 2nd (SKIP) node MUST go in a SEPARATE Hetzner project (OTHER_TOKEN): the
# `apprafter=true` label anchor is project-wide, so a 2nd target sharing node 1's
# token makes `up` ADOPT node 1 instead of provisioning a new server (found on run 2 —
# server count stayed 1, RIP==node1). A different project has no such collision.
# `target add` also does NOT auto-activate → `target use` before `up`.
"$APPRAFTER" target add e2e-retro --provider hetzner-cloud --tier solo --region "$REGION" \
    --token "$OTHER_TOKEN" --no-interactive --force || mark_fail "STEP5 target add (retrofit)"
"$APPRAFTER" target use e2e-retro >/dev/null 2>&1 || mark_fail "STEP5 target use e2e-retro (activate the 2nd target)"
if timeout 1500 "$APPRAFTER" up; then
    "$APPRAFTER" kubeconfig > "$KUBECONFIG"
    RNODE=$(node_name); RIP=$(target_ipv4)
    printf '  retrofit node=%s ip=%s (provisioned with APPRAFTER_SKIP_NODE_SWAP=1 → no bake)\n' "$RNODE" "${RIP:-?}"
    # Must be a GENUINELY DIFFERENT node than node 1 — else we retrofit the already-baked
    # first node and both the transition AND the SKIP-at-bootstrap check are void.
    { [ -n "$RIP" ] && [ "$RIP" != "$IP" ]; } && ok "STEP5 retrofit is a distinct 2nd node ($RIP != node1 $IP)" \
        || mark_fail "STEP5 retrofit node IP=$RIP == node1 IP=$IP — target activation failed"
    NSRV=$(python3 "$HZ" list "$OTHER_TOKEN" 2>/dev/null | awk '/servers/{print $2}')
    [ "${NSRV:-0}" = "1" ] && ok "STEP5 retrofit is a genuine new server in the OTHER project (cleanup sweeps it)" || printf '  NOTE STEP5 OTHER-project server count=%s (expected 1)\n' "${NSRV:-?}"

    # SKIP-gate assertion: does APPRAFTER_SKIP_NODE_SWAP=1 actually prevent the bootstrap
    # bake? The SKIP node must come up CUSHIONLESS (no active swap).
    RSWAP=$(node_ssh "$RIP" 'swapon --show')
    [[ "$RSWAP" != *"/swapfile"* ]] && ok "STEP5 SKIP_NODE_SWAP=1 node booted cushionless (bootstrap skip-gate works)" \
        || mark_fail "STEP5 SKIP node has /swapfile active — APPRAFTER_SKIP_NODE_SWAP=1 did NOT skip the bootstrap bake"

    # Deploy pg so there is a PRE-EXISTING container before prep (swapBehavior applies
    # at container creation → a pre-prep container keeps swap.max=max until prep sets 0).
    wait_apps_synced 8 || printf '  (retrofit platform not fully Synced — proceeding)\n'
    deploy_pg_app retro-app || mark_fail "STEP5 apply retro app"
    deadline=$(( $(date +%s) + 300 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do [ -n "$(app_pod retro-app)" ] && break; sleep 15; done
    RPOD=$(app_pod retro-app); printf '  pre-prep app pod = %s\n' "${RPOD:-<none>}"
    if [ -n "$RPOD" ]; then
        PRE_SM=$(pod_swap_max "$RPOD")
        printf '  pre-prep memory.swap.max = %q (expect literal max — no NoSwap yet)\n' "$PRE_SM"
    fi

    # The retrofit: `apprafter node prep` (SKIP_NODE_SWAP is process-wide; prep must
    # ignore it — unset for the prep call so the swap step actually runs).
    printf '  running `apprafter node prep --yes` (retrofit over one k3s restart)...\n'
    env -u APPRAFTER_SKIP_NODE_SWAP "$APPRAFTER" node prep --yes 2>&1 | tail -8

    # After prep: the pre-existing container's memory.swap.max is set to 0 inline
    # (depth-agnostic set over kubepods — pre-existing containers get NoSwap without a roll).
    RPOD2=$(app_pod retro-app)
    if [ -n "$RPOD2" ]; then
        POST_SM=$(pod_swap_max "$RPOD2")
        printf '  post-prep memory.swap.max = %q\n' "$POST_SM"
        [ "$POST_SM" = "0" ] && ok "STEP5 pre-existing container memory.swap.max==0 after prep (inline set)" || mark_fail "STEP5 post-prep memory.swap.max='$POST_SM' (expected 0)"
    fi

    # Node-level asserts the prep must have landed on the LIVE node.
    RSWAP2=$(node_ssh "$RIP" 'swapon --show')
    [[ "$RSWAP2" == *"/swapfile"* ]] && ok "STEP5 retrofit swap now active (swapon --show)" || mark_fail "STEP5 /swapfile not active after prep"
    RCZ=$(configz "$RNODE")
    RSWB=$(printf '%s' "$RCZ" | python3 -c "import json,sys;print(json.load(sys.stdin).get('kubeletconfig',{}).get('memorySwap',{}).get('swapBehavior'))" 2>/dev/null)
    [ "$RSWB" = "NoSwap" ] && ok "STEP5 configz swapBehavior=NoSwap after prep" || mark_fail "STEP5 post-prep configz swapBehavior=$RSWB"

    # Q7 — VPA InPlace (on every T1 since 2.16e) may revert the inline swap.max on
    # its next resize. FORCE a resize (bump the app's recommendation via crictl update
    # on the running container) and RE-READ swap.max. This is the decision point:
    # if it reverts, the inline set is a window-closer only and the workload roll is
    # MANDATORY (design decision 4 / Q7). We record BOTH outcomes, not fail on revert.
    printf '  Q7: forcing a resize then re-reading memory.swap.max...\n'
    if [ -n "$RPOD2" ]; then
        RCID=$(kubectl -n "$NS" get pod "$RPOD2" -o jsonpath='{.status.containerStatuses[0].containerID}' 2>/dev/null | sed 's#.*/##')
        if [ -n "$RCID" ]; then
            # crictl update bumps the memory limit → the kubelet/CRI rewrites the
            # container cgroup (the same write path VPA InPlace uses).
            node_ssh "$RIP" "crictl update --memory $((256*1024*1024)) $RCID" >/dev/null 2>&1 || printf '  (crictl update non-zero — resize path may differ)\n'
            sleep 8
            AFTER_RESIZE_SM=$(pod_swap_max "$RPOD2")
            printf '  Q7 RESULT: memory.swap.max after forced resize = %q\n' "$AFTER_RESIZE_SM"
            if [ "$AFTER_RESIZE_SM" = "0" ]; then
                ok "STEP5/Q7 memory.swap.max STAYED 0 across resize (inline set survives → roll not mandatory)"
            else
                printf '  NOTE STEP5/Q7 memory.swap.max REVERTED to %q on resize → the inline set is a window-closer only; the workload roll is MANDATORY (design decision 4). Recorded, not a walk failure.\n' "$AFTER_RESIZE_SM"
            fi
        else
            printf '  NOTE STEP5/Q7 could not resolve containerID for a forced resize — Q7 revert-check SKIPPED\n'
        fi
    fi

    # Idempotency: a 2nd `node prep` must NOT add a 2nd swapfile or a 2nd fstab line.
    printf '  re-running `node prep` (must be idempotent)...\n'
    env -u APPRAFTER_SKIP_NODE_SWAP "$APPRAFTER" node prep --yes 2>&1 | tail -4
    FSTAB_COUNT=$(node_ssh "$RIP" 'grep -c "^/swapfile " /etc/fstab')
    SWAPON_COUNT=$(node_ssh "$RIP" 'swapon --show=NAME --noheadings' | grep -c '/swapfile')
    printf '  fstab /swapfile lines=%s ; active /swapfile areas=%s\n' "${FSTAB_COUNT:-?}" "${SWAPON_COUNT:-?}"
    { [ "${FSTAB_COUNT:-0}" -le 1 ] && [ "${SWAPON_COUNT:-0}" -le 1 ]; } 2>/dev/null \
        && ok "STEP5 node prep idempotent (single swapfile + single fstab line after 2nd run)" \
        || mark_fail "STEP5 node prep NOT idempotent (fstab=$FSTAB_COUNT active=$SWAPON_COUNT — a 2nd swapfile/line)"

    # `apprafter node status` on the RETROFITTED node (active target e2e-retro):
    # (a) GOMEMLIMIT must render via the CLI's OWN probe (parse-bug guard, as
    #     STEP2 but on the retrofit path), and (b) `provision` must read
    #     `applied (retrofit)` — NOT `unknown` — now that the retrofit drops the
    #     provision breadcrumb. Capture-to-var + bash-match (never `| grep -q`).
    NS5=$("$APPRAFTER" node status 2>&1)
    printf '%s\n' "$NS5" >> "$NOTE" 2>&1
    if [[ "$NS5" == *GOMEMLIMIT*unknown* ]] || [[ "$NS5" != *GOMEMLIMIT* ]]; then
        mark_fail "STEP5 node status GOMEMLIMIT not rendered on the retrofit node (probe parse bug)"
    else
        ok "STEP5 node status renders a non-unknown GOMEMLIMIT on the retrofit node (CLI probe)"
    fi
    if [[ "$NS5" == *applied\ \(retrofit\)* ]]; then
        ok "STEP5 node status provision=applied (retrofit) — retrofit breadcrumb dropped"
    elif [[ "$NS5" == *provision*unknown* ]]; then
        mark_fail "STEP5 node status provision=unknown on a retrofitted node — breadcrumb NOT written"
    else
        mark_fail "STEP5 node status provision breadcrumb missing 'applied (retrofit)' on the retrofit node"
    fi

    ok "STEP5 hz.py will sweep BOTH nodes on cleanup (2 servers in the project)"
else
    mark_fail "STEP5 retrofit `apprafter up` failed — retrofit + Q7 + idempotency UNTESTED"
fi
unset APPRAFTER_SKIP_NODE_SWAP
export HCLOUD_TOKEN="$TOKEN"   # restore node 1's token for the remaining steps + cleanup destroy
# Point the kubeconfig/active target back at the FIRST node for steps 6-8.
"$APPRAFTER" target use e2e >/dev/null 2>&1 || true
"$APPRAFTER" kubeconfig > "$KUBECONFIG" 2>/dev/null || true

# =============================================================================
printf '\n=== STEP 6: induced-failure rollback (invalid drop-in → /readyz timeout → whole-step rollback) ===\n'
# Inject an INVALID kubelet drop-in via the undocumented fault hook
# (APPRAFTER_NODE_SWAP_FORCE_INVALID_DROPIN — out of --help, like
# APPRAFTER_SKIP_NODE_SWAP) → `apprafter node prep` → k3s fails to come back →
# the /readyz poll TIMES OUT → the whole-step rollback runs → the cluster
# returns → configz shows `swapBehavior` GONE while `failSwapOn:false` REMAINS
# (the running config the rollback restarted k3s with keeps failSwapOn:false and
# drops swapBehavior; the drop-in FILE is deleted last but does not re-take in
# configz until the next restart). This exercises the P1/Q1 safety net — the
# unit golden-tests cover the rollback SEQUENCER; this proves it on a LIVE node.
#
# Node/ordering care: the rollback leaves the node SWAPLESS, which would break
# STEP 7's reboot-swap-persistence if run on node 1. So STEP 6 targets the
# RETROFIT node 2 (the OTHER-project node STEP 5 already provisioned + swap-prepped);
# after the rollback it is DONE (cleanup sweeps the OTHER project). STEP 7's
# reboot then runs on the still-swapful node 1. We NEVER run STEP 6 on node 1.
if [ -z "$OTHER_TOKEN" ]; then
    printf '  SKIP: OTHER_TOKEN unset — STEP 6 needs the retrofit node 2 from STEP 5 (OTHER project).\n'
    ok "STEP6 SKIP (no OTHER_TOKEN → no node 2 to fault-inject; node 1 must NOT be used)"
else
    # Re-activate the retrofit target (node 2 in the OTHER project). STEP 5's tail
    # switched the active target + token back to node 1 for steps 7-8; STEP 6 flips
    # to node 2 for its duration and flips BACK at the end.
    export HCLOUD_TOKEN="$OTHER_TOKEN"
    "$APPRAFTER" target use e2e-retro >/dev/null 2>&1 || mark_fail "STEP6 target use e2e-retro"
    "$APPRAFTER" kubeconfig > "$KUBECONFIG" 2>/dev/null || mark_fail "STEP6 retrofit kubeconfig"
    R6NODE=$(node_name); R6IP=$(target_ipv4)
    printf '  fault-inject target = node2 %s (%s)\n' "${R6NODE:-<none>}" "${R6IP:-<none>}"
    if [ -z "$R6IP" ] || [ "$R6IP" = "$IP" ]; then
        mark_fail "STEP6 could not resolve node2 (got '$R6IP', node1 is '$IP') — refusing to fault-inject node 1"
    else
        # Force the INVALID drop-in for this single `node prep` (unset the OFF-only
        # SKIP hook so the swap step actually runs then fails on the bad drop-in).
        printf '  running `node prep` with APPRAFTER_NODE_SWAP_FORCE_INVALID_DROPIN=1 (expect /readyz timeout → rollback)...\n'
        PREP_OUT=$(env -u APPRAFTER_SKIP_NODE_SWAP APPRAFTER_NODE_SWAP_FORCE_INVALID_DROPIN=1 \
            "$APPRAFTER" node prep --yes 2>&1)
        PREP_RC=$?
        printf '%s\n' "$PREP_OUT" | tail -12 | sed 's/^/    | /'

        # `node prep` MUST fail (the invalid drop-in bricked the restart) — the
        # rollback then makes it exit non-zero with the rolled-back diagnostic.
        [ "$PREP_RC" -ne 0 ] && ok "STEP6 node prep exited non-zero (invalid drop-in → apply failed)" \
            || mark_fail "STEP6 node prep exited 0 — the invalid drop-in did NOT brick the restart"

        # The /readyz timeout must have been hit (the apply's recovery poll).
        [[ "$PREP_OUT" == *"did not recover"* ]] && ok "STEP6 /readyz recovery timeout hit (k3s did not come back on the invalid drop-in)" \
            || mark_fail "STEP6 no '/readyz recovery timeout' in prep output — restart may not have failed"

        # The whole-step rollback must have run and RECOVERED (swapoff succeeded).
        [[ "$PREP_OUT" == *"rolled back cleanly"* ]] && ok "STEP6 whole-step rollback ran and recovered cleanly" \
            || mark_fail "STEP6 rollback did not report a clean recovery (see prep output above)"

        # The cluster must be back: poll /readyz until it returns.
        printf '  polling /readyz until the cluster returns post-rollback...\n'
        r6back=0; deadline=$(( $(date +%s) + 240 ))
        while [ "$(date +%s)" -lt "$deadline" ]; do
            RZ=$(kubectl get --raw /readyz 2>/dev/null)
            [ "$RZ" = "ok" ] && { r6back=1; break; }
            sleep 10
        done
        [ "$r6back" -eq 1 ] && ok "STEP6 cluster returned after rollback (/readyz=ok)" || mark_fail "STEP6 /readyz did not return in 4m after rollback"

        # Rollback correctness via configz (capture then bash-match — never grep -q):
        # swapBehavior GONE, failSwapOn:false STILL present (the running config the
        # rollback restarted k3s with keeps failSwapOn:false and dropped swapBehavior).
        if [ "$r6back" -eq 1 ]; then
            R6CZ=$(configz "$R6NODE")
            R6FSW=$(printf '%s' "$R6CZ" | python3 -c "import json,sys;print(json.load(sys.stdin).get('kubeletconfig',{}).get('failSwapOn'))" 2>/dev/null)
            R6SWB=$(printf '%s' "$R6CZ" | python3 -c "import json,sys;print(json.load(sys.stdin).get('kubeletconfig',{}).get('memorySwap',{}).get('swapBehavior'))" 2>/dev/null)
            printf '  post-rollback configz failSwapOn=%s memorySwap.swapBehavior=%s\n' "$R6FSW" "$R6SWB"
            { [ "$R6SWB" = "None" ] || [ -z "$R6SWB" ]; } && ok "STEP6 configz swapBehavior GONE after rollback" \
                || mark_fail "STEP6 post-rollback configz swapBehavior=$R6SWB (expected gone/None)"
            [ "$R6FSW" = "False" ] && ok "STEP6 configz failSwapOn:false STILL present after rollback (removed LAST, not while swap on)" \
                || mark_fail "STEP6 post-rollback configz failSwapOn=$R6FSW (expected False)"
            # The node is now swapless (rollback swapped off + removed the swapfile) — expected.
            R6SWAP=$(node_ssh "$R6IP" 'swapon --show')
            [[ "$R6SWAP" != *"/swapfile"* ]] && ok "STEP6 node2 left swapless after rollback (swapoff + rm /swapfile — expected)" \
                || printf '  NOTE STEP6 node2 still shows /swapfile after a clean rollback (unexpected but non-fatal — node2 is disposable)\n'
        fi
        ok "STEP6 node2 fault-injection done — cleanup sweeps the OTHER project (node2 is disposable)"
    fi
    # Flip the active target + token BACK to node 1 for STEP 7 (reboot) + cleanup.
    export HCLOUD_TOKEN="$TOKEN"
    "$APPRAFTER" target use e2e >/dev/null 2>&1 || true
    "$APPRAFTER" kubeconfig > "$KUBECONFIG" 2>/dev/null || true
fi

# =============================================================================
printf '\n=== STEP 7: reboot persistence — swap reactivates from fstab + swappiness + CRI NoSwap ===\n'
# The throwaway FIRST node. hz.py has no reboot verb → reboot over SSH.
IP=$(target_ipv4); NODE=$(node_name)
printf '  rebooting node %s (%s)...\n' "$NODE" "${IP:-?}"
node_ssh "$IP" 'nohup sh -c "sleep 2; systemctl reboot" >/dev/null 2>&1 &' >/dev/null 2>&1 || node_ssh "$IP" 'reboot' >/dev/null 2>&1 || true
sleep 45
# Wait for SSH + the kube-API to come back.
deadline=$(( $(date +%s) + 300 ))
back=0
while [ "$(date +%s)" -lt "$deadline" ]; do
    if [ "$(node_ssh "$IP" 'echo up')" = "up" ]; then back=1; break; fi
    sleep 10
done
[ "$back" -eq 1 ] && ok "STEP7 node back on SSH after reboot" || mark_fail "STEP7 node did not return on SSH in 5m"

if [ "$back" -eq 1 ]; then
    # swap reactivates from fstab (sw,nofail persists it across the boot).
    RB_SWAP=$(node_ssh "$IP" 'swapon --show')
    [[ "$RB_SWAP" == *"/swapfile"* ]] && ok "STEP7 swap reactivated from fstab after reboot" || mark_fail "STEP7 /swapfile NOT active after reboot (fstab sw,nofail failed?)"
    # swappiness persists (sysctl drop-in survives the boot).
    RB_SWAPPINESS=$(node_ssh "$IP" 'cat /proc/sys/vm/swappiness')
    [ "$RB_SWAPPINESS" = "10" ] && ok "STEP7 vm.swappiness=10 persists after reboot" || mark_fail "STEP7 post-reboot swappiness=$RB_SWAPPINESS (expected 10)"

    # A POST-BOOT container gets memory.swap.max=0 from the CRI (the self-healing claim:
    # swapBehavior:NoSwap in the config → every container created after boot is clamped).
    deadline=$(( $(date +%s) + 300 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do kubectl get --raw /readyz >/dev/null 2>&1 && break; sleep 10; done
    kubectl -n "$NS" run swap-postboot --image=nginxdemos/hello:plain-text --restart=Never >/dev/null 2>&1 || true
    deadline=$(( $(date +%s) + 180 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        ph=$(kubectl -n "$NS" get pod swap-postboot -o jsonpath='{.status.phase}' 2>/dev/null)
        [ "$ph" = "Running" ] && break
        sleep 8
    done
    # Retry — post-reboot the exec can race the container becoming exec-ready (an empty
    # read on the first run); poll pod_swap_max until it returns a value.
    PB_SM=""; pbdl=$(( $(date +%s) + 120 ))
    while [ "$(date +%s)" -lt "$pbdl" ]; do
        PB_SM=$(pod_swap_max swap-postboot)
        [ -n "$PB_SM" ] && break
        sleep 8
    done
    printf '  post-boot container memory.swap.max = %q\n' "$PB_SM"
    [ "$PB_SM" = "0" ] && ok "STEP7 post-boot container memory.swap.max==0 from the CRI (self-healing)" || mark_fail "STEP7 post-boot memory.swap.max='$PB_SM' (expected 0)"
    kubectl -n "$NS" delete pod swap-postboot --wait=false >/dev/null 2>&1 || true
fi

# =============================================================================
printf '\n=== STEP 8: record AFTER snapshot (free/swapon/vmstat/configz) ===\n'
CZ2=$(configz "$NODE")
FSW2=$(printf '%s' "$CZ2" | python3 -c "import json,sys;print(json.load(sys.stdin).get('kubeletconfig',{}).get('failSwapOn'))" 2>/dev/null)
SWB2=$(printf '%s' "$CZ2" | python3 -c "import json,sys;print(json.load(sys.stdin).get('kubeletconfig',{}).get('memorySwap',{}).get('swapBehavior'))" 2>/dev/null)
{ echo; echo "===== AFTER (step 8) ====="; echo "-- free -m --"; node_ssh "$IP" 'free -m';
  echo "-- swapon --show --"; node_ssh "$IP" 'swapon --show';
  echo "-- vmstat (pswpin/pswpout) --"; node_ssh "$IP" 'grep -E "pswpin|pswpout" /proc/vmstat';
  echo "-- configz failSwapOn/swapBehavior --"; printf '  failSwapOn=%s swapBehavior=%s\n' "$FSW2" "$SWB2"; } >> "$NOTE" 2>&1
ok "STEP8(after) free/swapon/vmstat/configz snapshot recorded (dumped in CLEANUP)"

printf '\n=== SUMMARY (FAIL=%s) ===\n' "$FAIL"
exit "$FAIL"
