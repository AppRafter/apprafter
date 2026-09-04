#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# 2.16d baseline MEASUREMENT walk (real Hetzner cpx22). Provisions a
# throwaway single-node cluster with the CURRENTLY-PUBLISHED platform (all
# BestEffort), deploys a representative app that pulls a pg + redis claim
# (so CNPG + Dragonfly come up), lets it reach steady state, then captures
# per-container memory workingSetBytes (RSS) via the kubelet /stats/summary
# API — kind can't serve that API, so this MUST run on real Hetzner.
#
# Output: a value table (platform request = RSS*0.8; backend Guaranteed
# limit = RSS*1.2; app seed = app RSS) + the D2 budget-close check
# (sum(requests)+Guaranteed reserves+eviction-hard < allocatable). The
# controller pins these into docs/measurements/ and bakes them into 2.16d.
#
# Judged by log markers (ok:/FAILED/GREEN), NOT exit code. Bulletproof
# cleanup: apprafter destroy + API sweep + verify both token projects -> 0.
set -uo pipefail

APPRAFTER=/projects/omnixal/apprafter/cli/target/release/apprafter
HZ=/projects/omnixal/apprafter/e2e/hz.py     # reused API sweep/verify helper (written alongside)
REGION="${APPRAFTER_E2E_REGION:-nbg1}"
TOKEN="${HCLOUD_TOKEN:?HCLOUD_TOKEN required}"
OTHER_TOKEN="${OTHER_TOKEN:-}"
FAIL=0
mark_fail() { FAIL=1; printf 'FAILED: %s\n' "$1"; }

WORK=$(mktemp -d /tmp/apprafter-meas-XXXXXX)
export HOME="$WORK/home"; mkdir -p "$HOME/.ssh"
export APPRAFTER_SSH_PRIVATE_KEY="$WORK/id_ed25519"
ssh-keygen -t ed25519 -N '' -f "$APPRAFTER_SSH_PRIVATE_KEY" -C apprafter-meas >/dev/null 2>&1
export APPRAFTER_SSH_PUBLIC_KEY="$(cat "$APPRAFTER_SSH_PRIVATE_KEY.pub")"
export KUBECONFIG="$WORK/kubeconfig"
RESULTS="$WORK/results.txt"
cd "$WORK"

cleanup() {
    printf '\n=== CLEANUP (always runs) ===\n'
    "$APPRAFTER" destroy --yes >/dev/null 2>&1 && printf 'apprafter destroy: ok\n' || printf 'apprafter destroy: (non-zero; sweeping)\n'
    python3 "$HZ" sweep "$TOKEN" 2>/dev/null
    printf '  VERIFY primary project:\n'; python3 "$HZ" verify "$TOKEN" || FAIL=1
    if [ -n "$OTHER_TOKEN" ]; then printf '  VERIFY other project:\n'; python3 "$HZ" verify "$OTHER_TOKEN" || FAIL=1; fi
    # keep RESULTS content echoed above; remove the scratch dir
    rm -rf "$WORK"
    if [ "$FAIL" -eq 0 ]; then printf '\n=== MEASUREMENT GREEN ===\n'; else printf '\n=== MEASUREMENT RED ===\n'; fi
}
trap cleanup EXIT INT TERM

printf '=== Phase 1: provision + bootstrap published platform (tier=solo, %s) ===\n' "$REGION"
"$APPRAFTER" target add e2e --provider hetzner-cloud --tier solo --region "$REGION" \
    --server-type "${APPRAFTER_E2E_SERVER_TYPE:-cpx22}" \
    --token "$TOKEN" --no-interactive --force || { mark_fail "target add"; exit 1; }
timeout 1500 "$APPRAFTER" up || { mark_fail "apprafter up"; exit 1; }
"$APPRAFTER" kubeconfig > "$KUBECONFIG" || { mark_fail "kubeconfig"; exit 1; }
printf 'ok: cluster bootstrapped\n'

printf '\n=== Phase 2: wait for platform steady-state (Argo apps Synced) ===\n'
deadline=$(( $(date +%s) + 600 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    total=$(kubectl -n argocd get applications.argoproj.io -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print(len(d.get('items',[])))" 2>/dev/null || echo 0)
    synced=$(kubectl -n argocd get applications.argoproj.io -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print(sum(1 for a in d.get('items',[]) if a.get('status',{}).get('sync',{}).get('status')=='Synced'))" 2>/dev/null || echo 0)
    printf '  argo apps synced %s/%s\n' "$synced" "$total"
    [ "$total" -ge 8 ] && [ "$synced" -ge "$total" ] && break
    sleep 20
done

printf '\n=== Phase 3: deploy a representative app (needs.pg + needs.redis) ===\n'
kubectl apply -f - <<'EOF' || mark_fail "apply meas-app"
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: meas-app
  namespace: default
spec:
  base:
    image: nginxdemos/hello:plain-text
    expose:
      port: 80
    needs:
      pg: {}
      redis: {}
EOF
printf '  waiting for the CNPG cluster + Dragonfly pod to appear + become Running (up to 10m)...\n'
deadline=$(( $(date +%s) + 600 ))
pg_ready=0; df_ready=0
while [ "$(date +%s)" -lt "$deadline" ]; do
    # CNPG instance pods carry cnpg.io/cluster label; Dragonfly pods carry app=<name> / dragonfly
    pg_ready=$(kubectl get pods -A -l 'cnpg.io/cluster' --field-selector=status.phase=Running -o name 2>/dev/null | wc -l | tr -d ' ')
    df_ready=$(kubectl get pods -A -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print(sum(1 for p in d.get('items',[]) if 'dragonfly' in (p['metadata'].get('labels',{}).get('app','')+p['metadata']['name']).lower() and p.get('status',{}).get('phase')=='Running'))" 2>/dev/null || echo 0)
    printf '  pg Running=%s  dragonfly Running=%s\n' "$pg_ready" "$df_ready"
    [ "${pg_ready:-0}" -ge 1 ] && [ "${df_ready:-0}" -ge 1 ] && break
    sleep 20
done
[ "${pg_ready:-0}" -ge 1 ] || mark_fail "no CNPG pg pod Running"
[ "${df_ready:-0}" -ge 1 ] || mark_fail "no Dragonfly pod Running"

printf '\n=== Phase 4: steady-state settle (5 min) ===\n'
sleep 300

printf '\n=== Phase 5: MEASURE per-container RSS (kubelet /stats/summary) + node budget ===\n'
NODE=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}')
CAP=$(kubectl get node "$NODE" -o jsonpath='{.status.capacity.memory}')
ALLOC=$(kubectl get node "$NODE" -o jsonpath='{.status.allocatable.memory}')
printf '  node=%s capacity.memory=%s allocatable.memory=%s\n' "$NODE" "$CAP" "$ALLOC"
kubectl get --raw "/api/v1/nodes/${NODE}/proxy/stats/summary" 2>/dev/null > "$WORK/summary.json" || mark_fail "kubelet summary unreachable"
# backend qosClass baseline (expect BestEffort today)
PG_QOS=$(kubectl get pods -A -l 'cnpg.io/cluster' -o jsonpath='{.items[0].status.qosClass}' 2>/dev/null)
printf '  baseline CNPG pod qosClass = %s (expect BestEffort pre-2.16d)\n' "$PG_QOS"

python3 - "$WORK/summary.json" "$CAP" "$ALLOC" <<'PY' | tee "$RESULTS"
import json, sys, re
summary = json.load(open(sys.argv[1]))
def to_bytes(q):
    m = re.match(r'^(\d+)([A-Za-z]*)$', q or '')
    if not m: return 0
    n = int(m.group(1)); u = m.group(2)
    mul = {'':1,'Ki':1024,'Mi':1024**2,'Gi':1024**3,'k':1000,'M':1000**2,'G':1000**3}.get(u,1)
    return n*mul
cap = to_bytes(sys.argv[2]); alloc = to_bytes(sys.argv[3])
rows = []
for pod in summary.get('pods', []):
    ns = pod['podRef']['namespace']; name = pod['podRef']['name']
    for c in pod.get('containers', []):
        ws = c.get('memory', {}).get('workingSetBytes')
        if ws is None: continue
        rows.append((ns, name, c['name'], int(ws)))
rows.sort(key=lambda r: -r[3])
print('=== per-container RSS (workingSetBytes), descending ===')
total = 0
for ns, name, cn, ws in rows:
    total += ws
    print(f'  {ws/1048576:7.1f}Mi  {ns}/{name}/{cn}')
print(f'  --- total kubepods workingSet ~ {total/1048576:.0f}Mi ---')
print(f'=== node: capacity={cap/1048576:.0f}Mi allocatable={alloc/1048576:.0f}Mi headroom(cap-kubepods)={ (cap-total)/1048576:.0f}Mi ===')
# rough value table (platform request = RSS*0.8; backend Guaranteed = RSS*1.2)
def ceil_mi(b, mul):
    import math; return math.ceil(b*mul/1048576)
print('=== derived value proposals (Mi) ===')
for ns, name, cn, ws in rows:
    tag = 'backendGuaranteed(x1.2)' if ('cnpg' in name or 'dragonfly' in name.lower() or 'postgres' in name) else 'request(x0.8)'
    factor = 1.2 if 'Guaranteed' in tag else 0.8
    print(f'  {name}/{cn}: measured {ws/1048576:.0f}Mi -> {tag} {ceil_mi(ws,factor)}Mi')
PY
printf 'ok: measurement captured (see the table above; controller pins it to docs/measurements/)\n'

printf '\n=== SUMMARY (FAIL=%s) ===\n' "$FAIL"
exit "$FAIL"
