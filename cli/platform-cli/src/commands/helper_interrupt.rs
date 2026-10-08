// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What `apprafter backup create`, `apprafter export` and `apprafter restore`
//! do when they are interrupted — Ctrl-C (SIGINT) or SIGTERM: delete the
//! helper pods this process created, and nothing else.
//!
//! # Why a handler, and why only this
//!
//! Each of the three commands runs its dumps and loads in helper pods that
//! keep themselves alive for six hours or more (`backup_core::helper_pod`),
//! and deletes each one when its step is done, on every return path, by a
//! guard. A signal's default action kills the process without unwinding, so
//! no guard ran: an interrupted command left its helper running `sleep`, and
//! only the next run of the same step replaced it. So on the first SIGINT or
//! SIGTERM these commands now:
//!
//! 1. refuse every later helper apply, and wait at most
//!    [`APPLY_SETTLE_BOUND`] for an apply already under way to be answered;
//! 2. delete each helper pod this process CREATED, and only those (see
//!    below);
//! 3. give the command's own thread up to [`UNWIND_BOUND`] to stop: its
//!    temporary files (a decrypted kubeconfig, staged data) are removed as it
//!    unwinds, and a restore prints what it left down;
//! 4. say what they did, and exit with 128 + the signal's number (130 for
//!    SIGINT, 143 for SIGTERM).
//!
//! All of it within [`STOP_BOUND`]. A second signal ends the process at once,
//! from the signal handler itself, with the same code; the one thing it does
//! first is remove the copies of the kubeconfig the stop writes for its own
//! kubectl ([`COPY_PATHS`]), which would otherwise stay in `$TMPDIR`
//! decrypted. The command's own temporary kubeconfig stays, as after any kill.
//!
//! Nothing else is undone. A restore stopped partway leaves its applications
//! scaled down; the record of what to bring them back to is in the cluster,
//! on each application (`restore::PRE_RESTORE_REPLICAS_ANNOTATION`), and
//! running the same restore again is what puts them back. That split is
//! deliberate: deleting a pod this process created cannot hurt anything
//! else, while reversing a half-applied restore from a signal handler could.
//!
//! # Which pods are this process's
//!
//! A helper pod's name has no owner: two runs of the same step for the same
//! claim use one name, and a run applies over a running pod of the same spec
//! instead of replacing it. So only the apiserver can say which pod a run
//! created, and it says so in one place: the answer to a CREATE. A helper
//! that is not there is created (`kubectl create`, which fails rather than
//! touch a pod another run created in the meantime) and one that is there is
//! applied over; each is recorded before it is sent and settled by the
//! apiserver's answer, the uid of the pod it created or applied over
//! ([`Origin`]). At stop time ([`cleanup_action`]):
//!
//! * a pod this process's create was answered with is deleted, with that uid
//!   as the delete's precondition, so a pod of the same name created since by
//!   someone else is never the one deleted;
//! * a pod this process applied over is left alone: another run may be using
//!   it, and that run deletes it;
//! * a pod whose create or apply was never answered is left too, and named
//!   with the command that deletes it. The terminal sends Ctrl-C to every
//!   process in the foreground group — the `kubectl` this process is running
//!   included — so the call under way may have died before or after it
//!   reached the apiserver, and a pod of that name found afterwards may just
//!   as well be another run's. Only an answer makes a pod this process's; a
//!   read after the fact cannot.
//!
//! # The command's own thread, after the signal
//!
//! The signal does not stop the command's own thread. Ctrl-C reaches the
//! whole foreground group, so the kubectl or restic the thread waits on
//! usually dies with it; a SIGTERM sent to this process alone (`timeout`,
//! systemd, a cancelled CI job) does not, and the thread carries on for as
//! long as the stop waits. So from the moment the signal arrives it starts
//! nothing ([`refuse_if_interrupted`]): `KubectlExec`, the kubectl wrappers
//! of `k8s_helpers`, the restic runs of `backup create` and `restore`, and
//! each step of a restore refuse before they spawn, and what was under way
//! when the signal came is the last thing it does. Nor does it delete
//! (`KubectlExec::delete_pod_best_effort` returns at once): the deletes
//! above, with their preconditions, are the only ones made.
//!
//! `restore --reprovision` installs the handler only once its cluster
//! exists. The provisioning before that runs in-process — Hetzner API calls,
//! helm, kubectl — where no refusal reaches every call, and there is no
//! helper pod to delete yet; a signal there ends the process at once.
//!
//! # On Windows
//!
//! There are no signals. A console control handler takes Ctrl-C, Ctrl-Break
//! and the console window closing, and does what SIGINT does above, exiting
//! with 130: the first event marks the process interrupted, wakes the same
//! `interrupt` thread the Unix path runs the stop on, and returns; a second
//! one, while the stop is still working, removes the stop's kubeconfig
//! copies and ends the process from the handler, at once and with nothing
//! else run (`TerminateProcess`, the counterpart of `_exit`). Windows ends a
//! process about five seconds after its console window closes, so that
//! event gives the stop four seconds, not [`STOP_BOUND`], and its handler
//! does not return — returning would let Windows end the process straight
//! away — but waits for the stop to end it.
//!
//! Windows has no SIGTERM: `taskkill /F` and Task Manager's End process end
//! the CLI at once, as SIGKILL does, and none of this runs. Logoff and
//! shutdown are left to the default handling: Windows sends those events to
//! services only, and ends an interactive console program without them.
//!
//! The guarantee about the command's own thread is weaker than on Unix,
//! where the handler sets the flag before any thread can see a child die.
//! The console delivers Ctrl-C to every process attached to it, each on a
//! thread of its own, in no defined order: the `kubectl` the command waits
//! on can die of it, and the command's thread see that, before this
//! process's handler has run. So a kubectl child that exits with
//! `STATUS_CONTROL_C_EXIT` marks the process interrupted before its result
//! is returned ([`note_child_exit`]), and the next step refuses as it would
//! after the event; should no event follow, the command's thread starts the
//! stop itself once it has unwound ([`StopGuard`]). Restic children are not
//! checked this way yet: they run through `backup_core`, which moves with
//! the backup commands (WI-439).
//!
//! The in-cluster runner has its own, different stop (`apprafter-backup`'s
//! `stop` module): it is PID 1 of a Job's pod, is stopped by SIGTERM at its
//! deadline, and records the run's failure as well.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Output, Stdio};
#[cfg(unix)]
use std::sync::atomic::AtomicPtr;
#[cfg(windows)]
use std::sync::atomic::AtomicU32;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::{mpsc, OnceLock};
use std::sync::{Arc, LazyLock, Mutex, Once};
use std::thread;
use std::time::{Duration, Instant};

use cli_core::{CliError, Result};

/// The most the whole stop may take, from the signal to the exit.
pub const STOP_BOUND: Duration = Duration::from_secs(15);

/// Windows: the most the stop may take when the console window closes.
/// Windows ends the process about five seconds after that event arrives,
/// whatever its handler is doing.
#[cfg(windows)]
const CLOSE_STOP_BOUND: Duration = Duration::from_secs(4);

/// Windows: how long the command's own thread, unwound after a kubectl died
/// of a console Ctrl-C, waits for the console's event to start the stop
/// before it starts the stop itself ([`StopGuard`]).
#[cfg(windows)]
const EVENT_GRACE: Duration = Duration::from_secs(1);

/// The most the stop waits for a helper create or apply under way to be
/// answered before it acts on the pods. kubectl answers in well under a
/// second, or dies with the terminal's Ctrl-C at once.
pub const APPLY_SETTLE_BOUND: Duration = Duration::from_secs(5);

/// The most the stop waits, once its deletes are done, for the command's own
/// thread to stop: long enough for its unwinding (temporary files, a
/// restore's closing lines) after the kubectl it was waiting on died with the
/// same Ctrl-C.
pub const UNWIND_BOUND: Duration = Duration::from_secs(3);

/// The grace period a helper pod is deleted with: its `sleep` is PID 1 of its
/// container and ignores SIGTERM, so a longer one would only be waited out.
const DELETE_GRACE_SECONDS: u32 = 1;

/// Set by the signal handler itself — an atomic store, the one thing a handler
/// may safely do — the moment SIGINT or SIGTERM arrives, before any thread
/// can see a `kubectl` child that died of the same Ctrl-C. On Windows, set by
/// the console handler on the first event, and by [`note_child_exit`] (see
/// the module docs).
static INTERRUPTED: LazyLock<Arc<AtomicBool>> = LazyLock::new(|| Arc::new(AtomicBool::new(false)));

/// Set by [`StopGuard`] when the command's own thread has unwound and parked.
static UNWOUND: AtomicBool = AtomicBool::new(false);

/// What the stop adds about the command, after its own lines.
static NOTE: Mutex<Option<&'static str>> = Mutex::new(None);

/// Whether this process has received SIGINT or SIGTERM (with the handler
/// installed).
pub(crate) fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst) || test_seam::this_thread_interrupted()
}

/// The error a helper operation refused after the interrupt returns.
pub(crate) fn interrupted_error() -> CliError {
    CliError::Other("interrupted: the helper pods this command created are being deleted".into())
}

/// Refuse to start anything once the process has been interrupted: every
/// `kubectl` and `restic` the command's own thread would run checks this
/// first (see the module docs).
pub(crate) fn refuse_if_interrupted() -> Result<()> {
    if interrupted() {
        return Err(interrupted_error());
    }
    Ok(())
}

/// `STATUS_CONTROL_C_EXIT`: the exit code of a Windows console program that
/// a Ctrl-C or Ctrl-Break ended, as `ExitStatus::code` reports it.
const STATUS_CONTROL_C_EXIT: i32 = 0xC000_013A_u32 as i32;

/// Whether a child that exited with `code` was ended by a console Ctrl-C or
/// Ctrl-Break (Windows). Pure, and the same on every platform: a Unix exit
/// code is never this value.
pub(crate) fn exited_by_console_interrupt(code: Option<i32>) -> bool {
    code == Some(STATUS_CONTROL_C_EXIT)
}

/// Read how a `kubectl` child the command waited on ended, before its result
/// is returned: on Windows, with the console handler installed, one that a
/// console Ctrl-C ended marks the process interrupted, so the command's next
/// step refuses even when this process's own event has not arrived yet (see
/// the module docs). Nothing on Unix, where the signal handler sets the flag
/// first.
pub(crate) fn note_child_exit(status: &ExitStatus) {
    if exited_by_console_interrupt(status.code()) && console_handler_installed() {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }
}

/// A finished `kubectl` child's result, read by [`note_child_exit`] on its
/// way to the caller: `.output().noted()`, `.wait().noted()`.
pub(crate) trait Noted {
    fn noted(self) -> Self;
}

impl Noted for std::io::Result<Output> {
    fn noted(self) -> Self {
        if let Ok(output) = &self {
            note_child_exit(&output.status);
        }
        self
    }
}

impl Noted for std::io::Result<ExitStatus> {
    fn noted(self) -> Self {
        if let Ok(status) = &self {
            note_child_exit(status);
        }
        self
    }
}

/// The flag is process-wide, and the tests of one binary share a process: a
/// test that set it would stop every test running beside it. So a test marks
/// only its own thread interrupted ([`test_seam::interrupt_this_thread`]).
#[cfg(test)]
pub(crate) mod test_seam {
    use std::cell::Cell;

    thread_local! {
        static INTERRUPTED_HERE: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn this_thread_interrupted() -> bool {
        INTERRUPTED_HERE.with(Cell::get)
    }

    /// This thread reads as interrupted until the guard drops.
    #[must_use = "the thread reads as interrupted only while this is held"]
    pub(crate) struct Interrupted(());

    impl Drop for Interrupted {
        fn drop(&mut self) {
            INTERRUPTED_HERE.with(|c| c.set(false));
        }
    }

    pub(crate) fn interrupt_this_thread() -> Interrupted {
        INTERRUPTED_HERE.with(|c| c.set(true));
        Interrupted(())
    }

    /// A kubeconfig whose one cluster is `127.0.0.1:1`, where nothing
    /// listens: a refusal test whose guard is broken spawns a kubectl that
    /// reaches nothing, and fails on its answer.
    pub(crate) fn unreachable_kubeconfig() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            "apiVersion: v1\nkind: Config\nclusters:\n- name: none\n  cluster:\n    \
             server: https://127.0.0.1:1\ncontexts:\n- name: none\n  context:\n    \
             cluster: none\n    user: none\ncurrent-context: none\nusers:\n- name: none\n  \
             user: {}\n",
        )
        .unwrap();
        file
    }
}

#[cfg(not(test))]
mod test_seam {
    #[inline(always)]
    pub(super) fn this_thread_interrupted() -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// The helper pods this process has applied
// ---------------------------------------------------------------------------

/// Which pod a helper create or apply left under its name, as the apiserver
/// answered it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// No answer to go by: the call is under way, died unanswered (the same
    /// Ctrl-C reaches its kubectl), or was answered with a pod other than the
    /// one this process read just before it. Never deleted: a pod of that
    /// name may be another run's.
    Unconfirmed,
    /// The apiserver answered this process's create with this uid.
    Created(String),
    /// This process applied over the pod with this uid, which was there
    /// before it (a running pod of the same spec).
    Reused(String),
}

#[derive(Clone, Debug)]
struct Tracked {
    origin: Origin,
    /// The kubeconfig the pod was applied through, as bytes: the command's
    /// file is a temporary one its own thread may already have removed by the
    /// time the stop needs it.
    kubeconfig: Arc<[u8]>,
}

#[derive(Debug, Default)]
struct State {
    pods: BTreeMap<(String, String), Tracked>,
    /// Applies begun and not yet answered.
    applying: usize,
    /// Set by the stop; no apply begins after it.
    stopping: bool,
}

/// The helper pods this process has applied and not yet deleted, keyed by
/// `(namespace, name)`. One per process ([`HelperPods::global`]), shared by
/// every `KubectlExec` the command builds and read by the stop.
#[derive(Clone, Debug, Default)]
pub(crate) struct HelperPods(Arc<Mutex<State>>);

/// A create or apply of a helper pod under way: held for exactly as long as
/// its kubectl, which the stop waits out before it acts on the pods, and
/// settled by the apiserver's answer ([`Self::answered`],
/// [`Self::not_created`]). Dropped without either, the pod stays
/// [`Origin::Unconfirmed`].
#[must_use = "the call counts as under way only while this is held"]
pub(crate) struct ApplyInFlight {
    pods: HelperPods,
    key: (String, String),
}

impl ApplyInFlight {
    /// The apiserver answered: record which pod the call left there, BEFORE
    /// the call stops counting as under way, so a stop waiting for it reads
    /// the answer.
    pub(crate) fn answered(self, origin: Origin) {
        if let Some(tracked) = self.pods.lock().pods.get_mut(&self.key) {
            tracked.origin = origin;
        }
    }

    /// The apiserver refused a create because a pod of that name was already
    /// there: this call created nothing, and that pod is not this process's.
    pub(crate) fn not_created(self) {
        self.pods.lock().pods.remove(&self.key);
    }
}

impl Drop for ApplyInFlight {
    fn drop(&mut self) {
        let mut state = self.pods.lock();
        state.applying = state.applying.saturating_sub(1);
    }
}

impl HelperPods {
    /// The process-wide set the stop reads.
    pub(crate) fn global() -> HelperPods {
        static GLOBAL: LazyLock<HelperPods> = LazyLock::new(HelperPods::default);
        GLOBAL.clone()
    }

    /// Record a helper pod about to be created or applied through
    /// `kubeconfig`, [`Origin::Unconfirmed`] until the apiserver answers, and
    /// count the call as under way until the returned guard is settled or
    /// dropped. Refused once the stop has begun: a pod made now would outlive
    /// the command.
    pub(crate) fn begin_apply(
        &self,
        kubeconfig: &Path,
        namespace: &str,
        name: &str,
    ) -> Result<ApplyInFlight> {
        let bytes: Arc<[u8]> = std::fs::read(kubeconfig)
            .map_err(|e| CliError::Other(format!("read kubeconfig {}: {e}", kubeconfig.display())))?
            .into();
        let key = (namespace.to_string(), name.to_string());
        let mut state = self.lock();
        if state.stopping {
            return Err(interrupted_error());
        }
        state.pods.insert(
            key.clone(),
            Tracked {
                origin: Origin::Unconfirmed,
                kubeconfig: bytes,
            },
        );
        state.applying += 1;
        Ok(ApplyInFlight {
            pods: self.clone(),
            key,
        })
    }

    /// Forget a helper pod the command itself has deleted.
    pub(crate) fn deleted(&self, namespace: &str, name: &str) {
        self.lock()
            .pods
            .remove(&(namespace.to_string(), name.to_string()));
    }

    /// Refuse every apply from now on ([`Self::begin_apply`]).
    pub(crate) fn close(&self) {
        self.lock().stopping = true;
    }

    /// Whether the stop has begun ([`Self::close`]).
    pub(crate) fn is_closed(&self) -> bool {
        self.lock().stopping
    }

    fn applies_in_flight(&self) -> usize {
        self.lock().applying
    }

    /// The helper pods live right now, in a stable order.
    fn snapshot(&self) -> Vec<((String, String), Tracked)> {
        self.lock()
            .pods
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn origin_of(&self, namespace: &str, name: &str) -> Option<Origin> {
        self.lock()
            .pods
            .get(&(namespace.to_string(), name.to_string()))
            .map(|t| t.origin.clone())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // Every operation is one step on the state, so a panic while holding
        // the lock cannot leave it half-updated: a poisoned lock is still good.
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

// ---------------------------------------------------------------------------
// What the stop does with each pod (pure)
// ---------------------------------------------------------------------------

/// What the stop does with one helper pod.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CleanupAction {
    /// Delete the pod with this uid, and only that one.
    Delete { uid: String },
    /// Leave it: this process applied over the pod with this uid, which was
    /// there before it.
    NotCreatedHere { uid: String },
    /// Leave it: no answer says this process created it.
    NotConfirmed,
}

/// Decide what the stop does with a helper pod of origin `origin`: delete
/// only a pod the apiserver's answer to this process's create names. Pure.
pub(crate) fn cleanup_action(origin: &Origin) -> CleanupAction {
    match origin {
        Origin::Created(uid) => CleanupAction::Delete { uid: uid.clone() },
        Origin::Reused(uid) => CleanupAction::NotCreatedHere { uid: uid.clone() },
        Origin::Unconfirmed => CleanupAction::NotConfirmed,
    }
}

/// The `DeleteOptions` body of a helper delete: that pod and no other
/// (`preconditions.uid`), and a one-second grace ([`DELETE_GRACE_SECONDS`]).
pub(crate) fn delete_options(uid: &str) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "DeleteOptions",
        "gracePeriodSeconds": DELETE_GRACE_SECONDS,
        "preconditions": { "uid": uid }
    })
}

// ---------------------------------------------------------------------------
// Running kubectl within the stop's bound
// ---------------------------------------------------------------------------

/// What a bounded `kubectl` run gave back.
struct Ran {
    ok: bool,
    stderr: String,
}

/// Run `kubectl` with `args` against the kubeconfig at `kubeconfig`, `stdin`
/// on its standard input, and kill it at `deadline`: `None` when it did not
/// finish in time or could not be started.
fn kubectl_bounded(
    kubectl: &Path,
    kubeconfig: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    deadline: Instant,
) -> Option<Ran> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return None;
    }
    let mut child = Command::new(kubectl)
        .args(args)
        .arg(format!("--request-timeout={}s", left.as_secs().max(1)))
        .env("KUBECONFIG", kubeconfig)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(bytes);
    }
    // Read both pipes on their own threads, so neither can fill and stall it.
    let drain = |r: Option<Box<dyn std::io::Read + Send>>| {
        thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut r) = r {
                let _ = r.read_to_string(&mut text);
            }
            text
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = out.join();
                return Some(Ran {
                    ok: status.success(),
                    stderr: err.join().unwrap_or_default(),
                });
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Clean up one helper pod `ns/name` of origin `origin`: delete it or leave
/// it ([`cleanup_action`]). Returns the line the stop prints for it.
pub(crate) fn clean_one(
    kubectl: &Path,
    kubeconfig: &Path,
    ns: &str,
    name: &str,
    origin: &Origin,
    deadline: Instant,
) -> String {
    let by_hand = format!("delete it with `kubectl delete pod {name} -n {ns}` if nothing uses it");
    match cleanup_action(origin) {
        CleanupAction::NotCreatedHere { .. } => format!(
            "  left helper pod {ns}/{name}: it was there before this command applied it, so \
             another run may be using it (that run deletes it)"
        ),
        CleanupAction::NotConfirmed => format!(
            "  left helper pod {ns}/{name}: the signal cut off this command's create of it \
             before the answer came, so it cannot tell whether it created the pod of that name \
             or another run did; {by_hand}"
        ),
        CleanupAction::Delete { uid } => {
            let body = delete_options(&uid).to_string();
            let path = format!("/api/v1/namespaces/{ns}/pods/{name}");
            match kubectl_bounded(
                kubectl,
                kubeconfig,
                &["delete", "--raw", &path, "-f", "-"],
                Some(body.as_bytes()),
                deadline,
            ) {
                Some(r) if r.ok => format!("  deleted helper pod {ns}/{name}"),
                Some(r) if r.stderr.contains("NotFound") => {
                    format!("  helper pod {ns}/{name} is already gone")
                }
                // The precondition refused it: the pod of that name now is
                // not the one this command created.
                Some(r) if r.stderr.contains("Conflict") => format!(
                    "  left helper pod {ns}/{name}: it is no longer the pod this command created"
                ),
                Some(r) => format!(
                    "  could not delete helper pod {ns}/{name} ({}): {by_hand}",
                    r.stderr.trim()
                ),
                None => format!("  ran out of time deleting helper pod {ns}/{name}: {by_hand}"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

/// Kept by the interruptible command for as long as it runs, declared FIRST
/// in it so that it is dropped LAST, after everything else the command holds
/// (its temporary files) has been dropped. Once the process has been
/// interrupted, its drop marks the command unwound and parks the thread: the
/// stop, on its own thread, finishes and exits the process.
///
/// On Windows the flag can be set with no console event behind it yet — by a
/// kubectl that a console Ctrl-C ended ([`note_child_exit`]). The event is
/// usually on its way and starts the stop under its own name; if none has
/// within [`EVENT_GRACE`], the drop starts the stop as for Ctrl-C, so the
/// thread never parks with nothing to end the process.
#[must_use = "the command is interruptible only while this is held"]
pub(crate) struct StopGuard(());

impl Drop for StopGuard {
    fn drop(&mut self) {
        if interrupted() {
            UNWOUND.store(true, Ordering::SeqCst);
            #[cfg(windows)]
            {
                let until = Instant::now() + EVENT_GRACE;
                while !STOP_REQUESTED.load(Ordering::SeqCst) && Instant::now() < until {
                    thread::sleep(Duration::from_millis(20));
                }
                request_stop(ConsoleEvent::CtrlC.cause());
            }
            loop {
                thread::park();
            }
        }
    }
}

/// Make the running command interruptible (see the module docs): on the
/// first SIGINT or SIGTERM, delete the helper pods it created and exit;
/// `note` is printed after that, for what the command leaves as it is.
///
/// A signal the process was started with IGNORED is left ignored — a command
/// run in the background of a non-interactive shell ignores SIGINT, and
/// installing a handler would let a Ctrl-C meant for the shell's foreground
/// job stop it.
pub(crate) fn install(note: Option<&'static str>) -> StopGuard {
    static INSTALL: Once = Once::new();
    *NOTE.lock().unwrap_or_else(|p| p.into_inner()) = note;
    INSTALL.call_once(|| {
        let signals: Vec<i32> = [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM]
            .into_iter()
            .filter(|&s| !ignored_at_start(s))
            .collect();
        if let Err(e) = register(&signals) {
            let _ = writeln!(
                std::io::stderr(),
                "warning: cannot handle Ctrl-C ({e}); an interrupted run leaves its helper pods \
                 running until their keep-alive ends"
            );
        }
    });
    StopGuard(())
}

/// Whether `signal` is ignored in this process right now — at install time,
/// that is how it was started.
#[cfg(unix)]
fn ignored_at_start(signal: i32) -> bool {
    // SAFETY: sigaction with a null new action only reads the current one
    // into `old`, which is a plain, zero-initialised struct.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(signal, std::ptr::null(), &mut old) == 0
            && old.sa_sigaction == libc::SIG_IGN
    }
}

/// Windows: never — which is not to say a Windows process cannot start with
/// Ctrl-C ignored. It can, through `SetConsoleCtrlHandler(NULL, TRUE)` in a
/// parent or `CREATE_NEW_PROCESS_GROUP`, and it inherits that; but the
/// console then delivers no Ctrl-C to any handler of the process, the one
/// installed here included, so the ignore holds without this command
/// preserving it.
#[cfg(windows)]
fn ignored_at_start(_signal: i32) -> bool {
    false
}

#[cfg(unix)]
fn register(signals: &[i32]) -> std::io::Result<()> {
    for &signal in signals {
        // In this order, because signal-hook runs a signal's actions in the
        // order they were registered: the first signal finds the flag unset
        // and only sets it; a second finds it set, removes the stop's copies
        // of the kubeconfig and ends the process from the handler itself,
        // whatever the stop is doing.
        let flag = Arc::clone(&INTERRUPTED);
        // SAFETY: the action runs inside the signal handler, and does only
        // what is async-signal-safe there: atomic loads, and unlink(2) of
        // paths whose memory is never freed ([`COPY_PATHS`]).
        unsafe {
            signal_hook::low_level::register(signal, move || {
                if flag.load(Ordering::SeqCst) {
                    unlink_all(&COPY_PATHS);
                }
            })?;
        }
        signal_hook::flag::register_conditional_shutdown(
            signal,
            128 + signal,
            Arc::clone(&INTERRUPTED),
        )?;
        signal_hook::flag::register(signal, Arc::clone(&INTERRUPTED))?;
    }
    let mut iterator = signal_hook::iterator::Signals::new(signals)?;
    thread::Builder::new()
        .name("interrupt".into())
        .spawn(move || {
            if let Some(signal) = iterator.forever().next() {
                stop(StopCause::signal(signal));
            }
        })?;
    Ok(())
}

/// Windows: the console events this command takes, each stopping it as
/// SIGINT does on Unix (exit code 130).
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConsoleEvent {
    CtrlC,
    CtrlBreak,
    /// The console window closing (its close button, or Task Manager's End
    /// task on it).
    Close,
}

#[cfg(windows)]
impl ConsoleEvent {
    /// The event `ctrl_type` names, or `None` for one left to the default
    /// handling (logoff and shutdown, which reach services only).
    fn of(ctrl_type: u32) -> Option<ConsoleEvent> {
        use windows_sys::Win32::System::Console::{
            CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
        };
        match ctrl_type {
            CTRL_C_EVENT => Some(ConsoleEvent::CtrlC),
            CTRL_BREAK_EVENT => Some(ConsoleEvent::CtrlBreak),
            CTRL_CLOSE_EVENT => Some(ConsoleEvent::Close),
            _ => None,
        }
    }

    /// The stop this event starts.
    fn cause(self) -> StopCause {
        let (label, bound) = match self {
            ConsoleEvent::CtrlC => ("Ctrl-C", STOP_BOUND),
            ConsoleEvent::CtrlBreak => ("Ctrl-Break", STOP_BOUND),
            ConsoleEvent::Close => ("The console window closed", CLOSE_STOP_BOUND),
        };
        StopCause {
            label,
            code: CONSOLE_EXIT_CODE,
            bound,
        }
    }
}

/// Windows: what every console event exits with, the code SIGINT gives on
/// Unix.
#[cfg(windows)]
const CONSOLE_EXIT_CODE: i32 = 130;

/// Windows: how many console events have arrived. The first starts the
/// stop, any later one ends the process; counted here rather than read off
/// [`INTERRUPTED`], which [`note_child_exit`] may set before any event.
#[cfg(windows)]
static CTRL_EVENTS: AtomicU32 = AtomicU32::new(0);

/// Windows: whether the stop has been asked for ([`request_stop`]).
#[cfg(windows)]
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Windows: the `interrupt` thread's wake-up, set once by [`register`].
#[cfg(windows)]
static STOP_REQUEST: OnceLock<mpsc::SyncSender<StopCause>> = OnceLock::new();

/// Windows: start the stop on the `interrupt` thread, once; a later request
/// does nothing.
#[cfg(windows)]
fn request_stop(cause: StopCause) {
    if !STOP_REQUESTED.swap(true, Ordering::SeqCst) {
        if let Some(wake) = STOP_REQUEST.get() {
            let _ = wake.try_send(cause);
        }
    }
}

/// Whether this process has a console handler installed (Windows; see
/// [`note_child_exit`]).
#[cfg(windows)]
fn console_handler_installed() -> bool {
    STOP_REQUEST.get().is_some()
}

/// Unix has no console handler: the signal handler sets the flag itself.
#[cfg(unix)]
fn console_handler_installed() -> bool {
    false
}

/// Windows: one console control handler for Ctrl-C, Ctrl-Break and the
/// console window closing, in place of the SIGINT/SIGTERM handlers above,
/// and the `interrupt` thread the stop runs on, as on Unix. `signals` names
/// no Windows event, so it is not read.
#[cfg(windows)]
fn register(_signals: &[i32]) -> std::io::Result<()> {
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{FALSE, TRUE};
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    /// Runs on a thread the system creates for each event, so — unlike a
    /// Unix signal handler — it may block and allocate; but it does no more
    /// than the Unix handler does. The first event marks the process
    /// interrupted, wakes the `interrupt` thread and returns at once. A
    /// second one, while the stop is still working, removes the stop's
    /// kubeconfig copies and ends the process from here.
    unsafe extern "system" fn handler(ctrl_type: u32) -> BOOL {
        let Some(event) = ConsoleEvent::of(ctrl_type) else {
            return FALSE;
        };
        if CTRL_EVENTS.fetch_add(1, Ordering::SeqCst) > 0 {
            unlink_all(&COPY_PATHS);
            exit_at_once(CONSOLE_EXIT_CODE);
        }
        INTERRUPTED.store(true, Ordering::SeqCst);
        request_stop(event.cause());
        if event == ConsoleEvent::Close {
            // Returning would let Windows end the process now: wait for the
            // stop to end it, within its four seconds.
            loop {
                thread::park();
            }
        }
        TRUE
    }

    let (wake, woken) = mpsc::sync_channel::<StopCause>(1);
    thread::Builder::new()
        .name("interrupt".into())
        .spawn(move || {
            if let Ok(cause) = woken.recv() {
                stop(cause);
            }
        })?;
    let _ = STOP_REQUEST.set(wake);
    // SAFETY: registers a plain `extern "system"` function for the life of
    // the process; it is never removed.
    if unsafe { SetConsoleCtrlHandler(Some(handler), TRUE) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Windows: end the process at once, running nothing else — no `atexit`
/// handler, no DLL detach — as `_exit` does from the Unix signal handler.
#[cfg(windows)]
fn exit_at_once(code: i32) -> ! {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
    // closing; TerminateProcess on it does not return when it succeeds.
    unsafe {
        TerminateProcess(GetCurrentProcess(), code as u32);
    }
    std::process::exit(code)
}

/// What started the stop: the name it gives it, the code the process exits
/// with, and the most the stop may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StopCause {
    label: &'static str,
    code: i32,
    bound: Duration,
}

impl StopCause {
    /// A Unix signal: 128 + its number, within [`STOP_BOUND`].
    #[cfg(unix)]
    fn signal(signal: i32) -> StopCause {
        let label = match signal {
            signal_hook::consts::SIGINT => "Ctrl-C (SIGINT)",
            signal_hook::consts::SIGTERM => "SIGTERM",
            _ => "a signal",
        };
        StopCause {
            label,
            code: 128 + signal,
            bound: STOP_BOUND,
        }
    }

    /// The first line the stop prints.
    fn announcement(&self) -> String {
        format!(
            "\n{}: deleting the helper pods this command created, then exiting \
             (at most {}s; interrupt again to exit at once).",
            self.label,
            self.bound.as_secs()
        )
    }
}

/// The stop: see the module docs. Ends the process, with `cause.code`,
/// within `cause.bound`.
fn stop(cause: StopCause) -> ! {
    let started = Instant::now();
    let deadline = started + cause.bound;
    let helpers = HelperPods::global();
    let say = |line: &str| {
        // A write to a closed terminal must not panic the stop.
        let _ = writeln!(std::io::stderr(), "{line}");
    };
    say(&cause.announcement());

    // 1. No apply after this; the ones under way are waited out.
    helpers.close();
    let settle = (started + APPLY_SETTLE_BOUND).min(deadline);
    while helpers.applies_in_flight() > 0 && Instant::now() < settle {
        thread::sleep(Duration::from_millis(20));
    }

    // 2. Each pod, through a private copy of the kubeconfig it was applied
    //    with: the command's own file may be gone already.
    let pods = helpers.snapshot();
    if pods.is_empty() {
        say("  no helper pod of this command was running");
    }
    let kubectl = Path::new(crate::commands::backup::KUBECTL_BIN);
    let mut copies: Vec<(Arc<[u8]>, PrivateCopy)> = Vec::new();
    for ((ns, name), tracked) in pods {
        let path = match copies
            .iter()
            .find(|(bytes, _)| **bytes == *tracked.kubeconfig)
        {
            Some((_, file)) => file.path().to_path_buf(),
            None => match private_copy(&tracked.kubeconfig) {
                Ok(file) => {
                    let path = file.path().to_path_buf();
                    copies.push((Arc::clone(&tracked.kubeconfig), file));
                    path
                }
                Err(e) => {
                    say(&format!(
                        "  could not write a kubeconfig to delete helper pod {ns}/{name} ({e}); \
                         delete it with `kubectl delete pod {name} -n {ns}`"
                    ));
                    continue;
                }
            },
        };
        say(&clean_one(
            kubectl,
            &path,
            &ns,
            &name,
            &tracked.origin,
            deadline,
        ));
    }
    drop(copies);
    say("  nothing else was undone");
    if let Some(note) = *NOTE.lock().unwrap_or_else(|p| p.into_inner()) {
        say(note);
    }

    // 3. The command's own thread, a moment to unwind.
    let unwind = (Instant::now() + UNWIND_BOUND).min(deadline);
    while !UNWOUND.load(Ordering::SeqCst) && Instant::now() < unwind {
        thread::sleep(Duration::from_millis(20));
    }
    std::process::exit(cause.code)
}

/// The stop's copies of the kubeconfig, as C paths, for a second signal to
/// remove before it ends the process ([`register`]). That exit is `_exit`,
/// from the signal handler: no destructor runs, so a copy is removed there or
/// not at all, and a handler can do no more than read these and `unlink(2)`
/// each. A slot holds a path while its copy exists ([`PrivateCopy`]); the
/// memory of a path is never freed, because the handler may be reading it at
/// any moment — a few dozen bytes per copy, and the stop makes one copy per
/// kubeconfig, once per process.
#[cfg(unix)]
static COPY_PATHS: [AtomicPtr<libc::c_char>; COPY_SLOTS] =
    [const { AtomicPtr::new(std::ptr::null_mut()) }; COPY_SLOTS];

/// Windows: the stop's kubeconfig copies, for a second Ctrl-C to remove.
/// No async-signal constraint applies (see `register`), so plain paths.
#[cfg(windows)]
static COPY_PATHS: [Mutex<Option<PathBuf>>; COPY_SLOTS] = [const { Mutex::new(None) }; COPY_SLOTS];

/// More than the stop ever needs: a command applies every helper through one
/// kubeconfig.
const COPY_SLOTS: usize = 8;

/// `unlink(2)` every path in `slots`. Async-signal-safe: the second signal
/// runs it from the handler.
#[cfg(unix)]
fn unlink_all(slots: &[AtomicPtr<libc::c_char>]) {
    for slot in slots {
        let path = slot.load(Ordering::SeqCst);
        if !path.is_null() {
            // SAFETY: a non-null slot holds a NUL-terminated path that is
            // never freed (see `COPY_PATHS`).
            unsafe {
                libc::unlink(path);
            }
        }
    }
}

/// Windows: remove every path in `slots` (a second Ctrl-C, see `register`).
/// A kubectl reading a copy may hold it open without sharing its deletion,
/// so each removal is retried a moment ([`remove_retrying`]).
#[cfg(windows)]
fn unlink_all(slots: &[Mutex<Option<PathBuf>>]) {
    for slot in slots {
        if let Some(path) = slot.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            remove_retrying(path, &mut |path| std::fs::remove_file(path));
        }
    }
}

/// `ERROR_SHARING_VIOLATION`: Windows refused to remove a file another
/// process has open without `FILE_SHARE_DELETE`.
#[cfg(any(windows, test))]
const ERROR_SHARING_VIOLATION: i32 = 32;

/// How many times, and how far apart, a removal refused by a sharing
/// violation is tried: at most 200 ms per path.
#[cfg(any(windows, test))]
const SHARING_RETRIES: usize = 10;
#[cfg(any(windows, test))]
const SHARING_PAUSE: Duration = Duration::from_millis(20);

/// Remove `path` with `remove`, trying again while it fails with a sharing
/// violation, up to [`SHARING_RETRIES`] tries in all; any other outcome ends
/// it. Best-effort: the result is not returned, the exit follows anyway.
#[cfg(any(windows, test))]
fn remove_retrying(path: &Path, remove: &mut dyn FnMut(&Path) -> std::io::Result<()>) {
    for attempt in 1..=SHARING_RETRIES {
        match remove(path) {
            Err(e)
                if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
                    && attempt < SHARING_RETRIES =>
            {
                thread::sleep(SHARING_PAUSE);
            }
            _ => return,
        }
    }
}

/// Put `path` in a free slot of `slots` and return its index, or fail when
/// none is free. The path's memory is leaked on purpose (see `COPY_PATHS`).
#[cfg(unix)]
fn claim_slot(slots: &[AtomicPtr<libc::c_char>], path: &Path) -> std::io::Result<usize> {
    use std::os::unix::ffi::OsStrExt as _;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())?.into_raw();
    for (index, slot) in slots.iter().enumerate() {
        if slot
            .compare_exchange(
                std::ptr::null_mut(),
                c_path,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            return Ok(index);
        }
    }
    // SAFETY: from `into_raw` above and never shared: no slot took it.
    drop(unsafe { std::ffi::CString::from_raw(c_path) });
    Err(std::io::Error::other(
        "no room to record it for a second Ctrl-C to remove",
    ))
}

/// Windows: put `path` in a free slot of `slots` and return its index, or
/// fail when none is free.
#[cfg(windows)]
fn claim_slot(slots: &[Mutex<Option<PathBuf>>], path: &Path) -> std::io::Result<usize> {
    for (index, slot) in slots.iter().enumerate() {
        let mut held = slot.lock().unwrap_or_else(|p| p.into_inner());
        if held.is_none() {
            *held = Some(path.to_path_buf());
            return Ok(index);
        }
    }
    Err(std::io::Error::other(
        "no room to record it for a second Ctrl-C to remove",
    ))
}

/// Free slot `index` of [`COPY_PATHS`]: its copy is gone.
#[cfg(unix)]
fn release_slot(index: usize) {
    COPY_PATHS[index].store(std::ptr::null_mut(), Ordering::SeqCst);
}

/// Free slot `index` of [`COPY_PATHS`]: its copy is gone.
#[cfg(windows)]
fn release_slot(index: usize) {
    *COPY_PATHS[index].lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// A copy of the kubeconfig the stop runs its kubectl through: a file only
/// this user can read, recorded in [`COPY_PATHS`] before the kubeconfig is
/// written into it, removed when dropped, and removed by a second signal.
struct PrivateCopy {
    file: Option<tempfile::NamedTempFile>,
    slot: usize,
}

impl PrivateCopy {
    fn path(&self) -> &Path {
        self.file.as_ref().expect("present until dropped").path()
    }
}

impl Drop for PrivateCopy {
    fn drop(&mut self) {
        // The file first, then the slot: a second signal in between only
        // unlinks a path that is gone.
        if let Some(file) = self.file.take() {
            let _ = file.close();
        }
        release_slot(self.slot);
    }
}

/// The kubeconfig `bytes` in a [`PrivateCopy`].
fn private_copy(bytes: &[u8]) -> std::io::Result<PrivateCopy> {
    let file = tempfile::Builder::new()
        .prefix("apprafter-interrupt-")
        .tempfile()?;
    // No slot, no copy: the kubeconfig is never written where a second
    // signal could not remove it.
    let slot = claim_slot(&COPY_PATHS, file.path())?;
    let mut copy = PrivateCopy {
        file: Some(file),
        slot,
    };
    let file = copy.file.as_mut().expect("just set");
    file.write_all(bytes)?;
    file.flush()?;
    Ok(copy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kubeconfig() -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "apiVersion: v1\nkind: Config\n").unwrap();
        f
    }

    /// Only a pod the apiserver's answer to this process's create names is
    /// deleted — that pod, by uid. One applied over is left for the run using
    /// it, and one no answer settled is left too: a pod of that name may be
    /// another run's.
    #[test]
    fn only_a_pod_this_process_created_is_deleted() {
        assert_eq!(
            cleanup_action(&Origin::Created("u-new".into())),
            CleanupAction::Delete {
                uid: "u-new".into()
            }
        );
        assert_eq!(
            cleanup_action(&Origin::Reused("u-old".into())),
            CleanupAction::NotCreatedHere {
                uid: "u-old".into()
            }
        );
        assert_eq!(
            cleanup_action(&Origin::Unconfirmed),
            CleanupAction::NotConfirmed
        );
    }

    /// A call is unconfirmed until its answer; the answer is recorded before
    /// the call stops counting as under way, so a stop that waited for it
    /// reads it; a create refused because the pod was there drops the record.
    #[test]
    fn the_apiservers_answer_settles_the_record_before_the_call_ends() {
        let kc = kubeconfig();
        let pods = HelperPods::default();

        let created = pods.begin_apply(kc.path(), "demo", "bk-pg-db").unwrap();
        assert_eq!(
            pods.origin_of("demo", "bk-pg-db"),
            Some(Origin::Unconfirmed)
        );
        assert_eq!(pods.applies_in_flight(), 1);
        created.answered(Origin::Created("u-1".into()));
        assert_eq!(pods.applies_in_flight(), 0);
        assert_eq!(
            pods.origin_of("demo", "bk-pg-db"),
            Some(Origin::Created("u-1".into()))
        );

        // Cut off: dropped unanswered, it stays unconfirmed.
        drop(pods.begin_apply(kc.path(), "demo", "bk-vol-v").unwrap());
        assert_eq!(
            pods.origin_of("demo", "bk-vol-v"),
            Some(Origin::Unconfirmed)
        );
        assert_eq!(pods.applies_in_flight(), 0);

        // Refused as AlreadyExists: nothing of this process's is there.
        pods.begin_apply(kc.path(), "demo", "bk-vol-v")
            .unwrap()
            .not_created();
        assert_eq!(pods.origin_of("demo", "bk-vol-v"), None);
        assert_eq!(pods.applies_in_flight(), 0);

        pods.deleted("demo", "bk-pg-db");
        assert_eq!(pods.origin_of("demo", "bk-pg-db"), None);
    }

    /// Once the stop has begun, no helper is applied — and every apply under
    /// way is counted until its guard drops, for the stop to wait out.
    #[test]
    fn no_apply_begins_after_the_stop_and_those_under_way_are_counted() {
        let kc = kubeconfig();
        let pods = HelperPods::default();
        let a = pods.begin_apply(kc.path(), "demo", "a").unwrap();
        let b = pods.begin_apply(kc.path(), "demo", "b").unwrap();
        assert_eq!(pods.applies_in_flight(), 2);
        pods.close();
        let refused = pods
            .begin_apply(kc.path(), "demo", "c")
            .err()
            .expect("an apply after the stop began");
        assert!(refused.to_string().starts_with("interrupted"), "{refused}");
        drop(a);
        assert_eq!(pods.applies_in_flight(), 1);
        drop(b);
        assert_eq!(pods.applies_in_flight(), 0);
        let names: Vec<String> = pods.snapshot().into_iter().map(|((_, n), _)| n).collect();
        assert_eq!(names, vec!["a", "b"], "the refused one is not recorded");
    }

    /// The stop reads the kubeconfig from the bytes it recorded, not the
    /// command's file, which may be gone by then.
    #[test]
    fn the_kubeconfig_is_kept_as_it_was_when_the_pod_was_applied() {
        let kc = kubeconfig();
        let pods = HelperPods::default();
        drop(pods.begin_apply(kc.path(), "demo", "a").unwrap());
        let path = kc.path().to_path_buf();
        drop(kc);
        assert!(!path.exists());
        let (_, tracked) = pods.snapshot().pop().unwrap();
        assert_eq!(&*tracked.kubeconfig, b"apiVersion: v1\nkind: Config\n");
        let copy = private_copy(&tracked.kubeconfig).unwrap();
        assert_eq!(
            std::fs::read(copy.path()).unwrap(),
            b"apiVersion: v1\nkind: Config\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(copy.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "readable by its owner only: {mode:o}");
        }
    }

    #[test]
    fn the_delete_names_the_pod_by_uid_with_a_one_second_grace() {
        assert_eq!(
            delete_options("u-1"),
            serde_json::json!({
                "apiVersion": "v1", "kind": "DeleteOptions", "gracePeriodSeconds": 1,
                "preconditions": {"uid": "u-1"}
            })
        );
    }

    /// A stub `kubectl` that logs its argv (and stdin, for a delete) and
    /// answers `delete` with `delete_answer` (a shell snippet), anything else
    /// with nothing.
    #[cfg(unix)]
    fn stub(dir: &tempfile::TempDir, delete_answer: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.path().join("kubectl-stub");
        let log = dir.path().join("log");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\ncase \"$1\" in __probe) exit 0;; esac\n\
                 echo \"$@\" >> {log}\n\
                 case \"$1\" in\n\
                 delete) cat >> {log}; echo >> {log}; {delete_answer};;\n\
                 esac",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Wait out ETXTBSY from a sibling test thread's fork (see backup.rs's
        // `stub_kubectl`).
        for _ in 0..200 {
            match Command::new(&path).arg("__probe").status() {
                Err(e) if e.raw_os_error() == Some(26) => thread::sleep(Duration::from_millis(5)),
                _ => break,
            }
        }
        path
    }

    #[cfg(unix)]
    fn log(dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(dir.path().join("log")).unwrap_or_default()
    }

    #[cfg(unix)]
    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    #[cfg(unix)]
    #[test]
    fn a_created_pod_is_deleted_by_uid_without_being_read() {
        let dir = tempfile::tempdir().unwrap();
        let kubectl = stub(&dir, "exit 0");
        let kc = kubeconfig();
        let line = clean_one(
            &kubectl,
            kc.path(),
            "demo",
            "bk-pg-db",
            &Origin::Created("u-1".into()),
            soon(),
        );
        assert_eq!(line, "  deleted helper pod demo/bk-pg-db");
        let log = log(&dir);
        assert!(!log.contains("get pod"), "{log}");
        assert!(
            log.starts_with(
                "delete --raw /api/v1/namespaces/demo/pods/bk-pg-db -f - --request-timeout="
            ),
            "{log}"
        );
        let body: serde_json::Value = serde_json::from_str(log.lines().nth(1).unwrap()).unwrap();
        assert_eq!(body, delete_options("u-1"));
    }

    /// A pod applied over, and one no answer settled, are left without a
    /// kubectl run at all: nothing read after the fact makes a pod this
    /// process's. The unsettled one is named with the command that deletes
    /// it.
    #[cfg(unix)]
    #[test]
    fn a_pod_not_confirmed_as_created_here_is_left_unread() {
        let kc = kubeconfig();
        let dir = tempfile::tempdir().unwrap();
        let kubectl = stub(&dir, "exit 0");
        let line = clean_one(
            &kubectl,
            kc.path(),
            "nats",
            "rs-js-x",
            &Origin::Reused("u-old".into()),
            soon(),
        );
        assert!(
            line.starts_with("  left helper pod nats/rs-js-x: it was there before"),
            "{line}"
        );
        let line = clean_one(
            &kubectl,
            kc.path(),
            "demo",
            "ld-pg-db",
            &Origin::Unconfirmed,
            soon(),
        );
        assert!(
            line.starts_with("  left helper pod demo/ld-pg-db: the signal cut off"),
            "{line}"
        );
        assert!(
            line.contains("kubectl delete pod ld-pg-db -n demo"),
            "{line}"
        );
        assert_eq!(log(&dir), "", "no kubectl was run");
    }

    /// The apiserver's answers to a delete by uid, as `kubectl delete --raw`
    /// prints them (Kubernetes 1.36): a pod of that name that is another one
    /// now, and none at all.
    #[cfg(unix)]
    #[test]
    fn a_delete_refused_by_its_precondition_or_of_a_gone_pod_is_reported_as_such() {
        let kc = kubeconfig();
        let dir = tempfile::tempdir().unwrap();
        let kubectl = stub(
            &dir,
            "echo 'Error from server (Conflict): Operation cannot be fulfilled on Pod \"bk-pg-db\": \
             the UID in the precondition (u-1) does not match the UID in record (u-2).' >&2; exit 1",
        );
        let line = clean_one(
            &kubectl,
            kc.path(),
            "demo",
            "bk-pg-db",
            &Origin::Created("u-1".into()),
            soon(),
        );
        assert_eq!(
            line,
            "  left helper pod demo/bk-pg-db: it is no longer the pod this command created"
        );

        let dir = tempfile::tempdir().unwrap();
        let kubectl = stub(
            &dir,
            "echo 'Error from server (NotFound): pods \"bk-pg-db\" not found' >&2; exit 1",
        );
        let line = clean_one(
            &kubectl,
            kc.path(),
            "demo",
            "bk-pg-db",
            &Origin::Created("u-1".into()),
            soon(),
        );
        assert_eq!(line, "  helper pod demo/bk-pg-db is already gone");

        let dir = tempfile::tempdir().unwrap();
        let kubectl = stub(&dir, "echo 'Unable to connect to the server' >&2; exit 1");
        let line = clean_one(
            &kubectl,
            kc.path(),
            "demo",
            "bk-pg-db",
            &Origin::Created("u-1".into()),
            soon(),
        );
        assert!(
            line.contains("could not delete helper pod demo/bk-pg-db (Unable to connect"),
            "{line}"
        );
        assert!(
            line.contains("kubectl delete pod bk-pg-db -n demo"),
            "{line}"
        );
    }

    /// Bounded: a kubectl that does not answer is killed at the deadline, and
    /// the pod is named for deleting by hand.
    #[cfg(unix)]
    #[test]
    fn a_kubectl_that_hangs_is_killed_at_the_deadline() {
        let kc = kubeconfig();
        let dir = tempfile::tempdir().unwrap();
        let kubectl = stub(&dir, "exec sleep 30");
        let started = Instant::now();
        let line = clean_one(
            &kubectl,
            kc.path(),
            "demo",
            "bk-pg-db",
            &Origin::Created("u-1".into()),
            Instant::now() + Duration::from_millis(500),
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(
            line.starts_with("  ran out of time deleting helper pod demo/bk-pg-db"),
            "{line}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_signal_the_process_ignores_is_seen_as_ignored() {
        // SIGURG's default action is to ignore it, which is not SIG_IGN; set
        // SIG_IGN on it explicitly to see the check tell them apart.
        let sig = libc::SIGURG;
        assert!(!ignored_at_start(sig));
        // SAFETY: SIG_IGN for SIGURG, which nothing in the test binary uses,
        // restored straight after.
        unsafe {
            let prev = libc::signal(sig, libc::SIG_IGN);
            assert!(ignored_at_start(sig));
            libc::signal(sig, prev);
        }
        assert!(!ignored_at_start(sig));
    }

    // ------------------------------------------------------------------
    // A second signal removes the stop's kubeconfig copies
    // ------------------------------------------------------------------

    #[cfg(unix)]
    fn recorded(slots: &[AtomicPtr<libc::c_char>], path: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt as _;
        slots.iter().any(|slot| {
            let p = slot.load(Ordering::SeqCst);
            // SAFETY: a non-null slot holds a NUL-terminated path never freed.
            !p.is_null()
                && unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes() == path.as_os_str().as_bytes()
        })
    }

    /// Windows: the slots hold plain paths (see `COPY_PATHS`).
    #[cfg(windows)]
    fn recorded(slots: &[Mutex<Option<PathBuf>>], path: &Path) -> bool {
        slots
            .iter()
            .any(|slot| slot.lock().unwrap_or_else(|p| p.into_inner()).as_deref() == Some(path))
    }

    #[cfg(unix)]
    #[test]
    fn what_the_second_signal_runs_removes_every_recorded_path() {
        let slots: [AtomicPtr<libc::c_char>; 2] =
            [const { AtomicPtr::new(std::ptr::null_mut()) }; 2];
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        for p in [&a, &b, &c] {
            std::fs::write(p, "kubeconfig").unwrap();
        }
        claim_slot(&slots, &a).unwrap();
        claim_slot(&slots, &b).unwrap();
        // Full: refused, so no copy is ever written unrecorded.
        assert!(claim_slot(&slots, &c).is_err());
        unlink_all(&slots);
        assert!(!a.exists() && !b.exists());
        assert!(c.exists(), "only what was recorded");
    }

    /// Windows: the same, with the slots a second Ctrl-C reads.
    #[cfg(windows)]
    #[test]
    fn what_the_second_signal_runs_removes_every_recorded_path() {
        let slots: [Mutex<Option<PathBuf>>; 2] = [const { Mutex::new(None) }; 2];
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        for p in [&a, &b, &c] {
            std::fs::write(p, "kubeconfig").unwrap();
        }
        claim_slot(&slots, &a).unwrap();
        claim_slot(&slots, &b).unwrap();
        // Full: refused, so no copy is ever written unrecorded.
        assert!(claim_slot(&slots, &c).is_err());
        unlink_all(&slots);
        assert!(!a.exists() && !b.exists());
        assert!(c.exists(), "only what was recorded");
    }

    /// Windows: a sharing violation — a kubectl holding the copy open — is
    /// waited out a moment; anything else, success included, ends the tries.
    #[test]
    fn a_removal_is_retried_through_a_sharing_violation_only() {
        let sharing = || std::io::Error::from_raw_os_error(ERROR_SHARING_VIOLATION);
        let path = Path::new("copy");

        let mut calls = 0;
        remove_retrying(path, &mut |_| {
            calls += 1;
            if calls < 3 {
                Err(sharing())
            } else {
                Ok(())
            }
        });
        assert_eq!(calls, 3, "tried until it went through");

        let mut calls = 0;
        remove_retrying(path, &mut |_| {
            calls += 1;
            Err(sharing())
        });
        assert_eq!(calls, SHARING_RETRIES, "and no more than the bound");

        let mut calls = 0;
        remove_retrying(path, &mut |_| {
            calls += 1;
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        });
        assert_eq!(calls, 1, "any other error is final");
    }

    /// Only `STATUS_CONTROL_C_EXIT` reads as a console interrupt; no Unix
    /// exit code, and no ordinary failure, does.
    #[test]
    fn only_status_control_c_exit_is_a_console_interrupt() {
        assert!(exited_by_console_interrupt(Some(0xC000_013A_u32 as i32)));
        for code in [None, Some(0), Some(1), Some(130), Some(143), Some(-1)] {
            assert!(!exited_by_console_interrupt(code), "{code:?}");
        }
    }

    /// Unix keeps the stop's exact first line, code and bound.
    #[cfg(unix)]
    #[test]
    fn the_stop_names_a_unix_signal_as_it_always_has() {
        let int = StopCause::signal(libc::SIGINT);
        assert_eq!(
            int.announcement(),
            "\nCtrl-C (SIGINT): deleting the helper pods this command created, then exiting \
             (at most 15s; interrupt again to exit at once)."
        );
        assert_eq!((int.code, int.bound), (130, STOP_BOUND));
        let term = StopCause::signal(libc::SIGTERM);
        assert_eq!((term.label, term.code), ("SIGTERM", 143));
        assert_eq!(StopCause::signal(libc::SIGHUP).label, "a signal");
    }

    /// Windows: each event the handler takes stops as SIGINT does, the
    /// window closing within the four seconds Windows leaves it; logoff and
    /// shutdown are left to the default handling.
    #[cfg(windows)]
    #[test]
    fn each_console_event_names_its_stop() {
        use windows_sys::Win32::System::Console::{
            CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
            CTRL_SHUTDOWN_EVENT,
        };
        let cause = |ctrl| ConsoleEvent::of(ctrl).map(ConsoleEvent::cause);
        let c = cause(CTRL_C_EVENT).unwrap();
        assert_eq!((c.label, c.code, c.bound), ("Ctrl-C", 130, STOP_BOUND));
        let brk = cause(CTRL_BREAK_EVENT).unwrap();
        assert_eq!(
            (brk.label, brk.code, brk.bound),
            ("Ctrl-Break", 130, STOP_BOUND)
        );
        let close = cause(CTRL_CLOSE_EVENT).unwrap();
        assert_eq!(close.code, 130);
        assert_eq!(close.bound, Duration::from_secs(4));
        assert!(
            close.announcement().contains("(at most 4s;"),
            "{}",
            close.announcement()
        );
        assert_eq!(cause(CTRL_LOGOFF_EVENT), None);
        assert_eq!(cause(CTRL_SHUTDOWN_EVENT), None);
    }

    /// Each copy the stop writes is recorded before the kubeconfig goes in,
    /// for as long as it exists, and no longer.
    #[test]
    fn a_copy_is_recorded_for_exactly_as_long_as_it_exists() {
        let copy = private_copy(b"apiVersion: v1\n").unwrap();
        let path = copy.path().to_path_buf();
        assert!(path.exists());
        assert!(recorded(&COPY_PATHS, &path));
        drop(copy);
        assert!(!path.exists());
        assert!(!recorded(&COPY_PATHS, &path));
    }

    /// The whole of it, in a process of its own: the stop is deleting a
    /// helper pod through its private copy of the kubeconfig — its kubectl
    /// hangs — when a second SIGTERM comes. The process exits at once with
    /// 143 and leaves no copy behind; before, `_exit` from the handler left
    /// the decrypted kubeconfig in `$TMPDIR`.
    #[cfg(unix)]
    #[test]
    fn a_second_signal_exits_at_once_and_leaves_no_kubeconfig_copy() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        // The stop's kubectl, found on the child's PATH: `delete` hangs.
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let stub_dir = tempfile::tempdir().unwrap();
        let stub_path = stub(&stub_dir, "exec sleep 10");
        std::fs::copy(&stub_path, bin.join("kubectl")).unwrap();
        wait_until_executable(&bin.join("kubectl"));
        let kc = dir.path().join("kubeconfig");
        std::fs::write(&kc, "apiVersion: v1\nkind: Config\n").unwrap();

        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "commands::helper_interrupt::tests::second_signal_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("APPRAFTER_INTERRUPT_CHILD_KUBECONFIG", &kc)
            .env("TMPDIR", tmp.path())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id() as libc::pid_t;
        let copies = || -> Vec<String> {
            std::fs::read_dir(tmp.path())
                .unwrap()
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter(|n| n.starts_with("apprafter-interrupt-"))
                .collect()
        };
        let within = |what: &str, bound: Duration, done: &mut dyn FnMut() -> bool| {
            let until = Instant::now() + bound;
            while !done() {
                if Instant::now() > until {
                    // SAFETY: our own child.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                    panic!("{what} within {bound:?}");
                }
                thread::sleep(Duration::from_millis(20));
            }
        };
        // Ready once the child has recorded its helper pod.
        let ready = dir.path().join("ready");
        within("the child is ready", Duration::from_secs(30), &mut || {
            ready.exists()
        });

        // SAFETY: our own child.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        within(
            "the stop writes its kubeconfig copy",
            Duration::from_secs(10),
            &mut || !copies().is_empty(),
        );
        // SAFETY: our own child.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let started = Instant::now();
        let mut status = None;
        within(
            "the second signal ends it",
            Duration::from_secs(5),
            &mut || {
                status = child.try_wait().unwrap();
                status.is_some()
            },
        );
        let mut stderr = String::new();
        std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
        assert_eq!(status.unwrap().code(), Some(143), "{stderr}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "at once: {stderr}"
        );
        assert!(
            stderr.contains("SIGTERM: deleting the helper pods this command created"),
            "the first signal's stop had begun: {stderr}"
        );
        assert_eq!(copies(), Vec::<String>::new(), "{stderr}");
        // The stop was in its delete, through the copy, when the second
        // signal came.
        let log = log(&stub_dir);
        assert!(
            log.starts_with("delete --raw /api/v1/namespaces/demo/pods/bk-pg-db -f -"),
            "{log}"
        );
    }

    /// Wait out `ETXTBSY` on a freshly written executable (see `stub`).
    #[cfg(unix)]
    fn wait_until_executable(path: &Path) {
        for _ in 0..200 {
            match Command::new(path).arg("__probe").status() {
                Err(e) if e.raw_os_error() == Some(26) => thread::sleep(Duration::from_millis(5)),
                _ => break,
            }
        }
    }

    /// The child process of the test above, and nothing else: it installs
    /// the interrupt, records one helper pod this process created, says it is
    /// ready and waits for the signals. Run on its own it fails, loudly.
    #[cfg(unix)]
    #[test]
    #[ignore = "the child process of a_second_signal_exits_at_once_and_leaves_no_kubeconfig_copy"]
    fn second_signal_child() {
        let kc = std::env::var("APPRAFTER_INTERRUPT_CHILD_KUBECONFIG")
            .expect("run only by a_second_signal_exits_at_once_and_leaves_no_kubeconfig_copy");
        let kc = Path::new(&kc);
        let _guard = install(None);
        HelperPods::global()
            .begin_apply(kc, "demo", "bk-pg-db")
            .unwrap()
            .answered(Origin::Created("u-1".into()));
        std::fs::write(kc.with_file_name("ready"), "").unwrap();
        loop {
            thread::park();
        }
    }

    /// Windows: run this test binary's ignored test `name` as a child in a
    /// process group of its own — so a Ctrl-Break sent to it reaches no other
    /// process — with `TMP`/`TEMP` at `tmp` and `env` set, its stderr piped.
    #[cfg(windows)]
    fn console_child(name: &str, tmp: &Path, env: &[(&str, &Path)]) -> std::process::Child {
        use std::os::windows::process::CommandExt as _;
        use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                &format!("commands::helper_interrupt::tests::{name}"),
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TMP", tmp)
            .env("TEMP", tmp)
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        command.spawn().unwrap()
    }

    /// Windows: the child's stderr, line by line as it comes, and all of it
    /// once the child has exited.
    #[cfg(windows)]
    fn stderr_lines(
        child: &mut std::process::Child,
    ) -> (mpsc::Receiver<String>, thread::JoinHandle<String>) {
        use std::io::BufRead as _;
        let (lines, received) = mpsc::channel();
        let stderr = child.stderr.take().unwrap();
        let all = thread::spawn(move || {
            let mut all = String::new();
            for line in std::io::BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                all.push_str(&line);
                all.push('\n');
                let _ = lines.send(line);
            }
            all
        });
        (received, all)
    }

    /// Windows: wait for `child` to exit, at most `bound`; killed past it.
    #[cfg(windows)]
    fn exit_within(child: &mut std::process::Child, bound: Duration) -> Option<ExitStatus> {
        let until = Instant::now() + bound;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return Some(status);
            }
            if Instant::now() > until {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Windows twin of `a_second_signal_exits_at_once_and_leaves_no_kubeconfig_copy`,
    /// in a process of its own: the stop is under way — waiting out a helper
    /// apply that was never answered — with a kubeconfig copy recorded, when
    /// a second Ctrl-Break comes. The process exits at once with 130 and
    /// leaves no copy behind.
    ///
    /// No kubectl stand-in: on Windows `Command` starts only an `.exe`, so a
    /// script cannot stand in for one, and the copy is recorded by the child
    /// itself, as the stop records its own ([`private_copy`]). Without the
    /// second event the stop would run on for seconds and leave the copy.
    #[cfg(windows)]
    #[test]
    fn a_second_console_event_exits_at_once_and_leaves_no_kubeconfig_copy() {
        use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
        let dir = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let kc = dir.path().join("kubeconfig");
        std::fs::write(&kc, "apiVersion: v1\nkind: Config\n").unwrap();
        let mut child = console_child(
            "second_console_event_child",
            tmp.path(),
            &[("APPRAFTER_INTERRUPT_CHILD_KUBECONFIG", &kc)],
        );
        let (lines, all) = stderr_lines(&mut child);
        let copies = || -> Vec<String> {
            std::fs::read_dir(tmp.path())
                .unwrap()
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter(|n| n.starts_with("apprafter-interrupt-"))
                .collect()
        };
        let ctrl_break = |child: &std::process::Child| {
            // SAFETY: a console event to the child's own process group.
            let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) };
            assert_ne!(
                sent,
                0,
                "GenerateConsoleCtrlEvent: {}",
                std::io::Error::last_os_error()
            );
        };

        let ready = dir.path().join("ready");
        let until = Instant::now() + Duration::from_secs(30);
        while !ready.exists() {
            assert!(Instant::now() < until, "the child is not ready within 30s");
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(copies().len(), 1, "the child recorded its copy");

        ctrl_break(&child);
        let begun = "Ctrl-Break: deleting the helper pods this command created";
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match lines.recv_timeout(left) {
                Ok(line) if line.contains(begun) => break,
                Ok(_) => {}
                Err(_) => {
                    let _ = child.kill();
                    panic!(
                        "the first Ctrl-Break started no stop: {}",
                        all.join().unwrap()
                    );
                }
            }
        }
        ctrl_break(&child);
        let sent = Instant::now();
        let status = exit_within(&mut child, Duration::from_secs(5));
        let elapsed = sent.elapsed();
        let stderr = all.join().unwrap();
        let status =
            status.unwrap_or_else(|| panic!("the second Ctrl-Break ended nothing: {stderr}"));
        assert_eq!(status.code(), Some(130), "{stderr}");
        assert!(
            elapsed < Duration::from_secs(2),
            "at once ({elapsed:?}): {stderr}"
        );
        assert!(
            !stderr.contains("nothing else was undone"),
            "the stop was still waiting when the second event came: {stderr}"
        );
        assert_eq!(copies(), Vec::<String>::new(), "{stderr}");
    }

    /// The child process of the test above, and nothing else: it installs
    /// the interrupt, begins a helper apply it never settles (the stop waits
    /// for it), records a kubeconfig copy, says it is ready and waits for the
    /// events. Run on its own it fails, loudly.
    #[cfg(windows)]
    #[test]
    #[ignore = "the child process of a_second_console_event_exits_at_once_and_leaves_no_kubeconfig_copy"]
    fn second_console_event_child() {
        let kc = std::env::var("APPRAFTER_INTERRUPT_CHILD_KUBECONFIG").expect(
            "run only by a_second_console_event_exits_at_once_and_leaves_no_kubeconfig_copy",
        );
        let kc = Path::new(&kc);
        let _guard = install(None);
        let apply = HelperPods::global()
            .begin_apply(kc, "demo", "bk-pg-db")
            .unwrap();
        let copy = private_copy(b"apiVersion: v1\nkind: Config\n").unwrap();
        std::fs::write(kc.with_file_name("ready"), "").unwrap();
        let _held = (apply, copy);
        loop {
            thread::park();
        }
    }

    /// Windows, in a process of its own: a kubectl that a console Ctrl-C
    /// ended marks the process interrupted — the command's next step refuses
    /// — and, with no console event to follow, the command's own thread,
    /// once unwound, starts the stop itself instead of parking forever.
    #[cfg(windows)]
    #[test]
    fn a_kubectl_ended_by_a_console_interrupt_stops_the_command_without_an_event() {
        let tmp = tempfile::tempdir().unwrap();
        let mut child = console_child("console_interrupted_kubectl_child", tmp.path(), &[]);
        let (_, all) = stderr_lines(&mut child);
        let status = exit_within(&mut child, Duration::from_secs(30));
        let stderr = all.join().unwrap();
        let status = status.unwrap_or_else(|| panic!("the stop never ended the child: {stderr}"));
        assert!(
            stderr.contains("child: refused after the kubectl's exit"),
            "{stderr}"
        );
        assert!(
            stderr.contains("Ctrl-C: deleting the helper pods this command created"),
            "{stderr}"
        );
        assert!(
            stderr.contains("no helper pod of this command was running"),
            "{stderr}"
        );
        assert_eq!(status.code(), Some(130), "{stderr}");
    }

    /// The child process of the test above, and nothing else.
    #[cfg(windows)]
    #[test]
    #[ignore = "the child process of a_kubectl_ended_by_a_console_interrupt_stops_the_command_without_an_event"]
    fn console_interrupted_kubectl_child() {
        use std::os::windows::process::ExitStatusExt as _;
        let guard = install(None);
        assert!(!interrupted(), "interrupted before anything happened");
        note_child_exit(&ExitStatus::from_raw(STATUS_CONTROL_C_EXIT as u32));
        assert!(refuse_if_interrupted().is_err(), "the next step ran");
        eprintln!("child: refused after the kubectl's exit");
        drop(guard);
        unreachable!("the stop ends the process");
    }
}
