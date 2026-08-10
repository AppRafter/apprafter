#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# 2.16f Argo CD footprint-tuning walk (real Hetzner, PUBLISHED artifacts).
# Validates that the argocd footprint tuning (GOMEMLIMIT/GOGC env +
# resource.exclusions + applicationSet.replicas:0, then a measured
# controller-request re-pin) cuts the app-controller working set below ~200Mi
# WITHOUT breaking sync/health, and that the fresh-install SSA contention
# (helm writes applicationSet replicas:1, Argo applies 0 — F1) converges.
#
# The measure->pin split means TWO invocations against two published versions:
#   MODE=tuning  (after the rc is published, before the stable):
#     bootstrap the PRE stable -> measure pre -> upgrade to the RC (tuning) ->
#     assert convergence + app-controller < ~200Mi + no restarts/OOMKilled ->
#     measure post-tuning. The controller reads the post value, computes the
#     request re-pin (RSS*0.8), and cuts the stable.
#   MODE=stable  (after the re-pinned stable is published):
#     Leg 3 (fresh install on the shipped stable — F1 SSA) then Leg 2 (downgrade
#     to the rc, upgrade back to the stable = the re-pin applies on upgrade).
#
# Measurement = the 2.16d method + load profile: per-CONTAINER workingSetBytes
# via the kubelet /stats/summary API (kind can't serve it -> real Hetzner),
# taken with a managed app carrying needs.pg + needs.redis, ~5 min settle.
#
# Judged by log markers (ok:/FAILED/GREEN), NOT the exit code (sandbox-run masks
# the inner code). Bulletproof cleanup + verify both token projects -> 0. Tokens
# from backup-test.env (pre-flight hz.py list = 0 before the destructive sweep).
set -uo pipefail

APPRAFTER=/projects/omnixal/apprafter/cli/target/release/apprafter
HZ=/projects/omnixal/apprafter/e2e/hz.py
REGION="${APPRAFTER_E2E_REGION:-nbg1}"
TOKEN="${HCLOUD_TOKEN:?HCLOUD_TOKEN required}"
OTHER_TOKEN="${OTHER_TOKEN:-}"

MODE="${1:-tuning}"                                  # tuning | stable
PRE_VERSION="${PRE_VERSION:-0.2.52}"                 # stable channel-latest before 2.16f
RC_VERSION="${RC_VERSION:-0.2.53-rc.1}"              # the tuning walk-vehicle
STABLE_VERSION="${STABLE_VERSION:-0.2.53}"           # the re-pinned final stable
TARGET_RSS_MI="${TARGET_RSS_MI:-200}"               # acceptance: app-controller <
EXPECT_CONTROLLER_REQ="${EXPECT_CONTROLLER_REQ:-}"  # optional exact re-pin assert (stable mode)

FAIL=0
mark_fail() { FAIL=1; printf 'FAILED: %s\n' "$1"; }
ok() { printf 'ok: %s\n' "$1"; }

WORK=$(mktemp -d /tmp/apprafter-argofp-XXXXXX)
export HOME="$WORK/home"; mkdir -p "$HOME/.ssh"
export APPRAFTER_SSH_PRIVATE_KEY="$WORK/id_ed25519"
ssh-keygen -t ed25519 -N '' -f "$APPRAFTER_SSH_PRIVATE_KEY" -C apprafter-argofp >/dev/null 2>&1
export APPRAFTER_SSH_PUBLIC_KEY="$(cat "$APPRAFTER_SSH_PRIVATE_KEY.pub")"
export KUBECONFIG="$WORK/kubeconfig"
MEASDIR=/projects/omnixal/apprafter/docs/measurements
cd "$WORK"

cleanup() {
    printf '\n=== CLEANUP (always runs) ===\n'
    "$APPRAFTER" destroy --yes >/dev/null 2>&1 && printf 'apprafter destroy: ok\n' || printf 'apprafter destroy: (non-zero; sweeping)\n'
    python3 "$HZ" sweep "$TOKEN" 2>/dev/null
    printf '  VERIFY primary project:\n'; python3 "$HZ" verify "$TOKEN" || FAIL=1
    if [ -n "$OTHER_TOKEN" ]; then printf '  VERIFY other project:\n'; python3 "$HZ" verify "$OTHER_TOKEN" || FAIL=1; fi
    rm -rf "$WORK"
    if [ "$FAIL" -eq 0 ]; then printf '\n=== ARGOCD-FOOTPRINT WALK GREEN (mode=%s) ===\n' "$MODE"; else printf '\n=== ARGOCD-FOOTPRINT WALK RED (mode=%s) ===\n' "$MODE"; fi
}
# cleanup runs once on EXIT; INT/TERM just `exit` (which fires EXIT) so a SIGTERM
# STOPS the walk — a bare `trap cleanup TERM` would run cleanup then RESUME the
# interrupted loop against the destroyed cluster (2.16e lesson).
trap cleanup EXIT
trap 'exit 143' INT TERM

# ---- helpers ---------------------------------------------------------------
NS=default
APP=fp-app

# Discover the argo-cd workload objects by name substring (release-name agnostic).
sts_appctrl() { kubectl -n argocd get statefulset -o name 2>/dev/null | grep -i 'application-controller' | head -1; }
deploy_reposrv() { kubectl -n argocd get deploy -o name 2>/dev/null | grep -i 'repo-server' | head -1; }
deploy_appset() { kubectl -n argocd get deploy -o name 2>/dev/null | grep -i 'applicationset' | head -1; }

platform_version() {  # live PlatformStack status.currentVersion
    kubectl get platformstacks.apprafter.io -A -o jsonpath='{.items[0].status.currentVersion}' 2>/dev/null
}
argocd_app_sync() { kubectl -n argocd get application.argoproj.io argocd -o jsonpath='{.status.sync.status}' 2>/dev/null; }
argocd_app_health() { kubectl -n argocd get application.argoproj.io argocd -o jsonpath='{.status.health.status}' 2>/dev/null; }

wait_all_apps_synced() {  # total>=N and all Synced+Healthy
    local want="${1:-9}" deadline; deadline=$(( $(date +%s) + 900 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        local total synced healthy
        total=$(kubectl -n argocd get applications.argoproj.io -o json 2>/dev/null | python3 -c "import json,sys;print(len(json.load(sys.stdin).get('items',[])))" 2>/dev/null || echo 0)
        synced=$(kubectl -n argocd get applications.argoproj.io -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print(sum(1 for a in d['items'] if a.get('status',{}).get('sync',{}).get('status')=='Synced'))" 2>/dev/null || echo 0)
        healthy=$(kubectl -n argocd get applications.argoproj.io -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print(sum(1 for a in d['items'] if a.get('status',{}).get('health',{}).get('status')=='Healthy'))" 2>/dev/null || echo 0)
        printf '  argo apps synced=%s healthy=%s / total=%s\n' "$synced" "$healthy" "$total"
        [ "$total" -ge "$want" ] && [ "$synced" -ge "$total" ] && [ "$healthy" -ge "$total" ] && return 0
        sleep 20
    done
    return 1
}

wait_platform_version() {  # poll until PlatformStack.status.currentVersion == $1
    local want="$1" deadline; deadline=$(( $(date +%s) + 900 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        local cur; cur=$(platform_version)
        printf '  platform currentVersion=%s (want %s)\n' "${cur:-<none>}" "$want"
        [ "$cur" = "$want" ] && return 0
        sleep 20
    done
    return 1
}

deploy_app() {  # managed app (no explicit resources) with pg + redis (2.16d load profile)
    kubectl apply -f - <<EOF
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: { name: $APP, namespace: $NS }
spec:
  base:
    image: nginxdemos/hello:plain-text
    expose: { port: 80 }
    needs:
      pg: {}
      redis: {}
EOF
}

wait_app_backends() {  # CNPG pg pod + Dragonfly pod Running
    local deadline; deadline=$(( $(date +%s) + 600 )); local pg=0 df=0
    while [ "$(date +%s)" -lt "$deadline" ]; do
        pg=$(kubectl get pods -A -l 'cnpg.io/cluster' --field-selector=status.phase=Running -o name 2>/dev/null | wc -l | tr -d ' ')
        df=$(kubectl get pods -n dragonfly-system -o json 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print(sum(1 for p in d.get('items',[]) if p['metadata']['name'].startswith('platform-redis-') and p.get('status',{}).get('phase')=='Running'))" 2>/dev/null || echo 0)
        printf '  pg Running=%s  dragonfly Running=%s\n' "$pg" "$df"
        [ "${pg:-0}" -ge 1 ] && [ "${df:-0}" -ge 1 ] && return 0
        sleep 20
    done
    return 1
}

# per-container workingSetBytes + cpu via kubelet /stats/summary. Prints a table
# for the argocd namespace, writes APPCTRL_RSS_MI / REPO_RSS_MI to $WORK/rss.env.
measure() {
    local label="$1" out="$WORK/summary.json"
    local NODE; NODE=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}')
    kubectl get --raw "/api/v1/nodes/${NODE}/proxy/stats/summary" 2>/dev/null > "$out" || { mark_fail "kubelet /stats/summary unreachable ($label)"; return 1; }
    python3 - "$out" "$label" "$WORK/rss.env" <<'PY'
import json, sys
summary = json.load(open(sys.argv[1])); label = sys.argv[2]; envf = sys.argv[3]
rows = []
for pod in summary.get('pods', []):
    ns = pod['podRef']['namespace']; name = pod['podRef']['name']
    if ns != 'argocd': continue
    for c in pod.get('containers', []):
        ws = c.get('memory', {}).get('workingSetBytes'); cpu = c.get('cpu', {}).get('usageNanoCores')
        if ws is None: continue
        rows.append((name, c['name'], int(ws), int(cpu or 0)))
rows.sort(key=lambda r: -r[2])
print(f'=== argocd per-container RSS + CPU [{label}] ===')
appctrl = repo = 0
for name, cn, ws, cpu in rows:
    print(f'  {ws/1048576:7.1f}Mi  {cpu/1e6:6.1f}m  {name}/{cn}')
    if 'application-controller' in name: appctrl = ws
    if 'repo-server' in name and cn != 'cue-cmp': repo = ws
with open(envf, 'w') as f:
    f.write(f'APPCTRL_RSS_MI={appctrl//1048576}\nREPO_RSS_MI={repo//1048576}\n')
print(f'  -> app-controller {appctrl/1048576:.0f}Mi | repo-server(main) {repo/1048576:.0f}Mi')
PY
    # shellcheck disable=SC1091
    . "$WORK/rss.env"
}

assert_no_restarts_oomkill() {  # M3: memory tuning against hard limits — catch a died-and-came-back pod
    local dump; dump=$(kubectl -n argocd get pods -o json 2>/dev/null)
    local bad; bad=$(printf '%s' "$dump" | python3 -c "
import json,sys
d=json.load(sys.stdin); probs=[]
for p in d.get('items',[]):
    for cs in p.get('status',{}).get('containerStatuses',[]):
        rc=cs.get('restartCount',0)
        term=(cs.get('lastState',{}).get('terminated') or {})
        if rc>0 or term.get('reason')=='OOMKilled':
            probs.append(f\"{p['metadata']['name']}/{cs['name']} restarts={rc} lastTerm={term.get('reason','-')}\")
print('\n'.join(probs))
" 2>/dev/null)
    if [ -z "$bad" ]; then ok "no restarts / no OOMKilled in argocd ns"; else mark_fail "argocd pod restart/OOMKill: $bad"; fi
    printf '  containerStatuses dump:\n'; printf '%s' "$dump" | python3 -c "
import json,sys
d=json.load(sys.stdin)
for p in d.get('items',[]):
    for cs in p.get('status',{}).get('containerStatuses',[]):
        print(f\"    {p['metadata']['name']}/{cs['name']} ready={cs.get('ready')} restarts={cs.get('restartCount')}\")
" 2>/dev/null
}

assert_tuning_present() {  # exclusions in argocd-cm + env on the two workloads
    kubectl -n argocd get cm argocd-cm -o jsonpath='{.data.resource\.exclusions}' 2>/dev/null | grep -q 'CiliumIdentity' \
        && ok "argocd-cm carries resource.exclusions" || mark_fail "resource.exclusions absent from argocd-cm"
    local sc; sc=$(sts_appctrl)
    kubectl -n argocd get "$sc" -o json 2>/dev/null | grep -q 'GOMEMLIMIT' \
        && ok "app-controller carries GOMEMLIMIT/GOGC env" || mark_fail "app-controller env missing GOMEMLIMIT"
    local rd; rd=$(deploy_reposrv)
    kubectl -n argocd get "$rd" -o json 2>/dev/null | grep -q 'GOMEMLIMIT' \
        && ok "repo-server carries GOMEMLIMIT/GOGC env" || mark_fail "repo-server env missing GOMEMLIMIT"
}

assert_appset_off() {  # applicationSet Deployment 0/0 + no pod + argocd app Healthy in-tree
    local ad; ad=$(deploy_appset)
    if [ -z "$ad" ]; then ok "applicationset Deployment absent entirely"; return; fi
    local desired; desired=$(kubectl -n argocd get "$ad" -o jsonpath='{.spec.replicas}' 2>/dev/null)
    [ "${desired:-x}" = "0" ] && ok "applicationset Deployment desired replicas=0" || mark_fail "applicationset desired replicas=$desired (expected 0)"
    local pods; pods=$(kubectl -n argocd get pods -o name 2>/dev/null | grep -ic applicationset)
    [ "${pods:-1}" = "0" ] && ok "no applicationset-controller pod" || mark_fail "applicationset pod still present ($pods)"
}

assert_argocd_synced_healthy() {
    local s h; s=$(argocd_app_sync); h=$(argocd_app_health)
    [ "$s" = "Synced" ] && ok "argocd app Synced (no OutOfSync residue)" || mark_fail "argocd app sync=$s (expected Synced)"
    [ "$h" = "Healthy" ] && ok "argocd app Healthy (appset 0/0 Healthy in-tree)" || mark_fail "argocd app health=$h (expected Healthy)"
}

controller_req_memory() {  # the app-controller's request.memory pin
    local sc; sc=$(sts_appctrl)
    kubectl -n argocd get "$sc" -o jsonpath='{.spec.template.spec.containers[0].resources.requests.memory}' 2>/dev/null
}

assert_rss_under_target() {
    local mi="$1"
    [ -n "$mi" ] && [ "$mi" -lt "$TARGET_RSS_MI" ] \
        && ok "app-controller working set ${mi}Mi < ${TARGET_RSS_MI}Mi target" \
        || mark_fail "app-controller working set ${mi:-?}Mi NOT < ${TARGET_RSS_MI}Mi (m5 ladder: 200-256 -> lower GOMEMLIMIT + re-walk)"
}

# ---- provision (common) ----------------------------------------------------
printf '=== Phase 1: provision (CLI %s) + bootstrap channel-latest (tier=solo, %s) ===\n' "$($APPRAFTER --version 2>/dev/null)" "$REGION"
"$APPRAFTER" target add e2e --provider hetzner-cloud --tier solo --region "$REGION" \
    --token "$TOKEN" --no-interactive --force || { mark_fail "target add"; exit 1; }
timeout 1500 "$APPRAFTER" up || { mark_fail "apprafter up"; exit 1; }
"$APPRAFTER" kubeconfig > "$KUBECONFIG" || { mark_fail "kubeconfig"; exit 1; }
ok "cluster bootstrapped"

printf '\n=== Phase 2: platform steady (all Argo apps Synced+Healthy) ===\n'
wait_all_apps_synced 9 || mark_fail "platform never reached all-Synced+Healthy"
BOOT_VER=$(platform_version); printf '  bootstrapped platform version = %s\n' "${BOOT_VER:-<none>}"

printf '\n=== Phase 3: deploy managed app (needs.pg + needs.redis) + settle 5m ===\n'
deploy_app || mark_fail "apply managed app"
wait_app_backends || mark_fail "app backends (pg/dragonfly) never Running"
sleep 300

if [ "$MODE" = "tuning" ]; then
    # ---- Leg 1: tuning on the upgrade path -------------------------------
    printf '\n=== Leg 1: pre-change baseline must be the PRE stable (%s) ===\n' "$PRE_VERSION"
    [ "$BOOT_VER" = "$PRE_VERSION" ] && ok "bootstrapped $PRE_VERSION (rc is a prerelease → channel-latest stayed stable)" \
        || mark_fail "bootstrapped $BOOT_VER, expected $PRE_VERSION — the rc leaked to channel-latest? (do NOT run tuning mode after the stable is published)"
    printf '\n--- measure PRE-change (loaded) ---\n'; measure "pre-change $PRE_VERSION"
    PRE_APPCTRL="${APPCTRL_RSS_MI:-0}"; printf '  PRE app-controller = %sMi (request pin %s)\n' "$PRE_APPCTRL" "$(controller_req_memory)"

    printf '\n=== Leg 1: upgrade to the tuning rc (%s) ===\n' "$RC_VERSION"
    "$APPRAFTER" platform upgrade --to "$RC_VERSION" --cached 2>&1 | tail -3
    wait_platform_version "$RC_VERSION" || mark_fail "platform never reached $RC_VERSION"
    wait_all_apps_synced 9 || mark_fail "apps not all Synced+Healthy after tuning upgrade"
    assert_argocd_synced_healthy
    assert_appset_off
    assert_tuning_present
    printf '\n--- settle 3m then measure POST-tuning ---\n'; sleep 180
    measure "post-tuning $RC_VERSION"
    POST_APPCTRL="${APPCTRL_RSS_MI:-0}"
    assert_rss_under_target "$POST_APPCTRL"
    assert_no_restarts_oomkill

    printf '\n=== Leg 1 result: app-controller %sMi -> %sMi ===\n' "$PRE_APPCTRL" "$POST_APPCTRL"
    if [ "$POST_APPCTRL" -gt 0 ]; then
        REPIN=$(( POST_APPCTRL * 8 / 10 ))
        printf '  >>> RE-PIN controller.resources.requests.memory = %sMi (post-RSS %sMi * 0.8) — cut the stable with this. <<<\n' "$REPIN" "$POST_APPCTRL"
    fi
    mkdir -p "$MEASDIR"
    { echo "# 2.16f argocd footprint — MODE=tuning ($RC_VERSION)";
      echo "pre-change ($PRE_VERSION) app-controller: ${PRE_APPCTRL}Mi  repo-server: ${REPO_RSS_MI:-?}Mi";
      echo "post-tuning ($RC_VERSION) app-controller: ${POST_APPCTRL}Mi";
      echo "controller request re-pin proposal: $(( POST_APPCTRL * 8 / 10 ))Mi"; } > "$MEASDIR/2.16f-argocd-footprint-tuning.txt"
    printf '  wrote %s/2.16f-argocd-footprint-tuning.txt\n' "$MEASDIR"

elif [ "$MODE" = "stable" ]; then
    # ---- Leg 3: fresh install on the shipped stable (F1 SSA contention) ---
    printf '\n=== Leg 3: FRESH install must be the shipped STABLE (%s) ===\n' "$STABLE_VERSION"
    [ "$BOOT_VER" = "$STABLE_VERSION" ] && ok "fresh-bootstrapped $STABLE_VERSION (channel-latest = the shipped stable)" \
        || mark_fail "fresh-bootstrapped $BOOT_VER, expected $STABLE_VERSION"
    assert_argocd_synced_healthy
    assert_appset_off                 # the F1 assertion: helm wrote replicas:1, Argo SSA'd 0, converged to 0/0
    assert_tuning_present
    measure "fresh-stable $STABLE_VERSION"
    assert_rss_under_target "${APPCTRL_RSS_MI:-0}"
    assert_no_restarts_oomkill
    REQ=$(controller_req_memory)
    [ "$REQ" != "288Mi" ] && ok "controller request re-pinned to $REQ (down from 288Mi)" || mark_fail "controller request still 288Mi (re-pin not applied)"
    if [ -n "$EXPECT_CONTROLLER_REQ" ]; then
        [ "$REQ" = "$EXPECT_CONTROLLER_REQ" ] && ok "controller request == expected $EXPECT_CONTROLLER_REQ" || mark_fail "controller request $REQ != expected $EXPECT_CONTROLLER_REQ"
    fi

    # ---- Leg 2: the re-pin applies on the UPGRADE path (rc -> stable) -----
    printf '\n=== Leg 2: downgrade to rc (%s) then upgrade to stable (%s) ===\n' "$RC_VERSION" "$STABLE_VERSION"
    "$APPRAFTER" platform upgrade --to "$RC_VERSION" --cached 2>&1 | tail -3   # downgrade stable->rc is from>=to => Safe/ungated
    wait_platform_version "$RC_VERSION" || mark_fail "platform never reached $RC_VERSION (downgrade)"
    wait_all_apps_synced 9 || mark_fail "apps not Synced+Healthy at rc"
    RC_REQ=$(controller_req_memory); printf '  at rc: controller request = %s (expect 288Mi tuning-only)\n' "$RC_REQ"
    "$APPRAFTER" platform upgrade --to "$STABLE_VERSION" --cached 2>&1 | tail -3
    wait_platform_version "$STABLE_VERSION" || mark_fail "platform never reached $STABLE_VERSION (re-pin upgrade)"
    wait_all_apps_synced 9 || mark_fail "apps not Synced+Healthy after re-pin upgrade"
    assert_argocd_synced_healthy
    UP_REQ=$(controller_req_memory)
    [ "$UP_REQ" != "288Mi" ] && [ "$UP_REQ" = "$REQ" ] && ok "re-pin applied on upgrade: controller request $RC_REQ -> $UP_REQ" || mark_fail "re-pin upgrade: controller request $UP_REQ (rc was $RC_REQ, fresh-stable was $REQ)"
    assert_no_restarts_oomkill
    printf '\n--- settle 2m then final measure ---\n'; sleep 120
    measure "final-stable-upgrade $STABLE_VERSION"
    assert_rss_under_target "${APPCTRL_RSS_MI:-0}"

else
    mark_fail "unknown MODE=$MODE (want: tuning | stable)"
fi

printf '\n=== SUMMARY (mode=%s FAIL=%s) ===\n' "$MODE" "$FAIL"
exit "$FAIL"
