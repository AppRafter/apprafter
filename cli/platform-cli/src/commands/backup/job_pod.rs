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

/// How long `backup run` lets its Job's pod stay unschedulable, for lack of
/// room ([`Unplaced::NoRoom`]) or for a rule of the nodes
/// ([`Unplaced::Other`]), before it stops waiting.
///
/// A pod the scheduler could not place is tried again whenever a pod leaves
/// the node, so a pod that only waits for room another pod is giving back is
/// placed within seconds of that pod being gone. While any pod in the cluster
/// is stopping, the room it gives back may be what the runner waits for, and
/// `backup run` does not count that time at all ([`stopping_pods`]): a CNPG
/// instance being deleted can take three minutes to shut down. What is left
/// is scheduling latency and the moment between a pod's deletion and its
/// first sighting, so two minutes of no room with nothing stopping is a node
/// that nothing is about to make room on.
///
/// A pod whose room is being made by preemption is not unschedulable in this
/// sense. It is [`JobPod::Preempting`], and the clock does not run for it.
pub(crate) const UNSCHEDULABLE_GRACE: Duration = Duration::from_secs(120);

/// How long `backup run` lets its Job's pod be kept off by a condition of
/// the node ([`Unplaced::NodeCondition`]) before it stops waiting.
///
/// Such a condition lifts by itself, but not at once. The kubelet keeps a
/// memory- or disk-pressure taint for five minutes after the pressure ends
/// (`evictionPressureTransitionPeriod`), and a runner evicted by that pressure
/// is retried straight into it. A node restarting is not ready for a few
/// minutes. Ten minutes covers both. A condition still there after that is
/// not about to lift.
pub(crate) const NODE_CONDITION_GRACE: Duration = Duration::from_secs(600);

/// Why the scheduler placed a pod on no node, read from the message it
/// writes on the pod (`0/1 nodes are available: 1 Insufficient memory. …`).
///
/// Only a lack of room is answered by freeing memory or a bigger machine, so
/// only [`Unplaced::NoRoom`] gets that advice. The message itself is printed
/// in every case: it is the scheduler's own account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unplaced {
    /// The nodes the pod may use lack room for it: the scheduler counts at
    /// least one node short of a resource (`Insufficient memory`, `Too many
    /// pods`), and no node held off by a condition that lifts by itself.
    /// A node the pod can never use (a taint it does not tolerate) does not
    /// change what is short on the one it can.
    NoRoom,
    /// A node is held off by a condition of its own that lifts when the
    /// condition ends: a `node.kubernetes.io/…` taint (memory, disk or PID
    /// pressure, not ready, unreachable) or a cordon (`a cordon`). Each one
    /// is named once.
    NodeCondition(Vec<String>),
    /// Neither: every reason is another rule of the nodes (a taint the runner
    /// does not tolerate, an affinity), or the message could not be read.
    Other,
}

/// The kind of [`Unplaced`] a message says. See there.
pub(crate) fn unplaced(message: &str) -> Unplaced {
    let Some((_, rest)) = message.split_once("nodes are available: ") else {
        return Unplaced::Other;
    };
    // The per-node reasons end at the first sentence break; what follows is
    // the scheduler's account of preemption.
    let reasons = rest.split(". ").next().unwrap_or("").trim_end_matches('.');
    let mut room = false;
    let mut lifting: Vec<String> = Vec::new();
    // Each reason is `<node count> <reason>`. Before Kubernetes 1.25 a taint
    // read `node(s) had taint {…}, that the pod didn't tolerate`, whose tail
    // becomes a piece of its own here and matches nothing.
    for piece in reasons.split(", ") {
        let reason = piece.trim().split_once(' ').map_or("", |(_, r)| r);
        let condition = if reason.starts_with("Insufficient ") || reason == "Too many pods" {
            room = true;
            None
        } else if reason == "node(s) were unschedulable" {
            Some("a cordon".to_string())
        } else {
            taint_key(reason).filter(|k| k.starts_with("node.kubernetes.io/"))
        };
        if let Some(c) = condition {
            if !lifting.contains(&c) {
                lifting.push(c);
            }
        }
    }
    if !lifting.is_empty() {
        Unplaced::NodeCondition(lifting)
    } else if room {
        Unplaced::NoRoom
    } else {
        Unplaced::Other
    }
}

/// The key of the taint a reason names: `node(s) had untolerated taint
/// {node.kubernetes.io/memory-pressure: }` gives
/// `node.kubernetes.io/memory-pressure`.
fn taint_key(reason: &str) -> Option<String> {
    let (_, after) = reason.split_once("taint {")?;
    let key = after.split([':', '}']).next()?.trim();
    (!key.is_empty()).then(|| key.to_string())
}

/// How long `backup run` waits for a pod kept off for `cause`.
pub(crate) fn grace_for(cause: &Unplaced) -> Duration {
    match cause {
        Unplaced::NodeCondition(_) => NODE_CONDITION_GRACE,
        Unplaced::NoRoom | Unplaced::Other => UNSCHEDULABLE_GRACE,
    }
}

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
    /// No live pod because an attempt failed and the Job controller has not
    /// started the next yet: it waits 10 s, doubling up to 6 min, between
    /// attempts. `failed` attempts have failed of `attempts` at most
    /// (`backoffLimit` + 1). The Job's counts alone read `Failed` here
    /// (`active: 0`, `failed: 1`), but the Job has not failed, and a Job the
    /// CronJob started still holds the schedule.
    Retrying { failed: u64, attempts: u64 },
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
///
/// With no live pod, a Job that has failed attempts and attempts left is
/// [`JobPod::Retrying`], read from the Job alone.
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
    match live {
        Some(pod) => classify(pod),
        None => retrying(job).unwrap_or(JobPod::None),
    }
}

/// Kubernetes' default `backoffLimit`, for a Job read without one (the
/// apiserver fills it in on every Job it stores).
const DEFAULT_BACKOFF_LIMIT: u64 = 6;

/// [`JobPod::Retrying`] when the Job, with no live pod, is between a failed
/// attempt and the next one: some attempts failed, none is active or
/// succeeded, attempts are left, it is not suspended, and the Job controller
/// has not begun to fail or complete it. `FailureTarget` is the condition it
/// sets first when the Job fails (a deadline, the last attempt), before
/// `Failed` once the pods are gone; `SuccessCriteriaMet` is the same for
/// success.
fn retrying(job: &Value) -> Option<JobPod> {
    let count = |key: &str| {
        job.pointer(&format!("/status/{key}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let failed = count("failed");
    if failed == 0 || count("active") > 0 || count("succeeded") > 0 {
        return None;
    }
    if job.pointer("/spec/suspend").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let ending = job
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .is_some_and(|cs| {
            cs.iter().any(|c| {
                c.get("status").and_then(Value::as_str) == Some("True")
                    && matches!(
                        c.get("type").and_then(Value::as_str),
                        Some("FailureTarget" | "SuccessCriteriaMet" | "Failed" | "Complete")
                    )
            })
        });
    if ending {
        return None;
    }
    let attempts = job
        .pointer("/spec/backoffLimit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_BACKOFF_LIMIT)
        + 1;
    (failed < attempts).then_some(JobPod::Retrying { failed, attempts })
}

/// `1 failed attempt`, `2 failed attempts`.
fn failed_attempts(n: u64) -> String {
    if n == 1 {
        "1 failed attempt".to_string()
    } else {
        format!("{n} failed attempts")
    }
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
/// when the Job's current pod is a different one, and when the kind of
/// reason changes ([`Unplaced`]): a pod kept off by a pressure taint for five
/// minutes that then finds no room has begun a new wait, not spent one. A
/// retry's new pod starts its own streak.
#[derive(Debug, Default)]
pub(crate) struct UnschedulableClock {
    streak: Option<(String, std::mem::Discriminant<Unplaced>, Instant)>,
}

impl UnschedulableClock {
    /// Record one observation. Returns how long the same pod has been
    /// unschedulable for the same kind of reason, `Some(ZERO)` on the
    /// observation that starts a streak, and `None` when it is not
    /// unschedulable now.
    pub(crate) fn observe(&mut self, state: &JobPod, now: Instant) -> Option<Duration> {
        let JobPod::Unschedulable { uid, message } = state else {
            self.streak = None;
            return None;
        };
        let kind = std::mem::discriminant(&unplaced(message));
        match &self.streak {
            Some((seen, k, since)) if seen == uid && *k == kind => {
                Some(now.saturating_duration_since(*since))
            }
            _ => {
                self.streak = Some((uid.clone(), kind, now));
                Some(Duration::ZERO)
            }
        }
    }

    /// End the streak without an observation: the time that follows does
    /// not count toward it.
    pub(crate) fn reset(&mut self) {
        self.streak = None;
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
        JobPod::Retrying { failed, attempts } => Some(format!(
            "Retrying after {} ({attempts} attempts at most)",
            failed_attempts(*failed)
        )),
    }
}

/// The lines `backup status` prints under a Job line whose pod cannot be
/// scheduled, or `None` for any other state.
///
/// A Job the CronJob started holds the schedule while it waits. The CronJob
/// is `concurrencyPolicy: Forbid`, so no later scheduled run starts until
/// this one runs or its deadline stops it. A Job with no deadline — every Job
/// of a chart before 0.2.80 — holds the schedule until it is deleted, and
/// that line says so and gives the command ([`deadline_of`]); it used to
/// promise a deadline the Job did not have. A manual Job has no owner and
/// holds nothing. Only the first kind gets that line.
///
/// What it says depends on the scheduler's reason ([`Unplaced`]): only a lack
/// of room gets the runner's requests and `apprafter top`.
pub(crate) fn status_hint(job: &Value, state: &JobPod) -> Option<String> {
    let JobPod::Unschedulable { message, .. } = state else {
        return None;
    };
    let scheduled_by = job
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .and_then(|refs| {
            refs.iter()
                .find(|r| r.get("kind").and_then(Value::as_str) == Some("CronJob"))
        })
        .map(|r| r.get("name").and_then(Value::as_str).unwrap_or(""));
    let mut out = match unplaced(message) {
        Unplaced::NoRoom => {
            let mut room = match runner_requests(job) {
                Some(asks) => format!(
                    "    No node has room for the backup runner's pod, which asks for {asks}.\n"
                ),
                None => "    No node has room for the backup runner's pod.\n".to_string(),
            };
            room.push_str(
                "    `apprafter top` shows how much of each node is requested, and by what.\n",
            );
            room
        }
        Unplaced::NodeCondition(what) => format!(
            "    Its pod is kept off by {}, a condition of the node that lifts when it ends. A \
             memory- or disk-pressure taint stays five minutes after the pressure does.\n",
            what.join(" and ")
        ),
        Unplaced::Other => "    The scheduler's reasons are not a lack of room: no node accepts \
                            the pod until they change.\n"
            .to_string(),
    };
    if let Some(cronjob) = scheduled_by {
        let what = if cronjob == super::CHECK_CRONJOB_NAME {
            "check"
        } else {
            "backup"
        };
        match deadline_of(job) {
            Some(_) => out.push_str(&format!(
                "    Until this Job runs or its deadline stops it, the schedule starts no other \
                 {what}.\n"
            )),
            None => out.push_str(&format!(
                "    This Job has no deadline (activeDeadlineSeconds), so nothing stops it: until \
                 it runs or is deleted, the schedule starts no other {what}. To clear it:\n      \
                 {}\n",
                delete_command(
                    job.pointer("/metadata/namespace")
                        .and_then(Value::as_str)
                        .unwrap_or(super::PLATFORMSTACK_NAMESPACE),
                    job.pointer("/metadata/name")
                        .and_then(Value::as_str)
                        .unwrap_or("<job>")
                )
            )),
        }
    }
    out.push_str(&format!(
        "    What to check and change: {RUNNER_UNSCHEDULABLE_DOC}\n"
    ));
    Some(out)
}

/// The command that deletes a Job, as every hint here spells it — so a
/// report can tell whether a hint it prints already gave it.
pub(crate) fn delete_command(namespace: &str, name: &str) -> String {
    format!("kubectl -n {namespace} delete job {name}")
}

/// A Job's `spec.activeDeadlineSeconds`, or `None` when it has none — as no
/// Job of a chart before 0.2.80 has. Pure.
pub(crate) fn deadline_of(job: &Value) -> Option<u64> {
    job.pointer("/spec/activeDeadlineSeconds")
        .and_then(Value::as_u64)
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
            elapsed(grace_for(&unplaced(message))),
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
        JobPod::Retrying { failed, attempts } => format!(
            "  … retrying after {}, {attempts} attempts at most ({})",
            failed_attempts(*failed),
            elapsed(waited)
        ),
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
        JobPod::Retrying { failed, .. } => format!(
            "it was retrying after {} when {timeout_minutes}m ran out",
            failed_attempts(*failed)
        ),
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

/// What `backup run` knows about its Job when it stops waiting for the pod.
#[derive(Debug)]
pub(crate) struct GiveUp<'a> {
    pub(crate) namespace: &'a str,
    pub(crate) name: &'a str,
    /// The scheduler's message, as the pod carries it.
    pub(crate) message: &'a str,
    /// What that message says ([`unplaced`]).
    pub(crate) cause: &'a Unplaced,
    /// [`runner_requests`] of the Job.
    pub(crate) requests: Option<&'a str>,
    /// Other backup or check Jobs whose runner is running: each holds room
    /// of the same size, and this one could start once they have finished.
    pub(crate) holders: &'a [String],
    /// Attempts of this Job that failed before this one (`status.failed`).
    pub(crate) failed: u64,
    /// Why the newest of those failed ([`last_failed_attempt`]).
    pub(crate) last_failure: Option<&'a str>,
}

impl GiveUp<'_> {
    /// `The backup never started` for a first attempt; `Attempt 2 of the
    /// backup could not be scheduled` once an attempt has run and failed,
    /// which did start.
    fn headline(&self) -> String {
        let why = match self.cause {
            Unplaced::NoRoom => "no node has room for its pod".to_string(),
            Unplaced::NodeCondition(what) => {
                format!("its pod was kept off by {}", what.join(" and "))
            }
            Unplaced::Other => "no node accepts its pod".to_string(),
        };
        if self.failed == 0 {
            format!("The backup never started: {why}.")
        } else {
            format!(
                "Attempt {} of the backup could not be scheduled: {why}.",
                self.failed + 1
            )
        }
    }
}

/// What `backup run` prints when it gives up on a Job whose pod stayed
/// unschedulable past [`grace_for`] its reason. It goes to stdout, one fact
/// per line, just before the typed error: inside the error, miette would
/// wrap the scheduler's message and split the URL across lines.
///
/// `deleted` is the outcome of deleting the Job; `Err` carries why it could
/// not be deleted.
pub(crate) fn unschedulable_report(give_up: &GiveUp<'_>, deleted: Result<(), String>) -> String {
    let GiveUp {
        namespace,
        name,
        message,
        cause,
        requests,
        holders,
        failed,
        last_failure,
    } = give_up;
    let mut out = format!(
        "  ✗ {}\n    The scheduler says: {message}\n",
        give_up.headline()
    );
    match cause {
        Unplaced::NoRoom => {
            // Another runner that is running holds the room, and may be the
            // scheduled backup itself: then "the scheduled backup cannot
            // start either" would be false, and it is the one to name.
            let who = if holders.is_empty() {
                "The scheduled backup asks for the same, so it cannot start either.".to_string()
            } else {
                let (verb, own) = if holders.len() == 1 {
                    ("is running and holds", "it has")
                } else {
                    ("are running and hold", "they have")
                };
                format!(
                    "{} {verb} room of the same size; this backup can start once {own} \
                     finished.",
                    and_list(holders)
                )
            };
            match requests {
                Some(r) => out.push_str(&format!("    The runner asks for {r}. {who}\n")),
                None => out.push_str(&format!("    {who}\n")),
            }
            out.push_str(
                "    `apprafter top` shows how much of each node is requested, and by what.\n",
            );
        }
        Unplaced::NodeCondition(_) => out.push_str(&format!(
            "    That lifts when the node's condition ends, but it was still there after {}. \
             The scheduled backup is kept off the same way while it lasts.\n",
            elapsed(NODE_CONDITION_GRACE)
        )),
        Unplaced::Other => out.push_str(
            "    That is not a lack of room: no node accepts the pod until it changes, and the \
             scheduled backup meets the same rules.\n",
        ),
    }
    if *failed > 0 {
        let (before, recorded) = if *failed == 1 {
            ("1 attempt".to_string(), "that attempt recorded one")
        } else {
            (format!("{failed} attempts"), "one of them recorded it")
        };
        let why = match (last_failure, *failed) {
            (Some(w), 1) => format!(" ({w})"),
            (Some(w), _) => format!(" (the last: {w})"),
            (None, _) => String::new(),
        };
        out.push_str(&format!(
            "    Before it, {before} failed{why}. `apprafter backup status` shows the runner's \
             lastError if {recorded}.\n"
        ));
    }
    out.push_str(&format!(
        "    What to check and change: {RUNNER_UNSCHEDULABLE_DOC}\n"
    ));
    out.push_str(&match deleted {
        Ok(()) => "    The Job is deleted, so it will not start later on its own, at a time \
                   nobody chose and possibly beside the scheduled backup.\n"
            .to_string(),
        Err(e) => format!(
            "    Deleting the Job failed ({e}). Delete it yourself, or it starts on its own \
             whenever a node takes it:\n      kubectl -n {namespace} delete job {name}\n"
        ),
    });
    out
}

/// The text and the help of the typed error `backup run` fails with after
/// [`unschedulable_report`], for a wait of `waited`: `(what, help)`. The
/// error reads `backup Job <name> <what>`.
///
/// Short on purpose: the report above it carries the scheduler's message and
/// the link, which miette would wrap.
pub(crate) fn give_up_error(give_up: &GiveUp<'_>, waited: Duration) -> (String, String) {
    let why = match give_up.cause {
        Unplaced::NoRoom => "no node had room for its pod".to_string(),
        Unplaced::NodeCondition(what) => format!("its pod was kept off by {}", what.join(" and ")),
        Unplaced::Other => "no node accepted its pod".to_string(),
    };
    let what = if give_up.failed == 0 {
        format!("never started: {why} for {}", elapsed(waited))
    } else {
        format!(
            "could not schedule attempt {} for {}, after {}: {why}",
            give_up.failed + 1,
            elapsed(waited),
            failed_attempts(give_up.failed)
        )
    };
    let help = match give_up.cause {
        Unplaced::NoRoom if !give_up.holders.is_empty() => {
            let (verb, own) = if give_up.holders.len() == 1 {
                ("is running and asks", "it has")
            } else {
                ("are running and ask", "they have")
            };
            format!(
                "The lines above give the scheduler's reason. {} {verb} for the same room as \
                 this backup. Run `apprafter backup run` again once `apprafter backup status` \
                 shows {own} finished.",
                and_list(give_up.holders)
            )
        }
        Unplaced::NoRoom => "The lines above give the scheduler's reason and what the runner asks \
                             for. `apprafter top` shows how much of each node is requested, and \
                             by what: free enough for the runner, or move to a bigger machine, \
                             then run `apprafter backup run` again. Until then the scheduled \
                             backup cannot start either."
            .to_string(),
        Unplaced::NodeCondition(what) => format!(
            "The lines above give the scheduler's reason. {} keeps the runner off the node until \
             the node's condition ends; run `apprafter backup run` again once it has. Until then \
             the scheduled backup is kept off the same way.",
            what.join(" and ")
        ),
        Unplaced::Other => "The lines above give the scheduler's reason, and it is not a lack of \
                            room: no node accepts the runner's pod until it changes. Run \
                            `apprafter backup run` again once it has; the scheduled backup meets \
                            the same rules."
            .to_string(),
    };
    (what, help)
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Why the newest failed attempt of `job` failed, from its pod: the pod's own
/// reason (`Evicted: The node was low on resource: memory.`), else its
/// container's (`Error (exit 1)`), else `Failed`. `None` when no pod of the
/// Job has failed.
pub(crate) fn last_failed_attempt(job: &Value, pods: &[Value]) -> Option<String> {
    let pod = pods
        .iter()
        .filter(|p| owned_by(p, job))
        .filter(|p| p.pointer("/status/phase").and_then(Value::as_str) == Some("Failed"))
        .max_by_key(|p| {
            p.pointer("/metadata/creationTimestamp")
                .and_then(Value::as_str)
                .unwrap_or("")
        })?;
    let text = |ptr: &str| pod.pointer(ptr).and_then(Value::as_str).unwrap_or("");
    if let Some(r) = reason_with_message(text("/status/reason"), text("/status/message")) {
        return Some(r);
    }
    let terminated = ["/status/initContainerStatuses", "/status/containerStatuses"]
        .iter()
        .filter_map(|p| pod.pointer(p).and_then(Value::as_array))
        .flatten()
        .find_map(|s| s.pointer("/state/terminated"))
        .filter(|t| t.get("exitCode").and_then(Value::as_i64) != Some(0));
    Some(match terminated {
        Some(t) => {
            let reason = t.get("reason").and_then(Value::as_str).unwrap_or("Failed");
            match t.get("exitCode").and_then(Value::as_i64) {
                Some(code) => format!("{reason} (exit {code})"),
                None => reason.to_string(),
            }
        }
        None => "Failed".to_string(),
    })
}

/// The runner's own words for the newest failed attempt of `job`: the
/// `lastError` of its status ConfigMap (`status_cm`), when the runner wrote
/// it during that attempt, that is when its `lastFailure` is no earlier than
/// the attempt's pod was created. The runner records a failed run before it
/// exits, so for most failures this says what went wrong where the pod says
/// only `Error (exit 1)`. `None` when the record is older — an earlier
/// attempt's, an earlier Job's — or there is none: an attempt that was
/// killed (`OOMKilled`, `Evicted`) records nothing, and its pod says why.
/// With no failed pod to date the attempt by, the Job's start is used.
pub(crate) fn runner_error_during(
    job: &Value,
    pods: &[Value],
    status_cm: Option<&Value>,
) -> Option<String> {
    let at = |v: &Value, ptr: &str| {
        v.pointer(ptr)
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
    };
    let attempt_began = pods
        .iter()
        .filter(|p| owned_by(p, job))
        .filter(|p| p.pointer("/status/phase").and_then(Value::as_str) == Some("Failed"))
        .filter_map(|p| at(p, "/metadata/creationTimestamp"))
        .max()
        .or_else(|| at(job, "/status/startTime"))
        .or_else(|| at(job, "/metadata/creationTimestamp"))?;
    let data = status_cm?.get("data")?;
    if at(data, "/lastFailure")? < attempt_began {
        return None;
    }
    let error = data.get("lastError").and_then(Value::as_str)?.trim();
    (!error.is_empty()).then(|| error.to_string())
}

/// The line `backup run` prints when it sees that attempt `failed` (of at
/// most `attempts`) of its Job has failed, with `why` from
/// [`runner_error_during`] or [`last_failed_attempt`]. Printed once for each
/// attempt: `… retrying` alone does not say why, and the next attempt may
/// fail the same way.
pub(crate) fn failed_attempt_note(failed: u64, attempts: u64, why: Option<&str>) -> String {
    format!(
        "  … attempt {failed} of at most {attempts} failed: {}",
        why.unwrap_or("no reason was recorded")
    )
}

/// The error `backup run` ends with when its `--timeout` runs out after
/// `failed` attempts of the Job have failed and none has succeeded. The Job
/// is left to go on, as on any timeout, but the command does not exit 0: no
/// backup has been taken, and a script that reads the exit code must not
/// take a Job whose attempts fail for one that is only slow.
pub(crate) fn timed_out_failing(
    name: &str,
    timeout_minutes: u64,
    failed: u64,
    why: Option<&str>,
) -> String {
    format!(
        "backup Job {name} has taken no backup: {} before the wait ended at {timeout_minutes}m, \
         the last one: {}",
        failed_attempts(failed),
        why.unwrap_or("no reason was recorded")
    )
}

/// The pods, as `namespace/name`, that are placed on a node and being
/// deleted: the room they hold is given back when they are gone, and the
/// scheduler then tries an unschedulable pod again. Sorted.
///
/// A finished pod holds no room, and one never placed held none.
pub(crate) fn stopping_pods(pods: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = pods
        .iter()
        .filter(|p| p.pointer("/metadata/deletionTimestamp").is_some())
        .filter(|p| {
            p.pointer("/spec/nodeName")
                .and_then(Value::as_str)
                .is_some_and(|n| !n.is_empty())
        })
        .filter(|p| {
            !matches!(
                p.pointer("/status/phase").and_then(Value::as_str),
                Some("Succeeded" | "Failed")
            )
        })
        .map(|p| {
            let field = |k: &str| {
                p.pointer(&format!("/metadata/{k}"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
            };
            format!("{}/{}", field("namespace"), field("name"))
        })
        .collect();
    out.sort();
    out
}

/// A progress line for `backup run` while its pod has no room and pods are
/// stopping ([`stopping_pods`]): the unschedulable clock does not run.
pub(crate) fn stopping_note(stopping: &[String], waited: Duration) -> String {
    let pods = if stopping.len() == 1 {
        "1 pod stops and gives its".to_string()
    } else {
        format!("{} pods stop and give theirs", stopping.len())
    };
    format!(
        "  … no room for its pod yet; waiting while {pods} back: {} ({})",
        stopping.join(", "),
        elapsed(waited)
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

    /// A Job between an attempt that failed and the retry the Job controller
    /// creates after a back-off (10 s, doubling up to 6 min): no live pod,
    /// `active: 0`, `failed: 1`, and no condition.
    fn job_between_attempts() -> Value {
        json!({
            "metadata": {"name": "apprafter-backup-29312345", "uid": JOB_UID},
            "spec": {"backoffLimit": 6},
            "status": {"failed": 1, "startTime": "2026-09-23T14:39:47Z"}
        })
    }

    fn failed_attempt() -> Value {
        let mut p = running_pod();
        p["status"]["phase"] = json!("Failed");
        p
    }

    #[test]
    fn a_job_between_a_failed_attempt_and_its_retry_is_retrying() {
        // Its counts read `Failed` (no pod active, one failed), but the Job
        // has not failed: it has attempts left and will start the next one.
        let j = job_between_attempts();
        let retrying = JobPod::Retrying {
            failed: 1,
            attempts: 7,
        };
        assert_eq!(job_pod(&j, &[failed_attempt()]), retrying);
        assert_eq!(job_pod(&j, &[]), retrying);
        // The apiserver defaults `backoffLimit` to 6; a Job read without it
        // gets the same default rather than none.
        let mut no_limit = j.clone();
        no_limit["spec"]
            .as_object_mut()
            .unwrap()
            .remove("backoffLimit");
        assert_eq!(job_pod(&no_limit, &[]), retrying);
        let mut two = j.clone();
        two["spec"]["backoffLimit"] = json!(2);
        two["status"]["failed"] = json!(2);
        assert_eq!(
            job_pod(&two, &[]),
            JobPod::Retrying {
                failed: 2,
                attempts: 3
            }
        );
        // Once the retry's pod exists, it is what is read.
        assert!(matches!(
            job_pod(&j, &[failed_attempt(), unschedulable_pod()]),
            JobPod::Unschedulable { .. }
        ));
    }

    #[test]
    fn a_job_that_is_failing_or_has_no_attempt_left_is_not_retrying() {
        // The Job controller marks a failing Job `FailureTarget` first and
        // `Failed` once its pods are gone: in between it is not retrying.
        let mut failing = job_between_attempts();
        failing["status"]["conditions"] = json!([
            {"type": "FailureTarget", "status": "True", "reason": "DeadlineExceeded"}
        ]);
        assert_eq!(job_pod(&failing, &[]), JobPod::None);
        // Every attempt used: the Failed condition is on its way.
        let mut spent = job_between_attempts();
        spent["spec"]["backoffLimit"] = json!(0);
        assert_eq!(job_pod(&spent, &[]), JobPod::None);
        // Suspended: nothing will be started.
        let mut suspended = job_between_attempts();
        suspended["spec"]["suspend"] = json!(true);
        assert_eq!(job_pod(&suspended, &[]), JobPod::None);
        // A pod is active but this listing did not catch it: say nothing.
        let mut active = job_between_attempts();
        active["status"]["active"] = json!(1);
        assert_eq!(job_pod(&active, &[]), JobPod::None);
        // Nothing failed: a Job whose first pod is not there yet.
        let mut fresh = job_between_attempts();
        fresh["status"] = json!({"startTime": "2026-09-23T14:39:47Z"});
        assert_eq!(job_pod(&fresh, &[]), JobPod::None);
    }

    #[test]
    fn a_retrying_job_is_said_to_retry_by_every_command() {
        let r = JobPod::Retrying {
            failed: 1,
            attempts: 7,
        };
        assert_eq!(
            status_outcome(&r).as_deref(),
            Some("Retrying after 1 failed attempt (7 attempts at most)")
        );
        assert_eq!(
            status_outcome(&JobPod::Retrying {
                failed: 2,
                attempts: 7
            })
            .as_deref(),
            Some("Retrying after 2 failed attempts (7 attempts at most)")
        );
        let n = progress_note(&r, Duration::from_secs(40), None);
        assert_eq!(
            n,
            "  … retrying after 1 failed attempt, 7 attempts at most (40s)"
        );
        let t = timeout_note(&r, 60, "apprafter-system", "j");
        assert!(
            t.contains("it was retrying after 1 failed attempt when 60m ran out"),
            "{t}"
        );
        assert!(!t.contains("still running"), "{t}");
        assert_eq!(status_hint(&job_between_attempts(), &r), None);
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

    /// FIRES (live walk): a scheduled Job from a chart that set no
    /// `activeDeadlineSeconds` (0.2.79 and earlier) was told "Until this Job
    /// runs or its deadline stops it" — it has no deadline, nothing stops
    /// it, and it holds the schedule until someone deletes it. Say so, and
    /// give the command.
    #[test]
    fn a_scheduled_job_with_no_deadline_is_said_to_wait_until_deleted() {
        let mut stuck = job();
        stuck["metadata"]["name"] = json!("apprafter-backup-29312345");
        stuck["metadata"]["namespace"] = json!("apprafter-system");
        stuck["metadata"]["ownerReferences"] =
            json!([{"kind": "CronJob", "name": "apprafter-backup", "uid": "cj"}]);
        let h = status_hint(&stuck, &unsched("a")).unwrap();
        assert!(!h.contains("its deadline stops it"), "{h}");
        assert!(h.contains("no deadline"), "{h}");
        assert!(h.contains("until it runs or is deleted"), "{h}");
        assert!(h.contains("schedule starts no other backup"), "{h}");
        assert!(
            h.contains("kubectl -n apprafter-system delete job apprafter-backup-29312345"),
            "{h}"
        );

        // DOES NOT FIRE: with a deadline, the deadline is what ends it.
        stuck["spec"] = json!({"activeDeadlineSeconds": 21600});
        let h = status_hint(&stuck, &unsched("a")).unwrap();
        assert!(h.contains("its deadline stops it"), "{h}");
        assert!(!h.contains("no deadline"), "{h}");

        // The check CronJob's Job holds the CHECK schedule, not the backup's.
        stuck["metadata"]["ownerReferences"] =
            json!([{"kind": "CronJob", "name": "apprafter-backup-check", "uid": "cj"}]);
        let h = status_hint(&stuck, &unsched("a")).unwrap();
        assert!(h.contains("schedule starts no other check"), "{h}");
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
        let r = unschedulable_report(&give_up(INSUFFICIENT, &Unplaced::NoRoom, 0), Ok(()));
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

        let no_requests = GiveUp {
            requests: None,
            ..give_up(INSUFFICIENT, &Unplaced::NoRoom, 0)
        };
        let r = unschedulable_report(&no_requests, Err("forbidden".to_string()));
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

    // ------------------------------------------------------------------
    // Why the scheduler placed no pod: room, a node condition, or neither
    // ------------------------------------------------------------------

    /// The kubelet taints a node under memory pressure, and keeps the taint
    /// for five minutes after the pressure ends. A runner evicted by that
    /// pressure is retried into this.
    const MEMORY_PRESSURE: &str = "0/1 nodes are available: 1 node(s) had untolerated taint \
                                   {node.kubernetes.io/memory-pressure: }. preemption: 0/1 nodes \
                                   are available: 1 Preemption is not helpful for scheduling.";

    #[test]
    fn the_schedulers_reasons_are_read_as_room_a_node_condition_or_neither() {
        let cases: &[(&str, Unplaced)] = &[
            // The live run's message.
            (INSUFFICIENT, Unplaced::NoRoom),
            (
                "0/1 nodes are available: 1 Insufficient cpu, 1 Insufficient memory.",
                Unplaced::NoRoom,
            ),
            (
                "0/1 nodes are available: 1 Too many pods. preemption: 0/1 nodes are available: \
                 1 No preemption victims found for incoming pod.",
                Unplaced::NoRoom,
            ),
            // A node the runner can never use does not change what is
            // short on the one it can.
            (
                "0/3 nodes are available: 1 Insufficient memory, 2 node(s) had untolerated taint \
                 {node-role.kubernetes.io/control-plane: }. preemption: 0/3 nodes are available: \
                 1 No preemption victims found for incoming pod, 2 Preemption is not helpful for \
                 scheduling.",
                Unplaced::NoRoom,
            ),
            (
                MEMORY_PRESSURE,
                Unplaced::NodeCondition(vec!["node.kubernetes.io/memory-pressure".to_string()]),
            ),
            (
                "0/1 nodes are available: 1 node(s) had untolerated taint \
                 {node.kubernetes.io/not-ready: }.",
                Unplaced::NodeCondition(vec!["node.kubernetes.io/not-ready".to_string()]),
            ),
            // The form Kubernetes used before 1.25, whose clause carries
            // a comma of its own.
            (
                "0/1 nodes are available: 1 node(s) had taint {node.kubernetes.io/disk-pressure: \
                 }, that the pod didn't tolerate.",
                Unplaced::NodeCondition(vec!["node.kubernetes.io/disk-pressure".to_string()]),
            ),
            (
                "0/1 nodes are available: 1 node(s) were unschedulable. preemption: 0/1 nodes \
                 are available: 1 Preemption is not helpful for scheduling.",
                Unplaced::NodeCondition(vec!["a cordon".to_string()]),
            ),
            // The last reason ends the sentence with its full stop.
            (
                "0/1 nodes are available: 1 node(s) were unschedulable.",
                Unplaced::NodeCondition(vec!["a cordon".to_string()]),
            ),
            (
                "0/1 nodes are available: 1 Too many pods.",
                Unplaced::NoRoom,
            ),
            // Room short on one node while another is under pressure: the
            // pressure may lift and that node take it.
            (
                "0/2 nodes are available: 1 Insufficient memory, 1 node(s) had untolerated taint \
                 {node.kubernetes.io/memory-pressure: }.",
                Unplaced::NodeCondition(vec!["node.kubernetes.io/memory-pressure".to_string()]),
            ),
            (
                "0/1 nodes are available: 1 node(s) had untolerated taint {dedicated: gpu}.",
                Unplaced::Other,
            ),
            (
                "0/1 nodes are available: 1 node(s) didn't match Pod's node affinity/selector.",
                Unplaced::Other,
            ),
            ("the scheduler gave no reason", Unplaced::Other),
            ("", Unplaced::Other),
        ];
        for (message, want) in cases {
            assert_eq!(&unplaced(message), want, "{message}");
        }
    }

    #[test]
    fn a_node_condition_is_waited_for_longer_than_the_kubelet_keeps_its_taint() {
        // evictionPressureTransitionPeriod: 5 minutes by default.
        assert!(grace_for(&Unplaced::NodeCondition(vec![])) > Duration::from_secs(300));
        assert_eq!(grace_for(&Unplaced::NoRoom), UNSCHEDULABLE_GRACE);
        assert_eq!(grace_for(&Unplaced::Other), UNSCHEDULABLE_GRACE);
    }

    fn unsched_with(uid: &str, message: &str) -> JobPod {
        JobPod::Unschedulable {
            uid: uid.to_string(),
            message: message.to_string(),
        }
    }

    #[test]
    fn a_new_kind_of_reason_starts_a_new_streak() {
        // Five minutes kept off by a pressure taint, then the taint lifts
        // and there is no room: that is a new wait, not one already spent.
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut c = UnschedulableClock::default();
        assert_eq!(
            c.observe(&unsched_with("a", MEMORY_PRESSURE), t0),
            Some(Duration::ZERO)
        );
        assert_eq!(
            c.observe(&unsched_with("a", MEMORY_PRESSURE), t0 + s(300)),
            Some(s(300))
        );
        assert_eq!(c.observe(&unsched("a"), t0 + s(305)), Some(Duration::ZERO));
        assert_eq!(c.observe(&unsched("a"), t0 + s(310)), Some(s(5)));
        c.reset();
        assert_eq!(c.observe(&unsched("a"), t0 + s(315)), Some(Duration::ZERO));
    }

    #[test]
    fn only_a_lack_of_room_gets_the_room_advice() {
        let mut manual = job();
        manual["spec"] = json!({"template": {"spec": {"containers": [{
            "resources": {"requests": {"cpu": "100m", "memory": "256Mi"}}
        }]}}});
        let h = status_hint(&manual, &unsched_with("a", MEMORY_PRESSURE)).unwrap();
        assert!(
            h.contains("kept off by node.kubernetes.io/memory-pressure"),
            "{h}"
        );
        assert!(h.contains("five minutes after"), "{h}");
        assert!(h.contains(RUNNER_UNSCHEDULABLE_DOC), "{h}");
        for wrong in ["room", "256Mi", "apprafter top"] {
            assert!(!h.contains(wrong), "{wrong} in {h}");
        }
        let h = status_hint(
            &manual,
            &unsched_with(
                "a",
                "0/1 nodes are available: 1 node(s) had untolerated taint {dedicated: gpu}.",
            ),
        )
        .unwrap();
        assert!(h.contains("not a lack of room"), "{h}");
        assert!(!h.contains("256Mi"), "{h}");
        assert!(!h.contains("apprafter top"), "{h}");
        let n = progress_note(
            &unsched_with("a", MEMORY_PRESSURE),
            Duration::from_secs(95),
            Some(Duration::from_secs(90)),
        );
        assert!(n.contains("giving up at 10m 0s"), "{n}");
    }

    fn give_up<'a>(message: &'a str, cause: &'a Unplaced, failed: u64) -> GiveUp<'a> {
        GiveUp {
            namespace: "apprafter-system",
            name: "apprafter-backup-manual-x",
            message,
            cause,
            requests: Some("256Mi of memory and 100m of CPU"),
            holders: &[],
            failed,
            last_failure: (failed > 0).then_some(
                "Evicted: The node was low on resource: memory. Threshold quantity: 100Mi, \
                 available: 87004Ki.",
            ),
        }
    }

    #[test]
    fn an_attempt_after_one_that_failed_is_not_reported_as_never_started() {
        let r = unschedulable_report(&give_up(INSUFFICIENT, &Unplaced::NoRoom, 1), Ok(()));
        assert!(!r.contains("never started"), "{r}");
        assert!(
            r.contains("✗ Attempt 2 of the backup could not be scheduled: no node has room"),
            "{r}"
        );
        assert!(
            r.contains("Before it, 1 attempt failed (Evicted: The node was low on resource"),
            "{r}"
        );
        assert!(
            r.contains("shows the runner's lastError if that attempt recorded one."),
            "{r}"
        );
        let r = unschedulable_report(&give_up(INSUFFICIENT, &Unplaced::NoRoom, 2), Ok(()));
        assert!(
            r.contains("Before it, 2 attempts failed (the last: Evicted"),
            "{r}"
        );
        assert!(
            r.contains("shows the runner's lastError if one of them recorded it."),
            "{r}"
        );
        let (what, _) = give_up_error(
            &give_up(INSUFFICIENT, &Unplaced::NoRoom, 2),
            Duration::from_secs(123),
        );
        assert_eq!(
            what,
            "could not schedule attempt 3 for 2m 3s, after 2 failed attempts: no node had room \
             for its pod"
        );
        // With no attempt before it, it is the first and never started.
        let (what, help) = give_up_error(
            &give_up(INSUFFICIENT, &Unplaced::NoRoom, 0),
            Duration::from_secs(123),
        );
        assert_eq!(
            what,
            "never started: no node had room for its pod for 2m 3s"
        );
        assert!(help.contains("`apprafter top`"), "{help}");
        assert!(help.contains("`apprafter backup run`"), "{help}");
        assert!(
            help.contains("the scheduled backup cannot start either"),
            "{help}"
        );
    }

    #[test]
    fn a_give_up_on_a_node_condition_names_it_and_gives_no_room_advice() {
        let cause = Unplaced::NodeCondition(vec!["node.kubernetes.io/memory-pressure".to_string()]);
        let r = unschedulable_report(&give_up(MEMORY_PRESSURE, &cause, 0), Ok(()));
        assert!(
            r.contains(
                "✗ The backup never started: its pod was kept off by \
                 node.kubernetes.io/memory-pressure."
            ),
            "{r}"
        );
        assert!(
            r.contains(&format!("The scheduler says: {MEMORY_PRESSURE}\n")),
            "{r}"
        );
        assert!(r.contains("still there after 10m 0s"), "{r}");
        assert!(r.contains(RUNNER_UNSCHEDULABLE_DOC), "{r}");
        for wrong in ["room", "256Mi", "apprafter top", "asks for"] {
            assert!(!r.contains(wrong), "{wrong} in {r}");
        }
        let (what, help) =
            give_up_error(&give_up(MEMORY_PRESSURE, &cause, 0), NODE_CONDITION_GRACE);
        assert_eq!(
            what,
            "never started: its pod was kept off by node.kubernetes.io/memory-pressure for 10m 0s"
        );
        assert!(
            help.contains("node.kubernetes.io/memory-pressure"),
            "{help}"
        );
        assert!(help.contains("`apprafter backup run`"), "{help}");
        for wrong in ["room", "apprafter top", "bigger machine"] {
            assert!(!help.contains(wrong), "{wrong} in {help}");
        }

        let other = "0/1 nodes are available: 1 node(s) had untolerated taint {dedicated: gpu}.";
        let r = unschedulable_report(&give_up(other, &Unplaced::Other, 0), Ok(()));
        assert!(
            r.contains("✗ The backup never started: no node accepts its pod."),
            "{r}"
        );
        assert!(r.contains("not a lack of room"), "{r}");
        assert!(!r.contains("256Mi"), "{r}");
        assert!(!r.contains("apprafter top"), "{r}");
        let (what, help) = give_up_error(&give_up(other, &Unplaced::Other, 0), UNSCHEDULABLE_GRACE);
        assert_eq!(what, "never started: no node accepted its pod for 2m 0s");
        assert!(help.contains("not a lack of room"), "{help}");
    }

    #[test]
    fn the_last_failed_attempt_says_why_it_failed() {
        let mut evicted = running_pod();
        evicted["metadata"]["creationTimestamp"] = json!("2026-09-23T14:00:00Z");
        evicted["status"] = json!({
            "phase": "Failed", "reason": "Evicted",
            "message": "The node was low on resource: memory."
        });
        let mut crashed = running_pod();
        crashed["metadata"]["creationTimestamp"] = json!("2026-09-23T14:10:00Z");
        crashed["status"] = json!({"phase": "Failed", "containerStatuses": [{
            "name": "runner", "state": {"terminated": {"reason": "Error", "exitCode": 1}}
        }]});
        assert_eq!(
            last_failed_attempt(&job(), &[evicted.clone()]).as_deref(),
            Some("Evicted: The node was low on resource: memory.")
        );
        // The newest failed attempt, and the live pod is not one.
        assert_eq!(
            last_failed_attempt(
                &job(),
                &[evicted.clone(), crashed.clone(), unschedulable_pod()]
            )
            .as_deref(),
            Some("Error (exit 1)")
        );
        let mut bare = crashed.clone();
        bare["status"] = json!({"phase": "Failed"});
        assert_eq!(
            last_failed_attempt(&job(), &[bare]).as_deref(),
            Some("Failed")
        );
        let mut foreign = evicted;
        foreign["metadata"]["ownerReferences"] = owner("someone-else");
        assert_eq!(
            last_failed_attempt(&job(), &[foreign, unschedulable_pod()]),
            None
        );
    }

    /// The runner's `lastError` is quoted for the attempt it was written
    /// during, and for no other: not an earlier attempt's, not an earlier
    /// Job's, not one the kernel or the kubelet killed before it could write.
    #[test]
    fn the_runners_own_words_are_quoted_for_the_attempt_that_wrote_them() {
        let failed_at = |created: &str| {
            let mut p = running_pod();
            p["metadata"]["creationTimestamp"] = json!(created);
            p["status"] = json!({"phase": "Failed", "containerStatuses": [{
                "name": "runner", "state": {"terminated": {"reason": "Error", "exitCode": 1}}
            }]});
            p
        };
        let record = |at: &str, error: &str| {
            json!({"data": {"lastFailure": at, "lastError": error,
                            "lastSuccess": "2026-09-22T03:10:00+00:00"}})
        };
        let bucket = "restic backup failed (exit 1): Fatal: unable to open config file: \
                      The specified bucket does not exist.";
        let first = failed_at("2026-09-23T14:39:47Z");
        // Written 11 s into the attempt, with the runner's own precision.
        let cm = record("2026-09-23T14:39:58.412057339+00:00", bucket);
        assert_eq!(
            runner_error_during(&job(), std::slice::from_ref(&first), Some(&cm)).as_deref(),
            Some(bucket)
        );
        // The next attempt failed too, and was killed before it could write:
        // the first attempt's words are not its reason.
        let second = failed_at("2026-09-23T14:40:20Z");
        assert_eq!(
            runner_error_during(&job(), &[first.clone(), second], Some(&cm)),
            None
        );
        // A record from before this Job: an earlier run's.
        let old = record("2026-09-22T03:04:00+00:00", "an earlier run's error");
        assert_eq!(
            runner_error_during(&job(), std::slice::from_ref(&first), Some(&old)),
            None
        );
        // No failed pod left to date the attempt by: the Job's start does.
        assert_eq!(
            runner_error_during(&job(), &[], Some(&cm)).as_deref(),
            Some(bucket)
        );
        assert_eq!(runner_error_during(&job(), &[], Some(&old)), None);
        // Nothing recorded, or no record at all.
        let empty = record("2026-09-23T14:39:58+00:00", "  ");
        assert_eq!(
            runner_error_during(&job(), std::slice::from_ref(&first), Some(&empty)),
            None
        );
        assert_eq!(runner_error_during(&job(), &[first], None), None);
    }

    #[test]
    fn a_failed_attempt_and_a_timeout_after_failures_say_why() {
        assert_eq!(
            failed_attempt_note(2, 7, Some("the staging volume held 318Mi")),
            "  … attempt 2 of at most 7 failed: the staging volume held 318Mi"
        );
        assert_eq!(
            failed_attempt_note(1, 7, None),
            "  … attempt 1 of at most 7 failed: no reason was recorded"
        );
        let e = timed_out_failing(
            "apprafter-backup-manual-x",
            60,
            3,
            Some("OOMKilled (exit 137)"),
        );
        assert_eq!(
            e,
            "backup Job apprafter-backup-manual-x has taken no backup: 3 failed attempts before \
             the wait ended at 60m, the last one: OOMKilled (exit 137)"
        );
    }

    #[test]
    fn only_placed_pods_being_deleted_are_giving_room_back() {
        let mut stopping = running_pod();
        stopping["metadata"]["namespace"] = json!("demo");
        stopping["metadata"]["name"] = json!("shop-pg-1");
        stopping["metadata"]["deletionTimestamp"] = json!("2026-09-23T15:02:00Z");
        let mut unplaced_deleting = unschedulable_pod();
        unplaced_deleting["metadata"]["deletionTimestamp"] = json!("2026-09-23T15:02:00Z");
        let mut finished_deleting = stopping.clone();
        finished_deleting["metadata"]["name"] = json!("done");
        finished_deleting["status"]["phase"] = json!("Succeeded");
        assert_eq!(
            stopping_pods(&[
                running_pod(),
                stopping,
                unplaced_deleting,
                finished_deleting,
                unschedulable_pod()
            ]),
            vec!["demo/shop-pg-1".to_string()]
        );
        assert!(stopping_pods(&[running_pod()]).is_empty());
    }

    #[test]
    fn a_wait_for_pods_that_are_stopping_names_them() {
        let n = stopping_note(
            &["demo/shop-pg-1".to_string(), "demo/vault-0".to_string()],
            Duration::from_secs(150),
        );
        assert_eq!(
            n,
            "  … no room for its pod yet; waiting while 2 pods stop and give theirs back: \
             demo/shop-pg-1, demo/vault-0 (2m 30s)"
        );
    }

    #[test]
    fn a_give_up_while_another_runner_runs_names_it_instead_of_the_schedule() {
        // The weekly check, or a scheduled backup, holds the room this run
        // needs. Saying the scheduled backup cannot start would be wrong: it
        // may be the one running.
        let holders = ["apprafter-backup-check-29312350".to_string()];
        let g = GiveUp {
            holders: &holders,
            ..give_up(INSUFFICIENT, &Unplaced::NoRoom, 0)
        };
        let r = unschedulable_report(&g, Ok(()));
        assert!(
            r.contains(
                "The runner asks for 256Mi of memory and 100m of CPU. \
                 apprafter-backup-check-29312350 is running and holds room of the same size; \
                 this backup can start once it has finished."
            ),
            "{r}"
        );
        assert!(!r.contains("cannot start either"), "{r}");
        let (_, help) = give_up_error(&g, UNSCHEDULABLE_GRACE);
        assert!(help.contains("apprafter-backup-check-29312350"), "{help}");
        assert!(help.contains("`apprafter backup status`"), "{help}");
        assert!(!help.contains("cannot start either"), "{help}");
        assert!(!help.contains("bigger machine"), "{help}");
        let two = ["a".to_string(), "b".to_string()];
        let r = unschedulable_report(
            &GiveUp {
                holders: &two,
                ..give_up(INSUFFICIENT, &Unplaced::NoRoom, 0)
            },
            Ok(()),
        );
        assert!(r.contains("a and b are running and hold room"), "{r}");
    }
}
