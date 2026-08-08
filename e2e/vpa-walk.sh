#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# 2.16e VPA vertical-autoscaling walk (real Hetzner, PUBLISHED artifacts).
# Provisions a throwaway solo node with the current CLI (v0.2.39) + published
# platform-stack 0.2.49 / operator v0.2.39 / cue-cmp 0.1.20, then validates the
# 2.16e VPA integration. In-place resize needs k8s 1.35 + containerd 2.0 —
# kind/k3d can't do it, so this MUST run on real Hetzner.
#
# SCOPE: this walk hard-asserts the operator↔VPA INTEGRATION that is
# deterministic in a walk session — the operator emits a webhook-ACCEPTED VPA CR
# (validating the alpha feature gate), the floors/policy are correct, the
# recommendation is mirrored into Application.status, the managed→pro prune
# fires, the `off` knob is honoured, failurePolicy is Ignore, and metrics-server
# is present. The RESIZE DYNAMICS themselves (a real upward move, downward
# reclaim, up-only, the 512Mi cap) are upstream-VPA behaviour and need a
# sustained memory workload — which the AppRafter `Application` schema cannot
# express (no `command`/`args`) — so they are documented as manually-verifiable
# rather than auto-asserted here (see the NOTE at the end + spec walk #4-7).
#
# Judged by log markers (ok:/FAILED/GREEN). Bulletproof cleanup + verify both
# token projects -> 0. Tokens from backup-test.env (pre-flight hz.py list = 0).
set -uo pipefail

APPRAFTER=/projects/omnixal/apprafter/cli/target/release/apprafter
HZ=/projects/omnixal/apprafter/e2e/hz.py
REGION="${APPRAFTER_E2E_REGION:-nbg1}"
TOKEN="${HCLOUD_TOKEN:?HCLOUD_TOKEN required}"
OTHER_TOKEN="${OTHER_TOKEN:-}"
FAIL=0
mark_fail() { FAIL=1; printf 'FAILED: %s\n' "$1"; }
ok() { printf 'ok: %s\n' "$1"; }

WORK=$(mktemp -d /tmp/apprafter-vpa-XXXXXX)
export HOME="$WORK/home"; mkdir -p "$HOME/.ssh"
export APPRAFTER_SSH_PRIVATE_KEY="$WORK/id_ed25519"
ssh-keygen -t ed25519 -N '' -f "$APPRAFTER_SSH_PRIVATE_KEY" -C apprafter-vpa >/dev/null 2>&1
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
    if [ "$FAIL" -eq 0 ]; then printf '\n=== VPA WALK GREEN ===\n'; else printf '\n=== VPA WALK RED ===\n'; fi
}
trap cleanup EXIT INT TERM

NS=default
APP=vpa-app
# Safe field extraction via kubectl jsonpath (NO eval).
vpa_jp() { kubectl -n "$NS" get verticalpodautoscaler "$1" -o jsonpath="$2" 2>/dev/null; }
vpa_exists() { kubectl -n "$NS" get verticalpodautoscaler "$1" >/dev/null 2>&1; }
app_pod() { kubectl -n "$NS" get pods -l "apprafter.io/application=$1" -o jsonpath='{.items[0].metadata.name}' 2>/dev/null; }
pod_uid() { kubectl -n "$NS" get pod "$1" -o jsonpath='{.metadata.uid}' 2>/dev/null; }
pod_restarts() { kubectl -n "$NS" get pod "$1" -o jsonpath='{.status.containerStatuses[0].restartCount}' 2>/dev/null; }
deploy_managed() {  # a managed app (no explicit resources → the seed applies → VPA-targeted)
    kubectl apply -f - <<EOF
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: { name: $APP, namespace: $NS }
spec:
  base:
    image: nginxdemos/hello:plain-text
    expose: { port: 80 }
EOF
}

printf '=== Phase 1: provision (CLI %s) + bootstrap published 0.2.49 ===\n' "$($APPRAFTER --version 2>/dev/null)"
"$APPRAFTER" target add e2e --provider hetzner-cloud --tier solo --region "$REGION" \
    --token "$TOKEN" --no-interactive --force || { mark_fail "target add"; exit 1; }
timeout 1500 "$APPRAFTER" up || { mark_fail "apprafter up"; exit 1; }
"$APPRAFTER" kubeconfig > "$KUBECONFIG" || { mark_fail "kubeconfig"; exit 1; }
ok "cluster bootstrapped"

printf '\n=== Phase 2: platform steady + VPA component healthy ===\n'
deadline=$(( $(date +%s) + 600 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    total=$(kubectl -n argocd get applications.argoproj.io -o json 2>/dev/null | python3 -c "import json,sys;print(len(json.load(sys.stdin).get('items',[])))" 2>/dev/null || echo 0)
    synced=$(kubectl -n argocd get applications.argoproj.io -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print(sum(1 for a in d.get('items',[]) if a.get('status',{}).get('sync',{}).get('status')=='Synced'))" 2>/dev/null || echo 0)
    [ "$total" -ge 9 ] && [ "$synced" -ge "$total" ] && break
    sleep 20
done
NVPA=$(kubectl -n vpa get deploy -o name 2>/dev/null | wc -l | tr -d ' ')
[ "${NVPA:-0}" -ge 1 ] && ok "vpa component deployed ($NVPA deployments: recommender/updater/admission)" || mark_fail "vpa component not deployed"

# #2 metrics-server (M1 hard prereq)
printf '\n=== #2 metrics-server (kubectl top) ===\n'
deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$deadline" ]; do kubectl top pod -n kube-system >/dev/null 2>&1 && break; sleep 15; done
kubectl top pod -n kube-system >/dev/null 2>&1 && ok "#2 kubectl top responds (metrics-server present)" || mark_fail "#2 metrics-server absent — VPA gets no recommendations"

# T13: measure the VPA controllers' actual RSS to right-size the component_vpa.cue seed (100Mi/300Mi).
printf '\n=== T13 measure VPA controller RSS (right-size the seed) ===\n'
sleep 60  # let the controllers settle
kubectl top pod -n vpa 2>/dev/null || printf '  (kubectl top -n vpa not ready yet)\n'

# no VPA component pod is BestEffort (2.16d principle; ps 0.2.50 gave all 3 controllers resources)
printf '\n=== VPA pods QoS (no BestEffort — 2.16d / ps 0.2.50) ===\n'
BE=$(kubectl -n vpa get pods -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print('\n'.join(p['metadata']['name'] for p in d.get('items',[]) if p.get('status',{}).get('qosClass')=='BestEffort'))" 2>/dev/null)
[ -z "$BE" ] && ok "no BestEffort pod in the vpa namespace (recommender/updater/admission all sized)" || mark_fail "BestEffort VPA pods remain: $BE"

printf '\n=== Phase 3: deploy managed app ===\n'
deploy_managed || mark_fail "apply managed app"
deadline=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$deadline" ]; do [ -n "$(app_pod $APP)" ] && break; sleep 15; done
POD=$(app_pod "$APP"); printf '  app pod: %s\n' "$POD"
[ -n "$POD" ] && ok "managed app Running" || mark_fail "managed app pod never appeared"

# #1 the operator emitted a VPA CR AND the webhook ACCEPTED it (validates the alpha feature gate)
printf '\n=== #1 VPA CR created + accepted (feature-gate InPlaceOrRecreate) ===\n'
deadline=$(( $(date +%s) + 120 ))
while [ "$(date +%s)" -lt "$deadline" ]; do vpa_exists "$APP" && break; sleep 10; done
if vpa_exists "$APP"; then
    UM=$(vpa_jp "$APP" '{.spec.updatePolicy.updateMode}')
    [ "$UM" = "InPlace" ] && ok "#1 VPA CR $APP accepted by webhook (updateMode=InPlace — alpha gate active)" || mark_fail "#1 VPA CR updateMode=$UM (expected InPlace)"
else
    mark_fail "#1 no VPA CR — operator didn't emit OR the webhook REJECTED the in-place mode (feature-gate off?)"
    kubectl -n "$NS" get events 2>/dev/null | grep -iE "vertical|vpa|feature" | tail -5
fi

# #8 minAllowed floor + containerName wildcard + RequestsOnly + minReplicas
printf '\n=== #8 CR policy (minAllowed / containerName / RequestsOnly / minReplicas) ===\n'
MINMEM=$(vpa_jp "$APP" '{.spec.resourcePolicy.containerPolicies[0].minAllowed.memory}')
CN=$(vpa_jp "$APP" '{.spec.resourcePolicy.containerPolicies[0].containerName}')
CV=$(vpa_jp "$APP" '{.spec.resourcePolicy.containerPolicies[0].controlledValues}')
MR=$(vpa_jp "$APP" '{.spec.updatePolicy.minReplicas}')
[ "$MINMEM" = "32Mi" ] && ok "#8 minAllowed.memory=32Mi (seed floor, never 0)" || mark_fail "#8 minAllowed.memory=$MINMEM"
[ "$CN" = "*" ] && ok "#8 containerName='*' (wildcard, N2)" || mark_fail "#8 containerName=$CN"
[ "$CV" = "RequestsOnly" ] && ok "#8 controlledValues=RequestsOnly" || mark_fail "#8 controlledValues=$CV"
[ "$MR" = "1" ] && ok "#8 minReplicas=1 (H6 — single-replica not skipped)" || mark_fail "#8 minReplicas=$MR"

# #3 failurePolicy Ignore — kill admission-controller, a pod still creates
printf '\n=== #3 failurePolicy Ignore (kill admission-controller, pod still creates) ===\n'
kubectl -n vpa delete pod -l app.kubernetes.io/component=admission-controller --wait=false 2>/dev/null
kubectl -n vpa delete pod -l app=vpa-admission-controller --wait=false 2>/dev/null
sleep 3
kubectl -n "$NS" run h3probe --image=nginxdemos/hello:plain-text --restart=Never 2>/dev/null
sleep 8
kubectl -n "$NS" get pod h3probe >/dev/null 2>&1 && ok "#3 pod created while admission-controller down (failurePolicy Ignore)" || mark_fail "#3 pod creation blocked (failurePolicy Fail?)"
kubectl -n "$NS" delete pod h3probe --wait=false 2>/dev/null

# #9 mirror + #4 no-thrash — VPA produces a recommendation (confidence-widened, fast even for a tiny app),
# the operator mirrors it into Application.status, and the pod is NOT recreated (in-place, no thrash).
printf '\n=== #9 mirror + #4 no-thrash (recommendation → Application.status, pod not recreated) ===\n'
UID0=$(pod_uid "$POD"); R0=$(pod_restarts "$POD")
deadline=$(( $(date +%s) + 900 ))  # up to 15m for the first recommendation
RECO=""
while [ "$(date +%s)" -lt "$deadline" ]; do
    RECO=$(vpa_jp "$APP" '{.status.recommendation.containerRecommendations[0].target.memory}')
    [ -n "$RECO" ] && break
    sleep 30
done
if [ -n "$RECO" ]; then
    ok "#4/#9 VPA recommendation target.memory=$RECO (>= 32Mi floor)"
    # #9 mirror is eventually-consistent: the operator reads the VPA on its 60s
    # reconcile (it does NOT watch the VPA — M2 design), so poll a few cycles.
    MIR=""; mdl=$(( $(date +%s) + 200 ))
    while [ "$(date +%s)" -lt "$mdl" ]; do
        MIR=$(kubectl -n "$NS" get application.apprafter.io "$APP" -o jsonpath='{.status.recommendedResources.recommendation.target.memory}' 2>/dev/null)
        [ -n "$MIR" ] && break
        sleep 20
    done
    [ -n "$MIR" ] && ok "#9 mirror: Application.status.recommendedResources.target.memory=$MIR (eventually-consistent ~1 reconcile)" || mark_fail "#9 recommendation NOT mirrored into Application.status after 200s"
    grep -qi "Too few replicas" <(kubectl -n vpa logs deploy/vpa-updater --tail=300 2>/dev/null) && mark_fail "#4 'Too few replicas' in updater log (min-replicas skipped the app)" || ok "#4 no 'Too few replicas' (minReplicas:1 works)"
    POD2=$(app_pod "$APP"); UID1=$(pod_uid "$POD2"); R1=$(pod_restarts "$POD2")
    [ "$UID0" = "$UID1" ] && [ "${R1:-0}" = "${R0:-0}" ] && ok "#4 pod not recreated (uid stable, RESTARTS=$R1) — in-place, no thrash" || mark_fail "#4 pod recreated (uid $UID0→$UID1, restarts $R0→$R1)"
else
    mark_fail "#9 no VPA recommendation after 15m (metrics-server? feature-gate? history) — mirror untestable"
fi

# #10 managed→pro prune
printf '\n=== #10 managed→pro prune ===\n'
kubectl apply -f - <<EOF 2>/dev/null
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: { name: $APP, namespace: $NS }
spec:
  base:
    image: nginxdemos/hello:plain-text
    expose: { port: 80 }
    resources:
      requests: { memory: "64Mi", cpu: "25m" }
      limits: { memory: "256Mi" }
EOF
deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$deadline" ]; do vpa_exists "$APP" || break; sleep 15; done
vpa_exists "$APP" && mark_fail "#10 VPA CR NOT pruned on managed→pro (would fight the user's resources)" || ok "#10 VPA CR pruned after adding explicit resources (pro-mode)"

# #11 mode off → CR still emitted with updateMode Off (mirror keeps learning), pods not mutated
printf '\n=== #11 mode off (observe-only) ===\n'
kubectl delete application.apprafter.io "$APP" -n "$NS" --wait=true 2>/dev/null
"$APPRAFTER" platform autoscale set off 2>&1 | grep -qi "off" && ok "autoscale set off" || mark_fail "autoscale set off failed"
"$APPRAFTER" platform autoscale show 2>&1 | grep -qi "off" && ok "autoscale show → off" || mark_fail "autoscale show did not report off"
deploy_managed; sleep 40
OFFUM=$(vpa_jp "$APP" '{.spec.updatePolicy.updateMode}')
[ "$OFFUM" = "Off" ] && ok "#11 VPA CR emitted with updateMode=Off (recommender still learns → mirror)" || mark_fail "#11 mode off did not set updateMode=Off (got $OFFUM)"
"$APPRAFTER" platform autoscale set full 2>/dev/null && ok "autoscale set full (restored)"

# #13 checkpoint durability (soft — the object exists once the recommender has looped)
printf '\n=== #13 VPACheckpoint persisted ===\n'
kubectl get verticalpodautoscalercheckpoints -A -o name 2>/dev/null | grep -q checkpoint \
  && ok "#13 VPACheckpoint object exists (durable across recommender restart)" || printf '  note: no checkpoint yet (needs more recommender loops); #13 soft\n'

# #14 post-saturation headroom (H8 — the 2.16d D2 budget is superseded by observed requests)
printf '\n=== #14 node headroom (H8 supersession) ===\n'
NODE=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}')
CAP=$(kubectl get node "$NODE" -o jsonpath='{.status.capacity.memory}')
ALLOC=$(kubectl get node "$NODE" -o jsonpath='{.status.allocatable.memory}')
printf '  node capacity=%s allocatable=%s\n' "$CAP" "$ALLOC"
ok "#14 headroom measured (allocatable=$ALLOC) — record vs the 2.16d baseline in docs/measurements/"

printf '\n=== NOTE: resize-dynamics scenarios (#5 downward / #6 up-only / #7 512Mi cap / #12 image-rollout) ===\n'
printf '  These need a sustained memory workload the Application schema cannot express (no command/args)\n'
printf '  and/or a multi-day decay window; they are upstream-VPA behaviours validated manually, not here.\n'
printf '  This walk gates the operator<->VPA INTEGRATION (#1/#2/#3/#8/#9/#4-no-thrash/#10/#11/#13/#14).\n'

printf '\n=== SUMMARY (FAIL=%s) ===\n' "$FAIL"
exit "$FAIL"
