#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# platformstack-listmap-upgrade-proof.sh — WI-400's proof that PlatformStack
# `status.conditions` can change from an atomic list to a list-map keyed by
# `type` on a live cluster, and that the two field managers that write it then
# prune nothing.
#
# Not a walk and not wired into CI: the change happens once, from the
# `operator/v0.2.52` CRD to the working tree's, and this script is the record
# of what the apiserver does across it. Rerun it when either manager's apply
# changes (`operator-controllers/platform-stack/src/stall.rs`,
# `reconcile::build_status_patch`).
#
#   Leg a  kubectl only, ~2 min. Every apply the operator can make, under the
#          operator's own field managers, from every ownership state a live
#          upgrade, a rollback and a re-upgrade leave behind, and the two
#          hazards the operator is built around: an apply by key from a list
#          owned whole keeps what it leaves out, and a stall apply under the
#          atomic CRD replaces the list.
#   Leg b  ~4 min more. The working-tree operator binary, run outside the
#          cluster against the same kind apiserver: its first pass takes a
#          stack written under the atomic CRD by key and retires a condition
#          in the same pass, it removes a ReconcileStalled after a pass that
#          finishes, it leaves alone (once, no write loop) one that another
#          field manager holds, and it writes nothing while the atomic CRD is
#          served again.
#
# Usage:
#   PROOF_OPERATOR_BIN=<path to apprafter-operator> e2e/platformstack-listmap-upgrade-proof.sh
#   PROOF_LEG=a e2e/platformstack-listmap-upgrade-proof.sh       # leg a only
#
# Requires kind (+ docker or podman; KIND_EXPERIMENTAL_PROVIDER=podman is
# passed through), kubectl, jq and git. The cluster has a private kubeconfig:
# nothing here reads or writes the caller's.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LEG="${PROOF_LEG:-ab}"
CLUSTER="apprafter-listmap-proof"
OLD_TAG="operator/v0.2.52"
CRD_PATH="operator/charts/apprafter-operator/templates/crd-platformstack.yaml"
STALL="apprafter-reconcile-deadline"
CONTROLLER="platform-controller"
: "${APPRAFTER_KIND_NODE_IMAGE:=kindest/node:v1.36.4@sha256:099e049362a1526b2db71494e1947aae99bd16290d7c895f2b7ea312e3cbfaed}"

case "$LEG" in
    a | ab) ;;
    *) echo "PROOF_LEG must be a or ab, not '$LEG'" >&2; exit 2 ;;
esac
if [[ "$LEG" == ab && ! -x "${PROOF_OPERATOR_BIN:-}" ]]; then
    echo "leg b needs PROOF_OPERATOR_BIN, the working tree's apprafter-operator binary" >&2
    echo "(or PROOF_LEG=a to run leg a alone)" >&2
    exit 2
fi
for tool in kind kubectl jq git; do
    command -v "$tool" >/dev/null || { echo "missing tool: $tool" >&2; exit 2; }
done

WORK="$(mktemp -d)"
KUBECONFIG="$WORK/kubeconfig"
export KUBECONFIG
OP_PID=""
# shellcheck disable=SC2329  # invoked by the trap
cleanup() {
    if [[ -n "$OP_PID" ]]; then kill "$OP_PID" 2>/dev/null || true; fi
    kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT

fail() {
    echo "FAIL: $*" >&2
    if [[ -f "$WORK/operator.log" ]]; then
        echo "--- operator log (last 40 lines)" >&2
        tail -40 "$WORK/operator.log" >&2
    fi
    exit 1
}

expect() { # label actual want
    if [[ "$2" == "$3" ]]; then
        echo "  ok: $1"
    else
        fail "$1: got '$2', want '$3'"
    fi
}

K() { kubectl --context "kind-$CLUSTER" "$@"; }

# The CRD from a git revision, or the working tree with `-`. The chart's
# label line is a Helm directive and the only line that is not YAML.
crd_file() { # rev out
    if [[ "$1" == - ]]; then
        grep -v '{{' "$ROOT/$CRD_PATH" >"$2"
    else
        git -C "$ROOT" show "$1:$CRD_PATH" | grep -v '{{' >"$2"
    fi
}

use_crd() { # file
    K apply --server-side --force-conflicts --field-manager=argocd-controller -f "$1" >/dev/null
    K wait --for=condition=Established --timeout=60s crd/platformstacks.apprafter.io >/dev/null
}

cond() { # type status
    printf '{"type":"%s","status":"%s","reason":"R","message":"m","lastTransitionTime":"2026-10-02T00:00:00Z"}' "$1" "$2"
}

stack() { # namespace name
    K apply --server-side --field-manager=apprafter-cli -f - >/dev/null <<YAML
apiVersion: apprafter.io/v1alpha1
kind: PlatformStack
metadata: {name: $2, namespace: $1}
spec:
  channel: stable
  pin: "0.2.80"
  autoUpgrade: false
  source:
    upstream: oci://ghcr.io/apprafter/platform-stack
    repoURL: oci://ghcr.io/apprafter/platform-stack
    checkInterval: 6h
  values: {tier: 1}
YAML
}

# One status apply, as the operator makes it: SSA on the status
# subresource, forced, under `manager`, carrying `conditions` alone.
status_apply() { # namespace name manager conditions-json
    printf '{"apiVersion":"apprafter.io/v1alpha1","kind":"PlatformStack","metadata":{"name":"%s","namespace":"%s"},"status":{"conditions":%s}}' \
        "$2" "$1" "$4" |
        K apply --server-side --subresource=status --field-manager="$3" --force-conflicts -f - >/dev/null
}

types() { # namespace name
    K -n "$1" get platformstack "$2" \
        -o jsonpath='{range .status.conditions[*]}{.type}={.status} {end}' | sed 's/ $//'
}

sorted_types() { # namespace name; where the merged order is not the point
    K -n "$1" get platformstack "$2" -o json |
        jq -r '[.status.conditions[]? | "\(.type)=\(.status)"] | sort | join(" ")'
}

# What `manager` owns of status.conditions: the condition types it owns by
# key, WHOLE for an ownership of the whole list, - for none.
owns() { # namespace name manager
    K -n "$1" get platformstack "$2" --show-managed-fields -o json | jq -r --arg m "$3" '
        [.metadata.managedFields[]? | select(.subresource == "status" and .manager == $m)
         | .fieldsV1."f:status"."f:conditions"] | first
        | if . == null then "-"
          elif . == {} then "WHOLE"
          else [keys[] | select(startswith("k:")) | ltrimstr("k:") | fromjson | .type]
               | sort | join(",") end'
}

rv() { K -n "$1" get platformstack "$2" -o jsonpath='{.metadata.resourceVersion}'; }

echo "=== kind cluster $CLUSTER"
kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true
kind create cluster --name "$CLUSTER" --image "$APPRAFTER_KIND_NODE_IMAGE" --wait 90s >/dev/null
crd_file "$OLD_TAG" "$WORK/crd-old.yaml"
crd_file - "$WORK/crd-new.yaml"
expect "the $OLD_TAG CRD's conditions are atomic" \
    "$(grep -c 'x-kubernetes-list-type' "$WORK/crd-old.yaml" || true)" "0"
expect "the working tree's conditions are a list-map keyed by type" \
    "$(grep -A1 'x-kubernetes-list-map-keys' "$WORK/crd-new.yaml" | tail -1 | tr -d ' ')" "-type"

NS=listmap-proof
K create namespace "$NS" >/dev/null

echo "=== leg a: the atomic CRD"
use_crd "$WORK/crd-old.yaml"
stack "$NS" upgraded
status_apply "$NS" upgraded "$CONTROLLER" "[$(cond Ready True),$(cond Synced True),$(cond BackupHealthy True)]"
expect "under the atomic CRD platform-controller owns the list whole" \
    "$(owns "$NS" upgraded "$CONTROLLER")" "WHOLE"
stack "$NS" duplicated
status_apply "$NS" duplicated "$CONTROLLER" "[$(cond Ready True),$(cond Ready False),$(cond Synced True)]"
expect "an atomic list takes a duplicate type" "$(types "$NS" duplicated)" \
    "Ready=True Ready=False Synced=True"
for name in stalled-first retired; do
    stack "$NS" "$name"
    status_apply "$NS" "$name" "$CONTROLLER" "[$(cond Ready True),$(cond Synced True),$(cond BackupHealthy True)]"
done

echo "=== leg a: the list-map CRD (the upgrade)"
use_crd "$WORK/crd-new.yaml"
expect "the CRD change alone moves no ownership" "$(owns "$NS" upgraded "$CONTROLLER")" "WHOLE"
expect "the CRD change alone moves no condition" "$(types "$NS" upgraded)" \
    "Ready=True Synced=True BackupHealthy=True"
status_apply "$NS" upgraded "$CONTROLLER" "[$(cond Ready True),$(cond Synced True),$(cond BackupHealthy True)]"
expect "platform-controller's first apply owns each condition by key" \
    "$(owns "$NS" upgraded "$CONTROLLER")" "BackupHealthy,Ready,Synced"
expect "and keeps every condition" "$(types "$NS" upgraded)" \
    "Ready=True Synced=True BackupHealthy=True"
status_apply "$NS" upgraded "$STALL" "[$(cond ReconcileStalled True)]"
expect "the stall manager's apply adds its condition beside them" "$(types "$NS" upgraded)" \
    "Ready=True Synced=True BackupHealthy=True ReconcileStalled=True"
expect "and owns that one by key" "$(owns "$NS" upgraded "$STALL")" "ReconcileStalled"
status_apply "$NS" upgraded "$CONTROLLER" "[$(cond Ready False),$(cond Synced True),$(cond BackupHealthy True)]"
expect "platform-controller's apply without it leaves it" "$(types "$NS" upgraded)" \
    "Ready=False Synced=True BackupHealthy=True ReconcileStalled=True"
before="$(rv "$NS" upgraded)"
status_apply "$NS" upgraded "$STALL" "[$(cond ReconcileStalled True)]"
expect "an identical stall apply is no change (no watch event)" "$(rv "$NS" upgraded)" "$before"
status_apply "$NS" upgraded "$STALL" "[]"
expect "the stall manager's empty apply removes only its own condition" "$(types "$NS" upgraded)" \
    "Ready=False Synced=True BackupHealthy=True"
expect "and leaves it no ownership" "$(owns "$NS" upgraded "$STALL")" "-"
status_apply "$NS" upgraded "$CONTROLLER" "[$(cond Ready False),$(cond Synced True)]"
expect "an apply by key that leaves out a condition it owns by key removes it" \
    "$(types "$NS" upgraded)" "Ready=False Synced=True"

# What the operator's first write after the upgrade must not do: from a list
# owned whole, an apply by key keeps what it leaves out, for good.
status_apply "$NS" retired "$CONTROLLER" "[$(cond Ready True),$(cond Synced True)]"
expect "an apply by key from a list owned whole keeps a condition it leaves out" \
    "$(types "$NS" retired)" "Ready=True Synced=True BackupHealthy=True"
expect "owned by nobody: platform-controller owns only what it sent" \
    "$(owns "$NS" retired "$CONTROLLER")" "Ready,Synced"
status_apply "$NS" retired "$CONTROLLER" "[$(cond Ready False),$(cond Synced True)]"
expect "and no later apply by key removes it" "$(types "$NS" retired)" \
    "Ready=False Synced=True BackupHealthy=True"
# What the operator does about it: re-apply the list as read, then leave it out.
status_apply "$NS" retired "$CONTROLLER" "[$(cond Ready False),$(cond Synced True),$(cond BackupHealthy True)]"
expect "applying it as it is makes it platform-controller's" \
    "$(owns "$NS" retired "$CONTROLLER")" "BackupHealthy,Ready,Synced"
status_apply "$NS" retired "$CONTROLLER" "[$(cond Ready False),$(cond Synced True)]"
expect "and the apply after it that leaves it out removes it" "$(types "$NS" retired)" \
    "Ready=False Synced=True"

# A cut before platform-controller's first write after the upgrade.
status_apply "$NS" stalled-first "$STALL" "[$(cond ReconcileStalled True)]"
expect "a stall apply into a list platform-controller owns whole adds beside it" \
    "$(types "$NS" stalled-first)" "Ready=True Synced=True BackupHealthy=True ReconcileStalled=True"
expect "and leaves platform-controller owning the list whole" \
    "$(owns "$NS" stalled-first "$CONTROLLER")" "WHOLE"
before="$(rv "$NS" stalled-first)"
status_apply "$NS" stalled-first "$STALL" "[$(cond ReconcileStalled True)]"
expect "an identical stall apply there is no change (a stall that goes on)" \
    "$(rv "$NS" stalled-first)" "$before"
status_apply "$NS" stalled-first "$CONTROLLER" "[$(cond Ready True),$(cond Synced True),$(cond BackupHealthy True)]"
expect "the stall survives platform-controller's first apply by key" "$(types "$NS" stalled-first)" \
    "Ready=True Synced=True BackupHealthy=True ReconcileStalled=True"
expect "which takes its own conditions by key" \
    "$(owns "$NS" stalled-first "$CONTROLLER")" "BackupHealthy,Ready,Synced"
status_apply "$NS" stalled-first "$STALL" "[]"
expect "and the stall manager's empty apply then removes it" "$(types "$NS" stalled-first)" \
    "Ready=True Synced=True BackupHealthy=True"

# A cut on the very first pass, before platform-controller has written.
stack "$NS" fresh
status_apply "$NS" fresh "$STALL" "[$(cond ReconcileStalled True)]"
expect "a stall apply on a stack with no status yet sets it alone" "$(types "$NS" fresh)" \
    "ReconcileStalled=True"
status_apply "$NS" fresh "$CONTROLLER" "[$(cond Ready True)]"
expect "platform-controller's first apply adds its conditions beside it" \
    "$(sorted_types "$NS" fresh)" "Ready=True ReconcileStalled=True"

expect "a duplicate type written under the atomic CRD is still read" "$(types "$NS" duplicated)" \
    "Ready=True Ready=False Synced=True"
status_apply "$NS" duplicated "$CONTROLLER" "[$(cond Ready True),$(cond Synced True)]"
expect "an apply with one condition per type heals it" "$(types "$NS" duplicated)" \
    "Ready=True Synced=True"
if status_apply "$NS" duplicated "$CONTROLLER" \
    "[$(cond Ready True),$(cond Ready False),$(cond Synced True)]" 2>"$WORK/dup.err"; then
    fail "an apply naming one type twice was accepted"
fi
expect "an apply naming one type twice is refused" \
    "$(grep -c 'duplicate entries for key \[type="Ready"\]' "$WORK/dup.err")" "1"

stack "$NS" held
status_apply "$NS" held "$CONTROLLER" "[$(cond Ready True)]"
status_apply "$NS" held "$STALL" "[$(cond ReconcileStalled True)]"
status_apply "$NS" held listmap-proof-holder "[$(cond ReconcileStalled True)]"
status_apply "$NS" held "$STALL" "[]"
expect "a stall another manager co-owns survives the stall manager's empty apply" \
    "$(types "$NS" held)" "Ready=True ReconcileStalled=True"
status_apply "$NS" held listmap-proof-holder "[]"
expect "and goes with that manager's" "$(types "$NS" held)" "Ready=True"

for name in rolledback rolledback-twice hazard; do
    stack "$NS" "$name"
    status_apply "$NS" "$name" "$CONTROLLER" "[$(cond Ready True)]"
    status_apply "$NS" "$name" "$STALL" "[$(cond ReconcileStalled True)]"
done

echo "=== leg a: back to the atomic CRD (a rollback while stalled)"
use_crd "$WORK/crd-old.yaml"
expect "the rollback alone moves no condition" "$(types "$NS" rolledback)" \
    "Ready=True ReconcileStalled=True"
# The older operator copies every condition it reads into its apply.
status_apply "$NS" rolledback "$CONTROLLER" "[$(cond Ready True),$(cond ReconcileStalled True)]"
expect "the older operator carries the stall forward" "$(types "$NS" rolledback)" \
    "Ready=True ReconcileStalled=True"
# A second write that changes a condition takes the whole list from the
# stall manager too.
status_apply "$NS" rolledback-twice "$CONTROLLER" "[$(cond Ready True),$(cond ReconcileStalled True)]"
status_apply "$NS" rolledback-twice "$CONTROLLER" "[$(cond Ready False),$(cond ReconcileStalled True)]"
expect "and keeps carrying it through a change" "$(types "$NS" rolledback-twice)" \
    "Ready=False ReconcileStalled=True"
# What the stall manager's CRD check is for: the CRD switch leaves the
# by-key ownership in place, so managedFields cannot tell the lists apart.
expect "the rollback alone leaves platform-controller's ownership by key" \
    "$(owns "$NS" hazard "$CONTROLLER")" "Ready"
expect "and the stall manager's" "$(owns "$NS" hazard "$STALL")" "ReconcileStalled"
status_apply "$NS" hazard "$STALL" "[$(cond ReconcileStalled True)]"
expect "under the atomic CRD a stall apply replaces the whole list" \
    "$(types "$NS" hazard)" "ReconcileStalled=True"

echo "=== leg a: the list-map CRD again (the re-upgrade)"
use_crd "$WORK/crd-new.yaml"
status_apply "$NS" rolledback "$CONTROLLER" "[$(cond Ready True)]"
expect "platform-controller's apply by key leaves the stall" "$(types "$NS" rolledback)" \
    "Ready=True ReconcileStalled=True"
expect "owned by key by platform-controller: its own condition only" \
    "$(owns "$NS" rolledback "$CONTROLLER")" "Ready"
expect "the stall manager is left owning the list whole" "$(owns "$NS" rolledback "$STALL")" "WHOLE"
status_apply "$NS" rolledback "$STALL" "[]"
expect "its empty apply then removes nothing" "$(types "$NS" rolledback)" \
    "Ready=True ReconcileStalled=True"
expect "and drops its ownership of the whole list" "$(owns "$NS" rolledback "$STALL")" "-"
status_apply "$NS" rolledback "$STALL" "[$(cond ReconcileStalled True)]"
expect "applying the stall as it is adopts it" "$(owns "$NS" rolledback "$STALL")" "ReconcileStalled"
status_apply "$NS" rolledback "$STALL" "[]"
expect "and the empty apply after it removes it" "$(types "$NS" rolledback)" "Ready=True"

status_apply "$NS" rolledback-twice "$CONTROLLER" "[$(cond Ready True)]"
expect "after two older writes the stall is left too" "$(types "$NS" rolledback-twice)" \
    "Ready=True ReconcileStalled=True"
expect "owned by nobody" "$(owns "$NS" rolledback-twice "$STALL")" "-"
status_apply "$NS" rolledback-twice "$STALL" "[]"
expect "an empty apply alone keeps it" "$(types "$NS" rolledback-twice)" \
    "Ready=True ReconcileStalled=True"
status_apply "$NS" rolledback-twice "$STALL" "[$(cond ReconcileStalled True)]"
status_apply "$NS" rolledback-twice "$STALL" "[]"
expect "adopted, then let go, it goes" "$(types "$NS" rolledback-twice)" "Ready=True"

if [[ "$LEG" == a ]]; then
    echo "GREEN (leg a)"
    exit 0
fi

echo "=== leg b: the working-tree operator against a stack written under the atomic CRD"
K create namespace apprafter-system >/dev/null
K create namespace argocd >/dev/null
K apply --server-side -f - >/dev/null <<'YAML'
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata: {name: applications.argoproj.io}
spec:
  group: argoproj.io
  names: {kind: Application, listKind: ApplicationList, plural: applications, singular: application}
  scope: Namespaced
  versions:
    - name: v1alpha1
      served: true
      storage: true
      schema: {openAPIV3Schema: {type: object, x-kubernetes-preserve-unknown-fields: true}}
YAML
K wait --for=condition=Established --timeout=60s crd/applications.argoproj.io >/dev/null
# The root Application, settled on the pin: synced, healthy, its source
# platform-controller's. No Argo CD runs here; nothing syncs it.
K apply --server-side --field-manager="$CONTROLLER" -f - >/dev/null <<'YAML'
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata: {name: platform, namespace: argocd}
spec:
  source:
    repoURL: oci://ghcr.io/apprafter/platform-stack
    chart: platform-stack
    targetRevision: "0.2.80"
    helm: {valuesObject: {tier: 1}}
status:
  sync: {status: Synced}
  health: {status: Healthy}
  operationState: {phase: Succeeded}
YAML
for f in "$ROOT"/operator/charts/apprafter-operator/templates/crd-*.yaml; do
    [[ "$(basename "$f")" == crd-platformstack.yaml ]] && continue
    grep -v '{{' "$f" | K apply --server-side --force-conflicts --field-manager=argocd-controller -f - >/dev/null
done
use_crd "$WORK/crd-old.yaml"
stack apprafter-system default
# A status the older operator wrote: settled on 0.2.80, a check far in the
# future so this pass asks no registry anything, every condition it writes.
# Its BackupHealthy is from before backups were turned off: this pass retires
# it, in the first write after the upgrade.
printf '{"apiVersion":"apprafter.io/v1alpha1","kind":"PlatformStack","metadata":{"name":"default","namespace":"apprafter-system"},"status":{"currentVersion":"0.2.80","targetVersion":"0.2.80","availableVersion":"0.2.80","lastUpstreamCheck":"2099-01-01T00:00:00Z","versionHistory":[{"version":"0.2.80","appliedAt":"2026-09-20T00:00:00Z","outcome":"succeeded"}],"conditions":[%s,%s,%s,%s,%s,%s,%s,%s]}}' \
    "$(cond Ready True)" "$(cond Synced True)" "$(cond UpstreamReachable True)" \
    "$(cond YankedVersion False)" "$(cond MigrationPending False)" \
    "$(cond UpgradeAvailable False)" "$(cond UnauthorizedSourceModification False)" \
    "$(cond BackupHealthy True)" |
    K apply --server-side --subresource=status --field-manager="$CONTROLLER" --force-conflicts -f - >/dev/null
SETTLED="MigrationPending,Ready,Synced,UnauthorizedSourceModification,UpgradeAvailable,UpstreamReachable,YankedVersion"
use_crd "$WORK/crd-new.yaml"
expect "the stack starts owned whole" "$(owns apprafter-system default "$CONTROLLER")" "WHOLE"

start_operator() {
    POD_NAME=listmap-proof HTTP_PORT=18089 RUST_LOG=info \
        "$PROOF_OPERATOR_BIN" >>"$WORK/operator.log" 2>&1 &
    OP_PID=$!
}
stop_operator() {
    kill "$OP_PID" 2>/dev/null || true
    wait "$OP_PID" 2>/dev/null || true
    OP_PID=""
}
start_operator

wait_for() { # label seconds command...
    local label="$1" secs="$2"
    shift 2
    for _ in $(seq 1 "$secs"); do
        if "$@"; then echo "  ok: $label"; return 0; fi
        kill -0 "$OP_PID" 2>/dev/null || fail "the operator exited"
        sleep 1
    done
    fail "$label: not within ${secs}s"
}
owned_by_key() { [[ ",$(owns apprafter-system default "$CONTROLLER")," == *,Ready,* ]]; }
stall_gone() { ! types apprafter-system default | grep -q ReconcileStalled; }
backup_gone() { ! types apprafter-system default | grep -q BackupHealthy; }
logged() { grep -qF "$1" "$WORK/operator.log"; }
type_set() {
    K -n apprafter-system get platformstack default -o json |
        jq -r '[.status.conditions[].type] | sort | join(",")'
}
holds_settled() { # every settled type still present
    local have
    have="$(type_set)"
    local t
    for t in ${SETTLED//,/ }; do
        [[ ",$have," == *",$t,"* ]] || return 1
    done
}

wait_for "the operator's first pass re-applies the conditions by key" 90 owned_by_key
wait_for "and its next write retires BackupHealthy, backups being off" 30 backup_gone
holds_settled || fail "a condition was lost: $(type_set)"
echo "  ok: every condition the older operator wrote is still there"
expect "platform-controller does not own ReconcileStalled" \
    "$(owns apprafter-system default "$CONTROLLER" | grep -c ReconcileStalled || true)" "0"

# A cut, as reconcile_with_deadline reports it (stall::mark's body).
printf '{"apiVersion":"apprafter.io/v1alpha1","kind":"PlatformStack","metadata":{"name":"default"},"status":{"conditions":[{"type":"ReconcileStalled","status":"True","reason":"ReconcileTimedOut","message":"the last reconcile did not finish within 120s and was abandoned; the other conditions are from the last reconcile that finished","lastTransitionTime":"%s"}]}}' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" |
    K -n apprafter-system apply --server-side --subresource=status --field-manager="$STALL" \
        --force-conflicts -f - >/dev/null
wait_for "the next pass that finishes removes ReconcileStalled" 60 stall_gone
wait_for "and says so" 10 logged "ReconcileStalled removed: a reconcile finished"
expect "the stall manager is left owning nothing" "$(owns apprafter-system default "$STALL")" "-"
holds_settled || fail "a condition was lost: $(type_set)"
echo "  ok: and every other condition stays"

status_apply apprafter-system default listmap-proof-holder "[$(cond ReconcileStalled True)]"
wait_for "a ReconcileStalled another manager holds is left, and named" 60 \
    logged "ReconcileStalled kept: another field manager holds it"
before="$(rv apprafter-system default)"
sleep 20
expect "with no write loop: the stack does not change for 20s" "$(rv apprafter-system default)" "$before"
expect "the stall is still there" "$(types apprafter-system default | grep -c ReconcileStalled || true)" "1"
status_apply apprafter-system default listmap-proof-holder "[]"
wait_for "it goes when that manager lets it go" 30 stall_gone

echo "=== leg b: the atomic CRD served again under the running operator (a rollback)"
stop_operator
status_apply apprafter-system default "$STALL" "[$(cond ReconcileStalled True)]"
use_crd "$WORK/crd-old.yaml"
owned_by_key || fail "the rollback moved platform-controller's ownership: $(owns apprafter-system default "$CONTROLLER")"
echo "  ok: the rollback leaves the ownership by key, as in leg a"
held="$(types apprafter-system default)"
start_operator
wait_for "a pass under the atomic CRD keeps ReconcileStalled, and says why" 90 \
    logged "ReconcileStalled kept: the served CRD keeps status.conditions atomic"
expect "and writes no condition away" "$(types apprafter-system default)" "$held"
before="$(rv apprafter-system default)"
sleep 20
expect "with no write loop: the stack does not change for 20s" "$(rv apprafter-system default)" "$before"
use_crd "$WORK/crd-new.yaml"
K -n apprafter-system annotate platformstack default listmap-proof/poke="$(date +%s)" --overwrite >/dev/null
wait_for "back on the list-map CRD, the next pass removes it" 60 stall_gone
holds_settled || fail "a condition was lost: $(type_set)"
echo "  ok: and every other condition stays"

echo "=== leg b: a condition an older operator left behind in the upgrade window"
# The older operator's two writes around the CRD's sync wave, each of the
# whole status: its last under the atomic CRD still reports BackupHealthy; its
# first under the list-map CRD, backups having been turned off meanwhile,
# leaves it out, and from its whole-list ownership that apply keeps it.
older_write() { # jq program on .status.conditions
    K -n apprafter-system get platformstack default -o json |
        jq -c "{apiVersion, kind, metadata: {name: .metadata.name, namespace: .metadata.namespace}, status: (.status | .conditions |= ($1))}" |
        K apply --server-side --subresource=status --field-manager="$CONTROLLER" --force-conflicts -f - >/dev/null
}
stop_operator
use_crd "$WORK/crd-old.yaml"
older_write ". + [$(cond BackupHealthy True)]"
use_crd "$WORK/crd-new.yaml"
older_write 'map(select(.type != "BackupHealthy"))'
expect "the older operator's apply by key leaves BackupHealthy behind" \
    "$(types apprafter-system default | grep -c BackupHealthy || true)" "1"
expect "owned by nobody" "$(owns apprafter-system default "$CONTROLLER" | grep -c BackupHealthy || true)" "0"
start_operator
wait_for "the operator's next write adopts it, and the one after removes it" 90 backup_gone
holds_settled || fail "a condition was lost: $(type_set)"
echo "  ok: and every other condition stays"

echo "GREEN (legs a and b)"
