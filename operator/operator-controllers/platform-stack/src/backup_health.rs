// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `BackupHealthy`: whether the scheduled off-site backup can run, read from
//! Kubernetes' own objects.
//!
//! # Why not from the runner's record
//!
//! The backup runner writes its own outcome to the `apprafter-backup-status`
//! ConfigMap, and every failure the runner can see ends up there. The
//! failures that matter most here are the ones it cannot see, because it is
//! not running when they happen:
//!
//! - its pod is never scheduled. On a 4 GB node with `needs.pg` and a
//!   persistent `needs.redis`, the pod stayed `Pending` all night with
//!   `FailedScheduling: 0/1 nodes are available: 1 Insufficient memory`;
//! - the kernel kills it at its memory limit (`OOMKilled`). The whole
//!   container goes, so nothing is written;
//! - the kubelet evicts it, for node memory pressure or for staging past the
//!   `/staging` emptyDir's `sizeLimit`;
//! - the Job's deadline passes while the pod was never placed;
//! - the Job controller cannot create the pod at all (a quota, a
//!   LimitRange, an admission webhook), which only its events record;
//! - every attempt fails and the Job gives up (`BackoffLimitExceeded`).
//!
//! In each of those the ConfigMap keeps saying whatever the last run that
//! did start said, which can be a success. So the condition is built from
//! what Kubernetes records instead: the two CronJobs, their Jobs'
//! `Complete`/`Failed`/`FailureTarget` conditions, and their pods'
//! `PodScheduled` condition, container states and eviction reason. The
//! runner's record is read only to add its own words to a failure.
//!
//! # The verdict
//!
//! - **Absent** while `spec.backup.enabled` is not true. A cluster that did
//!   not ask for backups is not told they are missing.
//! - **True** when the most recent finished backup succeeded, the most
//!   recent finished check passed (or none has run yet), and no unfinished
//!   run is in trouble.
//! - **False** when a run is in trouble: named by `reason`, with a `message`
//!   that names the Job, the pod and the cause.
//! - **Unknown** when nothing can be judged yet: no run has finished, the
//!   CronJob has not been deployed, or the objects could not be read. Never
//!   `True` on no evidence.
//!
//! # No flapping
//!
//! A pod that has not been placed, or has not started, becomes trouble only
//! after [`NOT_STARTED_GRACE_SECS`]. The scheduler places a waiting pod as
//! soon as room appears, and room is often on its way: a CNPG instance being
//! deleted takes up to three minutes to stop, the kubelet keeps a memory- or
//! disk-pressure taint for five minutes after the pressure ends, and a node
//! that restarts is not ready for a few minutes. A pod the scheduler is
//! making room for by preemption (`status.nominatedNodeName`) is not trouble
//! at all. A container that dies (`OOMKilled`, `Evicted`) is trouble at once:
//! that attempt has failed and nothing will undo it.
//!
//! Messages are built from the objects' own timestamps, never from "how long
//! ago", so re-reading unchanged objects gives a byte-identical condition and
//! the status write is skipped.
//!
//! Pure: [`assess`] is a function of the objects (as JSON), the previous
//! condition and the instant it is given. [`observe`] does the reads.
//!
//! # Beside it: retention
//!
//! `BackupHealthy` answers one question: do the scheduled runs run, and do
//! they succeed. The weekly check belongs to it — a check that does not
//! pass (`RepositoryCheckFailed`) says the repository cannot be trusted,
//! which is a failing backup in every sense that matters.
//!
//! Whether retention is enforced is a different question and gets its own
//! condition, `BackupRetention`, rather than more reasons here. The
//! in-cluster key that ADR 0050 recommends is scoped so that it cannot
//! delete, and a cluster running on it can never prune: folded into this
//! condition, that would be a permanent `False` on the recommended setup,
//! and a real failure (a pod no node takes) would either hide behind it or
//! push it out of sight. Two conditions keep both visible at once.
//!
//! The model it follows is this one: the same reconcile writes it from the
//! same [`Observed`] (the runner's status ConfigMap is already read, and
//! is where a prune's outcome and the repository's size belong), through
//! [`apply`] with its own type, in the same status write. One writer
//! matters: `status.conditions` is an atomic list under the one field
//! manager, so a condition written by anything else would need the CRD to
//! declare it a list-map keyed by `type` first.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use k8s_openapi::api::batch::v1::{CronJob, Job};
use k8s_openapi::api::core::v1::{ConfigMap, Pod};
use kube::api::{Api, ListParams};
use kube::Client;
use operator_core::{PlatformStackCondition, PlatformStackStatus};
use serde_json::Value;

use crate::status::{condition, upsert_condition, COND_BACKUP_HEALTHY};

/// Where the platform chart deploys both CronJobs, their Jobs and their pods.
pub const BACKUP_NAMESPACE: &str = "apprafter-system";
/// The nightly backup CronJob (`templates/backup.yaml`).
pub const BACKUP_CRONJOB: &str = "apprafter-backup";
/// The weekly `restic check` CronJob. Absent when `checkSchedule` is empty.
pub const CHECK_CRONJOB: &str = "apprafter-backup-check";
/// Name prefix of a Job `apprafter backup run` creates. Such a Job has no
/// owner (it must outlive the CronJob), so it is recognised by this prefix
/// together with the `apprafter.io/manual` label.
pub const MANUAL_JOB_PREFIX: &str = "apprafter-backup-manual-";
/// Label the chart puts on both CronJobs' pod templates, which `backup run`
/// copies. Selecting on it keeps the pod watch to the runner pods.
pub const RUNNER_POD_SELECTOR: &str = "apprafter.io/backup-runner=true";
/// The runner's own record of its last run.
pub const RUNNER_STATUS_CONFIGMAP: &str = "apprafter-backup-status";

/// How long a runner pod may stay unplaced, or placed with its container not
/// started, before the condition says so. See the module docs for what the
/// ten minutes cover. A nightly job loses nothing by being reported ten
/// minutes late; a condition that flips on every rolling upgrade teaches its
/// reader to ignore it.
pub const NOT_STARTED_GRACE_SECS: i64 = 600;

pub const REASON_SUCCEEDED: &str = "Succeeded";
pub const REASON_NO_RUN_YET: &str = "NoRunYet";
pub const REASON_NOT_DEPLOYED: &str = "ScheduleNotDeployed";
pub const REASON_UNREADABLE: &str = "StateUnreadable";
pub const REASON_SUSPENDED: &str = "ScheduleSuspended";
pub const REASON_UNSCHEDULABLE: &str = "RunnerUnschedulable";
pub const REASON_NOT_STARTED: &str = "RunnerNotStarted";
pub const REASON_OOM_KILLED: &str = "RunnerOOMKilled";
pub const REASON_EVICTED: &str = "RunnerEvicted";
pub const REASON_DEADLINE_EXCEEDED: &str = "DeadlineExceeded";
pub const REASON_BACKOFF_LIMIT: &str = "BackoffLimitExceeded";
/// The weekly check Job gave up with every attempt failing by itself:
/// `restic check` did not pass. Kept apart from [`REASON_BACKOFF_LIMIT`]
/// because it is the one reason about the repository rather than the run.
pub const REASON_CHECK_FAILED: &str = "RepositoryCheckFailed";
pub const REASON_FAILED: &str = "Failed";

/// Longest piece of the runner's own `lastError` quoted in a message.
const RUNNER_ERROR_QUOTE_MAX: usize = 300;

/// Kubernetes' default `backoffLimit`, for a Job read without one.
const DEFAULT_BACKOFF_LIMIT: u64 = 6;

/// The objects [`assess`] reads, as JSON.
#[derive(Debug, Clone, Default)]
pub struct Observed {
    /// Every CronJob in [`BACKUP_NAMESPACE`].
    pub cronjobs: Vec<Value>,
    /// Every Job in [`BACKUP_NAMESPACE`].
    pub jobs: Vec<Value>,
    /// The pods matching [`RUNNER_POD_SELECTOR`] there.
    pub pods: Vec<Value>,
    /// `data` of [`RUNNER_STATUS_CONFIGMAP`], when it exists and was read.
    pub runner_status: Option<Value>,
}

/// What the stack's `BackupHealthy` condition should be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Backups are off: the condition must not be on the stack at all.
    Absent,
    /// The condition, without its transition time (see [`apply`]).
    Condition {
        status: &'static str,
        reason: &'static str,
        message: String,
    },
}

/// A verdict, and when it could next change without any object changing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub verdict: Verdict,
    /// Set while a pod is inside [`NOT_STARTED_GRACE_SECS`]: when the grace
    /// ends the verdict may change, and no watch event will say so.
    pub recheck_after: Option<StdDuration>,
}

impl Assessment {
    fn now(verdict: Verdict) -> Self {
        Self {
            verdict,
            recheck_after: None,
        }
    }
}

/// Read what [`assess`] needs. An error names what could not be read; the
/// runner's ConfigMap is best-effort and never an error.
pub async fn observe(client: &Client) -> Result<Observed, String> {
    let lp = ListParams::default();
    let cronjobs = Api::<CronJob>::namespaced(client.clone(), BACKUP_NAMESPACE)
        .list(&lp)
        .await
        .map_err(|e| format!("listing CronJobs in {BACKUP_NAMESPACE}: {e}"))?;
    let jobs = Api::<Job>::namespaced(client.clone(), BACKUP_NAMESPACE)
        .list(&lp)
        .await
        .map_err(|e| format!("listing Jobs in {BACKUP_NAMESPACE}: {e}"))?;
    let pods = Api::<Pod>::namespaced(client.clone(), BACKUP_NAMESPACE)
        .list(&ListParams::default().labels(RUNNER_POD_SELECTOR))
        .await
        .map_err(|e| format!("listing the runner pods in {BACKUP_NAMESPACE}: {e}"))?;
    // Enrichment only: a missing or unreadable record leaves the message
    // without the runner's own words, and changes no verdict.
    let runner_status = Api::<ConfigMap>::namespaced(client.clone(), BACKUP_NAMESPACE)
        .get_opt(RUNNER_STATUS_CONFIGMAP)
        .await
        .ok()
        .flatten()
        .and_then(|cm| cm.data)
        .and_then(|d| serde_json::to_value(d).ok());
    Ok(Observed {
        cronjobs: as_json(&cronjobs.items),
        jobs: as_json(&jobs.items),
        pods: as_json(&pods.items),
        runner_status,
    })
}

/// The typed objects as the JSON [`assess`] reads. A typed k8s-openapi
/// object always serializes, so nothing is dropped in practice.
fn as_json<T: serde::Serialize>(items: &[T]) -> Vec<Value> {
    items
        .iter()
        .filter_map(|o| serde_json::to_value(o).ok())
        .collect()
}

/// Put `verdict` on `status` as the condition `type_`: remove it for
/// [`Verdict::Absent`], otherwise upsert it through [`condition`], which
/// keeps the prior `lastTransitionTime` while the status value is
/// unchanged. So "since" on a `False` condition is when backups started
/// failing, and stays there while the cause changes (unschedulable, then
/// the deadline).
///
/// Takes the type rather than assuming [`COND_BACKUP_HEALTHY`] so a second
/// backup condition (see "Beside it" in the module docs) goes on the stack
/// the same way, in the same write.
pub fn apply(
    status: &mut PlatformStackStatus,
    type_: &str,
    verdict: &Verdict,
    prior: &[PlatformStackCondition],
) {
    match verdict {
        Verdict::Absent => {
            if let Some(conds) = status.conditions.as_mut() {
                conds.retain(|c| c.type_ != type_);
            }
        }
        Verdict::Condition {
            status: s,
            reason,
            message,
        } => upsert_condition(status, condition(type_, s, reason, message, prior)),
    }
}

/// The verdict for a stack whose `spec.backup.enabled` is `enabled`, given
/// what [`observe`] returned, the stack's current conditions and `now`.
pub fn assess(
    enabled: bool,
    observed: Result<&Observed, &str>,
    prior: &[PlatformStackCondition],
    now: DateTime<Utc>,
) -> Assessment {
    if !enabled {
        return Assessment::now(Verdict::Absent);
    }
    let observed = match observed {
        Ok(o) => o,
        // Never keep a stale `True` on a cluster we cannot see into.
        Err(e) => {
            return Assessment::now(unknown(
                REASON_UNREADABLE,
                format!("could not read the backup Jobs: {e}"),
            ))
        }
    };
    let prior = prior.iter().find(|c| c.type_ == COND_BACKUP_HEALTHY);
    let cronjob = |name: &str| {
        observed
            .cronjobs
            .iter()
            .find(|c| str_at(c, "/metadata/name") == Some(name))
    };
    let Some(backup_cronjob) = cronjob(BACKUP_CRONJOB) else {
        return Assessment::now(unknown(
            REASON_NOT_DEPLOYED,
            format!(
                "spec.backup.enabled is true, but CronJob {BACKUP_NAMESPACE}/{BACKUP_CRONJOB} \
                 does not exist: the platform chart has not deployed the schedule"
            ),
        ));
    };
    let backup = assess_run(Run::Backup, backup_cronjob, observed, prior, now);
    let check = cronjob(CHECK_CRONJOB).map(|c| assess_run(Run::Check, c, observed, prior, now));

    let recheck_at = [Some(&backup), check.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(|r| r.recheck_at)
        .min();
    let recheck_after = recheck_at.map(|at| {
        // At least a second, so an instant already past cannot spin the
        // controller; the extra second lands the recheck after the edge.
        let secs = (at - now).num_seconds().max(0) + 1;
        StdDuration::from_secs(u64::try_from(secs).unwrap_or(1))
    });

    let verdict = match (backup.outcome, check.map(|c| c.outcome)) {
        (Outcome::Trouble { reason, message }, _)
        | (_, Some(Outcome::Trouble { reason, message })) => Verdict::Condition {
            status: "False",
            reason,
            message,
        },
        (Outcome::NoRun { message }, _) => unknown(REASON_NO_RUN_YET, message),
        (Outcome::Succeeded { message }, Some(Outcome::Succeeded { message: check })) => {
            Verdict::Condition {
                status: "True",
                reason: REASON_SUCCEEDED,
                message: format!("{message}; {check}"),
            }
        }
        (Outcome::Succeeded { message }, _) => Verdict::Condition {
            status: "True",
            reason: REASON_SUCCEEDED,
            message,
        },
    };
    Assessment {
        verdict,
        recheck_after,
    }
}

fn unknown(reason: &'static str, message: String) -> Verdict {
    Verdict::Condition {
        status: "Unknown",
        reason,
        message,
    }
}

/// Which of the two CronJobs a run belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Run {
    Backup,
    Check,
}

impl Run {
    fn cronjob(self) -> &'static str {
        match self {
            Run::Backup => BACKUP_CRONJOB,
            Run::Check => CHECK_CRONJOB,
        }
    }

    /// What a message calls one run.
    fn noun(self) -> &'static str {
        match self {
            Run::Backup => "backup",
            Run::Check => "repository check",
        }
    }

    /// Where the runner's status ConfigMap records this run's failure, as
    /// `(time key, error key)`, so a failed Job can quote the runner's own
    /// words. The check Job runs `restic check` directly and writes no
    /// record, so a failed check quotes nothing; once it records its own
    /// outcome, its keys go here and every failure message picks them up.
    fn record_keys(self) -> Option<(&'static str, &'static str)> {
        match self {
            Run::Backup => Some(("lastFailure", "lastError")),
            Run::Check => None,
        }
    }

    /// Does `job` belong to this CronJob? Its own Jobs carry a controller
    /// reference to it (as does `kubectl create job --from=cronjob/…`). A
    /// backup Job `apprafter backup run` created has no owner, so it is
    /// recognised by name and label; it is evidence about the schedule
    /// because it runs the schedule's own Job template.
    fn owns(self, job: &Value) -> bool {
        let owner = cronjob_owner(job);
        if let Some(owner) = owner {
            return owner == self.cronjob();
        }
        self == Run::Backup
            && str_at(job, "/metadata/name").is_some_and(|n| n.starts_with(MANUAL_JOB_PREFIX))
            && str_at(job, "/metadata/labels/apprafter.io~1manual") == Some("true")
    }
}

fn cronjob_owner(job: &Value) -> Option<&str> {
    job.pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)?
        .iter()
        .find(|r| r.get("kind").and_then(Value::as_str) == Some("CronJob"))
        .and_then(|r| r.get("name").and_then(Value::as_str))
}

/// One CronJob's contribution to the verdict.
enum Outcome {
    Trouble {
        reason: &'static str,
        message: String,
    },
    Succeeded {
        message: String,
    },
    NoRun {
        message: String,
    },
}

struct RunAssessment {
    outcome: Outcome,
    recheck_at: Option<DateTime<Utc>>,
}

/// How a finished Job ended.
enum Ending<'a> {
    Succeeded {
        at: Option<&'a str>,
    },
    Failed {
        reason: &'a str,
        message: &'a str,
        at: Option<&'a str>,
    },
}

/// A Job's ending, or `None` while it runs. `FailureTarget` and
/// `SuccessCriteriaMet` are the conditions the Job controller sets first,
/// before its pods are gone; they are already final.
fn ending(job: &Value) -> Option<Ending<'_>> {
    let conds = job
        .pointer("/status/conditions")
        .and_then(Value::as_array)?;
    let find = |types: &[&str]| {
        types.iter().find_map(|t| {
            conds.iter().find(|c| {
                c.get("type").and_then(Value::as_str) == Some(t)
                    && c.get("status").and_then(Value::as_str) == Some("True")
            })
        })
    };
    if let Some(c) = find(&["Failed", "FailureTarget"]) {
        return Some(Ending::Failed {
            reason: c.get("reason").and_then(Value::as_str).unwrap_or(""),
            message: c.get("message").and_then(Value::as_str).unwrap_or(""),
            at: c.get("lastTransitionTime").and_then(Value::as_str),
        });
    }
    find(&["Complete", "SuccessCriteriaMet"]).map(|c| Ending::Succeeded {
        at: str_at(job, "/status/completionTime")
            .or_else(|| c.get("lastTransitionTime").and_then(Value::as_str)),
    })
}

fn assess_run(
    run: Run,
    cronjob: &Value,
    observed: &Observed,
    prior: Option<&PlatformStackCondition>,
    now: DateTime<Utc>,
) -> RunAssessment {
    let noun = run.noun();
    let cron = run.cronjob();
    if cronjob.pointer("/spec/suspend").and_then(Value::as_bool) == Some(true) {
        return RunAssessment {
            outcome: Outcome::Trouble {
                reason: REASON_SUSPENDED,
                message: format!(
                    "CronJob {BACKUP_NAMESPACE}/{cron} is suspended: no scheduled {noun} runs \
                     until its spec.suspend is cleared"
                ),
            },
            recheck_at: None,
        };
    }

    let mut jobs: Vec<&Value> = observed
        .jobs
        .iter()
        .filter(|j| run.owns(j))
        .filter(|j| j.pointer("/metadata/deletionTimestamp").is_none())
        .collect();
    // Newest first. The timestamp is parsed rather than compared as text,
    // and the name breaks a tie inside one second.
    jobs.sort_by(|a, b| {
        let key = |j: &Value| {
            (
                time_at(j, "/metadata/creationTimestamp"),
                name(j).to_string(),
            )
        };
        key(b).cmp(&key(a))
    });

    // An unfinished run in trouble is the most urgent thing to say: a
    // scheduled one holds the schedule while it waits.
    let mut recheck_at: Option<DateTime<Utc>> = None;
    let mut trouble: Option<Outcome> = None;
    for job in jobs.iter().filter(|j| ending(j).is_none()) {
        let (found, at) = unfinished_trouble(run, job, &observed.pods, now);
        recheck_at = earliest(recheck_at, at);
        if trouble.is_none() {
            trouble = found;
        }
    }
    if let Some(outcome) = trouble {
        return RunAssessment {
            outcome,
            recheck_at,
        };
    }

    let finished = jobs.iter().find_map(|j| ending(j).map(|e| (*j, e)));
    let outcome = match finished {
        Some((
            job,
            Ending::Failed {
                reason,
                message,
                at,
            },
        )) => failed_job(run, job, reason, message, at, observed, prior),
        Some((job, Ending::Succeeded { at })) => Outcome::Succeeded {
            message: format!(
                "the last {noun}, Job {}, succeeded at {}",
                name(job),
                at.unwrap_or("an unrecorded time")
            ),
        },
        None => no_finished_run(run, cronjob, &jobs),
    };
    RunAssessment {
        outcome,
        recheck_at,
    }
}

/// No finished Job to read. The CronJob's own record of its last success
/// outlives the Jobs it keeps (three of each), so it still counts.
fn no_finished_run(run: Run, cronjob: &Value, jobs: &[&Value]) -> Outcome {
    let noun = run.noun();
    let cron = run.cronjob();
    if let Some(at) = str_at(cronjob, "/status/lastSuccessfulTime") {
        return Outcome::Succeeded {
            message: format!(
                "the last scheduled {noun} succeeded at {at}; its Job is no longer kept"
            ),
        };
    }
    if let Some(job) = jobs.first() {
        return Outcome::NoRun {
            message: format!(
                "no {noun} has finished yet: Job {} has not finished",
                name(job)
            ),
        };
    }
    if let Some(at) = str_at(cronjob, "/status/lastScheduleTime") {
        return Outcome::NoRun {
            message: format!(
                "no {noun} Job is left to read, and none has succeeded: CronJob {cron} last \
                 started one at {at}"
            ),
        };
    }
    let schedule = str_at(cronjob, "/spec/schedule").unwrap_or("?");
    Outcome::NoRun {
        message: format!(
            "no {noun} has run yet: CronJob {cron} (schedule \"{schedule}\") has not started one"
        ),
    }
}

/// Trouble in a Job that has not finished, and when to look again if its pod
/// is inside the grace.
fn unfinished_trouble(
    run: Run,
    job: &Value,
    pods: &[Value],
    now: DateTime<Utc>,
) -> (Option<Outcome>, Option<DateTime<Utc>>) {
    let noun = run.noun();
    let job_name = name(job);
    let owned: Vec<&Value> = pods.iter().filter(|p| owned_by(p, job)).collect();
    let grace = Duration::seconds(NOT_STARTED_GRACE_SECS);
    // A scheduled Job holds the schedule while it waits: the CronJob is
    // `concurrencyPolicy: Forbid`. A manual one holds nothing.
    let holds = if cronjob_owner(job).is_some() {
        holds_the_schedule(noun)
    } else {
        String::new()
    };

    // When a wait that began at `since` stops being inside the grace, if it
    // has not stopped already.
    let not_yet = |since: Option<DateTime<Utc>>| -> Option<DateTime<Utc>> {
        let due = since.unwrap_or(now) + grace;
        (now < due).then_some(due)
    };

    let live = owned
        .iter()
        .filter(|p| !matches!(str_at(p, "/status/phase"), Some("Succeeded" | "Failed")))
        .filter(|p| p.pointer("/metadata/deletionTimestamp").is_none())
        .max_by_key(|p| time_at(p, "/metadata/creationTimestamp"));
    if let Some(pod) = live {
        let pod_name = name(pod);
        let created = time_at(pod, "/metadata/creationTimestamp");
        let placed = str_at(pod, "/spec/nodeName").is_some_and(|n| !n.is_empty());
        if !placed {
            let scheduled = pod_condition(pod, "PodScheduled");
            let unschedulable = scheduled.filter(|c| {
                c.get("status").and_then(Value::as_str) == Some("False")
                    && c.get("reason").and_then(Value::as_str) == Some("Unschedulable")
            });
            if let Some(cond) = unschedulable {
                if str_at(pod, "/status/nominatedNodeName").is_some_and(|n| !n.is_empty()) {
                    // Room is being made by preemption; the pod's next change
                    // (placement) wakes the controller.
                    return (None, None);
                }
                let since_raw = cond
                    .get("lastTransitionTime")
                    .and_then(Value::as_str)
                    .or_else(|| str_at(pod, "/metadata/creationTimestamp"));
                let since = since_raw.and_then(parse_time).or(created);
                if let Some(due) = not_yet(since) {
                    return (None, Some(due));
                }
                let said = cond
                    .get("message")
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty())
                    .unwrap_or("the scheduler gave no reason");
                let asks = requests(pod)
                    .map(|r| format!(" It asks for {r}."))
                    .unwrap_or_default();
                return (
                    Some(Outcome::Trouble {
                        reason: REASON_UNSCHEDULABLE,
                        message: format!(
                            "{noun} Job {job_name}: its pod {pod_name} has not been scheduled \
                             since {}: {said}.{asks}{holds}",
                            since_raw.unwrap_or("its creation"),
                            said = said.trim_end_matches('.'),
                        ),
                    }),
                    None,
                );
            }
            // Not tried yet, or held by a gate: counted from creation.
            if let Some(due) = not_yet(created) {
                return (None, Some(due));
            }
            let why = scheduled
                .and_then(|c| {
                    reason_with_message(
                        c.get("reason").and_then(Value::as_str).unwrap_or(""),
                        c.get("message").and_then(Value::as_str).unwrap_or(""),
                    )
                })
                .map(|w| format!(": {w}"))
                .unwrap_or_default();
            return (
                Some(Outcome::Trouble {
                    reason: REASON_NOT_STARTED,
                    message: format!(
                        "{noun} Job {job_name}: its pod {pod_name} has not been scheduled since \
                         {}{why}.{holds}",
                        str_at(pod, "/metadata/creationTimestamp").unwrap_or("its creation"),
                    ),
                }),
                None,
            );
        }
        if str_at(pod, "/status/phase") == Some("Pending") {
            // Placed, container not started. Counted from placement, which is
            // when the image pull and the volume mounts begin.
            let placed_at = pod_condition(pod, "PodScheduled")
                .filter(|c| c.get("status").and_then(Value::as_str) == Some("True"))
                .and_then(|c| c.get("lastTransitionTime").and_then(Value::as_str));
            let since = placed_at.and_then(parse_time).or(created);
            if let Some(due) = not_yet(since) {
                return (None, Some(due));
            }
            let node = str_at(pod, "/spec/nodeName").unwrap_or("?");
            let waiting = waiting_reason(pod)
                .map(|w| format!(": {w}"))
                .unwrap_or_default();
            return (
                Some(Outcome::Trouble {
                    reason: REASON_NOT_STARTED,
                    message: format!(
                        "{noun} Job {job_name}: its pod {pod_name} was placed on {node} at {} \
                         and its container has not started{waiting}.{holds}",
                        placed_at
                            .or_else(|| str_at(pod, "/metadata/creationTimestamp"))
                            .unwrap_or("an unrecorded time"),
                    ),
                }),
                None,
            );
        }
    }

    // No live pod in trouble. An attempt that already died of a cause the
    // runner cannot record is trouble now, even while the next attempt runs:
    // the same data meets the same limit.
    let last_failed = owned
        .iter()
        .filter(|p| str_at(p, "/status/phase") == Some("Failed"))
        .max_by_key(|p| time_at(p, "/metadata/creationTimestamp"));
    if let Some(pod) = last_failed {
        // The Job's own count lags its pods by a sync; the failed pods it
        // still has are the floor.
        let failed = failed_attempts(job, pods).max(1);
        let attempts = job
            .pointer("/spec/backoffLimit")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_BACKOFF_LIMIT)
            + 1;
        let prefix = format!("{noun} Job {job_name}: its attempt {failed} of at most {attempts}");
        let next = if failed < attempts {
            " The Job retries until its backoff limit."
        } else {
            " It was the last attempt the Job makes."
        };
        match attempt_cause(pod) {
            Cause::OomKilled(what) => {
                return (
                    Some(Outcome::Trouble {
                        reason: REASON_OOM_KILLED,
                        message: format!(
                            "{prefix} {what}; a killed runner records nothing itself.{next}"
                        ),
                    }),
                    None,
                )
            }
            Cause::Evicted(what) => {
                return (
                    Some(Outcome::Trouble {
                        reason: REASON_EVICTED,
                        message: format!(
                            "{prefix} {what}; an evicted runner records nothing itself.{next}"
                        ),
                    }),
                    None,
                )
            }
            // An ordinary non-zero exit: the runner recorded it itself, and
            // the retry may well succeed. The Job's own ending decides.
            Cause::Exited(_) | Cause::Unknown(_) => {}
        }
    }

    // No pod at all, and none the Job controller counts either: it has not
    // created one. A ResourceQuota or LimitRange the pod breaks, an
    // admission webhook that refuses it, or a missing ServiceAccount does
    // this, and the only record is the Job's `FailedCreate` events — the Job
    // itself says nothing until its deadline, six hours later. Counted from
    // the Job's creation; a pod that is created wakes the controller.
    //
    // The counts matter: a pod of this Job that the runner label does not
    // select, or one being replaced after a deletion, is counted as active
    // or failed, and is not "no pod".
    if owned.is_empty() && counts_no_pod(job) {
        let created_raw = str_at(job, "/metadata/creationTimestamp");
        if let Some(due) = not_yet(created_raw.and_then(parse_time)) {
            return (None, Some(due));
        }
        let why = if job.pointer("/spec/suspend").and_then(Value::as_bool) == Some(true) {
            "the Job is suspended (its spec.suspend is true)"
        } else {
            "the Job controller has not created one. The Job's FailedCreate events say why: a \
             quota or LimitRange the pod breaks, an admission webhook that refuses it, or a \
             missing ServiceAccount"
        };
        return (
            Some(Outcome::Trouble {
                reason: REASON_NOT_STARTED,
                message: format!(
                    "{noun} Job {job_name}: it has had no pod since it was created at {}: \
                     {why}.{holds}",
                    created_raw.unwrap_or("an unrecorded time"),
                ),
            }),
            None,
        );
    }
    (None, None)
}

/// Does the Job controller count no pod of `job` at all — none active, none
/// finished either way? Absent counts are zero, as the API omits them.
fn counts_no_pod(job: &Value) -> bool {
    ["/status/active", "/status/failed", "/status/succeeded"]
        .iter()
        .all(|p| job.pointer(p).and_then(Value::as_u64).unwrap_or(0) == 0)
}

/// How many attempts of `job` have failed: the Job's own count, or the
/// failed pods it still has when that is more (the count is updated a sync
/// after the pod ends).
fn failed_attempts(job: &Value, pods: &[Value]) -> u64 {
    let counted = job
        .pointer("/status/failed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let seen = pods
        .iter()
        .filter(|p| owned_by(p, job))
        .filter(|p| str_at(p, "/status/phase") == Some("Failed"))
        .count() as u64;
    counted.max(seen)
}

/// The sentence an unfinished scheduled Job's trouble ends with. Once the
/// Job has ended it no longer holds anything, so [`failed_job`] strips it
/// from the earlier description it quotes.
fn holds_the_schedule(noun: &str) -> String {
    format!(" Until it runs or its deadline stops it, no later scheduled {noun} starts.")
}

/// A failed Job's trouble.
///
/// A finished Job does not change, so once the condition describes it, the
/// description is kept (see the end of this function): the evidence read
/// the first time — the runner's record, the pod the Job still had, what the
/// previous condition said about it — may not all be there on the next read.
fn failed_job(
    run: Run,
    job: &Value,
    job_reason: &str,
    job_message: &str,
    at: Option<&str>,
    observed: &Observed,
    prior: Option<&PlatformStackCondition>,
) -> Outcome {
    let noun = run.noun();
    let job_name = name(job);
    let prefix = format!("{noun} Job {job_name}:");
    let when = at.unwrap_or("an unrecorded time");
    let failed_at = at.and_then(parse_time);

    let last_failed = observed
        .pods
        .iter()
        .filter(|p| owned_by(p, job))
        .filter(|p| str_at(p, "/status/phase") == Some("Failed"))
        .max_by_key(|p| time_at(p, "/metadata/creationTimestamp"));
    let cause = last_failed.map(attempt_cause);
    let recorded = run
        .record_keys()
        .and_then(|keys| runner_record(job, failed_at, observed.runner_status.as_ref(), keys));
    let recorded_text = recorded
        .as_deref()
        .map(|e| format!(" The runner recorded: {e}"))
        .unwrap_or_default();

    let (reason, message) = match job_reason {
        "DeadlineExceeded" => {
            let limit = job
                .pointer("/spec/activeDeadlineSeconds")
                .and_then(Value::as_i64)
                .map(|s| format!("its {} deadline", human_secs(s)))
                .unwrap_or_else(|| "its deadline".to_string());
            // What the previous condition saw about this Job's pod before the
            // deadline: the one record of a pod that was never placed, since
            // the Job controller deletes it at the deadline.
            let before = prior
                .filter(|p| {
                    matches!(
                        p.reason.as_deref(),
                        Some(REASON_UNSCHEDULABLE) | Some(REASON_NOT_STARTED)
                    )
                })
                .and_then(|p| p.message.as_deref())
                .and_then(|m| m.strip_prefix(prefix.as_str()))
                .map(|m| {
                    let held = holds_the_schedule(noun);
                    m.strip_suffix(held.as_str()).unwrap_or(m).trim()
                });
            let evidence = if !recorded_text.is_empty() {
                recorded_text.clone()
            } else if let Some(before) = before {
                format!(" Its runner never started: {before}")
            } else if let Some(c) = &cause {
                format!(" Its last attempt {}.", c.describe())
            } else if run == Run::Backup {
                " The runner recorded nothing for this run: its pod never started, or it was \
                 killed before it could."
                    .to_string()
            } else {
                " No pod of it is left to say how far it got.".to_string()
            };
            (
                REASON_DEADLINE_EXCEEDED,
                format!(
                    "{prefix} failed at {when}: DeadlineExceeded, active longer than \
                     {limit}.{evidence}"
                ),
            )
        }
        "BackoffLimitExceeded" => {
            let attempts = match failed_attempts(job, &observed.pods) {
                0 => String::new(),
                1 => " after 1 failed attempt".to_string(),
                n => format!(" after {n} failed attempts"),
            };
            // Every attempt ran and failed by itself. For a backup that is
            // the Job giving up; for the check it is `restic check` failing,
            // which is the repository integrity signal, so it gets its own
            // reason and a word on what that exit means.
            let (ran_and_failed, what_it_means) = match run {
                Run::Backup => (REASON_BACKOFF_LIMIT, ""),
                Run::Check => (
                    REASON_CHECK_FAILED,
                    " restic check exits non-zero when it finds the repository damaged, and \
                     when it cannot read the repository at all; its output is in that pod's \
                     log.",
                ),
            };
            let (reason, last) = match &cause {
                Some(Cause::OomKilled(w)) => (REASON_OOM_KILLED, format!(" The last one {w}.")),
                Some(Cause::Evicted(w)) => (REASON_EVICTED, format!(" The last one {w}.")),
                Some(c) => (
                    ran_and_failed,
                    format!(" The last one {}.{what_it_means}", c.describe()),
                ),
                None => (ran_and_failed, String::new()),
            };
            (
                reason,
                format!(
                    "{prefix} failed at {when}{attempts} (BackoffLimitExceeded).{last}\
                     {recorded_text}"
                ),
            )
        }
        other => {
            let said = reason_with_message(other, job_message)
                .unwrap_or_else(|| "no reason given".to_string());
            (
                REASON_FAILED,
                format!("{prefix} failed at {when}: {said}.{recorded_text}"),
            )
        }
    };

    // Keep the description of a finished Job once written. Only a
    // description OF THE ENDING counts (`<prefix> failed at …`): while the
    // Job was retrying, the condition described an attempt under the same
    // reason (an OOM kill is `RunnerOOMKilled` both before and after the
    // Job gives up), and keeping that would leave "the Job retries" on the
    // stack for a Job that has stopped. The kind proof caught exactly that.
    let ended = format!("{prefix} failed at ");
    if let Some(p) = prior {
        if p.status == "False"
            && p.reason.as_deref() == Some(reason)
            && p.message
                .as_deref()
                .is_some_and(|m| m.starts_with(ended.as_str()))
        {
            return Outcome::Trouble {
                reason,
                message: p.message.clone().unwrap_or(message),
            };
        }
    }
    Outcome::Trouble { reason, message }
}

/// The runner's own `lastError`, when its `lastFailure` falls inside this
/// Job's life. The record names no Job, so the time is the link: from the
/// Job's start to shortly after it failed (the runner writes during its
/// 90-second termination grace, before the Job is marked failed).
fn runner_record(
    job: &Value,
    failed_at: Option<DateTime<Utc>>,
    record: Option<&Value>,
    (time_key, error_key): (&str, &str),
) -> Option<String> {
    let record = record?;
    let started = time_at(job, "/status/startTime")
        .or_else(|| time_at(job, "/metadata/creationTimestamp"))?;
    let failed_at = failed_at?;
    let last_failure = record
        .get(time_key)
        .and_then(Value::as_str)
        .and_then(parse_time)?;
    let window_end = failed_at + Duration::seconds(180);
    if last_failure < started || last_failure > window_end {
        return None;
    }
    let error = record
        .get(error_key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|e| !e.is_empty())?;
    let mut quoted: String = error.chars().take(RUNNER_ERROR_QUOTE_MAX).collect();
    if quoted.len() < error.len() {
        quoted.push('…');
    }
    Some(quoted)
}

/// Why one attempt's pod ended.
enum Cause {
    /// Killed at its memory limit. Carries the sentence tail.
    OomKilled(String),
    /// Evicted by the kubelet. Carries the sentence tail.
    Evicted(String),
    /// Exited on its own with this code.
    Exited(String),
    /// Failed without a readable reason.
    Unknown(String),
}

impl Cause {
    fn describe(&self) -> &str {
        match self {
            Cause::OomKilled(s) | Cause::Evicted(s) | Cause::Exited(s) | Cause::Unknown(s) => s,
        }
    }
}

fn attempt_cause(pod: &Value) -> Cause {
    let pod_name = name(pod);
    let evicted = str_at(pod, "/status/reason") == Some("Evicted")
        || pod_condition(pod, "DisruptionTarget").is_some_and(|c| {
            c.get("status").and_then(Value::as_str) == Some("True")
                && c.get("reason").and_then(Value::as_str) == Some("TerminationByKubelet")
        });
    if evicted {
        let why = str_at(pod, "/status/message")
            .filter(|m| !m.is_empty())
            .or_else(|| {
                pod_condition(pod, "DisruptionTarget")
                    .and_then(|c| c.get("message").and_then(Value::as_str))
            })
            .unwrap_or("the kubelet gave no reason");
        return Cause::Evicted(format!(
            "was evicted (pod {pod_name}): {}",
            why.trim().trim_end_matches('.')
        ));
    }
    let terminated = pod
        .pointer("/status/containerStatuses")
        .and_then(Value::as_array)
        .and_then(|cs| cs.iter().find_map(|c| c.pointer("/state/terminated")));
    let Some(t) = terminated else {
        return Cause::Unknown(format!("failed (pod {pod_name})"));
    };
    let reason = t.get("reason").and_then(Value::as_str).unwrap_or("");
    let finished = t
        .get("finishedAt")
        .and_then(Value::as_str)
        .map(|f| format!(", at {f}"))
        .unwrap_or_default();
    if reason == "OOMKilled" {
        let limit = memory_limit(pod)
            .map(|l| format!(" at its {l} memory limit"))
            .unwrap_or_default();
        return Cause::OomKilled(format!("was OOMKilled{limit} (pod {pod_name}{finished})"));
    }
    match t.get("exitCode").and_then(Value::as_i64) {
        Some(code) => {
            let named = if reason.is_empty() || reason == "Error" {
                String::new()
            } else {
                format!(", {reason}")
            };
            Cause::Exited(format!(
                "exited with code {code}{named} (pod {pod_name}{finished})"
            ))
        }
        None => Cause::Unknown(format!("failed (pod {pod_name})")),
    }
}

fn owned_by(pod: &Value, job: &Value) -> bool {
    let uid = str_at(job, "/metadata/uid");
    let job_name = str_at(job, "/metadata/name");
    pod.pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .is_some_and(|refs| {
            refs.iter().any(|r| {
                r.get("kind").and_then(Value::as_str) == Some("Job")
                    && match (uid, job_name) {
                        (Some(u), _) => r.get("uid").and_then(Value::as_str) == Some(u),
                        (None, Some(n)) => r.get("name").and_then(Value::as_str) == Some(n),
                        (None, None) => false,
                    }
            })
        })
}

fn pod_condition<'a>(pod: &'a Value, type_: &str) -> Option<&'a Value> {
    pod.pointer("/status/conditions")
        .and_then(Value::as_array)?
        .iter()
        .find(|c| c.get("type").and_then(Value::as_str) == Some(type_))
}

/// The first waiting reason a placed-but-pending pod reports, init
/// containers first.
fn waiting_reason(pod: &Value) -> Option<String> {
    ["/status/initContainerStatuses", "/status/containerStatuses"]
        .iter()
        .filter_map(|p| pod.pointer(p).and_then(Value::as_array))
        .flatten()
        .find_map(|s| {
            let w = s.pointer("/state/waiting")?;
            reason_with_message(
                w.get("reason").and_then(Value::as_str).unwrap_or(""),
                w.get("message").and_then(Value::as_str).unwrap_or(""),
            )
        })
}

/// `256Mi of memory and 100m of CPU`, from the first container's requests.
fn requests(pod: &Value) -> Option<String> {
    let r = pod.pointer("/spec/containers/0/resources/requests")?;
    let mem = r.get("memory").and_then(Value::as_str);
    let cpu = r.get("cpu").and_then(Value::as_str);
    match (mem, cpu) {
        (Some(m), Some(c)) => Some(format!("{m} of memory and {c} of CPU")),
        (Some(m), None) => Some(format!("{m} of memory")),
        (None, Some(c)) => Some(format!("{c} of CPU")),
        (None, None) => None,
    }
}

fn memory_limit(pod: &Value) -> Option<&str> {
    str_at(pod, "/spec/containers/0/resources/limits/memory")
}

fn reason_with_message(reason: &str, message: &str) -> Option<String> {
    let message = message.trim().trim_end_matches('.');
    match (reason.is_empty(), message.is_empty()) {
        (true, true) => None,
        (false, true) => Some(reason.to_string()),
        (true, false) => Some(message.to_string()),
        (false, false) => Some(format!("{reason}: {message}")),
    }
}

/// `6h`, `90m`, `600s`.
fn human_secs(s: i64) -> String {
    if s > 0 && s % 3600 == 0 {
        format!("{}h", s / 3600)
    } else if s > 0 && s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

fn earliest(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

fn name(v: &Value) -> &str {
    str_at(v, "/metadata/name").unwrap_or("?")
}

fn str_at<'a>(v: &'a Value, pointer: &str) -> Option<&'a str> {
    v.pointer(pointer).and_then(Value::as_str)
}

fn time_at(v: &Value, pointer: &str) -> Option<DateTime<Utc>> {
    str_at(v, pointer).and_then(parse_time)
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    //! Every failure mode the runner cannot record, and recovery, over the
    //! objects as the apiserver returns them. The shapes are the ones a
    //! Kubernetes 1.36 cluster writes: a Job the CronJob started carries a
    //! controller reference to it, a pod carries one to its Job by uid, the
    //! scheduler's verdict is the pod's `PodScheduled` condition.
    use super::*;
    use serde_json::json;

    const NOW: &str = "2026-09-23T04:00:00Z";
    const INSUFFICIENT: &str = "0/1 nodes are available: 1 Insufficient memory. preemption: 0/1 \
                                nodes are available: 1 No preemption victims found for incoming \
                                pod.";

    fn now() -> DateTime<Utc> {
        parse_time(NOW).unwrap()
    }

    fn cronjob(name: &str) -> Value {
        json!({
            "apiVersion": "batch/v1", "kind": "CronJob",
            "metadata": { "name": name, "namespace": BACKUP_NAMESPACE, "uid": format!("{name}-uid") },
            "spec": { "schedule": "0 3 * * *", "concurrencyPolicy": "Forbid" },
            "status": {},
        })
    }

    /// A Job the CronJob `owner` started at `created`, with `conditions`.
    fn job(owner: &str, name: &str, created: &str, conditions: Vec<Value>) -> Value {
        json!({
            "apiVersion": "batch/v1", "kind": "Job",
            "metadata": {
                "name": name, "namespace": BACKUP_NAMESPACE, "uid": format!("{name}-uid"),
                "creationTimestamp": created,
                "ownerReferences": [{
                    "apiVersion": "batch/v1", "kind": "CronJob", "name": owner,
                    "uid": format!("{owner}-uid"), "controller": true,
                }],
            },
            "spec": { "activeDeadlineSeconds": 21600, "backoffLimit": 6 },
            "status": { "startTime": created, "conditions": conditions },
        })
    }

    fn backup_job(name: &str, created: &str, conditions: Vec<Value>) -> Value {
        job(BACKUP_CRONJOB, name, created, conditions)
    }

    fn cond(type_: &str, reason: &str, message: &str, at: &str) -> Value {
        json!({ "type": type_, "status": "True", "reason": reason, "message": message,
                "lastTransitionTime": at })
    }

    fn complete(at: &str) -> Vec<Value> {
        vec![
            cond("SuccessCriteriaMet", "CompletionsReached", "", at),
            cond("Complete", "", "", at),
        ]
    }

    fn failed(reason: &str, message: &str, at: &str) -> Vec<Value> {
        vec![
            cond("FailureTarget", reason, message, at),
            cond("Failed", reason, message, at),
        ]
    }

    /// A runner pod of `job`, created at `created`, not yet placed.
    fn pod(job: &Value, suffix: &str, created: &str) -> Value {
        let job_name = name(job);
        json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {
                "name": format!("{job_name}-{suffix}"), "namespace": BACKUP_NAMESPACE,
                "creationTimestamp": created,
                "labels": { "apprafter.io/backup-runner": "true" },
                "ownerReferences": [{
                    "apiVersion": "batch/v1", "kind": "Job", "name": job_name,
                    "uid": job["metadata"]["uid"], "controller": true,
                }],
            },
            "spec": { "containers": [{
                "name": "runner",
                "resources": { "requests": { "cpu": "100m", "memory": "256Mi" },
                               "limits": { "memory": "512Mi" } },
            }]},
            "status": { "phase": "Pending" },
        })
    }

    fn unschedulable(mut p: Value, since: &str) -> Value {
        p["status"]["conditions"] = json!([{
            "type": "PodScheduled", "status": "False", "reason": "Unschedulable",
            "message": INSUFFICIENT, "lastTransitionTime": since,
        }]);
        p
    }

    fn running(mut p: Value, at: &str) -> Value {
        p["spec"]["nodeName"] = json!("node-1");
        p["status"] = json!({
            "phase": "Running",
            "conditions": [{ "type": "PodScheduled", "status": "True", "lastTransitionTime": at }],
            "containerStatuses": [{ "name": "runner", "state": { "running": { "startedAt": at } } }],
        });
        p
    }

    fn oom_killed(mut p: Value, at: &str) -> Value {
        p["spec"]["nodeName"] = json!("node-1");
        p["status"] = json!({
            "phase": "Failed",
            "containerStatuses": [{ "name": "runner", "state": { "terminated": {
                "reason": "OOMKilled", "exitCode": 137, "finishedAt": at } } }],
        });
        p
    }

    fn exited(mut p: Value, code: i64, at: &str) -> Value {
        p["spec"]["nodeName"] = json!("node-1");
        p["status"] = json!({
            "phase": "Failed",
            "containerStatuses": [{ "name": "runner", "state": { "terminated": {
                "reason": "Error", "exitCode": code, "finishedAt": at } } }],
        });
        p
    }

    fn evicted(mut p: Value) -> Value {
        p["spec"]["nodeName"] = json!("node-1");
        p["status"] = json!({
            "phase": "Failed", "reason": "Evicted",
            "message": "Usage of EmptyDir volume \"staging\" exceeds the limit \"10Gi\". ",
            "containerStatuses": [{ "name": "runner", "state": { "terminated": {
                "reason": "ContainerStatusUnknown", "exitCode": 137 } } }],
        });
        p
    }

    fn observed(jobs: Vec<Value>, pods: Vec<Value>) -> Observed {
        Observed {
            cronjobs: vec![cronjob(BACKUP_CRONJOB)],
            jobs,
            pods,
            runner_status: None,
        }
    }

    fn run(o: &Observed, prior: &[PlatformStackCondition]) -> Assessment {
        assess(true, Ok(o), prior, now())
    }

    /// `(status, reason, message)` of a verdict that sets the condition.
    fn cond_of(a: &Assessment) -> (&'static str, &'static str, String) {
        match &a.verdict {
            Verdict::Condition {
                status,
                reason,
                message,
            } => (status, reason, message.clone()),
            Verdict::Absent => panic!("expected a condition, got Absent"),
        }
    }

    /// The condition `a` would put on the stack, as the prior of the next
    /// assessment.
    fn as_prior(a: &Assessment) -> Vec<PlatformStackCondition> {
        let mut status = PlatformStackStatus::default();
        apply(&mut status, COND_BACKUP_HEALTHY, &a.verdict, &[]);
        status.conditions.unwrap_or_default()
    }

    // -- the verdict's frame -------------------------------------------------

    #[test]
    fn a_cluster_without_backups_carries_no_condition() {
        let a = assess(false, Ok(&observed(vec![], vec![])), &[], now());
        assert_eq!(a.verdict, Verdict::Absent);
        assert_eq!(a.recheck_after, None);
    }

    #[test]
    fn disabling_backups_removes_a_condition_left_from_before() {
        let prior = as_prior(&run(&observed(vec![], vec![]), &[]));
        let mut status = PlatformStackStatus {
            conditions: Some(prior.clone()),
            ..Default::default()
        };
        apply(&mut status, COND_BACKUP_HEALTHY, &Verdict::Absent, &prior);
        assert!(status
            .conditions
            .unwrap()
            .iter()
            .all(|c| c.type_ != COND_BACKUP_HEALTHY));
    }

    #[test]
    fn an_unreadable_cluster_is_unknown_never_healthy() {
        let a = assess(true, Err("jobs.batch is forbidden"), &[], now());
        let (status, reason, message) = cond_of(&a);
        assert_eq!((status, reason), ("Unknown", REASON_UNREADABLE));
        assert!(message.contains("jobs.batch is forbidden"), "{message}");
    }

    #[test]
    fn enabled_but_not_deployed_says_the_schedule_is_missing() {
        let o = Observed::default();
        let (status, reason, message) = cond_of(&run(&o, &[]));
        assert_eq!((status, reason), ("Unknown", REASON_NOT_DEPLOYED));
        assert!(
            message.contains("apprafter-system/apprafter-backup"),
            "{message}"
        );
    }

    #[test]
    fn no_run_yet_is_unknown_and_names_the_schedule() {
        let (status, reason, message) = cond_of(&run(&observed(vec![], vec![]), &[]));
        assert_eq!((status, reason), ("Unknown", REASON_NO_RUN_YET));
        assert!(message.contains("0 3 * * *"), "{message}");
    }

    #[test]
    fn a_suspended_schedule_is_a_failure() {
        let mut o = observed(vec![], vec![]);
        o.cronjobs[0]["spec"]["suspend"] = json!(true);
        let (status, reason, _) = cond_of(&run(&o, &[]));
        assert_eq!((status, reason), ("False", REASON_SUSPENDED));
    }

    #[test]
    fn the_last_run_succeeding_is_true_and_names_the_job() {
        let j = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            complete("2026-09-23T03:01:02Z"),
        );
        let (status, reason, message) = cond_of(&run(&observed(vec![j], vec![]), &[]));
        assert_eq!((status, reason), ("True", REASON_SUCCEEDED));
        assert!(message.contains("apprafter-backup-29312340"), "{message}");
        assert!(message.contains("2026-09-23T03:01:02Z"), "{message}");
    }

    #[test]
    fn the_cronjobs_own_record_of_a_success_outlives_its_jobs() {
        let mut o = observed(vec![], vec![]);
        o.cronjobs[0]["status"] = json!({
            "lastScheduleTime": "2026-09-23T03:00:00Z",
            "lastSuccessfulTime": "2026-09-23T03:01:02Z",
        });
        let (status, _, message) = cond_of(&run(&o, &[]));
        assert_eq!(status, "True");
        assert!(message.contains("2026-09-23T03:01:02Z"), "{message}");
    }

    // -- a pod that is never placed -------------------------------------------

    #[test]
    fn an_unschedulable_pod_inside_the_grace_changes_nothing_yet_and_asks_to_be_rechecked() {
        // Five minutes unplaced: the scheduler may be waiting for a pod that
        // is still stopping.
        let j = backup_job("apprafter-backup-29312400", "2026-09-23T03:55:00Z", vec![]);
        let p = unschedulable(
            pod(&j, "x7k2q", "2026-09-23T03:55:00Z"),
            "2026-09-23T03:55:00Z",
        );
        let a = run(&observed(vec![j], vec![p]), &[]);
        let (status, reason, _) = cond_of(&a);
        assert_eq!((status, reason), ("Unknown", REASON_NO_RUN_YET));
        // 03:55 + 10 min = 04:05, five minutes from 04:00, plus the second
        // that lands the recheck after the edge.
        assert_eq!(a.recheck_after, Some(StdDuration::from_secs(301)));
    }

    #[test]
    fn inside_the_grace_a_healthy_cluster_stays_healthy() {
        let done = backup_job(
            "apprafter-backup-29310900",
            "2026-09-22T03:00:00Z",
            complete("2026-09-22T03:01:00Z"),
        );
        let j = backup_job("apprafter-backup-29312400", "2026-09-23T03:55:00Z", vec![]);
        let p = unschedulable(
            pod(&j, "x7k2q", "2026-09-23T03:55:00Z"),
            "2026-09-23T03:55:00Z",
        );
        let a = run(&observed(vec![done, j], vec![p]), &[]);
        assert_eq!(cond_of(&a).0, "True");
        assert!(a.recheck_after.is_some());
    }

    #[test]
    fn an_unschedulable_pod_past_the_grace_is_reported_with_the_schedulers_words() {
        let done = backup_job(
            "apprafter-backup-29310900",
            "2026-09-22T03:00:00Z",
            complete("2026-09-22T03:01:00Z"),
        );
        let j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        let p = unschedulable(
            pod(&j, "x7k2q", "2026-09-23T03:00:00Z"),
            "2026-09-23T03:00:04Z",
        );
        let a = run(&observed(vec![done, j], vec![p]), &[]);
        let (status, reason, message) = cond_of(&a);
        assert_eq!((status, reason), ("False", REASON_UNSCHEDULABLE));
        for needle in [
            "backup Job apprafter-backup-29312340:",
            "apprafter-backup-29312340-x7k2q",
            "since 2026-09-23T03:00:04Z",
            "1 Insufficient memory",
            "256Mi of memory and 100m of CPU",
            // A scheduled Job holds the schedule: the reader must learn that
            // nothing later runs either.
            "no later scheduled backup starts",
        ] {
            assert!(message.contains(needle), "missing {needle:?} in {message}");
        }
        assert_eq!(a.recheck_after, None);
    }

    #[test]
    fn exactly_at_the_end_of_the_grace_the_pod_counts_as_unschedulable() {
        let j = backup_job("apprafter-backup-29312340", "2026-09-23T03:50:00Z", vec![]);
        let p = unschedulable(
            pod(&j, "x7k2q", "2026-09-23T03:50:00Z"),
            "2026-09-23T03:50:00Z",
        );
        let (_, reason, _) = cond_of(&run(&observed(vec![j], vec![p]), &[]));
        assert_eq!(reason, REASON_UNSCHEDULABLE);
    }

    #[test]
    fn a_pod_being_made_room_for_by_preemption_is_not_trouble() {
        let j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        let mut p = unschedulable(
            pod(&j, "x7k2q", "2026-09-23T03:00:00Z"),
            "2026-09-23T03:00:04Z",
        );
        p["status"]["nominatedNodeName"] = json!("node-1");
        let (status, _, _) = cond_of(&run(&observed(vec![j], vec![p]), &[]));
        assert_ne!(status, "False");
    }

    #[test]
    fn a_manual_run_holds_no_schedule_and_does_not_say_it_does() {
        let mut j = backup_job(
            "apprafter-backup-manual-20260923-030000",
            "2026-09-23T03:00:00Z",
            vec![],
        );
        j["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("ownerReferences");
        j["metadata"]["labels"] = json!({ "apprafter.io/manual": "true" });
        let p = unschedulable(
            pod(&j, "x7k2q", "2026-09-23T03:00:00Z"),
            "2026-09-23T03:00:04Z",
        );
        let (_, reason, message) = cond_of(&run(&observed(vec![j], vec![p]), &[]));
        assert_eq!(reason, REASON_UNSCHEDULABLE);
        assert!(!message.contains("no later scheduled"), "{message}");
    }

    #[test]
    fn a_container_that_never_starts_is_reported_after_the_grace() {
        let j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        let mut p = pod(&j, "x7k2q", "2026-09-23T03:00:00Z");
        p["spec"]["nodeName"] = json!("node-1");
        p["status"] = json!({
            "phase": "Pending",
            "conditions": [{ "type": "PodScheduled", "status": "True",
                             "lastTransitionTime": "2026-09-23T03:00:01Z" }],
            "containerStatuses": [{ "name": "runner", "state": { "waiting": {
                "reason": "ImagePullBackOff", "message": "Back-off pulling image" } } }],
        });
        let (status, reason, message) = cond_of(&run(&observed(vec![j], vec![p]), &[]));
        assert_eq!((status, reason), ("False", REASON_NOT_STARTED));
        assert!(
            message.contains("ImagePullBackOff: Back-off pulling image"),
            "{message}"
        );
    }

    // -- a runner killed by what it cannot record ------------------------------

    #[test]
    fn an_oom_killed_attempt_is_a_failure_at_once_even_while_the_job_retries() {
        let j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        let mut j = j;
        j["status"]["failed"] = json!(1);
        let first = oom_killed(
            pod(&j, "aaaaa", "2026-09-23T03:00:00Z"),
            "2026-09-23T03:00:40Z",
        );
        let second = running(
            pod(&j, "bbbbb", "2026-09-23T03:00:55Z"),
            "2026-09-23T03:00:56Z",
        );
        let (status, reason, message) = cond_of(&run(&observed(vec![j], vec![first, second]), &[]));
        assert_eq!((status, reason), ("False", REASON_OOM_KILLED));
        assert!(message.contains("512Mi memory limit"), "{message}");
        assert!(
            message.contains("apprafter-backup-29312340-aaaaa"),
            "{message}"
        );
        assert!(message.contains("attempt 1 of at most 7"), "{message}");
    }

    #[test]
    fn an_evicted_attempt_carries_the_kubelets_reason() {
        let mut j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        j["status"]["failed"] = json!(1);
        let p = evicted(pod(&j, "aaaaa", "2026-09-23T03:00:00Z"));
        let (status, reason, message) = cond_of(&run(&observed(vec![j], vec![p]), &[]));
        assert_eq!((status, reason), ("False", REASON_EVICTED));
        assert!(
            message.contains("exceeds the limit \"10Gi\"; an evicted runner"),
            "{message}"
        );
    }

    #[test]
    fn an_ordinary_failed_attempt_waits_for_the_jobs_own_verdict() {
        // The runner records an ordinary error itself, and the next attempt
        // may succeed: no alarm until the Job gives up.
        let done = backup_job(
            "apprafter-backup-29310900",
            "2026-09-22T03:00:00Z",
            complete("2026-09-22T03:01:00Z"),
        );
        let mut j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        j["status"]["failed"] = json!(1);
        let p = exited(
            pod(&j, "aaaaa", "2026-09-23T03:00:00Z"),
            1,
            "2026-09-23T03:00:30Z",
        );
        let (status, _, _) = cond_of(&run(&observed(vec![done, j], vec![p]), &[]));
        assert_eq!(status, "True");
    }

    // -- a Job that has failed --------------------------------------------------

    #[test]
    fn a_job_that_gave_up_after_oom_kills_names_the_oom_kill() {
        let mut j = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            failed(
                "BackoffLimitExceeded",
                "Job has reached the specified backoff limit",
                "2026-09-23T03:12:00Z",
            ),
        );
        j["status"]["failed"] = json!(7);
        let p = oom_killed(
            pod(&j, "ggggg", "2026-09-23T03:11:00Z"),
            "2026-09-23T03:11:40Z",
        );
        let (status, reason, message) = cond_of(&run(&observed(vec![j], vec![p]), &[]));
        assert_eq!((status, reason), ("False", REASON_OOM_KILLED));
        assert!(message.contains("after 7 failed attempts"), "{message}");
        assert!(
            message.contains("was OOMKilled at its 512Mi memory limit"),
            "{message}"
        );
    }

    #[test]
    fn a_job_that_gives_up_replaces_the_retrying_description() {
        // Found by the kind proof: attempt 1 OOM-killed (condition False,
        // RunnerOOMKilled, "The Job retries …"), attempt 2 OOM-killed, the
        // Job fails with BackoffLimitExceeded — and the stack kept saying
        // the Job retries, because the finished-Job description rule kept
        // any prior message of the same reason for the same Job.
        let mut j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        j["spec"]["backoffLimit"] = json!(1);
        j["status"]["failed"] = json!(1);
        let first = oom_killed(
            pod(&j, "aaaaa", "2026-09-23T03:00:00Z"),
            "2026-09-23T03:00:40Z",
        );
        let retrying = run(&observed(vec![j.clone()], vec![first.clone()]), &[]);
        let (_, reason, message) = cond_of(&retrying);
        assert_eq!(reason, REASON_OOM_KILLED);
        assert!(message.contains("The Job retries"), "{message}");

        let second = oom_killed(
            pod(&j, "bbbbb", "2026-09-23T03:00:51Z"),
            "2026-09-23T03:00:52Z",
        );
        j["status"]["failed"] = json!(2);
        j["status"]["conditions"] = json!(failed(
            "BackoffLimitExceeded",
            "Job has reached the specified backoff limit",
            "2026-09-23T03:00:53Z"
        ));
        let o = observed(vec![j], vec![first, second]);
        let ended = assess(true, Ok(&o), &as_prior(&retrying), now());
        let (status, reason, message) = cond_of(&ended);
        assert_eq!((status, reason), ("False", REASON_OOM_KILLED));
        assert!(
            message.contains("failed at 2026-09-23T03:00:53Z after 2 failed attempts"),
            "{message}"
        );
        assert!(!message.contains("retries"), "{message}");
        // And that ending is what stays.
        let again = assess(true, Ok(&o), &as_prior(&ended), now());
        assert_eq!(ended.verdict, again.verdict);
    }

    #[test]
    fn the_attempt_count_does_not_wait_for_the_jobs_own_count() {
        // The Job controller updates `status.failed` a sync after a pod ends;
        // the failed pods themselves are already there to count.
        let mut j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        j["spec"]["backoffLimit"] = json!(1);
        let first = oom_killed(
            pod(&j, "aaaaa", "2026-09-23T03:00:00Z"),
            "2026-09-23T03:00:40Z",
        );
        let second = oom_killed(
            pod(&j, "bbbbb", "2026-09-23T03:00:51Z"),
            "2026-09-23T03:00:52Z",
        );
        let (_, _, message) = cond_of(&run(&observed(vec![j], vec![first, second]), &[]));
        assert!(message.contains("attempt 2 of at most 2"), "{message}");
        assert!(message.contains("bbbbb"), "the newest attempt: {message}");
        assert!(!message.contains("retries"), "{message}");
    }

    #[test]
    fn the_last_attempt_is_not_promised_a_retry() {
        let mut j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        j["spec"]["backoffLimit"] = json!(0);
        let p = evicted(pod(&j, "aaaaa", "2026-09-23T03:00:00Z"));
        let (_, reason, message) = cond_of(&run(&observed(vec![j], vec![p]), &[]));
        assert_eq!(reason, REASON_EVICTED);
        assert!(message.contains("attempt 1 of at most 1"), "{message}");
        assert!(!message.contains("retries"), "{message}");
    }

    #[test]
    fn a_job_that_gave_up_on_ordinary_errors_quotes_the_runner() {
        let mut j = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            failed(
                "BackoffLimitExceeded",
                "Job has reached the specified backoff limit",
                "2026-09-23T03:12:00Z",
            ),
        );
        j["status"]["failed"] = json!(7);
        let p = exited(
            pod(&j, "ggggg", "2026-09-23T03:11:00Z"),
            1,
            "2026-09-23T03:11:40Z",
        );
        let mut o = observed(vec![j], vec![p]);
        o.runner_status = Some(json!({
            "lastFailure": "2026-09-23T03:11:39+00:00",
            "lastError": "restic backup: Fatal: unable to open repository: 403 Forbidden",
        }));
        let (status, reason, message) = cond_of(&run(&o, &[]));
        assert_eq!((status, reason), ("False", REASON_BACKOFF_LIMIT));
        assert!(message.contains("exited with code 1"), "{message}");
        assert!(
            message.contains("The runner recorded: restic backup: Fatal"),
            "{message}"
        );
    }

    #[test]
    fn a_runner_record_from_another_run_is_not_quoted() {
        let j = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            failed(
                "DeadlineExceeded",
                "Job was active longer than specified deadline",
                "2026-09-23T09:00:02Z",
            ),
        );
        let mut o = observed(vec![j], vec![]);
        // Yesterday's failure, not this Job's.
        o.runner_status = Some(json!({
            "lastFailure": "2026-09-22T03:04:00+00:00", "lastError": "yesterday's error",
        }));
        let (_, _, message) = cond_of(&run(&o, &[]));
        assert!(!message.contains("yesterday's error"), "{message}");
        assert!(message.contains("recorded nothing"), "{message}");
    }

    #[test]
    fn a_deadline_that_stopped_a_pod_never_placed_says_it_never_started() {
        // The night as it happens: unschedulable past the grace, then the
        // deadline. The Job controller deletes the pod at the deadline, so
        // the one record of why is what the condition said before.
        let j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        let p = unschedulable(
            pod(&j, "x7k2q", "2026-09-23T03:00:00Z"),
            "2026-09-23T03:00:04Z",
        );
        let before = run(&observed(vec![j.clone()], vec![p]), &[]);
        assert_eq!(cond_of(&before).1, REASON_UNSCHEDULABLE);

        let mut ended = j;
        ended["status"]["conditions"] = json!(failed(
            "DeadlineExceeded",
            "Job was active longer than specified deadline",
            "2026-09-23T09:00:02Z"
        ));
        let after = assess(
            true,
            Ok(&observed(vec![ended], vec![])),
            &as_prior(&before),
            now(),
        );
        let (status, reason, message) = cond_of(&after);
        assert_eq!((status, reason), ("False", REASON_DEADLINE_EXCEEDED));
        assert!(
            message.contains("active longer than its 6h deadline"),
            "{message}"
        );
        assert!(message.contains("Its runner never started"), "{message}");
        assert!(message.contains("1 Insufficient memory"), "{message}");
        // An ended Job holds nothing; the quoted earlier description must not
        // still say it does.
        assert!(!message.contains("no later scheduled"), "{message}");
    }

    #[test]
    fn a_deadline_with_nothing_left_to_read_says_so() {
        let j = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            failed(
                "DeadlineExceeded",
                "Job was active longer than specified deadline",
                "2026-09-23T03:02:02Z",
            ),
        );
        let (status, reason, message) = cond_of(&run(&observed(vec![j], vec![]), &[]));
        assert_eq!((status, reason), ("False", REASON_DEADLINE_EXCEEDED));
        assert!(
            message.contains("recorded nothing for this run"),
            "{message}"
        );
    }

    #[test]
    fn a_deadline_that_stopped_a_running_runner_quotes_what_it_recorded() {
        let j = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            failed(
                "DeadlineExceeded",
                "Job was active longer than specified deadline",
                "2026-09-23T09:01:10Z",
            ),
        );
        let mut o = observed(vec![j], vec![]);
        o.runner_status = Some(json!({
            "lastFailure": "2026-09-23T09:00:20+00:00",
            "lastError": "run exceeded its deadline of 6h",
        }));
        let (_, reason, message) = cond_of(&run(&o, &[]));
        assert_eq!(reason, REASON_DEADLINE_EXCEEDED);
        assert!(
            message.contains("The runner recorded: run exceeded its deadline of 6h"),
            "{message}"
        );
    }

    #[test]
    fn a_failed_jobs_description_is_kept_once_written() {
        // Re-reading the same finished Job must give the same bytes, or every
        // reconcile writes status. Here the evidence the first description
        // used (the earlier condition) is gone the second time.
        let j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        let p = unschedulable(
            pod(&j, "x7k2q", "2026-09-23T03:00:00Z"),
            "2026-09-23T03:00:04Z",
        );
        let before = run(&observed(vec![j.clone()], vec![p]), &[]);
        let mut ended = j;
        ended["status"]["conditions"] =
            json!(failed("DeadlineExceeded", "", "2026-09-23T09:00:02Z"));
        let o = observed(vec![ended], vec![]);
        let first = assess(true, Ok(&o), &as_prior(&before), now());
        let second = assess(true, Ok(&o), &as_prior(&first), now());
        assert_eq!(first.verdict, second.verdict);
    }

    #[test]
    fn any_other_job_failure_is_reported_with_its_reason() {
        let j = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            failed(
                "PodFailurePolicy",
                "Container runner for pod … failed with exit code 3",
                "2026-09-23T03:01:00Z",
            ),
        );
        let (status, reason, message) = cond_of(&run(&observed(vec![j], vec![]), &[]));
        assert_eq!((status, reason), ("False", REASON_FAILED));
        assert!(message.contains("PodFailurePolicy"), "{message}");
    }

    #[test]
    fn failure_target_alone_already_counts_as_failed() {
        // The Job controller sets `FailureTarget` first and `Failed` only once
        // the pods are gone, up to the pods' 90 s grace later.
        let j = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            vec![cond(
                "FailureTarget",
                "DeadlineExceeded",
                "",
                "2026-09-23T09:00:00Z",
            )],
        );
        let (status, reason, _) = cond_of(&run(&observed(vec![j], vec![]), &[]));
        assert_eq!((status, reason), ("False", REASON_DEADLINE_EXCEEDED));
    }

    // -- recovery -----------------------------------------------------------------

    #[test]
    fn a_later_successful_run_clears_a_failure() {
        let bad = backup_job(
            "apprafter-backup-29310900",
            "2026-09-22T03:00:00Z",
            failed("DeadlineExceeded", "", "2026-09-22T09:00:00Z"),
        );
        let good = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            complete("2026-09-23T03:01:00Z"),
        );
        let o = observed(vec![bad.clone()], vec![]);
        let failing = run(&o, &[]);
        assert_eq!(cond_of(&failing).0, "False");
        let o = observed(vec![bad, good], vec![]);
        let (status, reason, message) = cond_of(&assess(true, Ok(&o), &as_prior(&failing), now()));
        assert_eq!((status, reason), ("True", REASON_SUCCEEDED));
        assert!(message.contains("apprafter-backup-29312340"), "{message}");
    }

    #[test]
    fn a_successful_manual_run_counts_as_the_latest_run() {
        let bad = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            failed("DeadlineExceeded", "", "2026-09-23T03:30:00Z"),
        );
        let mut manual = backup_job(
            "apprafter-backup-manual-20260923-033500",
            "2026-09-23T03:35:00Z",
            complete("2026-09-23T03:36:00Z"),
        );
        manual["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("ownerReferences");
        manual["metadata"]["labels"] = json!({ "apprafter.io/manual": "true" });
        let (status, _, _) = cond_of(&run(&observed(vec![bad, manual], vec![]), &[]));
        assert_eq!(status, "True");
    }

    #[test]
    fn a_stuck_older_run_outranks_a_newer_success() {
        // The stuck scheduled Job holds the schedule even though a manual run
        // got through after it.
        let j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        let p = unschedulable(
            pod(&j, "x7k2q", "2026-09-23T03:00:00Z"),
            "2026-09-23T03:00:04Z",
        );
        let mut manual = backup_job(
            "apprafter-backup-manual-20260923-033500",
            "2026-09-23T03:35:00Z",
            complete("2026-09-23T03:36:00Z"),
        );
        manual["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("ownerReferences");
        manual["metadata"]["labels"] = json!({ "apprafter.io/manual": "true" });
        let (_, reason, _) = cond_of(&run(&observed(vec![j, manual], vec![p]), &[]));
        assert_eq!(reason, REASON_UNSCHEDULABLE);
    }

    #[test]
    fn jobs_that_are_not_the_backups_are_ignored() {
        let mut other = backup_job(
            "some-other-job",
            "2026-09-23T03:30:00Z",
            failed("BackoffLimitExceeded", "", "2026-09-23T03:40:00Z"),
        );
        other["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("ownerReferences");
        let good = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            complete("2026-09-23T03:01:00Z"),
        );
        let (status, _, _) = cond_of(&run(&observed(vec![good, other], vec![]), &[]));
        assert_eq!(status, "True");
    }

    // -- the weekly check -------------------------------------------------------

    #[test]
    fn a_failed_check_is_a_failure_even_when_backups_succeed() {
        let good = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            complete("2026-09-23T03:01:00Z"),
        );
        let mut check = job(
            CHECK_CRONJOB,
            "apprafter-backup-check-29310000",
            "2026-09-21T06:00:00Z",
            failed("BackoffLimitExceeded", "", "2026-09-21T06:12:00Z"),
        );
        check["status"]["failed"] = json!(7);
        let p = exited(
            pod(&check, "ccccc", "2026-09-21T06:11:00Z"),
            1,
            "2026-09-21T06:11:10Z",
        );
        let mut o = observed(vec![good, check], vec![p]);
        o.cronjobs.push(cronjob(CHECK_CRONJOB));
        // The runner's record is about backups; it is never quoted for a check.
        o.runner_status = Some(
            json!({ "lastFailure": "2026-09-21T06:11:00+00:00", "lastError": "backup error" }),
        );
        let (status, reason, message) = cond_of(&run(&o, &[]));
        // Every attempt of `restic check` ran and failed: the repository
        // reason, not the generic give-up the same ending is for a backup.
        assert_eq!((status, reason), ("False", REASON_CHECK_FAILED));
        assert!(
            message.starts_with("repository check Job apprafter-backup-check-29310000:"),
            "{message}"
        );
        assert!(message.contains("exited with code 1"), "{message}");
        assert!(
            message.contains("finds the repository damaged"),
            "{message}"
        );
        assert!(!message.contains("backup error"), "{message}");
    }

    #[test]
    fn a_check_killed_at_its_memory_limit_is_the_kill_not_a_damaged_repository() {
        // What kills the pod decides the reason: a check that never got to
        // finish says nothing about the repository.
        let mut check = job(
            CHECK_CRONJOB,
            "apprafter-backup-check-29310000",
            "2026-09-21T06:00:00Z",
            failed("BackoffLimitExceeded", "", "2026-09-21T06:12:00Z"),
        );
        check["status"]["failed"] = json!(7);
        let p = oom_killed(
            pod(&check, "ccccc", "2026-09-21T06:11:00Z"),
            "2026-09-21T06:11:10Z",
        );
        let good = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            complete("2026-09-23T03:01:00Z"),
        );
        let mut o = observed(vec![good, check], vec![p]);
        o.cronjobs.push(cronjob(CHECK_CRONJOB));
        let (_, reason, message) = cond_of(&run(&o, &[]));
        assert_eq!(reason, REASON_OOM_KILLED);
        assert!(!message.contains("damaged"), "{message}");
    }

    #[test]
    fn a_backup_that_gave_up_on_ordinary_errors_is_not_called_a_damaged_repository() {
        let mut j = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            failed("BackoffLimitExceeded", "", "2026-09-23T03:12:00Z"),
        );
        j["status"]["failed"] = json!(7);
        let p = exited(
            pod(&j, "ggggg", "2026-09-23T03:11:00Z"),
            1,
            "2026-09-23T03:11:40Z",
        );
        let (_, reason, message) = cond_of(&run(&observed(vec![j], vec![p]), &[]));
        assert_eq!(reason, REASON_BACKOFF_LIMIT);
        assert!(!message.contains("damaged"), "{message}");
    }

    // -- a Job that never gets a pod ----------------------------------------------

    #[test]
    fn a_job_with_no_pod_past_the_grace_is_reported_as_not_started() {
        // A LimitRange, a quota or a webhook refusing the pod: the Job
        // controller creates nothing, and only its events say so.
        let done = backup_job(
            "apprafter-backup-29310900",
            "2026-09-22T03:00:00Z",
            complete("2026-09-22T03:01:00Z"),
        );
        let j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        let a = run(&observed(vec![done, j], vec![]), &[]);
        let (status, reason, message) = cond_of(&a);
        assert_eq!((status, reason), ("False", REASON_NOT_STARTED));
        for needle in [
            "backup Job apprafter-backup-29312340:",
            "no pod since it was created at 2026-09-23T03:00:00Z",
            "FailedCreate",
            "no later scheduled backup starts",
        ] {
            assert!(message.contains(needle), "missing {needle:?} in {message}");
        }
        assert_eq!(a.recheck_after, None);
    }

    #[test]
    fn a_job_with_no_pod_inside_the_grace_asks_to_be_rechecked() {
        // Created a minute ago: the Job controller is about to create the
        // pod, and nothing will wake the controller if it never does.
        let done = backup_job(
            "apprafter-backup-29310900",
            "2026-09-22T03:00:00Z",
            complete("2026-09-22T03:01:00Z"),
        );
        let j = backup_job("apprafter-backup-29312400", "2026-09-23T03:59:00Z", vec![]);
        let a = run(&observed(vec![done, j], vec![]), &[]);
        assert_eq!(cond_of(&a).0, "True");
        assert_eq!(a.recheck_after, Some(StdDuration::from_secs(9 * 60 + 1)));
    }

    #[test]
    fn a_pod_the_job_counts_but_the_label_does_not_select_is_not_no_pod() {
        // A Job made from an edited template, or one between a deleted pod
        // and its replacement: the Job controller counts a pod, so one
        // exists or is on its way.
        for count in ["active", "failed", "succeeded"] {
            let mut j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
            j["status"][count] = json!(1);
            let a = run(&observed(vec![j], vec![]), &[]);
            assert_ne!(cond_of(&a).0, "False", "status.{count}=1");
            assert_eq!(a.recheck_after, None, "status.{count}=1");
        }
    }

    #[test]
    fn a_suspended_job_says_it_is_suspended() {
        let mut j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        j["spec"]["suspend"] = json!(true);
        let (_, reason, message) = cond_of(&run(&observed(vec![j], vec![]), &[]));
        assert_eq!(reason, REASON_NOT_STARTED);
        assert!(message.contains("the Job is suspended"), "{message}");
        assert!(!message.contains("FailedCreate"), "{message}");
    }

    #[test]
    fn a_deadline_after_no_pod_quotes_what_was_seen_before() {
        let j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        let before = run(&observed(vec![j.clone()], vec![]), &[]);
        assert_eq!(cond_of(&before).1, REASON_NOT_STARTED);
        let mut ended = j;
        ended["status"]["conditions"] = json!(failed(
            "DeadlineExceeded",
            "Job was active longer than specified deadline",
            "2026-09-23T09:00:02Z"
        ));
        let o = observed(vec![ended], vec![]);
        let (_, reason, message) = cond_of(&assess(true, Ok(&o), &as_prior(&before), now()));
        assert_eq!(reason, REASON_DEADLINE_EXCEEDED);
        assert!(
            message.contains("Its runner never started: it has had no pod since"),
            "{message}"
        );
        assert!(!message.contains("no later scheduled"), "{message}");
    }

    #[test]
    fn a_backup_failure_outranks_a_check_failure() {
        let bad = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            failed("DeadlineExceeded", "", "2026-09-23T03:30:00Z"),
        );
        let check = job(
            CHECK_CRONJOB,
            "apprafter-backup-check-29310000",
            "2026-09-21T06:00:00Z",
            failed("BackoffLimitExceeded", "", "2026-09-21T06:12:00Z"),
        );
        let mut o = observed(vec![bad, check], vec![]);
        o.cronjobs.push(cronjob(CHECK_CRONJOB));
        let (_, _, message) = cond_of(&run(&o, &[]));
        assert!(
            message.starts_with("backup Job apprafter-backup-29312340:"),
            "{message}"
        );
    }

    #[test]
    fn healthy_backups_and_checks_both_appear_in_the_message() {
        let good = backup_job(
            "apprafter-backup-29312340",
            "2026-09-23T03:00:00Z",
            complete("2026-09-23T03:01:00Z"),
        );
        let check = job(
            CHECK_CRONJOB,
            "apprafter-backup-check-29310000",
            "2026-09-21T06:00:00Z",
            complete("2026-09-21T06:00:40Z"),
        );
        let mut o = observed(vec![good, check], vec![]);
        o.cronjobs.push(cronjob(CHECK_CRONJOB));
        let (status, _, message) = cond_of(&run(&o, &[]));
        assert_eq!(status, "True");
        assert!(
            message.contains("the last repository check, Job apprafter-backup-check-29310000"),
            "{message}"
        );
    }

    // -- the condition on the stack ---------------------------------------------

    #[test]
    fn the_failure_keeps_its_first_transition_time_while_its_cause_changes() {
        // Unschedulable, then stopped by the deadline: both False, and the
        // reader wants to know since when backups have failed, not since the
        // cause was last reworded.
        let prior = vec![PlatformStackCondition {
            type_: COND_BACKUP_HEALTHY.to_string(),
            status: "False".to_string(),
            reason: Some(REASON_UNSCHEDULABLE.to_string()),
            message: Some("backup Job x: …".to_string()),
            last_transition_time: "2026-09-23T03:10:05+00:00".to_string(),
        }];
        let mut status = PlatformStackStatus::default();
        apply(
            &mut status,
            COND_BACKUP_HEALTHY,
            &Verdict::Condition {
                status: "False",
                reason: REASON_DEADLINE_EXCEEDED,
                message: "backup Job x: failed".to_string(),
            },
            &prior,
        );
        let c = &status.conditions.unwrap()[0];
        assert_eq!(c.last_transition_time, "2026-09-23T03:10:05+00:00");
        assert_eq!(c.reason.as_deref(), Some(REASON_DEADLINE_EXCEEDED));
    }

    // -- the chart grants what this module reads --------------------------------

    /// Every request [`observe`] and the controller's watches make, as
    /// `(apiGroup, resource, verb)` in [`BACKUP_NAMESPACE`]. Grow it with the
    /// code: a verb the chart lacks shows only on a live cluster.
    const ACCESS: &[(&str, &str, &str)] = &[
        ("batch", "cronjobs", "list"),
        ("batch", "cronjobs", "watch"),
        ("batch", "jobs", "list"),
        ("batch", "jobs", "watch"),
        ("", "pods", "list"),
        ("", "pods", "watch"),
        ("", "configmaps", "get"),
    ];

    /// The operator chart's RBAC with its three template expressions filled
    /// in the way the platform installs it, and every other template line
    /// (the `if` guard, the label includes) dropped.
    fn operator_rbac() -> Vec<Value> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../charts/apprafter-operator/templates/rbac.yaml");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let filled: String = text
            .replace(
                "{{ include \"apprafter-operator.fullname\" . }}",
                "apprafter-operator",
            )
            .replace(
                "{{ include \"apprafter-operator.serviceAccountName\" . }}",
                "apprafter-operator",
            )
            .replace("{{ .Release.Namespace }}", "apprafter-system")
            .lines()
            .filter(|l| !l.contains("{{"))
            .collect::<Vec<_>>()
            .join("\n");
        let docs: Vec<Value> = filled
            .split("\n---")
            .filter_map(|d| serde_yaml::from_str::<Value>(d).ok())
            .filter(|d| d.get("kind").is_some())
            .collect();
        assert!(
            docs.len() >= 4,
            "parsed only {} documents from {}",
            docs.len(),
            path.display()
        );
        docs
    }

    /// Does `rules` grant `verb` on `group/resource`?
    fn grants(rules: &Value, (group, resource, verb): (&str, &str, &str)) -> bool {
        let has = |r: &Value, key: &str, want: &str| {
            r.get(key)
                .and_then(Value::as_array)
                .is_some_and(|a| a.iter().any(|v| v == want || v == "*"))
        };
        rules.as_array().is_some_and(|rs| {
            rs.iter().any(|r| {
                has(r, "apiGroups", group)
                    && has(r, "resources", resource)
                    && has(r, "verbs", verb)
                    && r.get("resourceNames").is_none()
            })
        })
    }

    #[test]
    fn the_operator_chart_grants_every_read_the_backup_verdict_makes() {
        let docs = operator_rbac();
        let bound_to_operator = |binding: &Value| {
            binding["subjects"].as_array().is_some_and(|s| {
                s.iter().any(|x| {
                    x["kind"] == "ServiceAccount"
                        && x["name"] == "apprafter-operator"
                        && x["namespace"] == "apprafter-system"
                })
            })
        };
        // Rule sets that apply in BACKUP_NAMESPACE: a bound ClusterRole, or a
        // Role there bound by a RoleBinding there.
        let mut rule_sets: Vec<&Value> = Vec::new();
        for b in docs.iter().filter(|d| bound_to_operator(d)) {
            let role_kind = b["roleRef"]["kind"].as_str().unwrap_or("");
            let role_name = &b["roleRef"]["name"];
            let binding_ns = b["metadata"]["namespace"].as_str();
            for r in docs
                .iter()
                .filter(|r| r["kind"] == role_kind && &r["metadata"]["name"] == role_name)
            {
                let applies = match role_kind {
                    "ClusterRole" => {
                        b["kind"] == "ClusterRoleBinding" || binding_ns == Some(BACKUP_NAMESPACE)
                    }
                    "Role" => {
                        r["metadata"]["namespace"] == BACKUP_NAMESPACE
                            && binding_ns == Some(BACKUP_NAMESPACE)
                    }
                    _ => false,
                };
                if applies {
                    rule_sets.push(&r["rules"]);
                }
            }
        }
        assert!(
            !rule_sets.is_empty(),
            "no role is bound to the operator's ServiceAccount"
        );
        for access in ACCESS {
            assert!(
                rule_sets.iter().any(|rules| grants(rules, *access)),
                "the operator chart does not grant {access:?} in {BACKUP_NAMESPACE}: the \
                 BackupHealthy read (or its watch) would 403 on a real cluster"
            );
        }
    }

    #[test]
    fn re_reading_unchanged_objects_gives_identical_verdicts() {
        // What keeps the status write skipped between real changes.
        let j = backup_job("apprafter-backup-29312340", "2026-09-23T03:00:00Z", vec![]);
        let p = unschedulable(
            pod(&j, "x7k2q", "2026-09-23T03:00:00Z"),
            "2026-09-23T03:00:04Z",
        );
        let o = observed(vec![j], vec![p]);
        let first = run(&o, &[]);
        let later = assess(
            true,
            Ok(&o),
            &as_prior(&first),
            now() + Duration::minutes(30),
        );
        assert_eq!(first.verdict, later.verdict);
    }
}
