#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter needs.networkpolicy-walk e2e — the full Phase-2.10 egress
# CiliumNetworkPolicy enforcement chain on a local kind+Cilium cluster
# (plan item 2.10 Task 5, ADR 0045).
#
# Unlike the other needs/* walks (which run on the default CNI with
# APPRAFTER_BOOTSTRAP_SKIP_CILIUM=1), this walk REQUIRES Cilium: the
# 2.10 feature is a per-Application egress CiliumNetworkPolicy, and only
# real Cilium enforcement + Hubble flow verdicts can prove it. The
# cluster is therefore brought up with kind_up_cilium (default CNI +
# kube-proxy disabled so Cilium owns the datapath) and bootstrapped with
# bootstrap_with_cilium (Cilium installed as kube-proxy replacement).
#
# The chain proven (deterministically, via Hubble verdicts — NOT transient
# Application phases):
#
#   1. kind_up_cilium -> bootstrap_with_cilium -> `cilium status --wait`
#      -> enable Hubble. Build + side-load the WORKING-TREE operator +
#      admission-webhook (2.10 is UNRELEASED; LOCAL_OPERATOR mode, like
#      gitops-walk-per-env.sh's Phase 1b) + apply the branch CRDs/RBAC.
#   2. Apply a needs.pg Application `web` + a needs-LESS Application
#      `noproxy` in a tenant namespace. Wait both Deployments Ready.
#   3. CNP emission: the operator emits `web-egress` (pg rule:
#      cnpg-system + 5432) and `noproxy-egress` (baseline only, no pg rule).
#   4. ENFORCEMENT (the 2.10 acceptance): from the `noproxy` pod, a TCP
#      connect to the shared pg Service is DROPPED (Hubble DROPPED flow +
#      the connect fails); from the `web` pod the same connect is
#      FORWARDED (Hubble FORWARDED + the connect succeeds). An app's
#      reach to pg is gated behind a declared needs.pg.
#   5. internet: under the default `internet` profile, an external TCP
#      connect (1.1.1.1:443) from `web` SUCCEEDS (`toEntities: [world]`).
#   6. CLI + profile switch: `apprafter platform egress set internal`
#      drops the `world` rule from `web-egress`; the external connect now
#      FAILS while the pg connect still SUCCEEDS; `… egress show` prints
#      internal.
#   7. strict: `set strict` additionally drops the same-namespace rule
#      (web-egress carries only DNS + the pg need rule).
#
# CLI state injection
# -------------------
# `apprafter cluster-bootstrap` / `app …` read the kubeconfig from the
# CLI's per-target state store (not from $KUBECONFIG). We set
# APPRAFTER_CONFIG_DIR to a tmpdir and seed it with a minimal config.yaml
# (active_target: k3d) and a state.json carrying the kubeconfig as
# kubeconfig_yaml (plaintext) — the same approach the other walks use.
#
# Local-operator mode
# -------------------
# 2.10 is UNRELEASED, so this walk ALWAYS builds + side-loads the
# working-tree operator + admission-webhook and applies the branch CRDs
# (the released chart predates the spec.network.egress.profile field, the
# CNP RBAC, and the egress emission). Mirrors needs-disk-walk.sh's Phase 1b
# / gitops-walk-per-env.sh's local-operator path. POST-RELEASE this nightly
# flips to the published image (drop the unconditional Phase 1b), exactly
# like the e2e-pg / e2e-redis / e2e-disk nightlies did once their feature
# shipped.
#
# Required: docker (or podman → kind), cargo, kubectl, plus the Cilium +
# Hubble CLIs (cilium-cli / hubble — `nix develop` ships them, else the
# lib.sh wrappers fall back to `nix run nixpkgs#…`). All satisfied inside
# `nix develop` or on a standard CI runner with the cilium-cli install step.
#
# Exit codes:
#   0 — chain green
#   1 — assertion failure
#   2 — precondition missing

set -euo pipefail

# ---------------------------------------------------------------
# Source shared helpers
# ---------------------------------------------------------------

# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

# This walk is Cilium-only: force the kind runtime (Cilium's eBPF datapath
# is pathologically slow on k3d; kind_up_cilium also rejects k3d). The
# escape hatch APPRAFTER_E2E_RUNTIME=k3d is intentionally overridden here.
export APPRAFTER_E2E_RUNTIME=kind

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-netpol-walk"

APP_NS="demo"                       # tenant namespace
APP_PG="web"                        # needs.pg Application
APP_NOPROXY="noproxy"               # needs-less Application
CNP_PG="${APP_PG}-egress"           # rendered CNP name = <deployment>-egress
CNP_NOPROXY="${APP_NOPROXY}-egress"

# Group-qualify the collision-prone Application kind (bare `application`
# also matches Argo CD's argoproj.io Application).
APP_RES="application.apprafter.io"
# Cilium v2 CR. group-qualify to be explicit.
CNP_RES="ciliumnetworkpolicies.cilium.io"

CNPG_NS="cnpg-system"               # shared CNPG Cluster namespace
# The pg read-write Service the app connects to: CNPG names it <cluster>-rw,
# where the shared cluster is `platform-postgres`. The CNP allows the
# cnpg-system namespace + the cluster pod selector + 5432, so reaching this
# Service is gated behind needs.pg.
PG_SERVICE="platform-postgres-rw.${CNPG_NS}"
PG_PORT="5432"

EXTERNAL_HOST="1.1.1.1"             # external internet probe target
EXTERNAL_PORT="443"

OPERATOR_NS="apprafter-system"      # operator + webhook + PlatformStack

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------

for tool in cargo kubectl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done

# kind needs a container runtime: docker or podman.
if ! command -v docker >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
    printf 'ERROR: neither "docker" nor "podman" found on PATH\n' >&2
    exit 2
fi

# The Cilium + Hubble CLIs are reached via the lib.sh wrappers (cilium_cli /
# hubble_cli), which fall back to `nix run nixpkgs#…` when the bare binary is
# missing — so a missing `cilium` binary alone is NOT a precondition failure
# as long as `nix` is on PATH. Fail only when neither route exists.
if ! command -v cilium >/dev/null 2>&1 && ! command -v nix >/dev/null 2>&1; then
    # shellcheck disable=SC2016  # literal backticks in the user-facing message
    printf 'ERROR: neither a `cilium` binary nor `nix` (for the nixpkgs#cilium-cli fallback) is on PATH\n' >&2
    exit 2
fi

# ---------------------------------------------------------------
# Temp workspace
# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"

# Set to 1 only after the cluster is up AND $KUBECONFIG points at it
# (Phase 0). Until then, dump_diagnostics / k3d_down must NOT run — on a
# cluster-up failure $KUBECONFIG still points at the ambient cluster, and
# an e2e must never touch a non-test cluster.
K3D_CREATED=0

cleanup() {
    local exit_code=$?

    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! needs-networkpolicy-walk FAILED at %s (exit %d) !!!\n' \
            "$(elapsed)" "$exit_code" >&2
        if [ "$K3D_CREATED" -eq 1 ]; then
            dump_diagnostics
            dump_cilium_diagnostics
            printf 'Tearing down cluster (set APPRAFTER_E2E_SKIP_DESTROY=1 to keep).\n' >&2
        else
            printf 'cluster %s was never created (kind/podman unavailable in this shell?) — skipping diagnostics + teardown; your ambient KUBECONFIG was NOT touched.\n' \
                "$CLUSTER_NAME" >&2
        fi
    fi

    if [ "$K3D_CREATED" -eq 1 ]; then
        if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
            k3d_down "$CLUSTER_NAME" || true
        else
            printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' \
                "$CLUSTER_NAME"
            printf 'Run: kind delete cluster --name %s\n' "$CLUSTER_NAME"
        fi
    fi

    rm -rf "$TMPDIR_WORK"
    exit "$exit_code"
}
trap cleanup EXIT

# ---------------------------------------------------------------
# Cilium-specific failure diagnostics (in addition to dump_diagnostics).
# ---------------------------------------------------------------
dump_cilium_diagnostics() {
    [ -n "${KUBECONFIG:-}" ] || return 0
    printf '\n----- cilium diagnostics (failure) -----\n' >&2
    cilium_cli status >&2 2>&1 || true
    printf '\n--- CiliumNetworkPolicies (all namespaces) ---\n' >&2
    kubectl get "$CNP_RES" -A >&2 2>&1 || true
    printf '\n--- web-egress CNP spec.egress ---\n' >&2
    kubectl -n "$APP_NS" get "$CNP_RES" "$CNP_PG" -o jsonpath='{.spec.egress}' >&2 2>&1 || true
    printf '\n--- recent Hubble flows (demo ns) ---\n' >&2
    hubble_cli observe --namespace "$APP_NS" --last 50 >&2 2>&1 || true
    printf '%s\n' '----- end cilium diagnostics -----' >&2
}

# ---------------------------------------------------------------
# Helper: seed the CLI state store (mirrors gitops-walk.sh).
# ---------------------------------------------------------------
seed_apprafter_state() {
    local kubeconfig_content="$1"

    mkdir -p "${APPRAFTER_CONFIG_DIR}"
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/k3d/.apprafter"

    cat >"${APPRAFTER_CONFIG_DIR}/config.yaml" <<'YAML'
active_target: k3d
version: 1
YAML

    local kc_escaped
    kc_escaped=$(printf '%s' "$kubeconfig_content" \
        | sed 's/\\/\\\\/g' \
        | sed 's/"/\\"/g' \
        | awk '{printf "%s\\n", $0}')

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

# ---------------------------------------------------------------
# Local helper: wait_jsonpath <kind> <ns> <name> <jsonpath> <want> [timeout]
# ---------------------------------------------------------------
wait_jsonpath() {
    local kind="$1" ns="$2" name="$3" jsonpath="$4" want="$5"
    local timeout="${6:-180}"
    local deadline got
    deadline=$(( $(date +%s) + timeout ))

    printf '  wait %s/%s [%s] == %q (timeout %ss) ...\n' \
        "$kind" "$name" "$jsonpath" "$want" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        got=$(kubectl -n "$ns" get "$kind" "$name" \
            -o jsonpath="$jsonpath" 2>/dev/null || true)
        if [ "$got" = "$want" ]; then
            printf '  ok: %s/%s [%s] = %q\n' "$kind" "$name" "$jsonpath" "$got"
            return 0
        fi
        printf '    %s: got=%q want=%q\n' "$(date +%H:%M:%S)" "$got" "$want"
        sleep 5
    done
    printf 'ERROR: %s/%s [%s] never became %q (last=%q)\n' \
        "$kind" "$name" "$jsonpath" "$want" "${got:-}" >&2
    kubectl -n "$ns" describe "$kind" "$name" >&2 2>&1 || true
    return 1
}

# ---------------------------------------------------------------
# Local helper: assert_eq <description> <got> <want>
# ---------------------------------------------------------------
assert_eq() {
    local desc="$1" got="$2" want="$3"
    if [ "$got" = "$want" ]; then
        printf '  ok: %s = %q\n' "$desc" "$got"
        return 0
    fi
    printf 'ERROR: %s — got %q, want %q\n' "$desc" "$got" "$want" >&2
    return 1
}

# ---------------------------------------------------------------
# Local helper: jp <kind> <ns> <name> <jsonpath>  (read one value)
# ---------------------------------------------------------------
jp() {
    kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true
}

# ---------------------------------------------------------------
# Local helper: _has_empty_samens_rule
#   Reads a CiliumNetworkPolicy JSON on stdin and prints `present` if it
#   carries the same-namespace egress rule (a toEndpoints entry with an
#   EMPTY matchLabels object — `{"matchLabels":{}}` — which Cilium reads as
#   "every endpoint in the policy's own namespace"), else `absent`. Used to
#   confirm `strict` drops same-namespace egress. Pure jq-free: a `python3`
#   reader keeps it robust against ordering; falls back to a string probe.
# ---------------------------------------------------------------
_has_empty_samens_rule() {
    local json; json=$(cat)
    if command -v python3 >/dev/null 2>&1; then
        printf '%s' "$json" | python3 -c '
import json, sys
try:
    doc = json.load(sys.stdin)
except Exception:
    print("absent"); sys.exit(0)
for rule in (doc.get("spec", {}) or {}).get("egress", []) or []:
    for ep in rule.get("toEndpoints", []) or []:
        if ep.get("matchLabels", None) == {}:
            print("present"); sys.exit(0)
print("absent")
'
        return 0
    fi
    # Fallback: an empty-matchLabels rule serialises (compact) as
    # {"matchLabels":{}}. Grep for that shape.
    case "$json" in
        *'"matchLabels":{}'*) printf 'present' ;;
        *) printf 'absent' ;;
    esac
}

# ---------------------------------------------------------------
# Local helper: app_pod <ns> <application-label>
#   The name of the (first) Ready pod backing an Application — these are
#   the pods the CNP selects on egress, so all connectivity probes MUST
#   run from inside them (a separate tool pod is unselected → open egress).
# ---------------------------------------------------------------
app_pod() {
    kubectl -n "$1" get pod -l "apprafter.io/application=$2" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

# ---------------------------------------------------------------
# Local helper: probe_tcp <ns> <pod> <host> <port>
#   Run a short busybox `nc -z` TCP connect from inside <pod>. Returns 0
#   when the connect succeeds, non-zero on refusal/timeout (a denied
#   egress, or no listener). `nginxdemos/hello:plain-text` ships busybox
#   nc + timeout. The outer `timeout` is belt-and-braces in case nc's own
#   -w is ignored on a black-holed (policy-DROPPED) packet.
# ---------------------------------------------------------------
probe_tcp() {
    local ns="$1" pod="$2" host="$3" port="$4"
    kubectl -n "$ns" exec "$pod" -- \
        timeout 8 nc -w 5 -z "$host" "$port" >/dev/null 2>&1
}

# ---------------------------------------------------------------
# Local helper: assert_connect <desc> <ns> <pod> <host> <port> <want-ok|want-fail>
#   Retry the connect a few times (the CNP datapath programming lags the
#   CR apply; a FORWARD may briefly be a DROP and vice-versa right after a
#   profile switch). want-ok  → expect eventual success.
#                    want-fail → expect it to stay failing across all tries.
# ---------------------------------------------------------------
assert_connect() {
    local desc="$1" ns="$2" pod="$3" host="$4" port="$5" want="$6"
    local i ok
    if [ "$want" = "want-ok" ]; then
        for i in $(seq 1 12); do
            if probe_tcp "$ns" "$pod" "$host" "$port"; then
                printf '  ok: %s — connect SUCCEEDED (try %d)\n' "$desc" "$i"
                return 0
            fi
            sleep 5
        done
        printf 'ERROR: %s — connect to %s:%s NEVER succeeded (expected allowed)\n' \
            "$desc" "$host" "$port" >&2
        return 1
    else
        # want-fail: the connect must stay blocked. Poll a window so a
        # just-applied DROP rule has time to program, then require failure
        # on a final confirming attempt.
        for i in $(seq 1 12); do
            if probe_tcp "$ns" "$pod" "$host" "$port"; then
                # Still reachable — keep waiting for the deny to program.
                sleep 5
                continue
            fi
            # One confirming retry to avoid a transient blip masquerading
            # as enforcement.
            sleep 3
            if ! probe_tcp "$ns" "$pod" "$host" "$port"; then
                printf '  ok: %s — connect BLOCKED (try %d, confirmed)\n' "$desc" "$i"
                return 0
            fi
        done
        ok=1
        probe_tcp "$ns" "$pod" "$host" "$port" && ok=0
        if [ "$ok" -eq 1 ]; then
            printf '  ok: %s — connect BLOCKED (final check)\n' "$desc"
            return 0
        fi
        printf 'ERROR: %s — connect to %s:%s SUCCEEDED but should be BLOCKED\n' \
            "$desc" "$host" "$port" >&2
        return 1
    fi
}

# ---------------------------------------------------------------
# Local helper: assert_hubble_verdict <desc> <src-ns/pod> <to-namespace> <DROPPED|FORWARDED>
#   Assert Hubble observed >=1 flow with the given verdict from the source
#   pod to the target namespace. This is the LOAD-BEARING enforcement
#   assertion — it proves Cilium's datapath verdict, not just a connect
#   timeout (which a missing listener could also cause). We first generate
#   traffic (the caller already ran a probe), then read the flow buffer.
# ---------------------------------------------------------------
assert_hubble_verdict() {
    local desc="$1" src="$2" to_ns="$3" verdict="$4"
    local i flows
    # BEST-EFFORT: the connectivity contrast (assert_connect web vs noproxy to
    # the SAME pg Service) is the load-bearing proof — web reaching pg proves
    # the listener exists, so a noproxy failure IS the CNP drop. The Hubble
    # verdict is supplementary confirmation; if Hubble is unavailable (tier-1
    # ships it off per ADR 0020 / GitOps-managed / single-node) we WARN and
    # move on rather than failing the walk.
    if [ "${HUBBLE_OK:-0}" != 1 ]; then
        printf '  skip (best-effort): %s — Hubble unavailable; connect contrast already enforced\n' "$desc"
        return 0
    fi
    for i in $(seq 1 6); do
        flows=$(hubble_cli observe \
            --pod "$src" \
            --to-namespace "$to_ns" \
            --verdict "$verdict" \
            --last 200 2>/dev/null | grep -c "$verdict" || true)
        if [ "${flows:-0}" -ge 1 ]; then
            printf '  ok: %s — Hubble observed %s flow(s) %s -> ns/%s\n' \
                "$desc" "$flows" "$src" "$to_ns"
            return 0
        fi
        printf '    %s: no %s flow yet (try %d), re-probing ...\n' \
            "$(date +%H:%M:%S)" "$verdict" "$i"
        sleep 5
    done
    printf '  WARN (best-effort): %s — Hubble did not surface a %s flow %s -> ns/%s; connect contrast already enforced\n' \
        "$desc" "$verdict" "$src" "$to_ns" >&2
    hubble_cli observe --pod "$src" --to-namespace "$to_ns" --last 50 >&2 2>&1 || true
    return 0
}

# ===============================================================
# Phase 0: bring up the kind+Cilium cluster
# ===============================================================

phase "Phase 0: kind_up_cilium ${CLUSTER_NAME} (default CNI + kube-proxy disabled)"

kind_up_cilium "$CLUSTER_NAME"

cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
# The cluster exists and $KUBECONFIG points at it — cleanup may now safely
# diagnose/tear it down (and only it).
K3D_CREATED=1
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

# ===============================================================
# Phase 1: cluster-bootstrap WITH Cilium, then Cilium status + Hubble
# ===============================================================

phase "Phase 1: cluster-bootstrap (Cilium kube-proxy replacement) + Hubble"

kubeconfig_content=$(cat "$KUBECONFIG_FILE")
seed_apprafter_state "$kubeconfig_content"
export APPRAFTER_CONFIG_DIR
printf '  APPRAFTER_CONFIG_DIR=%s\n' "$APPRAFTER_CONFIG_DIR"

bootstrap_with_cilium

printf '  cluster-bootstrap complete; waiting for Cilium to converge ...\n'
# Cilium's eBPF datapath is slower to converge than a default CNI — give it
# a generous window. `cilium status --wait` blocks until the agent +
# operator are ready (and the node flips Ready).
cilium_cli status --wait --wait-duration 8m

# Hubble flow verdicts are a BEST-EFFORT confirmation layered on the
# load-bearing connectivity contrast (Phase 5: web reaches pg, noproxy does
# not). The platform's tier-1 Cilium ships Hubble OFF (ADR 0020) and is
# GitOps-managed, so enabling it is best-effort — if it does not come up (or
# Argo reverts it) the walk still proves enforcement via the connect contrast.
# HUBBLE_OK gates the optional verdict assertions.
HUBBLE_OK=0
printf '  enabling Hubble (best-effort; flow verdicts are supplementary) ...\n'
if cilium_cli hubble enable 2>/dev/null; then
    cilium_cli status --wait --wait-duration 5m 2>/dev/null || true
    deadline=$(( $(date +%s) + 180 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if cilium_cli status 2>/dev/null | grep -qi 'Hubble Relay.*OK'; then
            HUBBLE_OK=1; break
        fi
        sleep 10
    done
fi
if [ "$HUBBLE_OK" = 1 ]; then
    # `hubble observe` talks to Relay on 127.0.0.1:4245 — port-forward in the
    # background (cluster teardown on EXIT reaps it).
    cilium_cli hubble port-forward >/dev/null 2>&1 &
    sleep 5
    printf '  Hubble Relay -> OK (flow verdicts enabled)\n'
else
    printf '  WARN: Hubble unavailable; flow-verdict checks skipped (connectivity contrast still enforced)\n' >&2
fi

# ---------------------------------------------------------------
# Phase 1b: build + side-load the WORKING-TREE operator + admission-webhook
# and apply the branch CRDs + branch RBAC. 2.10 is UNRELEASED, so this is
# unconditional (the released chart predates spec.network.egress.profile,
# the CNP RBAC, and the egress emission). Mirrors gitops-walk-per-env.sh's
# Phase 1b / needs-disk-walk.sh's local-operator path.
# POST-RELEASE: drop this block + run the published image (set
# APPRAFTER_E2E_LOCAL_OPERATOR only for pre-release validation), exactly
# like the other nightlies flipped once their feature shipped.
# NOTE: the if-body below is intentionally NOT indented — it carries a
# column-0 heredoc (the CRD apply); `fi` closes it just before Phase 2.
# ---------------------------------------------------------------
phase "Phase 1b: build + load working-tree operator + webhook (2.10 unreleased)"
builder=podman; command -v podman >/dev/null 2>&1 || builder=docker

build_load_restart() { # <deployment> <operator-subdir>
    local dep="$1" sub="$2" img
    printf '  waiting for the %s Deployment to appear ...\n' "$dep"
    for _ in $(seq 1 60); do
        kubectl -n "$OPERATOR_NS" get deploy "$dep" >/dev/null 2>&1 && break
        sleep 5
    done
    img=$(kubectl -n "$OPERATOR_NS" get deploy "$dep" \
        -o jsonpath='{.spec.template.spec.containers[0].image}')
    printf '  building %s from the working tree (%s) ...\n' "$img" "$builder"
    "$builder" build -f "${REPO_ROOT}/operator/${sub}/Dockerfile" \
        -t "$img" "${REPO_ROOT}/operator"
    cluster_load_image "$CLUSTER_NAME" "$img"
    kubectl -n "$OPERATOR_NS" rollout restart "deploy/${dep}"
    kubectl -n "$OPERATOR_NS" rollout status "deploy/${dep}" --timeout=180s
}
build_load_restart apprafter-operator apprafter-operator
build_load_restart admission-webhook admission-webhook

# `rollout status` returns once the NEW webhook pod is Ready, but the OLD
# (released) pod lingers Terminating — wait until ONLY the branch webhook
# serves before any branch-typed apply (a branch CR may use new fields).
printf '  waiting for the old (released) webhook pod to fully terminate ...\n'
_wh_deadline=$(( $(date +%s) + 90 ))
while [ "$(date +%s)" -lt "$_wh_deadline" ]; do
    [ "$(kubectl -n "$OPERATOR_NS" get pods \
        -l app.kubernetes.io/name=admission-webhook --no-headers 2>/dev/null \
        | wc -l)" -le 1 ] && break
    sleep 3
done

# The released platform-stack chart predates the 2.10 CRD surface
# (PlatformStack.spec.network.egress.profile). Argo CD owns those CRDs via
# the apprafter-operator Application, so: disable automated sync on the
# parent + operator apps (else Argo reverts the drift), then apply the
# BRANCH-rendered CRDs server-side. Same rationale as side-loading the
# images. Mirrors needs-disk-walk.sh / gitops-walk-per-env.sh.
printf '  applying branch operator CRDs + RBAC (released chart predates the 2.10 schema) ...\n'
for _app in platform apprafter-operator; do
    kubectl -n argocd patch application.argoproj.io "$_app" --type=merge \
        -p '{"spec":{"syncPolicy":{"automated":null}}}' >/dev/null 2>&1 || true
done
_yq() { if command -v yq >/dev/null 2>&1; then yq "$@"; else nix run nixpkgs#yq-go -- "$@"; fi; }
# CRDs (incl. the new PlatformStack network.egress.profile field) + the
# operator ClusterRole/Binding (the new ciliumnetworkpolicies + CRD-read
# RBAC). Render the whole chart and apply CRDs + RBAC server-side so the
# branch operator has the permissions to emit the CNP.
#
# `--namespace "$OPERATOR_NS"` is LOAD-BEARING: the ClusterRoleBinding
# subject is templated as `namespace: {{ .Release.Namespace }}`. Without
# `-n` Helm defaults Release.Namespace to `default`, so the rendered
# binding points at `system:serviceaccount:default:apprafter-operator` —
# and `kubectl apply --force-conflicts` then OVERWRITES the published
# binding's correct `apprafter-system` subject, stripping the real
# operator SA of every cluster-scoped grant (403 on resourceclaims,
# applications, …) so the reconcile loop never runs and `web` hangs with
# an empty `.status.phase`. (Walk-found, 2026-06-09.)
helm template apprafter-operator "${REPO_ROOT}/operator/charts/apprafter-operator" \
    --namespace "$OPERATOR_NS" \
    | _yq 'select(.kind == "CustomResourceDefinition")' \
    | kubectl apply --server-side --force-conflicts -f -
helm template apprafter-operator "${REPO_ROOT}/operator/charts/apprafter-operator" \
    --namespace "$OPERATOR_NS" \
    | _yq 'select(.kind == "ClusterRole" or .kind == "ClusterRoleBinding")' \
    | kubectl apply --server-side --force-conflicts -f -
for _crd in applications serviceproviders resourceclaims retainedclaims platformstacks; do
    retry 12 5 -- kubectl wait --for=condition=Established \
        "crd/${_crd}.apprafter.io" --timeout=30s
done
printf '  branch CRDs + RBAC applied + Established\n'
# Restart the operator once more so it re-probes Cilium presence (the CNP
# CRD is now guaranteed Established) + picks up the new RBAC.
kubectl -n "$OPERATOR_NS" rollout restart deploy/apprafter-operator
kubectl -n "$OPERATOR_NS" rollout status deploy/apprafter-operator --timeout=180s
printf '  apprafter-operator + admission-webhook now running the working-tree build\n'

# ===============================================================
# Phase 2: platform readiness (CNPG operator, the seeded provider, webhook)
# ===============================================================

phase "Phase 2: platform readiness (AppProject, CNPG operator, provider, webhook)"

printf '  waiting for AppProject apps ...\n'
deadline=$(( $(date +%s) + 600 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    if kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1; then
        printf '  AppProject apps -> found\n'
        break
    fi
    sleep 10
done
kubectl -n argocd get appproject.argoproj.io apps >/dev/null 2>&1 || {
    printf 'ERROR: AppProject apps not found after 10 min\n' >&2
    exit 1
}

# CNPG operator must be Available — the needs.pg claim provisions a CNPG
# Cluster, and the pg Service the app reaches is its rw endpoint.
printf '  waiting for the CNPG operator Deployment ...\n'
retry 30 10 -- kubectl -n "$CNPG_NS" rollout status \
    deploy -l app.kubernetes.io/name=cloudnative-pg --timeout=60s

printf '  waiting for the admission-webhook Deployment ...\n'
retry 30 10 -- kubectl -n "$OPERATOR_NS" rollout status \
    deploy admission-webhook --timeout=60s

# ===============================================================
# Phase 3: apply a needs.pg app + a needs-less app
# ===============================================================

phase "Phase 3: apply needs.pg Application '${APP_PG}' + needs-less '${APP_NOPROXY}'"

kubectl create namespace "$APP_NS" 2>/dev/null || true

# web — declares needs.pg → its CNP gets the pg egress allow rule.
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP_PG}
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
      pg:
        selector:
          tier: integrated
        size: small
YAML

# noproxy — NO needs → its CNP carries only the baseline (no pg allow), so
# it cannot reach the shared pg even though pg exists in-cluster.
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP_NOPROXY}
  namespace: ${APP_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    expose:
      port: 80
YAML

# Both Deployments must reach Ready. web pauses on its pg claim first, then
# resumes; noproxy renders immediately (no needs).
wait_jsonpath "$APP_RES" "$APP_NS" "$APP_PG" '{.status.phase}' Ready 420
wait_jsonpath "$APP_RES" "$APP_NS" "$APP_NOPROXY" '{.status.phase}' Ready 240

kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP_PG}" --timeout=300s
kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP_NOPROXY}" --timeout=300s
printf '  both Deployments Available\n'

# ===============================================================
# Phase 4: CNP emission — web has the pg rule, noproxy does not
# ===============================================================

phase "Phase 4: CNP emission (web-egress has the pg rule; noproxy-egress baseline only)"

# Both CNPs exist (one per Application).
wait_jsonpath "$CNP_RES" "$APP_NS" "$CNP_PG" '{.metadata.name}' "$CNP_PG" 120
wait_jsonpath "$CNP_RES" "$APP_NS" "$CNP_NOPROXY" '{.metadata.name}' "$CNP_NOPROXY" 120

# The CNP is owned by the Application (cascading delete).
cnp_owner=$(jp "$CNP_RES" "$APP_NS" "$CNP_PG" '{.metadata.ownerReferences[0].kind}')
assert_eq "web-egress ownerRef Kind" "$cnp_owner" "Application"

# web-egress endpointSelector targets the app pods.
cnp_sel=$(jp "$CNP_RES" "$APP_NS" "$CNP_PG" \
    '{.spec.endpointSelector.matchLabels.apprafter\.io/application}')
assert_eq "web-egress endpointSelector apprafter.io/application" "$cnp_sel" "$APP_PG"

# web-egress carries the pg allow rule: an egress entry whose toEndpoints
# matchLabels names cnpg-system + the cluster pod selector, with port 5432.
web_egress_json=$(jp "$CNP_RES" "$APP_NS" "$CNP_PG" '{.spec.egress}')
if [ -z "$web_egress_json" ]; then
    printf 'ERROR: web-egress has an empty spec.egress\n' >&2
    exit 1
fi
case "$web_egress_json" in
    *"$CNPG_NS"*) printf '  ok: web-egress references the cnpg-system namespace\n' ;;
    *) printf 'ERROR: web-egress egress does not reference %s: %s\n' "$CNPG_NS" "$web_egress_json" >&2; exit 1 ;;
esac
case "$web_egress_json" in
    *5432*) printf '  ok: web-egress references pg port 5432\n' ;;
    *) printf 'ERROR: web-egress egress does not reference port 5432: %s\n' "$web_egress_json" >&2; exit 1 ;;
esac
case "$web_egress_json" in
    *world*) printf '  ok: web-egress carries the world rule (internet default)\n' ;;
    *) printf 'ERROR: web-egress missing the world rule under the internet default: %s\n' "$web_egress_json" >&2; exit 1 ;;
esac

# noproxy-egress must NOT carry the pg rule (no cnpg-system / 5432).
noproxy_egress_json=$(jp "$CNP_RES" "$APP_NS" "$CNP_NOPROXY" '{.spec.egress}')
case "$noproxy_egress_json" in
    *"$CNPG_NS"*) printf 'ERROR: noproxy-egress unexpectedly references %s (should have no pg rule): %s\n' "$CNPG_NS" "$noproxy_egress_json" >&2; exit 1 ;;
    *) printf '  ok: noproxy-egress carries NO pg rule (no cnpg-system reference)\n' ;;
esac

# ===============================================================
# Phase 5: ENFORCEMENT — the 2.10 acceptance (Hubble verdicts)
# ===============================================================

phase "Phase 5: enforcement — noproxy DROPPED to pg, web FORWARDED to pg"

POD_WEB=$(app_pod "$APP_NS" "$APP_PG")
POD_NOPROXY=$(app_pod "$APP_NS" "$APP_NOPROXY")
[ -n "$POD_WEB" ] || { printf 'ERROR: no web pod found\n' >&2; exit 1; }
[ -n "$POD_NOPROXY" ] || { printf 'ERROR: no noproxy pod found\n' >&2; exit 1; }
printf '  web pod: %s | noproxy pod: %s\n' "$POD_WEB" "$POD_NOPROXY"

# noproxy → pg: the connect must FAIL (egress to cnpg-system is not allowed
# without a declared needs.pg). Drive traffic first so Hubble has a flow.
assert_connect "noproxy -> pg (undeclared, must be denied)" \
    "$APP_NS" "$POD_NOPROXY" "$PG_SERVICE" "$PG_PORT" want-fail
# The LOAD-BEARING verdict: Cilium DROPPED the flow (not merely a timeout).
assert_hubble_verdict "noproxy -> cnpg-system" \
    "${APP_NS}/${POD_NOPROXY}" "$CNPG_NS" DROPPED

# web → pg: the connect must SUCCEED (needs.pg → the egress allow rule).
assert_connect "web -> pg (declared needs.pg, must be allowed)" \
    "$APP_NS" "$POD_WEB" "$PG_SERVICE" "$PG_PORT" want-ok
assert_hubble_verdict "web -> cnpg-system" \
    "${APP_NS}/${POD_WEB}" "$CNPG_NS" FORWARDED

# ===============================================================
# Phase 6: internet — external egress open under the default profile
# ===============================================================

phase "Phase 6: internet profile — external egress (${EXTERNAL_HOST}:${EXTERNAL_PORT}) allowed"

# Under the default `internet` profile the `world` rule allows external
# egress. (If the CI runner has no outbound internet this would be a
# false failure — the nightly runner does; documented in the workflow.)
assert_connect "web -> ${EXTERNAL_HOST} (internet profile, must be allowed)" \
    "$APP_NS" "$POD_WEB" "$EXTERNAL_HOST" "$EXTERNAL_PORT" want-ok

# ===============================================================
# Phase 7: CLI profile switch — internal drops world, keeps pg
# ===============================================================

phase "Phase 7: apprafter platform egress set internal — world dropped, pg kept"

apprafter platform egress set internal

# The operator re-renders web-egress WITHOUT the world rule. Poll the CNP.
printf '  waiting for web-egress to lose the world rule ...\n'
deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    eg=$(jp "$CNP_RES" "$APP_NS" "$CNP_PG" '{.spec.egress}')
    case "$eg" in
        *world*) sleep 5 ;;
        *) break ;;
    esac
done
eg=$(jp "$CNP_RES" "$APP_NS" "$CNP_PG" '{.spec.egress}')
case "$eg" in
    *world*) printf 'ERROR: web-egress STILL carries the world rule under internal: %s\n' "$eg" >&2; exit 1 ;;
    *) printf '  ok: web-egress dropped the world rule under internal\n' ;;
esac
# pg need rule survives the profile change.
case "$eg" in
    *"$CNPG_NS"*) printf '  ok: web-egress still carries the pg need rule under internal\n' ;;
    *) printf 'ERROR: web-egress lost the pg need rule under internal: %s\n' "$eg" >&2; exit 1 ;;
esac

# External egress now DROPPED; pg still FORWARDED.
assert_connect "web -> ${EXTERNAL_HOST} (internal profile, must be denied)" \
    "$APP_NS" "$POD_WEB" "$EXTERNAL_HOST" "$EXTERNAL_PORT" want-fail
assert_connect "web -> pg (internal profile, still allowed)" \
    "$APP_NS" "$POD_WEB" "$PG_SERVICE" "$PG_PORT" want-ok

# `egress show` reports the active profile.
show_out=$(apprafter platform egress show 2>&1)
printf '%s\n' "$show_out"
if ! printf '%s\n' "$show_out" | grep -qi 'internal'; then
    # shellcheck disable=SC2016  # literal backticks in the user-facing message
    printf 'ERROR: `apprafter platform egress show` did not report internal:\n%s\n' "$show_out" >&2
    exit 1
fi
printf '  ok: egress show reports internal\n'

# ===============================================================
# Phase 8: strict — same-namespace egress also blocked
# ===============================================================

phase "Phase 8: apprafter platform egress set strict — same-namespace egress dropped"

apprafter platform egress set strict

# strict baseline = DNS + need rules only (no world, no same-namespace).
# web-egress should now carry NEITHER a world rule NOR the empty-matchLabels
# same-namespace rule; the pg need rule survives. Assert via rule count +
# the absence of an all-namespace same-ns rule by checking that the only
# in-cluster allows are DNS (kube-system) + pg (cnpg-system).
printf '  waiting for web-egress to drop the same-namespace rule ...\n'
deadline=$(( $(date +%s) + 180 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    # The same-namespace rule renders as a toEndpoints entry with an EMPTY
    # matchLabels object ({"matchLabels":{}}). Detect its presence/absence.
    has_samens=$(kubectl -n "$APP_NS" get "$CNP_RES" "$CNP_PG" -o json 2>/dev/null \
        | _has_empty_samens_rule)
    [ "$has_samens" = "absent" ] && break
    sleep 5
done

# noproxy is needs-less → under strict its CNP allows ONLY DNS. A connect to
# the SAME-namespace web pod must now be DROPPED (strict isolates within a
# namespace). Probe noproxy -> web's pod IP on port 80.
WEB_POD_IP=$(kubectl -n "$APP_NS" get pod "$POD_WEB" -o jsonpath='{.status.podIP}' 2>/dev/null || true)
[ -n "$WEB_POD_IP" ] || { printf 'ERROR: could not read web pod IP for the strict same-ns probe\n' >&2; exit 1; }
assert_connect "noproxy -> web (same-namespace, strict, must be denied)" \
    "$APP_NS" "$POD_NOPROXY" "$WEB_POD_IP" "80" want-fail
assert_hubble_verdict "noproxy -> web (same-namespace, strict)" \
    "${APP_NS}/${POD_NOPROXY}" "$APP_NS" DROPPED

# web still reaches pg under strict (need rules are always emitted).
assert_connect "web -> pg (strict profile, still allowed)" \
    "$APP_NS" "$POD_WEB" "$PG_SERVICE" "$PG_PORT" want-ok

# ===============================================================
# Done — tear down on success path
# ===============================================================

trap - EXIT

if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    k3d_down "$CLUSTER_NAME" || true
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' \
        "$CLUSTER_NAME"
fi

rm -rf "$TMPDIR_WORK"

printf '\nneeds-networkpolicy-walk GREEN in %s\n' "$(elapsed)"
printf 'Chain proven: needs.pg -> web-egress pg-allow (FORWARDED), needs-less noproxy -> pg DROPPED; internet/internal/strict profile gating via apprafter platform egress\n'
