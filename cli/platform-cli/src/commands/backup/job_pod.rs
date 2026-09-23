// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Where a backup Job's pod is: the part of the answer a Job's own status
//! cannot give.
//!
//! A Job counts a pod the scheduler could not place in `status.active`, just
//! as it counts one that is running a backup. So `backup status` printed
//! `Running` for a runner that had never started, and `backup run` printed
//! `… still running` until its `--timeout`, then exited 0. All that time the
//! pod sat `Pending` with `FailedScheduling: 0/1 nodes are available: 1
//! Insufficient memory`. That was a 4 GB node holding one application with
//! `needs.pg` and a persistent `needs.redis`. The pod's own conditions carry
//! the answer, and this module reads them.
//!
//! Pure: the callers fetch the Job and the pods, and everything here is a
//! function of that JSON (and, for [`UnschedulableClock`], of the instants
//! passed in).

use std::time::{Duration, Instant};

use serde_json::Value;

/// The documentation section both commands point at when the runner's pod
/// cannot be scheduled: the symptom, the check, and what to change.
///
/// A published URL rather than a page title, so the reader can follow it from
/// a terminal. A test resolves it against the committed page and its anchor,
/// so renaming either breaks the build, not the link.
pub(crate) const RUNNER_UNSCHEDULABLE_DOC: &str =
    "https://docs.apprafter.dev/operator-guide/backup-restore/#runner-unschedulable";

/// How long `backup run` lets its Job's pod stay unschedulable before it
/// stops waiting.
///
/// A pod the scheduler could not place is tried again whenever a pod leaves
/// the node, so a pod that only waits for room another pod is giving back is
/// placed within seconds of that pod being gone. What bounds the wait is how
/// long the departing pod takes to stop: 30 s by Kubernetes' default, 90 s for
/// a backup runner stopped at its deadline (the chart's
/// `terminationGracePeriodSeconds`), and about 60 s measured on a full 4 GB
/// node mid-upgrade, where the new operator and autoscaler pods waited for the
/// old ones. Two minutes covers all three. A pod still unschedulable after
/// that is waiting for room that nothing is about to give back.
///
/// A pod whose room is being made by preemption is not unschedulable in this
/// sense. It is [`JobPod::Preempting`], and the clock does not run for it.
pub(crate) const UNSCHEDULABLE_GRACE: Duration = Duration::from_secs(120);

/// A backup Job's pod as seen by the two commands that report on a Job that
/// has not finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JobPod {
    /// No unfinished pod to read: none created yet, all of them finished, or
    /// a shape this does not recognise. Callers fall back to what the Job
    /// itself says.
    None,
    /// The scheduler tried and found no node with room for it
    /// (`PodScheduled=False`, reason `Unschedulable`), and is not making room
    /// by preemption. `message` is the scheduler's own account, the text
    /// `FailedScheduling` carries (`0/1 nodes are available: 1 Insufficient
    /// memory. …`).
    Unschedulable { uid: String, message: String },
    /// Not placed yet, but the scheduler has nominated `node` and is waiting
    /// for the lower-priority pods it evicted there to stop. It will be placed
    /// when they are gone.
    Preempting { node: String },
    /// No container is running yet: not placed yet, held by a scheduling
    /// gate, or placed with its containers still waiting. `reason` is the
    /// reason given, with its message when there is one:
    /// `ImagePullBackOff: Back-off pulling image …`,
    /// `CreateContainerConfigError: secret "…" not found`.
    NotStarted { reason: Option<String> },
    /// The pod's containers have started.
    Running,
}

/// The state of `job`'s pod, read from `pods` (any listing that contains
/// them; pods owned by other Jobs are ignored).
///
/// A pod belongs to the Job when an `ownerReferences` entry of kind `Job`
/// carries the Job's `uid`. It matches the Job's name only when the Job has
/// no uid. Matching by name alone would also match the pods of an earlier Job
/// that had the same name. Finished pods (`Succeeded`/`Failed`) and pods
/// being deleted do not describe what the Job is doing now. A Job that
/// retries has one of each, and a failed attempt must not hide the live one.
/// Of the rest, the newest is read.
pub(crate) fn job_pod(job: &Value, pods: &[Value]) -> JobPod {
    let live = pods
        .iter()
        .filter(|p| owned_by(p, job))
        .filter(|p| {
            !matches!(
                p.pointer("/status/phase").and_then(Value::as_str),
                Some("Succeeded" | "Failed")
            )
        })
        .filter(|p| p.pointer("/metadata/deletionTimestamp").is_none())
        .max_by_key(|p| {
            p.pointer("/metadata/creationTimestamp")
                .and_then(Value::as_str)
                .unwrap_or("")
        });
    live.map_or(JobPod::None, classify)
}

fn owned_by(pod: &Value, job: &Value) -> bool {
    let uid = job.pointer("/metadata/uid").and_then(Value::as_str);
    let name = job.pointer("/metadata/name").and_then(Value::as_str);
    let Some(refs) = pod
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
    else {
        return false;
    };
    refs.iter().any(|r| {
        if r.get("kind").and_then(Value::as_str) != Some("Job") {
            return false;
        }
        match (uid, name) {
            (Some(u), _) => r.get("uid").and_then(Value::as_str) == Some(u),
            (None, Some(n)) => r.get("name").and_then(Value::as_str) == Some(n),
            (None, None) => false,
        }
    })
}

fn classify(pod: &Value) -> JobPod {
    let placed = pod
        .pointer("/spec/nodeName")
        .and_then(Value::as_str)
        .is_some_and(|n| !n.is_empty());
    if !placed {
        let scheduled = pod
            .pointer("/status/conditions")
            .and_then(Value::as_array)
            .and_then(|cs| {
                cs.iter()
                    .find(|c| c.get("type").and_then(Value::as_str) == Some("PodScheduled"))
            });
        let Some(cond) =
            scheduled.filter(|c| c.get("status").and_then(Value::as_str) == Some("False"))
        else {
            // Not tried yet: the scheduler has not written its verdict.
            return JobPod::NotStarted { reason: None };
        };
        let reason = cond.get("reason").and_then(Value::as_str).unwrap_or("");
        let message = cond.get("message").and_then(Value::as_str).unwrap_or("");
        if reason != "Unschedulable" {
            // `SchedulingGated`, or a reason a later Kubernetes adds: say it,
            // but only `Unschedulable` means no node has room.
            return JobPod::NotStarted {
                reason: reason_with_message(reason, message),
            };
        }
        if let Some(node) = pod
            .pointer("/status/nominatedNodeName")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
        {
            return JobPod::Preempting {
                node: node.to_string(),
            };
        }
        return JobPod::Unschedulable {
            uid: pod
                .pointer("/metadata/uid")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            message: if message.is_empty() {
                "the scheduler gave no reason".to_string()
            } else {
                message.to_string()
            },
        };
    }
    match pod.pointer("/status/phase").and_then(Value::as_str) {
        Some("Running") => JobPod::Running,
        Some("Pending") => JobPod::NotStarted {
            reason: waiting_reason(pod),
        },
        _ => JobPod::None,
    }
}

/// The first waiting reason a placed-but-pending pod reports, init
/// containers first, because they run first.
fn waiting_reason(pod: &Value) -> Option<String> {
    ["/status/initContainerStatuses", "/status/containerStatuses"]
        .iter()
        .filter_map(|p| pod.pointer(p).and_then(Value::as_array))
        .flatten()
        .find_map(|s| {
            let w = s.pointer("/state/waiting")?;
            let reason = w.get("reason").and_then(Value::as_str).unwrap_or("");
            let message = w.get("message").and_then(Value::as_str).unwrap_or("");
            reason_with_message(reason, message)
        })
}

fn reason_with_message(reason: &str, message: &str) -> Option<String> {
    match (reason.is_empty(), message.is_empty()) {
        (true, true) => None,
        (false, true) => Some(reason.to_string()),
        (true, false) => Some(message.to_string()),
        (false, false) => Some(format!("{reason}: {message}")),
    }
}

/// How long one pod has been unschedulable, as this command has seen it.
///
/// Measured on this machine's monotonic clock from the first observation
/// that found it so, not from the condition's `lastTransitionTime`. That field
/// is on the apiserver's clock, and any skew between the two would shorten or
/// stretch the grace. Nothing is lost by starting late: `backup run` created
/// the Job itself, so it is watching from the pod's first minute.
///
/// The streak ends on any observation that finds the pod in another state,
/// and when the Job's current pod is a different one. A retry's new pod
/// starts its own streak.
#[derive(Debug, Default)]
pub(crate) struct UnschedulableClock {
    streak: Option<(String, Instant)>,
}

impl UnschedulableClock {
    /// Record one observation. Returns how long the same pod has been
    /// unschedulable, `Some(ZERO)` on the observation that starts a streak,
    /// and `None` when it is not unschedulable now.
    pub(crate) fn observe(&mut self, state: &JobPod, now: Instant) -> Option<Duration> {
        let JobPod::Unschedulable { uid, .. } = state else {
            self.streak = None;
            return None;
        };
        match &self.streak {
            Some((seen, since)) if seen == uid => Some(now.saturating_duration_since(*since)),
            _ => {
                self.streak = Some((uid.clone(), now));
                Some(Duration::ZERO)
            }
        }
    }
}

fn elapsed(d: Duration) -> String {
    super::format_elapsed(d.as_secs())
}

/// The outcome `backup status` prints for a Job that has not finished, or
/// `None` to leave it to the Job's own pod counts.
pub(crate) fn status_outcome(state: &JobPod) -> Option<String> {
    match state {
        JobPod::None => None,
        JobPod::Unschedulable { message, .. } => {
            Some(format!("Pending, cannot be scheduled: {message}"))
        }
        JobPod::Preempting { node } => Some(format!(
            "Pending, waiting for room on {node} while the pods evicted there stop"
        )),
        JobPod::NotStarted { reason: Some(r) } => Some(format!("Pending: {r}")),
        JobPod::NotStarted { reason: None } => Some("Pending".to_string()),
        JobPod::Running => Some("Running".to_string()),
    }
}

/// The lines `backup status` prints under a Job line whose pod cannot be
/// scheduled, or `None` for any other state.
///
/// A Job the CronJob started holds the schedule while it waits. The CronJob
/// is `concurrencyPolicy: Forbid`, so no later scheduled backup starts until
/// this one runs or its deadline stops it. A chart that sets no deadline
/// holds the schedule until the Job is deleted. A manual Job has no owner and
/// holds nothing. Only the first kind gets that line.
pub(crate) fn status_hint(job: &Value, state: &JobPod) -> Option<String> {
    if !matches!(state, JobPod::Unschedulable { .. }) {
        return None;
    }
    let scheduled = job
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .is_some_and(|refs| {
            refs.iter()
                .any(|r| r.get("kind").and_then(Value::as_str) == Some("CronJob"))
        });
    let mut out = match runner_requests(job) {
        Some(asks) => {
            format!("    No node has room for the backup runner's pod, which asks for {asks}.\n")
        }
        None => "    No node has room for the backup runner's pod.\n".to_string(),
    };
    out.push_str("    `apprafter top` shows how much of each node is requested, and by what.\n");
    if scheduled {
        out.push_str(
            "    Until this Job runs or its deadline stops it, the schedule starts no other \
             backup.\n",
        );
    }
    out.push_str(&format!(
        "    What to check and change: {RUNNER_UNSCHEDULABLE_DOC}\n"
    ));
    Some(out)
}

/// A progress line for `backup run` while it waits: what the pod is doing,
/// not only that time has passed. `waited` is the whole wait so far;
/// `unschedulable_for` is the current streak from [`UnschedulableClock`].
pub(crate) fn progress_note(
    state: &JobPod,
    waited: Duration,
    unschedulable_for: Option<Duration>,
) -> String {
    match state {
        JobPod::Unschedulable { message, .. } => format!(
            "  … its pod cannot be scheduled ({} so far, giving up at {}): {message}",
            elapsed(unschedulable_for.unwrap_or(Duration::ZERO)),
            elapsed(UNSCHEDULABLE_GRACE),
        ),
        JobPod::Preempting { node } => format!(
            "  … waiting for room on {node} while the pods evicted there stop ({})",
            elapsed(waited)
        ),
        JobPod::NotStarted { reason: Some(r) } => {
            format!("  … not started yet: {r} ({})", elapsed(waited))
        }
        JobPod::NotStarted { reason: None } => {
            format!("  … not started yet ({})", elapsed(waited))
        }
        JobPod::Running | JobPod::None => format!("  … still running ({})", elapsed(waited)),
    }
}

/// What `backup run` says when its `--timeout` ends the wait. The Job is left
/// in place either way. The words depend on the pod: a runner that never
/// started is not "still running".
pub(crate) fn timeout_note(
    state: &JobPod,
    timeout_minutes: u64,
    namespace: &str,
    name: &str,
) -> String {
    let what = match state {
        JobPod::Running | JobPod::None => format!("still running after {timeout_minutes}m"),
        JobPod::Unschedulable { message, .. } => {
            format!("its pod could not be scheduled in {timeout_minutes}m ({message})")
        }
        JobPod::Preempting { node } => {
            format!("its pod was still waiting for room on {node} after {timeout_minutes}m")
        }
        JobPod::NotStarted { reason: Some(r) } => {
            format!("its pod had not started after {timeout_minutes}m ({r})")
        }
        JobPod::NotStarted { reason: None } => {
            format!("its pod had not started after {timeout_minutes}m")
        }
    };
    format!(
        "  {what}. No longer waiting. The Job is NOT cancelled:\n    \
         kubectl -n {namespace} logs -f job/{name}\n    \
         apprafter backup status"
    )
}

/// What the runner's pod asks the scheduler for, read from the Job's pod
/// template: `256Mi of memory and 100m of CPU`. `None` when the template
/// requests neither.
///
/// Read from the Job, not written down here, because the chart owns the
/// number and a copy would go stale the day it changes.
pub(crate) fn runner_requests(job: &Value) -> Option<String> {
    let containers = job
        .pointer("/spec/template/spec/containers")
        .and_then(Value::as_array)?;
    let pick = |key: &str| -> Vec<String> {
        containers
            .iter()
            .filter_map(|c| {
                c.pointer(&format!("/resources/requests/{key}"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    };
    let memory = pick("memory");
    let cpu = pick("cpu");
    let part =
        |v: &[String], what: &str| (!v.is_empty()).then(|| format!("{} of {what}", v.join(" + ")));
    match (part(&memory, "memory"), part(&cpu, "CPU")) {
        (Some(m), Some(c)) => Some(format!("{m} and {c}")),
        (Some(one), None) | (None, Some(one)) => Some(one),
        (None, None) => None,
    }
}

/// What `backup run` prints when it gives up on a Job whose pod stayed
/// unschedulable past [`UNSCHEDULABLE_GRACE`]. It goes to stdout, one fact per
/// line, just before the typed error: inside the error, miette would wrap the
/// scheduler's message and split the URL across lines.
///
/// `requests` is [`runner_requests`] of the Job. `deleted` is the outcome of
/// deleting the Job; `Err` carries why it could not be deleted.
pub(crate) fn unschedulable_report(
    namespace: &str,
    name: &str,
    message: &str,
    requests: Option<&str>,
    deleted: Result<(), String>,
) -> String {
    let asks = match requests {
        Some(r) => format!(
            "    The runner asks for {r}. The scheduled backup asks for the same, so it cannot \
             start either.\n"
        ),
        None => {
            "    The scheduled backup asks for the same, so it cannot start either.\n".to_string()
        }
    };
    let job_line = match deleted {
        Ok(()) => "    The Job is deleted, so it will not start later on its own, at a time \
                   nobody chose and possibly beside the scheduled backup.\n"
            .to_string(),
        Err(e) => format!(
            "    Deleting the Job failed ({e}). Delete it yourself, or it starts on its own \
             whenever room appears:\n      kubectl -n {namespace} delete job {name}\n"
        ),
    };
    format!(
        "  ✗ The backup never started: no node has room for its pod.\n    \
         The scheduler says: {message}\n\
         {asks}    \
         `apprafter top` shows how much of each node is requested, and by what.\n    \
         What to check and change: {RUNNER_UNSCHEDULABLE_DOC}\n\
         {job_line}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const JOB_UID: &str = "523d891f-0a86-4ad1-aec3-a173141943a4";
    const POD_UID: &str = "f5393d51-1768-4430-8228-58c2e8949909";
    /// The scheduler's message, verbatim from the pod of the live run that
    /// found this (cpx22, 1958Mi allocatable, 1792Mi requested).
    const INSUFFICIENT: &str = "0/1 nodes are available: 1 Insufficient memory. no new claims \
                                to deallocate, preemption: 0/1 nodes are available: 1 No \
                                preemption victims found for incoming pod.";

    fn job() -> Value {
        json!({
            "metadata": {"name": "apprafter-backup-manual-20260923-143946", "uid": JOB_UID},
            "status": {"active": 1, "startTime": "2026-09-23T14:39:47Z"}
        })
    }

    fn owner(uid: &str) -> Value {
        json!([{"apiVersion": "batch/v1", "kind": "Job", "controller": true,
                "name": "apprafter-backup-manual-20260923-143946", "uid": uid}])
    }

    /// The pod `kubectl get pod -o yaml` showed on that run, trimmed to what
    /// is read.
    fn unschedulable_pod() -> Value {
        json!({
            "metadata": {
                "name": "apprafter-backup-manual-20260923-143946-4987h",
                "uid": POD_UID,
                "creationTimestamp": "2026-09-23T14:39:47Z",
                "ownerReferences": owner(JOB_UID)
            },
            "spec": {"containers": [{"name": "runner"}]},
            "status": {
                "phase": "Pending",
                "qosClass": "Burstable",
                "conditions": [{
                    "type": "PodScheduled", "status": "False", "reason": "Unschedulable",
                    "lastProbeTime": null, "lastTransitionTime": "2026-09-23T14:39:47Z",
                    "message": INSUFFICIENT
                }]
            }
        })
    }

    fn running_pod() -> Value {
        json!({
            "metadata": {"name": "p-run", "uid": "run-uid",
                         "creationTimestamp": "2026-09-23T14:39:47Z",
                         "ownerReferences": owner(JOB_UID)},
            "spec": {"nodeName": "node-1"},
            "status": {
                "phase": "Running",
                "conditions": [{"type": "PodScheduled", "status": "True"}],
                "containerStatuses": [{"name": "runner", "state": {"running": {}}}]
            }
        })
    }

    #[test]
    fn the_live_runs_pod_reads_as_unschedulable_with_the_schedulers_message() {
        assert_eq!(
            job_pod(&job(), &[unschedulable_pod()]),
            JobPod::Unschedulable {
                uid: POD_UID.to_string(),
                message: INSUFFICIENT.to_string()
            }
        );
    }

    #[test]
    fn a_placed_running_pod_is_running() {
        assert_eq!(job_pod(&job(), &[running_pod()]), JobPod::Running);
    }

    #[test]
    fn preemption_in_progress_is_not_unschedulable() {
        // The scheduler found room by evicting lower-priority pods and is
        // waiting for them to stop. The pod WILL be placed; giving up on
        // it would be wrong. It carries the same condition as a pod no node
        // can take. `nominatedNodeName` is the difference.
        let mut pod = unschedulable_pod();
        pod["status"]["nominatedNodeName"] = json!("node-1");
        assert_eq!(
            job_pod(&job(), &[pod]),
            JobPod::Preempting {
                node: "node-1".to_string()
            }
        );
    }

    #[test]
    fn a_pod_the_scheduler_has_not_judged_yet_is_only_not_started() {
        let mut pod = unschedulable_pod();
        pod["status"] = json!({"phase": "Pending"});
        assert_eq!(job_pod(&job(), &[pod]), JobPod::NotStarted { reason: None });
    }

    #[test]
    fn a_scheduling_gate_is_said_but_is_not_unschedulable() {
        let mut pod = unschedulable_pod();
        pod["status"]["conditions"] = json!([{
            "type": "PodScheduled", "status": "False", "reason": "SchedulingGated",
            "message": "Scheduling is blocked due to non-empty scheduling gates"
        }]);
        assert_eq!(
            job_pod(&job(), &[pod]),
            JobPod::NotStarted {
                reason: Some(
                    "SchedulingGated: Scheduling is blocked due to non-empty scheduling gates"
                        .to_string()
                )
            }
        );
    }

    #[test]
    fn a_placed_pod_waiting_on_its_container_says_why() {
        // Placed, but its container cannot start: the credential Secret is
        // missing, or the image cannot be pulled. These waits never end on
        // their own either, and "still running" is as untrue for them.
        let mut pod = running_pod();
        pod["status"] = json!({
            "phase": "Pending",
            "conditions": [{"type": "PodScheduled", "status": "True"}],
            "containerStatuses": [{"name": "runner", "state": {"waiting": {
                "reason": "CreateContainerConfigError",
                "message": "secret \"apprafter-backup-s3\" not found"
            }}}]
        });
        assert_eq!(
            job_pod(&job(), &[pod.clone()]),
            JobPod::NotStarted {
                reason: Some(
                    "CreateContainerConfigError: secret \"apprafter-backup-s3\" not found"
                        .to_string()
                )
            }
        );
        // An init container runs first, so its wait is the one reported.
        pod["status"]["initContainerStatuses"] = json!([{"name": "init", "state": {"waiting": {
            "reason": "ImagePullBackOff"
        }}}]);
        assert_eq!(
            job_pod(&job(), &[pod]),
            JobPod::NotStarted {
                reason: Some("ImagePullBackOff".to_string())
            }
        );
    }

    #[test]
    fn only_the_jobs_own_pods_count() {
        // Another Job's unschedulable pod, a pod owned by something else, and
        // a pod with no owner, in the same namespace listing.
        let mut other_job = unschedulable_pod();
        other_job["metadata"]["ownerReferences"] = owner("someone-else");
        let mut replica_set = unschedulable_pod();
        replica_set["metadata"]["ownerReferences"] =
            json!([{"kind": "ReplicaSet", "name": "x", "uid": JOB_UID}]);
        let mut orphan = unschedulable_pod();
        orphan["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("ownerReferences");
        assert_eq!(
            job_pod(&job(), &[other_job, replica_set, orphan]),
            JobPod::None
        );
    }

    #[test]
    fn a_job_without_a_uid_is_matched_by_name() {
        let mut j = job();
        j["metadata"].as_object_mut().unwrap().remove("uid");
        assert!(matches!(
            job_pod(&j, &[unschedulable_pod()]),
            JobPod::Unschedulable { .. }
        ));
        // And with neither, nothing matches rather than everything.
        let nameless = json!({"metadata": {}});
        assert_eq!(job_pod(&nameless, &[unschedulable_pod()]), JobPod::None);
    }

    #[test]
    fn a_finished_or_deleted_attempt_does_not_hide_the_live_one() {
        // A Job that retries after a failure has a Failed pod and a new one.
        // Only the new one says what the Job is doing now.
        let mut failed = running_pod();
        failed["metadata"]["creationTimestamp"] = json!("2026-09-23T15:00:00Z");
        failed["status"]["phase"] = json!("Failed");
        let mut deleting = running_pod();
        deleting["metadata"]["creationTimestamp"] = json!("2026-09-23T15:01:00Z");
        deleting["metadata"]["deletionTimestamp"] = json!("2026-09-23T15:02:00Z");
        assert!(matches!(
            job_pod(
                &job(),
                &[failed.clone(), unschedulable_pod(), deleting.clone()]
            ),
            JobPod::Unschedulable { .. }
        ));
        // With no live pod left, there is nothing to say.
        assert_eq!(job_pod(&job(), &[failed, deleting]), JobPod::None);
        assert_eq!(job_pod(&job(), &[]), JobPod::None);
    }

    #[test]
    fn of_two_live_pods_the_newest_is_read() {
        let mut older = running_pod();
        older["metadata"]["creationTimestamp"] = json!("2026-09-23T14:00:00Z");
        let mut newer = unschedulable_pod();
        newer["metadata"]["creationTimestamp"] = json!("2026-09-23T14:30:00Z");
        assert!(matches!(
            job_pod(&job(), &[newer.clone(), older.clone()]),
            JobPod::Unschedulable { .. }
        ));
        newer["metadata"]["creationTimestamp"] = json!("2026-09-23T13:00:00Z");
        assert_eq!(job_pod(&job(), &[newer, older]), JobPod::Running);
    }

    #[test]
    fn a_missing_scheduler_message_still_reads_as_unschedulable() {
        let mut pod = unschedulable_pod();
        pod["status"]["conditions"][0]
            .as_object_mut()
            .unwrap()
            .remove("message");
        assert_eq!(
            job_pod(&job(), &[pod]),
            JobPod::Unschedulable {
                uid: POD_UID.to_string(),
                message: "the scheduler gave no reason".to_string()
            }
        );
    }

    fn unsched(uid: &str) -> JobPod {
        JobPod::Unschedulable {
            uid: uid.to_string(),
            message: INSUFFICIENT.to_string(),
        }
    }

    #[test]
    fn the_clock_measures_one_unbroken_streak_of_one_pod() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut c = UnschedulableClock::default();
        assert_eq!(c.observe(&unsched("a"), t0), Some(Duration::ZERO));
        assert_eq!(c.observe(&unsched("a"), t0 + s(60)), Some(s(60)));
        assert_eq!(c.observe(&unsched("a"), t0 + s(125)), Some(s(125)));
        // One observation in another state ends the streak.
        assert_eq!(c.observe(&JobPod::Running, t0 + s(130)), None);
        assert_eq!(c.observe(&unsched("a"), t0 + s(135)), Some(Duration::ZERO));
        assert_eq!(c.observe(&unsched("a"), t0 + s(140)), Some(s(5)));
        // A different pod (a retry) starts its own.
        assert_eq!(c.observe(&unsched("b"), t0 + s(150)), Some(Duration::ZERO));
        assert_eq!(c.observe(&unsched("b"), t0 + s(160)), Some(s(10)));
        // Preemption is not a streak.
        let preempting = JobPod::Preempting {
            node: "n".to_string(),
        };
        assert_eq!(c.observe(&preempting, t0 + s(170)), None);
        assert_eq!(c.observe(&unsched("b"), t0 + s(175)), Some(Duration::ZERO));
    }

    #[test]
    fn the_grace_outlasts_what_a_departing_pod_can_take() {
        // The runner's own terminationGracePeriodSeconds (90) and the ~60 s
        // measured mid-upgrade both fit inside it, with scheduling latency
        // to spare. A grace below the runner's own 90 s would give up on
        // a backup that was only waiting for the previous one to stop.
        assert!(UNSCHEDULABLE_GRACE > Duration::from_secs(90));
        assert!(UNSCHEDULABLE_GRACE <= Duration::from_secs(300));
    }

    #[test]
    fn status_says_pending_with_the_reason_instead_of_running() {
        let s = status_outcome(&unsched("a")).unwrap();
        assert!(s.starts_with("Pending, cannot be scheduled: "), "{s}");
        assert!(s.contains("1 Insufficient memory"), "{s}");
        assert!(!s.contains("Running"), "{s}");
        assert_eq!(
            status_outcome(&JobPod::NotStarted {
                reason: Some("ImagePullBackOff".to_string())
            })
            .unwrap(),
            "Pending: ImagePullBackOff"
        );
        assert_eq!(status_outcome(&JobPod::Running).unwrap(), "Running");
        assert_eq!(status_outcome(&JobPod::None), None);
        assert!(status_outcome(&JobPod::Preempting {
            node: "n1".to_string()
        })
        .unwrap()
        .contains("n1"));
    }

    #[test]
    fn the_status_hint_names_top_the_docs_and_whether_the_schedule_is_held() {
        let mut manual = job();
        manual["spec"] = json!({"template": {"spec": {"containers": [{
            "resources": {"requests": {"cpu": "100m", "memory": "256Mi"}}
        }]}}});
        let h = status_hint(&manual, &unsched("a")).unwrap();
        assert!(
            h.contains("which asks for 256Mi of memory and 100m of CPU"),
            "{h}"
        );
        assert!(h.contains("`apprafter top`"), "{h}");
        assert!(h.contains(RUNNER_UNSCHEDULABLE_DOC), "{h}");
        assert!(
            !h.contains("schedule starts no other"),
            "a manual Job holds nothing: {h}"
        );

        let mut scheduled = job();
        scheduled["metadata"]["ownerReferences"] =
            json!([{"kind": "CronJob", "name": "apprafter-backup", "uid": "cj"}]);
        let h = status_hint(&scheduled, &unsched("a")).unwrap();
        assert!(h.contains("schedule starts no other backup"), "{h}");

        assert_eq!(status_hint(&scheduled, &JobPod::Running), None);
        assert_eq!(
            status_hint(&scheduled, &JobPod::NotStarted { reason: None }),
            None
        );
    }

    #[test]
    fn the_progress_line_says_what_the_pod_is_doing() {
        let s = Duration::from_secs;
        let n = progress_note(&unsched("a"), s(95), Some(s(90)));
        assert!(n.contains("cannot be scheduled"), "{n}");
        assert!(n.contains("1m 30s so far"), "{n}");
        assert!(n.contains("giving up at 2m 0s"), "{n}");
        assert!(n.contains("Insufficient memory"), "{n}");
        assert!(!n.contains("still running"), "{n}");
        assert_eq!(
            progress_note(&JobPod::Running, s(95), None),
            "  … still running (1m 35s)"
        );
        assert_eq!(
            progress_note(
                &JobPod::NotStarted {
                    reason: Some("ContainerCreating".to_string())
                },
                s(40),
                None
            ),
            "  … not started yet: ContainerCreating (40s)"
        );
    }

    #[test]
    fn a_timeout_on_a_pod_that_never_started_does_not_call_it_running() {
        let t = timeout_note(&unsched("a"), 1, "apprafter-system", "j");
        assert!(t.contains("could not be scheduled in 1m"), "{t}");
        assert!(t.contains("Insufficient memory"), "{t}");
        assert!(!t.contains("still running"), "{t}");
        assert!(t.contains("NOT cancelled"), "{t}");
        let t = timeout_note(&JobPod::Running, 60, "apprafter-system", "j");
        assert!(t.contains("still running after 60m"), "{t}");
        assert!(
            t.contains("kubectl -n apprafter-system logs -f job/j"),
            "{t}"
        );
    }

    #[test]
    fn the_give_up_report_carries_the_reason_the_requests_top_the_docs_and_the_job() {
        let r = unschedulable_report(
            "apprafter-system",
            "apprafter-backup-manual-x",
            INSUFFICIENT,
            Some("256Mi of memory and 100m of CPU"),
            Ok(()),
        );
        assert!(r.contains("never started"), "{r}");
        assert!(
            r.contains(&format!("The scheduler says: {INSUFFICIENT}\n")),
            "{r}"
        );
        assert!(
            r.contains("The runner asks for 256Mi of memory and 100m of CPU."),
            "{r}"
        );
        assert!(r.contains("scheduled backup asks for the same"), "{r}");
        assert!(r.contains("`apprafter top`"), "{r}");
        // The URL on a line of its own, whole: nothing wraps stdout.
        assert!(
            r.lines()
                .any(|l| l.trim_end().ends_with(RUNNER_UNSCHEDULABLE_DOC)),
            "{r}"
        );
        assert!(r.contains("The Job is deleted"), "{r}");

        let r = unschedulable_report(
            "apprafter-system",
            "apprafter-backup-manual-x",
            INSUFFICIENT,
            None,
            Err("forbidden".to_string()),
        );
        assert!(!r.contains("The runner asks for"), "{r}");
        assert!(r.contains("Deleting the Job failed (forbidden)"), "{r}");
        assert!(
            r.contains("kubectl -n apprafter-system delete job apprafter-backup-manual-x"),
            "{r}"
        );
        assert!(!r.contains("The Job is deleted"), "{r}");
    }

    #[test]
    fn the_runners_requests_are_read_from_the_jobs_pod_template() {
        let job = json!({"spec": {"template": {"spec": {"containers": [{
            "name": "runner",
            "resources": {"requests": {"cpu": "100m", "memory": "256Mi"},
                          "limits": {"memory": "512Mi"}}
        }]}}}});
        assert_eq!(
            runner_requests(&job).as_deref(),
            Some("256Mi of memory and 100m of CPU")
        );
        let memory_only = json!({"spec": {"template": {"spec": {"containers": [{
            "resources": {"requests": {"memory": "256Mi"}}
        }]}}}});
        assert_eq!(
            runner_requests(&memory_only).as_deref(),
            Some("256Mi of memory")
        );
        let none = json!({"spec": {"template": {"spec": {"containers": [{"name": "runner"}]}}}});
        assert_eq!(runner_requests(&none), None);
        assert_eq!(runner_requests(&json!({})), None);
    }

    #[test]
    fn the_documentation_link_resolves_to_a_committed_section() {
        // The CLI prints this URL to someone whose backup cannot run. If the
        // page moves or the anchor is renamed, the link breaks and nothing
        // else notices. The docs build checks links between pages, not a
        // URL inside a Rust string.
        let path = RUNNER_UNSCHEDULABLE_DOC
            .strip_prefix("https://docs.apprafter.dev/")
            .expect("a docs.apprafter.dev URL");
        let (page, anchor) = path.split_once("/#").expect("<page>/#<anchor>");
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs")
            .join(format!("{page}.md"));
        let text = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("{} must exist: {e}", file.display()));
        assert!(
            text.lines()
                .any(|l| l.starts_with('#') && l.trim_end().ends_with(&format!("{{#{anchor}}}"))),
            "{} has no heading with {{#{anchor}}}",
            file.display()
        );
    }
}
