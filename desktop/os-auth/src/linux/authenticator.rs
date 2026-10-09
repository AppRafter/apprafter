// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The Linux authenticator: polkit where it can prompt, PAM through the app's own password
//! field where it cannot.
//!
//! polkit cannot prompt when its probe ([`polkit::probe`]) answers `Unavailable` — the policy
//! file is not installed (`PolicyMissing`, an AppImage), a `rules.d` rule grants the action
//! without asking (`ImplicitGrant`), polkit refuses this session (`NotPermittedHere`), or there
//! is no polkitd (`NoBackend`: the packages recommend polkitd, they do not depend on it) — or
//! when the last dialog found no authentication agent (`NoAgent`), which only a request that may
//! open a dialog can find out. The `NoAgent` finding holds until a later polkit request reaches
//! an agent (the user started one) or the shell forgets it
//! ([`OsAuthenticator::reset_agent_memory`], on each lock-screen entry); the others are probed
//! afresh every time.
//!
//! A refusal (`NotPermittedHere`) is not always "polkit cannot prompt here". Outside an active
//! local session — none, an inactive one, a remote one — it is the policy's own defaults
//! (`allow_any` and `allow_inactive` are `no`), and the password field stands in. Inside one the
//! defaults ask (`auth_self`), so the refusal is an administrator's `rules.d` rule returning NO,
//! and it is final: [`OsAuthenticator::info`] reports nothing available and the password is
//! refused, PAM unasked. Which session polkitd sees is read from the files it reads
//! ([`session`] gives the method).
//!
//! The password path runs only where polkit cannot prompt. Where it can, the policy is polkit's
//! to apply — an administrator's rule may ask for more than the user's own password — so
//! [`OsAuthenticator::verify_password`] refuses there with `Unavailable { NotPermittedHere }`.
//!
//! Known limits, both because polkit's answers to the app say less than its rules decide:
//! - A rule that asks for an administrator (`AUTH_ADMIN`) is still a challenge to the probe,
//!   like `auth_self`; without an agent the first dialog finds `NoAgent`, and the password field
//!   then checks the user's own password. polkit says which identities it wants only to an
//!   agent (`BeginAuthentication`), so the level cannot be learnt without one.
//! - A rule returning NO outside an active local session reads exactly as the defaults' refusal
//!   there, so it still moves to the password field.
//!
//! polkit requests run one at a time: one asked while another's dialog may be open is `Busy`,
//! so dialogs never stack and a request cannot ride a grant another one is being given.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, TryLockError};

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{AuthInfo, AuthMethod, AuthOutcome, CancelledBy, UnavailableReason};
use zeroize::Zeroizing;

use super::pam::{Pam, PasswordCheck};
use super::polkit::{self, Action};
use super::session;

/// polkit as the authenticator asks it: the system's in the app, a script in the tests.
trait Polkit: Send + Sync {
    fn probe(&self, action: Action) -> Result<(), AuthOutcome>;
    fn verify(&self, action: Action, cancel: &CancellationToken) -> AuthOutcome;
}

/// The password path as the authenticator asks it.
trait Password: Send + Sync {
    fn available(&self) -> Result<(), UnavailableReason>;
    fn verify_password(
        &self,
        password: Zeroizing<String>,
        cancel: &CancellationToken,
        now_monotonic_ms: u64,
    ) -> PasswordCheck;
}

/// The session as the authenticator asks about it: sd-login's files in the app, a value in the
/// tests.
trait Session: Send + Sync {
    /// polkitd sees the app in an active local session ([`session::active_local_session`]).
    fn active_local(&self) -> bool;
}

struct SystemSession;

impl Session for SystemSession {
    fn active_local(&self) -> bool {
        session::active_local_session()
    }
}

struct SystemPolkit;

impl Polkit for SystemPolkit {
    fn probe(&self, action: Action) -> Result<(), AuthOutcome> {
        polkit::probe(action)
    }

    fn verify(&self, action: Action, cancel: &CancellationToken) -> AuthOutcome {
        polkit::verify(action, cancel)
    }
}

impl Password for Pam {
    fn available(&self) -> Result<(), UnavailableReason> {
        Pam::available(self)
    }

    fn verify_password(
        &self,
        password: Zeroizing<String>,
        cancel: &CancellationToken,
        now_monotonic_ms: u64,
    ) -> PasswordCheck {
        Pam::verify_password(self, password, cancel, now_monotonic_ms)
    }
}

/// Which mechanism authenticates the owner here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Polkit,
    Password,
    /// Neither: an administrator's refusal (see the module docs), for this reason.
    Refused(UnavailableReason),
}

/// Device-owner authentication on Linux. The shell keeps one, so the PAM back-off is per
/// process. Every method blocks: call them on a blocking worker, never on an async worker or
/// the main thread.
pub struct OsAuthenticator {
    polkit: Box<dyn Polkit>,
    password: Box<dyn Password>,
    session: Box<dyn Session>,
    /// The last polkit dialog found no authentication agent.
    no_agent: AtomicBool,
    /// Held while a polkit request runs: one at a time.
    request: Mutex<()>,
}

impl Default for OsAuthenticator {
    fn default() -> Self {
        Self::new()
    }
}

impl OsAuthenticator {
    /// The system's polkit and PAM.
    pub fn new() -> Self {
        Self::with(
            Box::new(SystemPolkit),
            Box::new(Pam::new()),
            Box::new(SystemSession),
        )
    }

    fn with(
        polkit: Box<dyn Polkit>,
        password: Box<dyn Password>,
        session: Box<dyn Session>,
    ) -> Self {
        Self {
            polkit,
            password,
            session,
            no_agent: AtomicBool::new(false),
            request: Mutex::new(()),
        }
    }

    /// Forgets that the last dialog found no authentication agent, so the next [`Self::info`]
    /// and request ask polkit again. The shell calls it each time the lock screen opens: the
    /// user may have started an agent since, and only a polkit request can find one.
    pub fn reset_agent_memory(&self) {
        self.no_agent.store(false, Ordering::SeqCst);
    }

    fn route(&self, action: Action) -> Route {
        match self.polkit.probe(action) {
            Ok(()) if self.no_agent.load(Ordering::SeqCst) => Route::Password,
            Ok(()) => Route::Polkit,
            Err(AuthOutcome::Unavailable {
                reason: reason @ UnavailableReason::NotPermittedHere,
            }) if self.session.active_local() => Route::Refused(reason),
            Err(AuthOutcome::Unavailable { .. }) => Route::Password,
            // Anything else is polkit answering; a request will hear it from polkit itself.
            Err(_) => Route::Polkit,
        }
    }

    /// What the lock screen and the settings show, probed without a prompt: polkit if it can
    /// prompt for unlocking, else the password field if PAM can check one, else unavailable
    /// with PAM's reason, the last fallback's — or with polkit's, when its refusal is final.
    pub fn info(&self) -> AuthInfo {
        let available = |method, password_field| AuthInfo {
            available: true,
            method: Some(method),
            unavailable: None,
            biometrics_choice: false,
            password_field,
        };
        let unavailable = |reason| AuthInfo {
            available: false,
            method: None,
            unavailable: Some(reason),
            biometrics_choice: false,
            password_field: false,
        };
        match self.route(Action::Unlock) {
            Route::Polkit => available(AuthMethod::Polkit, false),
            Route::Password => match self.password.available() {
                Ok(()) => available(AuthMethod::Pam, true),
                Err(reason) => unavailable(reason),
            },
            Route::Refused(reason) => unavailable(reason),
        }
    }

    /// Asks polkit to authenticate the owner for `action` through the session's agent (see
    /// [`polkit::verify`]). `Unavailable` means polkit cannot prompt here: [`Self::info`] then
    /// offers the password field, or, for a final refusal, nothing. `cancel` closes the agent's
    /// dialog. `Busy`, asking polkit nothing, while another request runs.
    pub fn verify(&self, action: Action, cancel: &CancellationToken) -> AuthOutcome {
        let _request = match self.request.try_lock() {
            Ok(request) => request,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return AuthOutcome::Busy,
        };
        let outcome = self.polkit.verify(action, cancel);
        match outcome {
            AuthOutcome::Unavailable {
                reason: UnavailableReason::NoAgent,
            } => self.no_agent.store(true, Ordering::SeqCst),
            // An agent asked: it is there now.
            AuthOutcome::Verified
            | AuthOutcome::Failed { .. }
            | AuthOutcome::Cancelled {
                by: CancelledBy::User,
            } => self.no_agent.store(false, Ordering::SeqCst),
            _ => {}
        }
        outcome
    }

    /// Checks the password from the app's own field through PAM (see [`Pam::verify_password`]),
    /// only where polkit cannot prompt for `action`; where it can, or where its refusal is final,
    /// this is `Unavailable { NotPermittedHere }` and PAM is not asked.
    pub fn verify_password(
        &self,
        action: Action,
        password: Zeroizing<String>,
        cancel: &CancellationToken,
        now_monotonic_ms: u64,
    ) -> PasswordCheck {
        match self.route(action) {
            Route::Password => self
                .password
                .verify_password(password, cancel, now_monotonic_ms),
            Route::Polkit => PasswordCheck {
                outcome: AuthOutcome::Unavailable {
                    reason: UnavailableReason::NotPermittedHere,
                },
                messages: Vec::new(),
            },
            Route::Refused(reason) => PasswordCheck {
                outcome: AuthOutcome::Unavailable { reason },
                messages: Vec::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::*;
    use UnavailableReason::{
        ImplicitGrant, NoAgent, NoBackend, NoPamService, NotConfigured, NotPermittedHere,
        PolicyMissing,
    };

    const SECRET: &str = "hunter2 but longer";
    /// Longer than any wait a passing test makes; a failing one panics instead of hanging.
    const PATIENCE: Duration = Duration::from_secs(10);

    const fn unavailable(reason: UnavailableReason) -> AuthOutcome {
        AuthOutcome::Unavailable { reason }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Probe(Action),
        Verify(Action),
        /// The password PAM got, the token's state, and the clock.
        Password(String, bool, u64),
    }

    type Calls = Arc<Mutex<Vec<Call>>>;

    type Hook = Box<dyn FnOnce() + Send>;

    /// polkit from a script: the probe's answer, then each verify's in turn.
    struct FakePolkit {
        probe: Result<(), AuthOutcome>,
        verify: Mutex<Vec<AuthOutcome>>,
        /// Runs inside the first verify, before it answers.
        during_first_verify: Mutex<Option<Hook>>,
        calls: Calls,
    }

    impl Polkit for FakePolkit {
        fn probe(&self, action: Action) -> Result<(), AuthOutcome> {
            self.calls.lock().unwrap().push(Call::Probe(action));
            self.probe
        }

        fn verify(&self, action: Action, _cancel: &CancellationToken) -> AuthOutcome {
            self.calls.lock().unwrap().push(Call::Verify(action));
            let hook = self.during_first_verify.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
            self.verify.lock().unwrap().remove(0)
        }
    }

    struct FakeSession {
        active_local: bool,
    }

    impl Session for FakeSession {
        fn active_local(&self) -> bool {
            self.active_local
        }
    }

    struct FakePassword {
        available: Result<(), UnavailableReason>,
        calls: Calls,
    }

    impl Password for FakePassword {
        fn available(&self) -> Result<(), UnavailableReason> {
            self.available
        }

        fn verify_password(
            &self,
            password: Zeroizing<String>,
            cancel: &CancellationToken,
            now_monotonic_ms: u64,
        ) -> PasswordCheck {
            self.calls.lock().unwrap().push(Call::Password(
                password.to_string(),
                cancel.is_cancelled(),
                now_monotonic_ms,
            ));
            PasswordCheck {
                outcome: AuthOutcome::Verified,
                messages: vec!["from PAM".to_owned()],
            }
        }
    }

    /// What an authenticator under test is built from.
    struct Setup {
        probe: Result<(), AuthOutcome>,
        verify: Vec<AuthOutcome>,
        pam: Result<(), UnavailableReason>,
        active_local: bool,
        during_first_verify: Option<Hook>,
    }

    impl Setup {
        fn new(
            probe: Result<(), AuthOutcome>,
            verify: &[AuthOutcome],
            pam: Result<(), UnavailableReason>,
        ) -> Self {
            Self {
                probe,
                verify: verify.to_vec(),
                pam,
                active_local: false,
                during_first_verify: None,
            }
        }

        fn build(self) -> (OsAuthenticator, Calls) {
            let calls = Calls::default();
            let authenticator = OsAuthenticator::with(
                Box::new(FakePolkit {
                    probe: self.probe,
                    verify: Mutex::new(self.verify),
                    during_first_verify: Mutex::new(self.during_first_verify),
                    calls: Arc::clone(&calls),
                }),
                Box::new(FakePassword {
                    available: self.pam,
                    calls: Arc::clone(&calls),
                }),
                Box::new(FakeSession {
                    active_local: self.active_local,
                }),
            );
            (authenticator, calls)
        }
    }

    /// Outside an active local session.
    fn authenticator(
        probe: Result<(), AuthOutcome>,
        verify: &[AuthOutcome],
        pam: Result<(), UnavailableReason>,
    ) -> (OsAuthenticator, Calls) {
        Setup::new(probe, verify, pam).build()
    }

    fn info(method: AuthMethod, password_field: bool) -> AuthInfo {
        AuthInfo {
            available: true,
            method: Some(method),
            unavailable: None,
            biometrics_choice: false,
            password_field,
        }
    }

    fn polkit_info() -> AuthInfo {
        info(AuthMethod::Polkit, false)
    }

    fn pam_info() -> AuthInfo {
        info(AuthMethod::Pam, true)
    }

    fn password_check(authenticator: &OsAuthenticator, action: Action) -> PasswordCheck {
        authenticator.verify_password(
            action,
            Zeroizing::new(SECRET.to_owned()),
            &CancellationToken::new(),
            42,
        )
    }

    const NOT_HERE: PasswordCheck = PasswordCheck {
        outcome: AuthOutcome::Unavailable {
            reason: NotPermittedHere,
        },
        messages: Vec::new(),
    };

    fn from_pam() -> PasswordCheck {
        PasswordCheck {
            outcome: AuthOutcome::Verified,
            messages: vec!["from PAM".to_owned()],
        }
    }

    #[test]
    fn where_polkit_can_prompt_it_is_the_method_and_the_password_is_refused() {
        let (auth, calls) = authenticator(Ok(()), &[AuthOutcome::Verified], Ok(()));
        assert_eq!(auth.info(), polkit_info());
        assert_eq!(password_check(&auth, Action::Confirm), NOT_HERE);
        assert_eq!(
            auth.verify(Action::Confirm, &CancellationToken::new()),
            AuthOutcome::Verified
        );
        assert_eq!(
            *calls.lock().unwrap(),
            [
                Call::Probe(Action::Unlock),
                Call::Probe(Action::Confirm),
                Call::Verify(Action::Confirm),
            ],
            "PAM was never asked"
        );
    }

    /// Outside an active local session (where polkit's own defaults refuse), and for every reason
    /// but a refusal inside one.
    #[test]
    fn where_polkit_cannot_prompt_the_password_field_appears_and_pam_checks_it() {
        for reason in [PolicyMissing, ImplicitGrant, NotPermittedHere, NoBackend] {
            let (auth, calls) = authenticator(Err(unavailable(reason)), &[], Ok(()));
            assert_eq!(auth.info(), pam_info(), "{reason:?}");
            let token = CancellationToken::new();
            token.cancel();
            let check = auth.verify_password(
                Action::Confirm,
                Zeroizing::new(SECRET.to_owned()),
                &token,
                7,
            );
            assert_eq!(check, from_pam(), "{reason:?}");
            assert_eq!(
                *calls.lock().unwrap(),
                [
                    Call::Probe(Action::Unlock),
                    Call::Probe(Action::Confirm),
                    Call::Password(SECRET.to_owned(), true, 7),
                ],
                "{reason:?}: the password, the token and the clock reach PAM"
            );
        }
    }

    /// The probe cannot see a missing agent; the first dialog does, and from then on the
    /// password field stands in, until a polkit request reaches an agent again.
    #[test]
    fn a_dialog_without_an_agent_moves_to_the_password_until_an_agent_answers() {
        for answered in [
            AuthOutcome::Verified,
            AuthOutcome::Failed {
                exhausted: false,
                retry_in_ms: None,
            },
            AuthOutcome::Cancelled {
                by: CancelledBy::User,
            },
        ] {
            let (auth, _) = authenticator(Ok(()), &[unavailable(NoAgent), answered], Ok(()));
            assert_eq!(auth.info(), polkit_info());
            assert_eq!(password_check(&auth, Action::Unlock), NOT_HERE);
            assert_eq!(
                auth.verify(Action::Unlock, &CancellationToken::new()),
                unavailable(NoAgent)
            );
            assert_eq!(auth.info(), pam_info(), "{answered:?}");
            assert_eq!(password_check(&auth, Action::Unlock), from_pam());
            assert_eq!(
                auth.verify(Action::Unlock, &CancellationToken::new()),
                answered
            );
            assert_eq!(auth.info(), polkit_info(), "{answered:?}: an agent asked");
        }
    }

    /// Answers that say nothing about an agent leave the finding as it is.
    #[test]
    fn other_answers_neither_find_nor_clear_a_missing_agent() {
        let others = [
            AuthOutcome::Busy,
            AuthOutcome::Cancelled {
                by: CancelledBy::App,
            },
            AuthOutcome::Cancelled {
                by: CancelledBy::System,
            },
            unavailable(PolicyMissing),
            unavailable(NoBackend),
        ];
        let (auth, _) = authenticator(Ok(()), &others, Ok(()));
        for outcome in others {
            assert_eq!(
                auth.verify(Action::Unlock, &CancellationToken::new()),
                outcome
            );
            assert_eq!(
                auth.info(),
                polkit_info(),
                "{outcome:?} found no missing agent"
            );
        }
        let mut script = vec![unavailable(NoAgent)];
        script.extend(others);
        let (auth, _) = authenticator(Ok(()), &script, Ok(()));
        auth.verify(Action::Unlock, &CancellationToken::new());
        for outcome in others {
            assert_eq!(
                auth.verify(Action::Unlock, &CancellationToken::new()),
                outcome
            );
            assert_eq!(auth.info(), pam_info(), "{outcome:?} cleared the finding");
        }
    }

    /// In an active local session polkit's defaults ask (`auth_self`): a refusal without a
    /// challenge there is an administrator's `rules.d` rule saying NO, and the password field
    /// must not get round it.
    #[test]
    fn a_refusal_in_an_active_local_session_is_final() {
        let mut setup = Setup::new(
            Err(unavailable(NotPermittedHere)),
            &[unavailable(NotPermittedHere)],
            Ok(()),
        );
        setup.active_local = true;
        let (auth, calls) = setup.build();
        assert_eq!(
            auth.info(),
            AuthInfo {
                available: false,
                method: None,
                unavailable: Some(NotPermittedHere),
                biometrics_choice: false,
                password_field: false,
            }
        );
        assert_eq!(password_check(&auth, Action::Confirm), NOT_HERE);
        assert_eq!(password_check(&auth, Action::Unlock), NOT_HERE);
        assert_eq!(
            auth.verify(Action::Unlock, &CancellationToken::new()),
            unavailable(NotPermittedHere)
        );
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| matches!(call, Call::Password(..))),
            "PAM was never asked: {:?}",
            calls.lock().unwrap()
        );
    }

    /// Only a refusal is the administrator's: polkit's other reasons for not prompting still
    /// move to the password inside an active local session.
    #[test]
    fn in_an_active_local_session_the_other_reasons_still_move_to_the_password() {
        for reason in [PolicyMissing, ImplicitGrant, NoBackend] {
            let mut setup = Setup::new(Err(unavailable(reason)), &[], Ok(()));
            setup.active_local = true;
            let (auth, _) = setup.build();
            assert_eq!(auth.info(), pam_info(), "{reason:?}");
            assert_eq!(
                password_check(&auth, Action::Confirm),
                from_pam(),
                "{reason:?}"
            );
        }
    }

    /// The shell forgets a missing agent each time the lock screen opens: the user may have
    /// started one since, and only a polkit request can find it.
    #[test]
    fn forgetting_the_missing_agent_asks_polkit_again() {
        let (auth, calls) = authenticator(
            Ok(()),
            &[unavailable(NoAgent), AuthOutcome::Verified],
            Ok(()),
        );
        assert_eq!(
            auth.verify(Action::Unlock, &CancellationToken::new()),
            unavailable(NoAgent)
        );
        assert_eq!(auth.info(), pam_info());
        auth.reset_agent_memory();
        assert_eq!(auth.info(), polkit_info(), "polkit is the method again");
        assert_eq!(password_check(&auth, Action::Unlock), NOT_HERE);
        assert_eq!(
            auth.verify(Action::Unlock, &CancellationToken::new()),
            AuthOutcome::Verified
        );
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| matches!(call, Call::Verify(_)))
                .count(),
            2,
            "the second request reached polkit"
        );
        auth.reset_agent_memory();
        assert_eq!(auth.info(), polkit_info(), "with nothing to forget");
    }

    /// One polkit request at a time: one asked while another's dialog may be open is `Busy` and
    /// never reaches polkit, so dialogs do not stack and none can ride another's grant.
    #[test]
    fn a_request_while_another_is_open_is_busy() {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let mut setup = Setup::new(
            Ok(()),
            &[AuthOutcome::Verified, AuthOutcome::Verified],
            Ok(()),
        );
        setup.during_first_verify = Some(Box::new(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(PATIENCE).unwrap();
        }));
        let (auth, calls) = setup.build();
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
            *calls.lock().unwrap(),
            [Call::Verify(Action::Unlock)],
            "the second request never reached polkit"
        );
        assert_eq!(
            auth.verify(Action::Confirm, &CancellationToken::new()),
            AuthOutcome::Verified,
            "once the first has answered, the next one runs"
        );
    }

    #[test]
    fn without_polkit_and_pam_nothing_is_available_and_pam_says_why() {
        for (probe, pam) in [
            (Err(unavailable(PolicyMissing)), NoPamService),
            (Err(unavailable(NoBackend)), NoPamService),
            (Err(unavailable(ImplicitGrant)), NotConfigured),
        ] {
            let (auth, _) = authenticator(probe, &[], Err(pam));
            assert_eq!(
                auth.info(),
                AuthInfo {
                    available: false,
                    method: None,
                    unavailable: Some(pam),
                    biometrics_choice: false,
                    password_field: false,
                },
                "{probe:?}"
            );
        }
    }

    /// A probe answer that is not `Unavailable` is polkit's to report through a request.
    #[test]
    fn any_other_probe_answer_stays_with_polkit() {
        for probe in [
            AuthOutcome::Busy,
            AuthOutcome::Cancelled {
                by: CancelledBy::App,
            },
            AuthOutcome::Failed {
                exhausted: false,
                retry_in_ms: None,
            },
        ] {
            let (auth, _) = authenticator(Err(probe), &[probe], Ok(()));
            assert_eq!(auth.info(), polkit_info(), "{probe:?}");
            assert_eq!(password_check(&auth, Action::Unlock), NOT_HERE, "{probe:?}");
            assert_eq!(
                auth.verify(Action::Unlock, &CancellationToken::new()),
                probe
            );
        }
    }

    /// polkit's own `Unavailable` comes back from `verify` as it is, so the shell can tell the
    /// user why and read `info` again.
    #[test]
    fn verify_returns_polkit_s_outcome() {
        for outcome in [
            unavailable(PolicyMissing),
            unavailable(ImplicitGrant),
            AuthOutcome::Busy,
        ] {
            let (auth, calls) = authenticator(Err(outcome), &[outcome], Ok(()));
            assert_eq!(
                auth.verify(Action::Confirm, &CancellationToken::new()),
                outcome
            );
            assert_eq!(*calls.lock().unwrap(), [Call::Verify(Action::Confirm)]);
        }
    }
}
