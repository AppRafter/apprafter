// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! macOS: LocalAuthentication's device-owner policy (`LAPolicyDeviceOwnerAuthentication`), which
//! asks for Touch ID or an Apple Watch where the Mac has them and for the account password
//! otherwise. The system chooses, and draws the dialog itself.
//!
//! A request:
//! 1. A fresh `LAContext`, never reused: a context that has evaluated the policy once can pass
//!    it again without asking.
//! 2. `canEvaluatePolicy:error:` on it. A refusal is the LAError's outcome ([`map_la_error`]),
//!    and no dialog opens.
//! 3. `evaluatePolicy:localizedReason:reply:` with [`reason`]. Its reply block sends the answer
//!    into a channel, and the caller's thread reads it in [`TICK`] steps, looking at the
//!    [`CancellationToken`] between them ([`wait_for`]). A tripped token invalidates the context
//!    (`invalidate`, which closes the dialog and makes the evaluation reply `LAErrorAppCancel`),
//!    waits up to [`CANCEL_GRACE`] for that reply, and answers `Cancelled { by: App }` whatever
//!    the reply then says: a dialog the app closed is the app's cancel even if the owner had just
//!    authenticated. A reply that arrives once the token has tripped is that cancel too.
//!
//! The dialog has no parent window, and the system names the app in it: the reason is a short
//! action without the app's name ("… is trying to unlock."), and the shell brings its window to
//! the front before it asks, so the dialog does not open over another app.
//!
//! Threads: the caller's, a blocking worker. `LAContext` is not `Send`: each request creates,
//! asks and drops its context on that one thread. The reply block runs on a private queue of the
//! framework and only sends into the channel.
//!
//! Requests run one at a time: one asked while another's dialog may be open is `Busy`, so
//! dialogs never stack.
//!
//! Only `system` touches the framework, and only on macOS; the rest of this module compiles and
//! is tested on every OS, against a fake.

#[cfg(target_os = "macos")]
mod system;

use std::sync::{Mutex, TryLockError};
use std::time::Duration;

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{AuthInfo, AuthMethod, AuthOutcome, CancelledBy, UnavailableReason};

use crate::outcome::map_la_error;
use crate::Action;

/// How often a wait looks at the token.
pub const TICK: Duration = Duration::from_millis(100);
/// How long an invalidated evaluation may take to reply before the wait gives up on it.
pub const CANCEL_GRACE: Duration = Duration::from_secs(2);

const APP_CANCELLED: AuthOutcome = AuthOutcome::Cancelled {
    by: CancelledBy::App,
};

/// The reason the dialog shows for `action`, as "<app> is trying to <reason>.": a short action
/// without the app's name, which the system adds. Never empty: LocalAuthentication raises an
/// Objective-C exception for an empty reason, and that aborts the process.
pub const fn reason(action: Action) -> &'static str {
    match action {
        Action::Unlock => "unlock",
        Action::Confirm => "approve a destructive operation",
    }
}

/// A refusal from LocalAuthentication: the code of an `NSError` in `LAErrorDomain`, or `None`
/// when there was no error, or one of another domain.
pub type LaError = Option<isize>;

/// What one request came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    /// `canEvaluatePolicy:error:` refused, so no dialog opened.
    CannotEvaluate(LaError),
    /// The evaluation's reply: the owner authenticated, or the refusal.
    Replied(Result<(), LaError>),
    /// The framework was not asked: the reason was empty (see [`reason`]).
    NotStarted,
    /// The token tripped; the context was invalidated.
    Cancelled,
}

/// LocalAuthentication as the authenticator asks it: the system's in the app, a script in the
/// tests.
pub trait LocalAuthentication: Send + Sync {
    /// `canEvaluatePolicy:error:` on a fresh context. Opens no dialog.
    fn can_evaluate(&self) -> Result<(), LaError>;
    /// One request on a fresh context (the module docs give the steps): opens the dialog with
    /// `reason` and waits until it answers, or until `cancel` closes it.
    fn evaluate(&self, reason: &str, cancel: &CancellationToken) -> Asked;
}

/// An evaluation in progress as [`wait_for`] waits for it.
pub trait Pending {
    /// The reply, if it comes within `timeout`.
    fn wait(&self, timeout: Duration) -> Option<Result<(), LaError>>;
    /// Invalidates the context, which closes the dialog. The evaluation may still reply
    /// afterwards, or not at all.
    fn invalidate(&self);
}

/// How long [`wait_for`] waits at a time and for an invalidated evaluation to reply.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub tick: Duration,
    pub grace: Duration,
}

/// The app's timing: [`TICK`] and [`CANCEL_GRACE`].
pub const TIMING: Timing = Timing {
    tick: TICK,
    grace: CANCEL_GRACE,
};

/// Waits for `evaluation` (the module docs give the rules). There is no limit: the owner may
/// take as long as they like, and only the token ends the wait early.
pub fn wait_for(evaluation: &impl Pending, cancel: &CancellationToken, timing: Timing) -> Asked {
    loop {
        if cancel.is_cancelled() {
            evaluation.invalidate();
            let _ = evaluation.wait(timing.grace);
            return Asked::Cancelled;
        }
        if let Some(reply) = evaluation.wait(timing.tick) {
            return if cancel.is_cancelled() {
                Asked::Cancelled
            } else {
                Asked::Replied(reply)
            };
        }
    }
}

/// A refusal of `canEvaluatePolicy:error:` as an outcome. One without an LAError says nothing
/// about why the policy cannot be evaluated: as if there were no LocalAuthentication.
fn cannot_evaluate(error: LaError) -> AuthOutcome {
    match error {
        Some(code) => map_la_error(code),
        None => AuthOutcome::Unavailable {
            reason: UnavailableReason::NoBackend,
        },
    }
}

/// The evaluation's reply as an outcome. Nothing but the framework's own success is `Verified`.
fn replied(reply: Result<(), LaError>) -> AuthOutcome {
    match reply {
        Ok(()) => AuthOutcome::Verified,
        Err(Some(code)) => map_la_error(code),
        Err(None) => AuthOutcome::Failed {
            exhausted: false,
            retry_in_ms: None,
        },
    }
}

/// Device-owner authentication on macOS. Every method blocks: call them on a blocking worker,
/// never on an async worker or the main thread.
pub struct OsAuthenticator {
    local_authentication: Box<dyn LocalAuthentication>,
    /// Held while a request runs: one at a time.
    request: Mutex<()>,
}

#[cfg(target_os = "macos")]
impl Default for OsAuthenticator {
    fn default() -> Self {
        Self::new()
    }
}

impl OsAuthenticator {
    /// The system's LocalAuthentication.
    #[cfg(target_os = "macos")]
    pub fn new() -> Self {
        Self::with(Box::new(system::SystemLocalAuthentication))
    }

    fn with(local_authentication: Box<dyn LocalAuthentication>) -> Self {
        Self {
            local_authentication,
            request: Mutex::new(()),
        }
    }

    /// What the lock screen and the settings show, probed without a dialog
    /// (`canEvaluatePolicy:error:`). The system chooses between Touch ID and the password, so
    /// there is no choice to offer and no field of the app's own. A refusal that is not
    /// `Unavailable` (none is expected for this policy) leaves it available: a request will
    /// report it.
    pub fn info(&self) -> AuthInfo {
        match self
            .local_authentication
            .can_evaluate()
            .map_err(cannot_evaluate)
        {
            Err(AuthOutcome::Unavailable { reason }) => AuthInfo {
                available: false,
                method: None,
                unavailable: Some(reason),
                biometrics_choice: false,
                password_field: false,
            },
            Ok(()) | Err(_) => AuthInfo {
                available: true,
                method: Some(AuthMethod::MacLocalAuthentication),
                unavailable: None,
                biometrics_choice: false,
                password_field: false,
            },
        }
    }

    /// Asks macOS to authenticate the owner for `action` (the module docs give the steps).
    /// Blocks until the dialog answers; `cancel` closes it. A token already tripped opens
    /// nothing, and so does a request while another one runs (`Busy`).
    pub fn verify(&self, action: Action, cancel: &CancellationToken) -> AuthOutcome {
        if cancel.is_cancelled() {
            return APP_CANCELLED;
        }
        let _request = match self.request.try_lock() {
            Ok(request) => request,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return AuthOutcome::Busy,
        };
        match self.local_authentication.evaluate(reason(action), cancel) {
            Asked::CannotEvaluate(error) => cannot_evaluate(error),
            Asked::Replied(reply) => replied(reply),
            Asked::NotStarted => AuthOutcome::Failed {
                exhausted: false,
                retry_in_ms: None,
            },
            Asked::Cancelled => APP_CANCELLED,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Instant;

    use super::*;
    use UnavailableReason::{NoBackend, NotConfigured, NotInteractive};

    /// Short enough for a quick test, long enough to tell "waited for the grace" apart.
    const FAST: Timing = Timing {
        tick: Duration::from_millis(5),
        grace: Duration::from_millis(300),
    };
    /// Longer than any wait a passing test makes; a failing one panics instead of hanging.
    const PATIENCE: Duration = Duration::from_secs(10);

    // LAError codes (`LAError.h`).
    const AUTHENTICATION_FAILED: isize = -1;
    const USER_CANCEL: isize = -2;
    const SYSTEM_CANCEL: isize = -4;
    const PASSCODE_NOT_SET: isize = -5;
    const BIOMETRY_LOCKOUT: isize = -8;
    const APP_CANCEL: isize = -9;
    const NOT_INTERACTIVE: isize = -1004;

    const fn unavailable(reason: UnavailableReason) -> AuthOutcome {
        AuthOutcome::Unavailable { reason }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        CanEvaluate,
        Evaluate(String),
    }

    type Calls = Arc<Mutex<Vec<Call>>>;

    type Hook = Box<dyn FnOnce() + Send>;

    /// LocalAuthentication from a script: `canEvaluatePolicy`'s answer, then each request's.
    struct FakeLocalAuthentication {
        can_evaluate: Result<(), LaError>,
        evaluate: Mutex<VecDeque<Asked>>,
        /// Runs inside the first request, before it answers.
        during_first_request: Mutex<Option<Hook>>,
        calls: Calls,
    }

    impl LocalAuthentication for FakeLocalAuthentication {
        fn can_evaluate(&self) -> Result<(), LaError> {
            self.calls.lock().unwrap().push(Call::CanEvaluate);
            self.can_evaluate
        }

        fn evaluate(&self, reason: &str, _cancel: &CancellationToken) -> Asked {
            self.calls
                .lock()
                .unwrap()
                .push(Call::Evaluate(reason.to_owned()));
            let hook = self.during_first_request.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
            self.evaluate
                .lock()
                .unwrap()
                .pop_front()
                .expect("a scripted answer")
        }
    }

    fn fixture(
        can_evaluate: Result<(), LaError>,
        evaluate: &[Asked],
        during_first_request: Option<Hook>,
    ) -> (OsAuthenticator, Calls) {
        let calls = Calls::default();
        let auth = OsAuthenticator::with(Box::new(FakeLocalAuthentication {
            can_evaluate,
            evaluate: Mutex::new(evaluate.iter().copied().collect()),
            during_first_request: Mutex::new(during_first_request),
            calls: Arc::clone(&calls),
        }));
        (auth, calls)
    }

    fn calls(calls: &Calls) -> Vec<Call> {
        std::mem::take(&mut *calls.lock().unwrap())
    }

    fn evaluate(action: Action) -> Call {
        Call::Evaluate(reason(action).to_owned())
    }

    const AVAILABLE: AuthInfo = AuthInfo {
        available: true,
        method: Some(AuthMethod::MacLocalAuthentication),
        unavailable: None,
        biometrics_choice: false,
        password_field: false,
    };

    fn unavailable_info(reason: UnavailableReason) -> AuthInfo {
        AuthInfo {
            available: false,
            method: None,
            unavailable: Some(reason),
            biometrics_choice: false,
            password_field: false,
        }
    }

    #[test]
    fn a_verified_reply_is_verified_and_the_reason_names_the_action() {
        let (auth, log) = fixture(
            Ok(()),
            &[Asked::Replied(Ok(())), Asked::Replied(Ok(()))],
            None,
        );
        assert_eq!(auth.info(), AVAILABLE);
        for action in [Action::Unlock, Action::Confirm] {
            assert_eq!(
                auth.verify(action, &CancellationToken::new()),
                AuthOutcome::Verified
            );
        }
        assert_eq!(
            calls(&log),
            [
                Call::CanEvaluate,
                evaluate(Action::Unlock),
                evaluate(Action::Confirm)
            ]
        );
    }

    #[test]
    fn the_replies_refusals_are_their_la_errors_outcomes() {
        for (code, outcome) in [
            (
                AUTHENTICATION_FAILED,
                AuthOutcome::Failed {
                    exhausted: false,
                    retry_in_ms: None,
                },
            ),
            (
                USER_CANCEL,
                AuthOutcome::Cancelled {
                    by: CancelledBy::User,
                },
            ),
            (
                SYSTEM_CANCEL,
                AuthOutcome::Cancelled {
                    by: CancelledBy::System,
                },
            ),
            (
                BIOMETRY_LOCKOUT,
                AuthOutcome::Failed {
                    exhausted: true,
                    retry_in_ms: None,
                },
            ),
            (APP_CANCEL, APP_CANCELLED),
            (NOT_INTERACTIVE, unavailable(NotInteractive)),
            (
                42,
                AuthOutcome::Failed {
                    exhausted: false,
                    retry_in_ms: None,
                },
            ),
        ] {
            let (auth, _) = fixture(Ok(()), &[Asked::Replied(Err(Some(code)))], None);
            assert_eq!(
                auth.verify(Action::Unlock, &CancellationToken::new()),
                outcome,
                "{code}"
            );
        }
    }

    /// A refusal without an LAError (no error object, or another domain's) is never verified.
    #[test]
    fn a_refusal_without_an_la_error_fails() {
        let (auth, _) = fixture(Ok(()), &[Asked::Replied(Err(None))], None);
        assert_eq!(
            auth.verify(Action::Confirm, &CancellationToken::new()),
            AuthOutcome::Failed {
                exhausted: false,
                retry_in_ms: None
            }
        );
    }

    #[test]
    fn a_policy_that_cannot_be_evaluated_is_unavailable_with_its_reason() {
        for (error, reason) in [
            (Some(PASSCODE_NOT_SET), NotConfigured),
            (Some(NOT_INTERACTIVE), NotInteractive),
            (None, NoBackend),
        ] {
            let (auth, log) = fixture(Err(error), &[Asked::CannotEvaluate(error)], None);
            assert_eq!(auth.info(), unavailable_info(reason), "{error:?}");
            assert_eq!(
                auth.verify(Action::Unlock, &CancellationToken::new()),
                unavailable(reason),
                "{error:?}"
            );
            assert_eq!(
                calls(&log),
                [Call::CanEvaluate, evaluate(Action::Unlock)],
                "{error:?}"
            );
        }
    }

    /// A refusal of the preflight that is not `Unavailable` is the request's to report.
    #[test]
    fn a_preflight_refusal_that_is_not_unavailable_leaves_it_available() {
        for code in [AUTHENTICATION_FAILED, BIOMETRY_LOCKOUT, USER_CANCEL, 42] {
            let (auth, _) = fixture(Err(Some(code)), &[Asked::CannotEvaluate(Some(code))], None);
            assert_eq!(auth.info(), AVAILABLE, "{code}");
            assert_eq!(
                auth.verify(Action::Unlock, &CancellationToken::new()),
                map_la_error(code),
                "{code}"
            );
        }
    }

    #[test]
    fn a_cancelled_request_is_the_app_s_cancel() {
        let (auth, _) = fixture(Ok(()), &[Asked::Cancelled], None);
        assert_eq!(
            auth.verify(Action::Unlock, &CancellationToken::new()),
            APP_CANCELLED
        );
    }

    #[test]
    fn a_request_that_never_started_fails() {
        let (auth, _) = fixture(Ok(()), &[Asked::NotStarted], None);
        assert_eq!(
            auth.verify(Action::Unlock, &CancellationToken::new()),
            AuthOutcome::Failed {
                exhausted: false,
                retry_in_ms: None
            }
        );
    }

    #[test]
    fn a_token_tripped_before_asks_nothing() {
        let (auth, log) = fixture(Ok(()), &[], None);
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(auth.verify(Action::Confirm, &token), APP_CANCELLED);
        assert!(calls(&log).is_empty());
    }

    /// One request at a time: one asked while another's dialog may be open is `Busy` and never
    /// reaches the framework, so dialogs do not stack.
    #[test]
    fn a_request_while_another_is_open_is_busy() {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let (auth, log) = fixture(
            Ok(()),
            &[Asked::Replied(Ok(())), Asked::Replied(Ok(()))],
            Some(Box::new(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(PATIENCE).unwrap();
            })),
        );
        thread::scope(|scope| {
            let first = scope.spawn(|| auth.verify(Action::Unlock, &CancellationToken::new()));
            entered.recv_timeout(PATIENCE).unwrap();
            assert_eq!(
                auth.verify(Action::Confirm, &CancellationToken::new()),
                AuthOutcome::Busy
            );
            release.send(()).unwrap();
            assert_eq!(first.join().unwrap(), AuthOutcome::Verified);
        });
        assert_eq!(
            calls(&log),
            [evaluate(Action::Unlock)],
            "the second request never reached the framework"
        );
        assert_eq!(
            auth.verify(Action::Confirm, &CancellationToken::new()),
            AuthOutcome::Verified,
            "once the first has answered, the next one runs"
        );
    }

    /// An empty reason would abort the process (the framework raises an exception for one).
    #[test]
    fn the_reasons_are_short_actions_and_never_empty() {
        assert_eq!(reason(Action::Unlock), "unlock");
        assert_eq!(reason(Action::Confirm), "approve a destructive operation");
        for action in [Action::Unlock, Action::Confirm] {
            assert!(!reason(action).trim().is_empty());
            assert!(
                !reason(action).contains("AppRafter"),
                "the system names the app"
            );
        }
    }

    // `wait_for`, against an evaluation the test answers.

    /// An evaluation that replies what the test sends, and on `invalidate` replies `on_cancel`
    /// (as the framework replies `LAErrorAppCancel`) if there is one.
    struct FakeEvaluation {
        replies: Receiver<Result<(), LaError>>,
        sender: Sender<Result<(), LaError>>,
        on_invalidate: Option<Result<(), LaError>>,
        invalidations: AtomicUsize,
    }

    impl FakeEvaluation {
        fn new(on_invalidate: Option<Result<(), LaError>>) -> Self {
            let (sender, replies) = mpsc::channel();
            Self {
                replies,
                sender,
                on_invalidate,
                invalidations: AtomicUsize::new(0),
            }
        }

        fn reply_later(&self, after: Duration, reply: Result<(), LaError>) {
            let sender = self.sender.clone();
            thread::spawn(move || {
                thread::sleep(after);
                let _ = sender.send(reply);
            });
        }

        fn invalidations(&self) -> usize {
            self.invalidations.load(Ordering::SeqCst)
        }
    }

    impl Pending for FakeEvaluation {
        fn wait(&self, timeout: Duration) -> Option<Result<(), LaError>> {
            self.replies.recv_timeout(timeout).ok()
        }

        fn invalidate(&self) {
            self.invalidations.fetch_add(1, Ordering::SeqCst);
            if let Some(reply) = self.on_invalidate {
                let _ = self.sender.send(reply);
            }
        }
    }

    fn trip_after(after: Duration) -> CancellationToken {
        let token = CancellationToken::new();
        let tripped = token.clone();
        thread::spawn(move || {
            thread::sleep(after);
            tripped.cancel();
        });
        token
    }

    #[test]
    fn a_reply_is_the_evaluation_s_and_nothing_is_invalidated() {
        for reply in [Ok(()), Err(Some(USER_CANCEL)), Err(None)] {
            let evaluation = FakeEvaluation::new(None);
            evaluation.reply_later(Duration::from_millis(30), reply);
            assert_eq!(
                wait_for(&evaluation, &CancellationToken::new(), FAST),
                Asked::Replied(reply)
            );
            assert_eq!(evaluation.invalidations(), 0);
        }
    }

    /// The framework replies to an invalidated context at once: the wait answers then, not
    /// after the grace.
    #[test]
    fn a_tripped_token_invalidates_the_context() {
        let evaluation = FakeEvaluation::new(Some(Err(Some(APP_CANCEL))));
        let token = trip_after(Duration::from_millis(30));
        let started = Instant::now();
        assert_eq!(wait_for(&evaluation, &token, FAST), Asked::Cancelled);
        assert_eq!(evaluation.invalidations(), 1);
        assert!(started.elapsed() < FAST.grace, "{:?}", started.elapsed());
    }

    /// An invalidation that brings no reply is waited for no longer than the grace.
    #[test]
    fn an_invalidation_without_a_reply_is_given_up_after_the_grace() {
        let evaluation = FakeEvaluation::new(None);
        let token = trip_after(Duration::from_millis(30));
        let started = Instant::now();
        assert_eq!(wait_for(&evaluation, &token, FAST), Asked::Cancelled);
        assert_eq!(evaluation.invalidations(), 1);
        let waited = started.elapsed();
        assert!(waited >= FAST.grace, "{waited:?}");
        assert!(waited < FAST.grace * 3, "{waited:?}");
    }

    /// The dialog closed under the owner's finger: the app's cancel, not a verification.
    #[test]
    fn a_verification_from_an_invalidated_context_is_the_app_s_cancel() {
        let evaluation = FakeEvaluation::new(Some(Ok(())));
        let token = trip_after(Duration::from_millis(30));
        assert_eq!(wait_for(&evaluation, &token, FAST), Asked::Cancelled);
        assert_eq!(evaluation.invalidations(), 1);
    }

    /// A reply that arrives once the token has tripped is not taken either.
    #[test]
    fn a_reply_after_the_token_tripped_is_the_app_s_cancel() {
        /// Trips the token as it hands over the reply, as a lock-on-sleep in the same instant
        /// would.
        struct Racing {
            token: CancellationToken,
        }
        impl Pending for Racing {
            fn wait(&self, _timeout: Duration) -> Option<Result<(), LaError>> {
                self.token.cancel();
                Some(Ok(()))
            }
            fn invalidate(&self) {
                panic!("the evaluation had replied");
            }
        }
        let token = CancellationToken::new();
        let racing = Racing {
            token: token.clone(),
        };
        assert_eq!(wait_for(&racing, &token, FAST), Asked::Cancelled);
    }

    #[test]
    fn a_token_tripped_before_the_wait_invalidates_at_once() {
        let evaluation = FakeEvaluation::new(Some(Err(Some(APP_CANCEL))));
        let token = CancellationToken::new();
        token.cancel();
        evaluation.reply_later(Duration::from_millis(50), Ok(()));
        assert_eq!(wait_for(&evaluation, &token, FAST), Asked::Cancelled);
        assert_eq!(evaluation.invalidations(), 1);
    }

    /// The owner may take their time: no limit ends the wait.
    #[test]
    fn a_slow_reply_is_still_taken() {
        let evaluation = FakeEvaluation::new(None);
        evaluation.reply_later(Duration::from_millis(200), Ok(()));
        assert_eq!(
            wait_for(&evaluation, &CancellationToken::new(), FAST),
            Asked::Replied(Ok(()))
        );
        assert_eq!(evaluation.invalidations(), 0);
    }

    /// The real framework, asked only what opens no dialog: `canEvaluatePolicy:error:`. Whatever
    /// this Mac answers, the call goes through and the answer is one the app can show.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_preflight_answers_without_a_dialog() {
        let info = OsAuthenticator::new().info();
        assert!(!info.biometrics_choice && !info.password_field, "{info:?}");
        if info.available {
            assert_eq!(info, AVAILABLE);
        } else {
            assert!(info.unavailable.is_some(), "{info:?}");
        }
    }
}
