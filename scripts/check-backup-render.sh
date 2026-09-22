#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-backup-render.sh — render the platform-stack chart with scheduled
# backup switched on, and assert that both backup CronJobs carry a Job
# deadline, and that the pods under them stop cleanly when it passes.
#
# ## Why
#
# Both CronJobs are `concurrencyPolicy: Forbid`: the controller never starts a
# run while the previous one is still active. A run that never ends therefore
# suppresses every later backup, and nothing fails — the Job just stays
# `Running`. `jobTemplate.spec.activeDeadlineSeconds` is the bound that holds
# whatever the run is stuck on (a table lock, a silent socket, a webhook that
# never answers): Kubernetes stops the Job and fails it with reason
# `DeadlineExceeded`, and the next slot runs.
#
# No other gate would notice it gone. `helm lint` and the tier smoke render the
# DEFAULT values, where backup is off and neither CronJob exists; `cue vet`
# checks the values, not the template that reads them.
#
# At the deadline Kubernetes sends each Job's PID 1 SIGTERM. The runner turns
# that into a recorded failure (lastFailure, the failure webhook, its helper
# pods deleted), which needs three things from the render, none of which any
# other gate looks at: the deadline in its env, the grace period that gives it
# time, and — for the check, which is restic under a shell — `exec`, so the
# signal reaches restic and it removes its exclusive repository lock (busybox
# sh execs the last command of `-c` on its own today; that is one shell's
# optimisation, and the explicit `exec` does not depend on it).
#
# Asserts, against a freshly rendered chart:
#
#   1. backup enabled, defaults: both CronJobs are Forbid and carry
#      activeDeadlineSeconds 21600 (six hours); the runner's
#      APPRAFTER_BACKUP_DEADLINE_SECONDS says the same; the runner pod has a
#      90 s termination grace period; the check execs restic.
#   2. backup enabled, both knobs set: each CronJob carries its own value, and
#      the runner's env follows the backup one.
#   3. a deadline under ten minutes is refused by values.schema.json.
#   4. the chart's default deadline is the one the runner and the CLI fall back
#      to (backup-core's DEFAULT_RUN_DEADLINE).
#
# Usage: bash scripts/check-backup-render.sh
# Exit 0 = every assertion held.
#
# Runs in `just lint` and in platform-stack-check.yml, NOT in the parallel
# pre-commit hook: it re-renders `platform-stack/dist` (a stale render would
# answer for the old template), and `rm -rf dist` under a sibling job that is
# reading the chart — check-component-enablement.sh — would fail that job at
# random.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Resolved through scripts/cue, the single resolver for this repo's pinned
# cue; see scripts/cue.
CUE_CMD=("$REPO_ROOT/scripts/cue")
command -v helm >/dev/null 2>&1 || {
    echo "::error::helm is not on PATH — run under \`nix develop\`" >&2
    exit 2
}

fail() {
    echo "::error::$*" >&2
    echo "FAIL: $*" >&2
    exit 1
}

# stderr kept apart from the value: a dirty tree makes cue print a warning
# there, and folding it in would turn the version into that warning.
cue_err="$(mktemp)"
workdir="$(mktemp -d)"
trap 'rm -rf "$workdir" "$cue_err"' EXIT
version="$("${CUE_CMD[@]}" export ./platform-stack/cue/... \
    -e currentVersion --out text 2>"$cue_err")" || {
    cat "$cue_err" >&2
    fail "could not read currentVersion"
}
[[ -n "$version" ]] || fail "currentVersion is empty"
chart="platform-stack/dist/platform-stack-${version}"
# Always re-rendered: a dist/ left over from before a template edit would
# answer for the old template.
make -C platform-stack render-only >/dev/null
[[ -d "$chart" ]] || fail "no rendered chart at $chart"

enabled="$workdir/enabled.yaml"
cat >"$enabled" <<'EOF'
backup:
  enabled: true
  bucket: s3:https://objects.example.com/backups
  credentialRef:
    name: apprafter-backup-s3
EOF

# `$1` rendered manifests, `$2` CronJob name: that CronJob's document alone.
# Documents are split on the `---` lines helm writes between them.
cronjob_doc() {
    awk -v want="$2" '
        /^---$/ { if (kind && name == want) printf "%s", doc; doc = ""; kind = 0; name = ""; next }
        { doc = doc $0 "\n" }
        /^kind: CronJob$/ { kind = 1 }
        /^  name: / { if (name == "") name = $2 }
        END { if (kind && name == want) printf "%s", doc }
    ' "$1"
}

# `$1` label, `$2` rendered manifests, `$3` CronJob name, `$4` expected
# deadline in seconds.
assert_cronjob() {
    local label="$1" rendered="$2" name="$3" want="$4" doc
    doc="$(cronjob_doc "$rendered" "$name")"
    [[ -n "$doc" ]] || fail "$label: no CronJob '$name' in the render"
    grep -qE '^  concurrencyPolicy: Forbid$' <<<"$doc" \
        || fail "$label: CronJob '$name' is not concurrencyPolicy: Forbid"
    # Exactly under jobTemplate.spec (six spaces): a deadline on the CronJob
    # spec or the pod spec is a different field with a different meaning.
    local got
    got="$(awk '/^  jobTemplate:$/{j=1} j&&/^    spec:$/{s=1; next} s&&/^      activeDeadlineSeconds: /{print $2; exit}' <<<"$doc")"
    [[ "$got" == "$want" ]] \
        || fail "$label: CronJob '$name' jobTemplate.spec.activeDeadlineSeconds is '${got:-absent}', want $want"
    echo "  ok: $label — $name: Forbid, activeDeadlineSeconds $want"
}

# `$1` label, `$2` rendered manifests, `$3` expected deadline in seconds: the
# runner container of the backup CronJob carries it as
# APPRAFTER_BACKUP_DEADLINE_SECONDS, and its pod has the grace period the
# runner's SIGTERM handling is sized for.
assert_runner_stops_cleanly() {
    local label="$1" rendered="$2" want="$3" doc got
    doc="$(cronjob_doc "$rendered" apprafter-backup)"
    got="$(awk '/^            - name: APPRAFTER_BACKUP_DEADLINE_SECONDS$/{f=1; next}
                f && /^              value: /{print $2; exit}' <<<"$doc")"
    [[ "$got" == "\"$want\"" ]] \
        || fail "$label: runner env APPRAFTER_BACKUP_DEADLINE_SECONDS is '${got:-absent}', want \"$want\" (the Job's activeDeadlineSeconds)"
    grep -qE '^          terminationGracePeriodSeconds: 90$' <<<"$doc" \
        || fail "$label: the runner pod has no 90 s terminationGracePeriodSeconds; at the deadline it is SIGKILLed before it records the failure"
    echo "  ok: $label — runner env deadline $want, 90 s termination grace"
}

# `$1` label, `$2` rendered manifests: the check's restic is PID 1.
assert_check_execs_restic() {
    local label="$1" rendered="$2" doc
    doc="$(cronjob_doc "$rendered" apprafter-backup-check)"
    grep -qE '^ +exec restic -r "\$APPRAFTER_BACKUP_REPO" check' <<<"$doc" \
        || fail "$label: the check does not exec restic; SIGTERM at the deadline would depend on the shell to reach restic, which must remove its exclusive lock"
    echo "  ok: $label — the check execs restic"
}

echo "==> backup CronJob deadlines, chart $version"

# 1. Defaults.
helm template platform "$chart" --values "$enabled" >"$workdir/defaults.yaml"
assert_cronjob "defaults" "$workdir/defaults.yaml" apprafter-backup 21600
assert_cronjob "defaults" "$workdir/defaults.yaml" apprafter-backup-check 21600
assert_runner_stops_cleanly "defaults" "$workdir/defaults.yaml" 21600
assert_check_execs_restic "defaults" "$workdir/defaults.yaml"

# 2. Both knobs set, to values that tell them apart.
helm template platform "$chart" --values "$enabled" \
    --set backup.activeDeadlineSeconds=2700 \
    --set backup.checkActiveDeadlineSeconds=43200 \
    >"$workdir/set.yaml"
assert_cronjob "knobs set" "$workdir/set.yaml" apprafter-backup 2700
assert_cronjob "knobs set" "$workdir/set.yaml" apprafter-backup-check 43200
assert_runner_stops_cleanly "knobs set" "$workdir/set.yaml" 2700

# 3. The ten-minute floor: a unit mistake is refused at install, not shipped
#    as a deadline that stops every run.
for key in activeDeadlineSeconds checkActiveDeadlineSeconds; do
    if helm template platform "$chart" --values "$enabled" \
        --set "backup.${key}=360" >/dev/null 2>"$workdir/err"; then
        fail "backup.${key}=360 rendered; values.schema.json must refuse a deadline under 600 s"
    fi
    grep -q "$key" "$workdir/err" \
        || fail "backup.${key}=360 failed for a reason that does not name the key: $(cat "$workdir/err")"
    echo "  ok: backup.${key}=360 is refused by the values schema"
done

# 4. The default the chart renders is the default the runner (no env) and the
#    CLI (no spec.backup.activeDeadlineSeconds) keep their helper pods alive
#    for. Two literals in two languages; this is what keeps them one number.
rust_default="$(sed -nE 's/^pub const DEFAULT_RUN_DEADLINE: Duration = Duration::from_secs\(([0-9]+)\);$/\1/p' \
    cli/backup-core/src/helper_pod.rs)"
[[ "$rust_default" == "21600" ]] \
    || fail "backup-core DEFAULT_RUN_DEADLINE is '${rust_default:-not found}', but the chart defaults the Job deadline to 21600"
echo "  ok: backup-core's DEFAULT_RUN_DEADLINE is the chart's 21600"

echo "PASS: both backup CronJobs carry a Job deadline, and stop cleanly at it."
