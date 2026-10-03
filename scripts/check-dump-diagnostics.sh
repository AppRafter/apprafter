#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-dump-diagnostics.sh — a failing walk's dump keeps the operator and
# webhook logs of the WHOLE walk.
#
# ## Why
#
# A failing walk is read once, from its job log, after its cluster is gone.
# dump_diagnostics (e2e/lib.sh) used to print the last 120 lines of each
# control-plane deployment. On run 37001547817 (needs-redis nightly,
# 2026-10-02) those 120 lines began 30 seconds after the stalled Application
# was created: steady-state requeue lines had pushed every line about it out
# of the window, so the dump could not say which object hung. Nothing else
# exercises the function until a walk fails, which is the worst time to learn
# that it no longer works.
#
# It runs dump_diagnostics against a stub kubectl, under the walks' own
# `set -euo pipefail`, and fails when:
#   - a control-plane log is read with a tail instead of --since-time the
#     walk start (or, when START_NS is unusable, instead of the whole log);
#   - the console shows more than APPRAFTER_E2E_DIAG_CONSOLE_LINES of a log,
#     or fewer, or does not say how many lines it left out;
#   - the previous instance of a restarted container is not collected, or
#     one is asked for a container that never restarted;
#   - an ANSI colour code reaches the console or a file;
#   - a call with APPRAFTER_E2E_DIAG_DIR set does not write its own fresh
#     subdirectory holding the WHOLE logs, every event and the objects, or
#     a call without it leaves a scratch file behind;
#   - an apiserver that answers nothing makes the function fail: most walks
#     call it bare from their EXIT trap under errexit, where a failure would
#     skip the teardown;
#   - a reconcile-deadline hit that the console tail cuts is not printed
#     first, or lib.sh greps for a text that is no longer the one
#     operator_core::deadline::ReconcileTimedOut displays
#     (operator/operator-core/src/deadline.rs).
#
# No cluster is reached: kubectl and helm are stubs at the front of PATH, the
# check refuses to run if `command -v` resolves either anywhere else, and
# KUBECONFIG names a file that does not exist.
#
# CHECK_DUMP_DIAGNOSTICS_LIB=<file> runs the checks against another copy of
# lib.sh — how the guard is mutation-tested (copy lib.sh, revert one fix,
# watch this fail). CHECK_DUMP_DIAGNOSTICS_DEADLINE_RS=<file> does the same
# for operator/operator-core/src/deadline.rs.
#
# Usage: bash scripts/check-dump-diagnostics.sh
# Exit 0 = every check passed. Offline, a few seconds, bash + coreutils only.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
LIB="${CHECK_DUMP_DIAGNOSTICS_LIB:-$REPO_ROOT/e2e/lib.sh}"
[ -f "$LIB" ] || { echo "ERROR: no lib.sh at $LIB" >&2; exit 2; }

WORK="$(mktemp -d -t check-dump-diagnostics.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
STUB_BIN="$WORK/bin"
mkdir -p "$STUB_BIN"

# The walk "started" at 11:30:00Z; the window opens 60s earlier.
WALK_START_S="$(date -u -d 2026-10-02T11:30:00Z +%s)"
WANT_WINDOW='--since-time=2026-10-02T11:29:00Z'
OP=apprafter-operator-585cd767f7-8w67p
WH=admission-webhook-5d55ccbd54-zsmj8

# A stand-in apiserver: it records every call and answers like a kind cluster
# whose operator restarted once and logged 3000 coloured lines this walk. It
# honours --tail, so a dump that asks for a tail gets one.
cat >"$STUB_BIN/kubectl" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$STUB_CALLS"
if [ "${STUB_MODE:-}" = dead ]; then
    echo 'The connection to the server 127.0.0.1:6443 was refused' >&2
    exit 1
fi
OP=apprafter-operator-585cd767f7-8w67p
WH=admission-webhook-5d55ccbd54-zsmj8
prev=false tail=-1 args=()
while [ $# -gt 0 ]; do
    case "$1" in
        -n | -c) shift ;;
        --previous | --previous=true) prev=true ;;
        --previous=false) prev=false ;;
        --tail=*) tail="${1#--tail=}" ;;
        *) args+=("$1") ;;
    esac
    shift
done
out="$(mktemp)"
trap 'rm -f "$out"' EXIT
rc=0
esc=$'\033'
case "${args[*]}" in
    'config current-context') echo kind-apprafter-redis-walk >"$out" ;;
    'get deploy apprafter-operator' | 'get deploy admission-webhook') ;;
    'get deploy apprafter-operator -o go-template='*)
        printf 'app.kubernetes.io/instance=apprafter-operator,app.kubernetes.io/name=apprafter-operator,' >"$out" ;;
    'get deploy admission-webhook -o go-template='*)
        printf 'app.kubernetes.io/name=admission-webhook,' >"$out" ;;
    'get deploy '*) echo 'Error from server (NotFound): deployments.apps not found' >&2; rc=1 ;;
    'get pods -l app.kubernetes.io/instance=apprafter-operator,app.kubernetes.io/name=apprafter-operator -o jsonpath='*)
        printf '%s\n' "$OP" >"$out" ;;
    'get pods -l app.kubernetes.io/name=admission-webhook -o jsonpath='*)
        printf '%s\n' "$WH" >"$out" ;;
    "get pod $OP -o jsonpath="*) printf 'operator 1\n' >"$out" ;;
    "get pod $WH -o jsonpath="*) printf 'admission-webhook 0\n' >"$out" ;;
    'get pods -A --no-headers')
        printf 'apprafter-system  %s  1/1  Running  1  4m\ncnpg-system  pg-1  0/1  CrashLoopBackOff  5  10m\n' "$OP" >"$out" ;;
    "logs $OP"* | 'logs deploy/apprafter-operator'*)
        if [ "$prev" = true ]; then
            printf '%s[31mERROR%s[0m panicked: the previous operator instance\n' "$esc" "$esc" >"$out"
        else
            {
                printf '%s[2m2026-10-02T11:58:45Z%s[0m %s[32m INFO%s[0m FIRST-LINE-OF-THE-WALK\n' "$esc" "$esc" "$esc" "$esc"
                for i in $(seq 2 2999); do
                    # Two deadline WARNs, far outside the console's 2000-line
                    # tail: an error_policy line inside kube-runtime's span,
                    # and a run-stream line (the only one that carries the
                    # text for SourceCredential, whose error_policy omits it).
                    if [ "$i" -eq 400 ] && [ "${STUB_DEADLINE_HIT:-1}" = 1 ]; then
                        printf '%s[2m2026-10-02T11:59:00Z%s[0m %s[33m WARN%s[0m reconciling object{object.ref=Application.v1alpha1.apprafter.io/web.demo object.reason=object updated}: operator_controllers_application: reconcile error name=web namespace=demo err=reconcile did not finish within 120s\n' \
                            "$esc" "$esc" "$esc" "$esc"
                        continue
                    fi
                    if [ "$i" -eq 700 ] && [ "${STUB_DEADLINE_HIT:-1}" = 1 ]; then
                        printf '%s[2m2026-10-02T11:59:00Z%s[0m %s[33m WARN%s[0m operator_controllers_sourcecredential: sourcecredential step error err=reconciler for object SourceCredential.v1alpha1.apprafter.io/repo.demo failed: reconcile did not finish within 90s\n' \
                            "$esc" "$esc" "$esc" "$esc"
                        continue
                    fi
                    printf '%s[2m2026-10-02T11:59:00Z%s[0m %s[32m INFO%s[0m reconciling object{object.ref=PlatformStack.v1alpha1.apprafter.io/default.apprafter-system}: steady-state line %d\n' \
                        "$esc" "$esc" "$esc" "$esc" "$i"
                done
                printf '%s[2m2026-10-02T12:03:02Z%s[0m %s[32m INFO%s[0m LAST-LINE-OF-THE-WALK\n' "$esc" "$esc" "$esc" "$esc"
            } >"$out"
        fi ;;
    "logs $WH"* | 'logs deploy/admission-webhook'*)
        if [ "$prev" = true ]; then
            echo "Error from server (BadRequest): previous terminated container \"admission-webhook\" in pod \"$WH\" not found" >&2
            rc=1
        else
            printf 'webhook listening on :8443\nwebhook ready\n' >"$out"
        fi ;;
    'logs pg-1'*) printf 'pg-1: database system is shut down\n' >"$out" ;;
    'get events -A --sort-by=.lastTimestamp')
        for i in $(seq 1 100); do printf 'demo  %ds  Normal  Event-%d  pod/web  message\n' "$i" "$i"; done >"$out" ;;
    'api-resources --api-group=apprafter.io -o name')
        printf 'applications.apprafter.io\nresourceclaims.apprafter.io\n' >"$out" ;;
    'get applications.apprafter.io -A -o yaml')
        printf 'apiVersion: v1\nitems:\n- kind: Application\n  metadata:\n    name: web\n    namespace: demo\nkind: List\n' >"$out" ;;
esac
if [ "$tail" -ge 0 ]; then tail -n "$tail" "$out"; else cat "$out"; fi
exit "$rc"
STUB
printf '#!/usr/bin/env bash\nexit 0\n' >"$STUB_BIN/helm"
chmod +x "$STUB_BIN/kubectl" "$STUB_BIN/helm"

fails=0
fail() { printf 'FAIL: %s\n' "$*" >&2; fails=$((fails + 1)); }
pass() { printf 'ok: %s\n' "$*"; }
# check <description> <command...> — passes when the command succeeds.
check() { local d="$1"; shift; if "$@"; then pass "$d"; else fail "$d"; fi; }
# refute <description> <command...> — passes when the command fails.
refute() { local d="$1"; shift; if "$@"; then fail "$d"; else pass "$d"; fi; }

# run_dump <name> [VAR=value ...] — one dump_diagnostics call in a fresh
# shell with the walks' options. Console in $WORK/<name>.console, kubectl
# calls in $WORK/<name>.calls, exit code in $WORK/<name>.rc.
run_dump() {
    local name="$1" rc=0
    shift
    mkdir -p "$WORK/$name.tmp"
    : >"$WORK/$name.calls"
    # shellcheck disable=SC2016  # expanded by the inner bash, on purpose
    env -u APPRAFTER_E2E_DIAG_DIR -u APPRAFTER_E2E_DIAG_CONSOLE_LINES \
        PATH="$STUB_BIN:$PATH" KUBECONFIG=/nonexistent/kubeconfig \
        STUB_CALLS="$WORK/$name.calls" TMPDIR="$WORK/$name.tmp" \
        START_NS="${WALK_START_S}000000000" LIB="$LIB" STUB_BIN="$STUB_BIN" \
        "$@" bash -c '
            set -euo pipefail
            # shellcheck source=e2e/lib.sh
            source "$LIB"
            [ "$(command -v kubectl)" = "$STUB_BIN/kubectl" ] || { echo "kubectl is not the stub" >&2; exit 97; }
            [ "$(command -v helm)" = "$STUB_BIN/helm" ] || { echo "helm is not the stub" >&2; exit 97; }
            dump_diagnostics
        ' 2>"$WORK/$name.console" || rc=$?
    echo "$rc" >"$WORK/$name.rc"
    if [ "$rc" -eq 97 ]; then
        echo "ERROR: refusing to go on, the stubs are not first on PATH:" >&2
        cat "$WORK/$name.console" >&2
        exit 2
    fi
}

# The predicates below never put `grep -q` at the end of a pipe: under
# pipefail an early exit there SIGPIPEs the producer and a match reads as a
# miss. They capture first and test the capture.
has() { grep -qF -- "$2" "$1"; }
has_re() { grep -qE -- "$2" "$1"; }
rc_is() { [ "$(cat "$WORK/$1.rc")" = "$2" ]; }
# control_plane_reads <calls> — every `kubectl logs` of an operator or webhook pod.
control_plane_reads() { grep -E -- "^-n apprafter-system logs ($OP|$WH|deploy/)" "$1" || true; }
all_reads_carry() { # <calls> <flag>
    local reads
    reads="$(control_plane_reads "$1")"
    [ -n "$reads" ] && [ -z "$(printf '%s\n' "$reads" | grep -vF -- "$2" || true)" ]
}
some_read_tails() { control_plane_reads "$1" | grep -E -- '--tail=[0-9]' >/dev/null; }
count_is() { [ "$(grep -cE -- "$2" "$1" || true)" = "$3" ]; } # <file> <regex> <n>
no_escape() { ! grep -q $'\033' "$1"; }
dir_empty() { [ -z "$(find "$1" -mindepth 1 -print -quit)" ]; }

# ---- A: a normal failing walk ------------------------------------------------
run_dump a
C="$WORK/a.console" K="$WORK/a.calls"
check "A: exits 0 under set -euo pipefail" rc_is a 0
check "A: no ANSI escape reaches the console" no_escape "$C"
check "A: every control-plane log is read $WANT_WINDOW" all_reads_carry "$K" "$WANT_WINDOW"
check "A: the operator log is read by pod, not by deployment" has_re "$K" "^-n apprafter-system logs $OP -c operator "
refute "A: no control-plane log is read with a tail" some_read_tails "$K"
check "A: the restarted operator's previous instance is read exactly once" \
    count_is "$K" "^-n apprafter-system logs $OP -c operator --previous=true " 1
refute "A: no previous instance asked of the webhook, which never restarted" \
    has_re "$K" "^-n apprafter-system logs $WH .*--previous=true"
check "A: the previous instance is on the console" has "$C" 'panicked: the previous operator instance'
check "A: ... under a PREVIOUS header" has "$C" "[operator] PREVIOUS instance (restarted 1x)"
check "A: the console ends the operator log at its last line" has "$C" 'LAST-LINE-OF-THE-WALK'
check "A: the console starts the operator log 2000 lines from its end" has_re "$C" 'steady-state line 1001$'
refute "A: no more than 2000 operator lines on the console" has_re "$C" 'steady-state line 1000$'
check "A: the console says what it left out" \
    has "$C" '... 1000 earlier line(s) omitted here; set APPRAFTER_E2E_DIAG_DIR to keep the whole log'
check "A: the webhook log is on the console" has "$C" 'webhook ready'
check "A: no scratch file is left behind" dir_empty "$WORK/a.tmp"

# ---- B: APPRAFTER_E2E_DIAG_DIR, called twice (two clusters, two runs) -------
DIAG="$WORK/diag"
run_dump b1 APPRAFTER_E2E_DIAG_DIR="$DIAG"
run_dump b2 APPRAFTER_E2E_DIAG_DIR="$DIAG"
mapfile -t subdirs < <(find "$DIAG" -mindepth 1 -maxdepth 1 -type d -name 'kind-apprafter-redis-walk-*' | sort)
check "B: each call writes its own <context>-<time>-XXXXXX subdirectory" test "${#subdirs[@]}" -eq 2
B="${subdirs[0]:-$DIAG/missing}"
OPLOG="$B/apprafter-system/$OP.operator.log"
check "B: exits 0" rc_is b1 0
check "B: the whole operator log is in the artifact (3000 lines)" count_is "$OPLOG" '.' 3000
check "B: ... from the walk's first line" has "$OPLOG" 'FIRST-LINE-OF-THE-WALK'
check "B: ... ANSI-free" no_escape "$OPLOG"
check "B: the restarted operator's previous instance is in the artifact" \
    has "$B/apprafter-system/$OP.operator.previous.log" 'panicked: the previous operator instance'
check "B: the webhook log is in the artifact" has "$B/apprafter-system/$WH.admission-webhook.log" 'webhook ready'
refute "B: no previous-instance file for the webhook, which never restarted" \
    test -e "$B/apprafter-system/$WH.admission-webhook.previous.log"
check "B: the console points at the whole log in the artifact" \
    has "$WORK/b1.console" "... 1000 earlier line(s) omitted here; the whole log is apprafter-system/$OP.operator.log in the e2e-diagnostics artifact"
check "B: every event is in the artifact (100 lines)" count_is "$B/events.txt" 'Event-' 100
check "B: the not-Ready pod's whole log is in the artifact" has "$B/pods/cnpg-system/pg-1.log" 'database system is shut down'
check "B: every apprafter.io kind is dumped as YAML" has "$B/objects/applications.apprafter.io.yaml" 'name: web'
check "B: ... resourceclaims too" test -e "$B/objects/resourceclaims.apprafter.io.yaml"
check "B: ... and the Argo CD Applications" test -e "$B/objects/applications.argoproj.io.yaml"
check "B: context.txt records the log window" has "$B/context.txt" "log window: $WANT_WINDOW"
check "B: no scratch file is left behind outside the artifact" dir_empty "$WORK/b1.tmp"

# ---- F: reconcile-deadline hits come first ------------------------------------
# deadline_section <console> — the lines between the deadline header and the
# first per-pod log header.
deadline_section() {
    awk '/^--- reconcile deadline hits \(whole walk\) ---$/ {on = 1; next}
         /^=== logs apprafter-system\// {on = 0}
         on' "$1"
}
section_has() { deadline_section "$1" | grep -F -- "$2" >/dev/null; }
check "F: a deadline hit 2600 lines back is printed before the per-pod logs" \
    section_has "$WORK/a.console" 'object.ref=Application.v1alpha1.apprafter.io/web.demo'
check "F: ... with the deadline text" section_has "$WORK/a.console" 'err=reconcile did not finish within 120s'
check "F: the run-stream hit, SourceCredential's only one, is printed there too" \
    section_has "$WORK/a.console" 'err=reconciler for object SourceCredential.v1alpha1.apprafter.io/repo.demo failed: reconcile did not finish within 90s'
DEADLINE_TEXT="$(bash -c 'source "$1" >/dev/null 2>&1; printf %s "${_DIAG_DEADLINE_TEXT:-}"' _ "$LIB")"
DEADLINE_RS="${CHECK_DUMP_DIAGNOSTICS_DEADLINE_RS:-$REPO_ROOT/operator/operator-core/src/deadline.rs}"
deadline_text_pinned() {
    [ -n "$DEADLINE_TEXT" ] && [ -f "$DEADLINE_RS" ] \
        && grep -F -- "#[error(\"${DEADLINE_TEXT} {}s\"" "$DEADLINE_RS" >/dev/null
}
check "F: lib.sh greps for exactly what ReconcileTimedOut displays (${DEADLINE_RS#"$REPO_ROOT"/})" \
    deadline_text_pinned
run_dump f STUB_DEADLINE_HIT=0
check "F: no hit prints (none)" section_has "$WORK/f.console" '(none)'

# ---- C: START_NS unusable -> the whole log, never a tail ---------------------
run_dump c START_NS=not-a-number
check "C: with START_NS unusable every control-plane log is read whole" all_reads_carry "$WORK/c.calls" '--tail=-1'
refute "C: no --since-time from an unusable START_NS" has "$WORK/c.calls" '--since-time='

# ---- D: the apiserver answers nothing ----------------------------------------
run_dump d STUB_MODE=dead
check "D: a dead apiserver still exits 0" rc_is d 0
check "D: ... and the dump reaches its end" has "$WORK/d.console" '----- end diagnostics -----'

# ---- E: APPRAFTER_E2E_DIAG_CONSOLE_LINES ---------------------------------------
run_dump e APPRAFTER_E2E_DIAG_CONSOLE_LINES=50
check "E: APPRAFTER_E2E_DIAG_CONSOLE_LINES=50 says 2950 lines were left out" \
    has "$WORK/e.console" '... 2950 earlier line(s) omitted here'
check "E: ... and shows the last 50 operator lines" count_is "$WORK/e.console" 'steady-state line|LAST-LINE-OF-THE-WALK' 50

if [ "$fails" -ne 0 ]; then
    for f in "$WORK"/*.console; do
        printf '\n--- %s (last 30 lines) ---\n' "${f##*/}" >&2
        tail -n 30 "$f" >&2
    done
    printf '\nFAILED: %d check(s); see FAIL lines above.\n' "$fails" >&2
    exit 1
fi
echo "OK: dump_diagnostics keeps the whole walk's control-plane logs"
