#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter needs.jetstream-walk e2e — the live-cluster proof for 2.5d
# (ADR 0061), on a local kind cluster (plan item 2.5 part B closure).
#
# Every measurement of the account/permission model up to this point came
# from podman running a bare `nats-server` (nats_accounts.rs's own gated
# tests). That settles permission SEMANTICS, but it cannot prove anything
# about the parts that only exist inside Kubernetes: whether the component
# actually turns on, whether the provisioner's Secret actually reaches the
# pod through the chart's `$include` + volume-mount arrangement, and above
# all whether the config-reloader's SIGHUP actually fires when the
# accounts Secret changes. ADR 0061 §1's entire deployment design rests on
# that reload working; this walk is the first time it is observed.
#
# Six phases, matching the coordinator's own numbering:
#
#   1. Chart installed, component DISABLED — no NATS pod exists (the lazy-
#      enablement promise).
#   2. Apply an Application with `needs: {jetstream: {dynamicStreams:
#      true}}`. Observe, IN ORDER: the override flips, the
#      apprafter.io/nats-auto-enabled annotation is stamped, the
#      nats-accounts Secret appears, the StatefulSet starts, the claim
#      reaches Ready=True.
#   3. Connect AS THE CLAIM USER from inside the cluster, using the
#      connection Secret's own values, WITH the inbox prefix set — create
#      a stream, publish, consume.
#   4. The negative: connect WITHOUT the inbox prefix and confirm it
#      fails. A NOPERM delivers no error signal, so this presents as a
#      timeout, not a permission error — time it.
#   5/6. A second Application in the SAME namespace: both users land in
#      ONE account, neither reaches the other's subjects, the FIRST
#      application's connection SURVIVES the whole-file rewrite the
#      second claim triggers, and the new user works WITHOUT a pod
#      restart (the hot reload ADR 0061 §1's `$include` design rests on).
#
# Plus, while a cluster exists: a claim declaring `streams: [...]` parks
# at Ready=False/AwaitingStreamCreation rather than going ready (2.5d part
# 2's closing fix, observed here rather than only unit-tested), and the
# operator log carries no forbidden-verb complaint (the class an RBAC miss
# manifests as: a reconcile that silently never progresses).
#
# UNPUBLISHED-COMPONENT SUBSTITUTION — read this before touching the
# script
# -------------------------------------------------------------------
# `component_nats.cue` / `component_nack.cue` / the `jetstream-integrated`
# ServiceProvider seed (2.5c) have never been published: `platform-stack`
# is its own OCI-published chart series (see CLAUDE.md), and a monorepo
# branch commit does not publish it. `cluster-bootstrap` pulls the
# PUBLISHED chart, so PlatformController's own render of
# `spec.overrides.nats.enabled=true` has no "nats" component to turn on —
# there is nothing published for Argo CD to sync, exactly the same gap
# `needs-disk-walk.sh` hit for `disk-local` and `needs-pg-walk.sh` /
# `needs-disk-walk.sh` hit for the operator image + CRDs.
#
# The established fix for an unpublished CR-shaped artifact (a
# ServiceProvider) is to seed it by hand, mirroring the branch CUE
# verbatim (Phase 1c below, `jetstream-integrated`) — that is a complete
# substitute, because a ServiceProvider is a plain CR the scheduler reads
# directly, with no chart or Argo CD involved at all.
#
# The NATS component itself is NOT that simple: it is a whole Helm chart
# Argo CD would render and sync. There is no equally clean way to "seed
# it by hand" while ALSO exercising the real override-driven pipeline
# (PlatformController rendering a "nats" Argo Application from the
# override). So this walk draws an explicit, disclosed line:
#
#   REAL, exercised for true:  the provisioner's own merge-patch of
#     spec.overrides.nats.enabled + the annotation stamp (Phase 4) — code
#     this branch wrote, observed against the REAL PlatformStack object.
#   SUBSTITUTED:  the StatefulSet coming into existence. Once (and only
#     once) the walk has observed the override flip for real, it applies
#     the `nats`/`nats-io/k8s` chart's OWN rendered manifests by hand
#     (Phase 4b, using component_nats.cue's exact pinned values) —
#     standing in for the Argo CD sync that would otherwise create it.
#     Everything downstream of that point (the StatefulSet mounting the
#     provisioner's REAL Secret, the reloader picking up its REAL
#     content, SIGHUP, the provisioner's own verify step) is then
#     exercised for real, against a REAL nats-server + reloader pair —
#     this is NOT a stand-in for those questions, only for "does
#     PlatformController correctly render a nats Application from an
#     override against a published chart," which is generic
#     platform-stack plumbing already exercised by every other component
#     in prior phases of this project, not something 2.5d's own tasks
#     touched.
#
# Put plainly, one more time: a green run of this script proves the
# BUILT path (the working-tree operator/webhook + a hand-applied render
# of the chart's own manifests), not the PUBLISHED one (Argo CD pulling
# platform-stack from ghcr.io and syncing the nats component itself). A
# green walk that quietly tested a different path than what a real
# cluster runs would be its own version of the bug this file exists to
# catch — so this is called out again, loudly, at its own call site
# (Phase 4b) and in the walk's own report. It is the single most
# important thing to understand about what this script does and does not
# prove, and it stops being true only once platform-stack actually
# publishes the nats component.
#
# Required: kind (or k3d), a container runtime (podman or docker), cargo,
# kubectl, helm — all satisfied inside `nix develop`.
#
# APPRAFTER_E2E_LOCAL_OPERATOR=1 is MANDATORY: every line of code this
# walk exercises (Backend::Nats, the connection/accounts Secret writers,
# the AwaitingStreamCreation gate, the branch CRD's needs.jetstream /
# ResourceClaim.spec.jetstream fields, the branch RBAC's apps/statefulsets
# rule) is unreleased.
#
# Exit codes:
#   0 — walk green
#   1 — assertion failure
#   2 — precondition missing

set -euo pipefail

# ---------------------------------------------------------------
# Source shared helpers
# ---------------------------------------------------------------

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-nats-walk"

APP_NS="demo"

APP1="walkapp"                       # dynamicStreams:true, no declared streams
CLAIM1="walkapp-jetstream"           # <app>-jetstream (ADR 0061 §6)
CONN1="walkapp-jetstream-conn"

APP2="walkapp2"                      # second app, SAME namespace (Phase 5/6)
CLAIM2="walkapp2-jetstream"
CONN2="walkapp2-jetstream-conn"

APP3="streamapp"                     # declares streams — must park (part-2 fix)
CLAIM3="streamapp-jetstream"

CLAIM_RES="resourceclaim.apprafter.io"

PROVIDER="jetstream-integrated"
NATS_NS="nats-system"
NATS_STS="nats"
ACCOUNTS_SECRET="nats-accounts"
PLATFORMSTACK_NS="apprafter-system"
PLATFORMSTACK_NAME="default"
AUTO_ANNOTATION="apprafter.io/nats-auto-enabled"
RETAINED_NS="apprafter-system"       # apprafter-system, mirrors the other walks' name

NATS_BOX_POD="nats-box"

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------

for tool in cargo kubectl helm; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done

if ! command -v docker >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
    printf 'ERROR: neither "docker" nor "podman" found on PATH\n' >&2
    exit 2
fi

if [ -z "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
    cat >&2 <<'EOF'
ERROR: needs-jetstream-walk requires APPRAFTER_E2E_LOCAL_OPERATOR=1.

Every code path this walk exercises (Backend::Nats, the connection/
accounts Secret writers, the AwaitingStreamCreation gate, the branch
CRD's needs.jetstream / ResourceClaim.spec.jetstream fields, and the
branch RBAC's apps/statefulsets rule) is unreleased. Run:

  APPRAFTER_E2E_LOCAL_OPERATOR=1 bash e2e/needs-jetstream-walk.sh
EOF
    exit 2
fi

# ---------------------------------------------------------------
# Temp workspace
# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"

# Set to 1 only after the cluster is up AND $KUBECONFIG points at it — see
# needs-pg-walk.sh for why this guards dump_diagnostics/k3d_down. This
# repository's rule is absolute: an e2e walk touches ONLY the cluster it
# created, and that includes read-only commands.
CLUSTER_CREATED=0

cleanup() {
    local exit_code=$?

    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! needs-jetstream-walk FAILED at %s (exit %d) !!!\n' \
            "$(elapsed)" "$exit_code" >&2
        if [ "$CLUSTER_CREATED" -eq 1 ]; then
            dump_diagnostics
            printf 'Tearing down cluster (set APPRAFTER_E2E_SKIP_DESTROY=1 to keep).\n' >&2
        else
            printf 'Cluster %s was never created — skipping diagnostics + teardown; your ambient KUBECONFIG was NOT touched.\n' \
                "$CLUSTER_NAME" >&2
        fi
    fi

    # Best-effort: stop the background subscriber from Phase 7 if it is
    # still running (it usually exits on its own once --count is hit).
    if [ -n "${SUB_PID:-}" ] && kill -0 "$SUB_PID" 2>/dev/null; then
        kill "$SUB_PID" 2>/dev/null || true
    fi

    if [ "$CLUSTER_CREATED" -eq 1 ]; then
        if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
            k3d_down "$CLUSTER_NAME" || true
        else
            printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' "$CLUSTER_NAME"
        fi
    fi

    rm -rf "$TMPDIR_WORK"
    exit "$exit_code"
}
trap cleanup EXIT

# ---------------------------------------------------------------
# Helper: seed the CLI state store (mirrors needs-pg-walk.sh / gitops-walk.sh)
# ---------------------------------------------------------------
seed_apprafter_state() {
    local kubeconfig_content="$1"
    mkdir -p "${APPRAFTER_CONFIG_DIR}"
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/${CLUSTER_NAME}/.apprafter"
    cat >"${APPRAFTER_CONFIG_DIR}/config.yaml" <<YAML
active_target: ${CLUSTER_NAME}
version: 1
YAML
    local kc_escaped
    kc_escaped=$(printf '%s' "$kubeconfig_content" \
        | sed 's/\\/\\\\/g' | sed 's/"/\\"/g' | awk '{printf "%s\\n", $0}')
    cat >"${APPRAFTER_CONFIG_DIR}/state/${CLUSTER_NAME}/.apprafter/state.json" <<STATE
{
  "hetzner_cloud": {
    "server_id": 1,
    "server_name": "${CLUSTER_NAME}-local",
    "ssh_key_ids": [],
    "kubeconfig_yaml": "${kc_escaped}"
  }
}
STATE
}

# ---------------------------------------------------------------
# Local helpers: wait_jsonpath / wait_gone / assert_eq / jp / cond_status
# (identical contract to needs-pg-walk.sh's own — see that file for the
# per-function rationale, not repeated here)
# ---------------------------------------------------------------
wait_jsonpath() {
    local kind="$1" ns="$2" name="$3" jsonpath="$4" want="$5"
    local timeout="${6:-180}"
    local deadline got
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait %s/%s [%s] == %q (timeout %ss) ...\n' "$kind" "$name" "$jsonpath" "$want" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        got=$(kubectl -n "$ns" get "$kind" "$name" -o jsonpath="$jsonpath" 2>/dev/null || true)
        if [ "$got" = "$want" ]; then
            printf '  ok: %s/%s [%s] = %q\n' "$kind" "$name" "$jsonpath" "$got"
            return 0
        fi
        printf '    %s: got=%q want=%q\n' "$(date +%H:%M:%S)" "$got" "$want"
        sleep 5
    done
    printf 'ERROR: %s/%s [%s] never became %q (last=%q)\n' "$kind" "$name" "$jsonpath" "$want" "${got:-}" >&2
    kubectl -n "$ns" describe "$kind" "$name" >&2 2>&1 || true
    return 1
}

wait_gone() {
    local kind="$1" ns="$2" name="$3" timeout="${4:-120}" deadline
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait %s/%s gone (timeout %ss) ...\n' "$kind" "$name" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        kubectl -n "$ns" get "$kind" "$name" >/dev/null 2>&1 || { printf '  ok: %s/%s is gone\n' "$kind" "$name"; return 0; }
        sleep 5
    done
    printf 'ERROR: %s/%s still present after %ss\n' "$kind" "$name" "$timeout" >&2
    return 1
}

assert_eq() {
    local desc="$1" got="$2" want="$3"
    if [ "$got" = "$want" ]; then
        printf '  ok: %s = %q\n' "$desc" "$got"
        return 0
    fi
    printf 'ERROR: %s — got %q, want %q\n' "$desc" "$got" "$want" >&2
    return 1
}

# assert_contains <description> <haystack> <needle> — a match failure is
# reported explicitly; never written as `grep -q` under pipefail (this
# repo's own recorded trap: a head/grep -q pipeline can turn a real match
# into SIGPIPE-induced failure). Plain substring test in bash, no pipe.
assert_contains() {
    local desc="$1" haystack="$2" needle="$3"
    if [[ "$haystack" == *"$needle"* ]]; then
        printf '  ok: %s contains %q\n' "$desc" "$needle"
        return 0
    fi
    printf 'ERROR: %s does not contain %q\n  got: %s\n' "$desc" "$needle" "$haystack" >&2
    return 1
}

assert_not_contains() {
    local desc="$1" haystack="$2" needle="$3"
    if [[ "$haystack" != *"$needle"* ]]; then
        printf '  ok: %s does not contain %q\n' "$desc" "$needle"
        return 0
    fi
    printf 'ERROR: %s UNEXPECTEDLY contains %q\n  got: %s\n' "$desc" "$needle" "$haystack" >&2
    return 1
}

jp() {
    kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true
}

cond_status() {
    kubectl -n "$2" get "$1" "$3" -o jsonpath="{.status.conditions[?(@.type==\"$4\")].status}" 2>/dev/null || true
}
cond_reason() {
    kubectl -n "$2" get "$1" "$3" -o jsonpath="{.status.conditions[?(@.type==\"$4\")].reason}" 2>/dev/null || true
}
cond_message() {
    kubectl -n "$2" get "$1" "$3" -o jsonpath="{.status.conditions[?(@.type==\"$4\")].message}" 2>/dev/null || true
}

strip_ansi() { sed $'s/\033\\[[0-9;]*[a-zA-Z]//g'; }

# operator_log — the whole apprafter-operator log, ANSI stripped, so a
# grep for a literal string cannot silently miss a colourised field the
# way needs-pg-walk.sh's own note warns about.
operator_log() {
    kubectl -n "$RETAINED_NS" logs deploy/apprafter-operator --all-containers --tail=-1 2>/dev/null | strip_ansi
}

# nats_run <args...> — run the `nats` CLI inside the persistent debug pod.
nats_run() {
    kubectl -n "$APP_NS" exec "$NATS_BOX_POD" -- nats "$@"
}

# write_pod_file <path> — pipe stdin (a host heredoc) into a file inside
# the debug pod. Used for the JSON stream/consumer configs `nats add`
# reads with --config, so no shell-quoting-through-two-layers is needed.
write_pod_file() {
    kubectl -n "$APP_NS" exec -i "$NATS_BOX_POD" -- sh -c "cat > $1"
}

# secret_val <ns> <name> <key> — base64-decoded Secret data key.
secret_val() {
    kubectl -n "$1" get secret "$2" -o jsonpath="{.data.$3}" 2>/dev/null | base64 -d
}

# ===============================================================
# Phase 0: bring up the cluster
# ===============================================================

phase "Phase 0: k3d_up ${CLUSTER_NAME}"

k3d_up "$CLUSTER_NAME"
cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
CLUSTER_CREATED=1
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

# ===============================================================
# Phase 1: seed CLI state, cluster-bootstrap, side-load the branch
#          operator/webhook + CRDs + RBAC (all unreleased)
# ===============================================================

phase "Phase 1: cluster-bootstrap (published platform stack)"

kubeconfig_content=$(cat "$KUBECONFIG_FILE")
seed_apprafter_state "$kubeconfig_content"
export APPRAFTER_CONFIG_DIR
printf '  APPRAFTER_CONFIG_DIR=%s\n' "$APPRAFTER_CONFIG_DIR"

bootstrap_with_retry
printf '  cluster-bootstrap complete\n'

phase "Phase 1b: build + load local operator + webhook, apply branch CRDs + RBAC"

build_load_restart apprafter-operator apprafter-operator
build_load_restart admission-webhook admission-webhook
printf '  waiting for the old (released) webhook pod to fully terminate ...\n'
_wh_deadline=$(( $(date +%s) + 90 ))
while [ "$(date +%s)" -lt "$_wh_deadline" ]; do
    [ "$(kubectl -n apprafter-system get pods -l app.kubernetes.io/name=admission-webhook --no-headers 2>/dev/null | wc -l)" -le 1 ] && break
    sleep 3
done
printf '  apprafter-operator + admission-webhook now running the working-tree build\n'

printf '  applying branch operator CRDs (published chart predates needs.jetstream) ...\n'
for _app in platform apprafter-operator; do
    kubectl -n argocd patch applications.argoproj.io "$_app" --type=merge \
        -p '{"spec":{"syncPolicy":{"automated":null}}}' >/dev/null 2>&1 || true
done
apply_branch_operator_crds
for _crd in applications serviceproviders resourceclaims retainedclaims platformstacks; do
    retry 12 5 -- kubectl wait --for=condition=Established "crd/${_crd}.apprafter.io" --timeout=30s
done
printf '  branch CRDs applied + Established\n'

apply_branch_operator_rbac

# ===============================================================
# Phase 1c: seed the unpublished platform-stack ARTIFACTS this branch
#           shipped (2.5c) but has not published — see the module doc
#           above for exactly why, and where the substitution stops.
# ===============================================================

phase "Phase 1c: seed nats-system namespace + jetstream-integrated ServiceProvider"

kubectl create namespace "$NATS_NS" 2>/dev/null || true
printf '  namespace %s present (mirrors namespaces.cue: shipped unconditionally)\n' "$NATS_NS"

if ! kubectl get serviceprovider "$PROVIDER" -n "$RETAINED_NS" >/dev/null 2>&1; then
    printf '  ServiceProvider %s absent — seeding it from the branch chart source (unpublished 2.5c artifact) ...\n' "$PROVIDER"
    kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: ServiceProvider
metadata:
  name: ${PROVIDER}
  namespace: ${RETAINED_NS}
  labels:
    apprafter.io/managed-by: apprafter
    tier: integrated
    location: in-cluster
spec:
  type: jetstream
  backend: nats
  config:
    namespace: ${NATS_NS}
    serverImage: "nats:2.14.3-alpine"
    sizeBytes:
      nano: 67108864
      small: 268435456
      medium: 1073741824
      large: 2147483648
      xlarge: 4294967296
    ceilingBytes: 4294967296
YAML
fi
sp_tier=$(jp serviceprovider "$RETAINED_NS" "$PROVIDER" '{.metadata.labels.tier}')
assert_eq "ServiceProvider ${PROVIDER} label tier" "$sp_tier" "integrated"

# ===============================================================
# Phase 2: readiness — AppProject, admission-webhook
# ===============================================================

phase "Phase 2: platform readiness (AppProject, admission-webhook)"

printf '  waiting for AppProject apps ...\n'
deadline=$(( $(date +%s) + 600 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1 && { printf '  AppProject apps -> found\n'; break; }
    sleep 10
done
kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1 || {
    printf 'ERROR: AppProject apps not found after 10 min\n' >&2; exit 1; }

printf '  waiting for the admission-webhook Deployment ...\n'
retry 30 10 -- kubectl -n "$RETAINED_NS" rollout status deploy admission-webhook --timeout=60s

# ===============================================================
# Phase 3 — coordinator step 1: chart installed, component DISABLED.
#           No NATS pod exists. This is the cheapest thing to get wrong.
# ===============================================================

phase "Phase 3 (coordinator step 1): lazy enablement — no nats pod exists yet"

nats_pod_count=$(kubectl -n "$NATS_NS" get pods --no-headers 2>/dev/null | wc -l | tr -d ' ')
assert_eq "pod count in ${NATS_NS} before any jetstream claim" "$nats_pod_count" "0"

overrides_before=$(jp platformstack "$PLATFORMSTACK_NS" "$PLATFORMSTACK_NAME" '{.spec.overrides.nats}')
assert_eq "PlatformStack.spec.overrides.nats before any claim" "$overrides_before" ""

# ===============================================================
# Phase 4 — coordinator step 2: apply the needs.jetstream Application.
#           Observe, IN ORDER: claim -> Scheduled -> override flips ->
#           annotation stamped -> nats-accounts Secret appears.
# ===============================================================

phase "Phase 4 (coordinator step 2): apply Application 1 (dynamicStreams: true)"

kubectl create namespace "$APP_NS" 2>/dev/null || true

kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP1}
  namespace: ${APP_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    expose:
      port: 80
    needs:
      jetstream:
        selector:
          tier: integrated
        dynamicStreams: true
YAML

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM1" '{.spec.type}' jetstream 180
claim_dyn=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM1" '{.spec.jetstream.dynamicStreams}')
assert_eq "ResourceClaim ${CLAIM1} spec.jetstream.dynamicStreams" "$claim_dyn" "true"

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM1" '{.status.provider}' "$PROVIDER" 120
sched1=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM1" Scheduled)
assert_eq "ResourceClaim ${CLAIM1} Scheduled condition" "$sched1" "True"

printf '  waiting for PlatformStack.spec.overrides.nats.enabled = true ...\n'
wait_jsonpath platformstack "$PLATFORMSTACK_NS" "$PLATFORMSTACK_NAME" \
    '{.spec.overrides.nats.enabled}' true 120

annot=$(kubectl -n "$PLATFORMSTACK_NS" get platformstack "$PLATFORMSTACK_NAME" \
    -o jsonpath="{.metadata.annotations['apprafter\.io/nats-auto-enabled']}" 2>/dev/null || true)
assert_eq "PlatformStack annotation ${AUTO_ANNOTATION}" "$annot" "true"

printf '  waiting for the %s Secret in %s ...\n' "$ACCOUNTS_SECRET" "$NATS_NS"
deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    kubectl -n "$NATS_NS" get secret "$ACCOUNTS_SECRET" >/dev/null 2>&1 && break
    sleep 5
done
kubectl -n "$NATS_NS" get secret "$ACCOUNTS_SECRET" >/dev/null 2>&1 || {
    printf 'ERROR: %s Secret never appeared in %s\n' "$ACCOUNTS_SECRET" "$NATS_NS" >&2; exit 1; }
accounts_conf_1=$(secret_val "$NATS_NS" "$ACCOUNTS_SECRET" 'accounts\.conf')
assert_contains "nats-accounts (first render)" "$accounts_conf_1" "claim_demo_walkapp_jetstream"
printf '  ok: nats-accounts Secret exists in %s BEFORE any nats pod was created (see Phase 4b)\n' "$NATS_NS"

# ===============================================================
# Phase 4b — the ONE substituted step. See the module doc's own
# "UNPUBLISHED-COMPONENT SUBSTITUTION" section for the full reasoning.
# Everything above this point (the override flip, the annotation, the
# accounts Secret) is REAL, observed against the operator's actual code.
# From here, the walk hand-applies the `nats`/`nats-io/k8s` chart's own
# rendered manifests — standing in for the Argo CD sync of an unpublished
# platform-stack component — so the REST of the pipeline (the StatefulSet
# mounting this REAL Secret, the reloader, SIGHUP, verify) can be
# exercised for real.
# ===============================================================

phase "Phase 4b (SUBSTITUTED — see module doc): install the nats chart by hand"

DEFAULT_SC=$(kubectl get storageclass -o jsonpath='{.items[?(@.metadata.annotations.storageclass\.kubernetes\.io/is-default-class=="true")].metadata.name}')
if [ -z "$DEFAULT_SC" ]; then
    DEFAULT_SC=$(kubectl get storageclass -o jsonpath='{.items[0].metadata.name}')
fi
printf '  cluster default StorageClass = %q (component_nats.cue pins "local-path", the k3s name — this walk uses the cluster'"'"'s actual default; harness-substrate accommodation, not a product deviation)\n' \
    "$DEFAULT_SC"

cat >"${TMPDIR_WORK}/nats-values.yaml" <<VALUES
container:
  image:
    repository: nats
    tag: 2.14.3-alpine
  resources:
    requests: { cpu: 100m, memory: 384Mi }
    limits: { cpu: 100m, memory: 384Mi }
  patch:
    - op: add
      path: /volumeMounts/-
      value: { name: accounts-secret, mountPath: /etc/nats-config/accounts-secret, readOnly: true }
podTemplate:
  patch:
    - op: add
      path: /spec/volumes/-
      value: { name: accounts-secret, secret: { secretName: ${ACCOUNTS_SECRET} } }
config:
  jetstream:
    enabled: true
    fileStore:
      pvc:
        size: 5Gi
        storageClassName: ${DEFAULT_SC}
    memoryStore:
      enabled: true
      maxSize: 192Mi
  merge:
    accounts\$include: "./accounts-secret/accounts.conf"
    feature_flags:
      js_ack_fc_v2: true
VALUES

helm repo add nats https://nats-io.github.io/k8s/helm/charts/ >/dev/null 2>&1 || true
helm repo update nats >/dev/null 2>&1 || true
helm template nats nats/nats --version 2.14.6 -n "$NATS_NS" -f "${TMPDIR_WORK}/nats-values.yaml" \
    | kubectl apply -n "$NATS_NS" -f -
printf '  applied the (unpublished) nats component'"'"'s rendered manifests by hand\n'

printf '  waiting for the %s StatefulSet to report a ready replica ...\n' "$NATS_STS"
wait_jsonpath statefulset "$NATS_NS" "$NATS_STS" '{.status.readyReplicas}' 1 300

NATS_POD="${NATS_STS}-0"
RESTARTS_BEFORE=$(kubectl -n "$NATS_NS" get pod "$NATS_POD" -o jsonpath='{.status.containerStatuses[?(@.name=="nats")].restartCount}')
START_BEFORE=$(kubectl -n "$NATS_NS" get pod "$NATS_POD" -o jsonpath='{.status.containerStatuses[?(@.name=="nats")].state.running.startedAt}')
printf '  nats-0 baseline: restarts=%s startedAt=%s\n' "$RESTARTS_BEFORE" "$START_BEFORE"

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM1" '{.status.ready}' true 240
conn1_ref=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM1" '{.status.connectionSecretRef}')
assert_eq "status.connectionSecretRef" "$conn1_ref" "$CONN1"
printf '  ok: claim %s Ready=True — deployment/verify pipeline (2.5d Task 6) closed the loop for real\n' "$CLAIM1"

# ===============================================================
# Phase 5 — coordinator step 3: connect AS THE CLAIM USER from inside
#           the cluster, WITH the inbox prefix, create/publish/consume.
# ===============================================================

phase "Phase 5 (coordinator step 3): connect as the claim user, with inbox prefix"

kubectl -n "$APP_NS" get pod "$NATS_BOX_POD" >/dev/null 2>&1 || \
    kubectl -n "$APP_NS" run "$NATS_BOX_POD" --image=natsio/nats-box:latest --restart=Never --command -- sleep infinity
kubectl -n "$APP_NS" wait --for=condition=Ready "pod/${NATS_BOX_POD}" --timeout=120s

CONN1_HOST=$(secret_val "$APP_NS" "$CONN1" host)
CONN1_PORT=$(secret_val "$APP_NS" "$CONN1" port)
CONN1_USER=$(secret_val "$APP_NS" "$CONN1" user)
CONN1_PASS=$(secret_val "$APP_NS" "$CONN1" pass)
CONN1_ACCOUNT=$(secret_val "$APP_NS" "$CONN1" account)
CONN1_SUBJECT_PREFIX=$(secret_val "$APP_NS" "$CONN1" subjectPrefix)
CONN1_INBOX_PREFIX=$(secret_val "$APP_NS" "$CONN1" inboxPrefix)
CONN1_SERVER="${CONN1_HOST}:${CONN1_PORT}"

for v in CONN1_HOST CONN1_PORT CONN1_USER CONN1_PASS CONN1_ACCOUNT CONN1_SUBJECT_PREFIX CONN1_INBOX_PREFIX; do
    [ -n "${!v}" ] || { printf 'ERROR: connection Secret %s field %s is empty\n' "$CONN1" "$v" >&2; exit 1; }
done
assert_eq "connection Secret account" "$CONN1_ACCOUNT" "ns_${APP_NS}"
assert_eq "connection Secret subjectPrefix" "$CONN1_SUBJECT_PREFIX" "${APP1}."
assert_eq "connection Secret inboxPrefix" "$CONN1_INBOX_PREFIX" "_INBOX_${APP_NS}_${APP1}"
printf '  ok: connection Secret %s carries all eight JETSTREAM_FIELDS with non-empty values\n' "$CONN1"

write_pod_file /tmp/stream1.json <<JSON
{"name":"walkstream","subjects":["${CONN1_SUBJECT_PREFIX}demo"],"storage":"file","retention":"limits","max_consumers":-1,"max_msgs":-1,"max_bytes":-1,"max_age":0,"max_msgs_per_subject":-1,"max_msg_size":-1,"discard":"old","num_replicas":1,"duplicate_window":120000000000}
JSON

stream_out=$(nats_run --server "$CONN1_SERVER" --user "$CONN1_USER" --password "$CONN1_PASS" --inbox-prefix "$CONN1_INBOX_PREFIX" \
    stream add walkstream --config /tmp/stream1.json 2>&1)
assert_contains "stream add (with inbox prefix)" "$stream_out" "was created"

pub_out=$(nats_run --server "$CONN1_SERVER" --user "$CONN1_USER" --password "$CONN1_PASS" --inbox-prefix "$CONN1_INBOX_PREFIX" \
    pub "${CONN1_SUBJECT_PREFIX}demo" "walk-message-1" 2>&1)
assert_contains "publish" "$pub_out" "Published"

write_pod_file /tmp/consumer1.json <<'JSON'
{"durable_name":"walkconsumer","ack_policy":"explicit","deliver_policy":"all"}
JSON
consumer_out=$(nats_run --server "$CONN1_SERVER" --user "$CONN1_USER" --password "$CONN1_PASS" --inbox-prefix "$CONN1_INBOX_PREFIX" \
    consumer add walkstream walkconsumer --config /tmp/consumer1.json 2>&1)
assert_contains "consumer add" "$consumer_out" "created"

consumed=$(nats_run --server "$CONN1_SERVER" --user "$CONN1_USER" --password "$CONN1_PASS" --inbox-prefix "$CONN1_INBOX_PREFIX" \
    consumer next walkstream walkconsumer --count 1 --raw 2>&1)
assert_eq "consumed message body" "$consumed" "walk-message-1"
printf '  ok: account + user + permissions + connection contract are mutually consistent end-to-end\n'

# ===============================================================
# Phase 6 — coordinator step 4: the negative — connect WITHOUT the
#           inbox prefix, confirm it FAILS, and TIME it (NOPERM delivers
#           no error signal, so this presents as a timeout).
# ===============================================================

phase "Phase 6 (coordinator step 4): connect WITHOUT the inbox prefix — expect failure"

write_pod_file /tmp/stream_neg.json <<JSON
{"name":"negstream","subjects":["${CONN1_SUBJECT_PREFIX}neg"],"storage":"file","retention":"limits","max_consumers":-1,"max_msgs":-1,"max_bytes":-1,"max_age":0,"max_msgs_per_subject":-1,"max_msg_size":-1,"discard":"old","num_replicas":1,"duplicate_window":120000000000}
JSON

NEG_T0=$(date +%s%N)
neg_out=$(nats_run --server "$CONN1_SERVER" --user "$CONN1_USER" --password "$CONN1_PASS" \
    stream add negstream --config /tmp/stream_neg.json 2>&1) || true
NEG_T1=$(date +%s%N)
NEG_MS=$(( (NEG_T1 - NEG_T0) / 1000000 ))

assert_not_contains "stream add without inbox prefix must NOT succeed" "$neg_out" "was created"
printf '  ok: the missing-inbox-prefix attempt failed as expected, in %d ms\n' "$NEG_MS"
printf '  observed failure text: %s\n' "$neg_out"
if [ "$NEG_MS" -gt 3000 ]; then
    printf '  FINDING: %d ms is well past what a human would wait before assuming a hang — the NOPERM-delivers-no-error-signal property (nats_client.rs'"'"'s own doc) is a real UX cost here, not just an internal implementation note. The failure text names no permission problem.\n' \
        "$NEG_MS"
fi

# ===============================================================
# Phase 7 — coordinator steps 5/6, same trigger: a second Application
#           in the SAME namespace. Both land in ONE account; neither
#           reaches the other's subjects; the FIRST claim's live
#           connection SURVIVES the whole-file rewrite; the new user
#           works WITHOUT a pod restart (the hot-reload proof).
# ===============================================================

phase "Phase 7 (coordinator steps 5/6): second Application — isolation + survival + hot reload"

# Start a LONG-LIVED subscriber as walkapp's user BEFORE app2 exists, so
# its connection predates (and must survive) the whole-file rewrite app2
# triggers. Backgrounded in THIS shell; the underlying `kubectl exec`
# process is what keeps the container-side `nats sub` connected.
SUB_LOG="${TMPDIR_WORK}/sub_output.log"
kubectl -n "$APP_NS" exec "$NATS_BOX_POD" -- \
    nats --server "$CONN1_SERVER" --user "$CONN1_USER" --password "$CONN1_PASS" --inbox-prefix "$CONN1_INBOX_PREFIX" \
    sub "${CONN1_SUBJECT_PREFIX}survive" --count 2 >"$SUB_LOG" 2>&1 &
SUB_PID=$!
sleep 3
if ! kill -0 "$SUB_PID" 2>/dev/null; then
    printf 'ERROR: the long-lived subscriber exited immediately — see %s\n' "$SUB_LOG" >&2
    cat "$SUB_LOG" >&2 || true
    exit 1
fi
printf '  long-lived subscriber connected (pid %s, still running)\n' "$SUB_PID"

pre_pub=$(nats_run --server "$CONN1_SERVER" --user "$CONN1_USER" --password "$CONN1_PASS" --inbox-prefix "$CONN1_INBOX_PREFIX" \
    pub "${CONN1_SUBJECT_PREFIX}survive" "before-rewrite" 2>&1)
assert_contains "pre-rewrite publish" "$pre_pub" "Published"
sleep 2

kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP2}
  namespace: ${APP_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    expose:
      port: 80
    needs:
      jetstream:
        selector:
          tier: integrated
        dynamicStreams: true
YAML

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM2" '{.status.ready}' true 240
conn2_ref=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM2" '{.status.connectionSecretRef}')
assert_eq "status.connectionSecretRef (app2)" "$conn2_ref" "$CONN2"

# --- hot reload proof: no pod restart, same startedAt ---
RESTARTS_AFTER=$(kubectl -n "$NATS_NS" get pod "$NATS_POD" -o jsonpath='{.status.containerStatuses[?(@.name=="nats")].restartCount}')
START_AFTER=$(kubectl -n "$NATS_NS" get pod "$NATS_POD" -o jsonpath='{.status.containerStatuses[?(@.name=="nats")].state.running.startedAt}')
assert_eq "nats-0 restartCount unchanged across the accounts-file rewrite" "$RESTARTS_AFTER" "$RESTARTS_BEFORE"
assert_eq "nats-0 container startedAt unchanged (no restart)" "$START_AFTER" "$START_BEFORE"
printf '  ok: no pod restart — if this held, the SIGHUP + $include design (ADR 0061 §1) did the work\n'

reload_evidence=$(kubectl -n "$NATS_NS" logs "$NATS_POD" -c reloader --tail=-1 2>/dev/null || true)
printf '  reloader container log (tail):\n%s\n' "$(printf '%s\n' "$reload_evidence" | tail -10)"

# --- isolation proof: both users in ONE account block ---
accounts_conf_2=$(secret_val "$NATS_NS" "$ACCOUNTS_SECRET" 'accounts\.conf')
assert_contains "nats-accounts (after app2) carries app1's user" "$accounts_conf_2" "claim_demo_walkapp_jetstream"
assert_contains "nats-accounts (after app2) carries app2's user" "$accounts_conf_2" "claim_demo_walkapp2_jetstream"
ns_block_count=$(printf '%s' "$accounts_conf_2" | grep -c '^ns_demo: {' || true)
assert_eq "exactly one ns_demo account block (both users in ONE account)" "$ns_block_count" "1"

CONN2_USER=$(secret_val "$APP_NS" "$CONN2" user)
CONN2_PASS=$(secret_val "$APP_NS" "$CONN2" pass)
CONN2_INBOX_PREFIX=$(secret_val "$APP_NS" "$CONN2" inboxPrefix)
cross_sub=$(nats_run --server "$CONN1_SERVER" --user "$CONN2_USER" --password "$CONN2_PASS" --inbox-prefix "$CONN2_INBOX_PREFIX" \
    sub "${CONN1_SUBJECT_PREFIX}>" --count 1 2>&1) || true
assert_contains "app2 subscribing to app1's subjects is denied" "$cross_sub" "ermission"

# --- survival proof: the SAME pid (never restarted), still running,
#     receives a message published AFTER the rewrite ---
if ! kill -0 "$SUB_PID" 2>/dev/null; then
    printf 'ERROR: the long-lived subscriber (pid %s) is no longer running after the accounts-file rewrite — its connection did NOT survive\n' "$SUB_PID" >&2
    cat "$SUB_LOG" >&2 || true
    exit 1
fi
printf '  ok: the pre-existing subscriber (pid %s) is STILL RUNNING after the rewrite (no drop)\n' "$SUB_PID"

post_pub=$(nats_run --server "$CONN1_SERVER" --user "$CONN1_USER" --password "$CONN1_PASS" --inbox-prefix "$CONN1_INBOX_PREFIX" \
    pub "${CONN1_SUBJECT_PREFIX}survive" "after-rewrite" 2>&1)
assert_contains "post-rewrite publish (proves the NEW user/account state is what the server is actually running, not just unchanged)" "$post_pub" "Published"

_sub_deadline=$(( $(date +%s) + 15 ))
while [ "$(date +%s)" -lt "$_sub_deadline" ] && kill -0 "$SUB_PID" 2>/dev/null; do sleep 1; done
sub_output=$(cat "$SUB_LOG" 2>/dev/null || true)
assert_contains "surviving subscriber received the PRE-rewrite message" "$sub_output" "before-rewrite"
assert_contains "surviving subscriber received the POST-rewrite message (same connection, both sides of the reload)" "$sub_output" "after-rewrite"
printf '  ok: ONE unbroken connection received messages from BEFORE and AFTER the accounts-file rewrite\n'

# ===============================================================
# Also worth checking (while a cluster exists):
#   - a claim declaring streams parks at AwaitingStreamCreation
#   - the operator log carries no forbidden-verb complaint
# ===============================================================

phase "Also: a claim declaring streams[] parks at AwaitingStreamCreation"

kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP3}
  namespace: ${APP_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    expose:
      port: 80
    needs:
      jetstream:
        selector:
          tier: integrated
        streams:
          - name: orders
            subjects: ["streamapp.orders.>"]
            maxBytes: "1Gi"
YAML

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM3" '{.spec.type}' jetstream 180
printf '  waiting for the claim to settle on Ready=False/AwaitingStreamCreation ...\n'
deadline=$(( $(date +%s) + 240 ))
reason3=""
while [ "$(date +%s)" -lt "$deadline" ]; do
    reason3=$(cond_reason "$CLAIM_RES" "$APP_NS" "$CLAIM3" Ready)
    [ "$reason3" = "AwaitingStreamCreation" ] && break
    sleep 5
done
assert_eq "ResourceClaim ${CLAIM3} Ready condition reason" "$reason3" "AwaitingStreamCreation"
ready3=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM3" '{.status.ready}')
assert_eq "ResourceClaim ${CLAIM3} status.ready" "$ready3" "false"
msg3=$(cond_message "$CLAIM_RES" "$APP_NS" "$CLAIM3" Ready)
assert_contains "AwaitingStreamCreation message names the real reason" "$msg3" "not yet created"
printf '  ok: a declared-streams claim parks with an actionable message: %q\n' "$msg3"

phase "Also: no forbidden-verb complaint in the operator log"

log_all="$(operator_log)"
if [[ "$log_all" == *"forbidden"* ]] || [[ "$log_all" == *"Forbidden"* ]]; then
    printf 'ERROR: the operator log contains a forbidden-verb complaint — an RBAC gap that would manifest as a reconcile silently never progressing:\n' >&2
    printf '%s\n' "$log_all" | grep -i forbidden >&2 || true
    exit 1
fi
printf '  ok: no "forbidden" anywhere in the operator log across the whole walk\n'

# ===============================================================
# Done
# ===============================================================

phase "needs-jetstream-walk: ALL PHASES GREEN (elapsed $(elapsed))"
printf 'FINAL: PASS\n'
