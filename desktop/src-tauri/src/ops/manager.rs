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
//! and then exactly the events after it. Every subscription has its own [`SubscriptionId`], so
//! a page can [`unsubscribe`](OperationManager::unsubscribe) exactly the one it made.
//!
//! An ended operation keeps its summary and replay until the webview
//! [`discard`](OperationManager::discard)s it after showing the result, or until
//! [`ENDED_KEPT`] operations ended after it. A plan that never runs — its prompt refused,
//! expired, swept, dropped by a lock or by a quit — sends the pages that followed it one
//! `Failed` saying why, so none of them waits for an end that never comes. A gesture that
//! failed (a wrong password) is not such an end: the plan waits for the owner's next try
//! ([`OperationManager::execute`] says why), and its pages hear nothing until it runs or ends.
//!
//! A plan leaves the manager's maps under its lock and is dropped after the lock is released:
//! what an executor captured may do anything when it goes, calling back into the manager
//! included.
//!
//! Quitting [`close`](OperationManager::close)s the manager first: from then on no operation
//! starts, decided under the same lock hold that would start it, so nothing begins between the
//! quit's [`cancel_all_and_wait`](OperationManager::cancel_all_and_wait) and the exit.

use std::collections::{HashMap, VecDeque};
use std::mem;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use apprafter_core::{
    CancellationToken, CoreError, CoreResult, Outcome, PlanClass, PlannedChange, Reporter, UiError,
};
use apprafter_desktop_ipc::{
    AuthOutcome, OpEvent, OpId, OpState, OpSummary, PlanView, Subscribed, SubscriptionId,
};

use zeroize::Zeroizing;

use super::replay::REPLAY_CAP;
use super::{panic_message, trip, Clock, OpReporter, ReplayBuffer, Stamp};
use crate::auth::{AuthPurpose, Authenticator, PasswordAnswer};
use crate::errors::{DesktopError, Refusal};

/// How long a plan can wait for `execute`, measured by [`elapsed_ms`](super::elapsed_ms):
/// a suspend counts, a wall clock stepped back does not.
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
    /// into the manager. Nor into the [`LockMachine`](crate::lock::LockMachine): a lock sends
    /// the pages of the plans it drops their final event from the machine's hook, under the
    /// machine's lock and then this one, and the locks are never taken the other way round.
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
    next_subscription: u64,
    /// Set once by [`OperationManager::close`]: no operation starts after it.
    closing: bool,
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

    /// Take out every plan one [`PLAN_TTL_MS`] past its expiry, its pages told it expired,
    /// for the caller to drop once it has let go of the lock.
    fn sweep(&mut self, clock: &dyn Clock) -> Vec<Pending> {
        self.pending
            .extract_if(|_, plan| plan.registered.elapsed_ms(clock) > 2 * PLAN_TTL_MS)
            .map(|(op_id, mut plan)| {
                refuse(&mut plan.sinks, &DesktopError::PlanExpired { op_id });
                plan
            })
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
    /// When it was registered; one [`PLAN_TTL_MS`] after it, `execute` refuses the plan.
    registered: Stamp,
    exec: Executor,
    /// Pages subscribed before `execute`; they follow the plan into its operation.
    sinks: Vec<Subscriber>,
}

/// One subscription: the page's sink and the id it ends it with.
struct Subscriber {
    id: SubscriptionId,
    sink: Arc<dyn EventSink>,
}

impl Pending {
    /// Its [`PLAN_TTL_MS`] has passed, on either clock.
    fn expired(&self, clock: &dyn Clock) -> bool {
        self.registered.elapsed_ms(clock) > PLAN_TTL_MS
    }
}

struct Prompt {
    /// Tripping it closes the OS dialog.
    cancel: CancellationToken,
    /// Set under the manager's lock by a lock, a cancel or a quit: the plan is refused,
    /// whatever the OS answers. The token trips later, on a thread of its own, so this flag —
    /// not the token — decides; a `Verified` that arrives before the dialog closed is
    /// refused too.
    refused: bool,
    sinks: Vec<Subscriber>,
}

struct Op {
    title: String,
    target: Option<String>,
    started_at_ms: u64,
    status: Status,
    replay: ReplayBuffer,
    /// Pages that receive what comes next; empty once the operation ended.
    sinks: Vec<Subscriber>,
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
    Run(Vec<Subscriber>),
    /// The plan waits for another `execute`, as it was, and the caller is told why (with what
    /// the OS said): nothing was asked (busy), or the owner was not verified and may try again
    /// (failed). The pages that followed the prompt follow the plan again and hear nothing: it
    /// has not ended.
    Again(Refusal, Vec<Subscriber>),
    /// Refused, for the reason given (with what the OS said); the pages that followed the
    /// prompt are told it.
    Refuse(Refusal, Vec<Subscriber>),
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
    /// `expires_at_ms` is wall time, for display; the plan itself expires once one
    /// [`PLAN_TTL_MS`] has passed on either clock ([`elapsed_ms`](super::elapsed_ms)): a
    /// suspend expires it, a wall clock stepped back does not keep it alive.
    ///
    /// A plan lives until it is executed, cancelled or discarded, the app locks, or one
    /// [`PLAN_TTL_MS`] after it expired: swept here and on every [`sweep`](Self::sweep)
    /// tick, so an abandoned plan does not keep what its executor captured. Until then
    /// `execute` answers `PlanExpired`.
    pub fn register_plan(&self, parts: PlanParts, exec: Executor) -> PlanView {
        let registered = Stamp::now(&*self.clock);
        let expires_at_ms = registered.wall_ms.saturating_add(PLAN_TTL_MS);
        let needs_gesture = parts.requires_gesture || parts.class == PlanClass::Destructive;
        let gesture = needs_gesture.then(|| AuthPurpose::Confirm {
            target: parts.target.clone(),
            verb: parts.verb,
        });
        let mut inner = self.lock();
        let swept = inner.sweep(&*self.clock);
        inner.next_id += 1;
        let op_id = OpId(inner.next_id);
        inner.pending.insert(
            op_id,
            Pending {
                title: parts.title.clone(),
                target: parts.target.clone(),
                gesture,
                registered,
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

    /// Run a plan, once: whatever happens here, the plan is spent — except when the owner may
    /// simply try again, and the plan then waits for another try, under the same id, with the
    /// time it had left: on `AuthBusy`, when another prompt was open and nothing was asked, and
    /// on `AuthFailed`, when the owner was not verified (a wrong password, a finger not
    /// recognised) or a back-off turned the try away.
    ///
    /// When the plan needs the owner, this asks `auth` and blocks until the prompt answers
    /// (so the caller is a blocking thread, never an async worker). Anything but `Verified`
    /// refuses the plan, and so does a lock, a cancel or a quit that closed the prompt — even
    /// when the OS still answered yes — and so does a prompt that stayed open past the
    /// plan's expiry; the executor then never runs. Otherwise it starts on a thread of its
    /// own, named `op-<id>`, with an 8 MiB stack.
    ///
    /// Why a failed gesture keeps the plan, where a cancelled or unavailable one spends it: a
    /// typo would otherwise send the owner back to plan the change again, and keeping it gives
    /// away nothing the gesture guards. Each try asks the owner again, and what bounds the
    /// tries is the authenticator's — the app's own back-off on the password paths, the OS's
    /// limits on its prompts — not the plan's. Every other rule holds: the plan runs once, its
    /// time to live runs from when it was planned, and a cancel, a lock or a quit drops it. A
    /// cancel is the owner (or the app, or the system) saying no, and an unavailable gesture will
    /// not become available by asking again, so both end the plan.
    ///
    /// Once the manager is [`close`](Self::close)d it refuses with `Closing`: before asking
    /// anything, and again under the lock hold that would start the operation or put a busy
    /// plan back, so a prompt that answers after the quit began starts nothing.
    pub fn execute(
        self: &Arc<Self>,
        id: OpId,
        auth: &dyn Authenticator,
    ) -> Result<OpId, DesktopError> {
        self.execute_with(id, auth, None)
            .map_err(|refusal| *refusal.error)
    }

    /// [`execute`](Self::execute), the gesture checking `password` — the app's own field, where
    /// the OS cannot prompt — instead of opening the OS's prompt
    /// ([`Authenticator::verify_password`]), by the same rules. A refused gesture carries what
    /// the OS said (PAM's messages), to the caller and to the pages that followed the plan. A
    /// plan that needs no gesture runs as it would, the password wiped unread.
    pub fn execute_with(
        self: &Arc<Self>,
        id: OpId,
        auth: &dyn Authenticator,
        password: Option<Zeroizing<String>>,
    ) -> Result<OpId, Refusal> {
        let mut inner = self.lock();
        let plan = inner
            .pending
            .remove(&id)
            .ok_or(DesktopError::PlanNotFound { op_id: id })?;
        if inner.closing {
            return spend(inner, plan, DesktopError::Closing);
        }
        if plan.expired(&*self.clock) {
            return spend(inner, plan, DesktopError::PlanExpired { op_id: id });
        }
        let mut plan = plan;
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
            let answer = panic::catch_unwind(AssertUnwindSafe(|| match password {
                Some(password) => auth.verify_password(&purpose, password, &cancel),
                None => auth.verify(&purpose, &cancel).into(),
            }));
            let expired = plan.expired(&*self.clock);
            inner = self.lock();
            let prompt = inner.prompts.remove(&id);
            match judge(id, answer, prompt, expired) {
                Answer::Run(sinks) => plan.sinks = sinks,
                Answer::Again(refusal, sinks) => {
                    plan.sinks = sinks;
                    // Quitting: nothing may wait for a try that will never come.
                    if inner.closing {
                        return spend(inner, plan, DesktopError::Closing);
                    }
                    inner.pending.insert(id, plan);
                    return Err(refusal);
                }
                Answer::Refuse(refusal, mut sinks) => {
                    fan_out(
                        &mut sinks,
                        &OpEvent::Failed {
                            error: refusal.to_ui(),
                        },
                    );
                    drop(inner);
                    drop((plan, sinks));
                    return Err(refusal);
                }
            }
        }
        // The same lock hold as the insert below: a quit that began while the prompt was
        // open is seen here, and one that begins after it finds the operation running.
        if inner.closing {
            return spend(inner, plan, DesktopError::Closing);
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
            return Err(DesktopError::Internal(message).into());
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
    /// has no events yet, and the sink follows it when it runs — or receives one `Failed`
    /// when it never will: refused, expired, swept or dropped by a lock (a plan the webview
    /// cancels or discards itself sends nothing). An ended operation returns its whole replay
    /// and keeps no sink.
    ///
    /// Every call gets a new [`SubscriptionId`], an ended operation's included, which
    /// [`unsubscribe`](Self::unsubscribe) takes.
    pub fn subscribe(
        &self,
        id: OpId,
        sink: Arc<dyn EventSink>,
    ) -> Result<Subscribed, DesktopError> {
        let mut inner = self.lock();
        let Inner {
            next_subscription,
            pending,
            prompts,
            ops,
            ..
        } = &mut *inner;
        let (sinks, replay) = if let Some(op) = ops.get_mut(&id) {
            let replay = op.replay.snapshot();
            (op.run().is_some().then_some(&mut op.sinks), replay)
        } else if let Some(plan) = pending.get_mut(&id) {
            (Some(&mut plan.sinks), Vec::new())
        } else if let Some(prompt) = prompts.get_mut(&id) {
            (Some(&mut prompt.sinks), Vec::new())
        } else {
            return Err(DesktopError::PlanNotFound { op_id: id });
        };
        *next_subscription += 1;
        let subscription = SubscriptionId(*next_subscription);
        if let Some(sinks) = sinks {
            sinks.push(Subscriber {
                id: subscription,
                sink,
            });
        }
        Ok(Subscribed {
            subscription,
            replay,
        })
    }

    /// End one subscription: its sink receives nothing more. The plan, prompt or operation
    /// `id` names is left as it is. An unknown `id` or `subscription` — the operation ended
    /// or was discarded meanwhile — is no error: there is nothing left to end.
    pub fn unsubscribe(&self, id: OpId, subscription: SubscriptionId) {
        let mut inner = self.lock();
        let Inner {
            pending,
            prompts,
            ops,
            ..
        } = &mut *inner;
        let sinks = pending
            .get_mut(&id)
            .map(|plan| &mut plan.sinks)
            .or_else(|| prompts.get_mut(&id).map(|prompt| &mut prompt.sinks))
            .or_else(|| ops.get_mut(&id).map(|op| &mut op.sinks));
        if let Some(sinks) = sinks {
            sinks.retain(|s| s.id != subscription);
        }
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

    /// Quit has begun: from now on [`execute`](Self::execute) starts nothing and refuses with
    /// `Closing`, telling the pages that followed the plan. Running operations, open prompts and
    /// pending plans are left to the rest of the quit: [`drop_all_plans`](Self::drop_all_plans)
    /// and [`cancel_all_and_wait`](Self::cancel_all_and_wait).
    pub fn close(&self) {
        self.lock().closing = true;
    }

    /// How many executors are running.
    pub fn running(&self) -> usize {
        self.lock().running()
    }

    /// The app locked: no plan made before it may run after it. Every plan goes, the pages
    /// that followed it told `Locked` (`Closing` once the manager is closed: a quit drops the
    /// plans too), and every open prompt is refused before this returns (its dialog closes,
    /// and its `execute` refuses whatever the OS answers and tells its pages
    /// `AuthCancelled`). Running operations go on: they were confirmed before the lock.
    pub fn drop_all_plans(&self) {
        let mut inner = self.lock();
        let why = if inner.closing {
            DesktopError::Closing
        } else {
            DesktopError::Locked
        };
        let mut plans: Vec<Pending> = inner.pending.drain().map(|(_, plan)| plan).collect();
        for plan in &mut plans {
            refuse(&mut plan.sinks, &why);
        }
        let prompts = refuse_prompts(&mut inner.prompts);
        drop(inner);
        drop(plans);
        for token in prompts {
            trip(token, CANCEL_THREAD);
        }
    }

    /// A page reloaded or closed: the sinks that delivered to it go.
    pub fn drop_subscribers_of(&self, webview: &str) {
        self.drop_subscribers_where(|s| s.sink.webview() == webview);
    }

    /// The app locked or unlocked: every subscription ends, on every plan, prompt and
    /// operation, and its sink receives nothing more. A locked page must not receive an
    /// operation's output, and the page cannot unsubscribe while locked (the gate refuses
    /// `op_unsubscribe`): it subscribes again once unlocked, so a sink kept across the lock
    /// would deliver every event twice. The operations run on, and their replays stay whole
    /// for that new subscription.
    pub fn drop_all_subscribers(&self) {
        self.drop_subscribers_where(|_| true);
    }

    /// The sinks dropped here are dropped under the lock, as everywhere: a sink never calls
    /// back into the manager.
    fn drop_subscribers_where(&self, gone: impl Fn(&Subscriber) -> bool) {
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
            sinks.retain(|s| !gone(s));
        }
    }

    /// The output ticker (every [`FLUSH_AGE_MS`](super::reporter::FLUSH_AGE_MS) while an
    /// operation runs): send the output that has waited long enough.
    ///
    /// The reporters are called after the lock is released. A reporter calls its sink,
    /// which takes this lock, under its own lock; holding them the other way round
    /// deadlocks against an operation that is writing.
    pub fn flush_due(&self) {
        let reporters: Vec<Arc<OpReporter>> = self
            .lock()
            .ops
            .values()
            .filter_map(|op| op.run().map(|run| run.reporter.clone()))
            .collect();
        let now = self.clock.now_ms();
        for reporter in reporters {
            reporter.flush_due(now);
        }
    }

    /// The idle ticker (every few seconds, whatever runs): drop every plan one
    /// [`PLAN_TTL_MS`] past its expiry, its pages told it expired, so an abandoned plan does
    /// not keep what its executor captured. The plans are dropped after the lock is released.
    pub fn sweep(&self) {
        let swept = self.lock().sweep(&*self.clock);
        drop(swept);
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

/// Tell the pages that followed a plan that it will never run, and why.
fn refuse(sinks: &mut Vec<Subscriber>, why: &DesktopError) {
    fan_out(sinks, &OpEvent::Failed { error: why.to_ui() });
}

/// `execute` refuses `plan` for `why`: its pages are told under the lock, and the plan is
/// dropped after the lock is released.
fn spend(
    inner: MutexGuard<'_, Inner>,
    mut plan: Pending,
    why: DesktopError,
) -> Result<OpId, Refusal> {
    refuse(&mut plan.sinks, &why);
    drop(inner);
    drop(plan);
    Err(why.into())
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
/// closed some other way; `expired` says the plan's time to live passed while it was open.
/// What the OS said comes with a refusal the OS gave, never with a yes refused afterwards.
/// A failed gesture keeps the plan ([`OperationManager::execute`] says why).
fn judge(
    id: OpId,
    answer: thread::Result<PasswordAnswer>,
    prompt: Option<Prompt>,
    expired: bool,
) -> Answer {
    let (refused, sinks) = prompt.map_or((true, Vec::new()), |p| (p.refused, p.sinks));
    let PasswordAnswer { outcome, messages } = match answer {
        Ok(answer) => answer,
        Err(panic) => {
            let err = DesktopError::Internal(format!(
                "the authentication prompt panicked: {}",
                panic_message(&*panic)
            ));
            return Answer::Refuse(err.into(), sinks);
        }
    };
    let said = |error: DesktopError| match outcome {
        AuthOutcome::Verified => Refusal::from(error),
        _ => Refusal::new(error, messages.clone()),
    };
    // A lock, a cancel or a quit closed it: whatever the OS answered, the plan was dropped.
    if refused {
        return Answer::Refuse(said(DesktopError::AuthCancelled), sinks);
    }
    let err = match outcome {
        // Run, or wait for another try: neither once the plan has expired.
        AuthOutcome::Verified | AuthOutcome::Busy | AuthOutcome::Failed { .. } if expired => {
            DesktopError::PlanExpired { op_id: id }
        }
        AuthOutcome::Verified => return Answer::Run(sinks),
        AuthOutcome::Busy => return Answer::Again(DesktopError::AuthBusy.into(), sinks),
        AuthOutcome::Failed {
            exhausted,
            retry_in_ms,
        } => {
            let failed = DesktopError::AuthFailed {
                exhausted,
                retry_in_ms,
            };
            return Answer::Again(said(failed), sinks);
        }
        AuthOutcome::Cancelled { .. } => DesktopError::AuthCancelled,
        AuthOutcome::Unavailable { reason } => DesktopError::AuthUnavailable { reason },
    };
    Answer::Refuse(said(err), sinks)
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
fn fan_out(sinks: &mut Vec<Subscriber>, event: &OpEvent) {
    sinks.retain(|s| panic::catch_unwind(AssertUnwindSafe(|| s.sink.send(event))).unwrap_or(false));
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
        SubscriptionId, UnavailableReason,
    };
    use serde_json::json;

    use zeroize::Zeroizing;

    use super::{
        test_spawn, EventSink, Executor, OperationManager, PlanParts, ENDED_KEPT, PLAN_TTL_MS,
    };
    use crate::auth::test_os::{self, Call, ScriptedOs};
    use crate::auth::{AuthPurpose, Authenticator, FakeAuthenticator, NoAuthenticator};
    use crate::errors::DesktopError;
    use crate::ops::replay::MESSAGE_CAP;
    use crate::ops::reporter::FLUSH_AGE_MS;
    use crate::ops::test_clock::ManualClock;
    use crate::ops::{test_trips, Clock};

    const T0: u64 = 1_700_000_000_000;
    const DAY: u64 = 24 * 60 * 60 * 1000;
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
    fn a_cancelled_or_unavailable_gesture_spends_the_plan_and_never_runs_it() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let mut cases: Vec<(AuthOutcome, DesktopError)> =
            [CancelledBy::User, CancelledBy::App, CancelledBy::System]
                .into_iter()
                .map(|by| (AuthOutcome::Cancelled { by }, DesktopError::AuthCancelled))
                .collect();
        cases.push((
            AuthOutcome::Unavailable {
                reason: UnavailableReason::NotInteractive,
            },
            DesktopError::AuthUnavailable {
                reason: UnavailableReason::NotInteractive,
            },
        ));
        let runs = Arc::new(AtomicUsize::new(0));
        for (outcome, expected) in cases {
            let view = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
            let sink = VecSink::new("main");
            mgr.subscribe(view.op_id, sink.clone()).unwrap();
            auth.then(outcome);
            let err = mgr.execute(view.op_id, &auth).unwrap_err();
            assert_eq!(err.to_ui(), expected.to_ui(), "{outcome:?}");
            assert_eq!(
                sink.events(),
                [OpEvent::Failed {
                    error: expected.to_ui()
                }],
                "{outcome:?}: the page that followed the plan hears it ended"
            );
            let again = mgr.execute(view.op_id, &auth);
            assert!(
                matches!(again, Err(DesktopError::PlanNotFound { .. })),
                "{outcome:?}: a refused plan is spent, got {again:?}"
            );
        }
        assert_eq!(runs.load(SeqCst), 0);
        assert!(mgr.list().is_empty());
    }

    /// A wrong password or an unrecognised finger: the owner may try again, so the plan waits
    /// under the same id, its expiry unchanged, and the pages that followed it follow it still
    /// (it has not ended). It still runs once.
    #[test]
    fn a_failed_gesture_keeps_the_plan_and_a_retry_with_the_same_id_runs_it() {
        let (clock, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let view = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
        let sink = VecSink::new("main");
        mgr.subscribe(view.op_id, sink.clone()).unwrap();
        let failed = AuthOutcome::Failed {
            exhausted: false,
            retry_in_ms: None,
        };
        auth.then(failed).then(failed);
        for _ in 0..2 {
            clock.advance(1_000);
            let err = mgr.execute(view.op_id, &auth).unwrap_err();
            assert_eq!(
                err.to_ui(),
                DesktopError::AuthFailed {
                    exhausted: false,
                    retry_in_ms: None
                }
                .to_ui()
            );
            assert!(sink.events().is_empty(), "the plan has not ended");
            assert!(mgr.list().is_empty(), "nothing runs");
        }
        assert_eq!(runs.load(SeqCst), 0);
        assert_eq!(mgr.execute(view.op_id, &auth).unwrap(), view.op_id);
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Finished);
        assert_eq!(runs.load(SeqCst), 1);
        assert_eq!(auth.asked().len(), 3, "every try asks the owner");
        assert_eq!(sink.events(), vec![completed(json!(null))]);
        assert!(
            matches!(
                mgr.execute(view.op_id, &auth),
                Err(DesktopError::PlanNotFound { .. })
            ),
            "once run, it is spent"
        );
    }

    /// The back-off's refusal is a failure too: the plan waits, and every try it turns away
    /// says how long it still refuses, until a try after it runs the plan.
    #[test]
    fn an_exhausted_gesture_refuses_each_try_until_the_back_off_ends() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let view = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
        for left in [30_000, 12_000] {
            auth.then(AuthOutcome::Failed {
                exhausted: true,
                retry_in_ms: Some(left),
            });
            let ui = mgr.execute(view.op_id, &auth).unwrap_err().to_ui();
            assert_eq!(ui.code.as_deref(), Some(errors::AUTH_FAILED), "{ui:?}");
            assert_eq!(ui.fields["exhausted"], json!(true));
            assert_eq!(ui.fields["retryInMs"], json!(left));
            assert_eq!(runs.load(SeqCst), 0);
        }
        assert_eq!(mgr.execute(view.op_id, &auth).unwrap(), view.op_id);
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Finished);
        assert_eq!(runs.load(SeqCst), 1);
    }

    /// Kept after a failed gesture, a plan still goes as any pending plan does: on a lock, and
    /// once its time to live has passed.
    #[test]
    fn a_plan_kept_after_a_failed_gesture_still_goes_on_a_lock_and_at_its_expiry() {
        let (clock, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let failed = AuthOutcome::Failed {
            exhausted: false,
            retry_in_ms: None,
        };
        let runs = Arc::new(AtomicUsize::new(0));
        let locked = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
        let sink = VecSink::new("main");
        mgr.subscribe(locked.op_id, sink.clone()).unwrap();
        auth.then(failed);
        mgr.execute(locked.op_id, &auth).unwrap_err();
        mgr.drop_all_plans();
        assert_eq!(
            sink.events(),
            [OpEvent::Failed {
                error: DesktopError::Locked.to_ui()
            }]
        );
        assert!(matches!(
            mgr.execute(locked.op_id, &auth),
            Err(DesktopError::PlanNotFound { .. })
        ));

        let expiring = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
        auth.then(failed);
        mgr.execute(expiring.op_id, &auth).unwrap_err();
        clock.advance(PLAN_TTL_MS + 1);
        let expired = mgr.execute(expiring.op_id, &auth);
        assert!(
            matches!(expired, Err(DesktopError::PlanExpired { op_id }) if op_id == expiring.op_id),
            "{expired:?}"
        );
        assert_eq!(runs.load(SeqCst), 0);
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

    /// What the owner typed into the confirm dialog's field.
    fn typed(password: &str) -> Option<Zeroizing<String>> {
        Some(Zeroizing::new(password.to_owned()))
    }

    #[test]
    fn a_password_given_is_checked_in_place_of_the_os_prompt() {
        let (auth, calls) = test_os::system(
            ScriptedOs::new(FakeAuthenticator::new().info(), AuthOutcome::Verified)
                .saying(AuthOutcome::Verified.into()),
        );
        let (_, mgr) = manager();
        let runs = Arc::new(AtomicUsize::new(0));
        let field = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
        mgr.execute_with(field.op_id, &*auth, typed("hunter2"))
            .unwrap();
        wait_ended(&mgr, field.op_id);
        let prompt = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
        mgr.execute_with(prompt.op_id, &*auth, None).unwrap();
        wait_ended(&mgr, prompt.op_id);
        assert_eq!(
            *calls.lock().unwrap(),
            [
                Call::Password(apprafter_os_auth::Action::Confirm, "hunter2".into(), 1_000),
                Call::Verify(apprafter_os_auth::Action::Confirm),
            ],
            "the field, then the OS's prompt"
        );
        assert_eq!(runs.load(SeqCst), 2);
    }

    /// The confirm dialog's retry: a wrong password keeps the plan, and the right one runs it
    /// under the same id. The page that followed the plan hears nothing of the wrong one (the
    /// plan has not ended), then follows the operation.
    #[test]
    fn a_wrong_password_keeps_the_plan_and_the_right_one_runs_it_under_the_same_id() {
        let auth = FakeAuthenticator::new().with_password("open sesame".to_owned());
        auth.saying(&["Authentication failure"]);
        let (_, mgr) = manager();
        let runs = Arc::new(AtomicUsize::new(0));
        let view = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
        let earlier = VecSink::new("main");
        mgr.subscribe(view.op_id, earlier.clone()).unwrap();
        let refusal = mgr
            .execute_with(view.op_id, &auth, typed("open sesame!"))
            .unwrap_err();
        assert!(
            matches!(
                *refusal.error,
                DesktopError::AuthFailed {
                    exhausted: false,
                    retry_in_ms: None
                }
            ),
            "{refusal:?}"
        );
        assert_eq!(
            refusal.to_ui().fields["messages"],
            json!(["Authentication failure"])
        );
        assert!(earlier.events().is_empty(), "the plan has not ended");
        assert_eq!(runs.load(SeqCst), 0);
        assert_eq!(
            mgr.execute_with(view.op_id, &auth, typed("open sesame"))
                .unwrap(),
            view.op_id
        );
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Finished);
        assert_eq!(runs.load(SeqCst), 1);
        assert_eq!(auth.asked().len(), 2);
        assert_eq!(earlier.events(), vec![completed(json!(null))]);
    }

    /// A password check that ends the plan (here PAM asked for a second secret the field does
    /// not hold) tells every page that followed it, with what PAM said.
    #[test]
    fn a_password_refusal_that_ends_the_plan_says_what_pam_said_to_every_page() {
        let auth = FakeAuthenticator::new().with_password("open sesame".to_owned());
        auth.saying(&["Verification code:"])
            .then(AuthOutcome::Unavailable {
                reason: UnavailableReason::NotInteractive,
            });
        let (_, mgr) = manager();
        let runs = Arc::new(AtomicUsize::new(0));
        let view = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
        let earlier = VecSink::new("main");
        mgr.subscribe(view.op_id, earlier.clone()).unwrap();
        let refusal = mgr
            .execute_with(view.op_id, &auth, typed("open sesame"))
            .unwrap_err();
        assert!(
            matches!(
                *refusal.error,
                DesktopError::AuthUnavailable {
                    reason: UnavailableReason::NotInteractive
                }
            ),
            "{refusal:?}"
        );
        assert_eq!(refusal.messages, ["Verification code:"]);
        assert_eq!(
            earlier.events(),
            [OpEvent::Failed {
                error: refusal.to_ui()
            }],
            "the page that followed the plan hears it with what was said"
        );
        assert!(matches!(
            mgr.execute_with(view.op_id, &auth, typed("open sesame")),
            Err(refusal) if matches!(*refusal.error, DesktopError::PlanNotFound { .. })
        ));
        assert_eq!(runs.load(SeqCst), 0);
        assert_eq!(auth.asked().len(), 1);
    }

    #[test]
    fn a_plan_without_a_gesture_runs_and_its_password_is_never_checked() {
        let auth = FakeAuthenticator::new().with_password("open sesame".to_owned());
        let (_, mgr) = manager();
        let runs = Arc::new(AtomicUsize::new(0));
        let view = mgr.register_plan(parts(PlanClass::Bounded), counting(&runs));
        mgr.execute_with(view.op_id, &auth, typed("wrong")).unwrap();
        wait_ended(&mgr, view.op_id);
        assert_eq!(runs.load(SeqCst), 1);
        assert!(auth.asked().is_empty());
    }

    #[test]
    fn where_the_os_cannot_verify_the_owner_a_destructive_plan_is_refused() {
        let reason = UnavailableReason::NoAgent;
        let (auth, _) = test_os::system(ScriptedOs::new(
            AuthInfo {
                available: false,
                method: None,
                unavailable: Some(reason),
                biometrics_choice: false,
                password_field: false,
            },
            AuthOutcome::Unavailable { reason },
        ));
        let (_, mgr) = manager();
        let runs = Arc::new(AtomicUsize::new(0));
        for password in [None, typed("hunter2")] {
            let view = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
            let refusal = mgr.execute_with(view.op_id, &*auth, password).unwrap_err();
            assert!(
                matches!(*refusal.error, DesktopError::AuthUnavailable { .. }),
                "{refusal:?}"
            );
        }
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
            mgr.subscribe(view.op_id, early.clone()).unwrap().replay,
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
            mgr.subscribe(view.op_id, late.clone()).unwrap().replay,
            vec![staged(1), warning.clone()]
        );
        open.send(()).unwrap();
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Finished);
        let all = vec![staged(1), warning, staged(2), completed(json!("done"))];
        assert_eq!(early.events(), all);
        assert_eq!(late.events(), vec![staged(2), completed(json!("done"))]);
        let after = VecSink::new("main");
        assert_eq!(
            mgr.subscribe(view.op_id, after.clone()).unwrap().replay,
            all
        );
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
        // Where the operation pauses: few enough events that the replay keeps every one.
        const PAUSE: u32 = 1_000;
        assert!((PAUSE as usize) < MESSAGE_CAP);
        let expected: Vec<OpEvent> = (0..N)
            .map(|index| OpEvent::Stage {
                index,
                total: N,
                title: String::new(),
            })
            .chain([completed(json!(N))])
            .collect();
        for _ in 0..8 {
            let (_, mgr) = manager();
            let (paused_tx, paused_rx) = mpsc::channel();
            let (resume, resume_gate) = gate();
            let (half_tx, half_rx) = mpsc::channel();
            let (finish, finish_gate) = gate();
            let view = mgr.register_plan(
                parts(PlanClass::Bounded),
                Box::new(move |r, _| {
                    for index in 0..N {
                        if index == PAUSE {
                            paused_tx.send(()).unwrap();
                            resume_gate.wait();
                        }
                        r.report(Event::Stage {
                            index,
                            total: N,
                            title: String::new(),
                        });
                        if index == (PAUSE + N) / 2 {
                            half_tx.send(()).unwrap();
                        }
                    }
                    finish_gate.wait();
                    complete(json!(N))
                }),
            );
            mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
            // Forced: the operation stopped after exactly PAUSE events, and goes on emitting
            // once this page has subscribed.
            paused_rx.recv_timeout(LONG).unwrap();
            let paused = VecSink::new("main");
            let replay = mgr.subscribe(view.op_id, paused.clone()).unwrap().replay;
            assert_same_events(&replay, &expected[..PAUSE as usize]);
            resume.send(()).unwrap();
            // Sampled: another page subscribes while the burst runs, wherever it lands.
            half_rx.recv_timeout(LONG).unwrap();
            let racing = VecSink::new("main");
            let racing_replay = mgr.subscribe(view.op_id, racing.clone()).unwrap().replay;
            finish.send(()).unwrap();
            wait_ended(&mgr, view.op_id);
            assert_same_events(&paused.events(), &expected[PAUSE as usize..]);
            let all: Vec<OpEvent> = racing_replay.into_iter().chain(racing.events()).collect();
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
        let replay = mgr
            .subscribe(view.op_id, VecSink::new("main"))
            .unwrap()
            .replay;
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
        let replay = mgr
            .subscribe(view.op_id, VecSink::new("main"))
            .unwrap()
            .replay;
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
        let replay = mgr
            .subscribe(failing.op_id, VecSink::new("main"))
            .unwrap()
            .replay;
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
        let replay = mgr
            .subscribe(done.op_id, VecSink::new("main"))
            .unwrap()
            .replay;
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

    #[test]
    fn dropping_every_subscriber_silences_them_all_and_the_op_runs_on_with_its_replay() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let (half_tx, half_rx) = mpsc::channel();
        let (open, gate) = gate();
        let running = mgr.register_plan(
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
        mgr.subscribe(running.op_id, main.clone()).unwrap();
        mgr.subscribe(running.op_id, other.clone()).unwrap();
        mgr.execute(running.op_id, &auth).unwrap();
        half_rx.recv_timeout(LONG).unwrap();
        // A plan and an open prompt have subscribers too.
        let plan = mgr.register_plan(parts(PlanClass::Bounded), returns(json!(0)));
        let on_plan = VecSink::new("main");
        mgr.subscribe(plan.op_id, on_plan.clone()).unwrap();
        let (prompt, opened, answer) = held_prompt();
        let asked = mgr.register_plan(parts(PlanClass::Destructive), returns(json!(1)));
        let on_prompt = VecSink::new("main");
        mgr.subscribe(asked.op_id, on_prompt.clone()).unwrap();
        let executing = execute_in_background(&mgr, asked.op_id, prompt);
        opened.recv_timeout(LONG).expect("the prompt opened");

        mgr.drop_all_subscribers();
        open.send(()).unwrap();
        assert_eq!(wait_ended(&mgr, running.op_id), OpState::Finished);
        answer.send(AuthOutcome::Verified).unwrap();
        executing
            .recv_timeout(LONG)
            .expect("execute returned")
            .unwrap();
        wait_ended(&mgr, asked.op_id);
        // A plan dropped later tells its pages why: none is left to tell.
        mgr.drop_all_plans();

        assert_eq!(main.events(), vec![staged(1)], "nothing after the drop");
        assert_eq!(other.events(), vec![staged(1)], "nothing after the drop");
        assert_eq!(on_plan.sends(), 0);
        assert_eq!(on_prompt.sends(), 0);
        let again = VecSink::new("main");
        assert_eq!(
            mgr.subscribe(running.op_id, again.clone()).unwrap().replay,
            vec![staged(1), staged(2), completed(json!(null))],
            "the replay holds what the dropped pages missed"
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
        let failed = AuthOutcome::Failed {
            exhausted: false,
            retry_in_ms: None,
        };
        for ((name, close), answered) in closers
            .into_iter()
            .flat_map(|closer| [(closer, AuthOutcome::Verified), (closer, failed)])
        {
            let (_, mgr) = manager();
            let (auth, opened, answer) = held_prompt();
            let ran = Arc::new(AtomicBool::new(false));
            let view = mgr.register_plan(parts(PlanClass::Destructive), flags(&ran));
            let sink = VecSink::new("main");
            mgr.subscribe(view.op_id, sink.clone()).unwrap();
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
            answer.send(answered).unwrap();
            let result = result.recv_timeout(LONG).expect("execute returned");
            assert!(
                matches!(result, Err(DesktopError::AuthCancelled)),
                "{name}, {answered:?}: {result:?}"
            );
            assert!(!ran.load(SeqCst), "the plan ran after {name} returned");
            assert_eq!(
                sink.events(),
                vec![OpEvent::Failed {
                    error: DesktopError::AuthCancelled.to_ui()
                }],
                "{name}: the page that followed the plan hears it was refused"
            );
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

    // 12. Time to live, on both clocks: a suspend counts, a wall step back does not.

    #[test]
    fn a_plan_that_expired_while_its_prompt_was_open_never_runs() {
        type Passes = fn(&ManualClock);
        let ways: [(&str, Passes); 2] = [
            ("time passed", |clock| clock.advance(PLAN_TTL_MS + 1)),
            // The machine slept with the dialog up: only the wall clock saw it.
            ("the machine slept", |clock| clock.set_wall(T0 + DAY)),
        ];
        // `Busy` and `Failed` would put the plan back to wait: not once it expired.
        let failed = AuthOutcome::Failed {
            exhausted: false,
            retry_in_ms: None,
        };
        for answered in [AuthOutcome::Verified, AuthOutcome::Busy, failed] {
            for (how, pass) in ways {
                let (clock, mgr) = manager();
                let (auth, opened, answer) = held_prompt();
                let ran = Arc::new(AtomicBool::new(false));
                let view = mgr.register_plan(parts(PlanClass::Destructive), flags(&ran));
                let result = execute_in_background(&mgr, view.op_id, auth);
                opened.recv_timeout(LONG).expect("the prompt opened");
                pass(&clock);
                answer.send(answered).unwrap();
                let result = result.recv_timeout(LONG).expect("execute returned");
                assert!(
                    matches!(result, Err(DesktopError::PlanExpired { op_id }) if op_id == view.op_id),
                    "{how}, {answered:?}: {result:?}"
                );
                assert!(!ran.load(SeqCst), "{how}, {answered:?}");
                assert!(mgr.list().is_empty(), "{how}, {answered:?}");
                assert!(
                    matches!(
                        mgr.execute(view.op_id, &FakeAuthenticator::new()),
                        Err(DesktopError::PlanNotFound { .. })
                    ),
                    "{how}, {answered:?}"
                );
            }
        }
    }

    #[test]
    fn a_plan_expires_when_either_clock_says_its_time_to_live_passed() {
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
        // The wall clock jumps a day ahead while the monotonic clock stands still: that is
        // what a suspend looks like from inside (`Instant` does not count the sleep), and a
        // plan shown before the lid closed must not run on wake.
        let slept = mgr.register_plan(parts(PlanClass::Bounded), counting(&runs));
        clock.set_wall(T0 + DAY);
        let result = mgr.execute(slept.op_id, &auth);
        assert!(
            matches!(result, Err(DesktopError::PlanExpired { op_id }) if op_id == slept.op_id),
            "{result:?}"
        );
        // The wall clock steps back a day while no time passes: the plan is as young as it
        // was, for `execute` and for the sweep alike. The view shows wall time.
        let fresh = mgr.register_plan(parts(PlanClass::Bounded), counting(&runs));
        assert_eq!(fresh.expires_at_ms, T0 + DAY + PLAN_TTL_MS);
        clock.set_wall(T0);
        mgr.sweep();
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
        mgr.sweep();
        assert!(!dropped.load(SeqCst), "kept for one TTL past its expiry");
        clock.advance(1);
        mgr.flush_due();
        assert!(!dropped.load(SeqCst), "the output ticker sweeps nothing");
        {
            let mgr = mgr.clone();
            within("sweep", move || mgr.sweep());
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
        let paths: [(&str, Path); 8] = [
            ("the sweep in register_plan", |mgr, clock, _| {
                clock.advance(2 * PLAN_TTL_MS + 1);
                mgr.register_plan(parts(PlanClass::Bounded), returns(json!(0)));
            }),
            ("the ticker's sweep", |mgr, clock, _| {
                clock.advance(2 * PLAN_TTL_MS + 1);
                mgr.sweep();
            }),
            ("the sweep after a suspend", |mgr, clock, _| {
                clock.set_wall(T0 + DAY);
                mgr.sweep();
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

    #[test]
    fn a_plan_that_never_runs_gives_the_pages_that_followed_it_a_final_event() {
        type End = fn(&Arc<OperationManager>, &ManualClock, OpId) -> DesktopError;
        let ends: [(&str, End); 7] = [
            ("a refused prompt", |mgr, _, id| {
                let auth = FakeAuthenticator::new();
                auth.then(AuthOutcome::Cancelled {
                    by: CancelledBy::User,
                });
                let err = mgr.execute(id, &auth).unwrap_err();
                assert!(matches!(err, DesktopError::AuthCancelled), "{err:?}");
                err
            }),
            // A failed one is no end: the plan waits for another try.
            ("an unavailable prompt", |mgr, _, id| {
                let auth = FakeAuthenticator::new();
                auth.then(AuthOutcome::Unavailable {
                    reason: UnavailableReason::NotInteractive,
                });
                mgr.execute(id, &auth).unwrap_err()
            }),
            ("a panicking prompt", |mgr, _, id| {
                mgr.execute(id, &PanickingPrompt).unwrap_err()
            }),
            ("an expired execute", |mgr, clock, id| {
                clock.advance(PLAN_TTL_MS + 1);
                let err = mgr.execute(id, &FakeAuthenticator::new()).unwrap_err();
                assert!(matches!(err, DesktopError::PlanExpired { .. }), "{err:?}");
                err
            }),
            ("a lock", |mgr, _, _| {
                mgr.drop_all_plans();
                DesktopError::Locked
            }),
            ("the ticker's sweep", |mgr, clock, id| {
                clock.advance(2 * PLAN_TTL_MS + 1);
                mgr.sweep();
                DesktopError::PlanExpired { op_id: id }
            }),
            ("the sweep in register_plan", |mgr, clock, id| {
                clock.set_wall(T0 + DAY);
                mgr.register_plan(parts(PlanClass::Bounded), returns(json!(0)));
                DesktopError::PlanExpired { op_id: id }
            }),
        ];
        for (name, end) in ends {
            let (clock, mgr) = manager();
            let view = mgr.register_plan(parts(PlanClass::Destructive), returns(json!(0)));
            let main = VecSink::new("main");
            let other = VecSink::new("other");
            mgr.subscribe(view.op_id, main.clone()).unwrap();
            mgr.subscribe(view.op_id, other.clone()).unwrap();
            let why = {
                let mgr = mgr.clone();
                within(name, move || end(&mgr, &clock, view.op_id))
            };
            let last = OpEvent::Failed { error: why.to_ui() };
            assert_eq!(main.events(), vec![last.clone()], "{name}");
            assert_eq!(other.events(), vec![last], "{name}");
            assert!(mgr.list().is_empty(), "{name}");
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
            mgr.subscribe(view.op_id, VecSink::new("main"))
                .unwrap()
                .replay,
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
            mgr.subscribe(view.op_id, VecSink::new("main"))
                .unwrap()
                .replay,
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
            mgr.subscribe(view.op_id, VecSink::new("main"))
                .unwrap()
                .replay,
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

    // 16. Quitting: once the manager closes, nothing new runs.

    fn closing() -> OpEvent {
        OpEvent::Failed {
            error: DesktopError::Closing.to_ui(),
        }
    }

    #[test]
    fn a_closed_manager_refuses_a_plan_and_leaves_running_ops_alone() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let (open, gate) = gate();
        let running = mgr.register_plan(
            parts(PlanClass::Bounded),
            Box::new(move |_, _| {
                gate.wait();
                complete(json!("confirmed before the quit"))
            }),
        );
        mgr.execute(running.op_id, &auth).unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let plan = mgr.register_plan(parts(PlanClass::Bounded), flags(&ran));
        let sink = VecSink::new("main");
        mgr.subscribe(plan.op_id, sink.clone()).unwrap();
        mgr.close();
        let err = mgr.execute(plan.op_id, &auth).unwrap_err();
        assert!(matches!(err, DesktopError::Closing), "{err:?}");
        assert_eq!(
            err.to_ui().code.as_deref(),
            Some(errors::CLOSING),
            "{err:?}"
        );
        assert!(!ran.load(SeqCst));
        assert_eq!(sink.events(), vec![closing()], "its page hears why");
        assert!(
            matches!(
                mgr.execute(plan.op_id, &auth),
                Err(DesktopError::PlanNotFound { .. })
            ),
            "the plan is spent"
        );
        assert_eq!(state_of(&mgr, running.op_id), Some(OpState::Running));
        open.send(()).unwrap();
        assert_eq!(wait_ended(&mgr, running.op_id), OpState::Finished);
    }

    #[test]
    fn a_closed_manager_asks_the_owner_nothing() {
        let (_, mgr) = manager();
        let auth = FakeAuthenticator::new();
        let view = mgr.register_plan(parts(PlanClass::Destructive), returns(json!(0)));
        mgr.close();
        assert!(matches!(
            mgr.execute(view.op_id, &auth),
            Err(DesktopError::Closing)
        ));
        assert!(auth.asked().is_empty(), "no prompt opens while quitting");
    }

    #[test]
    fn a_yes_that_arrives_after_the_manager_closed_runs_nothing() {
        // Forced: the prompt opened before the quit and answers yes after it, and nothing else
        // closed it — only the check made where the operation would start can refuse it.
        let (_, mgr) = manager();
        let (auth, opened, answer) = held_prompt();
        let ran = Arc::new(AtomicBool::new(false));
        let view = mgr.register_plan(parts(PlanClass::Destructive), flags(&ran));
        let sink = VecSink::new("main");
        mgr.subscribe(view.op_id, sink.clone()).unwrap();
        let result = execute_in_background(&mgr, view.op_id, auth);
        opened.recv_timeout(LONG).expect("the prompt opened");
        mgr.close();
        answer.send(AuthOutcome::Verified).unwrap();
        let result = result.recv_timeout(LONG).expect("execute returned");
        assert!(matches!(result, Err(DesktopError::Closing)), "{result:?}");
        assert!(!ran.load(SeqCst), "the plan ran after the manager closed");
        assert_eq!(sink.events(), vec![closing()]);
        assert!(mgr.list().is_empty());
        assert_eq!(mgr.running(), 0);
    }

    #[test]
    fn a_busy_or_failed_answer_after_the_manager_closed_does_not_put_the_plan_back() {
        let failed = AuthOutcome::Failed {
            exhausted: false,
            retry_in_ms: None,
        };
        for answered in [AuthOutcome::Busy, failed] {
            let (_, mgr) = manager();
            let (auth, opened, answer) = held_prompt();
            let runs = Arc::new(AtomicUsize::new(0));
            let view = mgr.register_plan(parts(PlanClass::Destructive), counting(&runs));
            let sink = VecSink::new("main");
            mgr.subscribe(view.op_id, sink.clone()).unwrap();
            let result = execute_in_background(&mgr, view.op_id, auth);
            opened.recv_timeout(LONG).expect("the prompt opened");
            mgr.close();
            answer.send(answered).unwrap();
            let result = result.recv_timeout(LONG).expect("execute returned");
            assert!(
                matches!(result, Err(DesktopError::Closing)),
                "{answered:?}: {result:?}"
            );
            assert_eq!(sink.events(), vec![closing()], "{answered:?}");
            assert!(
                matches!(
                    mgr.execute(view.op_id, &FakeAuthenticator::new()),
                    Err(DesktopError::PlanNotFound { .. })
                ),
                "{answered:?}: nothing waits for another try"
            );
            assert_eq!(runs.load(SeqCst), 0);
        }
    }

    #[test]
    fn plans_dropped_after_the_manager_closed_tell_their_pages_it_is_quitting() {
        let (_, mgr) = manager();
        let view = mgr.register_plan(parts(PlanClass::Bounded), returns(json!(0)));
        let sink = VecSink::new("main");
        mgr.subscribe(view.op_id, sink.clone()).unwrap();
        mgr.close();
        mgr.drop_all_plans();
        assert_eq!(sink.events(), vec![closing()], "not that the app locked");
    }

    // 17. Subscriptions: each one is numbered, and ending one ends exactly that one.

    #[test]
    fn every_subscription_has_its_own_id_and_unsubscribe_ends_exactly_that_one() {
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
        // The same page twice, as a component that remounted.
        let first = VecSink::new("main");
        let second = VecSink::new("main");
        let a = mgr.subscribe(view.op_id, first.clone()).unwrap();
        mgr.execute(view.op_id, &FakeAuthenticator::new()).unwrap();
        half_rx.recv_timeout(LONG).unwrap();
        let b = mgr.subscribe(view.op_id, second.clone()).unwrap();
        assert_ne!(a.subscription, b.subscription);
        assert_eq!(b.replay, vec![staged(1)]);
        // Neither another op's id nor an unknown subscription ends anything.
        mgr.unsubscribe(OpId(view.op_id.0 + 100), a.subscription);
        mgr.unsubscribe(view.op_id, SubscriptionId(a.subscription.0 + 100));
        mgr.unsubscribe(view.op_id, a.subscription);
        open.send(()).unwrap();
        assert_eq!(wait_ended(&mgr, view.op_id), OpState::Finished);
        assert_eq!(
            first.events(),
            vec![staged(1)],
            "nothing after its unsubscribe"
        );
        assert_eq!(second.events(), vec![staged(2), completed(json!(null))]);
        // An ended op keeps no sink, but its subscription still has an id, and ending it
        // is a no-op.
        let late = mgr.subscribe(view.op_id, VecSink::new("main")).unwrap();
        assert!(![a.subscription, b.subscription].contains(&late.subscription));
        mgr.unsubscribe(view.op_id, late.subscription);
    }

    #[test]
    fn unsubscribe_ends_a_subscription_to_a_plan_and_to_an_open_prompt() {
        let (_, mgr) = manager();
        let view = mgr.register_plan(parts(PlanClass::Bounded), returns(json!(0)));
        let gone = VecSink::new("main");
        let kept = VecSink::new("main");
        let sub = mgr.subscribe(view.op_id, gone.clone()).unwrap();
        mgr.subscribe(view.op_id, kept.clone()).unwrap();
        mgr.unsubscribe(view.op_id, sub.subscription);
        mgr.drop_all_plans();
        assert_eq!(gone.sends(), 0);
        assert_eq!(
            kept.events(),
            vec![OpEvent::Failed {
                error: DesktopError::Locked.to_ui()
            }]
        );

        let (auth, opened, answer) = held_prompt();
        let view = mgr.register_plan(parts(PlanClass::Destructive), returns(json!(0)));
        let result = execute_in_background(&mgr, view.op_id, auth);
        opened.recv_timeout(LONG).expect("the prompt opened");
        let gone = VecSink::new("main");
        let kept = VecSink::new("main");
        let sub = mgr.subscribe(view.op_id, gone.clone()).unwrap();
        mgr.subscribe(view.op_id, kept.clone()).unwrap();
        mgr.unsubscribe(view.op_id, sub.subscription);
        answer
            .send(AuthOutcome::Cancelled {
                by: CancelledBy::User,
            })
            .unwrap();
        let result = result.recv_timeout(LONG).expect("execute returned");
        assert!(matches!(result, Err(DesktopError::AuthCancelled)));
        assert_eq!(gone.sends(), 0);
        assert_eq!(
            kept.events(),
            vec![OpEvent::Failed {
                error: DesktopError::AuthCancelled.to_ui()
            }]
        );
    }
}
