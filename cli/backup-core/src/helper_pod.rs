// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Ephemeral helper-pod spec builders (pure) + `KubeExec`-forwarding stream
//! helpers (impure) for 2.6d backup/restore data extraction and load.
//!
//! # Pure pod-spec builders
//!
//! `pg_helper_pod_spec` / `volume_pod_spec` / `nats_pod_spec` return
//! serde_json::Value Pod specs that are applied via `apply_and_wait_pod_ready`
//! and then exec'd into. Each runs [`keep_alive_command`] — `sleep` for its
//! keep-alive ([`helper_keep_alive`]) — as its container command, so the pod
//! is there to exec into. `restartPolicy: Never` ensures a single attempt; the caller tears
//! down with `delete_pod_best_effort` after the stream completes.
//!
//! # How long a helper pod lives
//!
//! When the `sleep` ends, the container exits and every exec still running in
//! it dies with exit code 137. So the keep-alive is a hard cap on each single
//! extraction or load, whatever else allows it. It used to be a fixed `sleep
//! 3600`, so no one claim could take longer than an hour, even under a Job
//! deadline of six. It is now [`helper_keep_alive`]: the cluster's backup
//! deadline ([`run_deadline_of`]), and never less than six hours. A command
//! killed by it is explained ([`explain_keep_alive_end`]); the bare exit code
//! 137 says nothing about why.
//!
//! For a helper pod nobody deleted (a runner or a CLI killed before its
//! cleanup ran), the keep-alive ends the pod's process, not the Pod: with
//! `restartPolicy: Never` the object stays behind, `Completed`, until
//! something deletes it. The next run that applies a helper pod of that name
//! (the same step for the same claim) deletes it and creates its own
//! ([`stale_helper_reason`]); so does a run that finds one it cannot apply
//! over, built by another version with another spec. Applying over it used to
//! fail that run, after the whole five-minute Ready wait.
//!
//! # Impure forwarding helpers
//!
//! `apply_and_wait_pod_ready`, `exec_stream_to_file`, `exec_stream_from_file`,
//! `delete_pod_best_effort` delegate to the [`KubeExec`] trait so the engine
//! is portable across the CLI subprocess path and the future in-cluster runner.

use cli_core::Result;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use crate::kube::KubeExec;

// ---------------------------------------------------------------------------
// The run's deadline and the keep-alive it sets
// ---------------------------------------------------------------------------

/// The deadline of a backup run when `spec.backup.activeDeadlineSeconds` is
/// unset: the chart's default for the backup Job's `activeDeadlineSeconds`,
/// six hours. `scripts/check-backup-render.sh` asserts the two are the same
/// number.
pub const DEFAULT_RUN_DEADLINE: Duration = Duration::from_secs(21600);

/// The cluster's backup run deadline, read off `PlatformStack/default`:
/// `spec.backup.activeDeadlineSeconds`, else [`DEFAULT_RUN_DEADLINE`] — the
/// same resolution the chart makes for the backup Job. Pure.
///
/// A value that is not a positive whole number reads as unset: the CRD holds it
/// to 600 or more, so anything else is not a setting anyone made.
pub fn run_deadline_of(platformstack: Option<&Value>) -> Duration {
    platformstack
        .and_then(|ps| ps.pointer("/spec/backup/activeDeadlineSeconds"))
        .and_then(Value::as_u64)
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_RUN_DEADLINE)
}

/// How long a helper pod keeps itself alive, given the cluster's backup run
/// deadline ([`run_deadline_of`]): that deadline, and never less than
/// [`DEFAULT_RUN_DEADLINE`], six hours.
///
/// The scheduled runner and the CLI's `backup create`, `export` and `restore`
/// all use this, and why the floor:
///
/// * **The interactive commands have no Job deadline**, so the keep-alive is
///   the only limit on one dump or load of theirs. It used to be the backup
///   deadline alone, and that knob is set for the SCHEDULE: a cluster backing
///   up every fifteen minutes sets it to ten, and a restore whose `pg_restore`
///   needed eleven was then killed at ten, with a bare exit code 137. The
///   floor keeps it from shrinking with the schedule. Six hours is the
///   platform's own default for how long one backup of the cluster's data may
///   take, so an interactive run of the same data gets at least that; the old
///   fixed hour killed large loads. Raising the deadline for larger data
///   raises this too, which is the way to give a longer load more time.
/// * **The scheduled runner's Job deadline stops it first** whatever this
///   is, so the floor changes nothing about how long a scheduled run may
///   take. It keeps the runner's helper pods the SAME spec as the CLI's: a
///   pod's spec cannot change in place, so two specs for one name would
///   replace each other's pods (see [`is_immutable_pod_update`]) where one
///   spec simply reuses a running one.
/// * **The cost of a long keep-alive** is how long a helper leaked by a
///   killed command keeps running `sleep` — holding its env (a database
///   password) and any volume mount. The runner deletes its helpers when it
///   is stopped, and a leftover is replaced by the next run that needs its
///   name ([`stale_helper_reason`]), so six hours is also the most such a
///   leak lives by default.
pub fn helper_keep_alive(run_deadline: Duration) -> Duration {
    run_deadline.max(DEFAULT_RUN_DEADLINE)
}

/// `6h`, `90m`, `45s`, `5h59m50s`: a duration as `apprafter backup set
/// deadline` takes one, whole units and no zero parts.
pub fn human_duration(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60);
    let mut out = String::new();
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
        out.push_str(&format!("{m}m"));
    }
    if s > 0 || out.is_empty() {
        out.push_str(&format!("{s}s"));
    }
    out
}

/// The container command that keeps a helper pod alive for `keep_alive`
/// ([`helper_keep_alive`]; see the module docs for why that is the length).
pub fn keep_alive_command(keep_alive: Duration) -> Value {
    json!(["sleep", keep_alive.as_secs().max(1).to_string()])
}

// ---------------------------------------------------------------------------
// Pure pod-spec builders
// ---------------------------------------------------------------------------

/// Build the Pod spec of a PostgreSQL helper: the pod a backup or an export
/// runs `pg_dump` in, and the pod a restore runs `pg_restore` in.
///
/// No PVC mount — the container reaches the CNPG cluster Service over TCP. The
/// keep-alive command ([`keep_alive_command`]) lets the caller exec in and run
/// the tool after the pod reaches Running. Two variables go in the container
/// env, so that no exec'd command has to carry them:
///
/// * `PGPASSWORD` — the tool never prompts (there is no TTY to answer on) and
///   the password is never on an argv;
/// * `PGOPTIONS` = [`crate::extract::PG_HELPER_PGOPTIONS`] — a session whose
///   client is killed while it waits on a lock ends with it rather than
///   holding its locks until that lock is released.
///
/// The one builder for both sides on purpose: the restore's copy used to
/// replace the env with `PGPASSWORD` alone, so a `pg_restore` stopped while
/// its `--clean` waited for an `ACCESS EXCLUSIVE` lock left that request
/// queued on the server, blocking the application's queries on the table.
pub fn pg_helper_pod_spec(
    name: &str,
    ns: &str,
    image: &str,
    password: &str,
    keep_alive: Duration,
) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": { "apprafter.io/backup-helper": "true" }
        },
        "spec": {
            "restartPolicy": "Never",
            "containers": [{
                "name": "dump",
                "image": image,
                "command": keep_alive_command(keep_alive),
                "env": [
                    { "name": "PGPASSWORD", "value": password },
                    { "name": "PGOPTIONS", "value": crate::extract::PG_HELPER_PGOPTIONS }
                ]
            }]
        }
    })
}

/// Build a Pod spec that mounts a PersistentVolumeClaim at `/data`.
///
/// `read_only` = `true` for backup extraction (read the volume tree out into
/// a tar stream); `false` for `LoadData` restore (write the tree into a
/// freshly-provisioned PVC).  The distinction matters for the
/// `persistentVolumeClaim.readOnly` field and the matching `volumeMounts`
/// entry — Kubernetes enforces the readOnly flag on the mount, and a
/// RWO PVC mounted read-write on restore allows `tar x` to write files (L1).
pub fn volume_pod_spec(
    name: &str,
    ns: &str,
    image: &str,
    pvc: &str,
    read_only: bool,
    keep_alive: Duration,
) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": { "apprafter.io/backup-helper": "true" }
        },
        "spec": {
            "restartPolicy": "Never",
            "volumes": [{
                "name": "data",
                "persistentVolumeClaim": {
                    "claimName": pvc,
                    "readOnly": read_only
                }
            }],
            "containers": [{
                "name": "dump",
                "image": image,
                "command": keep_alive_command(keep_alive),
                "volumeMounts": [{
                    "name": "data",
                    "mountPath": "/data",
                    "readOnly": read_only
                }]
            }]
        }
    })
}

/// Build a Pod spec that runs the `nats` CLI against one server (2.6d-6).
///
/// No volumes: a JetStream stream is read over the NATS wire, not off a disk,
/// so this pod needs coordinates and credentials and nothing else. Both go in
/// the container ENV rather than on the command line — the `nats` CLI reads
/// `NATS_URL` / `NATS_USER` / `NATS_PASSWORD` natively, and an argv carrying
/// the manager password would show up in `ps` inside the pod and in any
/// `kubectl exec` audit entry. Same reasoning as `PGPASSWORD` above.
///
/// The pod belongs in the NAMESPACE NATS runs in: the manager Secret lives
/// there, and the default-deny NetworkPolicy bundle (`default` namespace only)
/// leaves same-namespace traffic to the server alone.
pub fn nats_pod_spec(
    name: &str,
    ns: &str,
    image: &str,
    url: &str,
    user: &str,
    password: &str,
    keep_alive: Duration,
) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": { "apprafter.io/backup-helper": "true" }
        },
        "spec": {
            "restartPolicy": "Never",
            "containers": [{
                "name": "dump",
                "image": image,
                "command": keep_alive_command(keep_alive),
                "env": [
                    { "name": "NATS_URL", "value": url },
                    { "name": "NATS_USER", "value": user },
                    { "name": "NATS_PASSWORD", "value": password }
                ]
            }]
        }
    })
}

/// POSIX-safe single-quote of an arbitrary string for embedding in a `sh -c`
/// script (wraps in single quotes, escaping embedded single quotes).
///
/// Shared by both sides of the backup: `extract` quotes stream names into the
/// dump script, `restore` quotes generated passwords into the Dragonfly load
/// script. It was private to the restore side until 2.6d-6 needed the same
/// rule — and two quoting functions in one repository is how one of them ends
/// up subtly different from the other.
pub fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ---------------------------------------------------------------------------
// A helper pod left from an earlier run
// ---------------------------------------------------------------------------

/// Why an existing pod with a helper pod's name cannot serve as that helper
/// and has to be deleted and created again; `None` when it can. Pure — both
/// [`KubeExec::apply_and_wait_pod_ready`] implementations ask it.
///
/// * **Ended** (`Succeeded` or `Failed`): a helper pod has `restartPolicy:
///   Never`, so once its keep-alive has run out, or its node lost it, it never
///   runs again. Applying the same spec over it changes nothing, and it never
///   becomes Ready; that used to fail the run that needed it after the whole
///   five-minute Ready wait.
/// * **Being deleted**: it is going away; the new one is created once it has.
///
/// A pod that is still running is not stale by this test: a run that applies
/// the same spec over it uses it as it is.
pub fn stale_helper_reason(pod: &Value) -> Option<String> {
    if pod
        .pointer("/metadata/deletionTimestamp")
        .is_some_and(|t| !t.is_null())
    {
        return Some("is being deleted".to_string());
    }
    match pod.pointer("/status/phase").and_then(Value::as_str) {
        Some(phase @ ("Succeeded" | "Failed")) => Some(format!(
            "has already ended (phase {phase}): it was left behind by an earlier run"
        )),
        _ => None,
    }
}

/// The apiserver's words for an update to a pod field that cannot change in
/// place, such as a container's `command` or `env`. A helper pod left by a
/// run that built a different spec for the same name — an older CLI or runner,
/// with another keep-alive or env — answers every apply with them.
pub const IMMUTABLE_POD_UPDATE: &str = "pod updates may not change fields";

/// Whether an apply was refused because the pod it would update has a spec
/// that cannot change in place ([`IMMUTABLE_POD_UPDATE`]). Such a pod is
/// replaced, like an ended one.
pub fn is_immutable_pod_update(message: &str) -> bool {
    message.contains(IMMUTABLE_POD_UPDATE)
}

/// How long a replaced helper pod may take to be gone before the apply gives
/// up. It is deleted with a one-second grace period
/// ([`STALE_POD_DELETE_GRACE_SECONDS`]), so this is time for the kubelet to
/// confirm, with a wide margin; a pod on an unreachable node never goes, and
/// that is an error worth stopping on.
pub const STALE_POD_GONE_WITHIN: Duration = Duration::from_secs(60);

/// The grace period a stale helper pod is deleted with. Its `sleep` is PID 1
/// of its container and ignores SIGTERM, so the default thirty seconds would
/// all be spent waiting; the work in it, if any, is abandoned.
pub const STALE_POD_DELETE_GRACE_SECONDS: u32 = 1;

// ---------------------------------------------------------------------------
// A command killed by its helper pod's keep-alive
// ---------------------------------------------------------------------------

/// How long [`explain_keep_alive_end`] waits for the pod's status to show
/// that its container has ended. A command killed with its container returns
/// at once, while the kubelet reports the container's end a second or two
/// later.
pub const KEEP_ALIVE_STATUS_WAIT: Duration = Duration::from_secs(15);

/// Whether an exec error reports exit code 137 — a command killed by
/// SIGKILL — in either implementation's words: kube-rs carries the
/// apiserver's `exit code 137`, `kubectl exec` its own `exit status: 137`.
pub fn is_exit_137(err: &cli_core::CliError) -> bool {
    let msg = err.to_string();
    msg.contains("exit code 137") || msg.contains("exit status: 137")
}

/// What a helper pod's status says about its keep-alive.
#[derive(Debug, PartialEq, Eq)]
pub enum KeepAliveState {
    /// The container has ended with exit code 0: its `sleep` ran out. The
    /// keep-alive it had, read off its own command.
    Ended(Option<Duration>),
    /// The container is still running.
    Running,
    /// Anything else: ended some other way, gone, or no status yet.
    Other,
}

/// Read [`KeepAliveState`] off a helper pod. Pure.
///
/// `sleep` exits 0 only when its time is up; a container killed otherwise
/// (the pod deleted, its node lost) ends with another code.
pub fn keep_alive_state(pod: &Value) -> KeepAliveState {
    let state = pod.pointer("/status/containerStatuses/0/state");
    if let Some(ended) = state.and_then(|s| s.get("terminated")) {
        if ended.get("exitCode").and_then(Value::as_i64) == Some(0) {
            let keep_alive = pod
                .pointer("/spec/containers/0/command")
                .and_then(Value::as_array)
                .and_then(|c| match c.as_slice() {
                    [cmd, secs] if cmd == "sleep" => secs.as_str()?.parse::<u64>().ok(),
                    _ => None,
                })
                .map(Duration::from_secs);
            return KeepAliveState::Ended(keep_alive);
        }
        return KeepAliveState::Other;
    }
    if state.and_then(|s| s.get("running")).is_some() {
        return KeepAliveState::Running;
    }
    KeepAliveState::Other
}

/// Explain an exec in helper pod `ns/pod` that was killed because the pod's
/// keep-alive ran out; any other error comes back unchanged.
///
/// Only an exit code 137 is looked into ([`is_exit_137`]): the pod is read
/// until its status shows the container ended ([`KEEP_ALIVE_STATUS_WAIT`]),
/// and only an end with exit code 0 — `sleep` running out — is the
/// keep-alive. The error then says so, how long the keep-alive was, and how
/// to raise it.
pub fn explain_keep_alive_end(
    k: &dyn KubeExec,
    pod: &str,
    ns: &str,
    err: cli_core::CliError,
) -> cli_core::CliError {
    explain_keep_alive_end_within(
        k,
        pod,
        ns,
        err,
        KEEP_ALIVE_STATUS_WAIT,
        Duration::from_secs(1),
    )
}

/// [`explain_keep_alive_end`] with its wait and poll interval given.
pub fn explain_keep_alive_end_within(
    k: &dyn KubeExec,
    pod: &str,
    ns: &str,
    err: cli_core::CliError,
    wait: Duration,
    interval: Duration,
) -> cli_core::CliError {
    if !is_exit_137(&err) {
        return err;
    }
    let deadline = std::time::Instant::now() + wait;
    loop {
        let state = match k.get_json(&["get", "pods", pod, "-n", ns, "-o", "json"]) {
            Ok(Some(p)) => keep_alive_state(&p),
            // Gone, or unreadable: nothing to say about it.
            Ok(None) | Err(_) => return err,
        };
        match state {
            KeepAliveState::Ended(keep_alive) => {
                let how_long = keep_alive
                    .map(|d| format!(" of {}", human_duration(d)))
                    .unwrap_or_default();
                return cli_core::CliError::Other(format!(
                    "the command in helper pod {ns}/{pod} was killed (exit code 137) because \
                     the pod's keep-alive{how_long} ran out: a helper pod keeps itself alive \
                     for a fixed time, and every command still running in it ends when that \
                     does. It is the cluster's backup deadline, and never less than six hours; \
                     for a dump or load that needs longer, raise it with `apprafter backup set \
                     deadline <longer>`, which also lets a scheduled backup run that long, and \
                     run the command again.\n{err}"
                ));
            }
            KeepAliveState::Other => return err,
            KeepAliveState::Running => {}
        }
        if std::time::Instant::now() >= deadline {
            return err;
        }
        std::thread::sleep(interval);
    }
}

/// The line both implementations print when they replace a stale helper pod.
pub fn replacing_stale_helper_note(ns: &str, name: &str, why: &str) -> String {
    format!("helper pod {ns}/{name} {why}; deleting it and creating it again")
}

// ---------------------------------------------------------------------------
// Impure forwarding helpers — delegate to KubeExec
// ---------------------------------------------------------------------------

/// Apply a Pod spec JSON and block until the pod reaches `Ready`.
/// Delegates to [`KubeExec::apply_and_wait_pod_ready`].
pub fn apply_and_wait_pod_ready(k: &dyn KubeExec, spec: &Value) -> Result<()> {
    k.apply_and_wait_pod_ready(spec)
}

/// Stream data DOWN from a pod to a local file (backup extraction).
/// Delegates to [`KubeExec::exec_stream_to_file`], which says what
/// `first_output_within` bounds.
pub fn exec_stream_to_file(
    k: &dyn KubeExec,
    pod: &str,
    ns: &str,
    argv: &[&str],
    out_path: &Path,
    first_output_within: Option<std::time::Duration>,
) -> Result<()> {
    k.exec_stream_to_file(pod, ns, argv, out_path, first_output_within)
}

/// Stream data UP from a local file into a pod (restore / LoadData L2).
/// Delegates to [`KubeExec::exec_stream_from_file`].
pub fn exec_stream_from_file(
    k: &dyn KubeExec,
    pod: &str,
    ns: &str,
    argv: &[&str],
    in_path: &Path,
) -> Result<()> {
    k.exec_stream_from_file(pod, ns, argv, in_path)
}

/// Delete the helper pod, ignoring errors (best-effort cleanup guard).
/// Delegates to [`KubeExec::delete_pod_best_effort`].
pub fn delete_pod_best_effort(k: &dyn KubeExec, name: &str, ns: &str) {
    k.delete_pod_best_effort(name, ns)
}

// ---------------------------------------------------------------------------
// Tests (pure pod-spec builders only — impure fns require a live cluster)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_dump_pod_uses_pg_image_and_no_pvc_mount() {
        let p = pg_helper_pod_spec(
            "bk-pg-alpha",
            "demo",
            "postgres:16-alpine",
            "pw",
            DEFAULT_RUN_DEADLINE,
        );
        assert_eq!(p["spec"]["containers"][0]["image"], "postgres:16-alpine");
        assert_eq!(p["metadata"]["namespace"], "demo");
        assert!(p["spec"]["containers"][0]["command"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "sleep"));
        assert!(p["spec"].get("volumes").is_none());
        assert_eq!(p["spec"]["restartPolicy"], "Never");
    }

    #[test]
    fn volume_pod_mounts_pvc_read_only_for_backup() {
        let p = volume_pod_spec(
            "bk-vol-data",
            "demo",
            "busybox:1.36",
            "sv-demo-shared",
            true,
            DEFAULT_RUN_DEADLINE,
        );
        let vol = &p["spec"]["volumes"][0];
        assert_eq!(vol["persistentVolumeClaim"]["claimName"], "sv-demo-shared");
        assert_eq!(vol["persistentVolumeClaim"]["readOnly"], true);
        assert_eq!(
            p["spec"]["containers"][0]["volumeMounts"][0]["readOnly"],
            true
        );
    }

    #[test]
    fn volume_pod_rw_for_restore_load() {
        // L1: LoadData must WRITE the tree into the fresh PVC → read_only=false
        let p = volume_pod_spec(
            "ld-vol-data",
            "demo",
            "busybox:1.36",
            "claim-x",
            false,
            DEFAULT_RUN_DEADLINE,
        );
        assert_eq!(
            p["spec"]["volumes"][0]["persistentVolumeClaim"]["readOnly"],
            false
        );
        assert_eq!(
            p["spec"]["containers"][0]["volumeMounts"][0]["readOnly"],
            false
        );
    }

    #[test]
    fn every_helper_pod_keeps_itself_alive_for_the_run_deadline_it_is_given() {
        // Not an hour: the `sleep` ending kills every exec in the pod, so a
        // fixed hour capped every single extraction at an hour, whatever the
        // Job deadline said.
        let twelve_hours = Duration::from_secs(43200);
        let want = json!(["sleep", "43200"]);
        for spec in [
            pg_helper_pod_spec("bk-pg-db", "demo", "postgres:18-alpine", "pw", twelve_hours),
            volume_pod_spec(
                "bk-vol-v",
                "demo",
                "busybox:1.36",
                "pvc",
                true,
                twelve_hours,
            ),
            nats_pod_spec(
                "bk-js-s",
                "nats",
                "nats:2",
                "nats://n:4222",
                "u",
                "p",
                twelve_hours,
            ),
        ] {
            assert_eq!(spec["spec"]["containers"][0]["command"], want, "{spec}");
        }
    }

    #[test]
    fn an_ended_or_departing_pod_is_stale_and_a_live_one_is_not() {
        for phase in ["Succeeded", "Failed"] {
            let pod = json!({"metadata": {"name": "bk-pg-db"}, "status": {"phase": phase}});
            let why = stale_helper_reason(&pod).expect(phase);
            assert!(why.contains(phase), "{why}");
        }
        let deleting = json!({
            "metadata": {"name": "bk-pg-db", "deletionTimestamp": "2026-09-23T00:00:00Z"},
            "status": {"phase": "Running"}
        });
        assert_eq!(
            stale_helper_reason(&deleting).as_deref(),
            Some("is being deleted")
        );
        for live in [
            json!({"metadata": {"name": "bk-pg-db"}, "status": {"phase": "Running"}}),
            json!({"metadata": {"name": "bk-pg-db"}, "status": {"phase": "Pending"}}),
            json!({"metadata": {"name": "bk-pg-db", "deletionTimestamp": null}}),
            json!({"metadata": {"name": "bk-pg-db"}}),
        ] {
            assert_eq!(stale_helper_reason(&live), None, "{live}");
        }
    }

    #[test]
    fn only_the_apiservers_immutable_update_refusal_counts() {
        // The apiserver's own text (k8s 1.35), trimmed.
        assert!(is_immutable_pod_update(
            "Pod \"bk-pg-db\" is invalid: spec: Forbidden: pod updates may not change fields \
             other than `spec.containers[*].image`,`spec.initContainers[*].image`,..."
        ));
        for other in [
            "pods \"bk-pg-db\" is forbidden: User cannot patch resource",
            "Pod \"bk-pg-db\" is invalid: metadata.name: Invalid value",
        ] {
            assert!(!is_immutable_pod_update(other), "{other}");
        }
    }

    #[test]
    fn a_helper_lives_for_the_deadline_and_never_less_than_six_hours() {
        // A schedule-tuned deadline does not shorten it…
        assert_eq!(
            helper_keep_alive(Duration::from_secs(600)),
            Duration::from_secs(6 * 3600)
        );
        assert_eq!(
            helper_keep_alive(DEFAULT_RUN_DEADLINE),
            DEFAULT_RUN_DEADLINE
        );
        // …a raised one lengthens it.
        assert_eq!(
            helper_keep_alive(Duration::from_secs(43200)),
            Duration::from_secs(43200)
        );
    }

    #[test]
    fn human_duration_writes_whole_units_only() {
        for (secs, want) in [
            (0, "0s"),
            (45, "45s"),
            (90, "1m30s"),
            (600, "10m"),
            (3600, "1h"),
            (21600, "6h"),
            (5400, "1h30m"),
            (21590, "5h59m50s"),
            (43200, "12h"),
        ] {
            assert_eq!(human_duration(Duration::from_secs(secs)), want, "{secs}s");
        }
    }

    fn helper_with(state: Value) -> Value {
        json!({
            "spec": {"containers": [{"name": "dump", "command": ["sleep", "21600"]}]},
            "status": {"containerStatuses": [{"name": "dump", "state": state}]}
        })
    }

    #[test]
    fn only_a_sleep_that_ran_out_is_the_keep_alive() {
        assert_eq!(
            keep_alive_state(&helper_with(
                json!({"terminated": {"exitCode": 0, "reason": "Completed"}})
            )),
            KeepAliveState::Ended(Some(Duration::from_secs(21600)))
        );
        // Killed with its pod (deleted, evicted): not the keep-alive.
        assert_eq!(
            keep_alive_state(&helper_with(
                json!({"terminated": {"exitCode": 137, "reason": "Error"}})
            )),
            KeepAliveState::Other
        );
        assert_eq!(
            keep_alive_state(&helper_with(
                json!({"running": {"startedAt": "2026-09-23T00:00:00Z"}})
            )),
            KeepAliveState::Running
        );
        assert_eq!(keep_alive_state(&json!({})), KeepAliveState::Other);
    }

    /// Answers `get pods` from a script of pod documents, one per call, the
    /// last repeated; `None` is a pod that is gone.
    struct PodReads(
        std::sync::Mutex<Vec<Option<Value>>>,
        std::sync::Mutex<usize>,
    );

    impl KubeExec for PodReads {
        fn apply_and_wait_pod_ready(&self, _: &Value) -> Result<()> {
            unreachable!()
        }
        fn exec_stream_to_file(
            &self,
            _: &str,
            _: &str,
            _: &[&str],
            _: &Path,
            _: Option<Duration>,
        ) -> Result<()> {
            unreachable!()
        }
        fn exec_stream_from_file(&self, _: &str, _: &str, _: &[&str], _: &Path) -> Result<()> {
            unreachable!()
        }
        fn delete_pod_best_effort(&self, _: &str, _: &str) {
            unreachable!()
        }
        fn get_secret_key(&self, _: &str, _: &str, _: &str) -> Result<String> {
            unreachable!()
        }
        fn get_json(&self, args: &[&str]) -> Result<Option<Value>> {
            assert_eq!(
                args,
                ["get", "pods", "ld-pg-db", "-n", "shop", "-o", "json"]
            );
            let mut n = self.1.lock().unwrap();
            let script = self.0.lock().unwrap();
            let answer = script[(*n).min(script.len() - 1)].clone();
            *n += 1;
            Ok(answer)
        }
    }

    fn reads(script: Vec<Option<Value>>) -> PodReads {
        PodReads(std::sync::Mutex::new(script), std::sync::Mutex::new(0))
    }

    fn killed() -> cli_core::CliError {
        cli_core::CliError::Other(
            "exec_stream_from_file: kubectl exec exited with exit status: 137.\n\
             kubectl stderr:\n  command terminated with exit code 137"
                .into(),
        )
    }

    fn explain(k: &PodReads, err: cli_core::CliError) -> String {
        explain_keep_alive_end_within(
            k,
            "ld-pg-db",
            "shop",
            err,
            Duration::from_millis(200),
            Duration::from_millis(10),
        )
        .to_string()
    }

    #[test]
    fn a_command_killed_by_the_keep_alive_is_told_so_and_how_to_raise_it() {
        // The container's end reaches the pod's status a moment after the
        // exec has failed: the explanation waits for it.
        let k = reads(vec![
            Some(helper_with(json!({"running": {}}))),
            Some(helper_with(json!({"terminated": {"exitCode": 0}}))),
        ]);
        let msg = explain(&k, killed());
        assert!(
            msg.starts_with("the command in helper pod shop/ld-pg-db was killed (exit code 137)"),
            "{msg}"
        );
        assert!(msg.contains("keep-alive of 6h ran out"), "{msg}");
        assert!(msg.contains("apprafter backup set deadline"), "{msg}");
        assert!(msg.contains("never less than six hours"), "{msg}");
        assert!(
            msg.ends_with("command terminated with exit code 137"),
            "{msg}"
        );

        // kube-rs words it as the apiserver does.
        let k = reads(vec![Some(helper_with(
            json!({"terminated": {"exitCode": 0}}),
        ))]);
        let msg = explain(
            &k,
            cli_core::CliError::Other(
                "exec_stream_to_file: exec [\"tar\"] in shop/ld-pg-db failed (status=Some(\"Failure\"), \
                 reason=NonZeroExitCode): command terminated with non-zero exit code: error \
                 executing command [tar], exit code 137"
                    .into(),
            ),
        );
        assert!(msg.contains("keep-alive of 6h ran out"), "{msg}");
    }

    #[test]
    fn any_other_137_or_failure_is_left_as_it_is() {
        let original = killed().to_string();
        // Killed with its pod, gone, or still running past the wait.
        for script in [
            vec![Some(helper_with(json!({"terminated": {"exitCode": 137}})))],
            vec![None],
            vec![Some(helper_with(json!({"running": {}})))],
        ] {
            let k = reads(script.clone());
            assert_eq!(explain(&k, killed()), original, "{script:?}");
        }
        // Not a 137 at all: the pod is not even read.
        let k = reads(vec![]);
        let other = cli_core::CliError::Other("pg_restore: error: could not connect".into());
        assert_eq!(explain(&k, other), "pg_restore: error: could not connect");
    }

    #[test]
    fn the_run_deadline_is_the_clusters_setting_or_six_hours() {
        assert_eq!(DEFAULT_RUN_DEADLINE, Duration::from_secs(6 * 3600));
        let set = json!({"spec": {"backup": {"activeDeadlineSeconds": 43200}}});
        assert_eq!(run_deadline_of(Some(&set)), Duration::from_secs(43200));
        for unset in [
            json!({"spec": {"backup": {"enabled": true}}}),
            json!({"spec": {}}),
            json!({"spec": {"backup": {"activeDeadlineSeconds": 0}}}),
            json!({"spec": {"backup": {"activeDeadlineSeconds": "12h"}}}),
        ] {
            assert_eq!(
                run_deadline_of(Some(&unset)),
                DEFAULT_RUN_DEADLINE,
                "{unset}"
            );
        }
        assert_eq!(run_deadline_of(None), DEFAULT_RUN_DEADLINE);
    }
}
