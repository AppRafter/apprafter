#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# 2.16d QoS-validation walk (real Hetzner, PUBLISHED artifacts). Provisions
# a throwaway node with the current CLI (which now emits k3s node
# reservations at bootstrap) and the published platform-stack 0.2.48 +
# operator v0.2.38 + cue-cmp 0.1.19, deploys an app that pulls a pg + redis
# claim, then asserts the 2.16d acceptance:
#   - the app pod (no explicit resources) is Burstable (measured seed), NOT BestEffort;
#   - the CNPG Postgres pod is Guaranteed — this covers ALL containers incl init (H6);
#   - the Dragonfly pod is Guaranteed;
#   - no AppRafter-managed platform pod is BestEffort;
#   - the node reserves headroom (allocatable.memory < capacity.memory).
# Judged by log markers (ok:/FAILED/GREEN). Bulletproof cleanup + verify.
set -uo pipefail

APPRAFTER=/projects/omnixal/apprafter/cli/target/release/apprafter
HZ=/projects/omnixal/apprafter/e2e/hz.py
REGION="${APPRAFTER_E2E_REGION:-nbg1}"
TOKEN="${HCLOUD_TOKEN:?HCLOUD_TOKEN required}"
OTHER_TOKEN="${OTHER_TOKEN:-}"
FAIL=0
mark_fail() { FAIL=1; printf 'FAILED: %s\n' "$1"; }
ok() { printf 'ok: %s\n' "$1"; }

WORK=$(mktemp -d /tmp/apprafter-qos-XXXXXX)
export HOME="$WORK/home"; mkdir -p "$HOME/.ssh"
export APPRAFTER_SSH_PRIVATE_KEY="$WORK/id_ed25519"
ssh-keygen -t ed25519 -N '' -f "$APPRAFTER_SSH_PRIVATE_KEY" -C apprafter-qos >/dev/null 2>&1
export APPRAFTER_SSH_PUBLIC_KEY="$(cat "$APPRAFTER_SSH_PRIVATE_KEY.pub")"
export KUBECONFIG="$WORK/kubeconfig"
cd "$WORK"

cleanup() {
    printf '\n=== CLEANUP (always runs) ===\n'
    "$APPRAFTER" destroy --yes >/dev/null 2>&1 && printf 'apprafter destroy: ok\n' || printf 'apprafter destroy: (non-zero; sweeping)\n'
    python3 "$HZ" sweep "$TOKEN" 2>/dev/null
    printf '  VERIFY primary project:\n'; python3 "$HZ" verify "$TOKEN" || FAIL=1
    if [ -n "$OTHER_TOKEN" ]; then printf '  VERIFY other project:\n'; python3 "$HZ" verify "$OTHER_TOKEN" || FAIL=1; fi
    rm -rf "$WORK"
    if [ "$FAIL" -eq 0 ]; then printf '\n=== QOS GREEN ===\n'; else printf '\n=== QOS RED ===\n'; fi
}
trap cleanup EXIT INT TERM

qos_of() { kubectl get pod "$1" -n "$2" -o jsonpath='{.status.qosClass}' 2>/dev/null; }

printf '=== Phase 1: provision (CLI %s → node reservations) + bootstrap published 0.2.48 ===\n' "$($APPRAFTER --version 2>/dev/null)"
"$APPRAFTER" target add e2e --provider hetzner-cloud --tier solo --region "$REGION" \
    --server-type "${APPRAFTER_E2E_SERVER_TYPE:-cpx22}" \
    --token "$TOKEN" --no-interactive --force || { mark_fail "target add"; exit 1; }
timeout 1500 "$APPRAFTER" up || { mark_fail "apprafter up"; exit 1; }
"$APPRAFTER" kubeconfig > "$KUBECONFIG" || { mark_fail "kubeconfig"; exit 1; }
ok "cluster bootstrapped"

printf '\n=== Phase 2: node reservations applied (allocatable < capacity) ===\n'
NODE=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}')
CAP=$(kubectl get node "$NODE" -o jsonpath='{.status.capacity.memory}')
ALLOC=$(kubectl get node "$NODE" -o jsonpath='{.status.allocatable.memory}')
printf '  capacity.memory=%s allocatable.memory=%s\n' "$CAP" "$ALLOC"
cap_ki=$(python3 -c "import re;q='$CAP';m=re.match(r'(\d+)',q);print(int(m.group(1)) if m else 0)")
alloc_ki=$(python3 -c "import re;q='$ALLOC';m=re.match(r'(\d+)',q);print(int(m.group(1)) if m else 0)")
if [ "$alloc_ki" -lt "$cap_ki" ]; then ok "node reserves headroom (allocatable $alloc_ki Ki < capacity $cap_ki Ki)"; else mark_fail "allocatable NOT < capacity — node reservations not applied"; fi

printf '\n=== Phase 3: platform steady + deploy app (needs.pg + needs.redis) ===\n'
deadline=$(( $(date +%s) + 480 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    total=$(kubectl -n argocd get applications.argoproj.io -o json 2>/dev/null | python3 -c "import json,sys;print(len(json.load(sys.stdin).get('items',[])))" 2>/dev/null || echo 0)
    synced=$(kubectl -n argocd get applications.argoproj.io -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print(sum(1 for a in d.get('items',[]) if a.get('status',{}).get('sync',{}).get('status')=='Synced'))" 2>/dev/null || echo 0)
    [ "$total" -ge 8 ] && [ "$synced" -ge "$total" ] && break
    sleep 20
done
kubectl apply -f - <<'EOF' || mark_fail "apply qos-app"
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: { name: qos-app, namespace: default }
spec:
  base:
    image: nginxdemos/hello:plain-text
    expose: { port: 80 }
    needs: { pg: {}, redis: {} }
EOF
printf '  waiting for app + CNPG + Dragonfly pods Running (up to 10m)...\n'
deadline=$(( $(date +%s) + 600 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    app_ok=$(kubectl -n default get pods -l apprafter.io/application=qos-app --field-selector=status.phase=Running -o name 2>/dev/null | wc -l | tr -d ' ')
    pg_ok=$(kubectl get pods -A -l 'cnpg.io/cluster' --field-selector=status.phase=Running -o name 2>/dev/null | wc -l | tr -d ' ')
    # Dragonfly INSTANCE pods are 'platform-redis-<class>-<NNN>' in dragonfly-system
    # (NOT 'dragonfly-*' — that name is only the operator). Match by ns + prefix.
    df_ok=$(kubectl -n dragonfly-system get pods --field-selector=status.phase=Running -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print(sum(1 for p in d.get('items',[]) if p['metadata']['name'].startswith('platform-redis-')))" 2>/dev/null || echo 0)
    printf '  app=%s pg=%s dragonfly=%s Running\n' "$app_ok" "$pg_ok" "$df_ok"
    [ "${app_ok:-0}" -ge 1 ] && [ "${pg_ok:-0}" -ge 1 ] && [ "${df_ok:-0}" -ge 1 ] && break
    sleep 20
done

printf '\n=== Phase 4: QoS assertions ===\n'
# app pod: Burstable (seed), NOT BestEffort
APP_POD=$(kubectl -n default get pods -l apprafter.io/application=qos-app -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)
APP_QOS=$(qos_of "$APP_POD" default)
printf '  app pod %s qosClass=%s\n' "$APP_POD" "$APP_QOS"
[ "$APP_QOS" = "Burstable" ] && ok "app (no explicit resources) is Burstable via the measured seed" || mark_fail "app qosClass=$APP_QOS (expected Burstable)"
APP_MEM_REQ=$(kubectl -n default get pod "$APP_POD" -o jsonpath='{.spec.containers[0].resources.requests.memory}' 2>/dev/null)
printf '  app container requests.memory=%s (expect 32Mi seed)\n' "$APP_MEM_REQ"
[ "$APP_MEM_REQ" = "32Mi" ] && ok "app seed request applied" || printf '  note: app request=%s\n' "$APP_MEM_REQ"

# CNPG pod: Guaranteed (covers ALL containers incl init — H6)
PG_NS=$(kubectl get pods -A -l 'cnpg.io/cluster' -o jsonpath='{.items[0].metadata.namespace}' 2>/dev/null)
PG_POD=$(kubectl get pods -A -l 'cnpg.io/cluster' -o jsonpath='{.items[0].metadata.name}' 2>/dev/null)
PG_QOS=$(qos_of "$PG_POD" "$PG_NS")
printf '  CNPG pod %s/%s qosClass=%s | initContainers: %s\n' "$PG_NS" "$PG_POD" "$PG_QOS" \
  "$(kubectl -n "$PG_NS" get pod "$PG_POD" -o jsonpath='{.spec.initContainers[*].name}' 2>/dev/null)"
[ "$PG_QOS" = "Guaranteed" ] && ok "CNPG Postgres is Guaranteed (H6: init containers carry resources too)" || mark_fail "CNPG qosClass=$PG_QOS (expected Guaranteed — H6 init-container gap?)"

# Dragonfly pod: Guaranteed. The INSTANCE pod is 'platform-redis-<class>-<NNN>'
# in dragonfly-system (the operator pod is 'dragonfly-operator-*' — exclude it
# by matching the platform-redis- prefix, not a 'dragonfly' substring).
DF_NS=dragonfly-system
DF_POD=$(kubectl -n dragonfly-system get pods -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print(next((p['metadata']['name'] for p in d.get('items',[]) if p['metadata']['name'].startswith('platform-redis-')),''))" 2>/dev/null)
DF_QOS=$(qos_of "$DF_POD" "$DF_NS")
printf '  Dragonfly pod %s/%s qosClass=%s\n' "$DF_NS" "$DF_POD" "$DF_QOS"
[ "$DF_QOS" = "Guaranteed" ] && ok "Dragonfly is Guaranteed" || mark_fail "Dragonfly qosClass=$DF_QOS (expected Guaranteed)"

# sweep: no AppRafter-managed platform pod is BestEffort
printf '\n=== Phase 5: no AppRafter platform pod is BestEffort ===\n'
BE=$(kubectl get pods -A -o json 2>/dev/null | python3 -c "
import json,sys
mgd={'argocd','apprafter-system','cnpg-system','dragonfly-system','cert-manager'}
d=json.load(sys.stdin); bad=[]
for p in d.get('items',[]):
    ns=p['metadata']['namespace']
    if ns in mgd and p.get('status',{}).get('qosClass')=='BestEffort':
        bad.append(ns+'/'+p['metadata']['name'])
print('\n'.join(bad))")
if [ -z "$BE" ]; then ok "no BestEffort pod in argocd/apprafter-system/cnpg-system/dragonfly-system/cert-manager"; else mark_fail "BestEffort platform pods remain:"; printf '%s\n' "$BE"; fi

printf '\n=== SUMMARY (FAIL=%s) ===\n' "$FAIL"
exit "$FAIL"
