#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-backup-render.sh — render the platform-stack chart with scheduled
# backup switched on, and assert that both backup CronJobs carry a Job
# deadline, that the pods under them stop cleanly when it passes, and that
# they carry the resources and restic settings the runner was measured with.
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
# pods deleted; for the check Job, lastCheck or the prune it was in), which
# needs two things from the render, neither of which any other gate looks at:
# the deadline in its env and the grace period that gives it time. Both Jobs
# run the runner binary since WI-389: the check Job as `apprafter-backup
# check`, with no shell in between, so the signal reaches the runner, which
# passes it on to restic so that restic removes its exclusive lock.
#
# Asserts, against a freshly rendered chart:
#
#   1. backup enabled, defaults: both CronJobs are Forbid and carry
#      activeDeadlineSeconds 21600 (six hours); each runner's
#      APPRAFTER_BACKUP_DEADLINE_SECONDS says the same as its own Job; both
#      pods have a 90 s termination grace period; the check Job runs the
#      runner as `check`.
#   2. backup enabled, both knobs set: each CronJob carries its own value, and
#      the runner's env follows the backup one.
#   2b. both runners carry the BACKUP Job's deadline as
#      APPRAFTER_BACKUP_RUN_DEADLINE_SECONDS — in the check Job too, where
#      APPRAFTER_BACKUP_DEADLINE_SECONDS is the check's own. The prune after
#      the check waits it out before it sweeps a run with no manifest: a
#      backup still dumping when the check starts holds no restic lock, so
#      the check passes beside it, and with the check's deadline in its place
#      the prune would delete that backup's claim snapshots under it.
#   3. a deadline under ten minutes is refused by values.schema.json.
#   4. the chart's default deadline is the one the runner and the CLI fall back
#      to (backup-core's DEFAULT_RUN_DEADLINE).
#   5. both CronJobs request the memory the runner was measured to need and
#      carry the limit sized from the same measurement, with no CPU limit
#      (WI-386): 128Mi requested, 384Mi limit, 100m CPU requested.
#   6. both CronJobs give restic the settings those numbers were measured
#      with: GOMAXPROCS "2" and GOMEMLIMIT "96MiB". Without GOMAXPROCS restic
#      sizes its concurrency by the node's CPU count, and a 32-CPU node took a
#      first backup to 695 MiB. Both slow restic's progress output to one line
#      a minute (the runner holds all of restic's output in memory until
#      restic exits, and copies what check and prune print into the log) and
#      give restic a cache directory it can write (HOME is / for the image's
#      user). Both keep what restic writes to disk on the staging volume:
#      TMPDIR is its mountPath and restic's cache is under it, so
#      stagingSizeLimit bounds the backup's dumps and both Jobs' cache and
#      temporary files, which in /tmp no limit counted.
#   7. in both CronJobs, the staging volume's sizeLimit and the limit the
#      runner checks the volume against (APPRAFTER_BACKUP_STAGING_SIZE_LIMIT)
#      are one value, defaulted and set.
#   8. retention (WI-389): both CronJobs get APPRAFTER_BACKUP_ENFORCE, `check`
#      when nothing sets it (the check Job prunes after a passing check) and
#      an explicit `operator` kept as it is, with the keep counts on both;
#      the check Job gets its depth from checkReadData / checkReadDataSubset
#      and the cluster label for the failure webhook; the values schema
#      refuses a mode that does not exist.
#   9. every APPRAFTER_* variable either CronJob renders is one the runner
#      reads: the chart and the runner spell each name in a different
#      language, and a name misspelt on either side is a setting that reaches
#      nothing — the runner falls back to its default without a word.
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

# yq reads the rendered containers: an env entry is found by its name, which
# a line-oriented read of the YAML cannot do reliably. The same resolution as
# check-component-claim-templates.sh; CI installs yq before this step.
if command -v yq >/dev/null 2>&1; then
    YQ=(yq)
else
    YQ=(nix run nixpkgs#yq-go --)
fi

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

# `$1` rendered manifests, `$2` CronJob name, `$3` a yq path under that
# CronJob's first container: the value there, or empty when it is absent.
container_value() {
    NAME="$2" "${YQ[@]}" -r "select(.kind == \"CronJob\" and .metadata.name == strenv(NAME))
        | .spec.jobTemplate.spec.template.spec.containers[0]$3 // \"\"" "$1"
}

# `$1` rendered manifests, `$2` CronJob name, `$3` env var: its literal value,
# or empty when the container does not carry it.
env_value() {
    VAR="$3" container_value "$1" "$2" '.env[] | select(.name == strenv(VAR)) | .value'
}

# `$1` label, `$2` rendered manifests, `$3` CronJob name, `$4` expected
# deadline in seconds: that CronJob's runner container carries it as
# APPRAFTER_BACKUP_DEADLINE_SECONDS, and its pod has the grace period the
# runner's SIGTERM handling is sized for.
assert_runner_stops_cleanly() {
    local label="$1" rendered="$2" name="$3" want="$4" doc got
    doc="$(cronjob_doc "$rendered" "$name")"
    got="$(awk '/^            - name: APPRAFTER_BACKUP_DEADLINE_SECONDS$/{f=1; next}
                f && /^              value: /{print $2; exit}' <<<"$doc")"
    [[ "$got" == "\"$want\"" ]] \
        || fail "$label: $name runner env APPRAFTER_BACKUP_DEADLINE_SECONDS is '${got:-absent}', want \"$want\" (the Job's activeDeadlineSeconds)"
    grep -qE '^          terminationGracePeriodSeconds: 90$' <<<"$doc" \
        || fail "$label: the $name pod has no 90 s terminationGracePeriodSeconds; at the deadline it is SIGKILLed before it records the failure"
    echo "  ok: $label — $name: runner env deadline $want, 90 s termination grace"
}

# `$1` label, `$2` rendered manifests, `$3` the backup Job's deadline in
# seconds: both runners carry it as APPRAFTER_BACKUP_RUN_DEADLINE_SECONDS.
assert_prune_waits_out_the_backup_deadline() {
    local label="$1" rendered="$2" want="$3" name got
    for name in apprafter-backup apprafter-backup-check; do
        got="$(env_value "$rendered" "$name" APPRAFTER_BACKUP_RUN_DEADLINE_SECONDS)"
        [[ "$got" == "$want" ]] \
            || fail "$label: $name runner env APPRAFTER_BACKUP_RUN_DEADLINE_SECONDS is '${got:-absent}', want '$want' (the BACKUP Job's activeDeadlineSeconds, which a prune waits out before it sweeps a run a backup may still be writing)"
    done
    echo "  ok: $label — both runners' prune waits out the backup deadline, $want"
}

# `$1` label, `$2` rendered manifests: the check Job runs the runner binary
# (the image's entrypoint) in its check mode, with no shell in between — the
# mode that checks, prunes after a passing check under `enforce: check`, and
# records both. A `command` would replace the entrypoint: the WI-389 prune and
# the record would silently not run.
assert_check_runs_the_runner() {
    local label="$1" rendered="$2" cmd args
    cmd="$(container_value "$rendered" apprafter-backup-check '.command')"
    [[ -z "$cmd" ]] \
        || fail "$label: the check container sets command '$cmd'; it must run the image's entrypoint, the runner"
    args="$(container_value "$rendered" apprafter-backup-check '.args // [] | join(" ")')"
    [[ "$args" == "check" ]] \
        || fail "$label: the check container's args are '${args:-absent}', want 'check' (the runner's check mode)"
    echo "  ok: $label — the check Job runs the runner as \`check\`"
}

echo "==> backup CronJob deadlines, chart $version"

# 1. Defaults.
helm template platform "$chart" --values "$enabled" >"$workdir/defaults.yaml"
assert_cronjob "defaults" "$workdir/defaults.yaml" apprafter-backup 21600
assert_cronjob "defaults" "$workdir/defaults.yaml" apprafter-backup-check 21600
assert_runner_stops_cleanly "defaults" "$workdir/defaults.yaml" apprafter-backup 21600
assert_runner_stops_cleanly "defaults" "$workdir/defaults.yaml" apprafter-backup-check 21600
assert_check_runs_the_runner "defaults" "$workdir/defaults.yaml"
assert_prune_waits_out_the_backup_deadline "defaults" "$workdir/defaults.yaml" 21600

# 2. Both knobs set, to values that tell them apart.
helm template platform "$chart" --values "$enabled" \
    --set backup.activeDeadlineSeconds=2700 \
    --set backup.checkActiveDeadlineSeconds=43200 \
    --set backup.stagingSizeLimit=3Gi \
    >"$workdir/set.yaml"
assert_cronjob "knobs set" "$workdir/set.yaml" apprafter-backup 2700
assert_cronjob "knobs set" "$workdir/set.yaml" apprafter-backup-check 43200
assert_runner_stops_cleanly "knobs set" "$workdir/set.yaml" apprafter-backup 2700
assert_runner_stops_cleanly "knobs set" "$workdir/set.yaml" apprafter-backup-check 43200
assert_check_runs_the_runner "knobs set" "$workdir/set.yaml"
assert_prune_waits_out_the_backup_deadline "knobs set" "$workdir/set.yaml" 2700

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

# `$1` label, `$2` rendered manifests, `$3` CronJob name: the measured
# requests and limit, and no CPU limit.
assert_resources() {
    local label="$1" rendered="$2" name="$3" got want
    for pair in "requests.cpu=100m" "requests.memory=128Mi" "limits.memory=384Mi" "limits.cpu="; do
        want="${pair#*=}"
        got="$(container_value "$rendered" "$name" ".resources.${pair%%=*}")"
        [[ "$got" == "$want" ]] \
            || fail "$label: CronJob '$name' resources.${pair%%=*} is '${got:-absent}', want '${want:-absent}' (WI-386 measured a typical run at about 100 MiB of anonymous memory with the restic settings below, and sized the limit from the largest run measured, 200 MiB)"
    done
    echo "  ok: $label — $name: requests cpu 100m, memory 128Mi; limit memory 384Mi; no CPU limit"
}

# `$1` label, `$2` rendered manifests, `$3` CronJob name, then `VAR=value`
# pairs the container's env must carry exactly.
assert_env() {
    local label="$1" rendered="$2" name="$3" pair got
    shift 3
    for pair in "$@"; do
        got="$(env_value "$rendered" "$name" "${pair%%=*}")"
        [[ "$got" == "${pair#*=}" ]] \
            || fail "$label: CronJob '$name' env ${pair%%=*} is '${got:-absent}', want '${pair#*=}'"
    done
    echo "  ok: $label — $name env: $*"
}

# `$1` label, `$2` rendered manifests, `$3` CronJob name: its runner writes
# to disk only on the staging volume, the volume the size limit applies to.
# TMPDIR is where the backup stages its dumps and where restic makes its
# temporary pack files (a prune's too); the cache is restic's own.
assert_staging_on_the_volume() {
    local label="$1" rendered="$2" name="$3" mount tmpdir cache
    mount="$(container_value "$rendered" "$name" '.volumeMounts[] | select(.name == "staging") | .mountPath')"
    [[ -n "$mount" ]] || fail "$label: the $name runner does not mount the staging volume"
    tmpdir="$(env_value "$rendered" "$name" TMPDIR)"
    [[ "$tmpdir" == "$mount" ]] \
        || fail "$label: $name runner TMPDIR is '${tmpdir:-absent}', but the staging volume is mounted at '$mount'; what the runner and restic write under TMPDIR would land outside the volume stagingSizeLimit bounds"
    cache="$(env_value "$rendered" "$name" RESTIC_CACHE_DIR)"
    [[ "$cache" == "$mount"/* ]] \
        || fail "$label: $name runner RESTIC_CACHE_DIR is '${cache:-absent}', not under the staging volume '$mount'"
    echo "  ok: $label — $name writes under $mount (TMPDIR) and keeps restic's cache at $cache"
}

# `$1` label, `$2` rendered manifests, `$3` CronJob name, `$4` expected size:
# the staging volume's sizeLimit and the limit the runner checks the volume
# against are the same value. The kubelet evicts on the first, from a usage
# figure up to a minute old and with two seconds' notice; the runner stops
# the run on the second and says why. Two different numbers would leave one
# of them never reached.
assert_staging_limit() {
    local label="$1" rendered="$2" name="$3" want="$4" volume env
    volume="$(NAME="$name" "${YQ[@]}" -r 'select(.kind == "CronJob" and .metadata.name == strenv(NAME))
        | .spec.jobTemplate.spec.template.spec.volumes[] | select(.name == "staging")
        | .emptyDir.sizeLimit // ""' "$rendered")"
    env="$(env_value "$rendered" "$name" APPRAFTER_BACKUP_STAGING_SIZE_LIMIT)"
    [[ "$volume" == "$want" ]] \
        || fail "$label: the $name staging volume's sizeLimit is '${volume:-absent}', want '$want'"
    [[ "$env" == "$want" ]] \
        || fail "$label: $name runner env APPRAFTER_BACKUP_STAGING_SIZE_LIMIT is '${env:-absent}', but the staging volume's sizeLimit is '$volume'"
    echo "  ok: $label — $name: staging sizeLimit and the runner's own limit are both $want"
}

echo "==> backup CronJob resources and restic settings, chart $version"

for rendered in "$workdir/defaults.yaml" "$workdir/set.yaml"; do
    label="defaults"
    [[ "$rendered" == "$workdir/set.yaml" ]] && label="knobs set"
    assert_resources "$label" "$rendered" apprafter-backup
    assert_resources "$label" "$rendered" apprafter-backup-check
    assert_env "$label" "$rendered" apprafter-backup \
        GOMAXPROCS=2 GOMEMLIMIT=96MiB RESTIC_PROGRESS_FPS=0.0167
    assert_env "$label" "$rendered" apprafter-backup-check \
        GOMAXPROCS=2 GOMEMLIMIT=96MiB RESTIC_PROGRESS_FPS=0.0167 \
        TMPDIR=/staging RESTIC_CACHE_DIR=/staging/restic-cache
    for name in apprafter-backup apprafter-backup-check; do
        assert_staging_on_the_volume "$label" "$rendered" "$name"
    done
done
for name in apprafter-backup apprafter-backup-check; do
    assert_staging_limit "defaults" "$workdir/defaults.yaml" "$name" 10Gi
    assert_staging_limit "knobs set" "$workdir/set.yaml" "$name" 3Gi
done

echo "==> retention and the check Job's settings (WI-389), chart $version"

# 8. Retention. Unset, both Jobs are told `check`: the check Job prunes after
#    a passing check, the backup Job does not. The check Job gets its depth
#    and the cluster label from the same values as ever.
assert_env "defaults" "$workdir/defaults.yaml" apprafter-backup \
    APPRAFTER_BACKUP_ENFORCE=check
assert_env "defaults" "$workdir/defaults.yaml" apprafter-backup-check \
    APPRAFTER_BACKUP_ENFORCE=check APPRAFTER_BACKUP_CHECK_READ_DATA=false \
    APPRAFTER_BACKUP_CHECK_READ_DATA_SUBSET=10% APPRAFTER_CLUSTER_ID=platform \
    APPRAFTER_BACKUP_REPO=s3:https://objects.example.com/backups
for var in APPRAFTER_BACKUP_KEEP_DAILY APPRAFTER_BACKUP_KEEP_WEEKLY APPRAFTER_BACKUP_KEEP_MONTHLY; do
    for name in apprafter-backup apprafter-backup-check; do
        got="$(env_value "$workdir/defaults.yaml" "$name" "$var")"
        [[ -z "$got" ]] \
            || fail "defaults: $name carries $var=$got; unset, the runner's own 7/4/6 applies"
    done
done
echo "  ok: defaults — no keep counts rendered; the runner's 7/4/6 applies"

#    An explicit mode is kept, on both Jobs, with the keep counts, and the
#    check's depth follows its knobs (a full read wins).
helm template platform "$chart" --values "$enabled" \
    --set backup.retention.enforce=operator \
    --set backup.retention.keepDaily=14 \
    --set backup.retention.keepWeekly=8 \
    --set backup.retention.keepMonthly=12 \
    --set backup.checkReadData=true \
    --set backup.clusterName=eu-prod \
    >"$workdir/retention.yaml"
for name in apprafter-backup apprafter-backup-check; do
    assert_env "explicit operator" "$workdir/retention.yaml" "$name" \
        APPRAFTER_BACKUP_ENFORCE=operator APPRAFTER_BACKUP_KEEP_DAILY=14 \
        APPRAFTER_BACKUP_KEEP_WEEKLY=8 APPRAFTER_BACKUP_KEEP_MONTHLY=12 \
        APPRAFTER_CLUSTER_ID=eu-prod
done
assert_env "explicit operator" "$workdir/retention.yaml" apprafter-backup-check \
    APPRAFTER_BACKUP_CHECK_READ_DATA=true
helm template platform "$chart" --values "$enabled" \
    --set backup.checkReadDataSubset= >"$workdir/structure.yaml"
assert_env "structure-only check" "$workdir/structure.yaml" apprafter-backup-check \
    APPRAFTER_BACKUP_CHECK_READ_DATA=false APPRAFTER_BACKUP_CHECK_READ_DATA_SUBSET=
for mode in check cluster; do
    helm template platform "$chart" --values "$enabled" \
        --set "backup.retention.enforce=$mode" >"$workdir/mode.yaml"
    assert_env "explicit $mode" "$workdir/mode.yaml" apprafter-backup-check \
        "APPRAFTER_BACKUP_ENFORCE=$mode"
done

#    A PlatformStack that sets only a keep count reaches the chart as a
#    `retention` map without `enforce` (the operator leaves out what the CR
#    leaves out): the chart's default must still fill it in.
cat >"$workdir/keep-only.yaml" <<'EOF2'
backup:
  retention:
    keepDaily: 5
EOF2
helm template platform "$chart" --values "$enabled" --values "$workdir/keep-only.yaml" \
    >"$workdir/keep-only-render.yaml"
assert_env "keep count only" "$workdir/keep-only-render.yaml" apprafter-backup-check \
    APPRAFTER_BACKUP_ENFORCE=check APPRAFTER_BACKUP_KEEP_DAILY=5

#    A mode that does not exist is refused at install, not rendered into a
#    runner that would read it as "prune nothing".
if helm template platform "$chart" --values "$enabled" \
    --set backup.retention.enforce=weekly >/dev/null 2>"$workdir/err"; then
    fail "backup.retention.enforce=weekly rendered; values.schema.json must refuse it"
fi
grep -q "enforce" "$workdir/err" \
    || fail "backup.retention.enforce=weekly failed for a reason that does not name the key: $(cat "$workdir/err")"
echo "  ok: backup.retention.enforce=weekly is refused by the values schema"

# 9. The names the chart renders are the names the runner reads, with every
#    optional variable switched on.
helm template platform "$chart" --values "$enabled" \
    --set backup.retention.keepDaily=14 \
    --set backup.retention.keepWeekly=8 \
    --set backup.retention.keepMonthly=12 \
    --set backup.failureWebhook=https://hooks.example.com/backup \
    --set backup.clusterName=eu-prod \
    >"$workdir/every-knob.yaml"
names="$("${YQ[@]}" -r 'select(.kind == "CronJob")
    | .spec.jobTemplate.spec.template.spec.containers[0].env[].name
    | select(test("^APPRAFTER_"))' "$workdir/every-knob.yaml" | grep '^APPRAFTER_' | sort -u)"
[[ -n "$names" ]] || fail "no APPRAFTER_* variable found in the rendered CronJobs"
# The runner's code without its tests: a test that sets the variable by its
# right name would otherwise stand in for code that reads a misspelt one.
# Every file keeps its tests in one module at its end.
runner_code="$(for f in cli/apprafter-backup/src/*.rs; do sed '/^#\[cfg(test)\]$/,$d' "$f"; done)"
count=0
while read -r name; do
    grep -qF "\"$name\"" <<<"$runner_code" \
        || fail "the chart renders $name, and the runner (cli/apprafter-backup/src, outside its tests) never reads it: the setting reaches nothing"
    count=$((count + 1))
done <<<"$names"
echo "  ok: the runner reads each of the $count APPRAFTER_* variables the CronJobs render"

echo "PASS: both backup CronJobs carry a Job deadline, stop cleanly at it, and"
echo "      carry the measured resources and restic settings; both keep what"
echo "      restic writes on the limited staging volume; the check Job runs the"
echo "      runner with the retention mode and depth it is configured with."
