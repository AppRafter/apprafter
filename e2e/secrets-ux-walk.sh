#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# AppRafter secrets-UX walk e2e — the CLI-FACING halves of day-2 ledger
# entries D6 and D7 (docs/measurements/day2-followups.md), on a local
# kind/k3d cluster.
#
# WHY THIS WALK EXISTS
# ====================
# D6 ("rotating a secret does not take effect until something else restarts
# the pods") and D7 ("the CLI cannot answer the question its own error asks")
# were both fixed in 2.22c and both are recorded as "walk owed". The OPERATOR
# halves already have one: e2e/needs-env-refs-walk.sh Phase 5 proves the
# `EnvSecretMissing` condition message distinguishes absent-Secret from
# present-but-wrong-key and names the namespace + the keys that ARE there, and
# its Phase 6 proves a rotation moves `status.envConfig.digest`/`changedAt`
# without rolling the pod. Both read the CR with kubectl.
#
# That is precisely the reader D7 is about. The whole complaint is that the
# answer existed in `status.conditions[type=Ready].message` and no CLI surface
# printed it, so every diagnosis path left the CLI. So this walk asserts the
# other end of both fixes — what the PRODUCT SHOWS:
#
#   D7 (CLI half)  `apprafter app status` prints the failure's EXPLANATION
#                  under its NAME: the reason, "carries no key", the namespace
#                  in quotes, and the key the Secret actually carries
#                  (cli/platform-cli/src/commands/app.rs :: format_not_ready_line).
#   D6 (CLI half)  `apprafter app status` flags a pod that predates the last
#                  config change with `← old config` plus the note explaining
#                  that a secret-sourced env var is resolved once at pod start
#                  (app.rs :: pod_is_stale / print_pod_summaries).
#
# It also covers the two `secret` verbs that exist because of D7 and the
# namespace footgun around them: `secret list` (where a secret lives + which
# KEYS it carries + when it was sealed) and `secret remove`.
#
# Nothing here duplicates needs-env-refs-walk: that walk applies a marker-form
# CR directly and reads conditions with kubectl; this one drives the real
# product surfaces — `secret seal|list|remove`, `app add`, `app status` — and
# reads THEIR output.
#
# THE LEGS
# ========
#   1. cluster up, CLI state seeded, `cluster-bootstrap`, working-tree
#      operator + webhook + branch CRDs/RBAC side-loaded.
#   2. the app namespace created explicitly, BEFORE any seal.
#   3. `secret seal shop-api -n <app-ns> --from-literal token=v1`
#      -> SealedSecret + unsealed Secret exist IN THE APP NAMESPACE, and
#         NOTHING of that name exists in apprafter-system.
#   4. `secret list -n <app-ns>` names the secret, its namespace, its KEY and
#      a real sealed-at timestamp.
#   5. `app add` from a host git daemon; CR Ready; a workload pod Running.
#   6. break the binding THROUGH A PRODUCT SURFACE: re-seal the SAME name with
#      a DIFFERENT key (sealing REPLACES, it does not merge) -> the referenced
#      key is genuinely gone -> CR phase EnvSecretMissing.
#   7. D7: `apprafter app status` explains it.
#   8. recover: re-seal the original key with a new value -> CR Ready.
#   9. D6: rotate again while a pod is running -> `apprafter app status` shows
#      `← old config` on that pod AND the pod was NOT replaced.
#  10. `secret remove --yes` -> BOTH the SealedSecret and the Secret are gone.
#
# ORDERING CONSTRAINTS — every one of these is load-bearing
# =========================================================
#   * The namespace is the point of legs 2-4. `secret seal`'s `--namespace`
#     DEFAULTS to `apprafter-system` (platform credential material), and a
#     secret sealed there is invisible to an app that references it — the
#     recorded footgun. Creating the namespace first and asserting the
#     apprafter-system NEGATIVE is what makes leg 3 mean anything.
#   * Leg 6 breaks the binding by RE-SEALING, never by `kubectl delete secret`:
#     the sealed-secrets controller re-creates a deleted Secret from the
#     SealedSecret within seconds, and the walk would lose the race. Re-sealing
#     under a different key replaces the object's data, which is both durable
#     and a real product path.
#   * Leg 6 asserts the referenced key is ACTUALLY gone from the unsealed
#     Secret before asserting anything downstream. Otherwise a walk that timed
#     out waiting for `EnvSecretMissing` could not say whether the operator or
#     the controller was at fault.
#   * Leg 9's `← old config` flag is `pod.startTime < status.envConfig.changedAt`.
#     The pod MUST therefore predate the rotation: the walk records the pod
#     WHILE Ready, and only then rotates. It asserts BOTH halves — the flag
#     appears AND the pod was not replaced — because D6's decision was that the
#     drift becomes VISIBLE and deliberately does NOT ACT. A walk that only
#     checked "the new value reached the pod" would assert the behaviour that
#     was explicitly turned down.
#   * Every re-seal passes `--yes` and the remove passes `--yes`: in a
#     non-interactive shell the overwrite/delete gate is a hard error (exit 1),
#     by design.
#   * `app status` output is captured with NO_COLOR=1 (the stale row and its
#     note go through cli-core's colour helper) and matched on SHORT fragments
#     — the multi-line notes carry TRAILING SPACES and fixed indentation, so
#     `$` anchors and column offsets never match.
#   * NEVER `cmd | grep -q`: under `set -o pipefail` grep closes the pipe on
#     its first match, the producer dies of SIGPIPE, and A MATCH READS AS A
#     MISS. Everything here captures to a variable or a file and matches with
#     `case` / `[[ == *needle* ]]`. Likewise no pipeline whose left side may
#     legitimately fail while polling for an object that does not exist yet.
#   * No phase waits for the Argo CD Application to be Synced+Healthy while the
#     CR is not Ready: the health Lua reports Progressing for every non-Ready
#     phase, so such a wait can only burn its budget. Legs poll the CR.
#   * The fixture pins `imagePolicy.resolve: "off"` (ADR 0040) so an unrelated
#     tag re-resolution cannot roll the pod and destroy leg 9.
#
# Local-operator mode (FORCED)
# ----------------------------
# The D7 line asserted in leg 7 ships in the WORKING-TREE CLI, which lib.sh's
# `apprafter()` already runs from source (`cargo run`), so the CLI half needs
# no side-load. The OPERATOR halves it depends on — the `EnvSecretMissing`
# message naming the available keys, and `status.envConfig` — are 2.22c, so the
# walk FORCES APPRAFTER_E2E_LOCAL_OPERATOR=1 (precedent:
# e2e/expose-deep-merge-walk.sh) and Phase 1b builds + side-loads the
# working-tree operator + admission-webhook, applies the branch-rendered CRDs
# and the branch RBAC. (The CRDs are applied for the SPEC side and for RBAC
# parity, NOT because status would otherwise be pruned: the Application CRD's
# `status` node carries `x-kubernetes-preserve-unknown-fields: true`, so
# `status.envConfig` survives against any published CRD version.)
#
# CLI state injection
# -------------------
# `cluster-bootstrap` / `secret *` / `app *` read the kubeconfig from the CLI's
# per-target state store, not from $KUBECONFIG. APPRAFTER_CONFIG_DIR points at
# a tmpdir seeded with config.yaml (active_target: k3d) + a state.json carrying
# the kubeconfig as plaintext `kubeconfig_yaml` — same as every other walk.
#
# Required: docker (or podman), git, cargo, kubectl — all satisfied inside
# `nix develop` or on a standard CI runner.
#
# Judge this walk BY READING THE LOG: every leg prints an `ok:` line, failures
# print `ERROR:` to stderr, and the final GREEN banner prints ONLY on the
# success path (a sandbox-run wrapper masks the inner exit code).
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
# The 2.22c operator surface this walk reads (envConfig, the keys-listing
# EnvSecretMissing message) is not in the published chart, so local-build
# mode is MANDATORY — force it rather than requiring the caller to know.
# ---------------------------------------------------------------
export APPRAFTER_E2E_LOCAL_OPERATOR=1

# The stale-pod row and its note go through cli-core::style, which honours
# NO_COLOR lazily per format call. Exported globally so every `cargo run`
# child inherits it and every captured file is plain text.
export NO_COLOR=1

# ---------------------------------------------------------------
# Constants
# ---------------------------------------------------------------

CLUSTER_NAME="apprafter-secrets-ux-walk"
FIXTURE_SRC="${REPO_ROOT}/e2e/fixtures/secrets-ux-app"

APP="shop"                      # AppRafter CR + Argo CD Application name
APP_NS="secrets-ux"             # tenant namespace (created explicitly, leg 2)
PLATFORM_NS="apprafter-system"  # operator + webhook + sealed-secrets live here
                                # AND it is `secret seal`'s DEFAULT namespace,
                                # which is exactly the footgun leg 3 asserts.

SECRET_NAME="shop-api"          # must match the fixture's secret: payload
SECRET_KEY="token"              # ditto
WRONG_KEY="other"               # the key leg 6 re-seals under, replacing token

VAL_1="tok-one"                 # sealed in leg 3
VAL_BREAK="tok-under-wrong-key" # leg 6 (under WRONG_KEY)
VAL_2="tok-two"                 # leg 8 recovery (under SECRET_KEY)
VAL_3="tok-three"               # leg 9 rotation (under SECRET_KEY)

ENV_VAR="API_KEY"               # the fixture env name bound to the secret

GIT_DAEMON_PORT="9422"          # distinct from 9418/9419/9420/9421

# Group-qualify the collision-prone kinds so kubectl never resolves to the
# wrong API group (Argo CD's argoproj.io Application vs apprafter.io).
ARGO_APP="application.argoproj.io"
AR_APP="application.apprafter.io"

# ---------------------------------------------------------------
# Tool checks (fail loudly, never silently skip)
# ---------------------------------------------------------------

for tool in git cargo kubectl helm; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'ERROR: required tool "%s" not found on PATH\n' "$tool" >&2
        exit 2
    fi
done

if ! command -v docker >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
    printf 'ERROR: neither "docker" nor "podman" found on PATH\n' >&2
    exit 2
fi

# ---------------------------------------------------------------
# Temp workspace
# ---------------------------------------------------------------

TMPDIR_WORK="$(mktemp -d)"
APPRAFTER_CONFIG_DIR="${TMPDIR_WORK}/apprafter-config"
GIT_REPOS_DIR="${TMPDIR_WORK}/git-repos"
KUBECONFIG_FILE="${TMPDIR_WORK}/kubeconfig"
OUT_DIR="${TMPDIR_WORK}/out"        # captured CLI output lives here
mkdir -p "$OUT_DIR"
GIT_DAEMON_PID=""

# Set to 1 only after the cluster is up AND $KUBECONFIG points at it. Until
# then dump_diagnostics / k3d_down must NOT run — on a cluster-up failure
# $KUBECONFIG still points at the ambient cluster, and an e2e must never touch
# a non-test cluster.
K3D_CREATED=0

cleanup() {
    local exit_code=$?

    # Kill the git daemon first so the port is free. `git daemon --detach`
    # double-forks (and the process renames itself `git-daemon`, hence the
    # `git[ -]daemon` pattern), so also pkill by the unique port so a detached
    # daemon cannot leak and wedge the port for the next run.
    if [ -n "$GIT_DAEMON_PID" ] && kill -0 "$GIT_DAEMON_PID" 2>/dev/null; then
        kill "$GIT_DAEMON_PID" 2>/dev/null || true
    fi
    pkill -f "git[ -]daemon.*${GIT_DAEMON_PORT}" 2>/dev/null || true

    if [ "$exit_code" -ne 0 ]; then
        printf '\n!!! secrets-ux-walk FAILED at %s (exit %d) !!!\n' \
            "$(elapsed)" "$exit_code" >&2
        if [ "$K3D_CREATED" -eq 1 ]; then
            dump_diagnostics
            printf 'Tearing down cluster (set APPRAFTER_E2E_SKIP_DESTROY=1 to keep).\n' >&2
        else
            printf 'cluster %s was never created (runtime unavailable?) — skipping diagnostics + teardown; your ambient KUBECONFIG was NOT touched.\n' \
                "$CLUSTER_NAME" >&2
        fi
    fi

    if [ "$K3D_CREATED" -eq 1 ]; then
        if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
            k3d_down "$CLUSTER_NAME" || true
        else
            printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' \
                "$CLUSTER_NAME"
            printf 'Run: <k3d|kind> cluster delete %s\n' "$CLUSTER_NAME"
        fi
    fi

    rm -rf "$TMPDIR_WORK"
    exit "$exit_code"
}
trap cleanup EXIT

# ---------------------------------------------------------------
# Helper: apprafter CLI with cwd = the fixture dir.
#
# lib.sh's apprafter() always cd's to REPO_ROOT/cli; `app add` however uses
# the CWD to locate apprafter/Application.cue (the scaffold gate). We cd into
# the fixture and pass --manifest-path so cargo still finds the CLI workspace.
# Only `app add` needs this; `secret *` and `app status` use the plain
# apprafter() from lib.sh.
# ---------------------------------------------------------------
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

# ---------------------------------------------------------------
# Helper: seed the CLI state store (mirrors gitops-walk-per-env.sh).
# ---------------------------------------------------------------
seed_apprafter_state() {
    local kubeconfig_content="$1"

    mkdir -p "${APPRAFTER_CONFIG_DIR}"
    mkdir -p "${APPRAFTER_CONFIG_DIR}/state/k3d/.apprafter"

    cat >"${APPRAFTER_CONFIG_DIR}/config.yaml" <<'YAML'
active_target: k3d
version: 1
YAML

    # Escape the kubeconfig for JSON embedding (newlines -> \n, escape
    # backslashes and double quotes).
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
# Helper: init the fixture git repo and start the host git daemon.
# ---------------------------------------------------------------
setup_git_server() {
    local repo_dst="${GIT_REPOS_DIR}/secrets-ux-app"

    mkdir -p "${GIT_REPOS_DIR}"

    cp -r "${FIXTURE_SRC}" "${repo_dst}"
    (
        cd "${repo_dst}"
        git init -b main
        git config user.email "e2e@apprafter.io"
        git config user.name "AppRafter E2E"
        git add .
        git commit -m "feat: initial secrets-ux-app fixture"
    )
    touch "${repo_dst}/.git/git-daemon-export-ok"

    # Reap any leaked daemon holding the port from a prior run.
    pkill -f "git[ -]daemon.*${GIT_DAEMON_PORT}" 2>/dev/null || true

    git daemon \
        --reuseaddr \
        --base-path="${GIT_REPOS_DIR}" \
        --export-all \
        --port="${GIT_DAEMON_PORT}" \
        --detach \
        "${GIT_REPOS_DIR}"

    GIT_DAEMON_PID=$(pgrep -f "git[ -]daemon.*${GIT_DAEMON_PORT}" | head -1 || true)
    printf '  git daemon started (port %s, base %s)\n' \
        "$GIT_DAEMON_PORT" "$GIT_REPOS_DIR"
}

# ---------------------------------------------------------------
# Assertion helpers
# ---------------------------------------------------------------

assert_eq() {  # <description> <got> <want>
    local desc="$1" got="$2" want="$3"
    if [ "$got" = "$want" ]; then
        printf '  ok: %s = %q\n' "$desc" "$got"
        return 0
    fi
    printf 'ERROR: %s — got %q, want %q\n' "$desc" "$got" "$want" >&2
    return 1
}

assert_ne() {  # <description> <got> <unwanted>
    local desc="$1" got="$2" unwanted="$3"
    if [ "$got" != "$unwanted" ]; then
        printf '  ok: %s — moved off %q (now %q)\n' "$desc" "$unwanted" "$got"
        return 0
    fi
    printf 'ERROR: %s — value did not change (still %q)\n' "$desc" "$got" >&2
    return 1
}

assert_nonempty() {  # <description> <got>
    local desc="$1" got="$2"
    if [ -n "$got" ]; then
        printf '  ok: %s = %q\n' "$desc" "$got"
        return 0
    fi
    printf 'ERROR: %s — got an empty value\n' "$desc" >&2
    return 1
}

# Substring match on a VARIABLE. Never `printf | grep -q`: with pipefail a
# grep that matches early kills the producer with SIGPIPE and the match reads
# as a miss. `case` needs no subprocess at all.
assert_contains() {  # <description> <haystack> <needle>
    local desc="$1" hay="$2" needle="$3"
    case "$hay" in
        *"$needle"*)
            printf '  ok: %s (found %q)\n' "$desc" "$needle"
            return 0
            ;;
    esac
    printf 'ERROR: %s — %q not found in:\n%s\n' "$desc" "$needle" "$hay" >&2
    return 1
}

# Substring match on a captured FILE. Dumps the whole file on failure — the
# CLI output IS the evidence for the D6/D7 legs.
assert_file_contains() {  # <description> <file> <needle>
    local desc="$1" file="$2" needle="$3" content
    content="$(cat "$file")"
    case "$content" in
        *"$needle"*)
            printf '  ok: %s (found %q)\n' "$desc" "$needle"
            return 0
            ;;
    esac
    printf 'ERROR: %s — %q not found. Full output of %s:\n' \
        "$desc" "$needle" "$file" >&2
    cat "$file" >&2
    return 1
}

# The inverse. A positive-only walk cannot distinguish "the flag appeared
# because of what I did" from "the flag was already there" — and for the D6 leg
# below, it was already there. This is what pins the difference.
assert_file_lacks() {  # <description> <file> <needle>
    local desc="$1" file="$2" needle="$3" content
    content="$(cat "$file")"
    case "$content" in
        *"$needle"*)
            printf 'ERROR: %s — %q IS present, and must not be. Full output of %s:\n' \
                "$desc" "$needle" "$file" >&2
            cat "$file" >&2
            return 1
            ;;
    esac
    printf '  ok: %s (%q absent, as required)\n' "$desc" "$needle"
    return 0
}

# First line of <file> containing <needle>, or empty. Pure bash: no pipeline,
# so a no-match cannot trip pipefail.
line_containing() {  # <file> <needle>
    local file="$1" needle="$2" line
    while IFS= read -r line; do
        case "$line" in
            *"$needle"*) printf '%s' "$line"; return 0 ;;
        esac
    done <"$file"
    return 0
}

# Count non-empty lines of a STRING without a pipeline (a `kubectl … | wc -l`
# whose left side legitimately fails takes the whole walk down under pipefail).
count_lines() {  # <string>
    local n=0 line
    while IFS= read -r line; do
        if [ -n "$line" ]; then n=$((n + 1)); fi
    done <<<"$1"
    printf '%d' "$n"
}

# Read one value. `|| true` so a missing object yields "" instead of killing
# the walk mid-poll.
jp() {  # <kind> <ns> <name> <jsonpath>
    kubectl -n "$2" get "$1" "$3" -o jsonpath="$4" 2>/dev/null || true
}

# Poll until a jsonpath renders exactly <want>.
wait_jsonpath() {  # <kind> <ns> <name> <jsonpath> <want> [timeout]
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

# Poll until an object exists at all.
wait_exists() {  # <kind> <ns> <name> [timeout]
    local kind="$1" ns="$2" name="$3" timeout="${4:-180}"
    local deadline
    deadline=$(( $(date +%s) + timeout ))
    printf '  wait for %s/%s in %s (timeout %ss) ...\n' "$kind" "$name" "$ns" "$timeout"
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if kubectl -n "$ns" get "$kind" "$name" >/dev/null 2>&1; then
            printf '  ok: %s/%s exists in %s\n' "$kind" "$name" "$ns"
            return 0
        fi
        sleep 5
    done
    printf 'ERROR: %s/%s never appeared in %s within %ss\n' \
        "$kind" "$name" "$ns" "$timeout" >&2
    return 1
}

# Poll until an object is gone.
wait_absent() {  # <kind> <ns> <name> [timeout]
    local kind="$1" ns="$2" name="$3" timeout="${4:-120}"
    local deadline
    deadline=$(( $(date +%s) + timeout ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if ! kubectl -n "$ns" get "$kind" "$name" >/dev/null 2>&1; then
            printf '  ok: %s/%s is gone from %s\n' "$kind" "$name" "$ns"
            return 0
        fi
        sleep 3
    done
    printf 'ERROR: %s/%s still present in %s after %ss\n' \
        "$kind" "$name" "$ns" "$timeout" >&2
    kubectl -n "$ns" get "$kind" "$name" -o yaml >&2 2>&1 || true
    return 1
}

# One-shot absence assertion (no polling — used for the apprafter-system
# NEGATIVE, which must be true the instant the seal returns).
assert_absent_now() {  # <description> <kind> <ns> <name>
    local desc="$1" kind="$2" ns="$3" name="$4"
    if kubectl -n "$ns" get "$kind" "$name" >/dev/null 2>&1; then
        printf 'ERROR: %s — %s/%s EXISTS in %s and must not\n' \
            "$desc" "$kind" "$name" "$ns" >&2
        kubectl -n "$ns" get "$kind" "$name" >&2 2>&1 || true
        return 1
    fi
    printf '  ok: %s (no %s/%s in %s)\n' "$desc" "$kind" "$name" "$ns"
    return 0
}

# Poll until the unsealed Secret's `.data.<key>` is present / absent. The
# sealed-secrets controller reconciles the Secret from the SealedSecret, so a
# re-seal lands a beat after `secret seal` returns.
wait_secret_key_present() {  # <ns> <name> <key> [timeout]
    local ns="$1" name="$2" key="$3" timeout="${4:-120}"
    local deadline got
    deadline=$(( $(date +%s) + timeout ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        got=$(jp secret "$ns" "$name" "{.data.${key}}")
        if [ -n "$got" ]; then
            printf '  ok: Secret %s/%s carries key %q\n' "$ns" "$name" "$key"
            return 0
        fi
        sleep 3
    done
    printf 'ERROR: Secret %s/%s never carried key %q within %ss\n' \
        "$ns" "$name" "$key" "$timeout" >&2
    kubectl -n "$ns" get secret "$name" -o yaml >&2 2>&1 || true
    return 1
}

wait_secret_key_absent() {  # <ns> <name> <key> [timeout]
    local ns="$1" name="$2" key="$3" timeout="${4:-120}"
    local deadline got
    deadline=$(( $(date +%s) + timeout ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        got=$(jp secret "$ns" "$name" "{.data.${key}}")
        if [ -z "$got" ]; then
            printf '  ok: Secret %s/%s no longer carries key %q\n' "$ns" "$name" "$key"
            return 0
        fi
        sleep 3
    done
    printf 'ERROR: Secret %s/%s still carries key %q after %ss — the re-seal did not replace it\n' \
        "$ns" "$name" "$key" "$timeout" >&2
    kubectl -n "$ns" get secret "$name" -o yaml >&2 2>&1 || true
    return 1
}

# The single workload pod's name / start time.
app_pod() {
    kubectl -n "$APP_NS" get pods -l "app.kubernetes.io/name=${APP}" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}
pod_start_time() {  # <pod>
    kubectl -n "$APP_NS" get pod "$1" \
        -o jsonpath='{.status.startTime}' 2>/dev/null || true
}

# Capture `apprafter app status <app>` (NO_COLOR is exported globally) into a
# file and echo the path. Fails loudly with the output on a non-zero exit.
capture_app_status() {  # <label>
    local label="$1" out="${OUT_DIR}/app-status-${1}.txt"
    if ! apprafter app status "$APP" >"$out" 2>&1; then
        printf 'ERROR: `apprafter app status %s` failed (%s). Output:\n' "$APP" "$label" >&2
        cat "$out" >&2
        return 1
    fi
    printf '%s' "$out"
}

# ===============================================================
# Phase 0: bring up the cluster
# ===============================================================

phase "Phase 0: cluster up ${CLUSTER_NAME}"

k3d_up "$CLUSTER_NAME"

cluster_kubeconfig_write "$CLUSTER_NAME" "$KUBECONFIG_FILE"
export KUBECONFIG="$KUBECONFIG_FILE"
K3D_CREATED=1
printf '  KUBECONFIG=%s\n' "$KUBECONFIG_FILE"

# ===============================================================
# Phase 1: seed CLI state and run cluster-bootstrap
# ===============================================================

phase "Phase 1: cluster-bootstrap (Cilium-skipped -> Argo CD -> platform-stack)"

kubeconfig_content=$(cat "$KUBECONFIG_FILE")
seed_apprafter_state "$kubeconfig_content"
export APPRAFTER_CONFIG_DIR
printf '  APPRAFTER_CONFIG_DIR=%s\n' "$APPRAFTER_CONFIG_DIR"

bootstrap_with_retry

printf '  cluster-bootstrap complete\n'

# ---------------------------------------------------------------
# Phase 1b (FORCED): build + side-load the working-tree operator +
# admission-webhook, then apply the branch CRDs + branch RBAC.
#
# Why each piece is mandatory rather than defensive:
#   * IMAGE  — the `EnvSecretMissing` message that names the keys the Secret
#              DOES carry, and `status.envConfig.{digest,changedAt}`, are
#              2.22c; the published operator predates both, so leg 7 would
#              assert against an older message and leg 9 would have no
#              changedAt to compare against.
#   * CRDs   — applied for spec-side parity with the branch operator. NOT for
#              `status.envConfig`: the Application CRD's `status` node carries
#              `x-kubernetes-preserve-unknown-fields: true`
#              (crd-application.yaml), so an unknown status key is PRESERVED,
#              not pruned. That distinction matters — the prune failure mode
#              is real for `spec` (structural, and exactly how the 2.22g
#              `backup.timeZone` write was silently discarded), and does not
#              apply here. An earlier draft of this comment claimed it did.
#   * RBAC   — the image is the branch's but the cluster's ClusterRole is the
#              published chart's, so a verb added in the same commit as the
#              code that needs it 403s here and nowhere else (the D8 sampler
#              that read as "inert" for three battery runs).
# NOTE: the if-body is intentionally NOT indented (column-0 pipelines); `fi`
# closes it just before Phase 2.
# ---------------------------------------------------------------
if [ -n "${APPRAFTER_E2E_LOCAL_OPERATOR:-}" ]; then
phase "Phase 1b: build + load local operator + webhook (FORCED for 2.22c)"
builder=podman; command -v podman >/dev/null 2>&1 || builder=docker

build_load_restart() { # <deployment> <operator-subdir>
    local dep="$1" sub="$2" img
    printf '  waiting for the %s Deployment to appear ...\n' "$dep"
    for _ in $(seq 1 60); do
        kubectl -n "$PLATFORM_NS" get deploy "$dep" >/dev/null 2>&1 && break
        sleep 5
    done
    img=$(kubectl -n "$PLATFORM_NS" get deploy "$dep" \
        -o jsonpath='{.spec.template.spec.containers[0].image}')
    printf '  building %s from the working tree (%s) ...\n' "$img" "$builder"
    "$builder" build -f "${REPO_ROOT}/operator/${sub}/Dockerfile" \
        -t "$img" "${REPO_ROOT}/operator"
    cluster_load_image "$CLUSTER_NAME" "$img"
    kubectl -n "$PLATFORM_NS" rollout restart "deploy/${dep}"
    kubectl -n "$PLATFORM_NS" rollout status "deploy/${dep}" --timeout=180s
}
build_load_restart apprafter-operator apprafter-operator
build_load_restart admission-webhook admission-webhook

# `rollout status` returns once the NEW webhook pod is Ready, but the OLD
# (released) pod lingers Terminating for its grace period, and during that
# window an apply can still route to it. Wait until ONLY the branch webhook
# serves before any branch-typed apply.
printf '  waiting for the old (released) webhook pod to fully terminate ...\n'
_wh_deadline=$(( $(date +%s) + 90 ))
while [ "$(date +%s)" -lt "$_wh_deadline" ]; do
    # Read FIRST with `|| true`, then count: `kubectl … | wc -l` is a pipeline
    # whose left side legitimately fails while the label selector matches
    # nothing, and pipefail would take the walk down mid-poll.
    _wh_pods=$(kubectl -n "$PLATFORM_NS" get pods \
        -l app.kubernetes.io/name=admission-webhook --no-headers 2>/dev/null || true)
    if [ "$(count_lines "$_wh_pods")" -le 1 ]; then break; fi
    sleep 3
done
printf '  apprafter-operator + admission-webhook now running the working-tree build\n'

# Argo CD owns the operator CRDs via the apprafter-operator Application, so
# disable automated sync on the parent + operator apps first (else Argo reverts
# the branch CRDs straight back to the published ones).
printf '  applying branch operator CRDs (published chart predates status.envConfig) ...\n'
for _app in platform apprafter-operator; do
    kubectl -n argocd patch "$ARGO_APP" "$_app" --type=merge \
        -p '{"spec":{"syncPolicy":{"automated":null}}}' >/dev/null 2>&1 || true
done
apply_branch_operator_crds
for _crd in applications serviceproviders resourceclaims retainedclaims; do
    retry 12 5 -- kubectl wait --for=condition=Established \
        "crd/${_crd}.apprafter.io" --timeout=30s
done
printf '  branch CRDs applied + Established\n'

apply_branch_operator_rbac
fi  # end APPRAFTER_E2E_LOCAL_OPERATOR (Phase 1b)

# ===============================================================
# Phase 2: platform readiness (AppProject, webhook, sealed-secrets)
# ===============================================================

phase "Phase 2: platform readiness (apps AppProject, admission-webhook, sealed-secrets)"

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
    kubectl -n argocd get appproject.argoproj.io >&2 || true
    exit 1
}

# The admission webhook validates the AppRafter CR on CREATE — it must be
# Available before the first sync renders one.
printf '  waiting for the admission-webhook Deployment ...\n'
retry 30 10 -- kubectl -n "$PLATFORM_NS" rollout status \
    deploy admission-webhook --timeout=60s

# `apprafter secret seal` fetches the controller's public cert through the
# kube-API service proxy, so the controller must be Ready AND its Service must
# have a ready endpoint — a Deployment that is merely "rolled out" can still
# have no backing endpoint, and the cert fetch then 503s.
printf '  waiting for the sealed-secrets controller ...\n'
for _ in $(seq 1 60); do
    kubectl -n "$PLATFORM_NS" get deploy sealed-secrets-controller >/dev/null 2>&1 && break
    sleep 5
done
retry 30 10 -- kubectl -n "$PLATFORM_NS" rollout status \
    deploy sealed-secrets-controller --timeout=60s
_ep_deadline=$(( $(date +%s) + 180 ))
_ep=""
while [ "$(date +%s)" -lt "$_ep_deadline" ]; do
    _ep=$(kubectl -n "$PLATFORM_NS" get endpoints sealed-secrets-controller \
        -o jsonpath='{.subsets[0].addresses[0].ip}' 2>/dev/null || true)
    if [ -n "$_ep" ]; then break; fi
    sleep 5
done
assert_nonempty "sealed-secrets-controller has a ready endpoint" "$_ep"

# ===============================================================
# Phase 3: the app namespace, created EXPLICITLY before any seal
# ===============================================================

phase "Phase 3: create namespace ${APP_NS} (before sealing — the seal is namespaced)"

# A SealedSecret only unseals as `<namespace>/<name>` (strict scope), so the
# namespace has to exist before the seal, not when Argo CD gets around to
# creating it during the first sync.
kubectl create namespace "$APP_NS" 2>/dev/null || true
retry 12 5 -- kubectl get namespace "$APP_NS" >/dev/null
printf '  ok: namespace %s exists\n' "$APP_NS"

# ===============================================================
# Phase 4: `apprafter secret seal` into the APP namespace
# ===============================================================

phase "Phase 4: secret seal ${SECRET_NAME} -n ${APP_NS} (and NOT into ${PLATFORM_NS})"

apprafter secret seal "$SECRET_NAME" \
    --namespace "$APP_NS" \
    --from-literal "${SECRET_KEY}=${VAL_1}"

# The SealedSecret is the source of truth; the controller unseals it into a
# plain Secret of the same name. Both must exist in the APP namespace.
wait_exists sealedsecret "$APP_NS" "$SECRET_NAME" 120
wait_secret_key_present "$APP_NS" "$SECRET_NAME" "$SECRET_KEY" 120

# THE NAMESPACE IS THE POINT. `secret seal --namespace` defaults to
# apprafter-system, where platform credential material lives; a secret sealed
# there is invisible to an app that references it by bare `<name>/<key>`, and
# the sealed blob cannot be moved afterwards (it only unseals as the namespace
# it was sealed for). The negative below is what proves the walk sealed into
# the app's own namespace rather than silently taking the default.
assert_absent_now "no SealedSecret leaked into the default namespace" \
    sealedsecret "$PLATFORM_NS" "$SECRET_NAME"
assert_absent_now "no Secret leaked into the default namespace" \
    secret "$PLATFORM_NS" "$SECRET_NAME"

# ===============================================================
# Phase 5: `apprafter secret list` answers where + which keys + when
# ===============================================================

phase "Phase 5: secret list -n ${APP_NS} (D7's before-the-error half)"

list_out="${OUT_DIR}/secret-list.txt"
if ! apprafter secret list -n "$APP_NS" >"$list_out" 2>&1; then
    printf 'ERROR: `apprafter secret list -n %s` failed. Output:\n' "$APP_NS" >&2
    cat "$list_out" >&2
    exit 1
fi
cat "$list_out"

assert_file_contains "secret list names the secret" "$list_out" "$SECRET_NAME"
# ASSERT AGAINST THE ROW, NOT THE WHOLE OUTPUT. The header is built from the
# `-n` argument this walk itself passed (`Sealed secrets in namespace 'X':`),
# and the EMPTY listing prints `No sealed secrets found in namespace 'X'.` —
# which also contains the namespace. Matching the namespace anywhere in the
# file is therefore satisfied by the null case: it would pass against a cluster
# holding no sealed secrets at all.
row="$(line_containing "$list_out" "$SECRET_NAME")"
assert_nonempty "secret list has a row for ${SECRET_NAME}" "$row"
assert_contains "the row names its namespace" "$row" "$APP_NS"
# The KEY NAME, read from the SealedSecret's own encryptedData map — nothing is
# decrypted and no Secret is read for its contents. This is the half that
# answers "is it spelled differently?" BEFORE an error rather than after.
assert_contains "the row names the key it carries" "$row" "$SECRET_KEY"

# And a REAL sealed-at stamp: only a seal made by this CLI carries
# `apprafter.io/sealed-at`; anything else renders "-". Matching the row for an
# RFC3339 date proves the provenance annotation survived the round trip, which
# a bare "not empty" check would not.
if [[ "$row" =~ 20[0-9][0-9]-[0-9][0-9]-[0-9][0-9]T ]]; then
    printf '  ok: the row carries a real sealed-at timestamp (not "-"): %s\n' "$row"
else
    printf 'ERROR: the %s row shows no RFC3339 sealed-at stamp (apprafter.io/sealed-at missing?):\n%s\n' \
        "$SECRET_NAME" "$row" >&2
    exit 1
fi

# ===============================================================
# Phase 6: git daemon + `apprafter app add` (the app that consumes it)
# ===============================================================

phase "Phase 6: git daemon + apprafter app add ${APP} (env ${ENV_VAR} -> secret ${SECRET_NAME}/${SECRET_KEY})"

setup_git_server

if [ "$(cluster_runtime)" = "kind" ]; then
    GIT_REPO_URL="git://$(detect_host_gateway_ip):${GIT_DAEMON_PORT}/secrets-ux-app"
else
    GIT_REPO_URL="git://host.k3d.internal:${GIT_DAEMON_PORT}/secrets-ux-app"
fi
GIT_REPO_URL_HOST="git://127.0.0.1:${GIT_DAEMON_PORT}/secrets-ux-app"
printf '  fixture repo URL (in-cluster): %s\n' "$GIT_REPO_URL"

printf '  verifying the git daemon is up (local clone check) ...\n'
retry 6 5 -- git ls-remote "$GIT_REPO_URL_HOST" >/dev/null
printf '  git daemon is reachable\n'

# No --env: the fixture declares no `environments`, so this is the base-only
# deploy and the Argo CD Application is the bare `shop`. Every later
# `apprafter app <verb> shop` then resolves the single deployment without
# --env — the logical-name UX.
apprafter_from_fixture app add \
    "$GIT_REPO_URL" \
    --name      "$APP" \
    --branch    main \
    --path      "/" \
    --namespace "$APP_NS" \
    --project   apps \
    --no-ping \
    --no-interactive

# Poll the CR, NOT Argo health. (Argo's health Lua reports Progressing for
# every non-Ready phase, so a Synced+Healthy wait is only safe while the app is
# Ready — and here it is not Ready yet by definition.)
wait_exists "$AR_APP" "$APP_NS" "$APP" 600
wait_jsonpath "$AR_APP" "$APP_NS" "$APP" '{.status.phase}' Ready 300

kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP}" --timeout=300s
retry 40 5 -- kubectl -n "$APP_NS" wait --for=condition=Ready \
    pod -l "app.kubernetes.io/name=${APP}" --timeout=20s

POD_INITIAL="$(app_pod)"
assert_nonempty "a workload pod is Running before anything is broken" "$POD_INITIAL"
printf '  ok: workload pod %s is Running with the sealed value bound to %s\n' \
    "$POD_INITIAL" "$ENV_VAR"

# THE NEGATIVE CONTROL FOR PHASE 10, AND IT HAS TO BE TAKEN HERE.
#
# `pod_is_stale` is `startTime < envConfig.changedAt`. Right now that is FALSE
# by construction: the digest is stamped in the same reconcile that applies the
# Deployment, so `changedAt` predates this pod by the seconds it took to
# schedule. This is the only moment in the walk when the un-flagged state
# provably exists.
#
# Without this line the walk's headline D6 assertion proves nothing. The
# recovery in Phase 9 is itself a digest change, so it moves `changedAt` past
# this pod's startTime and the flag is ALREADY set before Phase 10's rotation
# runs — a `pod_is_stale` that returned true unconditionally would sail through
# a positive-only check. Two independent reviewers caught this before the walk
# was ever run; the ordering comment at the top of this file described the
# property without anything asserting it.
status_fresh="$(capture_app_status fresh)"
if ! assert_file_lacks "a pod that POSTDATES the last config change is not flagged" \
    "$status_fresh" "← old config"; then
    # Print the two numbers the verdict is computed from. `pod_is_stale` is
    # `startTime < changedAt` and nothing else, so these two lines say
    # immediately whether the CLI compared correctly or the operator stamped
    # late — and a walk that fails without them costs a 15-minute round trip
    # to learn which.
    printf '\n--- the two timestamps the flag is computed from ---\n' >&2
    printf 'pod.status.startTime      = %s\n' \
        "$(kubectl -n "$APP_NS" get pod "$POD_INITIAL" -o jsonpath='{.status.startTime}' 2>/dev/null || true)" >&2
    printf 'status.envConfig.changedAt= %s\n' \
        "$(jp "$AR_APP" "$APP_NS" "$APP" '{.status.envConfig.changedAt}')" >&2
    printf 'status.envConfig.digest   = %s\n' \
        "$(jp "$AR_APP" "$APP_NS" "$APP" '{.status.envConfig.digest}')" >&2
    printf 'pod created               = %s\n' \
        "$(kubectl -n "$APP_NS" get pod "$POD_INITIAL" -o jsonpath='{.metadata.creationTimestamp}' 2>/dev/null || true)" >&2
    # Does changedAt keep MOVING while nothing changes? If it does, the drift
    # boundary is not a boundary and every pod is permanently "old config".
    printf -- '--- is changedAt stable across 40s of idle reconciles? ---\n' >&2
    for _i in 1 2 3 4; do
        printf '  t+%02ds changedAt=%s digest=%s\n' "$(( _i * 10 ))" \
            "$(jp "$AR_APP" "$APP_NS" "$APP" '{.status.envConfig.changedAt}')" \
            "$(jp "$AR_APP" "$APP_NS" "$APP" '{.status.envConfig.digest}')" >&2
        sleep 10
    done
    exit 1
fi

# ===============================================================
# Phase 7: break the binding THROUGH A PRODUCT SURFACE
# ===============================================================

phase "Phase 7: re-seal ${SECRET_NAME} under key ${WRONG_KEY} — the reference stops resolving"

# Sealing REPLACES the object's keys; it does not merge. So re-sealing the same
# NAME with a different KEY is a real, one-command way for a person to drop the
# key an app depends on — the `stripe_api_key` / `stripe-api-key` slip that D7
# exists for, reproduced through the product rather than staged with kubectl.
#
# Deliberately NOT `kubectl delete secret`: the sealed-secrets controller
# re-creates a deleted Secret from the surviving SealedSecret within seconds,
# and the walk would lose that race.
#
# `--yes` is mandatory: without it, a non-interactive shell refuses to replace
# an existing secret (hard error, exit 1) — the D14 overwrite gate.
apprafter secret seal "$SECRET_NAME" \
    --namespace "$APP_NS" \
    --yes \
    --from-literal "${WRONG_KEY}=${VAL_BREAK}"

# Assert the break landed BEFORE asserting anything downstream: the referenced
# key must be genuinely gone from the unsealed Secret, and the new one present.
# Otherwise a timeout below could not distinguish an operator bug from a
# controller that simply had not reconciled.
wait_secret_key_absent "$APP_NS" "$SECRET_NAME" "$SECRET_KEY" 120
wait_secret_key_present "$APP_NS" "$SECRET_NAME" "$WRONG_KEY" 120

# The operator re-reads on requeue (there is no Secret watch that fires
# instantly), so budget generously.
wait_jsonpath "$AR_APP" "$APP_NS" "$APP" '{.status.phase}' EnvSecretMissing 180

# ===============================================================
# Phase 8: D7 — `apprafter app status` prints the EXPLANATION, not just the name
# ===============================================================

phase "Phase 8: D7 CLI half — app status explains EnvSecretMissing"

# D7's title is "the CLI cannot answer the question its own error asks". 2.22c
# made the operator's diagnostic good and then left it in
# `status.conditions[type=Ready].message`, where no CLI surface read it:
# `app status` printed the phase and stopped, so the reader still went to
# `kubectl get application -o yaml`. `format_not_ready_line` is the fix, and
# this phase is the only thing that watches it work.
status_broken="$(capture_app_status broken)"
cat "$status_broken"

# The NAME of the failure...
assert_file_contains "app status prints the AppRafter phase" \
    "$status_broken" "AppRafter phase: EnvSecretMissing"
# ...and, immediately under it, its EXPLANATION. Short fragments only: the
# reason line is one long line assembled from the operator's message, and
# anchoring on its shape rather than its substance would make this brittle for
# no gain.
assert_file_contains "app status names the reason" \
    "$status_broken" "EnvSecretMissing:"
assert_file_contains "app status distinguishes wrong-key from absent-Secret" \
    "$status_broken" "carries no key"
assert_file_contains "app status names the namespace, so it need not be guessed" \
    "$status_broken" "namespace \"${APP_NS}\""
assert_file_contains "app status names the Secret" \
    "$status_broken" "\"${SECRET_NAME}\""
# The half that turns a message into an ANSWER: the key the Secret DOES carry.
# Without it the reader still has to go and look.
assert_file_contains "app status lists the key the Secret actually carries" \
    "$status_broken" "carries: ${WRONG_KEY}"
# And the env var whose reference failed, so a multi-secret app knows which.
#
# Match `env <VAR> →`, not the bare name: `app status` always prints a
# `Secrets (<ns>/<app>):` bindings table listing every declared binding,
# whether or not anything is wrong, so a bare `$ENV_VAR` match would be
# satisfied by that table and pin nothing about the reason line. The arrow
# prefix is produced only by the operator's diagnostic.
assert_file_contains "the reason line names the env var whose reference failed" \
    "$status_broken" "env ${ENV_VAR} →"

printf '  ok: D7 — the phase, the cause, the namespace, the Secret and its actual keys all come from `app status`\n'

# ===============================================================
# Phase 9: recover through the same surface
# ===============================================================

phase "Phase 9: re-seal ${SECRET_NAME} under ${SECRET_KEY} again -> Ready"

apprafter secret seal "$SECRET_NAME" \
    --namespace "$APP_NS" \
    --yes \
    --from-literal "${SECRET_KEY}=${VAL_2}"

wait_secret_key_present "$APP_NS" "$SECRET_NAME" "$SECRET_KEY" 120
wait_jsonpath "$AR_APP" "$APP_NS" "$APP" '{.status.phase}' Ready 240
kubectl -n "$APP_NS" wait --for=condition=Available \
    "deployment/${APP}" --timeout=300s
retry 40 5 -- kubectl -n "$APP_NS" wait --for=condition=Ready \
    pod -l "app.kubernetes.io/name=${APP}" --timeout=20s

# Re-record: the pod from Phase 6 MAY have been replaced in the meantime (a
# node eviction, an image pull retry), and Phase 10's whole assertion is about
# a specific pod's identity. Reading it fresh here is what makes the ordering
# constraint hold by construction rather than by hope.
POD_BEFORE_ROTATION="$(app_pod)"
assert_nonempty "a workload pod is Running again after recovery" "$POD_BEFORE_ROTATION"
START_BEFORE_ROTATION="$(pod_start_time "$POD_BEFORE_ROTATION")"
assert_nonempty "the pod reports a startTime" "$START_BEFORE_ROTATION"
CHANGED_BEFORE_ROTATION="$(jp "$AR_APP" "$APP_NS" "$APP" '{.status.envConfig.changedAt}')"
assert_nonempty "status.envConfig.changedAt is recorded while Ready" "$CHANGED_BEFORE_ROTATION"

# ===============================================================
# Phase 10: D6 — the rotation is VISIBLE in `app status` without being ACTED on
# ===============================================================

phase "Phase 10: D6 CLI half — rotate the value; app status flags the pod as ← old config"

# D6's decision: an env var sourced from a Secret is resolved once at pod start
# and never re-read, so a rotated secret silently does nothing until something
# restarts the workload. The fix makes that VISIBLE and deliberately does NOT
# make it ACT — an automatic roll was rejected as a Tier-1 default, because a
# Secret is not owned by one Application and the blast radius is unknowable to
# whoever sealed it.
#
# ORDERING: the flag is `pod.startTime < status.envConfig.changedAt`. The pod
# was recorded in Phase 9 and is already Running; the rotation happens BELOW
# it, so `changedAt` is necessarily newer than that pod's start time — the
# ordering holds by construction, not by timestamp arithmetic.
apprafter secret seal "$SECRET_NAME" \
    --namespace "$APP_NS" \
    --yes \
    --from-literal "${SECRET_KEY}=${VAL_3}"

wait_secret_key_present "$APP_NS" "$SECRET_NAME" "$SECRET_KEY" 120

# The digest is over the RESOLVED values, so a new value must move it and
# `changedAt` with it. Poll: the operator notices on requeue.
printf '  waiting for status.envConfig.changedAt to move ...\n'
_rot_deadline=$(( $(date +%s) + 180 ))
CHANGED_AFTER_ROTATION="$CHANGED_BEFORE_ROTATION"
while [ "$(date +%s)" -lt "$_rot_deadline" ]; do
    CHANGED_AFTER_ROTATION="$(jp "$AR_APP" "$APP_NS" "$APP" '{.status.envConfig.changedAt}')"
    if [ -n "$CHANGED_AFTER_ROTATION" ] && \
       [ "$CHANGED_AFTER_ROTATION" != "$CHANGED_BEFORE_ROTATION" ]; then
        break
    fi
    sleep 5
done
assert_ne "status.envConfig.changedAt moved on the rotation" \
    "$CHANGED_AFTER_ROTATION" "$CHANGED_BEFORE_ROTATION"

status_stale="$(capture_app_status stale)"
cat "$status_stale"

# THE FLAG. `print_pod_summaries` renders it on any pod whose startTime
# precedes changedAt. Matched as a short fragment: the row is padded to
# computed column widths and the note carries trailing spaces, so `$` anchors
# and fixed offsets could never match.
assert_file_contains "app status flags the pre-rotation pod as running old config" \
    "$status_stale" "← old config"
assert_file_contains "the stale row is the pod that predates the rotation" \
    "$status_stale" "$POD_BEFORE_ROTATION"
# The note that explains WHY, which is the whole reason the flag is not a bare
# marker: the reader has to know that a restart is what picks the value up.
assert_file_contains "app status explains what the flag means" \
    "$status_stale" "started before this application"
assert_file_contains "app status says the pods still serve the previous values" \
    "$status_stale" "still serving the previous values"

# AND THE NEGATIVE, asserted as hard as the positive: nothing rolled. If a
# future change starts stamping the digest onto the pod template, this flips
# and this phase is what says so.
POD_AFTER_ROTATION="$(app_pod)"
assert_eq "the rotation did NOT replace the pod" \
    "$POD_AFTER_ROTATION" "$POD_BEFORE_ROTATION"
START_AFTER_ROTATION="$(pod_start_time "$POD_AFTER_ROTATION")"
assert_eq "the rotation did NOT restart the pod (same startTime)" \
    "$START_AFTER_ROTATION" "$START_BEFORE_ROTATION"

printf '  ok: D6 — the rotation is visible in `app status` and the workload was left alone\n'

# ===============================================================
# Phase 11: `apprafter secret remove` deletes BOTH objects
# ===============================================================

phase "Phase 11: secret remove ${SECRET_NAME} -n ${APP_NS} --yes"

# `--yes` is mandatory here too: without it a non-interactive shell errors
# rather than prompting into the void.
#
# The command's own "✓ Removed …" line is NOT asserted: it prints
# unconditionally after a `kubectl delete --ignore-not-found`, so it would be
# printed just as happily for a name that never existed. The apiserver is the
# only witness worth having.
apprafter secret remove "$SECRET_NAME" --namespace "$APP_NS" --yes

wait_absent sealedsecret "$APP_NS" "$SECRET_NAME" 120
# The SealedSecret is deleted first so its cascade takes the owned Secret with
# it; the Secret is then deleted explicitly to also cover a plain, un-owned
# one. Both must be gone — and with the SealedSecret gone, the controller has
# no source to re-create the Secret from, so this cannot flap back.
wait_absent secret "$APP_NS" "$SECRET_NAME" 120

# (The application now has an unresolvable reference again — expected, and
# beside the point at this stage. The walk tears the cluster down next.)

# ===============================================================
# Done — tear down on the success path
# ===============================================================

# Remove the EXIT trap so cleanup() does not fire again — the tear-down is
# owned inline here on the success path.
trap - EXIT

if [ -n "$GIT_DAEMON_PID" ] && kill -0 "$GIT_DAEMON_PID" 2>/dev/null; then
    kill "$GIT_DAEMON_PID" 2>/dev/null || true
fi
pkill -f "git[ -]daemon.*${GIT_DAEMON_PORT}" 2>/dev/null || true

if [ -z "${APPRAFTER_E2E_SKIP_DESTROY:-}" ]; then
    k3d_down "$CLUSTER_NAME" || true
else
    printf '\nAPPRAFTER_E2E_SKIP_DESTROY set — leaving cluster %s up.\n' \
        "$CLUSTER_NAME"
fi

rm -rf "$TMPDIR_WORK"

printf '\nsecrets-ux-walk GREEN in %s\n' "$(elapsed)"
printf 'Proven (CLI halves of D6 + D7): secret seal lands in the APP namespace and nowhere else -> secret list names ns + key + a real sealed-at stamp -> app add binds env %s to %s/%s -> re-sealing under another key breaks the binding -> `app status` prints the reason, "carries no key", the namespace and the keys the Secret DOES carry (D7) -> recover -> a rotation shows `← old config` on the pre-rotation pod WITHOUT replacing it (D6) -> secret remove deletes SealedSecret + Secret\n' \
    "$ENV_VAR" "$SECRET_NAME" "$SECRET_KEY"
