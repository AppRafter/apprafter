#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# 2.16e VPA vertical-autoscaling walk (real Hetzner, PUBLISHED artifacts).
# Provisions a throwaway solo node with the current CLI + channel-latest
# platform-stack (0.2.56+ carries the `InPlace` feature-gate fix + the pinned
# recommender floors), then validates the 2.16e VPA integration AND that the
# three controllers are actually Running (the crash-loop guard added 2026-08-28,
# the check this walk lacked when it passed green on the crash-looping 0.2.49).
# In-place resize needs k8s 1.35 + containerd 2.0 —
# kind/k3d can't do it, so this MUST run on real Hetzner.
#
# SCOPE: this walk hard-asserts the operator↔VPA INTEGRATION that is
# deterministic in a walk session — the operator emits a webhook-ACCEPTED VPA CR
# (validating the alpha feature gate), the floors/policy are correct, the
# recommendation is mirrored into Application.status, the managed→pro prune
# fires, the `off` knob is honoured, failurePolicy is Ignore, and metrics-server
# is present.
#
# 2.22d (D10) added the half that was missing: a real UPWARD in-place resize is
# now hard-asserted. It had been "observed" by comparing the pod's requests to
# the recommendation on a tiny app whose recommendation IS the seed — an
# equality an untouched pod satisfies, which was nonetheless written up as
# evidence of a resize. The walk now runs a second app whose footprint comes
# from the image itself (the schema expresses no `command`/`args`), so the pair
# genuinely differs, reads what the KUBELET actuated rather than what the
# updater asked for, and fails rather than logging. The remaining dynamics
# (downward reclaim, up-only, the 512Mi cap) need a shaped workload or a
# multi-day window and stay manually verified (see the NOTE at the end).
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
# cleanup runs once on EXIT; INT/TERM just `exit` (which fires the EXIT trap) so
# a SIGTERM actually STOPS the walk — a bare `trap cleanup TERM` would run cleanup
# then RESUME the interrupted sleep loop against the now-destroyed cluster.
trap cleanup EXIT
trap 'exit 143' INT TERM

NS=default
APP=vpa-app
# 2.22d (D10): a SECOND managed app whose idle footprint is far above the 32Mi
# seed, so the recommendation and the seed genuinely DIFFER.
#
# This exists because the apply-observation could not fail. The rendered seed
# is 32Mi and the recommender's floor is pinned to 32Mi, so for a tiny nginx
# the recommendation IS 32Mi — and `requests == recommendation` is satisfied by
# a pod the updater never touched. That equality was reported as "resized in
# place" and written up as evidence. An assertion whose success condition is
# met by the null case is not evidence.
#
# The schema expresses no `command`/`args`, so the workload has to come from
# the image itself. RabbitMQ's Erlang VM idles around 100Mi with no arguments
# and no required env — comfortably above both the seed and the floor.
HOG=vpa-hog
SEED_MEM=32Mi
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
deploy_hog() {  # 2.22d (D10): managed, but with a real footprint — see $HOG above
    kubectl apply -f - <<EOF
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: { name: $HOG, namespace: $NS }
spec:
  base:
    image: rabbitmq:3-alpine
    expose: { port: 5672 }
EOF
}
# What the KUBELET actuated, not what the updater asked for. `spec` carries the
# desired value — the updater patching it proves a patch, not a resize; the
# resize is only real once it appears here.
pod_actuated_mem() {
    kubectl -n "$NS" get pod "$1" \
        -o jsonpath='{.status.containerStatuses[0].resources.requests.memory}' 2>/dev/null
}
# Mebibytes from a Kubernetes quantity, for ordering comparisons. `-1` marks
# an unparseable value so a bad reading fails the comparison instead of
# silently comparing as zero.
#
# Both suffix families are handled because the VPA emits either: binary `Ki`
# `Mi` `Gi`, decimal `k` `M` `G` (kilo is LOWERCASE in the decimal family,
# where `K` is not a suffix at all), and bare bytes.
mem_mib() { python3 -c "
import re,sys
s=sys.argv[1].strip()
m=re.match(r'^(\d+(?:\.\d+)?)(Ki|Mi|Gi|Ti|Pi|Ei|k|M|G|T|P|E)?\$',s)
if not m: print(-1); raise SystemExit
v=float(m.group(1)); u=(m.group(2) or '')
f={'':1/2**20,
   'Ki':1/1024,'Mi':1,'Gi':1024,'Ti':1024**2,'Pi':1024**3,'Ei':1024**4,
   'k':1000/2**20,'M':1000**2/2**20,'G':1000**3/2**20,
   'T':1000**4/2**20,'P':1000**5/2**20,'E':1000**6/2**20}
print(int(v*f[u]))" "$1" 2>/dev/null || echo -1; }

printf '=== Phase 1: provision (CLI %s) + bootstrap channel-latest (0.2.56+ InPlace fix) ===\n' "$($APPRAFTER --version 2>/dev/null)"
# `--server-type` is REQUIRED since 2.16h-a removed the implicit default
# (Decision 0): `apply` now refuses with `server_type_not_selected` rather than
# silently picking a SKU. This walk predates that change and had no flag, so it
# could not provision at all — which is consistent with 2.22d recording it as
# "code ready, never run". Same default and override as mvp.sh / the machine
# picker walk.
"$APPRAFTER" target add e2e --provider hetzner-cloud --tier solo --region "$REGION" \
    --server-type "${APPRAFTER_E2E_SERVER_TYPE:-cpx22}" \
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

# CONTROLLERS RUNNING — the tell for the InPlace feature-gate crash-loop
# (2026-08-21): a wrong gate name (`InPlaceOrRecreate` on VPA 1.7.1) puts the
# updater + admission-controller in CrashLoopBackOff. The recommender is
# UNAFFECTED, so a recommendation still appears (#9) and a dead updater
# trivially passes no-thrash (#4) — this walk was GREEN on the crash-looping
# ps 0.2.49 for exactly that reason. Asserting the three controllers are
# Available (and none CrashLoopBackOff) is the ONLY check that catches it.
printf '\n=== VPA controllers Running (feature-gate crash-loop guard) ===\n'
[ "${NVPA:-0}" -eq 3 ] && ok "3 VPA controller deployments present (recommender/updater/admission)" || mark_fail "expected 3 VPA controller deployments, found ${NVPA:-0}"
if kubectl -n vpa wait --for=condition=Available deploy --all --timeout=300s >/dev/null 2>&1; then
    ok "all VPA controllers Available (updater + admission NOT CrashLoopBackOff — InPlace gate accepted)"
else
    mark_fail "a VPA controller is NOT Available within 300s — updater/admission CrashLoopBackOff (bad feature-gate name?)"
    kubectl -n vpa get pods 2>/dev/null | sed 's/^/    VPAPOD /'
    kubectl -n vpa logs deploy/vpa-updater --tail=20 2>/dev/null | grep -iE "feature-gate|InPlace|flag|invalid|unknown" | sed 's/^/    UPD /'
fi
CLB=$(kubectl -n vpa get pods -o json 2>/dev/null | python3 -c "import json,sys
d=json.load(sys.stdin); bad=[]
for p in d.get('items',[]):
    for cs in p.get('status',{}).get('containerStatuses',[]):
        w=(cs.get('state',{}).get('waiting') or {}).get('reason','')
        if w in ('CrashLoopBackOff','Error') or cs.get('restartCount',0)>=3:
            bad.append(p['metadata']['name']+'/'+cs['name']+':'+(w or 'restarts='+str(cs.get('restartCount',0))))
print('\n'.join(bad))" 2>/dev/null)
[ -z "$CLB" ] && ok "no crash-looping / high-restart container in the vpa namespace" || mark_fail "crash-looping VPA container(s): $CLB"

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

# 2.22d (D10): deploy the differing-pair app NOW, so its usage history
# accumulates during the same window the recommender is already warming up in.
deploy_hog || mark_fail "apply hog app"
deadline=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$deadline" ]; do [ -n "$(app_pod $HOG)" ] && break; sleep 15; done
HOGPOD=$(app_pod "$HOG"); printf '  hog pod: %s\n' "$HOGPOD"
[ -n "$HOGPOD" ] && ok "hog app Running (footprint above the ${SEED_MEM} seed)" \
    || mark_fail "hog app pod never appeared — the differing-pair assertion cannot run"

# 2.22d (D10) part 3: in-place resize is a KUBERNETES feature and nothing pins
# the Kubernetes version — `build_k3s_user_data` installs the stable channel.
# It works today by upstream default rather than by anything the platform
# arranges, so assert it directly instead of assuming it.
printf '\n=== D10 kubernetes prerequisite (in-place pod resize) ===\n'
K8SMINOR=$(kubectl version -o json 2>/dev/null | python3 -c "
import json,sys,re
m=json.load(sys.stdin).get('serverVersion',{}).get('minor','')
d=re.match(r'^(\d+)',str(m)); print(d.group(1) if d else -1)" 2>/dev/null || echo -1)
printf '  server minor: %s\n' "$K8SMINOR"
[ "${K8SMINOR:--1}" -ge 33 ] 2>/dev/null \
    && ok "D10 k8s minor $K8SMINOR >= 33 — in-place pod resize is on by default" \
    || mark_fail "D10 k8s minor $K8SMINOR < 33 — updateMode:InPlace actuates NOTHING on this cluster"

# #1 the operator emitted a VPA CR AND the webhook ACCEPTED it (validates the alpha feature gate)
printf '\n=== #1 VPA CR created + accepted (feature-gate InPlace) ===\n'
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
deadline=$(( $(date +%s) + 1500 ))  # up to 25m — the recommender's time-to-first-rec varies (12->15m+)
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
    if [ -z "$MIR" ]; then
        printf '  --- MIRROR DIAGNOSTIC (2.16e #9) ---\n'
        printf '  VPA.status.recommendation[0]: %s\n' "$(kubectl -n "$NS" get vpa "$APP" -o jsonpath='{.status.recommendation.containerRecommendations[0]}' 2>/dev/null)"
        printf '  Application.status FULL: %s\n' "$(kubectl -n "$NS" get application.apprafter.io "$APP" -o jsonpath='{.status}' 2>/dev/null)"
        printf '  operator log (vpa/recommend/error) tail:\n'
        kubectl -n apprafter-system logs deploy/apprafter-operator --tail=400 2>/dev/null | grep -iE "vpa|recommend|vertical|reconcile|error|warn|panic" | tail -30 | sed 's/^/    OPLOG /'
        printf '  --- END DIAGNOSTIC ---\n'
    fi
    [ -n "$MIR" ] && ok "#9 mirror: Application.status.recommendedResources.target.memory=$MIR (eventually-consistent ~1 reconcile)" || mark_fail "#9 recommendation NOT mirrored into Application.status after 200s"
    grep -qi "Too few replicas" <(kubectl -n vpa logs deploy/vpa-updater --tail=300 2>/dev/null) && mark_fail "#4 'Too few replicas' in updater log (min-replicas skipped the app)" || ok "#4 no 'Too few replicas' (minReplicas:1 works)"
    POD2=$(app_pod "$APP"); UID1=$(pod_uid "$POD2"); R1=$(pod_restarts "$POD2")
    [ "$UID0" = "$UID1" ] && [ "${R1:-0}" = "${R0:-0}" ] && ok "#4 pod not recreated (uid stable, RESTARTS=$R1) — in-place, no thrash" || mark_fail "#4 pod recreated (uid $UID0→$UID1, restarts $R0→$R1)"
    # apply-observation on the TINY app stays soft AND is no longer allowed to
    # claim success from the degenerate case (2.22d / D10). Its recommendation
    # is the 32Mi floor, which is also the seed, so the equality proves nothing
    # either way — say so rather than printing `ok:` and having it read back as
    # evidence that a resize was observed.
    ACT=$(pod_actuated_mem "$POD2")
    printf '  tiny-app observation: actuated requests.memory=%s vs VPA target=%s (seed %s)\n' \
        "${ACT:-<unset>}" "$RECO" "$SEED_MEM"
    if [ "$RECO" = "$SEED_MEM" ]; then
        printf '  inconclusive: recommendation == seed == %s, so an untouched pod satisfies this\n' "$SEED_MEM"
        printf '  inconclusive: proves nothing either way — the hog app below carries the real assertion\n'
    elif [ -n "$ACT" ] && [ "$ACT" = "$RECO" ]; then
        ok "tiny app actuated to the recommendation ($ACT)"
    else
        printf '  note: tiny app not yet actuated to %s (in-place resize lags the first rec); soft\n' "$RECO"
    fi

    # THE assertion (2.22d / D10). The hog app's footprint is far above the
    # floor, so its recommendation cannot equal the seed — which means an
    # untouched pod FAILS this, and that is the whole point.
    printf '\n=== D10 apply-observation (differing pair, HARD) ===\n'
    HRECO=""; hdl=$(( $(date +%s) + 900 ))
    while [ "$(date +%s)" -lt "$hdl" ]; do
        HRECO=$(vpa_jp "$HOG" '{.status.recommendation.containerRecommendations[0].target.memory}')
        [ -n "$HRECO" ] && break
        sleep 30
    done
    if [ -z "$HRECO" ]; then
        mark_fail "D10 no recommendation for the hog app after 15m — differing-pair assertion could not run"
    elif [ "$(mem_mib "$HRECO")" -lt 0 ]; then
        mark_fail "D10 hog recommendation '$HRECO' could not be parsed as a quantity — the comparison would be meaningless, so this is a walk-harness bug, not a resize failure"
    elif [ "$(mem_mib "$HRECO")" -le "$(mem_mib "$SEED_MEM")" ]; then
        # Not a resize failure: the premise failed. Say which, or the next
        # reader will debug the wrong thing.
        mark_fail "D10 hog recommendation $HRECO is not above the $SEED_MEM seed — the pair is degenerate, so this walk still cannot observe a resize (pick a heavier image)"
    else
        ok "D10 differing pair established: hog recommendation $HRECO > $SEED_MEM seed"
        HPOD=$(app_pod "$HOG"); HUID0=$(pod_uid "$HPOD")
        HACT=""; adl=$(( $(date +%s) + 600 ))
        while [ "$(date +%s)" -lt "$adl" ]; do
            HACT=$(pod_actuated_mem "$(app_pod "$HOG")")
            [ "$HACT" = "$HRECO" ] && break
            sleep 20
        done
        if [ "$HACT" = "$HRECO" ]; then
            ok "D10 APPLY OBSERVED: kubelet-actuated requests.memory=$HACT == recommendation, from a $SEED_MEM seed"
            HUID1=$(pod_uid "$(app_pod "$HOG")")
            [ "$HUID0" = "$HUID1" ] \
                && ok "D10 resize was IN PLACE (pod uid stable across the resize)" \
                || mark_fail "D10 pod was RECREATED (uid $HUID0→$HUID1) — InPlace must never evict"
        else
            printf '  --- D10 DIAGNOSTIC ---\n'
            printf '  hog pod status.containerStatuses[0].resources: %s\n' "$(kubectl -n "$NS" get pod "$(app_pod "$HOG")" -o jsonpath='{.status.containerStatuses[0].resources}' 2>/dev/null)"
            printf '  hog pod resize conditions: %s\n' "$(kubectl -n "$NS" get pod "$(app_pod "$HOG")" -o jsonpath='{range .status.conditions[*]}{.type}={.status}({.reason}) {end}' 2>/dev/null)"
            printf '  Application.status.recommendedResources: %s\n' "$(kubectl -n "$NS" get application.apprafter.io "$HOG" -o jsonpath='{.status.recommendedResources}' 2>/dev/null)"
            kubectl -n vpa logs deploy/vpa-updater --tail=200 2>/dev/null | grep -iE "in-place|inplace|resize|infeasible|deferred" | tail -20 | sed 's/^/    UPDLOG /'
            printf '  --- END DIAGNOSTIC ---\n'
            mark_fail "D10 in-place resize NEVER actuated: recommendation $HRECO, actuated ${HACT:-<unset>} after 10m"
        fi
    fi

    # 2.22d (D10) part 2: the deferred/infeasible signal. It was specified by
    # ADR 0054 and built end-to-end, but the only production call site passed a
    # hardcoded false, so it had never fired. On a healthy cluster it must be
    # ABSENT — and a wrong `notApplied` is now possible, so assert the absence.
    printf '\n=== D10 notApplied signal ===\n'
    NA=$(kubectl -n "$NS" get application.apprafter.io "$HOG" -o jsonpath='{.status.recommendedResources.notApplied}' 2>/dev/null)
    if [ -z "$NA" ]; then
        ok "D10 notApplied absent on a healthy cluster (nothing is blocking the resize)"
    else
        # A message mentioning in-place support means the operator's own k8s
        # probe disagrees with the version assertion above — one of the two is
        # wrong, and that is worth failing over.
        mark_fail "D10 notApplied is set on a healthy cluster: $NA"
    fi
    # Done with the hog — later phases toggle cluster-wide autoscale and would
    # otherwise be reasoning about two apps.
    kubectl delete application.apprafter.io "$HOG" -n "$NS" --wait=false 2>/dev/null
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
printf '  The UPWARD in-place resize is now asserted here (D10): the hog app carries a footprint the\n'
printf '  image supplies, so its recommendation cannot equal the seed and an untouched pod fails.\n'
printf '  The rest need a shaped workload the schema cannot express (no command/args) and/or a\n'
printf '  multi-day decay window; they stay upstream-VPA behaviours validated manually.\n'
printf '  This walk gates the operator<->VPA INTEGRATION (#1/#2/#3/#8/#9/#4-no-thrash/#10/#11/#13/#14).\n'

printf '\n=== SUMMARY (FAIL=%s) ===\n' "$FAIL"
exit "$FAIL"
