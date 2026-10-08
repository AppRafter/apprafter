// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Plans and operations, held in Rust and addressed by [`OpId`] (ADR 0067 §3).
//!
//! A slice registers a plan — what the webview shows ([`PlanParts`]) and what runs once it is
//! confirmed ([`Executor`]) — and the webview gets a [`PlanView`] and an id. The plan never
//! crosses IPC. [`OperationManager::execute`] consumes it exactly once, asks the device owner
//! itself when the plan needs a gesture, and runs the executor on a thread of its own.
//!
//! An operation's events go into its [`ReplayBuffer`] and to every subscribed
//! [`EventSink`], both under the manager's lock, so a page that subscribes gets the replay
//! and then exactly the events after it. An ended operation keeps its summary and replay
//! until the webview [`discard`](OperationManager::discard)s it after showing the result, or
//! until [`ENDED_KEPT`] operations ended after it.
//!
//! A plan leaves the manager's maps under its lock and is dropped after the lock is released:
//! what an executor captured may do anything when it goes, calling back into the manager
//! included.

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::mem;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use apprafter_core::{
    CancellationToken, CoreError, CoreResult, Outcome, PlanClass, PlannedChange, Reporter, UiError,
};
use apprafter_desktop_ipc::{AuthOutcome, OpEvent, OpId, OpState, OpSummary, PlanView};

use super::replay::REPLAY_CAP;
use super::{trip, Clock, OpReporter, ReplayBuffer};
use crate::auth::{AuthPurpose, Authenticator};
use crate::errors::DesktopError;

/// How long a plan can wait for `execute`, on the monotonic clock.
pub const PLAN_TTL_MS: u64 = 10 * 60 * 1000;

/// How many ended operations are kept, not yet discarded, before the first of them to end
/// goes. A running operation is never evicted.
pub const ENDED_KEPT: usize = 64;

/// An operation thread's stack (GOTCHA-67: the core is synchronous and deep).
const OP_STACK_BYTES: usize = 8 << 20;

/// The threads that trip a token.
const CANCEL_THREAD: &str = "op-cancel";

/// Where an operation's events go: a page's channel.
pub trait EventSink: Send + Sync {
    /// `false` when the receiver is gone; the manager then drops the sink. Called — and the
    /// sink dropped — under the manager's lock, so it must be quick and must never call back
    /// into the manager.
    fn send(&self, event: &OpEvent) -> bool;
    /// The label of the webview the sink delivers to.
    fn webview(&self) -> &str;
}

/// What a plan shows, and what the OS prompt names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanParts {
    pub class: PlanClass,
    pub title: String,
    pub changes: Vec<PlannedChange>,
    /// The target the operation acts on, when it has one.
    pub target: Option<String>,
    /// What the prompt says the owner confirms, e.g. `delete`.
    pub verb: String,
    /// Ask the device owner before running. [`PlanParts::new`] sets it for a destructive
    /// plan; a slice sets it for an approval. A destructive plan asks even when it is unset.
    pub requires_gesture: bool,
}

impl PlanParts {
    /// No changes and no target yet; `requires_gesture` follows the class.
    pub fn new(class: PlanClass, title: impl Into<String>, verb: impl Into<String>) -> Self {
        Self {
            class,
            title: title.into(),
            changes: Vec::new(),
            target: None,
            verb: verb.into(),
            requires_gesture: class == PlanClass::Destructive,
        }
    }
}

/// What runs once a plan is confirmed. It reports through the reporter and stops when the
/// token trips: `Ok(Outcome::Cancelled { cleaned, left })` ends the operation cancelled with
/// what it removed on the way out and what it had to leave (a helper pod, a restic lock), and
/// `Err(CoreError::Cancelled)` ends it cancelled with nothing to name. `Completed` after the
/// token tripped means it finished anyway, and it ends completed.
pub type Executor = Box<
    dyn FnOnce(&dyn Reporter, &CancellationToken) -> CoreResult<Outcome<serde_json::Value>> + Send,
>;

pub struct OperationManager {
    clock: Arc<dyn Clock>,
    inner: Mutex<Inner>,
    /// Notified whenever an operation stops running.
    ended: Condvar,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    /// Registered, waiting for `execute`.
    pending: HashMap<OpId, Pending>,
    /// Taken by `execute`, the OS prompt open.
    prompts: HashMap<OpId, Prompt>,
    /// Running, or ended and not yet discarded.
    ops: HashMap<OpId, Op>,
    /// The ended operations in `ops`, the first to end first.
    ended: VecDeque<OpId>,
}

impl Inner {
    fn running(&self) -> usize {
        self.ops.values().filter(|op| op.run().is_some()).count()
    }

    /// Take out every plan one [`PLAN_TTL_MS`] past its deadline, for the caller to drop
    /// once it has let go of the lock.
    fn sweep(&mut self, now_ms: u64) -> Vec<Pending> {
        self.pending
            .extract_if(|_, plan| now_ms > plan.deadline_ms.saturating_add(PLAN_TTL_MS))
            .map(|(_, plan)| plan)
            .collect()
    }

    /// End the operation if it still runs, with the final event `last` makes: send it, keep
    /// it, and drop the sinks. Returns the ended operations past [`ENDED_KEPT`], for the
    /// caller to drop once it has let go of the lock.
    fn end(&mut self, id: OpId, last: impl FnOnce() -> (OpEvent, OpState)) -> Vec<Op> {
        let Some(op) = self.ops.get_mut(&id).filter(|op| op.run().is_some()) else {
            return Vec::new();
        };
        let (event, state) = last();
        // Nothing here may keep the operation running: not a sink, not the buffer.
        let _ = panic::catch_unwind(AssertUnwindSafe(|| {
            fan_out(&mut op.sinks, &event);
            op.replay.push(event);
        }));
        op.sinks.clear();
        op.status = Status::Ended(state);
        self.ended.push_back(id);
        let excess = self.ended.len().saturating_sub(ENDED_KEPT);
        let evicted: Vec<OpId> = self.ended.drain(..excess).collect();
        evicted
            .into_iter()
            .filter_map(|old| self.ops.remove(&old))
            .collect()
    }
}

struct Pending {
    title: String,
    target: Option<String>,
    /// Set when the owner must confirm before it runs.
    gesture: Option<AuthPurpose>,
    /// On the monotonic clock: after it, `execute` refuses the plan.
    deadline_ms: u64,
    exec: Executor,
    /// Pages subscribed before `execute`; they follow the plan into its operation.
    sinks: Vec<Arc<dyn EventSink>>,
}

struct Prompt {
    /// Tripping it closes the OS dialog.
    cancel: CancellationToken,
    /// Set under the manager's lock by a lock, a cancel or a quit: the plan is refused,
    /// whatever the OS answers. The token trips later, on a thread of its own, so this flag —
    /// not the token — decides; a `Verified` that arrives before the dialog closed is
    /// refused too.
    refused: bool,
    sinks: Vec<Arc<dyn EventSink>>,
}

struct Op {
    title: String,
    target: Option<String>,
    started_at_ms: u64,
    status: Status,
    replay: ReplayBuffer,
    /// Pages that receive what comes next; empty once the operation ended.
    sinks: Vec<Arc<dyn EventSink>>,
}

impl Op {
    /// `Some` while the executor runs.
    fn run(&self) -> Option<&Run> {
        match &self.status {
            Status::Running(run) => Some(run),
            Status::Ended(_) => None,
        }
    }
}

enum Status {
    Running(Run),
    /// Never `OpState::Running`.
    Ended(OpState),
}

struct Run {
    cancel: CancellationToken,
    reporter: Arc<OpReporter>,
}

/// What a prompt's answer means for its plan.
enum Answer {
    /// Run it; the pages that followed the prompt follow the operation.
    Run(Vec<Arc<dyn EventSink>>),
    /// Nothing was asked: the plan waits for another `execute`, as it was.
    Again(Vec<Arc<dyn EventSink>>),
    Refuse(DesktopError),
}

impl OperationManager {
    pub fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            inner: Mutex::default(),
            ended: Condvar::new(),
        })
    }

    /// Keep a plan for [`execute`](Self::execute); the view is all the webview gets. Its
    /// `expires_at_ms` is wall time, for display; the plan itself expires one
    /// [`PLAN_TTL_MS`] from now on the monotonic clock, whatever the wall clock does.
    ///
    /// A plan lives until it is executed, cancelled or discarded, the app locks, or one
    /// [`PLAN_TTL_MS`] after it expired: swept here and on every
    /// [`flush_due`](Self::flush_due) tick, so an abandoned plan does not keep what its
    /// executor captured. Until then `execute` answers `PlanExpired`.
    pub fn register_plan(&self, parts: PlanParts, exec: Executor) -> PlanView {
        let now = self.clock.monotonic_ms();
        let expires_at_ms = self.clock.now_ms().saturating_add(PLAN_TTL_MS);
        let needs_gesture = parts.requires_gesture || parts.class == PlanClass::Destructive;
        let gesture = needs_gesture.then(|| AuthPurpose::Confirm {
            target: parts.target.clone(),
            verb: parts.verb,
        });
        let mut inner = self.lock();
        let swept = inner.sweep(now);
        inner.next_id += 1;
        let op_id = OpId(inner.next_id);
        inner.pending.insert(
            op_id,
            Pending {
                title: parts.title.clone(),
                target: parts.target.clone(),
                gesture,
                deadline_ms: now.saturating_add(PLAN_TTL_MS),
                exec,
                sinks: Vec::new(),
            },
        );
        drop(inner);
        drop(swept);
        PlanView {
            op_id,
            class: parts.class,
            title: parts.title,
            changes: parts.changes,
            target: parts.target,
            expires_at_ms,
        }
    }

    /// Run a plan, once: whatever happens here, the plan is spent — except on `AuthBusy`,
    /// when another prompt was open and nothing was asked, and the plan waits for another try
    /// with the time it had left.
    ///
    /// When the plan needs the owner, this asks `auth` and blocks until the prompt answers
    /// (so the caller is a blocking thread, never an async worker). Anything but `Verified`
    /// refuses the plan, and so does a lock, a cancel or a quit that closed the prompt — even
    /// when the OS still answered yes — and so does a prompt that stayed open past the
    /// plan's expiry; the executor then never runs. Otherwise it starts on a thread of its
    /// own, named `op-<id>`, with an 8 MiB stack.
    pub fn execute(
        self: &Arc<Self>,
        id: OpId,
        auth: &dyn Authenticator,
    ) -> Result<OpId, DesktopError> {
        let now = self.clock.monotonic_ms();
        let mut inner = self.lock();
        let mut plan = inner
            .pending
            .remove(&id)
            .ok_or(DesktopError::PlanNotFound { op_id: id })?;
        if now > plan.deadline_ms {
            drop(inner);
            drop(plan);
            return Err(DesktopError::PlanExpired { op_id: id });
        }
        if let Some(purpose) = plan.gesture.clone() {
            let cancel = CancellationToken::new();
            inner.prompts.insert(
                id,
                Prompt {
                    cancel: cancel.clone(),
                    refused: false,
                    sinks: mem::take(&mut plan.sinks),
                },
            );
            drop(inner);
            let answer = panic::catch_unwind(AssertUnwindSafe(|| auth.verify(&purpose, &cancel)));
            let now = self.clock.monotonic_ms();
            inner = self.lock();
            let prompt = inner.prompts.remove(&id);
            match judge(id, answer, prompt, now > plan.deadline_ms) {
                Answer::Run(sinks) => plan.sinks = sinks,
                Answer::Again(sinks) => {
                    plan.sinks = sinks;
                    inner.pending.insert(id, plan);
                    return Err(DesktopError::AuthBusy);
                }
                Answer::Refuse(err) => {
                    drop(inner);
                    drop(plan);
                    return Err(err);
                }
            }
        }
        let cancel = CancellationToken::new();
        let reporter = {
            let manager = Arc::downgrade(self);
            Arc::new(OpReporter::new(
                self.clock.clone(),
                Box::new(move |event| {
                    if let Some(manager) = manager.upgrade() {
                        manager.publish(id, event);
                    }
                }),
            ))
        };
        inner.ops.insert(
            id,
            Op {
                title: plan.title,
                target: plan.target,
                started_at_ms: self.clock.now_ms(),
                status: Status::Running(Run {
                    cancel: cancel.clone(),
                    reporter: reporter.clone(),
                }),
                replay: ReplayBuffer::new(REPLAY_CAP),
                sinks: plan.sinks,
            },
        );
        drop(inner);
        let exec = plan.exec;
        let manager = Arc::clone(self);
        if let Err(e) = spawn_op(id, move || manager.run_op(id, exec, &reporter, &cancel)) {
            // The pages that followed the plan wait for a final event: they get one.
            let message = format!("could not start the operation's thread: {e}");
            let error = DesktopError::Internal(message.clone()).to_ui();
            self.end(id, || (OpEvent::Failed { error }, OpState::Failed));
            return Err(DesktopError::Internal(message));
        }
        Ok(id)
    }

    /// The operation's thread: run the executor, then send its final event. However the
    /// thread ends — a panic after the executor returned included — the operation ends, and
    /// a quit waiting for it wakes.
    fn run_op(&self, id: OpId, exec: Executor, reporter: &OpReporter, cancel: &CancellationToken) {
        let _ending = Ending { manager: self, id };
        let result = panic::catch_unwind(AssertUnwindSafe(|| exec(reporter, cancel)));
        // What the tool wrote goes out before the final event — and outside the manager's
        // lock: the reporter calls its sink, which takes that lock, under its own.
        reporter.finish();
        self.end(id, || final_event(result));
    }

    /// End the operation if it still runs (see [`Inner::end`]) and wake whoever waits for
    /// that; what the ended cap evicted is dropped after the lock is released.
    fn end(&self, id: OpId, last: impl FnOnce() -> (OpEvent, OpState)) {
        let evicted = self.lock().end(id, last);
        self.ended.notify_all();
        drop(evicted);
    }

    /// The reporter's sink: keep the event and send it in one step under the lock, so a
    /// subscriber's replay and its live events neither overlap nor leave a gap.
    fn publish(&self, id: OpId, event: OpEvent) {
        let mut inner = self.lock();
        if let Some(op) = inner.ops.get_mut(&id).filter(|op| op.run().is_some()) {
            fan_out(&mut op.sinks, &event);
            op.replay.push(event);
        }
    }

    /// What the operation reported so far; `sink` then receives every later event, with
    /// nothing lost or repeated in between, and the caller shows the replay first. A plan
    /// has no events yet, and the sink follows it when it runs. An ended operation returns
    /// its whole replay and keeps no sink.
    pub fn subscribe(
        &self,
        id: OpId,
        sink: Arc<dyn EventSink>,
    ) -> Result<Vec<OpEvent>, DesktopError> {
        let mut inner = self.lock();
        if let Some(op) = inner.ops.get_mut(&id) {
            if op.run().is_some() {
                op.sinks.push(sink);
            }
            return Ok(op.replay.snapshot());
        }
        if let Some(plan) = inner.pending.get_mut(&id) {
            plan.sinks.push(sink);
        } else if let Some(prompt) = inner.prompts.get_mut(&id) {
            prompt.sinks.push(sink);
        } else {
            return Err(DesktopError::PlanNotFound { op_id: id });
        }
        Ok(Vec::new())
    }

    /// Stop an operation: trip its token, which the executor sees and stops the processes
    /// it started through. Returns at once; the operation then ends with its own final
    /// event. An open prompt is refused before this returns, and its dialog closes; a plan
    /// not yet executed is dropped; an ended operation is left as it is.
    pub fn cancel(&self, id: OpId) -> Result<(), DesktopError> {
        let mut inner = self.lock();
        if let Some(plan) = inner.pending.remove(&id) {
            drop(inner);
            drop(plan);
            return Ok(());
        }
        let token = if let Some(prompt) = inner.prompts.get_mut(&id) {
            prompt.refused = true;
            prompt.cancel.clone()
        } else {
            match inner.ops.get(&id).map(Op::run) {
                None => return Err(DesktopError::PlanNotFound { op_id: id }),
                Some(None) => return Ok(()),
                Some(Some(run)) => run.cancel.clone(),
            }
        };
        drop(inner);
        trip(token, CANCEL_THREAD);
        Ok(())
    }

    /// Forget an ended operation (the webview has shown its result) or a plan (its dialog
    /// closed). A running operation or an open prompt is left alone: cancel it first.
    pub fn discard(&self, id: OpId) {
        let mut inner = self.lock();
        let plan = inner.pending.remove(&id);
        let op = if plan.is_none() && inner.ops.get(&id).is_some_and(|op| op.run().is_none()) {
            inner.ended.retain(|&ended| ended != id);
            inner.ops.remove(&id)
        } else {
            None
        };
        drop(inner);
        drop((plan, op));
    }

    /// Running and ended (not yet discarded) operations, the latest started first.
    pub fn list(&self) -> Vec<OpSummary> {
        let inner = self.lock();
        let mut list: Vec<OpSummary> = inner
            .ops
            .iter()
            .map(|(&op_id, op)| OpSummary {
                op_id,
                title: op.title.clone(),
                target: op.target.clone(),
                state: match op.status {
                    Status::Running(_) => OpState::Running,
                    Status::Ended(state) => state,
                },
                started_at_ms: op.started_at_ms,
            })
            .collect();
        list.sort_by_key(|s| std::cmp::Reverse((s.started_at_ms, s.op_id.0)));
        list
    }

    /// How many executors are running.
    pub fn running(&self) -> usize {
        self.lock().running()
    }

    /// The app locked: no plan made before it may run after it. Every plan goes, and every
    /// open prompt is refused before this returns (its dialog closes, and its `execute`
    /// refuses whatever the OS answers). Running operations go on: they were confirmed
    /// before the lock.
    pub fn drop_all_plans(&self) {
        let mut inner = self.lock();
        let plans: Vec<Pending> = inner.pending.drain().map(|(_, plan)| plan).collect();
        let prompts = refuse_prompts(&mut inner.prompts);
        drop(inner);
        drop(plans);
        for token in prompts {
            trip(token, CANCEL_THREAD);
        }
    }

    /// A page reloaded or closed: the sinks that delivered to it go.
    pub fn drop_subscribers_of(&self, webview: &str) {
        let mut inner = self.lock();
        let Inner {
            pending,
            prompts,
            ops,
            ..
        } = &mut *inner;
        let all = pending
            .values_mut()
            .map(|p| &mut p.sinks)
            .chain(prompts.values_mut().map(|p| &mut p.sinks))
            .chain(ops.values_mut().map(|op| &mut op.sinks));
        for sinks in all {
            sinks.retain(|s| s.webview() != webview);
        }
    }

    /// The output ticker (every [`FLUSH_AGE_MS`](super::reporter::FLUSH_AGE_MS)): send the
    /// output that has waited long enough, and sweep the plans long expired.
    ///
    /// The reporters are called after the lock is released. A reporter calls its sink,
    /// which takes this lock, under its own lock; holding them the other way round
    /// deadlocks against an operation that is writing.
    pub fn flush_due(&self) {
        let now = self.clock.monotonic_ms();
        let (reporters, swept) = {
            let mut inner = self.lock();
            let swept = inner.sweep(now);
            let reporters: Vec<Arc<OpReporter>> = inner
                .ops
                .values()
                .filter_map(|op| op.run().map(|run| run.reporter.clone()))
                .collect();
            (reporters, swept)
        };
        drop(swept);
        let now = self.clock.now_ms();
        for reporter in reporters {
            reporter.flush_due(now);
        }
    }

    /// Quit: refuse every open prompt and close its dialog, trip every running operation's
    /// token, then wait until no executor runs or `bound` has passed. `true` when all ended
    /// in time.
    pub fn cancel_all_and_wait(&self, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        let tokens: Vec<CancellationToken> = {
            let mut inner = self.lock();
            let mut tokens = refuse_prompts(&mut inner.prompts);
            tokens.extend(
                inner
                    .ops
                    .values()
                    .filter_map(|op| op.run().map(|run| run.cancel.clone())),
            );
            tokens
        };
        for token in tokens {
            trip(token, CANCEL_THREAD);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (inner, _) = self
            .ended
            .wait_timeout_while(self.lock(), remaining, |inner| inner.running() > 0)
            .unwrap_or_else(|p| p.into_inner());
        inner.running() == 0
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Ends its operation as failed when the operation's thread unwinds before the operation
/// ended; after a normal end it finds nothing to do.
struct Ending<'a> {
    manager: &'a OperationManager,
    id: OpId,
}

impl Drop for Ending<'_> {
    fn drop(&mut self) {
        self.manager.end(self.id, || {
            let error = DesktopError::Internal(
                "the operation's thread panicked before it reported how the operation ended".into(),
            );
            (
                OpEvent::Failed {
                    error: error.to_ui(),
                },
                OpState::Failed,
            )
        });
    }
}

/// Refuse every open prompt not refused yet; their tokens, to close their dialogs.
fn refuse_prompts(prompts: &mut HashMap<OpId, Prompt>) -> Vec<CancellationToken> {
    prompts
        .values_mut()
        .filter(|prompt| !prompt.refused)
        .map(|prompt| {
            prompt.refused = true;
            prompt.cancel.clone()
        })
        .collect()
}

/// What a prompt's answer means for plan `id`. `prompt` is its entry, gone when it was
/// closed some other way; `expired` says the plan's deadline passed while it was open.
fn judge(
    id: OpId,
    answer: thread::Result<AuthOutcome>,
    prompt: Option<Prompt>,
    expired: bool,
) -> Answer {
    let outcome = match answer {
        Ok(outcome) => outcome,
        Err(panic) => {
            return Answer::Refuse(DesktopError::Internal(format!(
                "the authentication prompt panicked: {}",
                panic_message(&*panic)
            )))
        }
    };
    // A lock, a cancel or a quit closed it: whatever the OS answered, the plan was dropped.
    let Some(prompt) = prompt.filter(|prompt| !prompt.refused) else {
        return Answer::Refuse(DesktopError::AuthCancelled);
    };
    match outcome {
        AuthOutcome::Verified | AuthOutcome::Busy if expired => {
            Answer::Refuse(DesktopError::PlanExpired { op_id: id })
        }
        AuthOutcome::Verified => Answer::Run(prompt.sinks),
        AuthOutcome::Busy => Answer::Again(prompt.sinks),
        AuthOutcome::Cancelled { .. } => Answer::Refuse(DesktopError::AuthCancelled),
        AuthOutcome::Failed { exhausted } => Answer::Refuse(DesktopError::AuthFailed { exhausted }),
        AuthOutcome::Unavailable { reason } => {
            Answer::Refuse(DesktopError::AuthUnavailable { reason })
        }
    }
}

/// The final event and state for how the executor returned.
fn final_event(
    result: thread::Result<CoreResult<Outcome<serde_json::Value>>>,
) -> (OpEvent, OpState) {
    match result {
        Ok(Ok(outcome @ Outcome::Completed { .. })) => {
            (OpEvent::Finished { outcome }, OpState::Finished)
        }
        Ok(Ok(outcome @ Outcome::Cancelled { .. })) => {
            (OpEvent::Finished { outcome }, OpState::Cancelled)
        }
        Ok(Err(CoreError::Cancelled)) => (
            OpEvent::Finished {
                outcome: Outcome::Cancelled {
                    cleaned: Vec::new(),
                    left: Vec::new(),
                },
            },
            OpState::Cancelled,
        ),
        Ok(Err(e)) => (
            OpEvent::Failed {
                error: UiError::from(&e),
            },
            OpState::Failed,
        ),
        Err(panic) => (
            OpEvent::Failed {
                error: DesktopError::Internal(format!(
                    "the operation panicked: {}",
                    panic_message(&*panic)
                ))
                .to_ui(),
            },
            OpState::Failed,
        ),
    }
}

/// Send `event` to every sink; a sink that answers `false` — or panics — is dropped, so a
/// broken page can never stop an operation from ending.
fn fan_out(sinks: &mut Vec<Arc<dyn EventSink>>, event: &OpEvent) {
    sinks
        .retain(|sink| panic::catch_unwind(AssertUnwindSafe(|| sink.send(event))).unwrap_or(false));
}

/// Start an operation's thread, named `op-<id>`, with an 8 MiB stack.
fn spawn_op(id: OpId, run: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    #[cfg(test)]
    if test_spawn::fails() {
        return Err(std::io::Error::other("no thread for the test"));
    }
    thread::Builder::new()
        .name(format!("op-{}", id.0))
        .stack_size(OP_STACK_BYTES)
        .spawn(run)
        .map(drop)
}

/// Making the next [`spawn_op`] on this thread fail, as an exhausted system would.
#[cfg(test)]
mod test_spawn {
    use std::cell::Cell;

    thread_local! {
        static FAIL_NEXT: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn fail_next() {
        FAIL_NEXT.with(|fail| fail.set(true));
    }

    pub(super) fn fails() -> bool {
        FAIL_NEXT.with(|fail| fail.replace(false))
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::sync::{Arc, Mutex, Weak};
    use std::thread;
    use std::time::{Duration, Instant};

    use apprafter_core::{
        CancellationToken, CoreError, CoreResult, Event, Outcome, PlanClass, PlannedChange, Stream,
    };
    use apprafter_desktop_ipc::{
        errors, AuthInfo, AuthOutcome, CancelledBy, OpEvent, OpId, OpState, OutputStream,
        UnavailableReason,
    };
    use serde_json::json;

    use super::{
        test_spawn, EventSink, Executor, OperationManager, PlanParts, ENDED_KEPT, PLAN_TTL_MS,
    };
    use crate::auth::{AuthPurpose, Authenticator, FakeAuthenticator, NoAuthenticator};
    use crate::errors::DesktopError;
    use crate::ops::reporter::FLUSH_AGE_MS;
    use crate::ops::test_clock::ManualClock;
    use crate::ops::{test_trips, Clock};

    const T0: u64 = 1_700_000_000_000;
    /// What a test waits before it calls something stuck.
    const LONG: Duration = Duration::from_secs(10);

    fn manager() -> (Arc<ManualClock>, Arc<OperationManager>) {
        let clock = Arc::new(ManualClock::at(T0));
        let mgr = OperationManager::new(clock.clone());
        (clock, mgr)
    }

    fn parts(class: PlanClass) -> PlanParts {
        PlanParts {
            target: Some("prod".into()),
            changes: vec![PlannedChange {
                kind: "Target".into(),
                object: "prod".into(),
                change: "delete".into(),
            }],
            ..PlanParts::new(class, "Remove target prod", "delete")
        }
    }

    /// What an executor that completed returns.
    fn complete(result: serde_json::Value) -> CoreResult<Outcome<serde_json::Value>> {
        Ok(Outcome::Completed { result })
    }

    fn returns(value: serde_json::Value) -> Executor {
        Box::new(move |_, _| complete(value))
    }

    /// Counts its runs (it can only run once: it is an `FnOnce`).
    fn counting(runs: &Arc<AtomicUsize>) -> Executor {
        let runs = runs.clone();
        Box::new(move |_, _| {
            runs.fetch_add(1, SeqCst);
            complete(json!(null))
        })
    }

    fn stage(index: u32) -> Event {
        Event::Stage {
            index,
            total: 9,
            title: format!("step {index}"),
        }
    }

    fn staged(index: u32) -> OpEvent {
        OpEvent::Stage {
            index,
            total: 9,
            title: format!("step {index}"),
        }
    }

    fn completed(result: serde_json::Value) -> OpEvent {
        OpEvent::Finished {
            outcome: Outcome::Completed { result },
        }
    }

    fn stdout(text: &str) -> OpEvent {
        OpEvent::Output {
            stream: OutputStream::Stdout,
            text: text.into(),
        }
    }

    /// A gate an executor waits at until the test opens it (or `LONG` passes).
    struct Gate(Mutex<mpsc::Receiver<()>>);

    fn gate() -> (mpsc::Sender<()>, Arc<Gate>) {
        let (tx, rx) = mpsc::channel();
        (tx, Arc::new(Gate(Mutex::new(rx))))
    }

    impl Gate {
        fn wait(&self) {
            let _ = self.0.lock().unwrap().recv_timeout(LONG);
        }
    }

    /// Records what it is sent; once closed it answers `false`, as a page that went away.
    struct VecSink {
        webview: String,
        alive: AtomicBool,
        sends: AtomicUsize,
        events: Mutex<Vec<OpEvent>>,
    }

    impl VecSink {
        fn new(webview: &str) -> Arc<Self> {
            Arc::new(Self {
                webview: webview.into(),
                alive: AtomicBool::new(true),
                sends: AtomicUsize::new(0),
                events: Mutex::default(),
            })
        }

        fn events(&self) -> Vec<OpEvent> {
            self.events.lock().unwrap().clone()
        }

        fn sends(&self) -> usize {
            self.sends.load(SeqCst)
        }

        fn close(&self) {
            self.alive.store(false, SeqCst);
        }
    }

    impl EventSink for VecSink {
        fn send(&self, event: &OpEvent) -> bool {
            self.sends.fetch_add(1, SeqCst);
            if !self.alive.load(SeqCst) {
                return false;
            }
            self.events.lock().unwrap().push(event.clone());
            true
        }

        fn webview(&self) -> &str {
            &self.webview
        }
    }

    fn state_of(mgr: &OperationManager, id: OpId) -> Option<OpState> {
        mgr.list()
            .into_iter()
            .find(|s| s.op_id == id)
            .map(|s| s.state)
    }

    /// Waits for the operation to end; its final event has been sent by then.
    fn wait_ended(mgr: &OperationManager, id: OpId) -> OpState {
        let deadline = Instant::now() + LONG;
        loop {
            match state_of(mgr, id) {
                Some(OpState::Running) | None => {}
                Some(state) => return state,
            }
            assert!(Instant::now() < deadline, "operation {id:?} did not end");
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// Runs `f` on a thread of its own, so a deadlock fails the test after `LONG` instead
    /// of hanging it; a panic in `f` is re-raised here.
    fn within<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let _ = tx.send(f());
        });
        match rx.recv_timeout(LONG) {
            Ok(value) => value,
            Err(RecvTimeoutError::Timeout) => {
                panic!("{what} did not return within {LONG:?}: deadlocked")
            }
            Err(RecvTimeoutError::Disconnected) => match handle.join() {
                Err(panic) => std::panic::resume_unwind(panic),
                Ok(()) => panic!("{what} ended without a result"),
            },
        }
    }

    /// The first difference, rather than two dumps of thousands of events.
    fn assert_same_events(got: &[OpEvent], want: &[OpEvent]) {
        if let Some(i) = (0..got.len().min(want.len())).find(|&i| got[i] != want[i]) {
            panic!("event {i} differs: got {:?}, want {:?}", got[i], want[i]);
        }
        assert_eq!(got.len(), want.len(), "same prefix, different lengths");
    }

    // 1. A plan view: fresh id, ten-minute expiry. Nothing of the executor can reach it:
    // `PlanView` has no field to carry it (ipc's `a_plan_view_has_camel_case_keys_and_no_payload`
    // pins its keys), so there is no string search here that could fail.

    #[test]
    fn a_plan_view_has_a_fresh_id_and_a_ten_minute_expiry() {
        let (_, mgr) = manager();
        let view = mgr.register_plan(parts(PlanClass::Destructive), returns(json!(0)));
        assert_eq!(view.expires_at_ms, T0 + PLAN_TTL_MS);
        assert_eq!(PLAN_TTL_MS, 600_000);
        assert_eq!(view.class, PlanClass::Destructive);
        assert_eq!(view.title, "Remove target prod");
        assert_eq!(view.target.as_deref(), Some("prod"));
        assert_eq!(view.changes, parts(PlanClass::Destructive).changes);
        let other = mgr.register_plan(parts(PlanClass::Bounded), returns(json!(1)));
        assert_ne!(view.op_id, other.op_id);
        assert!(mgr.list().is_empty(), "a plan is not an operation yet");
        assert_eq!(mgr.running(), 0);
    }

    // 2. Single use.

    #[test]
    fn a_plan_runs_once() {
        let (_, mgr) = manager();
        let runs = Arc::new(AtomicUsize::new(0));
        let view = mgr.register_plan(parts(PlanClass::Bounded), counting(&runs));
        let auth = FakeAuthenticator::new();
        assert_eq!(mgr.execute(view.op_id, &auth).unwrap(), view.op_id);
        let again = mgr.execute(view.op_id, &auth);
        assert!(
            matches!(again, Err(DesktopError::PlanNotFound { op_id }) if op_id == view.op_id),
            "{again:?}"
        );
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Finished);
        let again = mgr.execute(view.op_id, &auth);
        assert!(
            matches!(again, Err(DesktopError::PlanNotFound { .. })),
            "{again:?}"
        );
        assert_eq!(runs.load(SeqCst), 1);
    }

    #[test]
    fn an_unknown_id_is_not_found() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        assert!(matches!(
            mgr.execute(OpId(404), &auth),
            Err(DesktopError::PlanNotFound { op_id: OpId(404) })
        ));
        assert!(matches!(
            mgr.subscribe(OpId(404), VecSink::new("main")),
            Err(DesktopError::PlanNotFound { .. })
        ));
        assert!(matches!(
            mgr.cancel(OpId(404)),
            Err(DesktopError::PlanNotFound { .. })
        ));
    }

    // 3. Time to live.

    #[test]
    fn an_expired_plan_is_refused_and_gone() {
        let (clock, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let late = mgr.register_plan(parts(PlanClass::Bounded), counting(&runs));
        let edge = mgr.register_plan(parts(PlanClass::Bounded), returns(json!(1)));
        clock.advance(PLAN_TTL_MS);
        assert!(
            mgr.execute(edge.op_id, &auth).is_ok(),
            "still valid at expires_at_ms"
        );
        clock.advance(1);
        let first = mgr.execute(late.op_id, &auth);
        assert!(
            matches!(first, Err(DesktopError::PlanExpired { op_id }) if op_id == late.op_id),
            "{first:?}"
        );
        let second = mgr.execute(late.op_id, &auth);
        assert!(
            matches!(second, Err(DesktopError::PlanNotFound { .. })),
            "{second:?}"
        );
        assert_eq!(runs.load(SeqCst), 0);
    }

    #[test]
    fn a_plan_left_alone_is_swept_one_ttl_after_it_expired() {
        let (clock, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let a = mgr.register_plan(parts(PlanClass::Bounded), returns(json!(1)));
        let b = mgr.register_plan(parts(PlanClass::Bounded), returns(json!(1)));
        clock.advance(2 * PLAN_TTL_MS);
        mgr.register_plan(parts(PlanClass::Bounded), returns(json!(1)));
        assert!(
            matches!(
                mgr.execute(a.op_id, &auth),
                Err(DesktopError::PlanExpired { .. })
            ),
            "kept for one TTL past its expiry"
        );
        clock.advance(1);
        mgr.register_plan(parts(PlanClass::Bounded), returns(json!(1)));
        assert!(matches!(
            mgr.execute(b.op_id, &auth),
            Err(DesktopError::PlanNotFound { .. })
        ));
    }

    // 4. The gesture.

    #[test]
    fn a_destructive_plan_asks_the_owner_before_it_runs() {
        let (_, mgr) = manager();
        let auth = Arc::new(FakeAuthenticator::new());
        let asked_when_run = Arc::new(AtomicUsize::new(usize::MAX));
        let exec: Executor = {
            let auth = auth.clone();
            let seen = asked_when_run.clone();
            Box::new(move |_, _| {
                seen.store(auth.asked().len(), SeqCst);
                complete(json!(null))
            })
        };
        let view = mgr.register_plan(parts(PlanClass::Destructive), exec);
        mgr.execute(view.op_id, &*auth).unwrap();
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Finished);
        assert_eq!(
            auth.asked(),
            vec![AuthPurpose::Confirm {
                target: Some("prod".into()),
                verb: "delete".into(),
            }]
        );
        assert_eq!(asked_when_run.load(SeqCst), 1, "asked before it ran");
    }

    #[test]
    fn a_destructive_plan_asks_even_when_the_slice_cleared_requires_gesture() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let quiet = PlanParts {
            requires_gesture: false,
            ..parts(PlanClass::Destructive)
        };
        let view = mgr.register_plan(quiet, counting(&runs));
        mgr.execute(view.op_id, &auth).unwrap();
        wait_ended(&mgr, view.op_id);
        assert_eq!(auth.asked().len(), 1);
    }

    #[test]
    fn a_bounded_plan_never_asks_and_an_approval_does() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        for class in [PlanClass::Reversible, PlanClass::Bounded] {
            assert!(!PlanParts::new(class, "t", "v").requires_gesture);
            let view = mgr.register_plan(parts(class), returns(json!(1)));
            mgr.execute(view.op_id, &auth).unwrap();
            assert_eq!(wait_ended(&mgr, view.op_id), OpState::Finished);
        }
        assert!(auth.asked().is_empty(), "{:?}", auth.asked());
        assert!(PlanParts::new(PlanClass::Destructive, "t", "v").requires_gesture);
        let approval = PlanParts {
            requires_gesture: true,
            ..PlanParts::new(PlanClass::Bounded, "Approve migration", "approve")
        };
        let view = mgr.register_plan(approval, returns(json!(1)));
        mgr.execute(view.op_id, &auth).unwrap();
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Finished);
        assert_eq!(
            auth.asked(),
            vec![AuthPurpose::Confirm {
                target: None,
                verb: "approve".into(),
            }]
        );
    }

    #[test]
    fn a_refused_gesture_consumes_the_plan_and_never_runs_it() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let cases = [
            (
                AuthOutcome::Cancelled {
                    by: CancelledBy::User,
                },
                DesktopError::AuthCancelled,
            ),
            (
                AuthOutcome::Failed { exhausted: true },
                DesktopError::AuthFailed { exhausted: true },
            ),
            (
                AuthOutcome::Failed { exhausted: false },
                DesktopError::AuthFailed { exhausted: false },
            ),
            (
                AuthOutcome::Unavailable {
                    reason: UnavailableReason::NotInteractive,
                },
                DesktopError::AuthUnavailable {
                    reason: UnavailableReason::NotInteractive,
                },
            ),
        ];
        let runs = Arc::new(AtomicUsize::new(0));
        for (outcome, expected) in cases {
            let view = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
            auth.then(outcome);
            let err = mgr.execute(view.op_id, &auth).unwrap_err();
            assert_eq!(err.to_ui(), expected.to_ui(), "{outcome:?}");
            let again = mgr.execute(view.op_id, &auth);
            assert!(
                matches!(again, Err(DesktopError::PlanNotFound { .. })),
                "{outcome:?}: a refused plan is spent, got {again:?}"
            );
        }
        assert_eq!(runs.load(SeqCst), 0);
        assert!(mgr.list().is_empty());
    }

    #[test]
    fn without_an_os_backend_a_destructive_plan_is_refused() {
        let (_, mgr) = manager();
        let runs = Arc::new(AtomicUsize::new(0));
        let view = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
        let err = mgr.execute(view.op_id, &NoAuthenticator).unwrap_err();
        assert!(
            matches!(
                err,
                DesktopError::AuthUnavailable {
                    reason: UnavailableReason::NoBackend
                }
            ),
            "{err:?}"
        );
        assert_eq!(runs.load(SeqCst), 0);
    }

    /// An OS prompt that stays open until its token trips, then answers `answer` — an
    /// answer of `Verified` is a prompt the owner confirmed just as the app closed it.
    struct OpenPrompt {
        opened: Mutex<mpsc::Sender<()>>,
        answer: AuthOutcome,
    }

    impl Authenticator for OpenPrompt {
        fn info(&self) -> AuthInfo {
            FakeAuthenticator::new().info()
        }

        fn verify(&self, _purpose: &AuthPurpose, cancel: &CancellationToken) -> AuthOutcome {
            let _ = self.opened.lock().unwrap().send(());
            let deadline = Instant::now() + LONG;
            while !cancel.is_cancelled() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            self.answer
        }
    }

    #[test]
    fn closing_an_open_prompt_refuses_the_plan_even_if_the_os_said_yes() {
        type Closer = fn(&OperationManager, OpId);
        let closers: [(&str, Closer); 2] = [
            ("lock", |mgr, _| mgr.drop_all_plans()),
            ("cancel", |mgr, id| mgr.cancel(id).unwrap()),
        ];
        let answers = [
            AuthOutcome::Cancelled {
                by: CancelledBy::App,
            },
            AuthOutcome::Verified,
        ];
        for (name, close) in closers {
            for answer in answers {
                let (_, mgr) = manager();
                let (opened_tx, opened_rx) = mpsc::channel();
                let auth = Arc::new(OpenPrompt {
                    opened: Mutex::new(opened_tx),
                    answer,
                });
                let runs = Arc::new(AtomicUsize::new(0));
                let view = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
                let (result_tx, result_rx) = mpsc::channel();
                {
                    let mgr = mgr.clone();
                    thread::spawn(move || {
                        let _ = result_tx.send(mgr.execute(view.op_id, &*auth));
                    });
                }
                opened_rx.recv_timeout(LONG).expect("the prompt opened");
                assert!(mgr.list().is_empty(), "an open prompt is not an operation");
                close(&mgr, view.op_id);
                let result = result_rx.recv_timeout(LONG).expect("execute returned");
                assert!(
                    matches!(result, Err(DesktopError::AuthCancelled)),
                    "{name}, OS answered {answer:?}: {result:?}"
                );
                assert_eq!(runs.load(SeqCst), 0, "{name}, {answer:?}");
                assert!(mgr.list().is_empty());
                assert!(matches!(
                    mgr.execute(view.op_id, &FakeAuthenticator::new()),
                    Err(DesktopError::PlanNotFound { .. })
                ));
            }
        }
    }

    // 5. Lock.

    #[test]
    fn locking_drops_every_pending_plan_and_leaves_running_ops_alone() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let (open, gate) = gate();
        let running = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(move |_, _| {
                gate.wait();
                complete(json!("kept"))
            }),
        );
        mgr.execute(running.op_id, &auth).unwrap();
        let bounded = mgr.register_plan(parts(PlanClass::Bounded), counting(&runs));
        let destructive = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
        mgr.drop_all_plans();
        for id in [bounded.op_id, destructive.op_id] {
            let result = mgr.execute(id, &auth);
            assert!(
                matches!(result, Err(DesktopError::PlanNotFound { op_id }) if op_id == id),
                "{result:?}"
            );
        }
        assert_eq!(runs.load(SeqCst), 0);
        assert_eq!(state_of(&mgr, running.op_id), Some(OpState::Running));
        open.send(()).unwrap();
        assert_eq!(wait_ended(&mgr, running.op_id), OpState::Finished);
        let after = mgr.register_plan(parts(PlanClass::Bounded), counting(&runs));
        mgr.execute(after.op_id, &auth).unwrap();
        wait_ended(&mgr, after.op_id);
        assert_eq!(runs.load(SeqCst), 1, "a plan made after the lock runs");
    }

    // 6. Events and subscribers.

    #[test]
    fn subscribers_get_events_in_order_and_a_late_one_gets_the_replay_first() {
        let (_, mgr) = manager();
        let (half_tx, half_rx) = mpsc::channel();
        let (open, gate) = gate();
        let view = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(move |r, _| {
                r.report(stage(1));
                r.report(Event::Warning {
                    message: "a".into(),
                });
                half_tx.send(()).unwrap();
                gate.wait();
                r.report(stage(2));
                complete(json!("done"))
            }),
        );
        let early = VecSink::new("main");
        assert_eq!(
            mgr.subscribe(view.op_id, early.clone()).unwrap(),
            vec![],
            "a plan has no events yet"
        );
        mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
        half_rx.recv_timeout(LONG).unwrap();
        let late = VecSink::new("main");
        let warning = OpEvent::Warning {
            message: "a".into(),
        };
        assert_eq!(
            mgr.subscribe(view.op_id, late.clone()).unwrap(),
            vec![staged(1), warning.clone()]
        );
        open.send(()).unwrap();
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Finished);
        let all = vec![staged(1), warning, staged(2), completed(json!("done"))];
        assert_eq!(early.events(), all);
        assert_eq!(late.events(), vec![staged(2), completed(json!("done"))]);
        let after = VecSink::new("main");
        assert_eq!(mgr.subscribe(view.op_id, after.clone()).unwrap(), all);
        assert_eq!(after.sends(), 0, "nothing follows the final event");
    }

    #[test]
    fn a_sink_whose_page_is_gone_is_dropped_and_the_op_keeps_running() {
        let (_, mgr) = manager();
        let (half_tx, half_rx) = mpsc::channel();
        let (open, gate) = gate();
        let view = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(move |r, _| {
                r.report(stage(1));
                half_tx.send(()).unwrap();
                gate.wait();
                r.report(stage(2));
                complete(json!(null))
            }),
        );
        let gone = VecSink::new("main");
        gone.close();
        let live = VecSink::new("main");
        mgr.subscribe(view.op_id, gone.clone()).unwrap();
        mgr.subscribe(view.op_id, live.clone()).unwrap();
        mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
        half_rx.recv_timeout(LONG).unwrap();
        assert_eq!(gone.sends(), 1);
        open.send(()).unwrap();
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Finished);
        assert_eq!(gone.sends(), 1, "tried once, then dropped");
        assert_eq!(
            live.events(),
            vec![staged(1), staged(2), completed(json!(null))]
        );
    }

    #[test]
    fn a_subscriber_never_misses_or_repeats_an_event_while_the_op_emits() {
        const N: u32 = 4_000;
        let expected: Vec<OpEvent> = (0..N)
            .map(|index| OpEvent::Stage {
                index,
                total: N,
                title: String::new(),
            })
            .chain([completed(json!(N))])
            .collect();
        let mut interleaved = 0;
        for _ in 0..8 {
            let (_, mgr) = manager();
            let (half_tx, half_rx) = mpsc::channel();
            let (open, gate) = gate();
            let view = mgr.register_plan(
                parts(PlanClass::Bounded),
                Box::new(move |r, _| {
                    for index in 0..N {
                        r.report(Event::Stage {
                            index,
                            total: N,
                            title: String::new(),
                        });
                        if index == N / 2 {
                            half_tx.send(()).unwrap();
                        }
                    }
                    gate.wait();
                    complete(json!(N))
                }),
            );
            mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
            half_rx.recv_timeout(LONG).unwrap();
            let sink = VecSink::new("main");
            let replay = mgr.subscribe(view.op_id, sink.clone()).unwrap();
            if replay.len() < N as usize {
                interleaved += 1;
            }
            open.send(()).unwrap();
            wait_ended(&mgr, view.op_id);
            let all: Vec<OpEvent> = replay.into_iter().chain(sink.events()).collect();
            // The replay keeps the newest stages and leads with how many went before them:
            // what follows must be exactly the rest, the live events joined on without a gap.
            let (dropped, kept) = match all.split_first() {
                Some((OpEvent::Notice { message }, rest))
                    if message.contains("earlier message") =>
                {
                    let count = message.split(' ').next().and_then(|n| n.parse().ok());
                    (count.expect("the notice starts with a count"), rest)
                }
                _ => (0, &all[..]),
            };
            assert_same_events(kept, &expected[dropped..]);
        }
        // Not asserted (a scheduler may finish the burst first); printed for the record.
        println!("subscribed mid-burst in {interleaved} of 8 rounds");
    }

    #[test]
    fn pending_output_goes_out_before_the_final_event() {
        let (_, mgr) = manager();
        let view = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(|r, _| {
                r.report(Event::Output {
                    stream: Stream::Stdout,
                    bytes: b"tail".to_vec(),
                });
                complete(json!(1))
            }),
        );
        let sink = VecSink::new("main");
        mgr.subscribe(view.op_id, sink.clone()).unwrap();
        mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
        wait_ended(&mgr, view.op_id);
        assert_eq!(sink.events(), vec![stdout("tail"), completed(json!(1))]);
    }

    // 7. Cancel.

    #[test]
    fn cancel_trips_the_token_off_the_calling_thread_and_the_op_ends_cancelled() {
        let (_, mgr) = manager();
        let (started_tx, started_rx) = mpsc::channel();
        let (seen_tx, seen_rx) = mpsc::channel();
        let callback_mgr = Arc::downgrade(&mgr);
        let view = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(move |_, token| {
                let (tx, rx) = mpsc::channel();
                let _registration = token.on_cancel(move || {
                    // Calls back into the manager: a token tripped under its lock deadlocks.
                    let running = callback_mgr.upgrade().map(|m| m.running());
                    let _ = tx.send((thread::current().name().map(str::to_owned), running));
                });
                started_tx.send(()).unwrap();
                seen_tx.send(rx.recv_timeout(LONG).ok()).unwrap();
                token.check()?;
                complete(json!("not cancelled"))
            }),
        );
        mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
        started_rx.recv_timeout(LONG).unwrap();
        let caller = thread::current().name().map(str::to_owned);
        {
            let mgr = mgr.clone();
            within("cancel", move || mgr.cancel(view.op_id)).unwrap();
        }
        let (name, running) = seen_rx
            .recv_timeout(LONG)
            .unwrap()
            .expect("the callback ran");
        assert!(
            name.as_deref().is_some_and(|n| n.starts_with("op-cancel")),
            "tripped on {name:?}"
        );
        assert_ne!(name, caller);
        assert_eq!(running, Some(1));
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Cancelled);
        let replay = mgr.subscribe(view.op_id, VecSink::new("main")).unwrap();
        assert_eq!(
            replay.last(),
            Some(&OpEvent::Finished {
                outcome: Outcome::Cancelled {
                    cleaned: vec![],
                    left: vec![],
                },
            })
        );
        assert!(
            mgr.cancel(view.op_id).is_ok(),
            "cancelling an ended op is a no-op"
        );
        assert_eq!(state_of(&mgr, view.op_id), Some(OpState::Cancelled));
    }

    #[test]
    fn a_panicking_cancel_callback_never_reaches_the_caller() {
        let (_, mgr) = manager();
        let (started_tx, started_rx) = mpsc::channel();
        let view = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(move |_, token| {
                let _boom = token.on_cancel(|| panic!("a cancel callback panicked"));
                started_tx.send(()).unwrap();
                let deadline = Instant::now() + LONG;
                while !token.is_cancelled() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(CoreError::Cancelled)
            }),
        );
        mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
        started_rx.recv_timeout(LONG).unwrap();
        let result = {
            let mgr = mgr.clone();
            within("cancel", move || mgr.cancel(view.op_id))
        };
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Cancelled);
    }

    #[test]
    fn cancelling_a_plan_means_it_never_runs() {
        let (_, mgr) = manager();
        let runs = Arc::new(AtomicUsize::new(0));
        let view = mgr.register_plan(parts(PlanClass::Bounded), counting(&runs));
        mgr.cancel(view.op_id).unwrap();
        assert!(matches!(
            mgr.execute(view.op_id, &FakeAuthenticator::new()),
            Err(DesktopError::PlanNotFound { .. })
        ));
        assert_eq!(runs.load(SeqCst), 0);
    }

    // 8. How an executor ends.

    #[test]
    fn an_executor_panic_fails_the_op_and_the_manager_keeps_working() {
        let (_, mgr) = manager();
        let view = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(|r, _| {
                r.report(stage(1));
                panic!("executor bug")
            }),
        );
        mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Failed);
        let replay = mgr.subscribe(view.op_id, VecSink::new("main")).unwrap();
        assert_eq!(replay[0], staged(1));
        match replay.last() {
            Some(OpEvent::Failed { error }) => {
                assert_eq!(error.code.as_deref(), Some(errors::INTERNAL));
                assert!(error.message.contains("executor bug"), "{}", error.message);
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        let next = mgr.register_plan(parts(PlanClass::Bounded), returns(json!(2)));
        mgr.execute(next.op_id, &FakeAuthenticator::new()).unwrap();
        assert_eq!(wait_ended(&mgr, next.op_id), OpState::Finished);
    }

    #[test]
    fn an_executor_result_decides_how_the_op_ends() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let failing = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(|_, _| Err(CoreError::NoActiveTarget)),
        );
        mgr.execute(failing.op_id, &auth).unwrap();
        assert_eq!(wait_ended(&mgr, failing.op_id), OpState::Failed);
        let replay = mgr.subscribe(failing.op_id, VecSink::new("main")).unwrap();
        match replay.last() {
            Some(OpEvent::Failed { error }) => {
                assert_eq!(error.code.as_deref(), Some("apprafter::target::no_active"))
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        let stopped = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(|_, _| Err(CoreError::Cancelled)),
        );
        mgr.execute(stopped.op_id, &auth).unwrap();
        assert_eq!(wait_ended(&mgr, stopped.op_id), OpState::Cancelled);
        let done = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(|_, _| complete(json!(thread::current().name()))),
        );
        mgr.execute(done.op_id, &auth).unwrap();
        assert_eq!(wait_ended(&mgr, done.op_id), OpState::Finished);
        let replay = mgr.subscribe(done.op_id, VecSink::new("main")).unwrap();
        assert_eq!(
            replay,
            vec![completed(json!(format!("op-{}", done.op_id.0)))],
            "the executor ran on its own named thread"
        );
    }

    // 9. Listing, counting, quitting.

    #[test]
    fn list_shows_operations_newest_first_and_running_counts_them() {
        let (clock, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let (open_b, gate_b) = gate();
        let (open_a, gate_a) = gate();
        // Registered first, started second: newest is by start, not by id.
        let b = mgr.register_plan(
            PlanParts::new(PlanClass::Bounded, "second", "run"),
            Box::new(move |_, _| {
                gate_b.wait();
                complete(json!(null))
            }),
        );
        let a = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(move |_, _| {
                gate_a.wait();
                complete(json!(null))
            }),
        );
        mgr.register_plan(parts(PlanClass::Bounded), returns(json!(0)));
        mgr.execute(a.op_id, &auth).unwrap();
        clock.advance(5);
        mgr.execute(b.op_id, &auth).unwrap();
        let list = mgr.list();
        assert_eq!(
            list.iter().map(|s| s.op_id).collect::<Vec<_>>(),
            vec![b.op_id, a.op_id]
        );
        assert_eq!(list[0].title, "second");
        assert_eq!(list[0].target, None);
        assert_eq!(list[0].started_at_ms, T0 + 5);
        assert_eq!(list[1].title, "Remove target prod");
        assert_eq!(list[1].target.as_deref(), Some("prod"));
        assert_eq!(list[1].started_at_ms, T0);
        assert!(list.iter().all(|s| s.state == OpState::Running));
        assert_eq!(mgr.running(), 2);
        open_a.send(()).unwrap();
        assert_eq!(wait_ended(&mgr, a.op_id), OpState::Finished);
        assert_eq!(mgr.running(), 1);
        assert_eq!(state_of(&mgr, a.op_id), Some(OpState::Finished));
        open_b.send(()).unwrap();
        wait_ended(&mgr, b.op_id);
        assert_eq!(mgr.running(), 0);
    }

    #[test]
    fn discard_forgets_an_ended_op_or_a_plan_but_never_a_running_op() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let done = mgr.register_plan(parts(PlanClass::Bounded), returns(json!(1)));
        mgr.execute(done.op_id, &auth).unwrap();
        wait_ended(&mgr, done.op_id);
        mgr.discard(done.op_id);
        assert_eq!(state_of(&mgr, done.op_id), None);
        assert!(matches!(
            mgr.subscribe(done.op_id, VecSink::new("main")),
            Err(DesktopError::PlanNotFound { .. })
        ));

        let runs = Arc::new(AtomicUsize::new(0));
        let plan = mgr.register_plan(parts(PlanClass::Bounded), counting(&runs));
        mgr.discard(plan.op_id);
        assert!(matches!(
            mgr.execute(plan.op_id, &auth),
            Err(DesktopError::PlanNotFound { .. })
        ));
        assert_eq!(runs.load(SeqCst), 0);

        let (open, gate) = gate();
        let running = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(move |_, _| {
                gate.wait();
                complete(json!(null))
            }),
        );
        mgr.execute(running.op_id, &auth).unwrap();
        mgr.discard(running.op_id);
        assert_eq!(state_of(&mgr, running.op_id), Some(OpState::Running));
        open.send(()).unwrap();
        assert_eq!(wait_ended(&mgr, running.op_id), OpState::Finished);
    }

    /// Waits until its token trips, then reports it was cancelled.
    fn cooperative() -> Executor {
        Box::new(|_, token| {
            let deadline = Instant::now() + LONG;
            while !token.is_cancelled() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            token.check()?;
            complete(json!("never cancelled"))
        })
    }

    #[test]
    fn cancel_all_and_wait_is_true_when_everything_ended_in_time() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let ids: Vec<OpId> = (0..3)
            .map(|_| {
                let view = mgr.register_plan(parts(PlanClass::Bounded), cooperative());
                mgr.execute(view.op_id, &auth).unwrap()
            })
            .collect();
        assert_eq!(mgr.running(), 3);
        let all_ended = {
            let mgr = mgr.clone();
            within("cancel_all_and_wait", move || mgr.cancel_all_and_wait(LONG))
        };
        assert!(all_ended);
        assert_eq!(mgr.running(), 0);
        for id in ids {
            assert_eq!(state_of(&mgr, id), Some(OpState::Cancelled));
        }
    }

    #[test]
    fn cancel_all_and_wait_gives_up_on_a_stubborn_op_at_the_bound() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let polite = mgr.register_plan(parts(PlanClass::Bounded), cooperative());
        mgr.execute(polite.op_id, &auth).unwrap();
        let (open, gate) = gate();
        let stubborn = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(move |_, _| {
                gate.wait();
                complete(json!("ignored the token"))
            }),
        );
        mgr.execute(stubborn.op_id, &auth).unwrap();
        let bound = Duration::from_millis(200);
        let started = Instant::now();
        let all_ended = {
            let mgr = mgr.clone();
            within("cancel_all_and_wait", move || {
                mgr.cancel_all_and_wait(bound)
            })
        };
        let waited = started.elapsed();
        assert!(!all_ended);
        assert!(waited >= bound, "returned after {waited:?}");
        assert!(waited < Duration::from_secs(5), "returned after {waited:?}");
        // The bound says nothing about the polite one: it ends once its thread gets to it.
        assert_eq!(wait_ended(&mgr, polite.op_id), OpState::Cancelled);
        assert_eq!(mgr.running(), 1);
        open.send(()).unwrap();
        assert_eq!(wait_ended(&mgr, stubborn.op_id), OpState::Finished);
    }

    // 10. Pages.

    #[test]
    fn dropping_a_webviews_subscribers_leaves_the_others() {
        let (_, mgr) = manager();
        let (half_tx, half_rx) = mpsc::channel();
        let (open, gate) = gate();
        let view = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(move |r, _| {
                r.report(stage(1));
                half_tx.send(()).unwrap();
                gate.wait();
                r.report(stage(2));
                complete(json!(null))
            }),
        );
        let main = VecSink::new("main");
        let other = VecSink::new("other");
        mgr.subscribe(view.op_id, main.clone()).unwrap();
        mgr.subscribe(view.op_id, other.clone()).unwrap();
        mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
        half_rx.recv_timeout(LONG).unwrap();
        mgr.drop_subscribers_of("main");
        open.send(()).unwrap();
        wait_ended(&mgr, view.op_id);
        assert_eq!(main.events(), vec![staged(1)]);
        assert_eq!(
            other.events(),
            vec![staged(1), staged(2), completed(json!(null))]
        );
    }

    // The ticker and the lock order.

    #[test]
    fn the_ticker_sends_output_that_waited_long_enough() {
        let (clock, mgr) = manager();
        let (said_tx, said_rx) = mpsc::channel();
        let (open, gate) = gate();
        let view = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(move |r, _| {
                r.report(Event::Output {
                    stream: Stream::Stdout,
                    bytes: b"hi".to_vec(),
                });
                said_tx.send(()).unwrap();
                gate.wait();
                complete(json!(null))
            }),
        );
        let sink = VecSink::new("main");
        mgr.subscribe(view.op_id, sink.clone()).unwrap();
        mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
        said_rx.recv_timeout(LONG).unwrap();
        mgr.flush_due();
        assert_eq!(sink.events(), vec![], "not due yet");
        clock.advance(FLUSH_AGE_MS);
        {
            let mgr = mgr.clone();
            within("flush_due", move || mgr.flush_due());
        }
        assert_eq!(sink.events(), vec![stdout("hi")]);
        open.send(()).unwrap();
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Finished);
    }

    #[test]
    fn the_ticker_racing_a_chatty_op_never_deadlocks_or_loses_output() {
        const LINES: usize = 20_000;
        let (clock, mgr) = manager();
        let (ticking_tx, ticking_rx) = mpsc::channel::<()>();
        let view = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(move |r, _| {
                let _ = ticking_rx.recv_timeout(LONG);
                for _ in 0..LINES {
                    r.report(Event::Output {
                        stream: Stream::Stdout,
                        bytes: b"line\n".to_vec(),
                    });
                }
                complete(json!(null))
            }),
        );
        let sink = VecSink::new("main");
        mgr.subscribe(view.op_id, sink.clone()).unwrap();
        mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
        let ticks = {
            let mgr = mgr.clone();
            within("a ticker racing an operation's output", move || {
                let mut ticks = 0usize;
                loop {
                    clock.advance(FLUSH_AGE_MS);
                    mgr.flush_due();
                    ticks += 1;
                    if ticks == 1 {
                        ticking_tx.send(()).unwrap();
                    }
                    if mgr.running() == 0 {
                        return ticks;
                    }
                }
            })
        };
        let events = sink.events();
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                OpEvent::Output { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            text == "line\n".repeat(LINES),
            "{} bytes arrived of {}",
            text.len(),
            LINES * 5
        );
        assert_eq!(events.last(), Some(&completed(json!(null))));
        println!("{ticks} ticks, {} output events", events.len() - 1);
    }

    // 11. What a closed prompt, a busy one and a broken one leave.

    /// An OS prompt that stays open until the test answers it, whatever its token does.
    struct HeldPrompt {
        opened: Mutex<mpsc::Sender<()>>,
        answer: Mutex<mpsc::Receiver<AuthOutcome>>,
    }

    /// The prompt, then where it says it opened, then where the test answers it.
    fn held_prompt() -> (
        Arc<HeldPrompt>,
        mpsc::Receiver<()>,
        mpsc::Sender<AuthOutcome>,
    ) {
        let (opened_tx, opened) = mpsc::channel();
        let (answer, answer_rx) = mpsc::channel();
        let prompt = Arc::new(HeldPrompt {
            opened: Mutex::new(opened_tx),
            answer: Mutex::new(answer_rx),
        });
        (prompt, opened, answer)
    }

    impl Authenticator for HeldPrompt {
        fn info(&self) -> AuthInfo {
            FakeAuthenticator::new().info()
        }

        fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
            let _ = self.opened.lock().unwrap().send(());
            // A test that failed drops the sender: answer at once rather than hang.
            self.answer
                .lock()
                .unwrap()
                .recv_timeout(LONG)
                .unwrap_or(AuthOutcome::Cancelled {
                    by: CancelledBy::App,
                })
        }
    }

    /// Starts `execute` on a thread of its own; its result arrives on the receiver.
    fn execute_in_background(
        mgr: &Arc<OperationManager>,
        id: OpId,
        auth: Arc<dyn Authenticator>,
    ) -> mpsc::Receiver<Result<OpId, DesktopError>> {
        let (tx, rx) = mpsc::channel();
        let mgr = mgr.clone();
        thread::spawn(move || {
            let _ = tx.send(mgr.execute(id, &*auth));
        });
        rx
    }

    /// Records that it ran.
    fn flags(ran: &Arc<AtomicBool>) -> Executor {
        let ran = ran.clone();
        Box::new(move |_, _| {
            ran.store(true, SeqCst);
            complete(json!(null))
        })
    }

    #[test]
    fn a_prompt_closed_by_a_lock_a_cancel_or_a_quit_refuses_its_plan_before_the_dialog_closes() {
        type Closer = fn(&OperationManager, OpId);
        let closers: [(&str, Closer); 3] = [
            ("lock", |mgr, _| mgr.drop_all_plans()),
            ("cancel", |mgr, id| mgr.cancel(id).unwrap()),
            ("quit", |mgr, _| assert!(mgr.cancel_all_and_wait(LONG))),
        ];
        for (name, close) in closers {
            let (_, mgr) = manager();
            let (auth, opened, answer) = held_prompt();
            let ran = Arc::new(AtomicBool::new(false));
            let view = mgr.register_plan(parts(PlanClass::Destructive), flags(&ran));
            let result = execute_in_background(&mgr, view.op_id, auth);
            opened.recv_timeout(LONG).expect("the prompt opened");
            // Closing the prompt trips its token on a thread of its own. Holding that thread
            // back makes the OS answer yes after `close` returned and before the dialog
            // closed: the window a scheduler opens only sometimes.
            let held = {
                let mgr = mgr.clone();
                within(name, move || test_trips::held(|| close(&mgr, view.op_id)).1)
            };
            assert_eq!(held.len(), 1, "{name} closes the prompt");
            answer.send(AuthOutcome::Verified).unwrap();
            let result = result.recv_timeout(LONG).expect("execute returned");
            assert!(
                matches!(result, Err(DesktopError::AuthCancelled)),
                "{name}: {result:?}"
            );
            assert!(!ran.load(SeqCst), "the plan ran after {name} returned");
            assert!(mgr.list().is_empty(), "{name}");
            assert!(matches!(
                mgr.execute(view.op_id, &FakeAuthenticator::new()),
                Err(DesktopError::PlanNotFound { .. })
            ));
            for token in held {
                token.cancel();
            }
        }
    }

    #[test]
    fn a_busy_prompt_asked_nothing_so_the_plan_waits_with_its_expiry() {
        let (clock, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let view = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
        let sink = VecSink::new("main");
        mgr.subscribe(view.op_id, sink.clone()).unwrap();
        auth.then(AuthOutcome::Busy);
        let err = mgr.execute(view.op_id, &auth).unwrap_err();
        assert!(matches!(err, DesktopError::AuthBusy), "{err:?}");
        let help = err.to_ui().help.unwrap_or_default();
        assert!(help.contains("try again"), "{help}");
        assert_eq!(runs.load(SeqCst), 0);
        assert!(mgr.list().is_empty());
        // The same id, and the page that followed the plan follows it still.
        assert_eq!(mgr.execute(view.op_id, &auth).unwrap(), view.op_id);
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Finished);
        assert_eq!(runs.load(SeqCst), 1);
        assert_eq!(auth.asked().len(), 2);
        assert_eq!(sink.events(), vec![completed(json!(null))]);

        // A busy answer does not renew the plan.
        let later = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
        clock.advance(PLAN_TTL_MS / 2);
        auth.then(AuthOutcome::Busy);
        assert!(matches!(
            mgr.execute(later.op_id, &auth),
            Err(DesktopError::AuthBusy)
        ));
        clock.advance(PLAN_TTL_MS / 2 + 1);
        let expired = mgr.execute(later.op_id, &auth);
        assert!(
            matches!(expired, Err(DesktopError::PlanExpired { op_id }) if op_id == later.op_id),
            "{expired:?}"
        );
        assert_eq!(runs.load(SeqCst), 1);
    }

    /// The OS prompt broke.
    struct PanickingPrompt;

    impl Authenticator for PanickingPrompt {
        fn info(&self) -> AuthInfo {
            FakeAuthenticator::new().info()
        }

        fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
            panic!("the OS prompt broke")
        }
    }

    #[test]
    fn a_panicking_prompt_spends_the_plan_and_leaves_no_prompt_behind() {
        let (_, mgr) = manager();
        let runs = Arc::new(AtomicUsize::new(0));
        let view = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            mgr.execute(view.op_id, &PanickingPrompt)
        }));
        let err = result
            .expect("the prompt's panic stays inside execute")
            .unwrap_err();
        assert_eq!(
            err.to_ui().code.as_deref(),
            Some(errors::INTERNAL),
            "{err:?}"
        );
        assert!(err.to_string().contains("the OS prompt broke"), "{err}");
        // Nothing is left: no prompt a page could follow or a cancel could find.
        assert!(matches!(
            mgr.subscribe(view.op_id, VecSink::new("main")),
            Err(DesktopError::PlanNotFound { .. })
        ));
        assert!(matches!(
            mgr.cancel(view.op_id),
            Err(DesktopError::PlanNotFound { .. })
        ));
        assert!(matches!(
            mgr.execute(view.op_id, &FakeAuthenticator::new()),
            Err(DesktopError::PlanNotFound { .. })
        ));
        assert_eq!(runs.load(SeqCst), 0);
        assert!(mgr.list().is_empty());
    }

    // 12. Time to live, on the clock that never steps back.

    #[test]
    fn a_plan_that_expired_while_its_prompt_was_open_never_runs() {
        let (clock, mgr) = manager();
        let (auth, opened, answer) = held_prompt();
        let ran = Arc::new(AtomicBool::new(false));
        let view = mgr.register_plan(parts(PlanClass::Destructive), flags(&ran));
        let result = execute_in_background(&mgr, view.op_id, auth);
        opened.recv_timeout(LONG).expect("the prompt opened");
        clock.advance(PLAN_TTL_MS + 1);
        answer.send(AuthOutcome::Verified).unwrap();
        let result = result.recv_timeout(LONG).expect("execute returned");
        assert!(
            matches!(result, Err(DesktopError::PlanExpired { op_id }) if op_id == view.op_id),
            "{result:?}"
        );
        assert!(!ran.load(SeqCst));
        assert!(mgr.list().is_empty());
        assert!(matches!(
            mgr.execute(view.op_id, &FakeAuthenticator::new()),
            Err(DesktopError::PlanNotFound { .. })
        ));
    }

    #[test]
    fn the_time_to_live_runs_on_the_monotonic_clock_whatever_the_wall_clock_does() {
        let (clock, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let runs = Arc::new(AtomicUsize::new(0));
        // The TTL passes as the wall clock steps back to where it was: expired all the same.
        let stale = mgr.register_plan(parts(PlanClass::Bounded), counting(&runs));
        clock.advance(PLAN_TTL_MS + 1);
        clock.set_wall(T0);
        let result = mgr.execute(stale.op_id, &auth);
        assert!(
            matches!(result, Err(DesktopError::PlanExpired { .. })),
            "{result:?}"
        );
        // The wall clock jumps a day ahead while no time passes: the plan is as young as it
        // was, for `execute` and for the sweep alike. The view shows wall time.
        let fresh = mgr.register_plan(parts(PlanClass::Bounded), counting(&runs));
        assert_eq!(fresh.expires_at_ms, T0 + PLAN_TTL_MS);
        clock.set_wall(T0 + 24 * 60 * 60 * 1000);
        mgr.flush_due();
        mgr.register_plan(parts(PlanClass::Bounded), returns(json!(0)));
        mgr.execute(fresh.op_id, &auth).unwrap();
        assert_eq!(wait_ended(&mgr, fresh.op_id), OpState::Finished);
        assert_eq!(runs.load(SeqCst), 1);
    }

    // 13. Where a plan is dropped.

    /// Calls back into the manager when it is dropped, as what an executor captured may: a
    /// drop under the manager's lock deadlocks.
    struct CallsBackOnDrop {
        mgr: Weak<OperationManager>,
        dropped: Arc<AtomicBool>,
    }

    impl Drop for CallsBackOnDrop {
        fn drop(&mut self) {
            if let Some(mgr) = self.mgr.upgrade() {
                mgr.running();
            }
            self.dropped.store(true, SeqCst);
        }
    }

    fn calls_back_on_drop(mgr: &Arc<OperationManager>, dropped: &Arc<AtomicBool>) -> Executor {
        let captured = CallsBackOnDrop {
            mgr: Arc::downgrade(mgr),
            dropped: dropped.clone(),
        };
        Box::new(move |_, _| {
            let _captured = &captured;
            complete(json!(null))
        })
    }

    #[test]
    fn the_ticker_sweeps_an_abandoned_plan_with_no_new_plan_registered() {
        let (clock, mgr) = manager();
        let dropped = Arc::new(AtomicBool::new(false));
        let view = mgr.register_plan(
            parts(PlanClass::Bounded),
            calls_back_on_drop(&mgr, &dropped),
        );
        clock.advance(2 * PLAN_TTL_MS);
        mgr.flush_due();
        assert!(!dropped.load(SeqCst), "kept for one TTL past its expiry");
        clock.advance(1);
        {
            let mgr = mgr.clone();
            within("flush_due", move || mgr.flush_due());
        }
        assert!(dropped.load(SeqCst), "what the executor captured is freed");
        assert!(matches!(
            mgr.execute(view.op_id, &FakeAuthenticator::new()),
            Err(DesktopError::PlanNotFound { .. })
        ));
    }

    #[test]
    fn a_plan_is_dropped_outside_the_managers_lock_wherever_it_goes() {
        type Path = fn(&Arc<OperationManager>, &ManualClock, OpId);
        let paths: [(&str, Path); 7] = [
            ("the sweep in register_plan", |mgr, clock, _| {
                clock.advance(2 * PLAN_TTL_MS + 1);
                mgr.register_plan(parts(PlanClass::Bounded), returns(json!(0)));
            }),
            ("the sweep in flush_due", |mgr, clock, _| {
                clock.advance(2 * PLAN_TTL_MS + 1);
                mgr.flush_due();
            }),
            ("an expired execute", |mgr, clock, id| {
                clock.advance(PLAN_TTL_MS + 1);
                let result = mgr.execute(id, &FakeAuthenticator::new());
                assert!(matches!(result, Err(DesktopError::PlanExpired { .. })));
            }),
            ("a refused prompt", |mgr, _, id| {
                let auth = FakeAuthenticator::new();
                auth.then(AuthOutcome::Cancelled {
                    by: CancelledBy::User,
                });
                let result = mgr.execute(id, &auth);
                assert!(matches!(result, Err(DesktopError::AuthCancelled)));
            }),
            ("cancel", |mgr, _, id| mgr.cancel(id).unwrap()),
            ("discard", |mgr, _, id| mgr.discard(id)),
            ("a lock", |mgr, _, _| mgr.drop_all_plans()),
        ];
        for (name, path) in paths {
            let (clock, mgr) = manager();
            let dropped = Arc::new(AtomicBool::new(false));
            let view = mgr.register_plan(
                parts(PlanClass::Destructive),
                calls_back_on_drop(&mgr, &dropped),
            );
            {
                let mgr = mgr.clone();
                within(name, move || path(&mgr, &clock, view.op_id));
            }
            assert!(dropped.load(SeqCst), "{name} let the plan go");
        }
    }

    // 14. How an operation reports its end, whatever happens.

    #[test]
    fn a_cancelled_executor_reports_what_it_cleaned_and_what_it_left() {
        let (_, mgr) = manager();
        let view = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(|_, _| {
                Ok(Outcome::Cancelled {
                    cleaned: vec!["pod/apprafter-backup-helper".into()],
                    left: vec!["restic lock on s3:backups/prod".into()],
                })
            }),
        );
        let sink = VecSink::new("main");
        mgr.subscribe(view.op_id, sink.clone()).unwrap();
        mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Cancelled);
        let finished = OpEvent::Finished {
            outcome: Outcome::Cancelled {
                cleaned: vec!["pod/apprafter-backup-helper".into()],
                left: vec!["restic lock on s3:backups/prod".into()],
            },
        };
        assert_eq!(sink.events(), vec![finished.clone()]);
        assert_eq!(
            mgr.subscribe(view.op_id, VecSink::new("main")).unwrap(),
            vec![finished]
        );
    }

    /// A [`ManualClock`] whose next wall reading panics once armed.
    struct PanicsWhenArmed {
        clock: ManualClock,
        armed: AtomicBool,
    }

    impl Clock for PanicsWhenArmed {
        fn now_ms(&self) -> u64 {
            if self.armed.swap(false, SeqCst) {
                panic!("the clock broke");
            }
            self.clock.now_ms()
        }

        fn monotonic_ms(&self) -> u64 {
            self.clock.monotonic_ms()
        }
    }

    #[test]
    fn a_panic_after_the_executor_returned_still_ends_the_op_and_wakes_a_quit() {
        let clock = Arc::new(PanicsWhenArmed {
            clock: ManualClock::at(T0),
            armed: AtomicBool::new(false),
        });
        let mgr = OperationManager::new(clock.clone());
        let view = mgr.register_plan(parts(PlanClass::Bounded), {
            let clock = clock.clone();
            Box::new(move |_, token| {
                let deadline = Instant::now() + LONG;
                while !token.is_cancelled() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(1));
                }
                // The reporter's `finish` reads the clock first: the op's thread panics
                // there, after the executor returned.
                clock.armed.store(true, SeqCst);
                complete(json!("finished anyway"))
            })
        });
        let sink = VecSink::new("main");
        mgr.subscribe(view.op_id, sink.clone()).unwrap();
        mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
        let all_ended = {
            let mgr = mgr.clone();
            within("a quit waiting for the op", move || {
                mgr.cancel_all_and_wait(LONG)
            })
        };
        assert!(all_ended);
        assert_eq!(mgr.running(), 0);
        assert_eq!(state_of(&mgr, view.op_id), Some(OpState::Failed));
        let events = sink.events();
        match events.as_slice() {
            [OpEvent::Failed { error }] => {
                assert_eq!(error.code.as_deref(), Some(errors::INTERNAL), "{error:?}")
            }
            other => panic!("expected one Failed, got {other:?}"),
        }
        assert_eq!(
            mgr.subscribe(view.op_id, VecSink::new("main")).unwrap(),
            events
        );
    }

    #[test]
    fn an_op_whose_thread_cannot_start_ends_failed_and_its_pages_hear_it() {
        let (_, mgr) = manager();
        let runs = Arc::new(AtomicUsize::new(0));
        let view = mgr.register_plan(parts(PlanClass::Bounded), counting(&runs));
        let sink = VecSink::new("main");
        mgr.subscribe(view.op_id, sink.clone()).unwrap();
        test_spawn::fail_next();
        let err = mgr
            .execute(view.op_id, &FakeAuthenticator::new())
            .unwrap_err();
        assert_eq!(
            err.to_ui().code.as_deref(),
            Some(errors::INTERNAL),
            "{err:?}"
        );
        let events = sink.events();
        match events.as_slice() {
            [OpEvent::Failed { error }] => {
                assert_eq!(error.code.as_deref(), Some(errors::INTERNAL), "{error:?}")
            }
            other => panic!("expected one Failed, got {other:?}"),
        }
        assert_eq!(state_of(&mgr, view.op_id), Some(OpState::Failed));
        assert_eq!(mgr.running(), 0);
        assert_eq!(runs.load(SeqCst), 0);
        assert_eq!(
            mgr.subscribe(view.op_id, VecSink::new("main")).unwrap(),
            events
        );
    }

    // 15. Bounded memory.

    #[test]
    fn at_most_64_ended_ops_are_kept_and_the_first_to_end_goes_first() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let (open, gate) = gate();
        // Started first, ended last.
        let long = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(move |_, _| {
                gate.wait();
                complete(json!("long"))
            }),
        );
        mgr.execute(long.op_id, &auth).unwrap();
        let run_one = |mgr: &Arc<OperationManager>| {
            let view = mgr.register_plan(parts(PlanClass::Bounded), returns(json!(0)));
            mgr.execute(view.op_id, &auth).unwrap();
            assert_eq!(wait_ended(mgr, view.op_id), OpState::Finished);
            view.op_id
        };
        let ids: Vec<OpId> = (0..ENDED_KEPT + 3).map(|_| run_one(&mgr)).collect();
        let listed = |mgr: &OperationManager| -> HashSet<OpId> {
            mgr.list().into_iter().map(|s| s.op_id).collect()
        };
        let kept = listed(&mgr);
        assert_eq!(kept.len(), ENDED_KEPT + 1, "64 ended and the running one");
        assert!(kept.contains(&long.op_id), "a running op is never evicted");
        for id in &ids[..3] {
            assert!(!kept.contains(id), "{id:?}");
            assert!(matches!(
                mgr.subscribe(*id, VecSink::new("main")),
                Err(DesktopError::PlanNotFound { .. })
            ));
        }
        assert!(ids[3..].iter().all(|id| kept.contains(id)));
        // A discarded op frees its place: the next one to end evicts nothing.
        mgr.discard(ids[3]);
        let next = run_one(&mgr);
        let kept = listed(&mgr);
        assert_eq!(kept.len(), ENDED_KEPT + 1);
        assert!(kept.contains(&ids[4]) && kept.contains(&next));
        // The long op ends last, so the oldest ended goes, not the oldest started.
        open.send(()).unwrap();
        assert_eq!(wait_ended(&mgr, long.op_id), OpState::Finished);
        let kept = listed(&mgr);
        assert_eq!(kept.len(), ENDED_KEPT);
        assert!(kept.contains(&long.op_id));
        assert!(!kept.contains(&ids[4]));
    }
}
