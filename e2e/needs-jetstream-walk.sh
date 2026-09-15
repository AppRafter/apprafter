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
# Plus, while a cluster exists: a claim declaring `streams: [...]` reaches
# Ready=True only once NACK reports those streams live — the restored
# AwaitingStreamCreation gate, which is what stops the status lie the
# first 2.5e run recorded (`part3[1] PASS` beside `part3[2] FAIL`) — and
# the operator log carries no forbidden-verb complaint (the class an RBAC
# miss manifests as: a reconcile that silently never progresses).
#
# 2.5f adds five more (acceptance #9-#13), and they are the half of that
# subphase no unit test can reach, because each one is a claim about what
# a REAL nats-server does when nobody is enforcing anything:
#
#   9.  the observed-stream inventory lands on the claim, classified —
#       a dynamic stream as dynamic, a declared one as declared.
#   10. an application holding `dynamicStreams: true` creates a stream
#       under a NEIGHBOUR's subject prefix — which no permission refuses,
#       because a stream's subjects travel in a request body the server
#       never inspects — and the platform DETECTS it on the victim's
#       claim without deleting it, while leaving the innocent alone.
#   11. NamespaceDrainRisk appears on a workqueue's owner while a
#       dynamicStreams neighbour shares its account, and on nobody else.
#   12. ConsumeTargetMissing appears once the consumed stream's owning
#       application has departed.
#   13. an over-budget declaration is refused BY US, with the account's
#       numbers, instead of by nats-server as an opaque 10047 through
#       NACK — and no Stream CR is applied at all.
#
# 2.5 part 4 adds the fourteenth, and it is about an edit that is REFUSED
# rather than about NATS at all:
#
#   14. turning `dynamicStreams` on pauses the application behind a
#       security-boundary MigrationPlan (ADR 0052 trigger #15) before any
#       child is applied, and taking it back off — a narrowing — clears the
#       gate instead of asking again. The classification is unit-tested;
#       what only a cluster shows is that the edit actually STOPS, with the
#       plan's approval hash stamped and no claim generated meanwhile.
#
# And the fifteenth covers the one hop the other fourteen cannot see,
# because a documentation pass found it after they were all green:
#
#   15. the per-application egress CiliumNetworkPolicy carries a rule for
#       nats-system:4222, and the selector in that rule — read back off the
#       APPLIED object, not typed in here — selects the running nats-0.
#       `default_target` shipped with arms for pg and redis and a catch-all
#       `_ => None`, so `needs.jetstream` got no rule while the CNP still
#       made the app's pods egress default-deny: working credentials, no
#       route. Neither of the two reasons this walk missed it has been
#       removed (it still runs without a cilium-agent, and its NATS traffic
#       still comes from nats-box, which is not an Application) — #15 works
#       around both by reading the object and asking the apiserver to match
#       its labels. ENFORCEMENT remains unproven here; see the stand-in-CRD
#       comment in Phase 1b for exactly where that line falls.
#
# The sixteenth is the last 2.5 signal that had never been seen outside a
# unit test, and the only reason `docs/status.md` still read `🚧`:
#
#   16. an application ARRIVING into a namespace where a neighbour already
#       declares a stream collecting its `<app>.` prefix is TOLD so, on its
#       own claim, with the neighbour's composed stream named — and the
#       claim still reaches Ready, because a gated fan-in declaration is a
#       legitimate construct (ADR 0061 §6/§7). The platform is not refusing
#       anything; it is refusing to let it be a surprise. Two negatives
#       ride along (the declarer's OWN claim and an uninvolved third app),
#       and because "the condition is correctly absent" and "the condition
#       machinery never ran" look identical from outside, each negative is
#       gated on that claim having received a `status.streams` write first
#       — the inventory and the conditions travel in ONE server-side apply,
#       so an `observedAt` is proof the rules ran and chose silence. The
#       live MUTATION is what makes the negatives mean anything at all:
#       the foreign subject is taken back off the declaration and the
#       condition has to CLEAR.
#
# The seventeenth is the budget nobody was counting, found while writing
# the sixteenth:
#
#   17. a namespace whose account would push the cluster past the shared
#       NATS server's JetStream MEMORY budget is REFUSED, on its own
#       claim, naming the budget — instead of hanging on
#       AwaitingNatsUserReady. Each namespace account reserves `max_mem`
#       on the ONE server (a tenth of its file quota, floored at 64Mi)
#       against `component_nats.cue`'s `memoryStore.maxSize: 192Mi`, and
#       nothing summed the reservations against that ceiling. The chart's
#       own comment predicted a loud failure (a fatal exit at startup);
#       measured on the RELOAD path — which is the path production takes —
#       it is silent: nats-server 2.14.3 logs `insufficient memory
#       resources available (10028)`, reports `Reloaded server
#       configuration` anyway, keeps serving, and leaves an arbitrary,
#       run-to-run-varying subset of accounts (previously-working ones
#       included) with no JetStream while their users still authenticate
#       normally. The render is now refused before that file can ever be
#       written, so this criterion is also the proof that the refusal
#       leaves the INSTALLED file — and every namespace already on it —
#       untouched.
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
#   SUBSTITUTED:  the StatefulSet AND the nack (jetstream-controller)
#     Deployment + its jetstream.nats.io CRDs coming into existence.
#     Once (and only once) the walk has observed the override flip for
#     real, it applies BOTH the `nats` and `nack` charts' OWN rendered
#     manifests by hand (Phase 4b, using component_nats.cue's /
#     component_nack.cue's exact pinned values — `nack` WITH
#     `--include-crds`, since a plain `helm template` does not render a
#     chart's `crds/` directory at all) — standing in for the Argo CD
#     sync that would otherwise create both. Everything downstream of
#     that point (the StatefulSet mounting the provisioner's REAL
#     Secret, the reloader picking up its REAL content, SIGHUP, the
#     provisioner's own verify step, AND — 2.5e — nack actually
#     reconciling the Stream/Consumer/Account CRs the provisioner
#     applies) is then exercised for real, against a REAL nats-server +
#     reloader + nack triple — this is NOT a stand-in for any of THOSE
#     questions, only for "does PlatformController correctly render a
#     nats/nack Application from an override against a published
#     chart," which is generic platform-stack plumbing already
#     exercised by every other component in prior phases of this
#     project, not something 2.5d/2.5e's own tasks touched.
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

APP3="streamapp"                     # declares a stream — part-3 acceptance #1/#2/#5/#6
CLAIM3="streamapp-jetstream"

APP4="consumerapp"                   # consume-only — part-3 acceptance #3/#4/#5
CLAIM4="consumerapp-jetstream"

APP5="streamapp2"                    # neighbour declared stream — part-3 acceptance #6/#8
CLAIM5="streamapp2-jetstream"

APP6="latecomerapp"                  # arrives AFTER streamapp2 has data — part-3 acceptance #8
CLAIM6="latecomerapp-jetstream"

APP7="hogapp"                        # declares more than the account holds — 2.5f acceptance #13
CLAIM7="hogapp-jetstream"

# A separate, minimal namespace for the account-lifecycle checks (part-3
# acceptance #7) — kept apart from "demo" so tearing these two down does
# not disturb the isolation/survival/hot-reload fixtures already standing
# there (walkapp/walkapp2/streamapp/streamapp2/consumerapp).
ACCT_NS="jslife"
ACCTA="accta"
ACCTB="acctb"

APP_RES="application.apprafter.io"
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

for tool in cargo kubectl helm jq; do
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

# mgr_nats_run <namespace> <nats-args...> — run the `nats` CLI as the
# per-namespace management user (`mgr_<ns>`, `nats::mgr_secret_name`),
# whose password lives in `nats-mgr-<ns>` in nats-system. Part-3
# acceptance criteria #2/#3/#5/#6/#8 query stream/consumer existence THIS
# way — the management identity, not any one claim's own user — because
# it is the identity NACK application itself is meant to act as (ADR 0061
# §2/§8), and it has no subject-prefix restriction of its own.
mgr_nats_run() {
    local ns="$1"; shift
    local mgr_pass
    mgr_pass=$(secret_val "$NATS_NS" "nats-mgr-${ns}" password)
    if [ -z "$mgr_pass" ]; then
        printf 'mgr password for namespace %s not found in nats-mgr-%s\n' "$ns" "$ns" >&2
        return 1
    fi
    nats_run --server "$CONN1_SERVER" --user "mgr_${ns}" --password "$mgr_pass" "$@"
}

# force_grace_retainedclaim <claim-name> <claim-ns>
#   Mirrors needs-pg-walk.sh's Phase 9 technique EXACTLY: RetainedClaim is
#   immutable by admission (CEL self==oldSelf), so an in-place patch of
#   retainUntil is rejected — delete + recreate the SAME snapshot with a
#   retainUntil already in the past, so the grace-GC fires on its next
#   pass instead of after seven real days. This is NOT a shortened grace
#   period or an injected clock: it is the established, already-audited
#   mechanism this codebase uses everywhere it needs to force a
#   grace-gated GC inside a walk, reused verbatim rather than invented.
#   Generic over the snapshot's own backend-specific fields (read-modify-
#   write via jq rather than hardcoding a shape) since a jetstream claim's
#   RetainedClaim shape is not yet decided by this branch.
force_grace_retainedclaim() {
    local claim_name="$1" claim_ns="$2" rc_name
    rc_name="claim-${claim_ns}-${claim_name}"
    printf '  waiting for RetainedClaim %s to be snapshotted ...\n' "$rc_name"
    wait_jsonpath retainedclaim "$RETAINED_NS" "$rc_name" '{.spec.claimRef.name}' "$claim_name" 60
    local patched
    patched=$(kubectl -n "$RETAINED_NS" get retainedclaim "$rc_name" -o json \
        | jq '.spec.retainUntil = "2000-01-01T00:00:00Z" | {apiVersion, kind, metadata: {name: .metadata.name, namespace: .metadata.namespace}, spec}')
    kubectl -n "$RETAINED_NS" delete retainedclaim "$rc_name" --wait=true
    printf '%s' "$patched" | kubectl apply -f -
    printf '  ok: forced grace on RetainedClaim %s (past retainUntil, mirrors needs-pg-walk.sh Phase 9)\n' "$rc_name"
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

# ---------------------------------------------------------------
# A STAND-IN CiliumNetworkPolicy CRD — applied BEFORE the operator
# restarts, because the operator probes for this CRD exactly once, at
# leadership acquisition (`cilium_available` in apprafter-operator's
# main.rs), and skips the egress-CNP apply for the rest of its life when
# the probe comes back false.
#
# WHAT THIS BUYS, precisely: the operator renders and APPLIES the
# per-Application egress CNP, so acceptance #15 can read the NATS rule
# the shipped renderer actually produced out of the cluster and match its
# selector against the labels on the live nats-0. Without the CRD there is
# no object to read and the rule can only be inspected by reading the
# source, which is what let the missing `jetstream` arm in
# `default_target` ship in the first place.
#
# WHAT IT DOES NOT BUY, and nothing here should be read as claiming it:
# ENFORCEMENT. There is no cilium-agent on this cluster (bootstrap runs
# with APPRAFTER_BOOTSTRAP_SKIP_CILIUM=1 — see lib.sh on why Cilium's
# datapath is not viable on the k3d/kindnet substrate these walks use), so
# nothing ever evaluates this policy. A CNP here is an inert object. The
# schema is `x-kubernetes-preserve-unknown-fields` rather than Cilium's
# own, so it does not validate the policy either — it only stores it.
# Proving that a `needs.jetstream` pod's traffic is FORWARDED while an
# undeclared pod's is DROPPED needs Hubble verdicts on a real Cilium
# cluster, which is `e2e/needs-networkpolicy-walk.sh`'s job (kind_up_cilium
# + bootstrap_with_cilium); that walk covers pg today and extending it to
# jetstream means standing NATS up inside it.
#
# So #15 closes the two failure modes reachable without a datapath — "no
# rule at all" and "a rule whose selector matches nothing" — which between
# them are the whole of what went wrong here. It does not close "the rule
# exists, matches, and Cilium still drops the packet".
printf '  applying a stand-in CiliumNetworkPolicy CRD (storage only, no agent — see this block'"'"'s comment) ...\n'
kubectl apply -f - <<'YAML'
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: ciliumnetworkpolicies.cilium.io
spec:
  group: cilium.io
  names:
    kind: CiliumNetworkPolicy
    listKind: CiliumNetworkPolicyList
    plural: ciliumnetworkpolicies
    singular: ciliumnetworkpolicy
    shortNames: [cnp]
  scope: Namespaced
  versions:
    - name: v2
      served: true
      storage: true
      schema:
        openAPIV3Schema:
          type: object
          x-kubernetes-preserve-unknown-fields: true
YAML
retry 12 5 -- kubectl wait --for=condition=Established crd/ciliumnetworkpolicies.cilium.io --timeout=30s
printf '  stand-in CNP CRD Established (the operator probes for it at startup, which is next)\n'

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
    # ADR 0061 §5's policy ladder, mirroring service_providers.cue's own
    # value. 'report' is the default and what ships; 'delete' would have
    # the provisioner remove a captured stream, which is exactly what
    # acceptance #10 below must NOT have happen while it is looking.
    capturePolicy: report
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

# nack (2.5e) — the SAME substitution, for the SAME reason
# (component_nack.cue is equally unpublished). `--include-crds` is
# load-bearing: a plain `helm template` does NOT render a chart's
# `crds/` directory at all (confirmed against this exact chart —
# component_nack.cue's own comment records it), only `helm install` /
# Argo CD's Helm source handling does — so without this flag the
# jetstream.nats.io CRDs never appear and every part-3 acceptance check
# would stay red for a HARNESS reason, not a product one.
cat >"${TMPDIR_WORK}/nack-values.yaml" <<VALUES
jetstream:
  nats:
    url: "nats://nats.${NATS_NS}.svc:4222"
  # 2.5e walk finding — mirrors component_nack.cue EXACTLY, and the one
  # value in this file that the part-3 criteria actually depend on.
  # Without --crd-connect, nack IGNORES spec.account on every Stream and
  # Consumer (so the provisioner's Account CR is never consulted and
  # every declared stream lands 'Errored') AND opens a global connection
  # at startup that the server rejects the moment an accounts file
  # exists. See component_nack.cue's own comment for both measurements.
  additionalArgs:
    - "--crd-connect"
resources:
  requests:
    cpu: 25m
    memory: 12Mi
  limits:
    memory: 48Mi
VALUES
helm template nack nats/nack --version 0.35.0 --include-crds -n "$NATS_NS" -f "${TMPDIR_WORK}/nack-values.yaml" \
    | kubectl apply -n "$NATS_NS" -f -
printf '  applied the (unpublished) nack component'"'"'s rendered manifests by hand (CRDs included)\n'

printf '  waiting for the %s StatefulSet to report a ready replica ...\n' "$NATS_STS"
wait_jsonpath statefulset "$NATS_NS" "$NATS_STS" '{.status.readyReplicas}' 1 300

printf '  waiting for the jetstream.nats.io CRDs to be Established ...\n'
for _crd in streams consumers accounts; do
    retry 24 5 -- kubectl wait --for=condition=Established "crd/${_crd}.jetstream.nats.io" --timeout=30s
done
printf '  waiting for the nack (jetstream-controller) Deployment ...\n'
retry 30 10 -- kubectl -n "$NATS_NS" rollout status deploy/nack --timeout=60s

# nack RESTART BASELINE, not an assertion. Both charts are applied
# together above, so nack normally starts before the nats StatefulSet is
# listening and exits once or twice with `no servers available for
# connection` — a benign startup race that has nothing to do with the
# credential. Measured on a GREEN run: 3 restarts before it settled.
# Everything the nack assertion below cares about is what happens AFTER
# this point, so the baseline is what it compares against.
NACK_POD_BEFORE=$(kubectl -n "$NATS_NS" get pod -l app=nack -o jsonpath='{.items[0].metadata.name}')
NACK_RESTARTS_BEFORE=$(kubectl -n "$NATS_NS" get pod "$NACK_POD_BEFORE" \
    -o jsonpath='{.status.containerStatuses[?(@.name=="jsc")].restartCount}')
printf '  nack baseline: pod=%s restarts=%s\n' "$NACK_POD_BEFORE" "${NACK_RESTARTS_BEFORE:-0}"

NATS_POD="${NATS_STS}-0"
RESTARTS_BEFORE=$(kubectl -n "$NATS_NS" get pod "$NATS_POD" -o jsonpath='{.status.containerStatuses[?(@.name=="nats")].restartCount}')
START_BEFORE=$(kubectl -n "$NATS_NS" get pod "$NATS_POD" -o jsonpath='{.status.containerStatuses[?(@.name=="nats")].state.running.startedAt}')
printf '  nats-0 baseline: restarts=%s startedAt=%s\n' "$RESTARTS_BEFORE" "$START_BEFORE"

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM1" '{.status.ready}' true 240
conn1_ref=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM1" '{.status.connectionSecretRef}')
assert_eq "status.connectionSecretRef" "$conn1_ref" "$CONN1"
printf '  ok: claim %s Ready=True — deployment/verify pipeline (2.5d Task 6) closed the loop for real\n' "$CLAIM1"

# 2.5e: nack is STILL healthy, and has not restarted, now that an
# accounts file exists and a claim has been provisioned against it.
#
# This assertion is here, and hard-fails here, because of what the first
# 2.5e walk cost: nack was green at the `rollout status` above (an
# accounts-file-less server accepts anonymous connections), then died the
# instant the first claim wrote the accounts file — a server WITH
# accounts-and-users rejects unauthenticated clients, and without
# --crd-connect nack opens an unauthenticated global connection at
# startup. It CrashLoopBackOff'd from roughly minute 8
# and the walk ran another ten minutes before reporting seven red part-3
# criteria, none of which named the cause. A component that is healthy at
# bootstrap and dies on first use is exactly what a one-shot early
# `rollout status` cannot see, so the check has to be repeated AFTER the
# thing that breaks it.
#
# A restart DELTA, not `restartCount == 0`: an absolute-zero check is
# wrong here and was measured wrong on the very next run — both charts
# are applied together, so nack normally exits once or twice with
# `no servers available for connection` before the nats StatefulSet is
# listening. That is a startup race, not a credential failure, and a
# check that cannot tell the two apart would be red on every green run.
# The baseline is captured at the `rollout status` above, and this
# function ratchets it forward so a later call cannot be satisfied by
# restarts an earlier one already accepted.
assert_nack_healthy() {
    local label="$1" pod phase ready restarts baseline
    pod=$(kubectl -n "$NATS_NS" get pod -l app=nack \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
    if [ -z "$pod" ]; then
        printf 'ERROR (%s): no nack pod in %s at all\n' "$label" "$NATS_NS" >&2
        exit 1
    fi
    phase=$(kubectl -n "$NATS_NS" get pod "$pod" -o jsonpath='{.status.phase}')
    ready=$(kubectl -n "$NATS_NS" get pod "$pod" \
        -o jsonpath='{.status.containerStatuses[?(@.name=="jsc")].ready}')
    restarts=$(kubectl -n "$NATS_NS" get pod "$pod" \
        -o jsonpath='{.status.containerStatuses[?(@.name=="jsc")].restartCount}')
    # A REPLACED pod restarts its own counter from zero, so the baseline
    # only applies to the same pod.
    baseline="${NACK_RESTARTS_BEFORE:-0}"
    [ "$pod" = "$NACK_POD_BEFORE" ] || baseline=0
    printf '  nack health (%s): pod=%s phase=%s ready=%s restarts=%s (baseline %s)\n' \
        "$label" "$pod" "$phase" "$ready" "${restarts:-0}" "$baseline"
    if [ "$phase" != "Running" ] || [ "$ready" != "true" ] || [ "${restarts:-0}" -gt "$baseline" ]; then
        printf 'ERROR (%s): nack is not Running/ready, or has restarted SINCE the baseline — it was healthy at bootstrap and broke on first use.\n' \
            "$label" >&2
        printf 'The classic cause is the NATS server rejecting nack'"'"'s global connection once an accounts file exists — check that --crd-connect is in its args (see component_nack.cue'"'"'s own comment). nack log:\n' >&2
        kubectl -n "$NATS_NS" logs "$pod" --tail=40 >&2 2>&1 || true
        kubectl -n "$NATS_NS" logs "$pod" --previous --tail=40 >&2 2>&1 || true
        exit 1
    fi
    # Ratchet: a LATER call must not be satisfied by restarts an EARLIER
    # one already tolerated.
    NACK_POD_BEFORE="$pod"
    NACK_RESTARTS_BEFORE="${restarts:-0}"
}
assert_nack_healthy "after the first jetstream claim was provisioned"

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

assert_not_contains "the CLIENT cannot confirm the stream add without an inbox prefix" "$neg_out" "was created"
printf '  ok: the missing-inbox-prefix attempt failed FROM THE CLIENT'"'"'S SIDE as expected, in %d ms\n' "$NEG_MS"
printf '  observed failure text: %s\n' "$neg_out"

# MEASURED 2026-09-12, and it corrects what this phase used to imply.
#
# The client'"'"'s failure above is an ACKNOWLEDGEMENT failure, not an
# authorization one. What NATS denies is the REPLY inbox subscription; the
# request published alongside it is processed normally, so the stream IS
# created. Queried here as mgr_demo — the identity that can see the account
# for real rather than through the failing client.
#
# 2.5f'"'"'s inventory is what surfaced this: walkapp'"'"'s `status.streams`
# listed `negstream` as a live dynamic stream long after this phase had
# reported a failure. Asserted here so the fact is a guard rather than a
# footnote, and so a later reader does not take "expect failure" to mean
# "expect refusal" — it is a silent SUCCESS reported to the caller as a
# timeout, which is worse than either, because the natural response is to
# retry and the retry is what creates duplicates.
if mgr_nats_run "$APP_NS" stream info negstream --json >/dev/null 2>&1; then
    printf '  FINDING (measured): negstream EXISTS in NATS despite the client reporting failure — the missing inbox prefix costs the ACK, not the write.\n'
else
    printf 'ERROR: negstream does NOT exist, but this phase measured that it does (2026-09-12): the denied inbox costs only the reply, never the write. Either the client now aborts on the permissions violation before publishing, or the server refuses the create — both change what this phase proves, so re-derive the measurement rather than deleting this assertion.\n' >&2
    exit 1
fi

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
# Part 3 acceptance criteria (Task 1, written before any of part 3
# exists — ADR 0061 §8's Stream/Consumer/Account CR application over
# NACK, and the account-lifecycle correction over ADR 0042 §9.6).
#
# THESE EIGHT CHECKS ARE DELIBERATELY NOT FAIL-FAST. Every other phase in
# this file uses `set -e` semantics (the first failed assertion aborts
# the whole walk) because each one is an already-shipped capability: a
# regression anywhere should stop the walk cold. This section is
# different in kind — it is the SPECIFICATION for work that does not
# exist yet, checked incrementally as each of part 3's sub-tasks lands.
# A fail-fast version would only ever report on whichever criterion is
# EARLIEST unmet, hiding whether the ones after it have quietly
# regressed once they start passing. So each check runs regardless of
# the others' outcomes, results accumulate in PART3_FAILED, and the
# walk's OWN exit code (set at the very end) reflects the count — red
# today, meant to go green one sub-task at a time, never edited to match
# whatever ships.
# ===============================================================

phase "Part 3 fixtures: streamapp (still applied, no longer asserted here) + consumerapp + streamapp2 + jslife"

# streamapp (APP3/CLAIM3) — SAME app Part 2's AwaitingStreamCreation test
# used. Its outcome is now checked by acceptance criterion #1 below,
# which REPLACES that assertion rather than sitting beside it.
#
# The gate itself was NOT deleted when part 3 landed, contrary to what
# this comment used to say. It was rewritten: "a declared stream is never
# deliverable" became "this claim is not ready until NACK reports its own
# Stream/Consumer CRs live". So #1 passing means the streams really exist,
# which is exactly what #2 then goes and checks independently — the first
# 2.5e run had #1 green and #2 red at the same time, because the gate had
# been deleted on the promise of a change that had not actually worked.
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
            # 256Mi, NOT 1Gi — and the number is arithmetic, not taste.
            # Every claim in this namespace defaults to size 'small'
            # (268435456 B, the seeded sizeBytes above), and the account's
            # max_file is the SUM over the namespace: 5 claims in 'demo'
            # => ~1.25Gi. Two 1Gi streams do not both fit, and the second
            # is refused by nats-server with 'insufficient storage
            # resources available (10047)' — reproduced directly in podman.
            # The product handles that correctly (the claim stays unready
            # and now says so with NACK's own reason), but criteria 6 and 8
            # need BOTH streams to exist, so the fixture must not
            # over-subscribe the account it shares.
            maxBytes: "256Mi"
YAML

# consumerapp (APP4/CLAIM4) — consume-only (no streams/dynamicStreams of
# its own): declares a durable on streamapp's stream. `dynamicStreams`
# is irrelevant to a consume declaration — CONSUMER.CREATE/DURABLE.CREATE
# are already unconditionally in every claim's own allow list
# (nats_accounts::allow_list); what is NOT there is the STREAM to consume
# from, which only NACK (part 3) can create for a declared, non-dynamic
# stream. So this claim is expected to reach Ready=True today (nothing
# gates on `consume`, only on `streams` — 2.5d part 2's own scope), while
# the durable it names remains acceptance criterion #3/#4's business.
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP4}
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
        consume:
          - from: ${APP3}
            stream: orders
            durable: reader
YAML

# streamapp2 (APP5/CLAIM5) — a SECOND, independent declared-stream app in
# the SAME namespace/account: the "neighbour" acceptance criterion #6
# needs (delete removes only the deleted app's stream) and the
# pre-existing data criterion #8 needs (a later arrival must not clear
# it).
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP5}
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
          - name: invoices
            subjects: ["streamapp2.invoices.>"]
            # 'workqueue', and the retention is the POINT, not a detail:
            # ADR 0061 §4.1's 'sources' drain is destructive only against
            # a workqueue origin, so 'NamespaceDrainRisk' (acceptance #11)
            # has nothing to fire on without one. Also keeps criterion #8
            # honest — a workqueue deletes a message once ACKED, and
            # nothing consumes this stream, so the probe message stays.
            retention: workqueue
            # 256Mi, NOT 1Gi — and the number is arithmetic, not taste.
            # Every claim in this namespace defaults to size 'small'
            # (268435456 B, the seeded sizeBytes above), and the account's
            # max_file is the SUM over the namespace: 5 claims in 'demo'
            # => ~1.25Gi. Two 1Gi streams do not both fit, and the second
            # is refused by nats-server with 'insufficient storage
            # resources available (10047)' — reproduced directly in podman.
            # The product handles that correctly (the claim stays unready
            # and now says so with NACK's own reason), but criteria 6 and 8
            # need BOTH streams to exist, so the fixture must not
            # over-subscribe the account it shares.
            maxBytes: "256Mi"
YAML

wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM3" '{.spec.type}' jetstream 120
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM4" '{.spec.type}' jetstream 120
wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM5" '{.spec.type}' jetstream 120

# jslife — a separate, minimal namespace for the account-lifecycle checks
# (#7), kept apart from "demo" so tearing these two claims all the way
# down does not disturb the fixtures still standing there. Plain
# dynamicStreams:true claims — no NACK dependency — so both are expected
# to reach Ready=True today; #7 is about what happens to the ACCOUNT once
# they are deleted, not about whether they can be provisioned.
kubectl create namespace "$ACCT_NS" 2>/dev/null || true
for _acct_app in "$ACCTA" "$ACCTB"; do
kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${_acct_app}
  namespace: ${ACCT_NS}
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
done
wait_jsonpath "$CLAIM_RES" "$ACCT_NS" "${ACCTA}-jetstream" '{.status.ready}' true 180
wait_jsonpath "$CLAIM_RES" "$ACCT_NS" "${ACCTB}-jetstream" '{.status.ready}' true 180

# The declaring claims do not go Ready until NACK reports their Stream/
# Consumer CRs live (the restored AwaitingStreamCreation gate — see
# REASON_AWAITING_STREAM_CREATION in reconcile.rs). Wait for that
# explicitly rather than sleeping a fixed 45s and hoping: a fixed sleep
# that is too short reads as a product failure, and one that is too long
# is 45s added to every green run.
for _c in "$CLAIM3" "$CLAIM4" "$CLAIM5"; do
    wait_jsonpath "$CLAIM_RES" "$APP_NS" "$_c" '{.status.ready}' true 240 || true
done
assert_nack_healthy "after the declaring claims (streams + consumers) were applied"

phase "Part 3 + 2.5f + 2.5 part-4 + egress + prefix-pre-capture + memory-budget acceptance criteria — 17 checks, run independently (see this file's own note above)"

PART3_FAILED=0
record_part3() {
    local num="$1" desc="$2" status="$3"
    if [ "$status" = "0" ]; then
        printf '  part3[%s] PASS: %s\n' "$num" "$desc"
    else
        printf '  part3[%s] FAIL: %s\n' "$num" "$desc" >&2
        PART3_FAILED=$((PART3_FAILED + 1))
    fi
}

# --- #1: streamapp reaches Ready=True (replaces Part 2's "parks" assertion) ---
part3_check_1() {
    local ready reason
    ready=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM3" '{.status.ready}')
    reason=$(cond_reason "$CLAIM_RES" "$APP_NS" "$CLAIM3" Ready)
    printf '    streamapp: status.ready=%q Ready-condition-reason=%q\n' "$ready" "$reason"
    [ "$ready" = "true" ]
}
if part3_check_1; then record_part3 1 "streamapp (declared stream) reaches Ready=True" 0
else record_part3 1 "streamapp (declared stream) reaches Ready=True" 1; fi

# --- #2: the declared stream exists, with declared subjects/retention/maxBytes, queried as mgr_<ns> ---
part3_check_2() {
    local out
    out=$(mgr_nats_run "$APP_NS" stream info streamapp_orders --json 2>&1) || {
        printf '    querying stream "streamapp_orders" as mgr_demo failed: %s\n' "$out"
        return 1
    }
    printf '    stream info: %s\n' "$out"
    printf '%s' "$out" | jq -e '
        (.config.subjects == ["streamapp.orders.>"]) and
        (.config.retention == "limits") and
        (.config.max_bytes == 268435456)
    ' >/dev/null
}
if part3_check_2; then record_part3 2 "the declared stream exists with its declared subjects/retention/maxBytes" 0
else record_part3 2 "the declared stream (streamapp_orders) exists with its declared subjects/retention/maxBytes, queried as mgr_demo" 1; fi

# --- #3: the declared durable exists, named <consumer app>_<durable> ---
part3_check_3() {
    local out
    out=$(mgr_nats_run "$APP_NS" consumer info streamapp_orders consumerapp_reader --json 2>&1) || {
        printf '    querying consumer "consumerapp_reader" on stream "streamapp_orders" as mgr_demo failed: %s\n' "$out"
        return 1
    }
    printf '    consumer info: %s\n' "$out"
    return 0
}
if part3_check_3; then record_part3 3 "the declared durable exists, named consumerapp_reader" 0
else record_part3 3 "the declared durable (consumerapp_reader on streamapp_orders) exists, named <consumer app>_<durable>" 1; fi

# --- #4: a cross-application consume works end to end ---
part3_check_4() {
    local s_user s_pass s_inbox c_user c_pass c_inbox pub_out consumed
    s_user=$(secret_val "$APP_NS" "${CLAIM3}-conn" user)
    s_pass=$(secret_val "$APP_NS" "${CLAIM3}-conn" pass)
    s_inbox=$(secret_val "$APP_NS" "${CLAIM3}-conn" inboxPrefix)
    c_user=$(secret_val "$APP_NS" "${CLAIM4}-conn" user)
    c_pass=$(secret_val "$APP_NS" "${CLAIM4}-conn" pass)
    c_inbox=$(secret_val "$APP_NS" "${CLAIM4}-conn" inboxPrefix)
    if [ -z "$s_user" ] || [ -z "$c_user" ]; then
        printf '    connection Secret(s) for streamapp/consumerapp missing or empty\n'
        return 1
    fi
    pub_out=$(nats_run --server "$CONN1_SERVER" --user "$s_user" --password "$s_pass" --inbox-prefix "$s_inbox" \
        pub streamapp.orders.demo "part3-cross-consume-probe" 2>&1)
    printf '    producer (streamapp) publish: %s\n' "$pub_out"
    consumed=$(nats_run --server "$CONN1_SERVER" --user "$c_user" --password "$c_pass" --inbox-prefix "$c_inbox" \
        consumer next streamapp_orders consumerapp_reader --count 1 --raw 2>&1) || {
        printf '    consumer (consumerapp) pull via its declared durable failed: %s\n' "$consumed"
        return 1
    }
    printf '    consumed via the declared durable: %s\n' "$consumed"
    [ "$consumed" = "part3-cross-consume-probe" ]
}
if part3_check_4; then record_part3 4 "a cross-application consume works end to end (publish -> declared durable -> receive)" 0
else record_part3 4 "a cross-application consume works end to end: streamapp publishes, consumerapp receives through its declared durable (consumerapp_reader)" 1; fi

# --- #5: the producer cannot delete the consumer's durable (deny class D) ---
part3_check_5() {
    local s_user s_pass s_inbox del_out del_rc t0 t1 ms
    s_user=$(secret_val "$APP_NS" "${CLAIM3}-conn" user)
    s_pass=$(secret_val "$APP_NS" "${CLAIM3}-conn" pass)
    s_inbox=$(secret_val "$APP_NS" "${CLAIM3}-conn" inboxPrefix)
    if [ -z "$s_user" ]; then
        printf '    streamapp connection Secret missing/empty\n'
        return 1
    fi
    t0=$(date +%s%N)
    del_out=$(nats_run --server "$CONN1_SERVER" --user "$s_user" --password "$s_pass" --inbox-prefix "$s_inbox" \
        consumer rm streamapp_orders consumerapp_reader -f 2>&1)
    del_rc=$?
    t1=$(date +%s%N)
    ms=$(( (t1 - t0) / 1000000 ))
    printf '    producer (streamapp) delete attempt on the durable (%d ms, exit %d): %s\n' \
        "$ms" "$del_rc" "${del_out:-<no output>}"
    # Class D is a PUBLISH-permission deny on a request/reply-shaped
    # JetStream API call — per this project's own established finding
    # (nats_client.rs), that delivers NO error signal (a timeout, not a
    # clean "permission denied"), the same shape Phase 6's missing-
    # inbox-prefix negative already exercises. So the delete attempt's
    # own exit code/stdout is not reliable evidence either way — what
    # actually proves the deny is that the TARGET SURVIVES, queried as
    # mgr_demo (the identity that genuinely has delete rights).
    mgr_nats_run "$APP_NS" consumer info streamapp_orders consumerapp_reader --json >/dev/null 2>&1
}
if part3_check_5; then record_part3 5 "streamapp (the producer) cannot delete consumerapp's durable — it still exists afterward" 0
else record_part3 5 "the producer cannot delete the consumer's durable (deny class D), observed by the durable surviving a delete attempt" 1; fi

# --- #6: deleting an application removes its declared streams, leaves a neighbour's intact ---
part3_check_6() {
    if ! mgr_nats_run "$APP_NS" stream info streamapp_orders --json >/dev/null 2>&1; then
        printf '    precondition failed: streamapp_orders does not exist yet (see check 2)\n'
        return 1
    fi
    if ! mgr_nats_run "$APP_NS" stream info streamapp2_invoices --json >/dev/null 2>&1; then
        printf '    precondition failed: streamapp2_invoices (the neighbour) does not exist yet\n'
        return 1
    fi
    # Delete the application, THEN force its grace — a declared stream
    # holds DATA and goes the same way every other backend's data goes
    # here: at the seven-day `RetainedClaim` GC, not on delete.
    #
    # This check asserted the immediate form until the 2.5e walk measured
    # what actually happens. NACK's LEGACY controller registers no
    # finalizer, and its delete branch is gated on the CR having a
    # deletionTimestamp — so deleting a `Stream` CR removes it from etcd
    # before NACK ever observes the deletion, and the NATS stream is left
    # behind. `streamapp_orders gone=0` while the CR was already gone.
    # Deleting the CR is not a way to delete a stream.
    #
    # `gc_drop_nats` is, and ADR 0061 §8 is where it belongs ("GC, after
    # the seven-day grace, as `mgr_<ns>`"). Forcing the grace is the same
    # established technique criterion #7 below and needs-pg-walk.sh's
    # Phase 9 use — not a shortened grace, the audited mechanism for
    # reaching the GC inside a walk.
    kubectl delete "$APP_RES" "$APP3" -n "$APP_NS" --wait=true --timeout=120s
    force_grace_retainedclaim "${APP3}-jetstream" "$APP_NS" || true
    sleep 20
    local own_gone=0 neighbour_ok=0
    mgr_nats_run "$APP_NS" stream info streamapp_orders --json >/dev/null 2>&1 || own_gone=1
    mgr_nats_run "$APP_NS" stream info streamapp2_invoices --json >/dev/null 2>&1 && neighbour_ok=1
    printf '    after streamapp'"'"'s grace GC: streamapp_orders gone=%s, streamapp2_invoices (neighbour) still present=%s\n' \
        "$own_gone" "$neighbour_ok"
    [ "$own_gone" = "1" ] && [ "$neighbour_ok" = "1" ]
}
if part3_check_6; then record_part3 6 "streamapp's grace GC removed streamapp_orders and left streamapp2_invoices intact" 0
else record_part3 6 "a departed application's declared streams are reclaimed by its grace GC, and a neighbour's are left intact" 1; fi

# --- #7: the account survives while a neighbour remains, goes when the last claim does ---
# Seeds ONE dynamic stream per jslife app, each with subjects wholly
# under its OWN app prefix. Both apps are `dynamicStreams: true`, so each
# may create its own — and `gc_drop_nats`'s sweep keys on SUBJECTS, never
# on names (ADR 0061 §8), so the names here are deliberately opaque:
# nothing about "acctadyn" says who owns it, and the GC must work that out
# from `accta.events.>` alone.
#
# This is the ONLY live exercise of the GC's stream arm — the async-nats
# STREAM.LIST/STREAM.DELETE calls it makes as `mgr_<ns>` against a real
# server. Checks 2-6 cover NACK-created DECLARED streams, which take a
# completely different path (NACK deletes those when the provisioner
# deletes their CRs).
seed_jslife_dynamic_streams() {
    local app claim user pass inbox out
    for app in "$ACCTA" "$ACCTB"; do
        claim="${app}-jetstream"
        user=$(secret_val "$ACCT_NS" "${claim}-conn" user)
        pass=$(secret_val "$ACCT_NS" "${claim}-conn" pass)
        inbox=$(secret_val "$ACCT_NS" "${claim}-conn" inboxPrefix)
        if [ -z "$user" ]; then
            printf '    connection Secret for %s missing/empty\n' "$claim"
            return 1
        fi
        write_pod_file "/tmp/${app}dyn.json" <<JSON
{"name":"${app}dyn","subjects":["${app}.events.>"],"storage":"file","retention":"limits","max_consumers":-1,"max_msgs":-1,"max_bytes":-1,"max_age":0,"max_msgs_per_subject":-1,"max_msg_size":-1,"discard":"old","num_replicas":1,"duplicate_window":120000000000}
JSON
        out=$(nats_run --server "$CONN1_SERVER" --user "$user" --password "$pass" \
            --inbox-prefix "$inbox" stream add "${app}dyn" --config "/tmp/${app}dyn.json" 2>&1) || {
            printf '    could not create the %s dynamic stream: %s\n' "$app" "$out"
            return 1
        }
    done
    printf '    seeded dynamic streams acctadyn (accta.events.>) and acctbdyn (acctb.events.>)\n'
}

part3_check_7() {
    local accounts_now survives=0 gone_now=0 own_swept=0 neighbour_kept=0 last_swept=0
    accounts_now=$(secret_val "$NATS_NS" "$ACCOUNTS_SECRET" 'accounts\.conf')
    if [[ "$accounts_now" != *"ns_${ACCT_NS}:"* ]]; then
        printf '    precondition failed: ns_%s does not appear in nats-accounts yet\n' "$ACCT_NS"
        return 1
    fi
    seed_jslife_dynamic_streams || return 1

    kubectl delete "$APP_RES" "$ACCTA" -n "$ACCT_NS" --wait=true --timeout=120s
    force_grace_retainedclaim "${ACCTA}-jetstream" "$ACCT_NS" || true
    sleep 20
    accounts_now=$(secret_val "$NATS_NS" "$ACCOUNTS_SECRET" 'accounts\.conf')
    [[ "$accounts_now" == *"ns_${ACCT_NS}:"* ]] && survives=1
    # The sweep: accta's own dynamic stream goes, the neighbour's stays.
    mgr_nats_run "$ACCT_NS" stream info "${ACCTA}dyn" --json >/dev/null 2>&1 || own_swept=1
    mgr_nats_run "$ACCT_NS" stream info "${ACCTB}dyn" --json >/dev/null 2>&1 && neighbour_kept=1
    printf '    after deleting %s (neighbour %s remains): ns_%s still present=%s, %sdyn swept=%s, %sdyn kept=%s\n' \
        "$ACCTA" "$ACCTB" "$ACCT_NS" "$survives" "$ACCTA" "$own_swept" "$ACCTB" "$neighbour_kept"

    kubectl delete "$APP_RES" "$ACCTB" -n "$ACCT_NS" --wait=true --timeout=120s
    force_grace_retainedclaim "${ACCTB}-jetstream" "$ACCT_NS" || true
    sleep 20
    accounts_now=$(secret_val "$NATS_NS" "$ACCOUNTS_SECRET" 'accounts\.conf')
    [[ "$accounts_now" != *"ns_${ACCT_NS}:"* ]] && gone_now=1
    # "The account goes with its store" (ADR 0061 §8). Once the account is
    # out of the file nothing can query it any more — not even mgr_jslife,
    # whose own user went with it — so the only remaining evidence that the
    # last claim took the store too is the GC saying so. Asserted on the
    # operator's own log line, not inferred from the account's absence.
    # Captured into a variable, NOT `operator_log | grep -q`: this file
    # runs under `set -o pipefail`, and `grep -q` exits on its first match
    # — which SIGPIPEs the upstream `kubectl logs`, making the whole
    # pipeline exit non-zero on a SUCCESSFUL match. Same shape the
    # "no forbidden in the operator log" phase at the end of this file
    # already uses for the same reason.
    local gc_log
    gc_log="$(operator_log)"
    [[ "$gc_log" == *"nats GC: stream deleted"*"${ACCTB}dyn"* ]] && last_swept=1
    printf '    after deleting %s too (the LAST claim in %s): ns_%s gone=%s, %sdyn swept with the account=%s\n' \
        "$ACCTB" "$ACCT_NS" "$ACCT_NS" "$gone_now" "$ACCTB" "$last_swept"

    [ "$survives" = "1" ] && [ "$gone_now" = "1" ] && \
        [ "$own_swept" = "1" ] && [ "$neighbour_kept" = "1" ] && [ "$last_swept" = "1" ]
}
if part3_check_7; then record_part3 7 "the ns_jslife account survived one deletion and was removed once the last claim was, and the GC swept exactly the departing app's streams" 0
else record_part3 7 "the account survives while a neighbour remains and goes when the last claim does, and the GC sweeps the departing application's dynamic streams by SUBJECT while leaving the neighbour's" 1; fi

# --- #8: a second application arriving does NOT clear the first's streams ---
part3_check_8() {
    if ! mgr_nats_run "$APP_NS" stream info streamapp2_invoices --json >/dev/null 2>&1; then
        printf '    precondition failed: streamapp2_invoices does not exist yet (see check 2/6)\n'
        return 1
    fi
    local s2_user s2_pass s2_inbox pub_out info msgs
    s2_user=$(secret_val "$APP_NS" "${CLAIM5}-conn" user)
    s2_pass=$(secret_val "$APP_NS" "${CLAIM5}-conn" pass)
    s2_inbox=$(secret_val "$APP_NS" "${CLAIM5}-conn" inboxPrefix)
    if [ -z "$s2_user" ]; then
        printf '    streamapp2 connection Secret missing/empty\n'
        return 1
    fi
    pub_out=$(nats_run --server "$CONN1_SERVER" --user "$s2_user" --password "$s2_pass" --inbox-prefix "$s2_inbox" \
        pub streamapp2.invoices.probe "part3-clear-on-allocation-probe" 2>&1)
    printf '    seeded streamapp2_invoices with a probe message: %s\n' "$pub_out"

    kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP6}
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
    wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM6" '{.status.ready}' true 120 || true

    info=$(mgr_nats_run "$APP_NS" stream info streamapp2_invoices --json 2>&1) || {
        printf '    streamapp2_invoices vanished after the new arrival: %s\n' "$info"
        return 1
    }
    msgs=$(printf '%s' "$info" | jq -r '.state.messages')
    printf '    streamapp2_invoices message count after the new arrival (%s): %s\n' "$APP6" "$msgs"
    [ "${msgs:-0}" -ge 1 ]
}
if part3_check_8; then record_part3 8 "latecomerapp's arrival left streamapp2_invoices's existing message(s) intact" 0
else record_part3 8 "a second application arriving does NOT clear the first's streams (ADR 0042 §9.6's clear-on-allocation does NOT transfer verbatim — ADR 0061 §8's own correction)" 1; fi

# ===============================================================
# 2.5f acceptance criteria (#9-#13) — the observed-stream inventory
# (ADR 0061 §9), the foreign-subject capture detector (§5) and the
# conditions (§5/§6/§7).
#
# These run AFTER #1-#8 on purpose. #6 deletes streamapp and #7 tears
# down the whole jslife namespace, so everything below is written against
# the fixtures that SURVIVE to the end — walkapp (dynamicStreams:true),
# consumerapp (whose consume target #6 just removed), streamapp2 (the
# declared workqueue owner) and latecomerapp.
#
# Same not-fail-fast contract as #1-#8: each runs regardless of the
# others' outcome and accumulates into PART3_FAILED.
# ===============================================================

# --- #9: the status inventory exists and classifies what it observed ---
part3_check_9() {
    local deadline dyn decl obs_at bytes
    deadline=$(( $(date +%s) + 180 ))
    # The inventory refreshes on the 60s resync gate for a ready claim
    # (a ready claim never provisions again), so poll rather than sleep.
    while [ "$(date +%s)" -lt "$deadline" ]; do
        dyn=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM1" '{.status.streams.dynamic}')
        decl=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM5" '{.status.streams.declared}')
        if [[ "$dyn" == *walkstream* ]] && [[ "$decl" == *streamapp2_invoices* ]]; then
            break
        fi
        sleep 5
    done
    obs_at=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM1" '{.status.streams.observedAt}')
    bytes=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM1" '{.status.size.bytes}')
    printf '    walkapp   status.streams.dynamic  = %s\n' "${dyn:-<unset>}"
    printf '    walkapp   status.streams.observedAt = %q\n' "${obs_at:-}"
    printf '    walkapp   status.size.bytes       = %q\n' "${bytes:-}"
    printf '    streamapp2 status.streams.declared = %s\n' "${decl:-<unset>}"
    # walkstream was created BY walkapp's own user, under its own prefix,
    # and declared by nobody — the definition of `dynamic`. Its bytes are
    # what `apprafter app status` renders through the generic size path.
    [[ "$dyn" == *walkstream* ]] || return 1
    [[ "$decl" == *streamapp2_invoices* ]] || return 1
    [ -n "$obs_at" ] || return 1
    [ -n "$bytes" ] && [ "$bytes" -ge 1 ]
}
if part3_check_9; then record_part3 9 "status.streams classifies observed streams (declared/dynamic) and status.size.bytes is real" 0
else record_part3 9 "the status inventory (ADR 0061 §9) lists walkstream as walkapp's DYNAMIC stream, streamapp2_invoices as streamapp2's DECLARED one, and carries a live observedAt + size" 1; fi

# --- #10: a foreign-subject capture is detected on the VICTIM's claim ---
# walkapp holds `dynamicStreams: true`, so it may create a stream — and
# NATS does not permission-check a stream's SUBJECTS at creation (subjects
# travel in the request body, which the server never inspects while
# evaluating permissions; ADR 0061's opening constraint). That is the
# whole reason detection has to exist: this capture cannot be prevented,
# only observed.
#
# streamapp2 does NOT hold the flag, so it provably could not have created
# a stream under its own prefix — which is what makes the detection EXACT
# here rather than a guess (ADR 0061 §3).
#
# Subjects are `streamapp2.evil.>`, deliberately NOT under
# `streamapp2.invoices.>`: nats-server refuses to create a stream whose
# subjects overlap an existing WORKQUEUE stream's, and this check is about
# the detector, not about that refusal.
part3_check_10() {
    local out deadline victim_status victim_msg innocent ev
    write_pod_file /tmp/capture1.json <<'JSON'
{"name":"capture1","subjects":["streamapp2.evil.>"],"storage":"file","retention":"limits","max_consumers":-1,"max_msgs":-1,"max_bytes":-1,"max_age":0,"max_msgs_per_subject":-1,"max_msg_size":-1,"discard":"old","num_replicas":1,"duplicate_window":120000000000}
JSON
    out=$(nats_run --server "$CONN1_SERVER" --user "$CONN1_USER" --password "$CONN1_PASS" \
        --inbox-prefix "$CONN1_INBOX_PREFIX" stream add capture1 --config /tmp/capture1.json 2>&1) || {
        printf '    walkapp could not create the capture stream: %s\n' "$out"
        return 1
    }
    printf '    walkapp (dynamicStreams:true) created a stream over streamapp2 subjects: %s\n' \
        "$(printf '%s' "$out" | tail -1)"

    deadline=$(( $(date +%s) + 180 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        victim_status=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM5" ForeignSubjectCapture)
        [ "$victim_status" = "True" ] && break
        sleep 5
    done
    victim_msg=$(cond_message "$CLAIM_RES" "$APP_NS" "$CLAIM5" ForeignSubjectCapture)
    innocent=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM1" ForeignSubjectCapture)
    ev=$(kubectl -n "$APP_NS" get events -o json 2>/dev/null \
        | jq -r '[.items[] | select(.reason=="ForeignSubjectCapture") | .involvedObject.name] | join(",")')
    printf '    streamapp2 (victim)  ForeignSubjectCapture = %q\n' "${victim_status:-<unset>}"
    printf '    streamapp2 message: %s\n' "${victim_msg:-<none>}"
    printf '    walkapp    (its own dynamic stream is legitimate) ForeignSubjectCapture = %q\n' "${innocent:-<unset>}"
    printf '    ForeignSubjectCapture events on: %s\n' "${ev:-<none>}"
    # The capture stream must STILL EXIST: the shipped policy is `report`,
    # and a `report` that quietly deleted would be the worst of both.
    mgr_nats_run "$APP_NS" stream info capture1 --json >/dev/null 2>&1 || {
        printf '    capture1 is GONE — capturePolicy=report must not delete\n'
        return 1
    }
    [ "$victim_status" = "True" ] || return 1
    [[ "$victim_msg" == *capture1* ]] || return 1
    [ -z "$innocent" ] || return 1
    [[ "$ev" == *"$CLAIM5"* ]]
}
if part3_check_10; then record_part3 10 "a foreign-subject capture raises ForeignSubjectCapture + an Event on the VICTIM's claim, and does not fire on the innocent neighbour" 0
else record_part3 10 "a stream created under streamapp2's prefix by an application holding dynamicStreams is detected on streamapp2's claim (condition + Event), is left in place under capturePolicy=report, and does NOT flag walkapp's own legitimate dynamic stream" 1; fi

# --- #11: NamespaceDrainRisk fires on the workqueue owner, and the two
#          conditions that must NOT fire here do not ---
part3_check_11() {
    local deadline drain drain_msg bystander overlap
    deadline=$(( $(date +%s) + 180 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        drain=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM5" NamespaceDrainRisk)
        [ "$drain" = "True" ] && break
        sleep 5
    done
    drain_msg=$(cond_message "$CLAIM_RES" "$APP_NS" "$CLAIM5" NamespaceDrainRisk)
    # consumerapp owns no workqueue and holds no dynamicStreams — it is
    # neither the party at risk nor the risk, so the narrowed rule must
    # leave it alone. Without this half, a rule that fired on every claim
    # in the namespace would look identical to a correct one.
    bystander=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM4" NamespaceDrainRisk)
    # Nothing in this account carries subjects overlapping
    # streamapp2.invoices.> (capture1 is streamapp2.evil.>), so the
    # overlap condition must stay silent even though a workqueue exists.
    overlap=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM5" WorkqueueSubjectOverlap)
    printf '    streamapp2 (workqueue owner)     NamespaceDrainRisk = %q\n' "${drain:-<unset>}"
    printf '    streamapp2 message: %s\n' "${drain_msg:-<none>}"
    printf '    consumerapp (neither party)      NamespaceDrainRisk = %q\n' "${bystander:-<unset>}"
    printf '    streamapp2 (no overlapping subjects) WorkqueueSubjectOverlap = %q\n' "${overlap:-<unset>}"
    [ "$drain" = "True" ] || return 1
    [[ "$drain_msg" == *streamapp2_invoices* ]] || return 1
    [ -z "$bystander" ] || return 1
    [ -z "$overlap" ]
}
if part3_check_11; then record_part3 11 "NamespaceDrainRisk fires on the workqueue owner while a dynamicStreams neighbour exists, and fires on nobody else" 0
else record_part3 11 "NamespaceDrainRisk names streamapp2_invoices on its owner's claim (a dynamicStreams neighbour can drain a workqueue via sources — ADR 0061 §4.1), leaves the uninvolved consumerapp alone, and WorkqueueSubjectOverlap stays silent with no overlapping stream" 1; fi

# --- #12: ConsumeTargetMissing, once the owning application has gone ---
# Criterion #6 deleted streamapp and its grace GC removed streamapp_orders.
# consumerapp still declares a durable on it, so its consume entry now
# names a stream nobody declares and that does not exist.
part3_check_12() {
    local deadline missing msg
    deadline=$(( $(date +%s) + 180 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        missing=$(cond_status "$CLAIM_RES" "$APP_NS" "$CLAIM4" ConsumeTargetMissing)
        [ "$missing" = "True" ] && break
        sleep 5
    done
    msg=$(cond_message "$CLAIM_RES" "$APP_NS" "$CLAIM4" ConsumeTargetMissing)
    printf '    consumerapp ConsumeTargetMissing = %q\n' "${missing:-<unset>}"
    printf '    consumerapp message: %s\n' "${msg:-<none>}"
    [ "$missing" = "True" ] || return 1
    [[ "$msg" == *streamapp_orders* ]]
}
if part3_check_12; then record_part3 12 "ConsumeTargetMissing fires once the consumed stream's owning application has departed" 0
else record_part3 12 "consumerapp's durable names streamapp_orders, which criterion #6 removed with its owner — the claim must say so rather than sitting silently on a consumer that can never be created" 1; fi

# --- #13: QuotaExceeded is a PRE-FLIGHT, not an opaque server error ---
# Every claim in `demo` is size `small` (256Mi), so the account's max_file
# is a fraction of a gigabyte. A declaration of 8Gi cannot fit, and the
# point of the pre-flight is WHERE that is reported: on the claim, naming
# the stream and the numbers — rather than as nats-server's `insufficient
# storage resources available (10047)` surfacing through NACK, which names
# neither.
part3_check_13() {
    local reason msg stream_cr nats_stream
    kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP7}
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
          - name: huge
            subjects: ["hogapp.huge.>"]
            maxBytes: "8Gi"
YAML
    wait_jsonpath "$CLAIM_RES" "$APP_NS" "$CLAIM7" '{.spec.type}' jetstream 120 || return 1
    local deadline
    deadline=$(( $(date +%s) + 240 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        reason=$(cond_reason "$CLAIM_RES" "$APP_NS" "$CLAIM7" Ready)
        [ "$reason" = "QuotaExceeded" ] && break
        sleep 5
    done
    msg=$(cond_message "$CLAIM_RES" "$APP_NS" "$CLAIM7" Ready)
    # The Stream CR must never have been applied, and the stream must never
    # have been asked for — that is what "pre-flight" means here.
    stream_cr=$(kubectl -n "$NATS_NS" get stream "${APP_NS}-${APP7}-huge" -o name 2>/dev/null || true)
    nats_stream=1
    mgr_nats_run "$APP_NS" stream info hogapp_huge --json >/dev/null 2>&1 || nats_stream=0
    printf '    hogapp Ready reason = %q\n' "${reason:-<unset>}"
    printf '    hogapp Ready message: %s\n' "${msg:-<none>}"
    printf '    Stream CR present=%q ; stream exists in NATS=%s (both must be empty/0)\n' \
        "${stream_cr:-}" "$nats_stream"
    [ "$reason" = "QuotaExceeded" ] || return 1
    [ -z "$stream_cr" ] || return 1
    [ "$nats_stream" = "0" ]
}
if part3_check_13; then record_part3 13 "an over-budget declaration is refused by the pre-flight with QuotaExceeded, and no Stream CR or NATS stream is ever created" 0
else record_part3 13 "a declared stream larger than the namespace account can hold surfaces QuotaExceeded on the claim (ADR 0061 §6) instead of nats-server's opaque 10047 through NACK, and the Stream CR is never applied" 1; fi

# --- #14: turning `dynamicStreams` on PAUSES the application behind a
#          MigrationPlan (ADR 0052 trigger #15 / ADR 0061 §7) ---
#
# The one jetstream gating trigger a live cluster can show end to end in a
# single field edit, and the only one of the three whose whole chain —
# classifier → MigrationPlan CR in the app namespace → paused Application →
# children NOT applied — crosses three controllers and a CRD. A unit test
# proves the classification; only this proves the edit actually stops.
#
# Its OWN namespace, and the baseline app declares NO needs at all. Two
# reasons, both deliberate:
#   * `demo`'s account budget is the SUM over its claims (see #13's own
#     note) — a sixth claim there would move the number criterion #13
#     measures against.
#   * the baseline must be STAMPED (`status.lastAppliedSpec`) before the
#     escalating edit lands, or the classifier has nothing to diff and the
#     edit sails through un-gated. A needs-free app renders immediately, so
#     the stamp does not wait on NATS provisioning at all — and the
#     `(absent) → true` transition is the same trigger-#15 arm as
#     `false → true` (absent IS the effective false).
#
# The revert half is the NEGATIVE, and it is the one that would catch a
# detector that fired on PRESENCE rather than on the delta: taking the flag
# back off is a NARROWING, so the plan must be cleaned up and the app must
# resume — not pause a second time.
GATE_NS="jsgate"
APP8="gateapp"
gate_app_yaml() {
    # $1 = the needs block ("" for none) — everything else is identical, so
    # the diff the classifier sees is exactly the one field.
    cat <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP8}
  namespace: ${GATE_NS}
  labels:
    apprafter.io/managed-by: apprafter
spec:
  base:
    image: nginxdemos/hello:plain-text
    replicas: 1
    expose:
      port: 80
${1}
YAML
}
part3_check_14() {
    local plan phase_got trigger from to classification hash claims
    kubectl create namespace "$GATE_NS" 2>/dev/null || true
    gate_app_yaml "" | kubectl apply -f - >/dev/null || return 1
    # The stamped baseline is the precondition for ANY detection.
    # `status.lastAppliedSpec` is the WHOLE `ApplicationSpec` (it stamps
    # `app.spec`, base + environments), not the effective base — so the
    # image lives under `.base.image`. The classifier runs
    # `effective_baseline` over it to get the per-environment effective
    # spec it diffs against.
    wait_jsonpath "$APP_RES" "$GATE_NS" "$APP8" \
        '{.status.lastAppliedSpec.base.image}' 'nginxdemos/hello:plain-text' 180 || return 1

    # The escalation: one field.
    gate_app_yaml "    needs:
      jetstream:
        selector:
          tier: integrated
        dynamicStreams: true" | kubectl apply -f - >/dev/null || return 1

    local deadline
    deadline=$(( $(date +%s) + 180 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        plan=$(kubectl -n "$GATE_NS" get migrationplan.apprafter.io \
            -l "apprafter.io/application=${APP8}" -o name 2>/dev/null | head -n 1 || true)
        [ -n "$plan" ] && break
        sleep 5
    done
    if [ -z "${plan:-}" ]; then
        printf '    no MigrationPlan appeared in %s for %s within 180s\n' "$GATE_NS" "$APP8"
        kubectl -n "$GATE_NS" get "$APP_RES" "$APP8" -o yaml 2>&1 | tail -40
        return 1
    fi
    plan="${plan#migrationplan.apprafter.io/}"
    trigger=$(jp migrationplan.apprafter.io "$GATE_NS" "$plan" '{.spec.trigger.type}')
    from=$(jp migrationplan.apprafter.io "$GATE_NS" "$plan" '{.spec.trigger.from}')
    to=$(jp migrationplan.apprafter.io "$GATE_NS" "$plan" '{.spec.trigger.to}')
    classification=$(jp migrationplan.apprafter.io "$GATE_NS" "$plan" '{.spec.risks.classification}')
    hash=$(jp migrationplan.apprafter.io "$GATE_NS" "$plan" '{.spec.trigger.approvedSpecHash}')
    printf '    plan %s: trigger=%q %q→%q classification=%q hash=%.12s…\n' \
        "$plan" "$trigger" "$from" "$to" "$classification" "${hash:-<unset>}"
    assert_eq "MigrationPlan trigger type" "$trigger" "jetstream-dynamic-streams-enable" || return 1
    assert_eq "MigrationPlan trigger from" "$from" "false" || return 1
    assert_eq "MigrationPlan trigger to" "$to" "true" || return 1
    assert_eq "MigrationPlan classification" "$classification" "security-boundary" || return 1
    [ -n "$hash" ] || { printf '    approvedSpecHash is empty — a hashless plan can never consume (ADR 0052 §4)\n'; return 1; }

    # Paused, and — the part that matters — the escalation has NOT taken
    # effect: no jetstream claim was generated while approval is pending.
    wait_jsonpath "$APP_RES" "$GATE_NS" "$APP8" '{.status.phase}' \
        'AwaitingMigrationApproval' 120 || return 1
    claims=$(kubectl -n "$GATE_NS" get "$CLAIM_RES" -o name 2>/dev/null || true)
    printf '    ResourceClaims in %s while the plan is pending: %s\n' "$GATE_NS" "${claims:-<none>}"
    [ -z "$claims" ] || return 1

    # Revert — a narrowing. The plan is superseded and the app resumes.
    gate_app_yaml "" | kubectl apply -f - >/dev/null || return 1
    wait_gone migrationplan.apprafter.io "$GATE_NS" "$plan" 180 || return 1
    deadline=$(( $(date +%s) + 120 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        phase_got=$(jp "$APP_RES" "$GATE_NS" "$APP8" '{.status.phase}')
        [ "$phase_got" != "AwaitingMigrationApproval" ] && break
        sleep 5
    done
    printf '    after the revert: %s/%s phase=%q (must not be AwaitingMigrationApproval)\n' \
        "$GATE_NS" "$APP8" "${phase_got:-<unset>}"
    [ "$phase_got" != "AwaitingMigrationApproval" ]
}
if part3_check_14; then record_part3 14 "enabling needs.jetstream.dynamicStreams pauses the application behind a security-boundary MigrationPlan, generates no claim while it waits, and reverting clears the gate" 0
else record_part3 14 "flipping needs.jetstream.dynamicStreams to true creates a MigrationPlan with trigger jetstream-dynamic-streams-enable (ADR 0052 #15), pauses the Application before any child is applied, and taking the flag back off supersedes the plan instead of re-gating" 1; fi

# --- #15: the egress CNP's NATS rule selects the RUNNING nats-0 ---
#
# The defect this exists for: `default_target` (operator-rendering's egress
# builder) had arms for pg and redis and a catch-all `_ => None`, so
# `needs.jetstream` produced NO egress rule — while the CNP still selected
# the app's pods, which is what makes them egress default-deny. The app was
# handed working NATS credentials for a server its own pod could not open a
# socket to.
#
# Nothing in this walk could see it. It runs without a cilium-agent, so no
# policy is ever enforced; and every byte of NATS traffic the walk sends
# comes from the nats-box debug pod, which is not an Application and carries
# no CNP of its own. Two independent reasons — which is why this check is
# built on neither. It reads the rule the operator APPLIED and asks the
# apiserver, with the rule's own labels, whether they select the running
# server. That catches "no rule at all" and "a rule whose selector matches
# nothing", including the version-bound-label trap (selecting on
# `app.kubernetes.io/version` or `helm.sh/chart` would pass the day it is
# written and silently stop matching at the next chart bump).
#
# NOT proven here, deliberately and by construction: that Cilium then
# FORWARDS the packet. See the stand-in-CRD comment in Phase 1b.
part3_check_15() {
    local cnp rule port sel matched
    cnp=$(kubectl -n "$APP_NS" get ciliumnetworkpolicy "${APP1}-egress" -o json 2>&1) || {
        printf '    no CiliumNetworkPolicy %s/%s-egress — the operator applied none. If the log says "Cilium not detected", the stand-in CRD (Phase 1b) did not land before the operator acquired leadership: %s\n' \
            "$APP_NS" "$APP1" "$cnp"
        return 1
    }

    rule=$(printf '%s' "$cnp" | jq -c --arg ns "$NATS_NS" \
        '.spec.egress[] | select(.toEndpoints[0].matchLabels["io.kubernetes.pod.namespace"] == $ns)')
    if [ -z "$rule" ]; then
        printf '    the CNP carries NO egress rule for namespace %s. This is the original defect: the app is egress default-deny with no path to NATS. Rules present:\n%s\n' \
            "$NATS_NS" "$(printf '%s' "$cnp" | jq -c '.spec.egress')"
        return 1
    fi
    printf '    NATS egress rule as applied: %s\n' "$rule"

    port=$(printf '%s' "$rule" | jq -r '.toPorts[0].ports[0].port')
    assert_eq "NATS egress rule port" "$port" "4222" || return 1

    # Turn the rule's OWN matchLabels (minus the namespace pseudo-label,
    # which is Cilium's and not a pod label) into a label selector and hand
    # it back to the apiserver. Deriving it from the applied object rather
    # than repeating it here is the point: a selector typed into this script
    # would prove only that two copies of the same guess agree.
    sel=$(printf '%s' "$rule" | jq -r '
        .toEndpoints[0].matchLabels
        | to_entries
        | map(select(.key != "io.kubernetes.pod.namespace"))
        | map("\(.key)=\(.value)")
        | join(",")')
    if [ -z "$sel" ]; then
        printf '    the rule carries no pod labels at all — it would select every pod in %s\n' "$NATS_NS"
        return 1
    fi
    printf '    selector derived from the applied rule: %s\n' "$sel"

    matched=$(kubectl -n "$NATS_NS" get pods -l "$sel" -o jsonpath='{.items[*].metadata.name}' 2>&1) || {
        printf '    kubectl rejected the derived selector: %s\n' "$matched"
        return 1
    }
    printf '    pods in %s matching it: %s\n' "$NATS_NS" "${matched:-<none>}"
    printf '    live labels on %s-0: %s\n' "$NATS_STS" \
        "$(kubectl -n "$NATS_NS" get pod "${NATS_STS}-0" -o jsonpath='{.metadata.labels}' 2>/dev/null)"

    # Must select the running server itself, by name — "matched something"
    # is not enough when nats-box and the chart's test pod also live here
    # and share `app.kubernetes.io/name: nats`.
    case " $matched " in
        *" ${NATS_STS}-0 "*) return 0 ;;
        *)
            printf '    the rule does NOT select %s-0. The CNP renders, and drops every packet to NATS.\n' "$NATS_STS"
            return 1
            ;;
    esac
}
if part3_check_15; then record_part3 15 "the egress CNP carries a NATS rule on 4222 whose selector selects the running nats-0" 0
else record_part3 15 "the per-application egress CiliumNetworkPolicy carries a nats-system rule on port 4222 whose pod selector, taken from the applied object, matches the live nats-0 (enforcement itself is out of reach without a cilium-agent — see Phase 1b)" 1; fi

# --- #16: PrefixPreCaptured — an arriving application is told that its
#          prefix is already inside a neighbour's declared stream ---
#
# The last 2.5 signal with unit coverage only. `prefix_pre_captured`
# (`resourceclaim-provisioner/src/nats.rs`) is computed from DECLARATIONS,
# never from observed streams — it is the one rule here that does not need a
# stream to exist — but the pass that WRITES it does need a live account:
# `jetstream_signals_for` returns `None` (and writes nothing at all) when the
# `mgr_<ns>` Secret is unreadable or `STREAM.LIST` fails. So the condition is
# available on the arriving claim's first reconcile *through the provisioner*,
# and the cluster is what proves that hop — the unit tests exercise the rule
# on either side of it.
#
# It is a REPORT, not a fault: a neighbour's fan-in stream is the sanctioned
# exception to the hard `<app>.` publish prefix and is itself approval-gated
# (ADR 0052 trigger #16). So the claim must reach Ready ALONGSIDE the
# condition, and that is asserted rather than assumed — a regression that
# turned this report into a gate would otherwise leave the walk green.
#
# IN `demo`, NOT in a namespace of its own — and the reason is a MEASURED
# constraint of the shipped configuration, not a preference. Criterion #14
# takes its own namespace because its app declares no needs and therefore
# opens no account. This fixture opens one, and a fourth account does not fit:
#
#   * a namespace's account gets `max_mem = max(file_quota / 10, 64Mi)`
#     (`account_max_mem_bytes` + `ACCOUNT_MAX_MEM_FLOOR_BYTES`), and
#     `component_nats.cue` pins the server's `memoryStore.maxSize` at 192Mi,
#     sized there for "roughly three namespaces each sitting at the 64Mi
#     floor";
#   * by the time this criterion runs, `demo` holds six claims at `small`
#     (6 x 256Mi = 1.5Gi file => 153.6Mi of that 192Mi already reserved), so
#     ANY new account — even one at the bare 64Mi floor — is 217.6Mi and
#     overruns the server;
#   * measured directly (podman, nats-server 2.14.3, the pinned image): a
#     SIGHUP reload that overruns logs `Error enabling jetstream on
#     configured accounts: insufficient memory resources available (10028)`
#     and then reports `Reloaded server configuration` anyway. The server
#     does NOT die and the existing accounts keep working — but the new
#     account silently has no JetStream, its users still AUTHENTICATE, and
#     `$JS.API.INFO` simply never answers. The provisioner's verify step
#     then loops forever on `AwaitingNatsReady` ("the server may not have
#     reloaded the accounts file yet"), which is the one thing that did not
#     happen. The first draft of this criterion took its own namespace and
#     hit exactly that; the diagnosis is recorded in plan.md's 2.5 open list
#     because `component_nats.cue`'s own note predicts a cold-start FATAL and
#     the reload path is quieter and worse than that.
#
# So the three fixture applications join `demo`'s existing account and are
# sized `nano` (64Mi each) to keep it inside the ceiling: 6 x 256Mi + 3 x
# 64Mi = 1.69Gi file => 172.8Mi < 192Mi, verified against a real nats-server
# before this was written. Sharing the account costs nothing this criterion
# needs — `prefix_pre_captured` is a rule about ONE namespace's declarations —
# and it buys a harder test, because the rule now has to find `feeder` among
# nine peers instead of among three.
#
# Nothing already asserted moves: every criterion above has run and recorded
# by now, #13's quota verdict was taken against the six-claim budget it saw,
# and 8Gi still does not fit in 1.69Gi.
#
# The fixture, and why each subject is where it is:
#
#   feeder     streams: [{inbox, subjects: ["feeder.own.>", "arrival.>"]}]
#   arrival    needs.jetstream, declares nothing
#   bystander  needs.jetstream, declares nothing
#
#   * `arrival.>` is what fires the condition on `arrival`. (`arrival`
#     rather than the obvious `latecomer` because `demo` already holds a
#     `latecomerapp` from criterion #8 — two similar names in one account
#     would make this log hard to read, and `latecomerapp.` is a different
#     prefix from `latecomer.` in a way that is easy to misread and easy to
#     avoid.)
#   * `feeder.own.>` is NOT decoration. It makes feeder's own stream touch
#     feeder's own prefix, so feeder's negative actually exercises the
#     `p.app != me.app` self-filter. Without it there would be nothing for
#     that filter to remove and the negative would pass for the wrong reason.
#   * `bystander` shares the namespace and the account and is named by
#     nothing — not by feeder's declaration, not by `streamapp2.invoices.>`,
#     not by `hogapp.huge.>` — which separates "fires for the right prefix"
#     from "fires whenever any neighbour declares a stream".
#
# The admission webhook permits `arrival.>`: it rejects a bare `>` and the
# reserved roots (`$JS.`, `$SYS.`, `_INBOX`) and nothing else, because fan-in
# is a supported shape. And declaring a foreign subject AT CREATION is not
# approval-gated — the ADR 0052 triggers diff against `status.lastAppliedSpec`,
# which a brand-new Application does not have.
PRE_NS="$APP_NS"     # the SHARED demo account — see the note above for why
APP9="feeder"        # declares the fan-in stream
APP10="arrival"      # arrives into a prefix feeder already collects
APP11="bystander"    # arrives into a prefix nobody collects
feeder_yaml() {
    # $1 = the `inbox` stream's subject list, flow-style. The mutation below
    # rewrites exactly this and nothing else, so the only thing that can
    # explain the condition changing is the declaration changing.
    cat <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP9}
  namespace: ${PRE_NS}
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
        size: nano
        selector:
          tier: integrated
        streams:
          - name: inbox
            subjects: ${1}
            # Well inside what is left of this account's file budget
            # (1.69Gi, of which streamapp2_invoices holds 256Mi and
            # hogapp's 8Gi was refused outright by #13), so nothing here
            # can be confused with the quota pre-flight.
            maxBytes: "64Mi"
YAML
}
plain_js_app_yaml() {
    # $1 = app name. Declares the need and NOTHING else — no streams, no
    # consume, no dynamicStreams. This is the ARRIVING-application shape,
    # and it is what makes the fixture's point: the arriving side does
    # nothing wrong and nothing unusual, and is still told.
    cat <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${1}
  namespace: ${PRE_NS}
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
        size: nano
        selector:
          tier: integrated
YAML
}
# prefix_wait_observed <claim-name>
#   Block until the claim has a `status.streams.observedAt`. The inventory
#   and the jetstream conditions are written in ONE server-side apply
#   (`jetstream_status_body`), so an `observedAt` is positive evidence that
#   the rules ran against this claim — which is the whole difference between
#   "PrefixPreCaptured is correctly absent" and "nothing ever looked".
prefix_wait_observed() {
    local claim="$1" deadline got
    deadline=$(( $(date +%s) + 300 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        got=$(jp "$CLAIM_RES" "$PRE_NS" "$claim" '{.status.streams.observedAt}')
        if [ -n "$got" ]; then
            printf '    %s status.streams.observedAt = %s\n' "$claim" "$got"
            return 0
        fi
        sleep 5
    done
    printf '    %s never received a status.streams write in 300s — the signals pass did not run for it, so any negative below would prove nothing. Claim status:\n%s\n' \
        "$claim" "$(kubectl -n "$PRE_NS" get "$CLAIM_RES" "$claim" -o jsonpath='{.status}' 2>&1)"
    return 1
}

# prefix_claim_diagnosis <claim-name>
#   What a failing assertion in this criterion has to print. The first run
#   of #16 printed only the condition TYPES on the claim and cost a whole
#   20-minute walk to re-diagnose: the claim was not merely missing
#   `PrefixPreCaptured`, it had never reached Ready at all, and the reason
#   was on the Ready condition nobody printed. Everything a reader needs to
#   tell "the rule declined" from "the claim never got that far" goes here.
prefix_claim_diagnosis() {
    local claim="$1"
    printf '    --- %s ---\n' "$claim"
    printf '      status.ready   = %q\n' "$(jp "$CLAIM_RES" "$PRE_NS" "$claim" '{.status.ready}')"
    printf '      Ready  reason  = %q\n' "$(cond_reason "$CLAIM_RES" "$PRE_NS" "$claim" Ready)"
    printf '      Ready  message = %s\n' "$(cond_message "$CLAIM_RES" "$PRE_NS" "$claim" Ready)"
    printf '      condition types= %s\n' "$(jp "$CLAIM_RES" "$PRE_NS" "$claim" '{.status.conditions[*].type}')"
    printf '      status.streams = %s\n' "$(jp "$CLAIM_RES" "$PRE_NS" "$claim" '{.status.streams}')"
    printf '      spec.jetstream = %s\n' "$(jp "$CLAIM_RES" "$PRE_NS" "$claim" '{.spec.jetstream}')"
}
part3_check_16() {
    local st reason msg capture feeder_st bystander_st plans deadline
    # No namespace to create: this rides the demo account (see the note
    # above), which has existed since Phase 4.

    # feeder FIRST — and the wait is on its RESOURCECLAIM, not on its
    # Application. `prefix_pre_captured` reads `peers`, which
    # `jetstream_signals_for` builds by LISTING ResourceClaims and reading
    # `spec.jetstream.streams` off each one, so the declaration becomes
    # visible to a neighbour only once the Application controller has
    # generated the claim. Waiting on the Application would let `arrival`
    # reconcile against a namespace where feeder's declaration does not exist
    # yet — and that flake reads GREEN on the two negatives, which is the
    # worse direction to flake in.
    feeder_yaml '["feeder.own.>", "arrival.>"]' | kubectl apply -f - >/dev/null || return 1
    wait_jsonpath "$CLAIM_RES" "$PRE_NS" "${APP9}-jetstream" \
        '{.spec.jetstream.streams[0].subjects[*]}' 'feeder.own.> arrival.>' 240 || return 1

    plain_js_app_yaml "$APP10" | kubectl apply -f - >/dev/null || return 1
    plain_js_app_yaml "$APP11" | kubectl apply -f - >/dev/null || return 1

    deadline=$(( $(date +%s) + 300 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        st=$(cond_status "$CLAIM_RES" "$PRE_NS" "${APP10}-jetstream" PrefixPreCaptured)
        [ "$st" = "True" ] && break
        sleep 5
    done
    reason=$(cond_reason "$CLAIM_RES" "$PRE_NS" "${APP10}-jetstream" PrefixPreCaptured)
    msg=$(cond_message "$CLAIM_RES" "$PRE_NS" "${APP10}-jetstream" PrefixPreCaptured)
    printf '    arrival PrefixPreCaptured = %q reason = %q\n' "${st:-<unset>}" "${reason:-<unset>}"
    printf '    arrival message: %s\n' "${msg:-<none>}"
    if [ "$st" != "True" ]; then
        prefix_claim_diagnosis "${APP10}-jetstream"
        prefix_claim_diagnosis "${APP9}-jetstream"
        return 1
    fi
    assert_eq "arrival PrefixPreCaptured reason" "$reason" "PrefixDeclaredElsewhere" || return 1
    # The composed NATS-side name `<owner>_<name>`, not the declaration's
    # `inbox` — the message has to name the object an operator would go and
    # look at, and the composition is where an owner/name mix-up would show.
    assert_contains "arrival PrefixPreCaptured message" "$msg" "feeder_inbox" || return 1
    assert_contains "arrival PrefixPreCaptured message" "$msg" '"arrival."' || return 1

    # A REPORT, not a fault. If this ever became a gate the two negatives
    # would still pass and only this line would notice.
    if ! wait_jsonpath "$CLAIM_RES" "$PRE_NS" "${APP10}-jetstream" '{.status.ready}' true 300; then
        prefix_claim_diagnosis "${APP10}-jetstream"
        return 1
    fi
    # And the neighbour's DECLARED stream is not a capture: `declared_ns`
    # excludes it because somebody in the namespace vouches for it. The two
    # conditions look at the same stream and must disagree about it.
    capture=$(cond_status "$CLAIM_RES" "$PRE_NS" "${APP10}-jetstream" ForeignSubjectCapture)
    printf '    arrival ForeignSubjectCapture = %q (a DECLARED fan-in is vouched for, never a capture)\n' \
        "${capture:-<unset>}"
    [ -z "$capture" ] || return 1

    # --- the two negatives, each gated on evidence that the rules ran ---
    prefix_wait_observed "${APP9}-jetstream" || { prefix_claim_diagnosis "${APP9}-jetstream"; return 1; }
    prefix_wait_observed "${APP11}-jetstream" || { prefix_claim_diagnosis "${APP11}-jetstream"; return 1; }
    feeder_st=$(cond_status "$CLAIM_RES" "$PRE_NS" "${APP9}-jetstream" PrefixPreCaptured)
    bystander_st=$(cond_status "$CLAIM_RES" "$PRE_NS" "${APP11}-jetstream" PrefixPreCaptured)
    printf '    feeder    (its own declaration, over its own prefix too) PrefixPreCaptured = %q\n' \
        "${feeder_st:-<unset>}"
    printf '    bystander (nothing names its prefix)                     PrefixPreCaptured = %q\n' \
        "${bystander_st:-<unset>}"
    [ -z "$feeder_st" ] || return 1
    [ -z "$bystander_st" ] || return 1

    # --- the mutation: take the foreign subject back off ---
    #
    # Absence is not a measurement. The two negatives above are worth
    # something only if this condition can be made to STOP on a claim that
    # was carrying it a moment ago — and after this edit `arrival` is in
    # exactly `bystander`'s position (a neighbour declares a stream that does
    # not touch its prefix), observed as a TRANSITION rather than as a
    # never-was.
    #
    # `feeder.own.>` stays, so the stream keeps a subject (the webhook
    # refuses an empty list) and feeder's own self-filter stays exercised.
    #
    # This edit is NOT approval-gated, and that is a property of the
    # classifier rather than luck: trigger #16 arm 1 fires on a foreign
    # subject ADDED (`added_foreign` in `migration/src/strategy.rs`), and
    # taking one away is a narrowing. Asserted below anyway — if a removal
    # arm were ever added, the app would pause, the claim would never lose
    # the subject, and this would otherwise surface as an opaque timeout.
    feeder_yaml '["feeder.own.>"]' | kubectl apply -f - >/dev/null || return 1
    # Scoped to feeder by label, the way criterion #14 finds its own plan:
    # this namespace is shared, so an unlabelled listing would report a
    # neighbour's plan as feeder's.
    if ! wait_jsonpath "$CLAIM_RES" "$PRE_NS" "${APP9}-jetstream" \
        '{.spec.jetstream.streams[0].subjects[*]}' 'feeder.own.>' 240; then
        printf '    feeder-jetstream never lost the foreign subject. MigrationPlans labelled for %s: %s\n' \
            "$APP9" "$(kubectl -n "$PRE_NS" get migrationplan.apprafter.io -l "apprafter.io/application=${APP9}" -o name 2>/dev/null | tr '\n' ' ')"
        printf '    feeder Application phase=%q — if it is AwaitingMigrationApproval the classifier gained a removal arm and this criterion needs criterion #14'"'"'s approve/revert pattern\n' \
            "$(jp "$APP_RES" "$PRE_NS" "$APP9" '{.status.phase}')"
        return 1
    fi
    plans=$(kubectl -n "$PRE_NS" get migrationplan.apprafter.io -l "apprafter.io/application=${APP9}" -o name 2>/dev/null | tr '\n' ' ')
    printf '    MigrationPlans for %s after the narrowing: %s\n' "$APP9" "${plans:-<none>}"
    [ -z "${plans// /}" ] || return 1

    deadline=$(( $(date +%s) + 300 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        st=$(cond_status "$CLAIM_RES" "$PRE_NS" "${APP10}-jetstream" PrefixPreCaptured)
        [ -z "$st" ] && break
        sleep 5
    done
    printf '    after the narrowing: arrival PrefixPreCaptured = %q, status.ready = %q\n' \
        "${st:-<unset>}" "$(jp "$CLAIM_RES" "$PRE_NS" "${APP10}-jetstream" '{.status.ready}')"
    if [ -n "$st" ]; then
        printf '    the condition did not clear — message still: %s\n' \
            "$(cond_message "$CLAIM_RES" "$PRE_NS" "${APP10}-jetstream" PrefixPreCaptured)"
        printf '    feeder claim subjects now: %s\n' \
            "$(jp "$CLAIM_RES" "$PRE_NS" "${APP9}-jetstream" '{.spec.jetstream.streams[0].subjects[*]}')"
        return 1
    fi
    # Ready THROUGHOUT, not merely at the end: the report was never a gate
    # and clearing it was never a repair.
    [ "$(jp "$CLAIM_RES" "$PRE_NS" "${APP10}-jetstream" '{.status.ready}')" = "true" ]
}
if part3_check_16; then record_part3 16 "an arriving application is told its prefix is already inside a neighbour's declared stream, reaches Ready anyway, and the report clears when the declaration narrows" 0
else record_part3 16 "PrefixPreCaptured names the neighbour's composed stream on the ARRIVING claim (reason PrefixDeclaredElsewhere) while that claim still reaches Ready, does not fire on the declarer itself or on an uninvolved third application in the same account — each negative gated on a status.streams write proving the rules ran — and CLEARS once the foreign subject is taken back off the declaration" 1; fi

# --- #17: an out-of-memory-budget namespace is REFUSED, legibly ---
#
# THE FIXTURE IS SIZED AGAINST WHAT `demo` ALREADY HOLDS, and that is not
# incidental — it is the constraint that cost the previous fixture author
# a whole run. By the time this criterion runs, `demo` holds six claims at
# `small` (256Mi) and three at `nano` (64Mi): 1.6875Gi of file quota, so
# 172.8Mi of memory reservation (a tenth, floored per namespace) of the
# 192Mi the server has. That leaves 19.2Mi — less than the 64Mi FLOOR any
# new namespace costs however small its claims are. So this app is `nano`,
# in a namespace of its own, and still cannot fit; `demo` itself does not
# move, so criteria #1-#16 stand exactly where they were recorded.
#
# What is being asserted is not "it fails" but WHERE and HOW it fails:
#   * on the claim, with its own reason, naming the budget in bytes —
#     not `AwaitingNatsUserReady`, whose message ("the server may not have
#     reloaded the accounts file yet") blames the one thing that works;
#   * with the account never reaching the accounts file at all, which is
#     what keeps the INSTALLED file — and `demo`, still serving above —
#     unharmed. That second half is the whole reason the render refuses
#     rather than writing a file the server would partially reject.
#
# While this fixture stands the accounts file is refused CLUSTER-WIDE, so
# every other not-yet-ready claim reports the same condition. There is
# exactly one such claim here (hogapp, parked at QuotaExceeded by #13,
# already recorded), and the fixture is torn down at the end of this
# check.
BUDGET_NS="jsbudget"
APP12="budgetapp"
CLAIM12="budgetapp-jetstream"
# component_nats.cue's config.jetstream.memoryStore.maxSize: "192Mi", in
# bytes — the figure the refusal has to name. The operator holds the same
# number in NATS_MEMORY_BUDGET_BYTES_FALLBACK, gated against the chart by
# `the_nats_budget_constants_match_component_nats_cue`; this is a THIRD
# copy, on purpose, so the walk fails if the delivered behaviour ever
# stops matching the chart the walk itself installed.
NATS_MEMORY_BUDGET_BYTES=201326592
part3_check_17() {
    local reason msg accounts_now demo_ready demo_account claims_before
    kubectl create namespace "$BUDGET_NS" 2>/dev/null || true
    kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP12}
  namespace: ${BUDGET_NS}
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
        size: nano
        selector:
          tier: integrated
YAML
    wait_jsonpath "$CLAIM_RES" "$BUDGET_NS" "$CLAIM12" '{.spec.type}' jetstream 120 || return 1

    local deadline
    deadline=$(( $(date +%s) + 240 ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        reason=$(cond_reason "$CLAIM_RES" "$BUDGET_NS" "$CLAIM12" Ready)
        [ "$reason" = "NatsMemoryBudgetExceeded" ] && break
        sleep 5
    done
    msg=$(cond_message "$CLAIM_RES" "$BUDGET_NS" "$CLAIM12" Ready)
    printf '    %s Ready reason = %q\n' "$APP12" "${reason:-<unset>}"
    printf '    %s Ready message: %s\n' "$APP12" "${msg:-<none>}"
    if [ "$reason" != "NatsMemoryBudgetExceeded" ]; then
        printf '    the claim did not report the budget. If it is sitting on AwaitingNatsUserReady, that is the ORIGINAL defect: a server that reloaded the file and gave this account no JetStream, reported as a reload that has not happened yet.\n'
        return 1
    fi
    assert_contains "${APP12} Ready message" "$msg" "$NATS_MEMORY_BUDGET_BYTES" || return 1

    # The account never reached the file — the refusal is a refusal to
    # WRITE, not a note attached to a file that went out anyway.
    accounts_now=$(secret_val "$NATS_NS" "$ACCOUNTS_SECRET" 'accounts\.conf')
    printf '    ns_%s present in the accounts file: %s\n' "$BUDGET_NS" \
        "$([[ "$accounts_now" == *"ns_${BUDGET_NS}:"* ]] && echo yes || echo no)"
    [[ "$accounts_now" != *"ns_${BUDGET_NS}:"* ]] || return 1

    # And the file that IS installed still serves: demo's first claim is
    # still Ready, and its account's JETSTREAM still answers — `account
    # info` is the `$JS.API.INFO` round trip, which is precisely the call
    # that goes unanswered for an account the server could not enable.
    # Asking it of an untouched namespace is how this criterion proves the
    # refusal protected the incumbents instead of merely failing quietly.
    demo_ready=$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM1" '{.status.ready}')
    demo_account=$(mgr_nats_run "$APP_NS" account info 2>&1) || {
        printf '    the %s account stopped answering $JS.API.INFO while the over-budget namespace was pending: %s\n' \
            "$APP_NS" "$demo_account"
        return 1
    }
    printf '    %s still Ready=%q, and ns_%s still answers account info\n' \
        "$CLAIM1" "$demo_ready" "$APP_NS"
    [ "$demo_ready" = "true" ] || return 1

    # Tear the fixture down so the cluster is back under budget for the
    # phases after this one.
    claims_before=$(kubectl -n "$BUDGET_NS" get "$CLAIM_RES" -o name 2>/dev/null | tr '\n' ' ')
    printf '    claims in %s before teardown: %s\n' "$BUDGET_NS" "${claims_before:-<none>}"
    kubectl delete "$APP_RES" "$APP12" -n "$BUDGET_NS" --wait=true --timeout=120s
    wait_gone "$CLAIM_RES" "$BUDGET_NS" "$CLAIM12" 180 || return 1
    printf '    %s deleted — the cluster is back inside its memory budget\n' "$APP12"
    return 0
}
if part3_check_17; then record_part3 17 "a namespace that would overrun the server's JetStream memory budget is refused with NatsMemoryBudgetExceeded, naming the budget, and never reaches the accounts file" 0
else record_part3 17 "a namespace whose account would push the cluster past component_nats.cue's memoryStore.maxSize is held unready with its OWN reason naming the budget in bytes — not AwaitingNatsUserReady — its account never enters the accounts file, and the file already installed keeps serving the namespaces on it" 1; fi

# ===============================================================
# 2.28 (ADR 0065 §2): the tuning surface and the dead-letter queue.
#
# Everything about 2.28-B up to this point is proved in PIECES — the field
# NAMES against the real NACK CRD (e2e/jetstream-nack-shape-check.sh), the
# SERVER behaviours on the pinned server
# (docs/measurements/2.28-jetstream-2026-09-15.md), the provisioner's
# EMISSION by unit test. None of them joins the chain. These four do:
# manifest → claim → NACK CR → live NATS config, and then a dead-letter
# queue that actually receives something.
#
# The fixture is deliberately small in quota terms. The account's max_file
# is the SUM over the namespace's claims, so a sixth claim raises the
# budget by one 'small' (256Mi) while this app's two streams together ask
# for 80Mi — comfortably inside it, for the same reason streamapp asks for
# 256Mi rather than 1Gi (see its own note above).
# ===============================================================

phase "2.28 fixture: tuneapp — a tuned stream, a tuned durable, and a DLQ"

APP7="tuneapp"
CLAIM7="tuneapp-jetstream"

kubectl apply -f - <<YAML
apiVersion: apprafter.io/v1alpha1
kind: Application
metadata:
  name: ${APP7}
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
          - name: work
            subjects: ["tuneapp.work.>"]
            maxBytes: "64Mi"
            retention: workqueue
            compression: s2
            discard: new
            maxMsgsPerSubject: 100
            consumerLimits:
              maxAckPending: 64
        consume:
          - stream: work
            durable: worker
            ackWait: "1s"
            maxDeliver: 2
            maxAckPending: 10
            deadLetter:
              stream: worker-dlq
              maxBytes: "16Mi"
YAML

printf '  waiting for %s to reach Ready ...\n' "$CLAIM7"
for _ in $(seq 1 60); do
    [ "$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM7" '{.status.ready}')" = "true" ] && break
    sleep 5
done
printf '    %s status.ready=%q reason=%q\n' "$CLAIM7" \
    "$(jp "$CLAIM_RES" "$APP_NS" "$CLAIM7" '{.status.ready}')" \
    "$(cond_reason "$CLAIM_RES" "$APP_NS" "$CLAIM7" Ready)"

# --- #18: the CONSUMER tuning reaches the live consumer config ---
part3_check_18() {
    local out
    out=$(mgr_nats_run "$APP_NS" consumer info tuneapp_work tuneapp_worker --json 2>&1) || {
        printf '    querying consumer tuneapp_work/tuneapp_worker failed: %s\n' "$out"
        return 1
    }
    printf '    consumer config: %s\n' "$(printf '%s' "$out" | jq -c '.config | {ack_wait, max_deliver, max_ack_pending}' 2>/dev/null)"
    # ack_wait is nanoseconds on the wire: 1s == 1000000000.
    printf '%s' "$out" | jq -e '
        (.config.ack_wait == 1000000000) and
        (.config.max_deliver == 2) and
        (.config.max_ack_pending == 10)
    ' >/dev/null
}
if part3_check_18; then record_part3 18 "a consume entry's ackWait/maxDeliver/maxAckPending reach the LIVE consumer" 0
else record_part3 18 "the tuning declared on consume[] reaches the live NATS consumer config — the link no unit test can see, because NACK owns the durable and re-asserts its own config" 1; fi

# --- #19: the STREAM tuning reaches the live stream config ---
part3_check_19() {
    local out
    out=$(mgr_nats_run "$APP_NS" stream info tuneapp_work --json 2>&1) || {
        printf '    querying stream tuneapp_work failed: %s\n' "$out"
        return 1
    }
    printf '    stream config: %s\n' "$(printf '%s' "$out" | jq -c '.config | {compression, discard, max_msgs_per_subject, consumer_limits}' 2>/dev/null)"
    printf '%s' "$out" | jq -e '
        (.config.compression == "s2") and
        (.config.discard == "new") and
        (.config.max_msgs_per_subject == 100) and
        (.config.consumer_limits.max_ack_pending == 64)
    ' >/dev/null
}
if part3_check_19; then record_part3 19 "a stream's compression/discard/maxMsgsPerSubject/consumerLimits reach the LIVE stream" 0
else record_part3 19 "the tuning declared on streams[] reaches the live NATS stream config" 1; fi

# --- #20: the DLQ exists, and collects EXACTLY one advisory subject ---
part3_check_20() {
    local out subjects want
    out=$(mgr_nats_run "$APP_NS" stream info "tuneapp_worker-dlq" --json 2>&1) || {
        printf '    querying the DLQ stream failed: %s\n' "$out"
        return 1
    }
    subjects=$(printf '%s' "$out" | jq -c '.config.subjects' 2>/dev/null)
    want='["$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.tuneapp_work.tuneapp_worker"]'
    printf '    DLQ subjects: %s\n' "$subjects"
    # EXACTLY this pair's advisory. A wildcard here would collect every
    # neighbour's failures in the same account — the disclosure the deny
    # vector exists to prevent — so the assertion is equality, not a match.
    [ "$subjects" = "$want" ] || return 1
    printf '%s' "$out" | jq -e '(.config.retention == "limits")' >/dev/null
}
if part3_check_20; then record_part3 20 "the dead-letter queue is an ordinary declared stream collecting exactly this pair's advisory" 0
else record_part3 20 "deadLetter materialises a declared stream whose single subject is the exact MAX_DELIVERIES advisory for (composed stream, composed durable), with limits retention" 1; fi

# --- #21: the DLQ actually FILLS when redelivery is exhausted ---
part3_check_21() {
    local pub i before after
    pub=$(mgr_nats_run "$APP_NS" pub tuneapp.work.one 'poison' 2>&1) || {
        printf '    publishing to the tuned stream failed: %s\n' "$pub"
        return 1
    }
    before=$(mgr_nats_run "$APP_NS" stream info "tuneapp_worker-dlq" --json 2>&1 \
        | jq -r '.state.messages // 0' 2>/dev/null)
    # maxDeliver is 2, so two NAKs exhaust redelivery and the server emits
    # the advisory. `|| true` on each: a NAK'd fetch is a normal outcome
    # here, and `set -e` would otherwise end the walk on the thing we want.
    for i in 1 2 3; do
        mgr_nats_run "$APP_NS" consumer next tuneapp_work tuneapp_worker \
            --count 1 --timeout 3s --nak >/dev/null 2>&1 || true
        sleep 2
    done
    for i in $(seq 1 15); do
        after=$(mgr_nats_run "$APP_NS" stream info "tuneapp_worker-dlq" --json 2>&1 \
            | jq -r '.state.messages // 0' 2>/dev/null)
        [ "${after:-0}" -gt "${before:-0}" ] && break
        sleep 2
    done
    printf '    DLQ messages before=%s after=%s\n' "${before:-0}" "${after:-0}"
    [ "${after:-0}" -gt "${before:-0}" ]
}
if part3_check_21; then record_part3 21 "the DLQ RECEIVES an advisory once redelivery is exhausted" 0
else record_part3 21 "a message NAK'd past maxDeliver produces an advisory that lands in the declared dead-letter queue — the end of the chain, and the only check here that proves the DLQ is not merely a correctly-shaped empty stream" 1; fi

phase "Part 3 acceptance criteria summary"
if [ "$PART3_FAILED" -gt 0 ]; then
    printf '  %d of 21 acceptance criteria are RED. Part 3 (NACK CR application), 2.5f (inventory/detector/conditions), 2.5 part 4 (the migration triggers), the 2.5 egress rule, the prefix-pre-capture report, the memory-budget clamp and the 2.28 tuning + dead-letter queue have all landed, so each one is a real defect — not an expected gap.\n' "$PART3_FAILED"
else
    printf '  ok: all 21 acceptance criteria are GREEN.\n'
fi

# ===============================================================
# Also worth checking (while a cluster exists): the operator log
# carries no forbidden-verb complaint. Runs LAST so it covers every log
# line the whole walk (part-3 fixtures included) produced.
# ===============================================================

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

if [ "$PART3_FAILED" -gt 0 ]; then
    phase "needs-jetstream-walk: part 2 GREEN, acceptance RED (${PART3_FAILED}/21) (elapsed $(elapsed))"
    printf 'FINAL: ACCEPTANCE-RED (%d/21) — every part-2 capability above stayed green; see the summary above for which of the twenty-one criteria are unmet and why.\n' \
        "$PART3_FAILED"
    exit 1
fi

phase "needs-jetstream-walk: ALL PHASES GREEN (elapsed $(elapsed))"
printf 'FINAL: PASS\n'
