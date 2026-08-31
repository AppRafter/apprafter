#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter image-rollback walk (2.22e / D9 / ADR 0059).
#
# Proves the recovery path for a moving-tag deploy: the operator retains the
# previously resolved digest, `apprafter app rollback` HOLDS the application
# there, and `apprafter app unpin` releases it.
#
# THE ASSERTION THAT MATTERS is P6. A naive fix — set the workload back to the
# older digest without pinning — passes "the pod runs the old digest" and then
# fails within one reconcile, because the operator re-resolves the tag, finds
# the new build again, and rolls forward. So this walk waits out at least two
# full reconcile intervals and asserts the digest is STILL held. An assertion
# satisfied by the transient state is not evidence.
#
# WHY A MANIFEST TAG CHANGE RATHER THAN A PUSHED TAG MOVE. Producing a genuine
# same-tag move needs a registry we can push to. The operator resolves digests
# over HTTPS against webpki roots and has no CA / insecure escape hatch, so an
# in-cluster `registry:2` is unreachable to it — and the failure is SILENT
# (resolution falls back to the verbatim tag and the rollout proceeds), which
# would leave this walk green while testing nothing. Editing the manifest from
# nginx:1.27-alpine to 1.28-alpine drives the SAME retention and shift code
# path with two public tags, no credentials, and no new product surface. The
# same-tag half is already covered by the 2.4h resolution walk.
#
# Required: git, cargo, kubectl, and docker or podman.
#
# Usage:
#   APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/image-rollback-walk.sh
#
# Judge the outcome by READING THE LOG — every phase prints `ok:` lines and
# the run ends with a GREEN banner. Under `sandbox-run` the reported exit code
# is not trustworthy.
#
# Exit codes:
#   0 — walk green
#   1 — assertion failure
#   2 — precondition missing

set -euo pipefail

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

CLUSTER_NAME="apprafter-e2e-imgroll"
FIXTURE_SRC="${REPO_ROOT}/e2e/fixtures/image-rollback-app"
APP_NAME="image-rollback-app"
APP_NS="apprafter"
GIT_DAEMON_PORT="9419"
# Two full operator reconcile intervals plus slack. The operator requeues
# every 60s; a pin that leaks would be undone by the FIRST requeue after the
# workload settled, so one interval would already catch the naive fix — two
# is the margin that makes a pass mean something.
HOLD_WATCH_SECS=150

FAIL=0
mark_fail() { FAIL=1; printf 'FAILED: %s\n' "$1"; }
ok() { printf 'ok: %s\n' "$1"; }

# The pin ships in 2.22e and is UNRELEASED. Against the published operator
# image the pin annotation is simply ignored — the app keeps following its
# tag — so P3 onward would be asserting the DEFECT and this walk would pass by
# describing it. The Argo health branch is in the unpublished chart for the
# same reason and is side-loaded in Phase 1b.
if [ -z "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
    cat >&2 <<'EOF'
ERROR: image-rollback-walk requires APPRAFTER_E2E_LOCAL_OPERATOR=1.

The image pin (ADR 0059) is not published. Against the released operator the
annotation is ignored and the app keeps following its tag — so the walk would
exercise the OLD behaviour and report green while proving nothing.

  APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/image-rollback-walk.sh
EOF
    exit 2
fi

for tool in git cargo kubectl; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    }
done
if ! command -v docker >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
    printf 'ERROR: neither "docker" nor "podman" found on PATH\n' >&2
    exit 2
fi

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
GIT_REPOS_DIR="${TMPDIR_WORK}/git-repos"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"
GIT_DAEMON_PID=""

cleanup() {
    local exit_code=$?
    if [ -n "$GIT_DAEMON_PID" ] && kill -0 "$GIT_DAEMON_PID" 2>/dev/null; then
        kill "$GIT_DAEMON_PID" 2>/dev/null || true
    fi
    pkill -f "git[ -]daemon.*${GIT_DAEMON_PORT}" 2>/dev/null || true
    if [ "$exit_code" -ne 0 ] || [ "$FAIL" -ne 0 ]; then
        printf '\n!!! image-rollback-walk FAILED at %s !!!\n' "$(elapsed)" >&2
        dump_diagnostics
    fi
    if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
        k3d_down "$CLUSTER_NAME" || true
    else
        printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' "$CLUSTER_NAME"
    fi
    rm -rf "$TMPDIR_WORK"
    if [ "$exit_code" -eq 0 ] && [ "$FAIL" -eq 0 ]; then
        printf '\nimage-rollback-walk GREEN in %s\n' "$(elapsed)"
    fi
    exit "$(( exit_code != 0 ? exit_code : FAIL ))"
}
trap cleanup EXIT

apprafter_from_fixture() {
    (
        cd "${FIXTURE_SRC}"
        cargo run \
            --manifest-path "${REPO_ROOT}/cli/Cargo.toml" \
            --quiet \
            --bin apprafter \
            -- "$@"
    )
}

seed_apprafter_state() {
    local kubeconfig_content="$1"
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/k3d/.apprafter"
    cat >"${APPRAFTER_CONFIG_DIR}/config.yaml" <<'YAML'
active_target: k3d
version: 1
YAML
    local kc_escaped
    kc_escaped=$(printf '%s' "$kubeconfig_content" \
        | sed 's/\\/\\\\/g' | sed 's/"/\\"/g' | awk '{printf "%s\\n", $0}')
    cat >"${APPRAFTER_CONFIG_DIR}/state/k3d/.apprafter/state.json" <<STATE
{
  "hetzner_cloud": {
    "server_id": 1,
    "server_name": "k3d-local",
    "ssh_key_ids": [],
    "kubeconfig_yaml": "${kc_escaped}"
  }
}
STATE
}

setup_git_server() {
    local repo_dst="${GIT_REPOS_DIR}/${APP_NAME}"
    mkdir -p "${GIT_REPOS_DIR}"
    cp -r "${FIXTURE_SRC}" "${repo_dst}"
    (
        cd "${repo_dst}"
        git init -b main
        git config user.email "e2e@apprafter.io"
        git config user.name "AppRafter E2E"
        git add .
        git commit -m "feat: initial image-rollback fixture"
    )
    touch "${repo_dst}/.git/git-daemon-export-ok"
    pkill -f "git[ -]daemon.*${GIT_DAEMON_PORT}" 2>/dev/null || true
    git daemon --reuseaddr --base-path="${GIT_REPOS_DIR}" --export-all \
        --port="${GIT_DAEMON_PORT}" --detach "${GIT_REPOS_DIR}"
    GIT_DAEMON_PID=$(pgrep -f "git[ -]daemon.*${GIT_DAEMON_PORT}" | head -1 || true)
    printf '  git daemon started (port %s)\n' "$GIT_DAEMON_PORT"
}

# Argo CD sync state only. Deliberately NOT the Synced+Healthy helper the
# gitops walk uses: after the pin the application is Suspended BY DESIGN, and
# waiting for Healthy there would hang until the timeout and then report a
# failure that is actually the feature working.
wait_argo_synced() {
    local app_name="$1" deadline
    deadline=$(( $(date +%s) + 600 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        local s
        s=$(kubectl -n argocd get applications.argoproj.io "$app_name" \
            -o jsonpath='{.status.sync.status}' 2>/dev/null || true)
        [ "$s" = "Synced" ] && { printf '  Argo Application %s -> Synced\n' "$app_name"; return 0; }
        printf '    %s: sync=%s\n' "$(date +%H:%M:%S)" "$s"
        sleep 10
    done
    printf 'ERROR: %s did not reach Synced within 10 min\n' "$app_name" >&2
    return 1
}

cr_jp() { kubectl -n "$APP_NS" get applications.apprafter.io "$APP_NAME" -o jsonpath="$1" 2>/dev/null; }
argo_jp() { kubectl -n argocd get applications.argoproj.io "$APP_NAME" -o jsonpath="$1" 2>/dev/null; }
deploy_image() {
    kubectl -n "$APP_NS" get deployment "$APP_NAME" \
        -o jsonpath='{.spec.template.spec.containers[0].image}' 2>/dev/null
}
cond_reason() {
    cr_jp '{range .status.conditions[?(@.type=="ImageResolved")]}{.reason}{end}'
}

# ---------------------------------------------------------------
phase "Phase 0: cluster up"
# ---------------------------------------------------------------
k3d_up "$CLUSTER_NAME"
cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
seed_apprafter_state "$(cat "$KUBECONFIG_FILE")"
export APPRAFTER_CONFIG_DIR

phase "Phase 1: cluster-bootstrap"
bootstrap_with_retry

# ---------------------------------------------------------------
phase "Phase 1b: side-load the working-tree operator + the branch Argo health script"
# ---------------------------------------------------------------
builder=podman; command -v podman >/dev/null 2>&1 || builder=docker
build_load_restart() { # <deployment> <operator-subdir>
    local dep="$1" sub="$2" img
    for _ in $(seq 1 60); do
        kubectl -n apprafter-system get deploy "$dep" >/dev/null 2>&1 && break
        sleep 5
    done
    img=$(kubectl -n apprafter-system get deploy "$dep" \
        -o jsonpath='{.spec.template.spec.containers[0].image}')
    printf '  building %s from the working tree (%s) ...\n' "$img" "$builder"
    "$builder" build -f "${REPO_ROOT}/operator/${sub}/Dockerfile" -t "$img" "${REPO_ROOT}/operator"
    cluster_load_image "$CLUSTER_NAME" "$img"
    kubectl -n apprafter-system rollout restart "deploy/${dep}"
    kubectl -n apprafter-system rollout status "deploy/${dep}" --timeout=240s
}
build_load_restart apprafter-operator apprafter-operator
build_load_restart admission-webhook admission-webhook

# Argo CD owns the operator's CRDs and ClusterRole; stop it reverting the
# branch versions. No NEW verb is needed for the pin (the operator reads an
# annotation off an object it already watches), but the branch RBAC ships with
# the branch image on principle — the 2.22b harness gap was exactly this, and
# its symptom was a silent no-op that only the log showed.
# `argocd` is in this list because it OWNS argocd-cm with selfHeal on: without
# disabling it the health-script patch below is reverted within seconds, and
# the tile assertion then fails against the RELEASED script while looking like
# a product failure.
for _app in platform apprafter-operator argocd; do
    kubectl -n argocd patch applications.argoproj.io "$_app" --type=merge \
        -p '{"spec":{"syncPolicy":{"automated":null}}}' >/dev/null 2>&1 || true
done
_yq() { if command -v yq >/dev/null 2>&1; then yq "$@"; else nix run nixpkgs#yq-go -- "$@"; fi; }
helm template apprafter-operator "${REPO_ROOT}/operator/charts/apprafter-operator" \
    | _yq 'select(.kind == "CustomResourceDefinition" or .kind == "ClusterRole")' \
    | kubectl apply --server-side --force-conflicts -f - >/dev/null
retry 12 5 -- kubectl wait --for=condition=Established crd/applications.apprafter.io --timeout=30s
printf '  branch operator + CRDs + RBAC in place\n'

# The `Suspended` branch lives in the platform-stack chart, which this cluster
# pulled from the published channel. Side-load the WORKING-TREE health script
# into argocd-cm so P5 tests the Lua this branch actually ships rather than
# the released one. Extracted from the chart source, never hand-copied — a
# hand-copy would let the file and the assertion drift apart, and then P5
# would be proving a string this repository does not ship.
printf '  patching argocd-cm with the branch health script ...\n'
python3 - "${REPO_ROOT}/platform-stack/cue/component_argocd.cue" >"${TMPDIR_WORK}/health.lua" <<'PY'
import re, sys
src = open(sys.argv[1]).read()
m = re.search(r'"resource\.customizations\.health\.apprafter\.io_Application": """\n(.*?)\n\t+"""', src, re.S)
if not m:
    sys.exit("could not extract the health script from component_argocd.cue")
print('\n'.join(l.lstrip('\t') for l in m.group(1).split('\n')))
PY
kubectl -n argocd get configmap argocd-cm -o json \
    | python3 -c "
import json,sys
cm=json.load(sys.stdin)
cm.setdefault('data',{})['resource.customizations.health.apprafter.io_Application']=open('${TMPDIR_WORK}/health.lua').read()
json.dump(cm,sys.stdout)" \
    | kubectl apply -f - >/dev/null
# PROVE the patch stuck before relying on it. Argo owns this ConfigMap and
# reverts foreign writes; a silently-reverted patch would make P5 fail against
# the RELEASED script and read as a product defect.
if kubectl -n argocd get cm argocd-cm \
    -o jsonpath='{.data.resource\.customizations\.health\.apprafter\.io_Application}' \
    | grep -q 'image.pinned'; then
    printf '  argocd-cm carries the branch health script\n'
else
    printf 'ERROR: the argocd-cm health patch did not stick (Argo reverted it?)\n' >&2
    exit 1
fi
kubectl -n argocd rollout restart statefulset/argocd-application-controller >/dev/null 2>&1 || \
    kubectl -n argocd rollout restart deploy/argocd-application-controller >/dev/null 2>&1 || true
kubectl -n argocd rollout status statefulset/argocd-application-controller --timeout=180s >/dev/null 2>&1 || true
ok "Phase 1b branch operator + branch Argo health script loaded"

phase "Phase 2: git daemon + register the app"
setup_git_server
if [ "$(cluster_runtime)" = "kind" ]; then
    GIT_REPO_URL="git://$(detect_host_gateway_ip):${GIT_DAEMON_PORT}/${APP_NAME}"
else
    GIT_REPO_URL="git://host.k3d.internal:${GIT_DAEMON_PORT}/${APP_NAME}"
fi
retry 6 5 -- git ls-remote "git://127.0.0.1:${GIT_DAEMON_PORT}/${APP_NAME}" >/dev/null
apprafter_from_fixture app add "$GIT_REPO_URL" \
    --name "$APP_NAME" --branch main --path "/" \
    --namespace "$APP_NS" --project apps --no-ping --no-interactive
wait_argo_synced "$APP_NAME"

# ---------------------------------------------------------------
phase "P1: the first resolution is recorded, and there is nothing to roll back to yet"
# ---------------------------------------------------------------
deadline=$(( $(date +%s) + 420 ))
D1=""
while [ "$(date +%s)" -lt "$deadline" ]; do
    D1=$(cr_jp '{.status.image.resolved}')
    [ -n "$D1" ] && break
    sleep 10
done
[ -n "$D1" ] && ok "P1 resolved 1.27 to ${D1}" \
    || { mark_fail "P1 operator never resolved the image — nothing downstream can be tested"; exit 1; }

# `previous` MUST be absent here. If it were already set, every later
# assertion about the rollback target would be reading a value this walk did
# not put there, and P4 would pass for the wrong reason.
PREV0=$(cr_jp '{.status.image.previous.resolved}')
[ -z "$PREV0" ] && ok "P1 no retained digest yet (nothing has moved)" \
    || mark_fail "P1 status.image.previous is already ${PREV0} before anything moved"

kubectl wait --for=condition=Available "deployment/${APP_NAME}" -n "$APP_NS" --timeout=300s >/dev/null
ok "P1 workload Available"

# ---------------------------------------------------------------
phase "P2: the manifest moves to 1.28 — the old digest becomes the rollback target"
# ---------------------------------------------------------------
FIXTURE_COPY="${GIT_REPOS_DIR}/${APP_NAME}/apprafter/Application.cue"
sed -i 's/nginx:1.27-alpine/nginx:1.28-alpine/' "$FIXTURE_COPY"
(
    cd "${GIT_REPOS_DIR}/${APP_NAME}"
    git add apprafter/Application.cue
    git commit -m "chore(e2e): move image 1.27 -> 1.28"
)
kubectl -n argocd annotate applications.argoproj.io "$APP_NAME" \
    "argocd.argoproj.io/refresh=hard" --overwrite >/dev/null
wait_argo_synced "$APP_NAME"

deadline=$(( $(date +%s) + 420 ))
D2=""; PREV=""
while [ "$(date +%s)" -lt "$deadline" ]; do
    D2=$(cr_jp '{.status.image.resolved}')
    PREV=$(cr_jp '{.status.image.previous.resolved}')
    if [ -n "$D2" ] && [ "$D2" != "$D1" ] && [ "$PREV" = "$D1" ]; then break; fi
    printf '    %s: resolved=%s previous=%s\n' "$(date +%H:%M:%S)" "${D2:-<unset>}" "${PREV:-<unset>}"
    sleep 10
done
[ -n "$D2" ] && [ "$D2" != "$D1" ] && ok "P2 resolved 1.28 to ${D2} (differs from 1.27)" \
    || mark_fail "P2 the two tags did not resolve to different digests — the pair is degenerate and nothing below proves anything"
[ "$PREV" = "$D1" ] && ok "P2 retained ${D1} as the rollback target" \
    || mark_fail "P2 status.image.previous is ${PREV:-<unset>}, expected ${D1}"

PREV_TAG=$(cr_jp '{.status.image.previous.tag}')
[ "$PREV_TAG" = "nginx:1.27-alpine" ] && ok "P2 the retained digest carries the tag it came from" \
    || mark_fail "P2 previous.tag is '${PREV_TAG}', expected nginx:1.27-alpine"

# ---------------------------------------------------------------
phase "P3: bare rollback pins to the retained digest"
# ---------------------------------------------------------------
apprafter_from_fixture app rollback "$APP_NAME" --yes 2>&1 | tail -3

PINNED=$(cr_jp '{.metadata.annotations.apprafter\.io/image-pin}')
[ "$PINNED" = "$D1" ] && ok "P3 pin annotation carries ${D1}" \
    || mark_fail "P3 pin annotation is '${PINNED:-<unset>}', expected ${D1}"

# The dedicated field manager is load-bearing: un-pinning prunes exactly what
# this manager owns, so if the write landed under another manager the un-pin
# in P8 would either do nothing or prune something else.
# `--show-managed-fields` is REQUIRED: kubectl strips managedFields from
# `get -o json` by default, so without it this reads an empty list and
# reports no owner. The same omission made two shipped ownership guards
# unreachable, which is how it was found.
MGR=$(kubectl -n "$APP_NS" get applications.apprafter.io "$APP_NAME" -o json --show-managed-fields 2>/dev/null \
    | python3 -c "
import json,sys
d=json.load(sys.stdin)
for e in d.get('metadata',{}).get('managedFields',[]):
    f=e.get('fieldsV1',{}).get('f:metadata',{}).get('f:annotations',{})
    if 'f:apprafter.io/image-pin' in f:
        print(e.get('manager','')); break" 2>/dev/null)
[ "$MGR" = "apprafter-cli-pin" ] && ok "P3 pin owned by the dedicated field manager" \
    || mark_fail "P3 pin owned by '${MGR:-<none>}', expected apprafter-cli-pin"

# ---------------------------------------------------------------
phase "P4: the workload returns to the retained digest"
# ---------------------------------------------------------------
deadline=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    [ "$(deploy_image)" = "$D1" ] && break
    sleep 10
done
[ "$(deploy_image)" = "$D1" ] && ok "P4 Deployment rolled back to ${D1}" \
    || mark_fail "P4 Deployment image is $(deploy_image), expected ${D1}"

deadline=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    RUNNING=$(kubectl -n "$APP_NS" get pods -l "apprafter.io/application=${APP_NAME}" \
        -o jsonpath='{.items[0].spec.containers[0].image}' 2>/dev/null || true)
    [ "$RUNNING" = "$D1" ] && break
    sleep 10
done
[ "$RUNNING" = "$D1" ] && ok "P4 a pod is actually running ${D1}" \
    || mark_fail "P4 the running pod carries '${RUNNING:-<none>}', not the pinned digest"

PINSTATUS=$(cr_jp '{.status.image.pinned.resolved}')
[ "$PINSTATUS" = "$D1" ] && ok "P4 status.image.pinned records the honoured pin" \
    || mark_fail "P4 status.image.pinned is '${PINSTATUS:-<unset>}' — every surface reads this field"

[ "$(cond_reason)" = "Pinned" ] && ok "P4 ImageResolved reason is Pinned" \
    || mark_fail "P4 ImageResolved reason is '$(cond_reason)', expected Pinned"

# ---------------------------------------------------------------
phase "P5: the pin survives an Argo CD sync"
# ---------------------------------------------------------------
# The pin lives on an object Argo renders from Git and manages, so a sync
# could plausibly prune a foreign annotation or report the app OutOfSync
# forever. Per-key SSA ownership says it will not; only a live sync settles
# it, and this project has been wrong before by reasoning about Argo instead
# of observing it.
kubectl -n argocd annotate applications.argoproj.io "$APP_NAME" \
    "argocd.argoproj.io/refresh=hard" --overwrite >/dev/null
sleep 30
wait_argo_synced "$APP_NAME"
STILL=$(cr_jp '{.metadata.annotations.apprafter\.io/image-pin}')
[ "$STILL" = "$D1" ] && ok "P5 pin survived a forced sync" \
    || mark_fail "P5 the sync removed the pin (now '${STILL:-<unset>}') — the annotation home does not hold"

SYNC=$(argo_jp '{.status.sync.status}')
[ "$SYNC" = "Synced" ] && ok "P5 the app is Synced, not permanently OutOfSync" \
    || mark_fail "P5 sync status is '${SYNC}' — the pin makes Argo see permanent drift"

# A permanently-Suspended managed resource hangs a sync operation whose task
# set spans more than one wave or phase. The shipped app shape does not, and
# that is an invariant rather than an accident — so assert it.
OPPHASE=$(argo_jp '{.status.operationState.phase}')
[ "$OPPHASE" = "Succeeded" ] && ok "P5 sync operation completed (Suspended did not hang it)" \
    || mark_fail "P5 operationState.phase is '${OPPHASE}', expected Succeeded"

HEALTH=$(argo_jp '{.status.health.status}')
[ "$HEALTH" = "Suspended" ] && ok "P5 the Argo tile reads Suspended" \
    || mark_fail "P5 Argo health is '${HEALTH}', expected Suspended"

# ---------------------------------------------------------------
phase "P6: THE assertion — the pin still holds after two reconciles"
# ---------------------------------------------------------------
# The naive fix passes everything above and fails here: without a pin the
# operator re-resolves the tag, finds 1.28 again, and rolls forward.
printf '  watching for %ss ...\n' "$HOLD_WATCH_SECS"
watch_deadline=$(( $(date +%s) + HOLD_WATCH_SECS ))
LEAKED=""
while [ "$(date +%s)" -lt "$watch_deadline" ]; do
    cur=$(deploy_image)
    if [ "$cur" != "$D1" ]; then LEAKED="$cur"; break; fi
    sleep 15
done
if [ -z "$LEAKED" ]; then
    ok "P6 held at ${D1} across ${HOLD_WATCH_SECS}s (>= 2 reconciles)"
else
    mark_fail "P6 the pin LEAKED — image moved to ${LEAKED}; the operator rolled the app forward again"
fi

# ---------------------------------------------------------------
phase "P7: the pin is stated in words"
# ---------------------------------------------------------------
# A pin held outside Git is invisible to a reader of the repository, so the
# status surface is the only place the truth exists.
STATUS_OUT=$(apprafter_from_fixture app status "$APP_NAME" 2>&1 || true)
printf '%s\n' "$STATUS_OUT" | grep -qi "pinned" \
    && ok "P7 app status says the app is pinned" \
    || mark_fail "P7 app status never mentions the pin"
printf '%s\n' "$STATUS_OUT" | grep -qi "not following" \
    && ok "P7 app status says it is no longer following the tag" \
    || mark_fail "P7 app status does not say the app stopped following its tag"
printf '%s\n' "$STATUS_OUT" | grep -q "apprafter app unpin" \
    && ok "P7 app status names the way out" \
    || mark_fail "P7 app status does not name the un-pin command"

PLAT_OUT=$(apprafter_from_fixture platform status --cached 2>&1 || true)
printf '%s\n' "$PLAT_OUT" | grep -q "$APP_NAME" \
    && ok "P7 platform status lists the pinned application" \
    || mark_fail "P7 platform status does not list the pinned application"

# ---------------------------------------------------------------
phase "P8: unpin releases it and the app follows the tag again"
# ---------------------------------------------------------------
apprafter_from_fixture app unpin "$APP_NAME" --yes 2>&1 | tail -2

GONE=$(cr_jp '{.metadata.annotations.apprafter\.io/image-pin}')
[ -z "$GONE" ] && ok "P8 the annotation was pruned by the same-manager re-apply" \
    || mark_fail "P8 the pin annotation is still '${GONE}' after unpin"

deadline=$(( $(date +%s) + 300 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    [ "$(deploy_image)" = "$D2" ] && break
    sleep 10
done
[ "$(deploy_image)" = "$D2" ] && ok "P8 the workload rolled forward to ${D2}" \
    || mark_fail "P8 image is $(deploy_image) after unpin, expected ${D2} — the app did not resume following its tag"

[ -z "$(cr_jp '{.status.image.pinned.resolved}')" ] && ok "P8 status.image.pinned cleared" \
    || mark_fail "P8 status.image.pinned survived the unpin"

[ "$(cond_reason)" = "Resolved" ] && ok "P8 ImageResolved reason is Resolved again" \
    || mark_fail "P8 ImageResolved reason is '$(cond_reason)', expected Resolved"

deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    [ "$(argo_jp '{.status.health.status}')" = "Healthy" ] && break
    sleep 10
done
[ "$(argo_jp '{.status.health.status}')" = "Healthy" ] && ok "P8 the Argo tile is Healthy again" \
    || mark_fail "P8 Argo health is '$(argo_jp '{.status.health.status}')' after unpin, expected Healthy"

# ---------------------------------------------------------------
phase "P9: a malformed pin is refused rather than rendered"
# ---------------------------------------------------------------
# The annotation is hand-writable and its value is spliced into the container
# image, so the operator must reject a bad one AND keep deploying — a frozen
# reconcile here would be the ADR 0048 anchor-403 failure repeated.
kubectl -n "$APP_NS" annotate applications.apprafter.io "$APP_NAME" \
    "apprafter.io/image-pin=ghcr.io/somebody-else/web@sha256:$(printf 'a%.0s' {1..64})" \
    --overwrite >/dev/null
deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    [ "$(cond_reason)" = "PinRejected" ] && break
    sleep 10
done
[ "$(cond_reason)" = "PinRejected" ] && ok "P9 a foreign-repository pin is refused, loudly" \
    || mark_fail "P9 ImageResolved reason is '$(cond_reason)', expected PinRejected"
[ "$(deploy_image)" = "$D2" ] && ok "P9 the app kept following its tag while the pin was refused" \
    || mark_fail "P9 image is $(deploy_image) — a refused pin must not change what is deployed"
[ -z "$(cr_jp '{.status.image.pinned.resolved}')" ] && ok "P9 a refused pin is NOT reported as pinned" \
    || mark_fail "P9 status.image.pinned is set for a pin that was refused"

kubectl -n "$APP_NS" annotate applications.apprafter.io "$APP_NAME" \
    "apprafter.io/image-pin-" >/dev/null 2>&1 || true

printf '\n=== SUMMARY (FAIL=%s) ===\n' "$FAIL"
trap - EXIT
if [ -n "$GIT_DAEMON_PID" ] && kill -0 "$GIT_DAEMON_PID" 2>/dev/null; then
    kill "$GIT_DAEMON_PID" 2>/dev/null || true
fi
pkill -f "git[ -]daemon.*${GIT_DAEMON_PORT}" 2>/dev/null || true
if [ "$FAIL" -ne 0 ]; then
    dump_diagnostics
fi
if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    k3d_down "$CLUSTER_NAME" || true
fi
rm -rf "$TMPDIR_WORK"
if [ "$FAIL" -eq 0 ]; then
    printf '\nimage-rollback-walk GREEN in %s\n' "$(elapsed)"
else
    printf '\nimage-rollback-walk RED\n'
fi
exit "$FAIL"
