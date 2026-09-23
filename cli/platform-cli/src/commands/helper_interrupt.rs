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
//! from the signal handler itself, with the same code.
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
//! instead of replacing it. And the terminal sends Ctrl-C to every process in
//! the foreground group — the `kubectl` this process is running included — so
//! the apply under way may have died before or after it reached the
//! apiserver. So each helper apply is recorded before it is sent, with the
//! uid of the pod of that name seen just before, if any ([`Origin`]), and the
//! first read of the pod after it settles which pod the apply left there. At
//! stop time ([`cleanup_action`]):
//!
//! * a pod this apply CREATED — its uid differs from the one before — is
//!   deleted with that uid as the delete's precondition, so a pod of the same
//!   name created in the meantime by someone else is never the one deleted;
//! * a pod that was there BEFORE the apply is left alone: another run may be
//!   using it, and that run deletes it;
//! * a pod whose apply was never confirmed is read again, and judged the same
//!   way against the uid from before the apply.
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
//! The in-cluster runner has its own, different stop (`apprafter-backup`'s
//! `stop` module): it is PID 1 of a Job's pod, is stopped by SIGTERM at its
//! deadline, and records the run's failure as well.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Once};
use std::thread;
use std::time::{Duration, Instant};

use cli_core::{CliError, Result};

/// The most the whole stop may take, from the signal to the exit.
pub const STOP_BOUND: Duration = Duration::from_secs(15);

/// The most the stop waits for a helper apply under way to be answered before
/// it reads the pods. `kubectl apply` answers in well under a second, or dies
/// with the terminal's Ctrl-C at once.
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
/// can see a `kubectl` child that died of the same Ctrl-C.
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

/// Which pod a helper apply left under its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// Not known yet: the apply is under way, failed, or its pod has not been
    /// read since. `before` is the uid of the pod of that name just before the
    /// apply was sent, `None` when there was none.
    Unconfirmed { before: Option<String> },
    /// This process's apply created the pod with this uid.
    Created(String),
    /// The pod with this uid was there before this process applied it (the
    /// apply went over a running pod of the same spec).
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

/// An apply of a helper pod under way: held for exactly as long as its
/// `kubectl apply`, which the stop waits out before it reads the pods.
#[must_use = "the apply counts as under way only while this is held"]
pub(crate) struct ApplyInFlight(HelperPods);

impl Drop for ApplyInFlight {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.applying = state.applying.saturating_sub(1);
    }
}

impl HelperPods {
    /// The process-wide set the stop reads.
    pub(crate) fn global() -> HelperPods {
        static GLOBAL: LazyLock<HelperPods> = LazyLock::new(HelperPods::default);
        GLOBAL.clone()
    }

    /// Record a helper pod about to be applied through `kubeconfig`, with the
    /// uid of the pod of that name seen just before (`before`), and count the
    /// apply as under way until the returned guard drops. Refused once the
    /// stop has begun: a pod applied now would outlive the command.
    pub(crate) fn begin_apply(
        &self,
        kubeconfig: &Path,
        namespace: &str,
        name: &str,
        before: Option<String>,
    ) -> Result<ApplyInFlight> {
        let bytes: Arc<[u8]> = std::fs::read(kubeconfig)
            .map_err(|e| CliError::Other(format!("read kubeconfig {}: {e}", kubeconfig.display())))?
            .into();
        let mut state = self.lock();
        if state.stopping {
            return Err(interrupted_error());
        }
        state.pods.insert(
            (namespace.to_string(), name.to_string()),
            Tracked {
                origin: Origin::Unconfirmed { before },
                kubeconfig: bytes,
            },
        );
        state.applying += 1;
        Ok(ApplyInFlight(self.clone()))
    }

    /// Record the uid of the pod `namespace/name` as read after its apply.
    /// The first read settles an unconfirmed apply: a uid other than the one
    /// from before was created by it, the same one was not. Later reads
    /// change nothing.
    pub(crate) fn observed(&self, namespace: &str, name: &str, uid: &str) {
        if uid.is_empty() {
            return;
        }
        let mut state = self.lock();
        let Some(tracked) = state
            .pods
            .get_mut(&(namespace.to_string(), name.to_string()))
        else {
            return;
        };
        if let Origin::Unconfirmed { before } = &tracked.origin {
            tracked.origin = if before.as_deref() == Some(uid) {
                Origin::Reused(uid.to_string())
            } else {
                Origin::Created(uid.to_string())
            };
        }
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
    /// Leave it: the pod with this uid was there before this process applied
    /// it.
    NotCreatedHere { uid: String },
    /// There is no pod of that name.
    Gone,
    /// Read the pod first, then decide ([`cleanup_action`] again with what
    /// was read).
    ReadFirst,
}

/// Decide what the stop does with a helper pod of origin `origin`, given the
/// uid of the pod of that name as just read (`now`, `Some(None)` for none),
/// or `None` when it has not been read. Pure.
pub(crate) fn cleanup_action(origin: &Origin, now: Option<Option<&str>>) -> CleanupAction {
    match origin {
        Origin::Created(uid) => CleanupAction::Delete { uid: uid.clone() },
        Origin::Reused(uid) => CleanupAction::NotCreatedHere { uid: uid.clone() },
        Origin::Unconfirmed { before } => match now {
            None => CleanupAction::ReadFirst,
            Some(None) => CleanupAction::Gone,
            Some(Some(uid)) if before.as_deref() == Some(uid) => CleanupAction::NotCreatedHere {
                uid: uid.to_string(),
            },
            Some(Some(uid)) => CleanupAction::Delete {
                uid: uid.to_string(),
            },
        },
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
    stdout: String,
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
                return Some(Ran {
                    ok: status.success(),
                    stdout: out.join().unwrap_or_default(),
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

/// Clean up one helper pod `ns/name` of origin `origin`: read it if its
/// origin needs that, then delete it or leave it ([`cleanup_action`]). Returns
/// the line the stop prints for it.
pub(crate) fn clean_one(
    kubectl: &Path,
    kubeconfig: &Path,
    ns: &str,
    name: &str,
    origin: &Origin,
    deadline: Instant,
) -> String {
    let by_hand = format!("delete it with `kubectl delete pod {name} -n {ns}` if nothing uses it");
    let mut action = cleanup_action(origin, None);
    if action == CleanupAction::ReadFirst {
        let read = kubectl_bounded(
            kubectl,
            kubeconfig,
            &[
                "get",
                "pod",
                name,
                "-n",
                ns,
                "--ignore-not-found",
                "-o",
                "jsonpath={.metadata.uid}",
            ],
            None,
            deadline,
        );
        action = match read {
            Some(r) if r.ok => {
                let uid = r.stdout.trim();
                cleanup_action(origin, Some((!uid.is_empty()).then_some(uid)))
            }
            Some(r) => {
                return format!(
                    "  could not read helper pod {ns}/{name} ({}): {by_hand}",
                    r.stderr.trim()
                )
            }
            None => return format!("  ran out of time reading helper pod {ns}/{name}: {by_hand}"),
        };
    }
    match action {
        CleanupAction::Gone => format!("  helper pod {ns}/{name} is already gone"),
        CleanupAction::NotCreatedHere { .. } => format!(
            "  left helper pod {ns}/{name}: it was there before this command applied it, so \
             another run may be using it (that run deletes it)"
        ),
        CleanupAction::ReadFirst => unreachable!("decided above"),
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
#[must_use = "the command is interruptible only while this is held"]
pub(crate) struct StopGuard(());

impl Drop for StopGuard {
    fn drop(&mut self) {
        if interrupted() {
            UNWOUND.store(true, Ordering::SeqCst);
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
fn ignored_at_start(signal: i32) -> bool {
    // SAFETY: sigaction with a null new action only reads the current one
    // into `old`, which is a plain, zero-initialised struct.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(signal, std::ptr::null(), &mut old) == 0
            && old.sa_sigaction == libc::SIG_IGN
    }
}

fn register(signals: &[i32]) -> std::io::Result<()> {
    for &signal in signals {
        // In this order, because signal-hook runs a signal's actions in the
        // order they were registered: the first signal finds the flag unset
        // and only sets it; a second finds it set and ends the process from
        // the handler itself, whatever the stop is doing.
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
                stop(signal);
            }
        })?;
    Ok(())
}

/// The stop: see the module docs. Ends the process.
fn stop(signal: i32) -> ! {
    let started = Instant::now();
    let deadline = started + STOP_BOUND;
    let helpers = HelperPods::global();
    let say = |line: &str| {
        // A write to a closed terminal must not panic the stop.
        let _ = writeln!(std::io::stderr(), "{line}");
    };
    let signal_name = match signal {
        signal_hook::consts::SIGINT => "Ctrl-C (SIGINT)",
        signal_hook::consts::SIGTERM => "SIGTERM",
        _ => "a signal",
    };
    say(&format!(
        "\n{signal_name}: deleting the helper pods this command created, then exiting \
         (at most {}s; interrupt again to exit at once).",
        STOP_BOUND.as_secs()
    ));

    // 1. No apply after this; the ones under way are waited out.
    helpers.close();
    let settle = started + APPLY_SETTLE_BOUND;
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
    let mut copies: Vec<(Arc<[u8]>, tempfile::NamedTempFile)> = Vec::new();
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
    std::process::exit(128 + signal)
}

/// The kubeconfig `bytes` in a file only this user can read, removed when
/// dropped.
fn private_copy(bytes: &[u8]) -> std::io::Result<tempfile::NamedTempFile> {
    let mut file = tempfile::Builder::new()
        .prefix("apprafter-interrupt-")
        .tempfile()?;
    file.write_all(bytes)?;
    file.flush()?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kubeconfig() -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "apiVersion: v1\nkind: Config\n").unwrap();
        f
    }

    /// A pod this process's apply created is deleted — that pod, by uid; one
    /// that was there before the apply is left for the run using it; an
    /// unconfirmed apply is settled by reading the pod.
    #[test]
    fn only_a_pod_this_process_created_is_deleted() {
        let created = Origin::Created("u-new".into());
        assert_eq!(
            cleanup_action(&created, None),
            CleanupAction::Delete {
                uid: "u-new".into()
            }
        );
        let reused = Origin::Reused("u-old".into());
        assert_eq!(
            cleanup_action(&reused, None),
            CleanupAction::NotCreatedHere {
                uid: "u-old".into()
            }
        );

        let unconfirmed_over = Origin::Unconfirmed {
            before: Some("u-old".into()),
        };
        assert_eq!(
            cleanup_action(&unconfirmed_over, None),
            CleanupAction::ReadFirst
        );
        // The apply never reached the apiserver: the pod is the one from before.
        assert_eq!(
            cleanup_action(&unconfirmed_over, Some(Some("u-old"))),
            CleanupAction::NotCreatedHere {
                uid: "u-old".into()
            }
        );
        // A different uid: the pod from before went (deleted by its own run)
        // and this apply created the one there now.
        assert_eq!(
            cleanup_action(&unconfirmed_over, Some(Some("u-new"))),
            CleanupAction::Delete {
                uid: "u-new".into()
            }
        );
        assert_eq!(
            cleanup_action(&unconfirmed_over, Some(None)),
            CleanupAction::Gone
        );

        let unconfirmed_fresh = Origin::Unconfirmed { before: None };
        assert_eq!(
            cleanup_action(&unconfirmed_fresh, Some(Some("u-new"))),
            CleanupAction::Delete {
                uid: "u-new".into()
            }
        );
        assert_eq!(
            cleanup_action(&unconfirmed_fresh, Some(None)),
            CleanupAction::Gone
        );
    }

    #[test]
    fn the_first_read_after_an_apply_settles_its_origin_and_later_ones_do_not() {
        let kc = kubeconfig();
        let pods = HelperPods::default();
        drop(
            pods.begin_apply(kc.path(), "demo", "bk-pg-db", None)
                .unwrap(),
        );
        assert_eq!(
            pods.origin_of("demo", "bk-pg-db"),
            Some(Origin::Unconfirmed { before: None })
        );
        pods.observed("demo", "bk-pg-db", "u-1");
        pods.observed("demo", "bk-pg-db", "u-2");
        assert_eq!(
            pods.origin_of("demo", "bk-pg-db"),
            Some(Origin::Created("u-1".into()))
        );

        drop(
            pods.begin_apply(kc.path(), "demo", "bk-vol-v", Some("u-old".into()))
                .unwrap(),
        );
        pods.observed("demo", "bk-vol-v", "");
        assert_eq!(
            pods.origin_of("demo", "bk-vol-v"),
            Some(Origin::Unconfirmed {
                before: Some("u-old".into())
            }),
            "an empty uid settles nothing"
        );
        pods.observed("demo", "bk-vol-v", "u-old");
        assert_eq!(
            pods.origin_of("demo", "bk-vol-v"),
            Some(Origin::Reused("u-old".into()))
        );

        pods.deleted("demo", "bk-pg-db");
        assert_eq!(pods.origin_of("demo", "bk-pg-db"), None);
        // A pod never applied is never recorded by a read.
        pods.observed("demo", "other", "u-9");
        assert_eq!(pods.origin_of("demo", "other"), None);
    }

    /// Once the stop has begun, no helper is applied — and every apply under
    /// way is counted until its guard drops, for the stop to wait out.
    #[test]
    fn no_apply_begins_after_the_stop_and_those_under_way_are_counted() {
        let kc = kubeconfig();
        let pods = HelperPods::default();
        let a = pods.begin_apply(kc.path(), "demo", "a", None).unwrap();
        let b = pods.begin_apply(kc.path(), "demo", "b", None).unwrap();
        assert_eq!(pods.applies_in_flight(), 2);
        pods.close();
        let refused = pods
            .begin_apply(kc.path(), "demo", "c", None)
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
        drop(pods.begin_apply(kc.path(), "demo", "a", None).unwrap());
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
    /// answers `get` with `uid` and `delete` with `delete_answer` (a shell
    /// snippet).
    fn stub(dir: &tempfile::TempDir, uid: &str, delete_answer: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.path().join("kubectl-stub");
        let log = dir.path().join("log");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\ncase \"$1\" in __probe) exit 0;; esac\n\
                 echo \"$@\" >> {log}\n\
                 case \"$1\" in\n\
                 get) printf '%s' '{uid}'; exit 0;;\n\
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

    fn log(dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(dir.path().join("log")).unwrap_or_default()
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    #[test]
    fn a_created_pod_is_deleted_by_uid_without_being_read() {
        let dir = tempfile::tempdir().unwrap();
        let kubectl = stub(&dir, "unused", "exit 0");
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

    #[test]
    fn an_unconfirmed_apply_is_read_and_a_pod_from_before_it_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let kubectl = stub(&dir, "u-old", "exit 0");
        let kc = kubeconfig();
        let line = clean_one(
            &kubectl,
            kc.path(),
            "nats",
            "rs-js-x",
            &Origin::Unconfirmed {
                before: Some("u-old".into()),
            },
            soon(),
        );
        assert!(
            line.starts_with("  left helper pod nats/rs-js-x: it was there before"),
            "{line}"
        );
        let log = log(&dir);
        assert!(
            log.starts_with(
                "get pod rs-js-x -n nats --ignore-not-found -o jsonpath={.metadata.uid}"
            ),
            "{log}"
        );
        assert!(!log.contains("delete"), "{log}");
    }

    #[test]
    fn an_unconfirmed_apply_that_created_its_pod_deletes_that_pod() {
        let dir = tempfile::tempdir().unwrap();
        let kubectl = stub(&dir, "u-new", "exit 0");
        let kc = kubeconfig();
        let line = clean_one(
            &kubectl,
            kc.path(),
            "demo",
            "ld-pg-db",
            &Origin::Unconfirmed { before: None },
            soon(),
        );
        assert_eq!(line, "  deleted helper pod demo/ld-pg-db");
        assert!(log(&dir).contains(r#""preconditions":{"uid":"u-new"}"#));

        let dir = tempfile::tempdir().unwrap();
        let kubectl = stub(&dir, "", "exit 0");
        let line = clean_one(
            &kubectl,
            kc.path(),
            "demo",
            "ld-pg-db",
            &Origin::Unconfirmed { before: None },
            soon(),
        );
        assert_eq!(line, "  helper pod demo/ld-pg-db is already gone");
        assert!(!log(&dir).contains("delete"));
    }

    /// The apiserver's answers to a delete by uid, as `kubectl delete --raw`
    /// prints them (Kubernetes 1.36): a pod of that name that is another one
    /// now, and none at all.
    #[test]
    fn a_delete_refused_by_its_precondition_or_of_a_gone_pod_is_reported_as_such() {
        let kc = kubeconfig();
        let dir = tempfile::tempdir().unwrap();
        let kubectl = stub(
            &dir,
            "",
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
            "",
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
        let kubectl = stub(
            &dir,
            "",
            "echo 'Unable to connect to the server' >&2; exit 1",
        );
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
    #[test]
    fn a_kubectl_that_hangs_is_killed_at_the_deadline() {
        let kc = kubeconfig();
        let dir = tempfile::tempdir().unwrap();
        let kubectl = stub(&dir, "", "exec sleep 30");
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
}
