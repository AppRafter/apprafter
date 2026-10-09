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
//! an agent (the user started one); the others are probed afresh every time.
//!
//! The password path runs only where polkit cannot prompt. Where it can, the policy is polkit's
//! to apply — an administrator's rule may ask for more than the user's own password — so
//! [`OsAuthenticator::verify_password`] refuses there with `Unavailable { NotPermittedHere }`.

use std::sync::atomic::{AtomicBool, Ordering};

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{AuthInfo, AuthMethod, AuthOutcome, CancelledBy, UnavailableReason};
use zeroize::Zeroizing;

use super::pam::{Pam, PasswordCheck};
use super::polkit::{self, Action};

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
}

/// Device-owner authentication on Linux. The shell keeps one, so the PAM back-off is per
/// process. Every method blocks: call them on a blocking worker, never on an async worker or
/// the main thread.
pub struct OsAuthenticator {
    polkit: Box<dyn Polkit>,
    password: Box<dyn Password>,
    /// The last polkit dialog found no authentication agent.
    no_agent: AtomicBool,
}

impl Default for OsAuthenticator {
    fn default() -> Self {
        Self::new()
    }
}

impl OsAuthenticator {
    /// The system's polkit and PAM.
    pub fn new() -> Self {
        Self::with(Box::new(SystemPolkit), Box::new(Pam::new()))
    }

    fn with(polkit: Box<dyn Polkit>, password: Box<dyn Password>) -> Self {
        Self {
            polkit,
            password,
            no_agent: AtomicBool::new(false),
        }
    }

    fn route(&self, action: Action) -> Route {
        match self.polkit.probe(action) {
            Ok(()) if self.no_agent.load(Ordering::SeqCst) => Route::Password,
            Ok(()) => Route::Polkit,
            Err(AuthOutcome::Unavailable { .. }) => Route::Password,
            // Anything else is polkit answering; a request will hear it from polkit itself.
            Err(_) => Route::Polkit,
        }
    }

    /// What the lock screen and the settings show, probed without a prompt: polkit if it can
    /// prompt for unlocking, else the password field if PAM can check one, else unavailable
    /// with PAM's reason, the last fallback's.
    pub fn info(&self) -> AuthInfo {
        let available = |method, password_field| AuthInfo {
            available: true,
            method: Some(method),
            unavailable: None,
            biometrics_choice: false,
            password_field,
        };
        match self.route(Action::Unlock) {
            Route::Polkit => available(AuthMethod::Polkit, false),
            Route::Password => match self.password.available() {
                Ok(()) => available(AuthMethod::Pam, true),
                Err(reason) => AuthInfo {
                    available: false,
                    method: None,
                    unavailable: Some(reason),
                    biometrics_choice: false,
                    password_field: false,
                },
            },
        }
    }

    /// Asks polkit to authenticate the owner for `action` through the session's agent (see
    /// [`polkit::verify`]). `Unavailable` means polkit cannot prompt here: [`Self::info`] then
    /// offers the password field. `cancel` closes the agent's dialog.
    pub fn verify(&self, action: Action, cancel: &CancellationToken) -> AuthOutcome {
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
    /// only where polkit cannot prompt for `action`; where it can, this is
    /// `Unavailable { NotPermittedHere }` and PAM is not asked.
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
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use UnavailableReason::{
        ImplicitGrant, NoAgent, NoBackend, NoPamService, NotConfigured, NotPermittedHere,
        PolicyMissing,
    };

    const SECRET: &str = "hunter2 but longer";

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

    /// polkit from a script: the probe's answer, then each verify's in turn.
    struct FakePolkit {
        probe: Result<(), AuthOutcome>,
        verify: Mutex<Vec<AuthOutcome>>,
        calls: Calls,
    }

    impl Polkit for FakePolkit {
        fn probe(&self, action: Action) -> Result<(), AuthOutcome> {
            self.calls.lock().unwrap().push(Call::Probe(action));
            self.probe
        }

        fn verify(&self, action: Action, _cancel: &CancellationToken) -> AuthOutcome {
            self.calls.lock().unwrap().push(Call::Verify(action));
            self.verify.lock().unwrap().remove(0)
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

    fn authenticator(
        probe: Result<(), AuthOutcome>,
        verify: &[AuthOutcome],
        pam: Result<(), UnavailableReason>,
    ) -> (OsAuthenticator, Calls) {
        let calls = Calls::default();
        let authenticator = OsAuthenticator::with(
            Box::new(FakePolkit {
                probe,
                verify: Mutex::new(verify.to_vec()),
                calls: Arc::clone(&calls),
            }),
            Box::new(FakePassword {
                available: pam,
                calls: Arc::clone(&calls),
            }),
        );
        (authenticator, calls)
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
            AuthOutcome::Failed { exhausted: false },
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
            AuthOutcome::Failed { exhausted: false },
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
