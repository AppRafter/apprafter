// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! polkit: `CheckAuthorization` on the system bus for one of the two actions
//! `desktop/packaging/linux/dev.apprafter.desktop.policy` registers.
//!
//! The subject is the app's own connection, `system-bus-name` with its unique name: polkitd
//! takes the pid and uid from the bus's credentials for that name, so it trusts nothing the
//! app says about itself and no reused pid can stand in for it (the `unix-process` subject is
//! the class RUSTSEC-2026-0278 covers). No details are passed: polkitd refuses details from an
//! unprivileged caller, and the prompt's text comes only from the policy file.
//!
//! One request is up to three steps:
//! 1. A check without interaction. `Error.Failed` ("Action … is not registered") means the
//!    policy file is not installed: `PolicyMissing`. Authorized means a `rules.d` rule grants
//!    the action outright, which would make the gesture prove nothing: `ImplicitGrant`, refused.
//!    Neither authorized nor a challenge means this session may not authenticate at all
//!    (`allow_inactive=no` over SSH): `NotPermittedHere`.
//! 2. Only after a challenge: the same check with `AllowUserInteraction` and a cancellation id
//!    of its own, which makes polkitd open the session's authentication agent.
//! 3. While that check is open, tripping the [`CancellationToken`] sends
//!    `CancelCheckAuthorization`, which closes the agent's dialog.
//!
//! Step 2's answer means more than [`map_polkit`] can see from the answer alone:
//! - polkitd answers a check it cancelled not with `Error.Cancelled` but as dismissed or as not
//!   authorized (see [`crate::outcome`]), so a check the app cancelled is
//!   `Cancelled { by: App }` from this module's own record, whatever polkitd then answers;
//! - neither authorized nor a challenge, once step 1 said a challenge, is the agent's
//!   authentication failing: polkitd answers a failed authentication `(false, false)` without
//!   `polkit.dismissed` (`check_authorization_challenge_cb`, polkit 126), so it is `Failed`,
//!   not the `NotPermittedHere` the same answer means in step 1.
//!
//! Step 1 asks nobody, so it is a [`probe`], on a connection of its own that gives each call
//! [`PROBE_TIMEOUT`]: a polkitd that does not answer by then is hung, and the probe answers as if
//! there were none (`Unavailable { NoBackend }`, the password field's way). Steps 2 and 3 use a
//! connection without a limit: step 2 waits for a person, who may take minutes. Both
//! connections are the app's, and polkitd resolves either bus name to the same process.
//!
//! Threads: zbus's blocking API, whose own executor thread drives the socket. [`verify`] blocks
//! its caller until the agent answers, so it runs on a blocking worker, never on an async
//! worker or the main thread. The token is watched from the start: step 1 runs on a thread of
//! its own, and [`verify`] waits for its answer or for the token, so a cancel during step 1
//! returns at once (the probe, bounded, ends on its own). While the dialog is open a second,
//! scoped thread waits to send the cancel, because the token's callback must return at once
//! ([`CancellationToken::on_cancel`]).

use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{AuthOutcome, CancelledBy, UnavailableReason};
use zbus::zvariant::{OwnedValue, Value};
use zbus_polkit::policykit1::{AuthorityProxyBlocking, CheckAuthorizationFlags, Subject};

use crate::outcome::{map_polkit, PolkitAnswer, PolkitError};

/// The crate's one [`Action`]: the two actions `dev.apprafter.desktop.policy` registers.
pub use crate::Action;

impl Action {
    /// The action id in the policy file.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Unlock => "dev.apprafter.desktop.unlock",
            Self::Confirm => "dev.apprafter.desktop.confirm",
        }
    }
}

/// How long a probe waits for each answer from polkitd (see the module docs).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Asks polkit to authenticate the device owner for `action` (the module docs give the steps).
/// Blocks until the agent answers, or for up to [`PROBE_TIMEOUT`] when polkitd does not answer
/// step 1; `cancel` ends it at any step, closing the agent's dialog if one is open. A token
/// already tripped opens nothing.
pub fn verify(action: Action, cancel: &CancellationToken) -> AuthOutcome {
    authenticate(
        action.id(),
        cancel,
        move || probe(action),
        // No limit: step 2 waits for the owner.
        || SystemBus::connect(None),
    )
}

/// Step 1 alone, for `action`: whether polkit would open the session's authentication agent,
/// asking nobody. `Ok` when it would; otherwise the outcome [`verify`] would give without a
/// dialog. `Ok` does not say an agent is registered: polkit tells that only to a check that may
/// open one, so only [`verify`] finds `NoAgent`. A polkitd silent for [`PROBE_TIMEOUT`] is
/// none: `Unavailable { NoBackend }`.
pub fn probe(action: Action) -> Result<(), AuthOutcome> {
    let bus = SystemBus::connect(Some(PROBE_TIMEOUT))
        .map_err(|error| map_polkit(PolkitAnswer::Error(error)))?;
    probe_with(&bus, action.id())
}

const APP_CANCELLED: AuthOutcome = AuthOutcome::Cancelled {
    by: CancelledBy::App,
};

/// How one `CheckAuthorization` is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interaction<'a> {
    /// What polkit answers without asking anyone.
    None,
    /// polkitd may open the session's authentication agent; `cancellation_id` closes it.
    Allow { cancellation_id: &'a str },
}

/// What polkitd answered to `CancelCheckAuthorization`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancelReply {
    Accepted,
    /// `Error.Failed` ("No such cancellation_id"): the check has not reached polkitd yet, or
    /// has already ended.
    Unknown,
    /// Anything else, e.g. the bus is gone: asking again will not help.
    Refused,
}

/// polkitd as [`authenticate`] asks it: the system bus in the app, a script in the tests.
trait Authority: Sync {
    fn check(&self, action: &str, interaction: Interaction<'_>) -> PolkitAnswer;
    fn cancel_check(&self, cancellation_id: &str) -> CancelReply;
}

/// The three steps of the module docs: `first` is step 1 (in the app a [`probe`], bounded),
/// and `connect` gives the authority steps 2 and 3 ask.
fn authenticate<A: Authority>(
    action: &str,
    cancel: &CancellationToken,
    first: impl FnOnce() -> Result<(), AuthOutcome> + Send + 'static,
    connect: impl FnOnce() -> Result<A, PolkitError>,
) -> AuthOutcome {
    if cancel.is_cancelled() {
        return APP_CANCELLED;
    }
    // Registered before step 1, so a cancel during it ends the request at once.
    let prompt = Prompt::default();
    let _registration = {
        let prompt = prompt.clone();
        cancel.on_cancel(move || prompt.cancel())
    };
    if let Err(outcome) = prompt.first_check(first) {
        return outcome;
    }
    let authority = match connect() {
        Ok(authority) => authority,
        Err(error) => return map_polkit(PolkitAnswer::Error(error)),
    };
    // Tripped while connecting: no dialog opens.
    if prompt.cancelled() {
        return APP_CANCELLED;
    }
    let authority = &authority;
    let cancellation_id = next_cancellation_id();
    let answer = std::thread::scope(|scope| {
        let canceller = std::thread::Builder::new()
            .name("polkit-cancel".to_owned())
            .spawn_scoped(scope, || {
                prompt.send_cancel_when_tripped(authority, &cancellation_id)
            });
        // Without the thread a cancel could not close the dialog, so none opens.
        canceller.ok()?;
        let _answered = Answered(&prompt);
        Some(authority.check(
            action,
            Interaction::Allow {
                cancellation_id: &cancellation_id,
            },
        ))
    });
    let Some(answer) = answer else {
        return map_polkit(PolkitAnswer::Error(PolkitError::Other));
    };
    if prompt.cancelled() {
        APP_CANCELLED
    } else {
        prompt_outcome(answer)
    }
}

/// Step 1: `Ok` when polkit would ask the user, else the outcome without a dialog.
fn probe_with(authority: &impl Authority, action: &str) -> Result<(), AuthOutcome> {
    match authority.check(action, Interaction::None) {
        PolkitAnswer::Answered {
            authorized: true, ..
        } => Err(AuthOutcome::Unavailable {
            reason: UnavailableReason::ImplicitGrant,
        }),
        PolkitAnswer::Answered {
            challenge: true, ..
        } => Ok(()),
        other => Err(map_polkit(other)),
    }
}

/// Step 2's answer, for a check the app did not cancel.
fn prompt_outcome(answer: PolkitAnswer) -> AuthOutcome {
    match answer {
        PolkitAnswer::Answered {
            authorized: false,
            challenge: false,
            dismissed: false,
        } => AuthOutcome::Failed { exhausted: false },
        other => map_polkit(other),
    }
}

/// polkitd keeps a cancellation id per caller, and this one is unique in the process too.
fn next_cancellation_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "apprafter-desktop-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The first pause before asking polkitd again to cancel a check it does not know yet, and the
/// longest; the pause doubles in between.
const FIRST_RETRY: Duration = Duration::from_millis(10);
const LAST_RETRY: Duration = Duration::from_millis(200);

/// One open dialog, shared with the token's callback.
#[derive(Clone, Default)]
struct Prompt(Arc<(Mutex<PromptState>, Condvar)>);

#[derive(Default)]
struct PromptState {
    /// The token tripped: the app closed the dialog, whatever polkitd answers.
    cancelled: bool,
    /// Step 1's answer, once it has come.
    first: Option<Result<(), AuthOutcome>>,
    /// polkitd has answered the check.
    answered: bool,
}

impl Prompt {
    fn state(&self) -> MutexGuard<'_, PromptState> {
        self.0 .0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn cancelled(&self) -> bool {
        self.state().cancelled
    }

    /// The token's callback: records the cancel and wakes the canceller, never waits on D-Bus.
    fn cancel(&self) {
        self.state().cancelled = true;
        self.0 .1.notify_all();
    }

    fn answered(&self) {
        self.state().answered = true;
        self.0 .1.notify_all();
    }

    /// Step 1, `check`, on a thread of its own: its answer, or the app's cancel as soon as the
    /// token trips, whichever comes first. A cancelled check runs on to its own end (bounded, in
    /// the app) and its answer is dropped. A check that panics answers as no polkitd would.
    fn first_check(
        &self,
        check: impl FnOnce() -> Result<(), AuthOutcome> + Send + 'static,
    ) -> Result<(), AuthOutcome> {
        if self.cancelled() {
            return Err(APP_CANCELLED);
        }
        let prompt = self.clone();
        let spawned = std::thread::Builder::new()
            .name("polkit-probe".to_owned())
            .spawn(move || {
                let answer = panic::catch_unwind(AssertUnwindSafe(check)).unwrap_or_else(|_| {
                    tracing::error!("polkit's first check panicked: counted as no polkitd");
                    Err(map_polkit(PolkitAnswer::Error(PolkitError::Other)))
                });
                prompt.state().first = Some(answer);
                prompt.0 .1.notify_all();
            });
        // Without the thread a cancel could not end step 1 at once, so it is not asked.
        if spawned.is_err() {
            return Err(map_polkit(PolkitAnswer::Error(PolkitError::Other)));
        }
        let condvar = &self.0 .1;
        let mut state = self.state();
        loop {
            if state.cancelled {
                return Err(APP_CANCELLED);
            }
            if let Some(answer) = state.first.take() {
                return answer;
            }
            state = condvar.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Runs beside the open check: once the token trips, sends `CancelCheckAuthorization` until
    /// polkitd takes it or the check has been answered. polkitd knows the id only once the check
    /// has reached it, so a cancel that overtakes the check is refused as unknown and sent again.
    fn send_cancel_when_tripped(&self, authority: &impl Authority, cancellation_id: &str) {
        let condvar = &self.0 .1;
        let mut state = self.state();
        while !state.cancelled && !state.answered {
            state = condvar.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
        let mut pause = FIRST_RETRY;
        while !state.answered {
            drop(state);
            match authority.cancel_check(cancellation_id) {
                CancelReply::Accepted | CancelReply::Refused => return,
                CancelReply::Unknown => {}
            }
            state = self.state();
            if !state.answered {
                state = condvar
                    .wait_timeout(state, pause)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            pause = (pause * 2).min(LAST_RETRY);
        }
    }
}

/// Marks the check answered when dropped, so the canceller ends even if the check panics.
struct Answered<'a>(&'a Prompt);

impl Drop for Answered<'_> {
    fn drop(&mut self) {
        self.0.answered();
    }
}

/// polkitd on the system bus, asked about this connection.
struct SystemBus {
    authority: AuthorityProxyBlocking<'static>,
    subject: Subject,
}

impl SystemBus {
    /// The system bus, each call on it given `method_timeout` (none: no limit).
    fn connect(method_timeout: Option<Duration>) -> Result<Self, PolkitError> {
        let builder =
            zbus::blocking::connection::Builder::system().map_err(|e| polkit_error(&e))?;
        let builder = match method_timeout {
            Some(limit) => builder.method_timeout(limit),
            None => builder,
        };
        let connection = builder.build().map_err(|e| polkit_error(&e))?;
        let name = connection.unique_name().ok_or(PolkitError::Other)?;
        let name =
            OwnedValue::try_from(Value::from(name.as_str())).map_err(|_| PolkitError::Other)?;
        let subject = Subject {
            subject_kind: "system-bus-name".to_owned(),
            subject_details: HashMap::from([("name".to_owned(), name)]),
        };
        let authority = AuthorityProxyBlocking::new(&connection).map_err(|e| polkit_error(&e))?;
        Ok(Self { authority, subject })
    }
}

impl Authority for SystemBus {
    fn check(&self, action: &str, interaction: Interaction<'_>) -> PolkitAnswer {
        let (flags, cancellation_id) = match interaction {
            Interaction::None => (Default::default(), ""),
            Interaction::Allow { cancellation_id } => (
                CheckAuthorizationFlags::AllowUserInteraction.into(),
                cancellation_id,
            ),
        };
        match self.authority.check_authorization(
            &self.subject,
            action,
            &HashMap::new(),
            flags,
            cancellation_id,
        ) {
            Ok(result) => PolkitAnswer::from_result(
                result.is_authorized,
                result.is_challenge,
                &result.details,
            ),
            Err(error) => PolkitAnswer::Error(polkit_error(&error)),
        }
    }

    fn cancel_check(&self, cancellation_id: &str) -> CancelReply {
        match self.authority.cancel_check_authorization(cancellation_id) {
            Ok(()) => CancelReply::Accepted,
            Err(error) if polkit_error(&error) == PolkitError::Failed => CancelReply::Unknown,
            Err(_) => CancelReply::Refused,
        }
    }
}

/// A D-Bus error as polkit's: its error name, or [`PolkitError::Other`] for anything that is not
/// an error reply (no system bus, no polkitd on it, a call past its time limit — logged).
fn polkit_error(error: &zbus::Error) -> PolkitError {
    match error {
        zbus::Error::MethodError(name, _, _) => PolkitError::from_name(name.as_str()),
        zbus::Error::InputOutput(io) if io.kind() == std::io::ErrorKind::TimedOut => {
            tracing::warn!("polkitd did not answer within {PROBE_TIMEOUT:?}: counted as absent");
            PolkitError::Other
        }
        _ => PolkitError::Other,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    use super::*;
    use UnavailableReason::{ImplicitGrant, NoAgent, NoBackend, NotPermittedHere, PolicyMissing};

    const ACTION: &str = "dev.apprafter.desktop.unlock";
    /// Longer than any wait a passing test makes; a failing one panics instead of hanging.
    const PATIENCE: Duration = Duration::from_secs(10);

    const fn answered(authorized: bool, challenge: bool, dismissed: bool) -> PolkitAnswer {
        PolkitAnswer::Answered {
            authorized,
            challenge,
            dismissed,
        }
    }
    const CHALLENGE: PolkitAnswer = answered(false, true, false);
    const AUTHORIZED: PolkitAnswer = answered(true, false, false);
    const REFUSED: PolkitAnswer = answered(false, false, false);
    const DISMISSED: PolkitAnswer = answered(false, false, true);

    const fn unavailable(reason: UnavailableReason) -> AuthOutcome {
        AuthOutcome::Unavailable { reason }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Check {
            action: String,
            /// `Some(cancellation id)` for a check that may open the agent.
            interactive: Option<String>,
        },
        Cancel(String),
    }

    /// What the fake answers to the check that may open the agent.
    enum Dialog {
        Answers(PolkitAnswer),
        /// Stays open until a cancel for its id is accepted, refusing the first `unknown`
        /// cancels as polkitd refuses one that overtakes its check; then answers.
        OpenUntilCancelled {
            unknown: usize,
            then: PolkitAnswer,
        },
    }

    /// polkitd from a script, recording every call.
    struct Fake {
        probe: PolkitAnswer,
        dialog: Dialog,
        calls: Mutex<Vec<Call>>,
        /// Runs inside the first check, before it answers.
        during_probe: Option<Box<dyn Fn() + Send + Sync>>,
        /// Told when the dialog opens.
        opened: Mutex<Option<mpsc::Sender<()>>>,
        /// The id of an accepted cancel, and the count of refused ones.
        cancel: (Mutex<(Option<String>, usize)>, Condvar),
    }

    impl Fake {
        fn new(probe: PolkitAnswer, dialog: Dialog) -> Self {
            Self {
                probe,
                dialog,
                calls: Mutex::default(),
                during_probe: None,
                opened: Mutex::default(),
                cancel: Default::default(),
            }
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        /// A receiver told when the dialog opens.
        fn watch_dialog(&self) -> mpsc::Receiver<()> {
            let (tx, rx) = mpsc::channel();
            *self.opened.lock().unwrap() = Some(tx);
            rx
        }
    }

    impl Authority for Fake {
        fn check(&self, action: &str, interaction: Interaction<'_>) -> PolkitAnswer {
            let interactive = match interaction {
                Interaction::None => None,
                Interaction::Allow { cancellation_id } => Some(cancellation_id.to_owned()),
            };
            self.calls.lock().unwrap().push(Call::Check {
                action: action.to_owned(),
                interactive: interactive.clone(),
            });
            let Some(id) = interactive else {
                if let Some(hook) = &self.during_probe {
                    hook();
                }
                return self.probe;
            };
            if let Some(tx) = self.opened.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            match self.dialog {
                Dialog::Answers(answer) => answer,
                Dialog::OpenUntilCancelled { then, .. } => {
                    let (lock, condvar) = &self.cancel;
                    let deadline = Instant::now() + PATIENCE;
                    let mut cancelled = lock.lock().unwrap();
                    while cancelled.0.as_deref() != Some(id.as_str()) {
                        let left = deadline
                            .checked_duration_since(Instant::now())
                            .expect("the dialog was never cancelled");
                        cancelled = condvar.wait_timeout(cancelled, left).unwrap().0;
                    }
                    then
                }
            }
        }

        fn cancel_check(&self, cancellation_id: &str) -> CancelReply {
            self.calls
                .lock()
                .unwrap()
                .push(Call::Cancel(cancellation_id.to_owned()));
            let unknown = match self.dialog {
                Dialog::OpenUntilCancelled { unknown, .. } => unknown,
                Dialog::Answers(_) => 0,
            };
            let (lock, condvar) = &self.cancel;
            let mut cancelled = lock.lock().unwrap();
            if cancelled.1 < unknown {
                cancelled.1 += 1;
                return CancelReply::Unknown;
            }
            cancelled.0 = Some(cancellation_id.to_owned());
            condvar.notify_all();
            CancelReply::Accepted
        }
    }

    /// The app's authority is the system bus; a test's, a fake, borrowed.
    impl<T: Authority> Authority for &T {
        fn check(&self, action: &str, interaction: Interaction<'_>) -> PolkitAnswer {
            (**self).check(action, interaction)
        }

        fn cancel_check(&self, cancellation_id: &str) -> CancelReply {
            (**self).cancel_check(cancellation_id)
        }
    }

    /// `authenticate` as [`verify`] runs it, with `fake` as polkitd for every step.
    fn run(fake: &Arc<Fake>, cancel: &CancellationToken) -> AuthOutcome {
        let first = Arc::clone(fake);
        authenticate(
            ACTION,
            cancel,
            move || probe_with(&*first, ACTION),
            || Ok(&**fake),
        )
    }

    fn probe_only() -> Vec<Call> {
        vec![Call::Check {
            action: ACTION.to_owned(),
            interactive: None,
        }]
    }

    /// The cancellation id of the check that opened the dialog.
    fn dialog_id(calls: &[Call]) -> String {
        calls
            .iter()
            .find_map(|call| match call {
                Call::Check {
                    interactive: Some(id),
                    ..
                } => Some(id.clone()),
                _ => None,
            })
            .expect("no dialog was opened")
    }

    /// Runs `authenticate` and trips the token once the fake's dialog is open.
    fn cancelled_while_open(fake: &Arc<Fake>) -> AuthOutcome {
        let token = CancellationToken::new();
        let opened = fake.watch_dialog();
        let canceller = {
            let token = token.clone();
            thread::spawn(move || {
                opened
                    .recv_timeout(PATIENCE)
                    .expect("the dialog never opened");
                token.cancel();
            })
        };
        let outcome = run(fake, &token);
        canceller.join().unwrap();
        outcome
    }

    #[test]
    fn an_unregistered_action_is_policy_missing_and_opens_no_dialog() {
        let fake = Arc::new(Fake::new(
            PolkitAnswer::Error(PolkitError::Failed),
            Dialog::Answers(AUTHORIZED),
        ));
        let outcome = run(&fake, &CancellationToken::new());
        assert_eq!(outcome, unavailable(PolicyMissing));
        assert_eq!(fake.calls(), probe_only());
    }

    /// A `rules.d` rule that answers YES would make the gesture a no-op: refused, and the
    /// interactive check, which would come back authorized at once, is never made.
    #[test]
    fn an_action_granted_without_asking_is_refused_as_an_implicit_grant() {
        let fake = Arc::new(Fake::new(AUTHORIZED, Dialog::Answers(AUTHORIZED)));
        let outcome = run(&fake, &CancellationToken::new());
        assert_eq!(outcome, unavailable(ImplicitGrant));
        assert_eq!(fake.calls(), probe_only());
    }

    #[test]
    fn a_refusal_without_a_challenge_is_not_permitted_here_and_opens_no_dialog() {
        let fake = Arc::new(Fake::new(REFUSED, Dialog::Answers(AUTHORIZED)));
        let outcome = run(&fake, &CancellationToken::new());
        assert_eq!(outcome, unavailable(NotPermittedHere));
        assert_eq!(fake.calls(), probe_only());
    }

    #[test]
    fn any_other_error_on_the_first_check_is_polkits_mapping() {
        for (error, outcome) in [
            (PolkitError::NotSupported, unavailable(NoBackend)),
            (PolkitError::Other, unavailable(NoBackend)),
            (PolkitError::NotAuthorized, unavailable(NotPermittedHere)),
            (PolkitError::CancellationIdNotUnique, AuthOutcome::Busy),
        ] {
            let fake = Arc::new(Fake::new(
                PolkitAnswer::Error(error),
                Dialog::Answers(AUTHORIZED),
            ));
            assert_eq!(run(&fake, &CancellationToken::new()), outcome, "{error:?}");
            assert_eq!(fake.calls(), probe_only(), "{error:?}");
        }
    }

    /// What `info` asks: step 1 alone, which never opens a dialog.
    #[test]
    fn the_probe_asks_without_interaction_and_opens_no_dialog() {
        for (probe, outcome) in [
            (CHALLENGE, Ok(())),
            (AUTHORIZED, Err(unavailable(ImplicitGrant))),
            (answered(true, true, false), Err(unavailable(ImplicitGrant))),
            (REFUSED, Err(unavailable(NotPermittedHere))),
            (
                PolkitAnswer::Error(PolkitError::Failed),
                Err(unavailable(PolicyMissing)),
            ),
            (
                PolkitAnswer::Error(PolkitError::Other),
                Err(unavailable(NoBackend)),
            ),
        ] {
            let fake = Fake::new(probe, Dialog::Answers(AUTHORIZED));
            assert_eq!(probe_with(&fake, ACTION), outcome, "{probe:?}");
            assert_eq!(fake.calls(), probe_only(), "{probe:?}");
        }
    }

    #[test]
    fn a_challenge_opens_the_dialog_with_interaction_and_a_cancellation_id() {
        let fake = Arc::new(Fake::new(CHALLENGE, Dialog::Answers(AUTHORIZED)));
        let outcome = run(&fake, &CancellationToken::new());
        assert_eq!(outcome, AuthOutcome::Verified);
        let calls = fake.calls();
        let id = dialog_id(&calls);
        assert!(!id.is_empty(), "an empty id cannot be cancelled");
        assert_eq!(
            calls,
            [
                probe_only(),
                vec![Call::Check {
                    action: ACTION.to_owned(),
                    interactive: Some(id),
                }]
            ]
            .concat()
        );
    }

    #[test]
    fn what_the_dialog_answers_is_the_outcome() {
        for (answer, outcome) in [
            (AUTHORIZED, AuthOutcome::Verified),
            (
                DISMISSED,
                AuthOutcome::Cancelled {
                    by: CancelledBy::User,
                },
            ),
            // No agent registered for the session: polkitd answers the challenge again.
            (CHALLENGE, unavailable(NoAgent)),
            // The agent asked and the authentication failed.
            (REFUSED, AuthOutcome::Failed { exhausted: false }),
            (
                PolkitAnswer::Error(PolkitError::CancellationIdNotUnique),
                AuthOutcome::Busy,
            ),
            (
                PolkitAnswer::Error(PolkitError::Other),
                unavailable(NoBackend),
            ),
        ] {
            let fake = Arc::new(Fake::new(CHALLENGE, Dialog::Answers(answer)));
            assert_eq!(run(&fake, &CancellationToken::new()), outcome, "{answer:?}");
        }
    }

    /// polkitd reports its own cancel as the dialog dismissed or as not authorized, and the
    /// agent may answer in the same moment: the app's record decides.
    #[test]
    fn a_dialog_the_app_cancelled_is_cancelled_by_the_app_whatever_polkit_answers() {
        for then in [
            DISMISSED,
            REFUSED,
            AUTHORIZED,
            CHALLENGE,
            PolkitAnswer::Error(PolkitError::Cancelled),
            PolkitAnswer::Error(PolkitError::Other),
        ] {
            let fake = Arc::new(Fake::new(
                CHALLENGE,
                Dialog::OpenUntilCancelled { unknown: 0, then },
            ));
            assert_eq!(cancelled_while_open(&fake), APP_CANCELLED, "{then:?}");
            let calls = fake.calls();
            let id = dialog_id(&calls);
            assert_eq!(calls.last(), Some(&Call::Cancel(id)), "{then:?}");
        }
    }

    #[test]
    fn a_cancel_that_overtakes_its_check_is_sent_again() {
        let fake = Arc::new(Fake::new(
            CHALLENGE,
            Dialog::OpenUntilCancelled {
                unknown: 3,
                then: DISMISSED,
            },
        ));
        assert_eq!(cancelled_while_open(&fake), APP_CANCELLED);
        let calls = fake.calls();
        let id = dialog_id(&calls);
        let cancels = calls.iter().filter(|c| **c == Call::Cancel(id.clone()));
        assert_eq!(cancels.count(), 4);
    }

    #[test]
    fn a_token_tripped_before_the_request_asks_nothing() {
        let fake = Arc::new(Fake::new(CHALLENGE, Dialog::Answers(AUTHORIZED)));
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(run(&fake, &token), APP_CANCELLED);
        assert_eq!(fake.calls(), []);
    }

    #[test]
    fn a_token_tripped_during_the_first_check_opens_no_dialog() {
        let token = CancellationToken::new();
        let mut fake = Fake::new(CHALLENGE, Dialog::Answers(AUTHORIZED));
        let trip = token.clone();
        fake.during_probe = Some(Box::new(move || trip.cancel()));
        let fake = Arc::new(fake);
        assert_eq!(run(&fake, &token), APP_CANCELLED);
        assert_eq!(fake.calls(), probe_only());
    }

    /// polkitd hangs on the first check (in the app the probe gives up after `PROBE_TIMEOUT`):
    /// a lock or a quit tripping the token meanwhile ends the request at once, not when the
    /// check gives up, and no dialog opens.
    #[test]
    fn a_token_tripped_while_the_first_check_hangs_ends_the_request_at_once() {
        let (entered, probing) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let (entered, released) = (Mutex::new(entered), Mutex::new(released));
        let mut fake = Fake::new(CHALLENGE, Dialog::Answers(AUTHORIZED));
        fake.during_probe = Some(Box::new(move || {
            entered.lock().unwrap().send(()).unwrap();
            let _ = released.lock().unwrap().recv_timeout(PATIENCE);
        }));
        let fake = Arc::new(fake);
        let token = CancellationToken::new();
        let canceller = {
            let token = token.clone();
            thread::spawn(move || {
                probing
                    .recv_timeout(PATIENCE)
                    .expect("the first check began");
                token.cancel();
            })
        };
        let started = Instant::now();
        let outcome = run(&fake, &token);
        let took = started.elapsed();
        canceller.join().unwrap();
        release.send(()).unwrap();
        assert_eq!(outcome, APP_CANCELLED);
        assert!(took < PATIENCE / 2, "it waited {took:?} for the hung check");
        assert_eq!(fake.calls(), probe_only(), "no dialog opened");
    }

    #[test]
    fn a_token_tripped_while_connecting_for_the_dialog_opens_none() {
        let fake = Arc::new(Fake::new(CHALLENGE, Dialog::Answers(AUTHORIZED)));
        let token = CancellationToken::new();
        let first = Arc::clone(&fake);
        let outcome = authenticate(
            ACTION,
            &token,
            move || probe_with(&*first, ACTION),
            || {
                token.cancel();
                Ok(&*fake)
            },
        );
        assert_eq!(outcome, APP_CANCELLED);
        assert_eq!(fake.calls(), probe_only());
    }

    /// Step 1 is asked apart from the connection the dialog uses: in the app the probe's own,
    /// with its time limit, while the dialog's has none.
    #[test]
    fn the_first_check_is_not_asked_on_the_dialog_s_connection() {
        let probe = Arc::new(Fake::new(CHALLENGE, Dialog::Answers(REFUSED)));
        let dialog = Fake::new(REFUSED, Dialog::Answers(AUTHORIZED));
        let first = Arc::clone(&probe);
        let outcome = authenticate(
            ACTION,
            &CancellationToken::new(),
            move || probe_with(&*first, ACTION),
            || Ok(&dialog),
        );
        assert_eq!(outcome, AuthOutcome::Verified);
        assert_eq!(probe.calls(), probe_only());
        let calls = dialog.calls();
        assert_eq!(
            calls,
            [Call::Check {
                action: ACTION.to_owned(),
                interactive: Some(dialog_id(&calls)),
            }]
        );
    }

    #[test]
    fn a_failed_connection_for_the_dialog_is_polkits_mapping_and_a_panicking_check_no_backend() {
        let fake = Arc::new(Fake::new(CHALLENGE, Dialog::Answers(AUTHORIZED)));
        let first = Arc::clone(&fake);
        let outcome = authenticate(
            ACTION,
            &CancellationToken::new(),
            move || probe_with(&*first, ACTION),
            || Err::<&Fake, _>(PolkitError::Other),
        );
        assert_eq!(outcome, unavailable(NoBackend));
        assert_eq!(fake.calls(), probe_only());

        let mut fake = Fake::new(CHALLENGE, Dialog::Answers(AUTHORIZED));
        fake.during_probe = Some(Box::new(|| panic!("the first check broke")));
        let fake = Arc::new(fake);
        let (tx, answered) = mpsc::channel();
        {
            let fake = Arc::clone(&fake);
            thread::spawn(move || tx.send(run(&fake, &CancellationToken::new())));
        }
        let outcome = answered
            .recv_timeout(PATIENCE)
            .expect("a panicking check left the request waiting");
        assert_eq!(outcome, unavailable(NoBackend));
        assert_eq!(fake.calls(), probe_only());
    }

    #[test]
    fn a_token_tripped_after_the_answer_cancels_nothing() {
        let fake = Arc::new(Fake::new(CHALLENGE, Dialog::Answers(AUTHORIZED)));
        let token = CancellationToken::new();
        assert_eq!(run(&fake, &token), AuthOutcome::Verified);
        token.cancel();
        assert!(
            !fake.calls().iter().any(|c| matches!(c, Call::Cancel(_))),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn every_dialog_has_its_own_cancellation_id() {
        let first = Arc::new(Fake::new(CHALLENGE, Dialog::Answers(AUTHORIZED)));
        let second = Arc::new(Fake::new(CHALLENGE, Dialog::Answers(AUTHORIZED)));
        run(&first, &CancellationToken::new());
        run(&second, &CancellationToken::new());
        assert_ne!(dialog_id(&first.calls()), dialog_id(&second.calls()));
    }

    /// The action ids here are the ones the policy file registers, and the file asks for the
    /// user's own password every time, never an administrator's and never a kept grant.
    /// A probe asks nobody, so a polkitd silent this long is hung, not waiting for the owner.
    #[test]
    fn a_probe_waits_five_seconds_for_polkitd() {
        assert_eq!(PROBE_TIMEOUT, Duration::from_secs(5));
    }

    #[test]
    fn the_policy_file_registers_both_actions_with_auth_self_only() {
        let policy = include_str!("../../../packaging/linux/dev.apprafter.desktop.policy");
        assert!(
            policy.starts_with("<?xml "),
            "the XML declaration is line 1"
        );
        assert_eq!(
            policy.lines().nth(1),
            Some("<!-- SPDX-License-Identifier: FSL-1.1-Apache-2.0 -->")
        );
        let defaults = "<defaults><allow_any>no</allow_any><allow_inactive>no</allow_inactive>\
                        <allow_active>auth_self</allow_active></defaults>";
        for action in [Action::Unlock, Action::Confirm] {
            let start = policy
                .find(&format!("<action id=\"{}\">", action.id()))
                .unwrap_or_else(|| panic!("{} is not registered", action.id()));
            let body = &policy[start..];
            let body = &body[..body.find("</action>").expect("unterminated action")];
            assert!(body.contains(defaults), "{}: {body}", action.id());
        }
        // Two actions, and no `<allow_…>` beyond their six: nothing else can be granted.
        assert_eq!(policy.matches("<action ").count(), 2);
        assert_eq!(policy.matches("<allow_").count(), 6);
        assert_eq!(policy.matches(defaults).count(), 2);
    }
}
