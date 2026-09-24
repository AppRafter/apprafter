// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter platform …` thin wrappers. Track B.1.79.
//!
//! `status` reads `PlatformStack/default` from the cluster and
//! prints a human-readable summary (current/target/available
//! versions, conditions, recent history). `upgrade --to <v>`
//! patches `spec.pin`. Both shell out to `kubectl` rather than
//! pulling in kube-rs's Tokio runtime for the synchronous CLI
//! binary.

use chrono::{DateTime, Utc};
use cli_core::timefmt::format_timestamp_with_relative;
use cli_core::{CliError, Result};
use serde_json::Value;
use tabled::settings::{object::Columns, Modify, Width};
use tabled::{Table, Tabled};

use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_apply_server_side, kubectl_get_json,
    kubectl_get_json_showing_managed_fields, kubectl_merge_patch,
};
use cli_providers::k8s::kubectl::APPRAFTER_CLI_EGRESS_FIELD_MANAGER;

pub(crate) const PLATFORMSTACK_NAME: &str = "default";
pub(crate) const PLATFORMSTACK_NAMESPACE: &str = "apprafter-system";

/// Annotation the CLI stamps to ask the operator for an immediate
/// upstream OCI re-poll (instead of waiting for the operator's 6h
/// cadence). Contract shared with the operator: the operator sees
/// this RFC3339 timestamp is newer than `status.lastUpstreamCheck`,
/// does an immediate poll, and then stamps
/// `status.lastUpstreamCheck = now` (> the request ts). The CLI's
/// "recheck completed" signal is `status.lastUpstreamCheck` parsing
/// to a moment STRICTLY AFTER the request ts.
const RECHECK_REQUESTED_ANNOTATION: &str = "apprafter.io/recheck-requested";

/// How long `status` / `update` wait for the operator to honour a
/// recheck before falling back to the last-known status. Kept short
/// — the operator's poll is a single OCI HEAD; a longer wait would
/// only punish operators whose cluster runs a binary that predates
/// the recheck contract (it ignores the annotation, so we'd wait the
/// full budget every time).
const RECHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Poll interval while waiting for `status.lastUpstreamCheck` to
/// advance past the request timestamp.
const RECHECK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Tabled, Debug)]
pub(crate) struct ConditionRow {
    #[tabled(rename = "TYPE")]
    type_: String,
    #[tabled(rename = "STATUS")]
    status: String,
    #[tabled(rename = "REASON")]
    reason: String,
    #[tabled(rename = "MESSAGE")]
    message: String,
}

#[derive(Tabled)]
struct HistoryRow {
    #[tabled(rename = "APPLIED AT")]
    applied_at: String,
    #[tabled(rename = "VERSION")]
    version: String,
    #[tabled(rename = "OUTCOME")]
    outcome: String,
}

pub fn status(cached: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // Unless the operator opted into the last-known snapshot, ask the
    // operator for a fresh upstream re-check first so the displayed
    // `available` / `lastCheck` / `UpgradeAvailable` reflect the most
    // recent OCI poll rather than the operator's (up to 6h stale) cadence.
    if !cached {
        force_recheck_and_wait(kc.path(), Utc::now)?;
    }

    let json = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "PlatformStack {PLATFORMSTACK_NAMESPACE}/{PLATFORMSTACK_NAME} not found in cluster — \
             is `apprafter cluster-bootstrap` complete?"
        ))
    })?;

    // The PlatformStack, and nothing else. 2.23a moved the two
    // cluster-wide application roll-ups that used to print here into
    // `apprafter status` (`commands::app_rollup`): neither reads this
    // object, and a command named for the platform stack has no business
    // being the place a reader learns an application is broken.
    print_status(&json, Utc::now());
    Ok(())
}

/// Stamp the recheck-request annotation on the singleton
/// PlatformStack, then POLL `status.lastUpstreamCheck` until it
/// advances past the timestamp we wrote (the operator completed an
/// immediate poll) or the [`RECHECK_TIMEOUT`] budget elapses.
///
/// GRACEFUL on timeout: a cluster whose operator predates this
/// contract simply ignores the annotation and never advances
/// `lastUpstreamCheck` in response — we must NOT hard-fail (status
/// still has to render the last-known data). On timeout we print a
/// one-line `note:` and return `Ok(())`; the caller then reads +
/// renders whatever the cluster currently reports.
///
/// `now` is injected (a `Fn() -> DateTime<Utc>`) so the request
/// timestamp and the elapsed-budget check use the same clock and
/// tests can drive the comparison deterministically.
fn force_recheck_and_wait<F: Fn() -> DateTime<Utc>>(
    kubeconfig_path: &std::path::Path,
    now: F,
) -> Result<()> {
    let requested = now();
    let body = recheck_annotation_patch_body(&requested.to_rfc3339());
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kubeconfig_path,
    )?;

    let deadline = std::time::Instant::now() + RECHECK_TIMEOUT;
    loop {
        // Re-read just the status each cycle. A transient read error
        // here shouldn't abort the whole command — treat it like a
        // not-yet-fresh poll and keep waiting until the deadline.
        let last_check = kubectl_get_json(
            "platformstack",
            Some(PLATFORMSTACK_NAME),
            Some(PLATFORMSTACK_NAMESPACE),
            kubeconfig_path,
        )
        .ok()
        .flatten()
        .and_then(|j| {
            j.pointer("/status/lastUpstreamCheck")
                .and_then(Value::as_str)
                .map(str::to_string)
        });

        if recheck_completed(last_check.as_deref(), requested) {
            return Ok(());
        }

        if std::time::Instant::now() >= deadline {
            println!(
                "note: upstream re-check did not complete within {}s (operator may predate this \
                 feature); showing last-known data",
                RECHECK_TIMEOUT.as_secs()
            );
            return Ok(());
        }
        std::thread::sleep(RECHECK_POLL_INTERVAL);
    }
}

/// RFC 7396 merge-patch body that stamps the recheck-request
/// annotation. Pure fn — the timestamp string is the caller's; the
/// builder just wraps it in the metadata/annotations envelope so it
/// can be unit-tested without a cluster.
pub(crate) fn recheck_annotation_patch_body(ts: &str) -> String {
    // serde_json so the timestamp is JSON-escaped correctly (RFC3339
    // has no quotes/backslashes, but route it through the encoder
    // rather than hand-splicing to stay safe).
    serde_json::json!({
        "metadata": { "annotations": { RECHECK_REQUESTED_ANNOTATION: ts } }
    })
    .to_string()
}

/// The "recheck completed" predicate: did the operator stamp a
/// `status.lastUpstreamCheck` STRICTLY AFTER the request ts we
/// wrote? Pure fn so every branch is unit-testable.
///
/// - `None` (status carries no `lastUpstreamCheck`) → not completed.
/// - Unparseable timestamp → not completed (defensive; the operator
///   always writes RFC3339, but a mid-write CR shouldn't read as done).
/// - Parsed but `<= requested` → the value is stale (operator hasn't
///   polled since our request, OR predates the contract) → not done.
/// - Parsed and `> requested` → fresh → done.
pub(crate) fn recheck_completed(
    last_upstream_check: Option<&str>,
    requested: DateTime<Utc>,
) -> bool {
    let Some(raw) = last_upstream_check else {
        return false;
    };
    match DateTime::parse_from_rfc3339(raw) {
        Ok(parsed) => parsed.with_timezone(&Utc) > requested,
        Err(_) => false,
    }
}

/// What a condition's `status: "True"` MEANS. Read from the
/// platform-stack controller, which is the only writer of these.
///
/// Polarity is not decoration. `apprafter status` shows the conditions
/// worth a reader's attention, and the obvious filter — "anything not
/// `True`" — is exactly backwards for half of this list: `YankedVersion`
/// and `NodeDiskPressure` are bad news *because* they are `True`, so
/// that filter hides the two conditions a reader most needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConditionPolarity {
    /// `True` is healthy — surface when it is anything else.
    Positive,
    /// `True` is the problem — surface only when it is `True`.
    Negative,
    /// Real, but reported better somewhere else in the same output, so
    /// surfacing it here would be a second vaguer copy of a fact the
    /// reader already has a line above.
    ReportedElsewhere,
}

/// Classify a condition type, or `None` if this build has never heard
/// of it.
///
/// `None` is not "ignore": an unclassified condition is always
/// surfaced (see [`unhealthy_condition_rows`]). The operator is a
/// separate cargo workspace, so this list cannot be derived at compile
/// time; a unit test asserts it covers every type the controller
/// writes, so growing one there forces a decision here.
pub(crate) fn condition_polarity(type_: &str) -> Option<ConditionPolarity> {
    match type_ {
        // `Ready` mirrors the parent platform Application's health, and
        // `Synced` its agreement with the desired state: True is the
        // good news on both.
        "Ready" | "Synced" | "UpstreamReachable" => Some(ConditionPolarity::Positive),
        // `UnauthorizedSourceModification=True` means a foreign writer
        // WAS detected on `spec.source`. Reading it as positive is how
        // the roll-up came to call a clean cluster unhealthy.
        "YankedVersion" | "NodeDiskPressure" | "UnauthorizedSourceModification" => {
            Some(ConditionPolarity::Negative)
        }
        // On the version line.
        "UpgradeAvailable" => Some(ConditionPolarity::ReportedElsewhere),
        // Gets its own section, which names the plans rather than just
        // asserting that some exist.
        "MigrationPending" => Some(ConditionPolarity::ReportedElsewhere),
        // Positive (`True` is healthy), but with its own section
        // ([`backup_health_lines`]) that says since when and what to run
        // next, so the one-row table version would be a vaguer copy of it.
        "BackupHealthy" => Some(ConditionPolarity::ReportedElsewhere),
        // Retention, printed under `Backups:` by the same section
        // ([`backup_retention_lines`]), which knows that `False` for a
        // scoped key is a warning to act on, not a failing backup.
        "BackupRetention" => Some(ConditionPolarity::ReportedElsewhere),
        _ => None,
    }
}

/// The first platform release whose operator writes `BackupHealthy` and
/// `BackupRetention` (operator v0.2.52; the compatibility record is checked
/// by a test).
pub(crate) const BACKUP_CONDITIONS_SINCE: &str = "0.2.80";

/// The platform version the stack runs, when it is older than
/// [`BACKUP_CONDITIONS_SINCE`]: its operator writes neither backup
/// condition, so one on the stack was left there by a newer operator before
/// a rollback. The older operator carries it forward untouched in every
/// status write (it copies `status.conditions` and upserts only its own
/// types), and nothing re-evaluates it: read as current, the last `True`
/// would say "healthy" over a runner that no longer runs.
///
/// `None` when the version is newer, or does not parse (a branch, an unset
/// field): that proves nothing, and the condition is read.
fn backup_conditions_predate(json: &Value) -> Option<&str> {
    let current = json
        .pointer("/status/currentVersion")
        .and_then(Value::as_str)?;
    let version = semver::Version::parse(current.trim_start_matches('v')).ok()?;
    let since = semver::Version::parse(BACKUP_CONDITIONS_SINCE).ok()?;
    (version < since).then_some(current)
}

/// Where a reader whose backup cannot run for lack of room is sent: the
/// backup guide's troubleshooting entry, the same one `backup status` and
/// `backup run` print. A test resolves it against the committed page.
pub(crate) const BACKUP_UNSCHEDULABLE_DOC: &str =
    "https://docs.apprafter.dev/operator-guide/backup-restore/#runner-unschedulable";

/// Where every other `BackupHealthy` cause is explained, reason by reason.
pub(crate) const BACKUP_HEALTH_DOC: &str =
    "https://docs.apprafter.dev/how-it-works/backup-retention-and-checks/#when-a-backup-cannot-run";

/// The backup section of `apprafter status` and `apprafter platform status`,
/// from the operator's `BackupHealthy` condition.
///
/// The condition is built from the backup CronJobs, their Jobs and pods, so
/// it sees what the runner's own record cannot: a pod no node has room for,
/// a runner killed at its memory limit or evicted, a Job stopped by its
/// deadline before its pod ever started. A backup that cannot run must not
/// look healthy here, so anything but `True` is loud and names the next
/// command, and a cluster that enabled backups on an operator too old to
/// report on them says so instead of saying nothing.
///
/// The operator's second backup condition, `BackupRetention`, follows
/// ([`backup_retention_lines`]): it is kept apart from `BackupHealthy` so
/// that one never hides the other, and this section prints both for the
/// same reason.
pub(crate) fn backup_health_lines(json: &Value, now: DateTime<Utc>) -> Vec<String> {
    let mut lines = backup_runs_lines(json, now);
    lines.extend(backup_retention_lines(json, now));
    lines
}

/// Where retention, and each reason it is not enforced, is explained.
pub(crate) const BACKUP_RETENTION_DOC: &str =
    "https://docs.apprafter.dev/how-it-works/backup-retention-and-checks/#who-runs-the-prune";

/// The retention half of the backup section, from the operator's
/// `BackupRetention` condition: one quiet line when a prune ran after the
/// latest check; otherwise what is not enforced, since when, the operator's
/// message (with the repository's size and growth) and what to run next.
///
/// Nothing while backups are off, and nothing from an operator that does
/// not write the condition: every operator that reports `BackupHealthy`
/// reports this too (they shipped together), and one that reports neither
/// has already been named by the line above.
pub(crate) fn backup_retention_lines(json: &Value, now: DateTime<Utc>) -> Vec<String> {
    let enabled = json
        .pointer("/spec/backup/enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // An operator older than the condition: the runs line names it once.
    if !enabled || backup_conditions_predate(json).is_some() {
        return Vec::new();
    }
    let conditions = json.pointer("/status/conditions").and_then(Value::as_array);
    let Some(c) = conditions.and_then(|cs| cs.iter().find(|c| c["type"] == "BackupRetention"))
    else {
        return Vec::new();
    };
    let field = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("");
    let (status, reason, message) = (field("status"), field("reason"), field("message"));
    if status == "True" {
        return vec![format!("Retention: enforced — {message}")];
    }
    let since = c
        .get("lastTransitionTime")
        .and_then(Value::as_str)
        .map(|t| format_timestamp_with_relative(t, now))
        .unwrap_or_else(|| "an unrecorded time".to_string());
    let headline = match (status, reason) {
        // Chosen: said plainly, as the state it is, not as a failure.
        ("False", "EnforcedOutsideCluster") => {
            format!("Retention: not enforced in the cluster, by choice — {reason}")
        }
        ("False", _) => format!("Retention: NOT ENFORCED since {since} — {reason}"),
        _ => format!("Retention: not known yet — {reason}"),
    };
    let mut lines = vec![cli_core::style::warn(&headline)];
    if !message.is_empty() {
        lines.push(cli_core::style::warn(&format!("  {message}")));
    }
    let next = retention_next_step(reason, now).unwrap_or_else(|| {
        "`apprafter backup status` shows the runner's record of its last check and prune."
            .to_string()
    });
    lines.push(format!("  Next: {next}"));
    lines.push(format!(
        "  What it means and what to change: {BACKUP_RETENTION_DOC}"
    ));
    lines
}

/// What to run next for each `BackupRetention` reason the operator writes
/// that is not `True`. Every reason is named, so a new one is a decision
/// here (a test reads the reasons out of the operator's source).
fn retention_next_step(reason: &str, now: DateTime<Utc>) -> Option<String> {
    Some(match reason {
        "PruneNotPermitted" => {
            "prune from outside the cluster with the operator's full credentials: `apprafter \
             backup prune --credential-file <full-credentials.env>`. The cluster's key may not \
             delete on purpose (it keeps a compromised cluster from erasing history), so this \
             stays until retention runs where the full credentials are."
        }
        "EnforcedOutsideCluster" => {
            "`apprafter backup prune` with the operator's full credentials, on your own \
             cadence; or `apprafter backup set enforce check` to prune after the weekly check."
        }
        "CheckFailed" => {
            "`apprafter backup status` shows why the check failed (`last check`), and \
             `apprafter backup check` runs it from this machine. A check that does not pass \
             never prunes; retention resumes after one that does."
        }
        "CheckOff" => {
            "`apprafter backup set check 06:00` turns the weekly check, and the prune after it, \
             back on; `apprafter backup prune` prunes from outside the cluster meanwhile."
        }
        "PruneFailed" => {
            "`apprafter backup status` shows the prune's error; `apprafter backup prune` runs \
             the same prune from this machine."
        }
        "NoCheckYet" | "NoPruneYet" => {
            return Some(format!(
                "`apprafter backup status` shows the schedule. {} runs the check, and the prune \
                 after it, now.",
                run_check_now(now)
            ))
        }
        "RecordUnreadable" => {
            "the operator could not read the runner's record, and its log says why; `apprafter \
             backup status` reads it with your own credentials."
        }
        _ => return None,
    }
    .to_string())
}

/// The command that starts the weekly check Job now, as a code span.
///
/// The Job gets a name of its own each time, stamped as `apprafter backup
/// run` stamps a manual backup. `kubectl create job --from=cronjob/…` makes
/// the CronJob its owner, and the CronJob keeps up to three failed Jobs: a
/// fixed name made the second run of the same advice fail with
/// `AlreadyExists` whenever the first had failed, which is when it is needed
/// again.
fn run_check_now(now: DateTime<Utc>) -> String {
    format!(
        "`kubectl -n apprafter-system create job --from=cronjob/apprafter-backup-check \
         apprafter-backup-check-now-{}`",
        now.format("%Y%m%d-%H%M%S")
    )
}

/// The runs half of the backup section, from `BackupHealthy`.
fn backup_runs_lines(json: &Value, now: DateTime<Utc>) -> Vec<String> {
    let enabled = json
        .pointer("/spec/backup/enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let condition = json
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .and_then(|cs| cs.iter().find(|c| c["type"] == "BackupHealthy"));
    if enabled {
        if let (Some(version), Some(_)) = (backup_conditions_predate(json), condition) {
            return vec![cli_core::style::warn(&format!(
                "Backups: enabled, but this cluster's operator (platform {version}) does not \
                 report whether they run — `apprafter backup status` shows the last Jobs. The \
                 backup conditions on the stack were left by a newer release and are not \
                 current."
            ))];
        }
    }
    let Some(c) = condition else {
        return if enabled {
            vec![cli_core::style::warn(
                "Backups: enabled, but this cluster's operator does not report whether they \
                 run — `apprafter backup status` shows the last Jobs.",
            )]
        } else {
            vec!["Backups: not enabled.".to_string()]
        };
    };
    backup_condition_lines(c, "Backups", now, backup_next_step)
}

/// One backup condition as lines: a single quiet line when it is `True`;
/// otherwise a loud headline (with since when, for `False`), the operator's
/// own message, what to run next and the page that explains it.
fn backup_condition_lines(
    c: &Value,
    label: &str,
    now: DateTime<Utc>,
    next_step: fn(&str, DateTime<Utc>) -> Option<NextStep>,
) -> Vec<String> {
    let field = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("");
    let (status, reason, message) = (field("status"), field("reason"), field("message"));
    if status == "True" {
        return vec![format!("{label}: healthy — {message}")];
    }
    let mut lines = if status == "False" {
        let since = c
            .get("lastTransitionTime")
            .and_then(Value::as_str)
            .map(|t| format_timestamp_with_relative(t, now))
            .unwrap_or_else(|| "an unrecorded time".to_string());
        vec![cli_core::style::warn(&format!(
            "{label}: FAILING since {since} — {reason}"
        ))]
    } else {
        vec![cli_core::style::warn(&format!(
            "{label}: not known to be working — {reason}"
        ))]
    };
    if !message.is_empty() {
        lines.push(cli_core::style::warn(&format!("  {message}")));
    }
    // A reason this build has never heard of still gets a next step: the
    // one command that shows every backup Job.
    let (next, doc) = next_step(reason, now).unwrap_or((
        "`apprafter backup status` shows the Jobs and the runner's own record.".to_string(),
        BACKUP_HEALTH_DOC,
    ));
    lines.push(format!("  Next: {next}"));
    // Its own line, so the URL can be copied whole from any terminal width.
    lines.push(format!("  What it means and what to change: {doc}"));
    lines
}

/// What to run next, and the page that explains it.
type NextStep = (String, &'static str);

/// What to run next for each `BackupHealthy` reason the operator writes,
/// and the page that explains it. Every reason is named, so a new one is a
/// decision here rather than a silent fall-through to the generic advice
/// (a test reads the reasons out of the operator's source).
fn backup_next_step(reason: &str, now: DateTime<Utc>) -> Option<NextStep> {
    let step = match reason {
        "RunnerUnschedulable" => (
            "`apprafter backup status` shows the Job and its pod; `apprafter top` shows how \
             much of the node is requested, and by what.",
            BACKUP_UNSCHEDULABLE_DOC,
        ),
        // `backup status` reads the Job's pod, so for a Job the controller
        // could not create a pod for it has nothing to say but `Unknown`
        // (seen on kind): the FailedCreate events are the only record.
        "RunnerNotStarted" => (
            "`apprafter backup status` shows the Job and why its pod has not started. A Job \
             with no pod at all shows as Unknown there; `kubectl -n apprafter-system describe \
             job <name>` prints the FailedCreate events that say why.",
            BACKUP_HEALTH_DOC,
        ),
        "RunnerOOMKilled" => (
            "`apprafter backup status` shows the Job and its attempts; `apprafter top` shows \
             the node's memory.",
            BACKUP_HEALTH_DOC,
        ),
        // The kubelet evicts for two unrelated reasons, and the message
        // quotes which: node memory pressure, or staging grown past the
        // `/staging` volume's size limit.
        "RunnerEvicted" => (
            "`apprafter backup status` shows the Job and its attempts. For memory pressure, \
             `apprafter top` shows the node's memory; for the staging size limit, `apprafter \
             backup set staging-mode sequential` stages one namespace at a time.",
            BACKUP_HEALTH_DOC,
        ),
        // A pod of higher priority needed the runner's room: any pod, since
        // the runner's priority is below every other. The Job's next pod
        // waits for room, so the way out is the node's room, as for a
        // runner that cannot be scheduled.
        "RunnerPreempted" => (
            "`apprafter top` shows how much of the node is requested, and by what: another pod \
             needed the runner's room. `apprafter backup status` shows the Job and the runner's \
             record of the stop; the Job retries once there is room, and a retry that succeeds \
             clears this.",
            BACKUP_UNSCHEDULABLE_DOC,
        ),
        // A drain, a deletion, or a stop whose cause is gone with its pod.
        "RunnerStopped" => (
            "`apprafter backup status` shows the Job and the runner's own record of the stop \
             (`lastError`). The Job retries; a retry that succeeds clears this, and `apprafter \
             backup run` starts one once the Job has ended.",
            BACKUP_HEALTH_DOC,
        ),
        // Reported while the Job retries: the runner has recorded why, and
        // posted its failure webhook, for this attempt.
        "RunnerFailed" => (
            "`apprafter backup status` shows the Job and the runner's own record of the error \
             (`lastError`). A retry that succeeds clears this; `apprafter backup run` starts one \
             once the Job has ended.",
            BACKUP_HEALTH_DOC,
        ),
        // Only a Job no attempt of which failed on its own ends with this
        // reason (the operator keeps the attempts' reason otherwise), so it
        // is a runner that never started or one that ran out of time; the
        // message says which.
        "DeadlineExceeded" => (
            "`apprafter backup status` shows the Job and the runner's own record. A runner that \
             never started needs room on the node (`apprafter top` shows it); one that ran out \
             of time needs a longer deadline (`apprafter backup set deadline`, or `set \
             check-deadline` for the check).",
            BACKUP_HEALTH_DOC,
        ),
        "BackoffLimitExceeded" | "Failed" => (
            "`apprafter backup status` shows the Jobs and the runner's own record.",
            BACKUP_HEALTH_DOC,
        ),
        // Only a later check Job clears it, and the schedule is weekly: say
        // how to run one now instead of leaving the reader to wait a week.
        "RepositoryCheckFailed" => {
            return Some((
                format!(
                    "`apprafter backup check` runs the same check from this machine and prints \
                     restic's own output; `apprafter backup status` shows the check Job. Only a \
                     check that passes in the cluster clears this: {} runs one now.",
                    run_check_now(now)
                ),
                BACKUP_HEALTH_DOC,
            ))
        }
        "ScheduleSuspended" => (
            "resume the CronJob the message names: `kubectl -n apprafter-system patch cronjob \
             <name> --type merge -p '{\"spec\":{\"suspend\":false}}'`.",
            BACKUP_HEALTH_DOC,
        ),
        "NoRunYet" => (
            "`apprafter backup run` takes a backup now instead of waiting for the schedule; \
             `apprafter backup status` shows the schedule.",
            BACKUP_HEALTH_DOC,
        ),
        "ScheduleNotDeployed" => (
            "wait for the platform to sync the backup schedule (`apprafter status` shows the \
             platform's sync), then `apprafter backup status`.",
            BACKUP_HEALTH_DOC,
        ),
        "StateUnreadable" => (
            "the operator could not read the backup Jobs, and its log says why; `apprafter \
             backup status` reads them with your own credentials.",
            BACKUP_HEALTH_DOC,
        ),
        _ => return None,
    };
    Some((step.0.to_string(), step.1))
}

/// Every condition on the stack, as rows. Shared by the full table
/// `platform status` prints and the filtered view `status` prints.
fn condition_rows(status: &Value) -> Vec<ConditionRow> {
    status
        .pointer("/conditions")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    let field =
                        |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    ConditionRow {
                        type_: field("type"),
                        status: field("status"),
                        reason: field("reason"),
                        message: field("message"),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The conditions a reader of `apprafter status` should see, by
/// [`ConditionPolarity`]. An unclassified type is always kept: this
/// surface exists to answer "is anything wrong", and a condition this
/// build cannot interpret is the one case where guessing silently is
/// the worst available answer.
pub(crate) fn unhealthy_condition_rows(json: &Value) -> Vec<ConditionRow> {
    let status = json.get("status").cloned().unwrap_or(Value::Null);
    condition_rows(&status)
        .into_iter()
        .filter(|row| match condition_polarity(&row.type_) {
            Some(ConditionPolarity::Positive) => row.status != "True",
            Some(ConditionPolarity::Negative) => row.status == "True",
            Some(ConditionPolarity::ReportedElsewhere) => false,
            None => true,
        })
        .collect()
}

/// One line naming the version the cluster is on, and the one it could
/// move to when that differs.
///
/// Deliberately silent in the steady state (`available == current`),
/// which is where a cluster spends nearly all of its life: a field that
/// renders "available: the version you are already running" teaches the
/// reader to skip it on the one day it says something.
///
/// The two are compared as semver, as the operator's `UpgradeAvailable`
/// compares them. Only a NEWER channel release is an upgrade. An older one
/// is what a cluster sees when its running version was yanked after it was
/// installed — the resolver skips yanked releases — and the line says that
/// when `YankedVersion` does; it used to read "upgrade available" for a
/// downgrade. A version that does not parse is named and not judged — and so
/// is the channel's release before anything is installed (`currentVersion`
/// unset): there is nothing to upgrade from. Build metadata does not order
/// releases.
pub(crate) fn version_summary_line(json: &Value) -> String {
    let status = json.get("status").cloned().unwrap_or(Value::Null);
    let current = status
        .pointer("/currentVersion")
        .and_then(Value::as_str)
        .unwrap_or("(unset)");
    let available = status
        .pointer("/availableVersion")
        .and_then(Value::as_str)
        .filter(|a| *a != current && *a != "(unset)");
    let yanked = condition_rows(&status)
        .iter()
        .any(|row| row.type_ == "YankedVersion" && row.status == "True");
    let semver = |v: &str| semver::Version::parse(v.trim_start_matches('v')).ok();

    let Some(available) = available else {
        return format!("Platform: {current}");
    };
    // Precedence, not `Ord`: build metadata does not order releases (SemVer
    // §10), so `0.2.80+build.1` and `0.2.80` are the same one; semver's `Ord`
    // orders by it and called one "older" than the other.
    let order = match (semver(current), semver(available)) {
        (Some(c), Some(a)) => a.cmp_precedence(&c),
        _ => return format!("Platform: {current} — the channel's newest release: {available}"),
    };
    match order {
        std::cmp::Ordering::Greater => {
            format!("Platform: {current} — upgrade available: {available}")
        }
        std::cmp::Ordering::Equal => format!("Platform: {current}"),
        std::cmp::Ordering::Less if yanked => format!(
            "Platform: {current} — yanked; the channel's newest release that is not yanked, \
             {available}, is older than this one, so there is no upgrade to take"
        ),
        std::cmp::Ordering::Less => format!(
            "Platform: {current} — no upgrade: the channel's newest release, {available}, is \
             older than this one"
        ),
    }
}

/// Pure formatter — pulled out so unit tests can drive with a
/// fixture JSON without a cluster. `now` lets tests pin "now"
/// for deterministic relative-date formatting; production
/// callers use `Utc::now()`.
fn print_status(json: &Value, now: DateTime<Utc>) {
    let spec = json.get("spec").cloned().unwrap_or(Value::Null);
    let status = json.get("status").cloned().unwrap_or(Value::Null);

    let channel = spec
        .pointer("/channel")
        .and_then(Value::as_str)
        .unwrap_or("stable");
    let pin = spec
        .pointer("/pin")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "(unpinned)".to_string());
    let auto_upgrade = spec
        .pointer("/autoUpgrade")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tier = spec
        .pointer("/values/tier")
        .and_then(Value::as_i64)
        .unwrap_or(0);

    let current = status
        .pointer("/currentVersion")
        .and_then(Value::as_str)
        .unwrap_or("(unset)");
    let target = status
        .pointer("/targetVersion")
        .and_then(Value::as_str)
        .unwrap_or("(unset)");
    let available = status
        .pointer("/availableVersion")
        .and_then(Value::as_str)
        .unwrap_or("(unset)");
    let last_check_raw = status.pointer("/lastUpstreamCheck").and_then(Value::as_str);
    let last_check = last_check_raw
        .map(|s| format_timestamp_with_relative(s, now))
        .unwrap_or_else(|| "(never)".to_string());

    println!("PlatformStack {PLATFORMSTACK_NAMESPACE}/{PLATFORMSTACK_NAME} — tier {tier}");
    println!("  channel:     {channel}");
    println!("  pin:         {pin}");
    println!("  autoUpgrade: {auto_upgrade}");
    println!();
    println!("Versions:");
    println!("  current:   {current}");
    println!("  target:    {target}");
    println!("  available: {available}");
    println!("  lastCheck: {last_check}");
    println!();

    // The full table, unfiltered: this command is the detail view, and
    // an operator reading it wants to see a condition sitting at
    // `True` as much as one that is not. `apprafter status` takes the
    // filtered view (`unhealthy_condition_rows`) off the same rows.
    let conditions: Vec<ConditionRow> = condition_rows(&status);

    if conditions.is_empty() {
        println!("Conditions: (none)");
    } else {
        println!("Conditions:");
        println!("{}", render_conditions_table(&conditions));
    }
    println!();

    // The backup verdict again, in words: the table row above has no room
    // for since when or for what to run next.
    for line in backup_health_lines(json, now) {
        println!("{line}");
    }
    println!();

    let history: Vec<HistoryRow> = collect_history_rows(&status, now, 5);

    if history.is_empty() {
        println!("Recent history: (none)");
    } else {
        println!("Recent history (last {}):", history.len());
        println!("{}", Table::new(&history));
    }
}

/// Render the conditions table sized to the operator's
/// terminal. Without sizing the `MESSAGE` column dominates
/// (some operator-controller messages run hundreds of
/// characters); previous heuristic of a flat 60-char wrap
/// blew out narrow terminals (80 cols → table sprawled at
/// 130-ish cols). Compute a budget that subtracts the other
/// three columns' max widths plus separator overhead, then
/// wrap MESSAGE to that budget. Falls back to a sane 60 when
/// stdout isn't a TTY (CI, pipes — width unknown).
pub(crate) fn render_conditions_table(conditions: &[ConditionRow]) -> String {
    let terminal_width = terminal_width_or_default();
    // Compute the visible width each non-message column will
    // claim: max(header, cells). Plus 3 separators (` | `) of
    // 3 chars each × column gaps.
    let type_w = column_width(conditions, |r| &r.type_, "TYPE");
    let status_w = column_width(conditions, |r| &r.status, "STATUS");
    let reason_w = column_width(conditions, |r| &r.reason, "REASON");
    // Tabled adds borders / paddings; budget 12 char overhead
    // empirically (4 columns × 3-char gap + outer borders).
    let overhead = 12usize;
    let used = type_w + status_w + reason_w + overhead;
    let message_budget = terminal_width.saturating_sub(used).max(20);
    let mut t = Table::new(conditions);
    t.with(Modify::new(Columns::one(3)).with(Width::wrap(message_budget)));
    t.to_string()
}

fn column_width<F: Fn(&ConditionRow) -> &str>(
    rows: &[ConditionRow],
    field: F,
    header: &str,
) -> usize {
    rows.iter()
        .map(|r| field(r).len())
        .max()
        .unwrap_or(0)
        .max(header.len())
}

/// Look up the operator's terminal width with sane fallbacks
/// — `terminal_size` for TTYs, 100 when stdout is piped or
/// the lookup fails. 100 keeps tables readable on common CI
/// log capture without surprising sprawl.
fn terminal_width_or_default() -> usize {
    match terminal_size::terminal_size() {
        Some((terminal_size::Width(w), _)) => w as usize,
        None => 100,
    }
}

/// Collect the most recent history entries, sorted by
/// `appliedAt` desc. Falls back to declaration order when the
/// timestamp is missing/unparseable (puts unparseable last so
/// they don't dominate the visible head of the table).
fn collect_history_rows(status: &Value, now: DateTime<Utc>, take: usize) -> Vec<HistoryRow> {
    let mut entries: Vec<&Value> = status
        .pointer("/versionHistory")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().collect())
        .unwrap_or_default();
    // Stable sort by parsed `appliedAt` desc; entries without
    // a parseable timestamp sink to the bottom (they're either
    // corrupt CRs or freshly-created records still missing the
    // field).
    entries.sort_by(|a, b| {
        let ta = a
            .get("appliedAt")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
        let tb = b
            .get("appliedAt")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
        match (ta, tb) {
            (Some(a), Some(b)) => b.cmp(&a),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
    entries
        .into_iter()
        .take(take)
        .map(|e| {
            let raw_at = e.get("appliedAt").and_then(Value::as_str).unwrap_or("");
            HistoryRow {
                applied_at: if raw_at.is_empty() {
                    String::new()
                } else {
                    format_timestamp_with_relative(raw_at, now)
                },
                version: e
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                outcome: e
                    .get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            }
        })
        .collect()
}

// The absolute-plus-relative timestamp rendering used to live here, and
// was the only copy of it in the CLI — so the surfaces that did not import
// it from this module printed raw RFC3339 instead. It now lives in
// `cli_core::timefmt`, reachable from every crate. The tests below stayed
// behind deliberately: they pinned the shipped phrasing before the move and
// still pass against the moved implementation, which is what makes the move
// a move rather than a rewrite.

/// `apprafter platform freeze <component> [--version <v>]`
/// patches `PlatformStack.spec.overrides.<component>.pin`.
/// Without `--version` resolves the current effective version
/// through a fallback chain so the verb is always actionable
/// no matter which signals the cluster currently surfaces.
///
/// Resolution chain (walk-fix #3 post-B.1.79a / v0.1.147):
///
///   1. **PlatformStack.status.componentVersions.<component>**
///      — the operator's canonical version dial when present.
///      M1.5 ships this populated only on bump cycles though,
///      so it can be absent on steady state.
///   2. **Argo CD `Application argocd/<component>.spec.source.
///      targetRevision`** — the version Argo CD is actively
///      reconciling against. Always present for a chart-managed
///      component, since the umbrella's `templates/applications.
///      yaml` template emits it. This is the new fallback.
///   3. Hard error pointing the operator at `--version <v>`.
pub fn freeze(component: &str, version: Option<&str>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let stack = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "PlatformStack {PLATFORMSTACK_NAMESPACE}/{PLATFORMSTACK_NAME} not found"
        ))
    })?;

    let pin = match version {
        Some(v) => v.to_string(),
        None => {
            // Try the Argo CD Application as fallback when
            // PlatformStack.status doesn't carry the version
            // yet. 404 on the lookup is fine — we'll pass
            // `None` along to the resolver and let it surface
            // a clean error pointing at `--version <v>`.
            let app = kubectl_get_json(
                "application.argoproj.io",
                Some(component),
                Some("argocd"),
                kc.path(),
            )?;
            resolve_effective_pin(&stack, app.as_ref(), component)?
        }
    };

    let body = format!(r#"{{"spec":{{"overrides":{{"{component}":{{"pin":"{pin}"}}}}}}}}"#);
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;

    println!("✓ Component '{component}' frozen at version '{pin}'.");
    println!(
        "The PlatformController reconcile cycle will apply the override; the umbrella \
         chart's curated pin for '{component}' is ignored as long as the override is set."
    );
    println!("To revert: `apprafter platform unfreeze {component}`.");
    Ok(())
}

/// Resolve the effective pin for `component` through the
/// PlatformStack-then-Argo-CD fallback chain. Pure fn — tests
/// drive every branch with fixture JSON instead of needing a
/// live cluster.
///
/// Returns the rendered string verbatim. Caller threads it
/// into the merge-patch body. None Argo CD source is allowed
/// (caller may have 404'd on the lookup); the resolver only
/// errors when BOTH signals are absent.
pub(crate) fn resolve_effective_pin(
    stack: &Value,
    argocd_app: Option<&Value>,
    component: &str,
) -> Result<String> {
    if let Some(v) = stack
        .pointer(&format!("/status/componentVersions/{component}"))
        .and_then(Value::as_str)
    {
        return Ok(v.to_string());
    }
    if let Some(app) = argocd_app {
        if let Some(v) = app
            .pointer("/spec/source/targetRevision")
            .and_then(Value::as_str)
        {
            return Ok(v.to_string());
        }
    }
    Err(CliError::Other(format!(
        "No effective version known for component '{component}' — \
         PlatformStack.status.componentVersions.{component} is empty and \
         Argo CD Application argocd/{component} carries no spec.source.targetRevision. \
         Pass `--version <v>` to set the pin explicitly, or run `apprafter platform status` \
         to inspect the list of known components."
    )))
}

/// `apprafter platform unfreeze <component>` — RFC 7396
/// merge-patch with a `null` value removes the override entry.
pub fn unfreeze(component: &str) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // RFC 7396: null deletes the field. Patches the
    // `overrides.<component>` entry as a whole — strips both
    // `pin` and `values` overrides. If the operator wants to
    // keep a partial override (e.g. unfreeze the pin but keep
    // values overrides), they should patch manually; `unfreeze`
    // is the "fully revert to the chart's curated state" verb.
    let body = format!(r#"{{"spec":{{"overrides":{{"{component}":null}}}}}}"#);
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;
    println!("✓ Component '{component}' unfrozen. Chart's curated pin restored.");
    Ok(())
}

/// `apprafter platform rescue` — emergency recovery wrapper
/// over `apprafter cluster-bootstrap`. Re-applies the loader's
/// Cilium + Argo CD + CRDs + operator chain against the active
/// target. Useful when Argo CD itself is unable to self-adopt
/// and a regular upgrade flow won't reach the right reconcile
/// state.
pub fn rescue(yes: bool) -> Result<()> {
    if !yes {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        println!(
            "Emergency rescue: re-run the loader's cluster-bootstrap path against the active \
             target. This will apply the upstream Cilium / Argo CD / CRDs / operator manifests \
             as in the initial bootstrap — all AppRafter-managed Applications will lose their \
             current Sync/Healthy state for a few reconcile cycles."
        );
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }
    println!("Re-running cluster-bootstrap chain...");
    // `None` is correct here and is NOT the C1 defect repeating itself.
    // `platform rescue` has no `--target` flag; its doc comment above
    // and the confirmation prompt the operator just accepted both say
    // "the active target" in so many words. Acting on the active target
    // is the documented contract, so there is nothing to override.
    // Threading a target in would need a `--target` flag on `rescue`
    // and a reworded prompt first — do not "fix" this call site on its
    // own.
    crate::commands::cluster_bootstrap::run(None)
}

pub fn upgrade(to: Option<&str>, cached: bool) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    // Force a fresh upstream re-check first (unless `--cached`) so the
    // upgrade decision — and the operator's subsequent channel-latest
    // resolution when clearing the pin — acts on the most recent
    // availableVersion rather than the operator's up-to-6h-stale poll.
    if !cached {
        force_recheck_and_wait(kc.path(), Utc::now)?;
    }

    let body = match to {
        Some(v) => format!(r#"{{"spec":{{"pin":"{v}"}}}}"#),
        None => r#"{"spec":{"pin":null,"autoUpgrade":true}}"#.to_string(),
    };
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;
    match to {
        Some(v) => println!("Pinned PlatformStack/{PLATFORMSTACK_NAME} to {v}"),
        None => println!(
            "Cleared pin; autoUpgrade=true. PlatformController will resolve to channel-latest."
        ),
    }
    Ok(())
}

/// The three valid egress profiles, in order of decreasing
/// breadth. Single source of truth for both the validator and the
/// `set` error message; mirrors the operator/webhook enum.
const EGRESS_PROFILES: [&str; 3] = ["internet", "internal", "strict"];

/// Validate an egress profile string against the
/// `internet|internal|strict` enum. Pure fn — client-side guard so
/// a typo (`open`) is rejected with a clear message instead of
/// degrading into an admission-webhook rejection. Mirrors
/// `validator_platformstack.rs`'s enum (ADR 0045 §Decision #3/#4).
fn validate_egress_profile(profile: &str) -> Result<()> {
    if EGRESS_PROFILES.contains(&profile) {
        return Ok(());
    }
    Err(CliError::Other(format!(
        "egress profile '{profile}' is invalid; expected one of internet|internal|strict \
         (internet = DNS + same-ns + world + needs; internal = DNS + same-ns + needs; \
         strict = DNS + needs)."
    )))
}

/// Pure formatter for `apprafter platform egress show`. Reads
/// `/spec/network/egress/profile` from the PlatformStack JSON and
/// renders the active profile plus the three-line legend. An
/// absent field reports the documented operator default
/// (`internet`), flagged as unset so it's not mistaken for an
/// explicit `set`. Pulled out so tests drive it with a fixture
/// JSON without a cluster (mirrors `print_status`).
fn format_egress_profile(json: &Value) -> String {
    let active = json
        .pointer("/spec/network/egress/profile")
        .and_then(Value::as_str);
    let header = match active {
        Some(p) => format!("Egress profile: {p}"),
        None => "Egress profile: internet (default — field unset)".to_string(),
    };
    format!(
        "{header}\n\
         \n\
         Profiles:\n\
         \u{2022} internet  DNS + same-namespace + world (external internet) + declared needs\n\
         \u{2022} internal  DNS + same-namespace + declared needs (no external internet)\n\
         \u{2022} strict    DNS + declared needs (same-namespace egress also denied)\n\
         \n\
         Set with: apprafter platform egress set <internet|internal|strict>"
    )
}

/// `apprafter platform egress show` — read the singleton
/// PlatformStack and print the current egress profile + legend.
pub fn egress_show() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    // Managed-fields variant: `egress_field_appears_git_managed` reads
    // `metadata.managedFields`, which kubectl strips from `get -o json`
    // unless asked. Shipped in 2.10 on the plain getter, so the warning
    // below had never once been reachable.
    let json = kubectl_get_json_showing_managed_fields(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "PlatformStack {PLATFORMSTACK_NAMESPACE}/{PLATFORMSTACK_NAME} not found in cluster — \
             is `apprafter cluster-bootstrap` complete?"
        ))
    })?;

    println!("{}", format_egress_profile(&json));
    if egress_field_appears_git_managed(&json) {
        println!(
            "\n⚠ The egress profile field appears to be managed by Argo CD (an infra-repo \
             declares spec.network.egress.profile). `apprafter platform egress set` will be \
             reverted on the next sync — change it in git instead."
        );
    }
    Ok(())
}

/// `apprafter platform egress set <profile>` — server-side apply
/// the profile onto the singleton PlatformStack under the dedicated
/// field manager `apprafter-cli-egress`. SSA (not merge-patch) so the
/// value survives Argo CD self-heal: the platform-stack chart does not
/// declare this field, so there is no conflicting owner to revert it
/// (ADR 0045 §Decision #4 / design §E).
///
/// The manager is deliberately DISTINCT from `cluster-bootstrap`'s
/// `apprafter-cli` (which owns the REQUIRED `spec.source` + `spec.values`):
/// re-applying this partial object under that same manager would make SSA
/// prune source/values and the apiserver would reject the PlatformStack
/// (`Required value`). See [`APPRAFTER_CLI_EGRESS_FIELD_MANAGER`].
pub fn egress_set(profile: &str) -> Result<()> {
    validate_egress_profile(profile)?;
    let kc = ensure_kubeconfig_tempfile()?;

    // Best-effort: if an infra-repo already owns the field via
    // Argo CD, warn that git will win on the next sync. A 404 /
    // unparseable managedFields is non-fatal — fall through to the
    // unconditional advisory below.
    let existing = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?;
    let git_managed = existing
        .as_ref()
        .map(egress_field_appears_git_managed)
        .unwrap_or(false);

    let manifest = format!(
        "apiVersion: apprafter.io/v1alpha1\n\
         kind: PlatformStack\n\
         metadata:\n\
        \x20 name: {PLATFORMSTACK_NAME}\n\
        \x20 namespace: {PLATFORMSTACK_NAMESPACE}\n\
         spec:\n\
        \x20 network:\n\
        \x20   egress:\n\
        \x20     profile: {profile}\n"
    );
    kubectl_apply_server_side(&manifest, APPRAFTER_CLI_EGRESS_FIELD_MANAGER, kc.path())?;

    println!(
        "✓ Egress profile set to '{profile}' (field manager '{APPRAFTER_CLI_EGRESS_FIELD_MANAGER}')."
    );
    println!(
        "The operator's ApplicationController re-derives each app's egress CiliumNetworkPolicy \
         on its next reconcile; run `apprafter platform egress show` to confirm."
    );
    if git_managed {
        println!(
            "⚠ This field appears to be declared in an infra-repo Argo CD reconciles — git \
             wins on the next sync and this live value will be reverted. Change it in git."
        );
    } else {
        println!(
            "Note: the platform-stack chart does not declare this field, so this value persists \
             across Argo CD syncs. If you later opt into an infra-repo that declares \
             spec.network.egress.profile, git becomes authoritative and wins on the next sync."
        );
    }
    Ok(())
}

/// Shown after every `platform env` output so operators aren't misled: the
/// default env is a CLI convenience, not a rendering gate (ADR 0044).
const SOFT_ENV_NOTE: &str =
    "(soft default — preselects the `apprafter app add` env picker; it does NOT \
     change rendering. An app added without `--env` is still base-only.)";

/// Trim + reject empty/whitespace. Pure (unit-tested without a cluster).
fn validate_env(env: &str) -> Result<&str> {
    let trimmed = env.trim();
    if trimmed.is_empty() {
        return Err(CliError::Other("environment must not be empty".into()));
    }
    Ok(trimmed)
}

/// The path-scoped JSON merge-patch body for `spec.defaultEnvironment`. Pure.
fn default_environment_patch_body(env: &str) -> String {
    serde_json::json!({ "spec": { "defaultEnvironment": env } }).to_string()
}

/// `apprafter platform env show` — print the cluster's default environment.
pub fn env_show() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let stack = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?;
    let current = stack
        .as_ref()
        .and_then(|s| s.pointer("/spec/defaultEnvironment"))
        .and_then(Value::as_str);
    match current {
        Some(env) => println!("Default environment: {env}"),
        None => println!("Default environment: (unset)"),
    }
    println!("{SOFT_ENV_NOTE}");
    Ok(())
}

/// `apprafter platform env set <env>` — set the cluster's default environment.
pub fn env_set(env: &str) -> Result<()> {
    let env = validate_env(env)?;
    let kc = ensure_kubeconfig_tempfile()?;
    let body = default_environment_patch_body(env);
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;
    println!("✓ Default environment set to '{env}'.");
    println!("{SOFT_ENV_NOTE}");
    Ok(())
}

/// The path-scoped JSON merge-patch body for
/// `spec.resources.autoscale.mode`. Pure (unit-tested without a cluster).
fn autoscale_patch_body(mode: &str) -> String {
    serde_json::json!({ "spec": { "resources": { "autoscale": { "mode": mode } } } }).to_string()
}

/// Validate the autoscale mode string client-side so the user gets a
/// clear rejection rather than a raw apiserver/webhook error.
fn validate_autoscale_mode(mode: &str) -> Result<&str> {
    match mode {
        "full" | "up-only" | "off" => Ok(mode),
        other => Err(CliError::Other(format!(
            "invalid autoscale mode '{other}' (expected full|up-only|off)"
        ))),
    }
}

/// `apprafter platform autoscale show` — print the current VPA autoscale mode.
pub fn autoscale_show() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let stack = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?;
    let mode = stack
        .as_ref()
        .and_then(|s| s.pointer("/spec/resources/autoscale/mode"))
        .and_then(Value::as_str)
        .unwrap_or("full (default)");
    println!("Autoscale mode: {mode}");
    println!(
        "\nModes:\n\
         \u{2022} full      VPA applies both CPU and memory recommendations (up and down)\n\
         \u{2022} up-only   VPA scales resources up but never below the platform seed\n\
         \u{2022} off       VPA recommendations recorded but NOT applied to pods\n\
         \n\
         Set with: apprafter platform autoscale set <full|up-only|off>"
    );
    Ok(())
}

/// `apprafter platform autoscale set <mode>` — set the cluster-wide VPA
/// autoscale mode via merge-patch on the singleton PlatformStack. Uses
/// merge-patch (not SSA) for the same reason as `env set` / `target domain`:
/// the path is nested under `spec.resources` which may contain other fields
/// the CLI doesn't own; a scoped merge-patch touches only the leaf.
pub fn autoscale_set(mode: &str) -> Result<()> {
    let mode = validate_autoscale_mode(mode)?;
    let kc = ensure_kubeconfig_tempfile()?;
    let body = autoscale_patch_body(mode);
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;
    println!("✓ Autoscale mode set to '{mode}'.");
    if mode == "off" {
        println!(
            "⚠ off freezes live pods but does NOT restore them: the next deploy/recreation \
             reverts each pod to the platform seed (32Mi). Set explicit `resources` on apps \
             you want to keep at their current sizing."
        );
    }
    println!(
        "Note: if an infra-repo declares spec.resources.autoscale, Argo CD becomes \
         authoritative and wins on the next sync."
    );
    Ok(())
}

/// Best-effort: does any `metadata.managedFields` entry owned by a
/// manager OTHER than `apprafter-cli` whose name looks like Argo CD
/// (`argocd`, `argo-cd-*`, `application-controller`) carry the
/// `spec.network.egress` subtree? Argo CD's managed-fields entry
/// records `f:spec → f:network → f:egress` when the field is
/// git-declared. Conservative: parse failures / absence → `false`
/// (we then fall back to the unconditional advisory in `set`).
fn egress_field_appears_git_managed(json: &Value) -> bool {
    let Some(entries) = json
        .pointer("/metadata/managedFields")
        .and_then(Value::as_array)
    else {
        return false;
    };
    entries.iter().any(|e| {
        let manager = e.get("manager").and_then(Value::as_str).unwrap_or("");
        let is_argo = manager.contains("argocd")
            || manager.contains("argo-cd")
            || manager.contains("application-controller");
        if !is_argo {
            return false;
        }
        e.pointer("/fieldsV1/f:spec/f:network/f:egress").is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // The bootstrap field manager — only referenced from the
    // distinct-manager regression test below, so it lives here rather than
    // at module scope (where it would be an unused import in non-test builds).
    use cli_providers::k8s::kubectl::APPRAFTER_CLI_FIELD_MANAGER;
    use serde_json::json;

    use chrono::TimeZone;

    // ---- WI-386: the backup section ----

    fn with_backup(enabled: bool, condition: Option<Value>) -> Value {
        let mut conditions = vec![json!({ "type": "Ready", "status": "True" })];
        conditions.extend(condition);
        json!({
            "spec": { "backup": { "enabled": enabled } },
            "status": { "conditions": conditions },
        })
    }

    fn backup_condition(status: &str, reason: &str, message: &str) -> Value {
        json!({ "type": "BackupHealthy", "status": status, "reason": reason,
                "message": message, "lastTransitionTime": "2026-09-23T03:10:05Z" })
    }

    const UNSCHEDULABLE_MESSAGE: &str = "backup Job apprafter-backup-29312340: its pod \
        apprafter-backup-29312340-x7k2q has not been scheduled since 2026-09-23T03:00:04Z: \
        0/1 nodes are available: 1 Insufficient memory.";

    #[test]
    fn a_backup_that_cannot_be_scheduled_is_loud_and_says_what_to_run() {
        let stack = with_backup(
            true,
            Some(backup_condition(
                "False",
                "RunnerUnschedulable",
                UNSCHEDULABLE_MESSAGE,
            )),
        );
        let lines = backup_health_lines(&stack, frozen_now());
        let text = lines.join("\n");
        // What failed and since when.
        assert!(
            lines[0].contains("FAILING since 2026-09-23 03:10 UTC"),
            "{text}"
        );
        assert!(lines[0].contains("RunnerUnschedulable"), "{text}");
        assert!(text.contains("Insufficient memory"), "{text}");
        assert!(text.contains("apprafter-backup-29312340"), "{text}");
        // What to run next, and where the fix is described.
        assert!(text.contains("`apprafter backup status`"), "{text}");
        assert!(text.contains("`apprafter top`"), "{text}");
        assert!(
            lines
                .iter()
                .any(|l| l.trim_end().ends_with(BACKUP_UNSCHEDULABLE_DOC)),
            "the URL ends its own line so it copies whole: {text}"
        );
    }

    #[test]
    fn an_oom_killed_runner_points_at_the_node_and_the_reasons_page() {
        let stack = with_backup(
            true,
            Some(backup_condition(
                "False",
                "RunnerOOMKilled",
                "backup Job x: its attempt 1 of at most 7 was OOMKilled at its 384Mi memory limit",
            )),
        );
        let text = backup_health_lines(&stack, frozen_now()).join("\n");
        assert!(text.contains("OOMKilled at its 384Mi"), "{text}");
        assert!(text.contains("`apprafter top`"), "{text}");
        assert!(text.contains(BACKUP_HEALTH_DOC), "{text}");
    }

    #[test]
    fn a_deadline_failure_sends_the_reader_to_backup_status() {
        let stack = with_backup(
            true,
            Some(backup_condition(
                "False",
                "DeadlineExceeded",
                "backup Job x: failed",
            )),
        );
        let text = backup_health_lines(&stack, frozen_now()).join("\n");
        assert!(text.contains("FAILING"), "{text}");
        assert!(text.contains("`apprafter backup status`"), "{text}");
        // Found by review: this advice named only room on the node, while a
        // runner that ran and was too slow needs a longer deadline instead.
        assert!(text.contains("`apprafter top`"), "{text}");
        assert!(text.contains("`apprafter backup set deadline`"), "{text}");
        assert!(text.contains(BACKUP_HEALTH_DOC), "{text}");
    }

    #[test]
    fn a_failed_attempt_sends_the_reader_to_the_runners_record() {
        let stack = with_backup(
            true,
            Some(backup_condition(
                "False",
                "RunnerFailed",
                "backup Job apprafter-backup-29312340: its attempt 1 of at most 7 exited with \
                 code 1 (pod apprafter-backup-29312340-aaaaa, at 2026-09-23T03:00:30Z). The Job \
                 retries until its backoff limit. The runner recorded: restic backup: Fatal: \
                 unable to open repository: 503 Slow Down",
            )),
        );
        let text = backup_health_lines(&stack, frozen_now()).join("\n");
        assert!(text.contains("FAILING since"), "{text}");
        assert!(text.contains("503 Slow Down"), "{text}");
        assert!(
            text.contains("`apprafter backup status` shows the Job and the runner's own record"),
            "{text}"
        );
        assert!(text.contains("`lastError`"), "{text}");
    }

    #[test]
    fn a_backup_not_known_to_work_is_never_rendered_as_healthy() {
        for reason in ["NoRunYet", "StateUnreadable", "ScheduleNotDeployed"] {
            let stack = with_backup(true, Some(backup_condition("Unknown", reason, "why")));
            let text = backup_health_lines(&stack, frozen_now()).join("\n");
            assert!(text.contains("not known to be working"), "{reason}: {text}");
            assert!(!text.contains("healthy"), "{reason}: {text}");
            assert!(
                text.contains("`apprafter backup status`"),
                "{reason}: {text}"
            );
        }
    }

    #[test]
    fn a_healthy_backup_is_one_quiet_line() {
        let stack = with_backup(
            true,
            Some(backup_condition(
                "True",
                "Succeeded",
                "the last backup, Job apprafter-backup-29312340, succeeded at 2026-09-23T03:01:02Z",
            )),
        );
        let lines = backup_health_lines(&stack, frozen_now());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("Backups: healthy"), "{lines:?}");
        assert!(lines[0].contains("apprafter-backup-29312340"), "{lines:?}");
    }

    #[test]
    fn enabled_backups_with_no_condition_do_not_read_as_fine() {
        // An operator from before the condition existed. Silence here would
        // be read as "nothing wrong".
        let text = backup_health_lines(&with_backup(true, None), frozen_now()).join("\n");
        assert!(text.contains("does not report"), "{text}");
        assert!(text.contains("`apprafter backup status`"), "{text}");
    }

    #[test]
    fn a_cluster_without_backups_says_so_in_one_line() {
        let lines = backup_health_lines(&with_backup(false, None), frozen_now());
        assert_eq!(lines, vec!["Backups: not enabled.".to_string()]);
    }

    #[test]
    fn a_failed_repository_check_sends_the_reader_to_backup_check() {
        let stack = with_backup(
            true,
            Some(backup_condition(
                "False",
                "RepositoryCheckFailed",
                "repository check Job apprafter-backup-check-29310000: failed at \
                 2026-09-21T06:12:00Z after 7 failed attempts (BackoffLimitExceeded).",
            )),
        );
        let text = backup_health_lines(&stack, frozen_now()).join("\n");
        assert!(text.contains("FAILING since"), "{text}");
        assert!(text.contains("`apprafter backup check`"), "{text}");
        // The schedule is weekly, so the way to clear it now is named.
        assert!(
            text.contains("--from=cronjob/apprafter-backup-check"),
            "{text}"
        );
        assert!(text.contains(BACKUP_HEALTH_DOC), "{text}");
    }

    /// The `kubectl create job` line printed to run the check now, and the
    /// Job name in it.
    fn printed_check_job(text: &str) -> String {
        let at = text
            .find("--from=cronjob/apprafter-backup-check ")
            .unwrap_or_else(|| panic!("no create-job line in {text}"));
        let rest = &text[at + "--from=cronjob/apprafter-backup-check ".len()..];
        rest.split('`').next().unwrap().trim().to_string()
    }

    /// Found by review: the advice printed one fixed Job name. The Job it
    /// creates is owned by the CronJob, which keeps up to three failed ones,
    /// so a rerun that failed again stays — and the same command then fails
    /// with AlreadyExists, exactly when the reader needs to run it again.
    #[test]
    fn the_command_that_runs_the_check_now_names_a_new_job_each_time() {
        let later = frozen_now() + chrono::Duration::seconds(1);
        let failed = |now| {
            let stack = with_backup(
                true,
                Some(backup_condition(
                    "False",
                    "RepositoryCheckFailed",
                    "repository check Job x: failed",
                )),
            );
            printed_check_job(&backup_health_lines(&stack, now).join("\n"))
        };
        let not_yet = |now| {
            let stack = with_retention(
                Some(backup_condition("True", "Succeeded", "ok")),
                Some(retention_condition("Unknown", "NoCheckYet", "why")),
            );
            printed_check_job(&backup_health_lines(&stack, now).join("\n"))
        };
        for name in [failed(frozen_now()), not_yet(frozen_now())] {
            assert!(name.starts_with("apprafter-backup-check-"), "{name}");
            // A Job name is a DNS-1123 label, and its pods' `job-name` label
            // value must fit in 63 characters.
            assert!(name.len() <= 63, "{name}");
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{name}"
            );
        }
        assert_ne!(failed(frozen_now()), failed(later));
        assert_ne!(not_yet(frozen_now()), not_yet(later));
    }

    #[test]
    fn a_job_with_no_pod_is_sent_to_its_events() {
        // As the kind proof printed it. `backup status` shows such a Job as
        // Unknown, so the advice must name where the reason actually is.
        let stack = with_backup(
            true,
            Some(backup_condition(
                "False",
                "RunnerNotStarted",
                "backup Job apprafter-backup-29836689: it has had no pod since it was created at \
                 2026-09-23T22:09:00Z: the Job controller has not created one.",
            )),
        );
        let text = backup_health_lines(&stack, frozen_now()).join("\n");
        assert!(text.contains("FAILING since"), "{text}");
        assert!(
            text.contains("`kubectl -n apprafter-system describe job <name>`"),
            "{text}"
        );
        assert!(text.contains("FailedCreate"), "{text}");
    }

    #[test]
    fn an_evicted_runner_names_both_ways_out() {
        // As the kind proof printed it: the staging volume past its size
        // limit, where pointing only at the node's memory would mislead.
        let stack = with_backup(
            true,
            Some(backup_condition(
                "False",
                "RunnerEvicted",
                "backup Job wi386h-evict: failed at 2026-09-23T21:57:36Z after 1 failed attempt \
                 (BackoffLimitExceeded). The last one was evicted (pod wi386h-evict-vz8qf): Usage \
                 of EmptyDir volume \"staging\" exceeds the limit \"16Mi\".",
            )),
        );
        let text = backup_health_lines(&stack, frozen_now()).join("\n");
        assert!(text.contains("exceeds the limit"), "{text}");
        assert!(text.contains("`apprafter top`"), "{text}");
        assert!(
            text.contains("`apprafter backup set staging-mode sequential`"),
            "{text}"
        );
    }

    #[test]
    fn a_preempted_runner_is_sent_to_the_nodes_room() {
        // As the operator writes it (WI-386). The next pod waits for room,
        // so the advice is the capacity one, and the page is the entry that
        // explains the runner's priority.
        let stack = with_backup(
            true,
            Some(backup_condition(
                "False",
                "RunnerPreempted",
                "backup Job apprafter-backup-e: its attempt 1 of at most 7 was preempted (pod \
                 apprafter-backup-e-pqww9): default-scheduler: preempting to accommodate a higher \
                 priority pod. The Job retries until its backoff limit.",
            )),
        );
        let text = backup_health_lines(&stack, frozen_now()).join("\n");
        assert!(text.contains("FAILING since"), "{text}");
        assert!(text.contains("`apprafter top`"), "{text}");
        assert!(text.contains(BACKUP_UNSCHEDULABLE_DOC), "{text}");
    }

    #[test]
    fn a_reason_this_build_does_not_know_still_gets_a_next_step() {
        let stack = with_backup(true, Some(backup_condition("False", "SomethingNew", "why")));
        let text = backup_health_lines(&stack, frozen_now()).join("\n");
        assert!(text.contains("FAILING since"), "{text}");
        assert!(text.contains("Next: `apprafter backup status`"), "{text}");
        assert!(text.contains(BACKUP_HEALTH_DOC), "{text}");
    }

    #[test]
    fn every_backup_reason_the_operator_writes_has_its_own_next_step() {
        // The operator is a separate workspace, so its reasons are read out
        // of its source, as the condition types are above. A reason added
        // there fails here until somebody decides what the reader runs next,
        // instead of silently getting the generic advice.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../operator/operator-controllers/platform-stack/src/backup_health.rs");
        let src =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let reasons: Vec<String> = src
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub const REASON_"))
            .filter_map(|l| l.split_once("= \""))
            .filter_map(|(_, rest)| rest.split_once('"'))
            .map(|(name, _)| name.to_string())
            .collect();
        assert!(
            reasons.len() >= 12,
            "only {reasons:?} parsed out of {} — the declaration shape changed, and an \
             empty list would have passed",
            path.display()
        );
        for reason in reasons.iter().filter(|r| *r != "Succeeded") {
            assert!(
                backup_next_step(reason, frozen_now()).is_some(),
                "the operator writes BackupHealthy reason `{reason}` and this build gives \
                 it no next step of its own"
            );
        }
    }

    #[test]
    fn a_failing_backup_is_not_folded_into_the_condition_table() {
        // Its own section says it better; a table row beside it would be a
        // second, vaguer copy.
        let stack = with_backup(
            true,
            Some(backup_condition(
                "False",
                "RunnerUnschedulable",
                UNSCHEDULABLE_MESSAGE,
            )),
        );
        assert!(unhealthy_condition_rows(&stack).is_empty());
    }

    #[test]
    fn both_backup_documentation_links_resolve_to_committed_sections() {
        // The URLs are printed to someone whose backup cannot run. The docs
        // build checks links between pages, not a URL inside a Rust string.
        for url in [
            BACKUP_UNSCHEDULABLE_DOC,
            BACKUP_HEALTH_DOC,
            BACKUP_RETENTION_DOC,
        ] {
            let path = url
                .strip_prefix("https://docs.apprafter.dev/")
                .expect("a docs.apprafter.dev URL");
            let (page, anchor) = path.split_once("/#").expect("<page>/#<anchor>");
            let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../docs")
                .join(format!("{page}.md"));
            let text = std::fs::read_to_string(&file)
                .unwrap_or_else(|e| panic!("{} must exist: {e}", file.display()));
            assert!(
                text.lines().any(
                    |l| l.starts_with('#') && l.trim_end().ends_with(&format!("{{#{anchor}}}"))
                ),
                "{} has no heading with {{#{anchor}}}",
                file.display()
            );
        }
    }

    // ---- WI-389: retention, under the same section ----

    fn with_retention(health: Option<Value>, retention: Option<Value>) -> Value {
        let mut conditions = vec![json!({ "type": "Ready", "status": "True" })];
        conditions.extend(health);
        conditions.extend(retention);
        json!({
            "spec": { "backup": { "enabled": true } },
            "status": { "conditions": conditions },
        })
    }

    fn retention_condition(status: &str, reason: &str, message: &str) -> Value {
        json!({ "type": "BackupRetention", "status": status, "reason": reason,
                "message": message, "lastTransitionTime": "2026-09-20T06:00:41Z" })
    }

    const NOT_PERMITTED_MESSAGE: &str = "retention is not enforced: the cluster's S3 key may not \
        delete, and the prune after the weekly check at 2026-09-20T06:00:41+00:00 was not \
        permitted: the storage refused to delete snapshot ecd0be32 (Remove(<snapshot/ecd0be3219>) \
        failed: client.RemoveObject: Access Denied.), so nothing was deleted. Run `apprafter \
        backup prune` with full credentials. The repository held 1.2 GiB in 42 snapshot(s) and \
        310512 blob(s) at 2026-09-20T06:00:44+00:00; +155.1 MiB, +7 snapshot(s), +11020 blob(s) \
        since 2026-09-13T06:00:40+00:00.";

    /// The recommended setup: backups healthy, retention not enforced. Both
    /// say so, and the healthy line does not swallow the warning.
    #[test]
    fn a_key_that_may_not_delete_is_loud_beside_healthy_backups() {
        let stack = with_retention(
            Some(backup_condition(
                "True",
                "Succeeded",
                "the last backup succeeded",
            )),
            Some(retention_condition(
                "False",
                "PruneNotPermitted",
                NOT_PERMITTED_MESSAGE,
            )),
        );
        let lines = backup_health_lines(&stack, frozen_now());
        let text = lines.join("\n");
        assert!(lines[0].starts_with("Backups: healthy"), "{text}");
        assert!(
            lines[1].contains("Retention: NOT ENFORCED since 2026-09-20 06:00 UTC"),
            "{text}"
        );
        assert!(lines[1].contains("PruneNotPermitted"), "{text}");
        assert!(text.contains("+155.1 MiB"), "the growth is shown: {text}");
        assert!(
            text.contains("`apprafter backup prune --credential-file <full-credentials.env>`"),
            "{text}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.trim_end().ends_with(BACKUP_RETENTION_DOC)),
            "{text}"
        );
    }

    #[test]
    fn enforced_retention_is_one_quiet_line() {
        let stack = with_retention(
            Some(backup_condition("True", "Succeeded", "ok")),
            Some(retention_condition(
                "True",
                "Pruned",
                "the prune after the weekly check at t: forgot 2 snapshot(s)",
            )),
        );
        let lines = backup_health_lines(&stack, frozen_now());
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[1].starts_with("Retention: enforced — the prune after"),
            "{lines:?}"
        );
    }

    /// The explicit opt-out is stated as a choice, not as a failure.
    #[test]
    fn retention_chosen_to_run_outside_the_cluster_is_not_called_failing() {
        let stack = with_retention(
            Some(backup_condition("True", "Succeeded", "ok")),
            Some(retention_condition(
                "False",
                "EnforcedOutsideCluster",
                "spec.backup.retention.enforce is operator",
            )),
        );
        let text = backup_health_lines(&stack, frozen_now()).join("\n");
        assert!(
            text.contains("not enforced in the cluster, by choice"),
            "{text}"
        );
        assert!(!text.contains("NOT ENFORCED since"), "{text}");
        assert!(
            text.contains("`apprafter backup set enforce check`"),
            "{text}"
        );
    }

    #[test]
    fn retention_not_known_yet_is_never_rendered_as_enforced() {
        for reason in ["NoCheckYet", "NoPruneYet", "RecordUnreadable"] {
            let stack = with_retention(
                Some(backup_condition("True", "Succeeded", "ok")),
                Some(retention_condition("Unknown", reason, "why")),
            );
            let text = backup_health_lines(&stack, frozen_now()).join("\n");
            assert!(
                text.contains("Retention: not known yet"),
                "{reason}: {text}"
            );
            assert!(!text.contains("Retention: enforced"), "{reason}: {text}");
        }
    }

    #[test]
    fn an_operator_that_does_not_report_retention_is_named_once() {
        // An operator that reports neither condition is named once, by the
        // runs line; the retention half adds nothing to it.
        let neither = with_retention(None, None);
        let lines = backup_health_lines(&neither, frozen_now());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("does not report"), "{lines:?}");
        // Backups off: nothing about retention, even with a stale condition.
        let mut off = with_retention(
            None,
            Some(retention_condition("False", "PruneNotPermitted", "m")),
        );
        off["spec"]["backup"]["enabled"] = json!(false);
        assert!(backup_retention_lines(&off, frozen_now()).is_empty());
    }

    /// Found by review: after a rollback below the release whose operator
    /// writes these conditions, the older operator carries them forward
    /// untouched — it copies `status.conditions` and upserts only its own
    /// types — so the last `True` stayed on the stack, re-evaluated by
    /// nothing, and this printed "Backups: healthy" over a runner that no
    /// longer ran.
    #[test]
    fn backup_conditions_left_by_a_newer_operator_are_not_read_as_current() {
        let mut stack = with_retention(
            Some(backup_condition(
                "True",
                "Succeeded",
                "the last backup, Job apprafter-backup-29312340, succeeded at 2026-09-23T03:01:02Z",
            )),
            Some(retention_condition(
                "True",
                "Pruned",
                "forgot 2 snapshot(s)",
            )),
        );
        stack["status"]["currentVersion"] = json!("0.2.79");
        let lines = backup_health_lines(&stack, frozen_now());
        let text = lines.join("\n");
        assert_eq!(lines.len(), 1, "{text}");
        assert!(!text.contains("healthy"), "{text}");
        assert!(!text.contains("Retention: enforced"), "{text}");
        assert!(text.contains("does not report"), "{text}");
        assert!(text.contains("platform 0.2.79"), "{text}");
        assert!(text.contains("not current"), "{text}");
        assert!(text.contains("`apprafter backup status`"), "{text}");
        // `backup status` reads the retention verdict through this too, and
        // falls back to the runner's own record when it is empty.
        assert!(backup_retention_lines(&stack, frozen_now()).is_empty());

        // From the release that writes them on, they are read as they are.
        for version in [BACKUP_CONDITIONS_SINCE, "0.2.81", "0.3.0", "v0.2.80"] {
            stack["status"]["currentVersion"] = json!(version);
            let lines = backup_health_lines(&stack, frozen_now());
            assert!(
                lines[0].starts_with("Backups: healthy"),
                "{version}: {lines:?}"
            );
            assert!(
                lines[1].starts_with("Retention: enforced"),
                "{version}: {lines:?}"
            );
        }
        // A version that does not parse proves nothing either way: the
        // condition is read rather than hidden.
        for version in [json!("main"), json!(""), Value::Null] {
            stack["status"]["currentVersion"] = version.clone();
            let lines = backup_health_lines(&stack, frozen_now());
            assert!(
                lines[0].starts_with("Backups: healthy"),
                "{version}: {lines:?}"
            );
        }
    }

    #[test]
    fn an_old_release_with_no_condition_and_backups_off_says_the_usual() {
        let mut off = with_backup(false, None);
        off["status"]["currentVersion"] = json!("0.2.60");
        assert_eq!(
            backup_health_lines(&off, frozen_now()),
            vec!["Backups: not enabled.".to_string()]
        );
        let mut on = with_backup(true, None);
        on["status"]["currentVersion"] = json!("0.2.60");
        let text = backup_health_lines(&on, frozen_now()).join("\n");
        assert!(text.contains("does not report"), "{text}");
        assert!(!text.contains("not current"), "nothing was left: {text}");
    }

    /// [`BACKUP_CONDITIONS_SINCE`] names a release the chart's history
    /// records, and every release before it runs an older operator — which
    /// is what makes "older than this" mean "cannot have written them".
    #[test]
    fn the_first_release_that_reports_backups_is_recorded_and_every_older_one_runs_an_older_operator(
    ) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../platform-stack/cue/compatibility.cue");
        let src =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut records: Vec<(semver::Version, semver::Version)> = Vec::new();
        let mut open: Option<semver::Version> = None;
        for line in src.lines() {
            if let Some(v) = line
                .strip_prefix("compatibility: \"")
                .and_then(|r| r.split_once("\": {"))
                .and_then(|(v, _)| semver::Version::parse(v).ok())
            {
                open = Some(v);
            } else if let Some(op) = line
                .strip_prefix("\toperatorVersion: \"")
                .and_then(|r| r.split_once('"'))
                .and_then(|(v, _)| semver::Version::parse(v.trim_start_matches('v')).ok())
            {
                if let Some(v) = open.take() {
                    records.push((v, op));
                }
            }
        }
        assert!(
            records.len() > 100,
            "only {} records parsed out of {} — the record shape changed, and an empty list \
             would have passed",
            records.len(),
            path.display()
        );
        let since = semver::Version::parse(BACKUP_CONDITIONS_SINCE).unwrap();
        let (_, first_op) = records
            .iter()
            .find(|(v, _)| *v == since)
            .unwrap_or_else(|| panic!("no compatibility record for {since}"));
        for (v, op) in records.iter().filter(|(v, _)| *v < since) {
            assert!(
                op < first_op,
                "platform {v} runs operator {op}, not older than the {first_op} that {since} \
                 introduced the backup conditions with"
            );
        }
    }

    #[test]
    fn every_retention_reason_the_operator_writes_has_its_own_next_step() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../operator/operator-controllers/platform-stack/src/backup_retention.rs");
        let src =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let reasons: Vec<String> = src
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub const REASON_"))
            .filter_map(|l| l.split_once("= \""))
            .filter_map(|(_, rest)| rest.split_once('"'))
            .map(|(name, _)| name.to_string())
            .collect();
        assert!(
            reasons.len() >= 10,
            "only {reasons:?} parsed out of {} — the declaration shape changed",
            path.display()
        );
        for reason in reasons
            .iter()
            .filter(|r| !matches!(r.as_str(), "Pruned" | "NothingToPrune"))
        {
            assert!(
                retention_next_step(reason, frozen_now()).is_some(),
                "the operator writes BackupRetention reason `{reason}` and this build gives it \
                 no next step of its own"
            );
        }
    }

    // ---- 2.23a: the two slices `apprafter status` lifts from here ----

    #[test]
    fn version_summary_line_names_an_available_upgrade() {
        let stack = json!({
            "status": { "currentVersion": "0.2.53", "availableVersion": "0.2.59" }
        });
        let line = version_summary_line(&stack);
        assert!(line.contains("0.2.53"), "{line}");
        assert!(line.contains("0.2.59"), "{line}");
    }

    #[test]
    fn version_summary_line_is_quiet_when_there_is_nothing_to_upgrade_to() {
        // `availableVersion` equal to `currentVersion` is the steady state,
        // and it is the state a cluster is in almost all of the time. A line
        // that renders "available: <the version you are on>" trains the
        // reader to skip the field on the one day it differs.
        let stack = json!({
            "status": { "currentVersion": "0.2.59", "availableVersion": "0.2.59" }
        });
        let line = version_summary_line(&stack);
        assert!(line.contains("0.2.59"), "{line}");
        assert!(
            !line.contains("upgrade"),
            "steady state must not advertise an upgrade: {line}"
        );
    }

    /// FIRES (live walk, P5): a channel whose newest release is OLDER than
    /// the running one is no upgrade. The line used to compare the strings,
    /// so `0.2.80` with `availableVersion: 0.2.79` advertised a downgrade as
    /// "upgrade available", beside an `UpgradeAvailable=False/UpToDate`.
    #[test]
    fn version_summary_line_does_not_call_an_older_release_an_upgrade() {
        let stack = json!({ "status": {
            "currentVersion": "0.2.80", "availableVersion": "0.2.79",
            "conditions": [{ "type": "YankedVersion", "status": "False", "reason": "NotYanked",
                             "message": "currentVersion is not marked yanked" }]
        }});
        let line = version_summary_line(&stack);
        assert!(!line.contains("upgrade available"), "{line}");
        assert!(line.contains("no upgrade"), "{line}");
        assert!(line.contains("0.2.79"), "{line}");
        assert!(line.contains("older than this one"), "{line}");
        assert!(
            !line.contains("yanked"),
            "nothing says this one is yanked: {line}"
        );
    }

    /// Where an older channel release comes from in the field: the running
    /// version was yanked after it was installed, and the resolver skips
    /// yanked versions. The line says so when the operator's condition does.
    #[test]
    fn version_summary_line_says_the_running_version_is_yanked() {
        let stack = json!({ "status": {
            "currentVersion": "0.2.80", "availableVersion": "0.2.79",
            "conditions": [{ "type": "YankedVersion", "status": "True", "reason": "Yanked",
                             "message": "currentVersion 0.2.80 is yanked: the CRD does not apply" }]
        }});
        let line = version_summary_line(&stack);
        assert!(line.contains("0.2.80 — yanked"), "{line}");
        assert!(line.contains("not yanked, 0.2.79"), "{line}");
        assert!(!line.contains("upgrade available"), "{line}");
    }

    /// Semver, not string order: `0.2.100` is newer than `0.2.99`, and a
    /// leading `v` on one side is the same version.
    #[test]
    fn version_summary_line_compares_by_semver() {
        let newer = json!({
            "status": { "currentVersion": "0.2.99", "availableVersion": "0.2.100" }
        });
        assert!(
            version_summary_line(&newer).contains("upgrade available: 0.2.100"),
            "{}",
            version_summary_line(&newer)
        );
        let same = json!({
            "status": { "currentVersion": "v0.2.80", "availableVersion": "0.2.80" }
        });
        assert_eq!(version_summary_line(&same), "Platform: v0.2.80");
    }

    /// FIRES: build metadata is not part of a version's precedence (SemVer
    /// §10), so `0.2.80+build.1` and `0.2.80` are one release. semver's `Ord`
    /// orders by it anyway, and the line called the channel's `0.2.80`
    /// "older than this one". Pre-release still orders, as it must.
    #[test]
    fn version_summary_line_ignores_build_metadata() {
        for (current, available) in [
            ("0.2.80+build.1", "0.2.80"),
            ("0.2.80", "0.2.80+build.1"),
            ("0.2.80+a", "v0.2.80+b"),
        ] {
            let stack = json!({
                "status": { "currentVersion": current, "availableVersion": available }
            });
            assert_eq!(
                version_summary_line(&stack),
                format!("Platform: {current}"),
                "{current} vs {available}"
            );
        }
        let rc = json!({
            "status": { "currentVersion": "0.2.53-rc.1+build.7", "availableVersion": "0.2.53" }
        });
        assert!(
            version_summary_line(&rc).contains("upgrade available: 0.2.53"),
            "{}",
            version_summary_line(&rc)
        );
        let newer_build = json!({
            "status": { "currentVersion": "0.2.80", "availableVersion": "0.2.81+build.1" }
        });
        assert!(
            version_summary_line(&newer_build).contains("upgrade available"),
            "{}",
            version_summary_line(&newer_build)
        );
    }

    /// A PlatformStack with nothing installed yet — no `currentVersion` —
    /// has nothing to upgrade FROM: the line names what the channel offers
    /// and does not call it an upgrade. (It used to, by string inequality.)
    #[test]
    fn version_summary_line_before_anything_is_installed_names_the_channels_release() {
        let stack = json!({ "status": { "availableVersion": "0.2.80" } });
        assert_eq!(
            version_summary_line(&stack),
            "Platform: (unset) — the channel's newest release: 0.2.80"
        );
    }

    /// A version that does not parse is neither an upgrade nor a downgrade:
    /// the line names what the channel offers and claims nothing about it.
    #[test]
    fn version_summary_line_claims_nothing_about_a_version_it_cannot_read() {
        let stack = json!({
            "status": { "currentVersion": "0.2.80", "availableVersion": "next" }
        });
        let line = version_summary_line(&stack);
        assert!(!line.contains("upgrade"), "{line}");
        assert!(line.contains("next"), "{line}");
    }

    #[test]
    fn version_summary_line_survives_a_status_that_is_not_there_yet() {
        // A PlatformStack whose controller has not written status once —
        // real during bootstrap, and the moment a reader is most likely to
        // run `status`.
        let line = version_summary_line(&json!({}));
        assert!(!line.is_empty());
    }

    #[test]
    fn unhealthy_conditions_keep_a_positive_condition_that_is_not_true() {
        let stack = json!({ "status": { "conditions": [
            { "type": "Synced", "status": "False", "reason": "SyncError", "message": "boom" },
            { "type": "UpstreamReachable", "status": "True", "reason": "Ok", "message": "" },
        ]}});
        let rows = unhealthy_condition_rows(&stack);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].type_, "Synced");
    }

    #[test]
    fn unhealthy_conditions_keep_a_negative_condition_that_is_true() {
        // The case a naive `status != "True"` filter gets exactly backwards:
        // both of these are bad news precisely BECAUSE they are True, and a
        // reader who sees neither concludes the cluster is fine.
        let stack = json!({ "status": { "conditions": [
            { "type": "YankedVersion", "status": "True", "reason": "Yanked", "message": "0.2.19" },
            { "type": "NodeDiskPressure", "status": "True", "reason": "Low", "message": "3% left" },
        ]}});
        let rows = unhealthy_condition_rows(&stack);
        assert_eq!(rows.len(), 2, "{rows:?}");
    }

    #[test]
    fn unhealthy_conditions_drop_a_negative_condition_that_is_false() {
        let stack = json!({ "status": { "conditions": [
            { "type": "YankedVersion", "status": "False", "reason": "Clean", "message": "" },
            { "type": "NodeDiskPressure", "status": "False", "reason": "Ok", "message": "" },
        ]}});
        assert!(unhealthy_condition_rows(&stack).is_empty());
    }

    #[test]
    fn unhealthy_conditions_leave_the_two_that_have_their_own_surface_alone() {
        // `UpgradeAvailable` is on the version line and `MigrationPending`
        // gets a section that NAMES the plans. Repeating either here is not
        // redundancy that costs nothing — it is a second, vaguer report of
        // the same fact one line above a better one.
        let stack = json!({ "status": { "conditions": [
            { "type": "UpgradeAvailable", "status": "True", "reason": "New", "message": "" },
            { "type": "MigrationPending", "status": "True", "reason": "Await", "message": "" },
        ]}});
        assert!(unhealthy_condition_rows(&stack).is_empty());
    }

    #[test]
    fn an_unclassified_condition_is_always_surfaced() {
        // The failure this defends against is silence, not noise. A
        // condition type this build has never heard of may be bad news at
        // `True` or at `False`; guessing either way can hide it forever,
        // and a stale row a human has to classify is the cheaper mistake.
        for status in ["True", "False", "Unknown"] {
            let stack = json!({ "status": { "conditions": [
                { "type": "SomethingNewShipped", "status": status, "reason": "", "message": "" },
            ]}});
            let rows = unhealthy_condition_rows(&stack);
            assert_eq!(rows.len(), 1, "status={status} rows={rows:?}");
        }
    }

    #[test]
    fn unhealthy_conditions_of_a_healthy_stack_are_empty() {
        let stack = json!({ "status": { "conditions": [
            { "type": "Synced", "status": "True", "reason": "Ok", "message": "" },
            { "type": "UpstreamReachable", "status": "True", "reason": "Ok", "message": "" },
            { "type": "YankedVersion", "status": "False", "reason": "Clean", "message": "" },
        ]}});
        assert!(unhealthy_condition_rows(&stack).is_empty());
    }

    #[test]
    fn every_condition_type_the_operator_writes_is_classified() {
        // Completeness, DERIVED. The first version of this test carried a
        // hand-written list of six types taken from a grep, and asserted
        // that this build classified all six — which it did, because the
        // grep and the list were the same act. It could not see the two
        // types the grep had missed (`Ready`,
        // `UnauthorizedSourceModification`), so the roll-up shipped
        // reporting both of them as unhealthy on a healthy cluster and
        // this test stayed green through it.
        //
        // It now reads the operator's own declarations. The operator is a
        // separate cargo workspace, so this cannot be a compile-time
        // dependency — but it is the same repository, and the file below
        // is where every PlatformStack condition type is declared. Adding
        // a ninth there fails this test until somebody decides its
        // polarity here.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../operator/operator-controllers/platform-stack/src/status.rs");
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "{}: the operator's condition declarations could not be read, so this \
                 test would have judged nothing: {e}",
                path.display()
            )
        });
        let declared: Vec<String> = src
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub const COND_"))
            .filter_map(|l| l.split_once("= \""))
            .filter_map(|(_, rest)| rest.split_once('"'))
            .map(|(name, _)| name.to_string())
            .collect();
        // Non-vacuity: a rename of the `COND_` prefix, or a move of the
        // file, would otherwise leave this test asserting over an empty
        // list and reporting success.
        assert!(
            declared.len() >= 8,
            "only {declared:?} parsed out of {} — the declaration shape changed, and an \
             empty list would have passed",
            path.display()
        );
        for name in &declared {
            assert!(
                condition_polarity(name).is_some(),
                "`{name}` is declared by the platform-stack controller and this build \
                 does not classify it, so `apprafter status` would report it as a problem \
                 whatever its value"
            );
        }
    }

    #[test]
    fn the_two_conditions_a_healthy_cluster_carries_are_not_problems() {
        // The live regression, as observed: a healthy cluster reported
        // "2 condition(s) not healthy" naming `Ready=True` (the parent
        // Application IS healthy) and `UnauthorizedSourceModification=False`
        // (no foreign writer HAS been detected). Both readings were
        // inverted, both by the same omission.
        let stack = json!({ "status": { "conditions": [
            { "type": "Ready", "status": "True",
              "reason": "Healthy", "message": "parent platform Application reports Healthy" },
            { "type": "UnauthorizedSourceModification", "status": "False",
              "reason": "Clean", "message": "no foreign writer detected on spec.source" },
        ]}});
        assert!(unhealthy_condition_rows(&stack).is_empty());
    }

    #[test]
    fn the_same_two_conditions_are_problems_when_they_invert() {
        // The other half, which is what makes the test above a
        // classification and not a suppression.
        let stack = json!({ "status": { "conditions": [
            { "type": "Ready", "status": "False",
              "reason": "Degraded", "message": "parent platform Application is Degraded" },
            { "type": "UnauthorizedSourceModification", "status": "True",
              "reason": "ForeignWriter", "message": "spec.source was written by argocd" },
        ]}});
        assert_eq!(unhealthy_condition_rows(&stack).len(), 2);
    }

    fn frozen_now() -> DateTime<Utc> {
        // A fixed reference moment so relative-date tests
        // don't drift with wall-clock — picks a known
        // RFC3339 timestamp the helpers can subtract from.
        Utc.with_ymd_and_hms(2026, 5, 24, 14, 30, 0).unwrap()
    }

    #[test]
    fn print_status_handles_minimal_object() {
        // PlatformStack with only spec, no status (fresh CR).
        // Must not panic; should gracefully print "(unset)" /
        // "(none)" placeholders.
        let obj = json!({
            "spec": { "channel": "stable", "values": { "tier": 1 } }
        });
        print_status(&obj, frozen_now());
    }

    #[test]
    fn print_status_renders_full_object() {
        // Smoke test for the happy path — all sections populated.
        let obj = json!({
            "spec": {
                "channel": "stable",
                "autoUpgrade": true,
                "pin": "0.1.35",
                "values": { "tier": 1 }
            },
            "status": {
                "currentVersion": "0.1.35",
                "targetVersion": "0.1.35",
                "availableVersion": "0.1.38",
                "lastUpstreamCheck": "2026-05-23T22:30:00Z",
                "conditions": [
                    { "type": "Ready", "status": "True", "reason": "Healthy", "message": "ok" }
                ],
                "versionHistory": [
                    { "appliedAt": "2026-05-23T21:00:00Z", "version": "0.1.34", "outcome": "succeeded" }
                ]
            }
        });
        print_status(&obj, frozen_now());
    }

    #[test]
    fn format_timestamp_renders_absolute_and_relative() {
        // 90 minutes before the frozen `now` reference. The
        // absolute prefix is the parsed UTC moment; the
        // relative suffix is rounded to the most relevant
        // unit ("2 hours ago" matches the 1.5h → 2h round).
        let ts = "2026-05-24T13:00:00Z";
        let s = format_timestamp_with_relative(ts, frozen_now());
        assert!(s.starts_with("2026-05-24 13:00 UTC"), "got: {s}");
        assert!(s.contains("ago"), "expected past tense: {s}");
        assert!(s.contains("hour"), "expected hour unit: {s}");
    }

    #[test]
    fn format_timestamp_handles_unparseable_input() {
        // Unparseable input must surface verbatim so operators
        // don't lose information. Audit value > prettiness.
        let s = format_timestamp_with_relative("not-a-date", frozen_now());
        assert_eq!(s, "not-a-date");
    }

    #[test]
    fn humanise_relative_uses_just_now_under_45_seconds() {
        // Sub-minute precision is noise for platform events.
        // Up to 45s past or future renders as "just now" /
        // "in a few seconds" — keeps the output uncluttered.
        let now = frozen_now();
        let ten_seconds_ago = now - chrono::Duration::seconds(10);
        let s = format_timestamp_with_relative(&ten_seconds_ago.to_rfc3339(), now);
        assert!(s.contains("just now"), "got: {s}");
    }

    #[test]
    fn humanise_relative_handles_minutes_hours_days_months_years() {
        // Span coverage across every unit branch — guards
        // against an accidental thresholding regression that
        // could surface "60 minutes ago" instead of "1 hour
        // ago".
        let now = frozen_now();
        let cases = [
            (chrono::Duration::minutes(5), "5 minutes ago"),
            (chrono::Duration::hours(3), "3 hours ago"),
            (chrono::Duration::days(2), "2 days ago"),
            (chrono::Duration::days(45), "1 month ago"),
            (chrono::Duration::days(400), "1 year ago"),
        ];
        for (delta, expected_suffix) in cases {
            let ts = (now - delta).to_rfc3339();
            let s = format_timestamp_with_relative(&ts, now);
            assert!(
                s.contains(expected_suffix),
                "for delta {delta:?}, expected '{expected_suffix}' in '{s}'"
            );
        }
    }

    #[test]
    fn collect_history_rows_sorts_by_applied_at_desc() {
        // Source data deliberately out-of-order to assert the
        // sort actually fires (instead of merely preserving
        // declaration order which happens to be sorted).
        let status = json!({
            "versionHistory": [
                { "appliedAt": "2026-05-20T10:00:00Z", "version": "0.1.30", "outcome": "succeeded" },
                { "appliedAt": "2026-05-24T10:00:00Z", "version": "0.1.40", "outcome": "succeeded" },
                { "appliedAt": "2026-05-22T10:00:00Z", "version": "0.1.35", "outcome": "succeeded" }
            ]
        });
        let rows = collect_history_rows(&status, frozen_now(), 10);
        let versions: Vec<&str> = rows.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(versions, vec!["0.1.40", "0.1.35", "0.1.30"]);
    }

    #[test]
    fn collect_history_rows_puts_unparseable_timestamps_last() {
        // Corrupt / mid-write CRs (timestamp not yet stamped)
        // shouldn't dominate the visible head of the table.
        let status = json!({
            "versionHistory": [
                { "appliedAt": "garbage", "version": "0.0.0", "outcome": "succeeded" },
                { "appliedAt": "2026-05-24T10:00:00Z", "version": "0.1.40", "outcome": "succeeded" }
            ]
        });
        let rows = collect_history_rows(&status, frozen_now(), 10);
        // First row must be the parseable one; the unparseable
        // entry sinks to the bottom.
        assert_eq!(rows.first().map(|r| r.version.as_str()), Some("0.1.40"));
        assert_eq!(rows.get(1).map(|r| r.version.as_str()), Some("0.0.0"));
    }

    #[test]
    fn resolve_effective_pin_prefers_platformstack_status() {
        // Both sources present — PlatformStack status wins
        // because it's the operator's canonical version dial
        // and the Argo CD targetRevision may lag during a
        // bump cycle (in-flight reconcile shows the OLD
        // version on app.spec.source.targetRevision until
        // the umbrella patches it).
        let stack = json!({
            "status": { "componentVersions": { "cilium": "1.16.5" } }
        });
        let app = json!({
            "spec": { "source": { "targetRevision": "1.15.0" } }
        });
        let pin = resolve_effective_pin(&stack, Some(&app), "cilium").unwrap();
        assert_eq!(pin, "1.16.5");
    }

    #[test]
    fn resolve_effective_pin_falls_back_to_argocd_target_revision() {
        // Operator hits `freeze` on a cluster where the
        // operator binary doesn't (yet) write
        // componentVersions — this is the M1.5 default state.
        // The Argo CD Application's targetRevision is THE
        // authoritative version Argo CD is actively
        // reconciling against, so fall back to it instead
        // of erroring.
        let stack = json!({ "status": {} });
        let app = json!({
            "spec": { "source": { "targetRevision": "v1.16.5" } }
        });
        let pin = resolve_effective_pin(&stack, Some(&app), "cilium").unwrap();
        assert_eq!(pin, "v1.16.5");
    }

    #[test]
    fn resolve_effective_pin_errors_when_both_sources_empty() {
        // Neither PlatformStack.status nor Argo CD Application
        // carries a version — likely an unknown component
        // name OR a half-bootstrapped cluster. Error message
        // must point operators at the `--version <v>` escape
        // hatch and `platform status` for the canonical
        // component list.
        let stack = json!({ "status": {} });
        let err = resolve_effective_pin(&stack, None, "ghost-component")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("ghost-component"),
            "must surface component name: {err}"
        );
        assert!(err.contains("--version"), "must hint at --version: {err}");
        assert!(
            err.contains("platform status"),
            "must hint at platform status: {err}"
        );
    }

    #[test]
    fn resolve_effective_pin_handles_argocd_app_without_target_revision() {
        // Defensive: malformed Argo CD CR (e.g. mid-edit
        // with empty spec.source) is treated as "no signal"
        // — equivalent to passing None.
        let stack = json!({ "status": {} });
        let app = json!({ "spec": { "source": {} } });
        assert!(resolve_effective_pin(&stack, Some(&app), "x").is_err());
    }

    #[test]
    fn collect_history_rows_caps_at_take() {
        let entries: Vec<_> = (0..10)
            .map(|i| {
                json!({
                    "appliedAt": format!("2026-05-{:02}T10:00:00Z", 10 + i),
                    "version": format!("0.1.{i}"),
                    "outcome": "succeeded"
                })
            })
            .collect();
        let status = json!({ "versionHistory": entries });
        let rows = collect_history_rows(&status, frozen_now(), 3);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn validate_egress_profile_accepts_three_and_rejects_other() {
        // 2.10: the only valid presets are internet|internal|strict
        // (mirrors the webhook enum). Anything else — e.g. "open" —
        // is rejected client-side with a clear message rather than
        // degrading into an apiserver/webhook rejection.
        assert!(validate_egress_profile("internet").is_ok());
        assert!(validate_egress_profile("internal").is_ok());
        assert!(validate_egress_profile("strict").is_ok());

        let err = validate_egress_profile("open").unwrap_err().to_string();
        assert!(err.contains("open"), "must echo the bad value: {err}");
        assert!(
            err.contains("internet") && err.contains("internal") && err.contains("strict"),
            "must list the valid presets: {err}"
        );
    }

    #[test]
    fn format_egress_profile_reports_explicit_value() {
        let obj = json!({
            "spec": { "network": { "egress": { "profile": "strict" } } }
        });
        let s = format_egress_profile(&obj);
        assert!(s.contains("strict"), "must surface the set profile: {s}");
        // The legend lists all three presets regardless of which is active.
        assert!(s.contains("internet"), "legend must list internet: {s}");
        assert!(s.contains("internal"), "legend must list internal: {s}");
        assert!(s.contains("needs"), "legend must mention needs: {s}");
    }

    #[test]
    fn format_egress_profile_falls_back_to_internet_default_when_unset() {
        // Field absent (the common case — CLI-bootstrap-seeded CR
        // ships without the field) → report the documented operator
        // default `internet`, flagged as unset so it's not mistaken
        // for an explicit set.
        let obj = json!({ "spec": {} });
        let s = format_egress_profile(&obj);
        assert!(s.contains("internet"), "must default to internet: {s}");
        assert!(
            s.to_lowercase().contains("default") || s.to_lowercase().contains("unset"),
            "must flag the value as the unset default: {s}"
        );
    }

    #[test]
    fn egress_field_git_managed_detects_argocd_owner() {
        // Argo CD owns the egress subtree → git-managed.
        let owned_by_argo = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "argocd-application-controller",
                        "fieldsV1": { "f:spec": { "f:network": { "f:egress": {} } } }
                    }
                ]
            }
        });
        assert!(egress_field_appears_git_managed(&owned_by_argo));
    }

    #[test]
    fn egress_field_git_managed_false_when_only_cli_owns_or_absent() {
        // apprafter-cli's own SSA ownership must NOT count as
        // git-managed (else `set` would always warn after the
        // first run). And a CR with no managedFields at all → false.
        let owned_by_cli = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "apprafter-cli",
                        "fieldsV1": { "f:spec": { "f:network": { "f:egress": {} } } }
                    }
                ]
            }
        });
        assert!(!egress_field_appears_git_managed(&owned_by_cli));
        assert!(!egress_field_appears_git_managed(
            &json!({ "metadata": {} })
        ));
        // Argo CD owns OTHER fields but not egress → false.
        let argo_other = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "argocd-application-controller",
                        "fieldsV1": { "f:spec": { "f:channel": {} } }
                    }
                ]
            }
        });
        assert!(!egress_field_appears_git_managed(&argo_other));
    }

    #[test]
    fn recheck_annotation_patch_body_is_well_formed_merge_patch() {
        // The body must be a valid RFC-7396 merge patch nesting the
        // request timestamp under metadata.annotations[<annotation>].
        let ts = "2026-06-11T12:00:00+00:00";
        let body = recheck_annotation_patch_body(ts);
        let parsed: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(
            parsed.pointer(&format!(
                "/metadata/annotations/{}",
                RECHECK_REQUESTED_ANNOTATION.replace('/', "~1")
            )),
            Some(&Value::String(ts.to_string())),
            "body must stamp the request ts under the recheck annotation: {body}"
        );
    }

    #[test]
    fn recheck_completed_true_only_when_last_check_strictly_after_request() {
        let requested = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();

        // Strictly after → the operator polled in response → done.
        assert!(recheck_completed(Some("2026-06-11T12:00:01Z"), requested));
        // A minute later, different-but-equivalent zone form → done.
        assert!(recheck_completed(
            Some("2026-06-11T13:01:00+01:00"),
            requested
        ));
    }

    #[test]
    fn recheck_completed_false_when_stale_equal_absent_or_unparseable() {
        let requested = Utc.with_ymd_and_hms(2026, 6, 11, 12, 0, 0).unwrap();

        // Older than the request — pre-existing 6h-cadence value the
        // operator hasn't refreshed yet.
        assert!(!recheck_completed(Some("2026-06-11T11:59:59Z"), requested));
        // Exactly equal must NOT count (operator stamps strictly later).
        assert!(!recheck_completed(Some("2026-06-11T12:00:00Z"), requested));
        // No lastUpstreamCheck at all (fresh CR / pre-contract operator).
        assert!(!recheck_completed(None, requested));
        // Mid-write garbage reads as not-yet-done, not a crash.
        assert!(!recheck_completed(Some("not-a-timestamp"), requested));
    }

    #[test]
    fn validate_env_trims_and_accepts() {
        assert_eq!(validate_env("staging").unwrap(), "staging");
        assert_eq!(validate_env("  prod ").unwrap(), "prod");
    }

    #[test]
    fn validate_env_rejects_empty_and_whitespace() {
        assert!(validate_env("").is_err());
        assert!(validate_env("   ").is_err());
    }

    #[test]
    fn default_environment_patch_body_shape() {
        let body = default_environment_patch_body("staging");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["spec"]["defaultEnvironment"], "staging");
        assert_eq!(v["spec"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn egress_set_field_manager_is_distinct_from_bootstrap_and_not_argo_shaped() {
        // Walk-found regression (2.10 Phase 7): `cluster-bootstrap` seeds the
        // singleton PlatformStack (with the REQUIRED spec.source + spec.values)
        // under APPRAFTER_CLI_FIELD_MANAGER. `egress set` applies ONLY
        // {spec.network.egress.profile}; if it shared that manager, server-side
        // apply would PRUNE source/values and the apiserver would reject the CR
        // ("Required value"). The two managers MUST differ.
        assert_ne!(
            APPRAFTER_CLI_EGRESS_FIELD_MANAGER, APPRAFTER_CLI_FIELD_MANAGER,
            "egress set must not reuse the bootstrap field manager (SSA would prune source/values)"
        );
        // And the egress manager must NOT look Argo-CD-shaped, or
        // egress_field_appears_git_managed would mis-flag it as git-owned and
        // every `set` would print the spurious "git wins" advisory.
        for needle in ["argocd", "argo-cd", "application-controller"] {
            assert!(
                !APPRAFTER_CLI_EGRESS_FIELD_MANAGER.contains(needle),
                "egress field manager must not contain Argo-shaped substring {needle:?}"
            );
        }
    }

    #[test]
    fn autoscale_patch_body_is_path_scoped() {
        // The merge-patch must touch ONLY the autoscale.mode leaf — a
        // shallow `spec: { mode }` body would clobber other spec fields
        // on the PlatformStack that the CLI doesn't own.
        assert_eq!(
            autoscale_patch_body("up-only"),
            r#"{"spec":{"resources":{"autoscale":{"mode":"up-only"}}}}"#
        );
    }

    #[test]
    fn autoscale_validates_mode() {
        // All three documented presets must pass; anything else is
        // rejected client-side before touching the apiserver.
        assert!(validate_autoscale_mode("full").is_ok());
        assert!(validate_autoscale_mode("up-only").is_ok());
        assert!(validate_autoscale_mode("off").is_ok());
        assert!(validate_autoscale_mode("bogus").is_err());
        // Error must echo the bad value.
        let err = validate_autoscale_mode("auto").unwrap_err().to_string();
        assert!(err.contains("auto"), "must echo bad value: {err}");
        assert!(
            err.contains("full") && err.contains("up-only") && err.contains("off"),
            "must list valid presets: {err}"
        );
    }
}
