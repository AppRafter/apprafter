// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Device-owner authentication, behind one trait so the lock and the destructive-op gesture
//! never depend on an OS. The real backends are D.2d. Until then a release build has
//! [`NoAuthenticator`] — unavailable, so the lock fails closed (it cannot be enabled) — and a
//! test build has [`FakeAuthenticator`].

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{AuthInfo, AuthOutcome, UnavailableReason};

/// Why the OS is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthPurpose {
    Unlock,
    /// Run one destructive or approving operation.
    Confirm {
        target: Option<String>,
        verb: String,
    },
}

pub trait Authenticator: Send + Sync {
    fn info(&self) -> AuthInfo;
    /// Blocks until the OS prompt answers; `cancel` closes the prompt (lock-on-sleep, quit).
    /// Never call on an async worker or the main thread.
    fn verify(&self, purpose: &AuthPurpose, cancel: &CancellationToken) -> AuthOutcome;
}

/// No OS backend: nothing can be verified, so whatever needs the owner is refused.
pub struct NoAuthenticator;

impl Authenticator for NoAuthenticator {
    fn info(&self) -> AuthInfo {
        AuthInfo {
            available: false,
            method: None,
            unavailable: Some(UnavailableReason::NoBackend),
            biometrics_choice: false,
            password_field: false,
        }
    }

    fn verify(&self, _purpose: &AuthPurpose, _cancel: &CancellationToken) -> AuthOutcome {
        AuthOutcome::Unavailable {
            reason: UnavailableReason::NoBackend,
        }
    }
}

#[cfg(any(test, feature = "test-build"))]
pub use fake::FakeAuthenticator;

#[cfg(any(test, feature = "test-build"))]
mod fake {
    use std::collections::VecDeque;
    use std::sync::{Mutex, MutexGuard};

    use apprafter_core::CancellationToken;
    use apprafter_desktop_ipc::{AuthInfo, AuthMethod, AuthOutcome, CancelledBy};

    use super::{AuthPurpose, Authenticator};

    /// The test build's authenticator: answers from a script, `Verified` once the script
    /// runs out, and remembers every purpose it was asked for. A request whose token has
    /// already tripped is answered `Cancelled { by: App }` without using the script, as a
    /// real prompt closed by the app would be.
    #[derive(Debug, Default)]
    pub struct FakeAuthenticator {
        script: Mutex<VecDeque<AuthOutcome>>,
        asked: Mutex<Vec<AuthPurpose>>,
    }

    impl FakeAuthenticator {
        pub fn new() -> Self {
            Self::default()
        }

        /// Answer the next unanswered request with `outcome`.
        pub fn then(&self, outcome: AuthOutcome) -> &Self {
            lock(&self.script).push_back(outcome);
            self
        }

        /// Every purpose asked so far, in order.
        pub fn asked(&self) -> Vec<AuthPurpose> {
            lock(&self.asked).clone()
        }
    }

    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|p| p.into_inner())
    }

    impl Authenticator for FakeAuthenticator {
        fn info(&self) -> AuthInfo {
            AuthInfo {
                available: true,
                method: Some(AuthMethod::Fake),
                unavailable: None,
                biometrics_choice: false,
                password_field: false,
            }
        }

        fn verify(&self, purpose: &AuthPurpose, cancel: &CancellationToken) -> AuthOutcome {
            lock(&self.asked).push(purpose.clone());
            if cancel.is_cancelled() {
                return AuthOutcome::Cancelled {
                    by: CancelledBy::App,
                };
            }
            lock(&self.script)
                .pop_front()
                .unwrap_or(AuthOutcome::Verified)
        }
    }
}

#[cfg(test)]
mod tests {
    use apprafter_core::CancellationToken;
    use apprafter_desktop_ipc::{AuthMethod, AuthOutcome, CancelledBy, UnavailableReason};

    use super::{AuthPurpose, Authenticator, FakeAuthenticator, NoAuthenticator};

    fn confirm() -> AuthPurpose {
        AuthPurpose::Confirm {
            target: Some("prod".into()),
            verb: "delete".into(),
        }
    }

    #[test]
    fn without_a_backend_nothing_is_available_or_verified() {
        let info = NoAuthenticator.info();
        assert!(!info.available);
        assert_eq!(info.method, None);
        assert_eq!(info.unavailable, Some(UnavailableReason::NoBackend));
        assert!(!info.biometrics_choice && !info.password_field);
        for purpose in [AuthPurpose::Unlock, confirm()] {
            assert_eq!(
                NoAuthenticator.verify(&purpose, &CancellationToken::new()),
                AuthOutcome::Unavailable {
                    reason: UnavailableReason::NoBackend
                }
            );
        }
    }

    #[test]
    fn the_fake_answers_its_script_in_order_then_verifies() {
        let fake = FakeAuthenticator::new();
        fake.then(AuthOutcome::Cancelled {
            by: CancelledBy::User,
        })
        .then(AuthOutcome::Busy);
        let token = CancellationToken::new();
        assert_eq!(
            fake.verify(&confirm(), &token),
            AuthOutcome::Cancelled {
                by: CancelledBy::User
            }
        );
        assert_eq!(fake.verify(&AuthPurpose::Unlock, &token), AuthOutcome::Busy);
        assert_eq!(fake.verify(&confirm(), &token), AuthOutcome::Verified);
        assert_eq!(
            fake.asked(),
            vec![confirm(), AuthPurpose::Unlock, confirm()]
        );
    }

    #[test]
    fn the_fake_is_available_as_itself() {
        let info = FakeAuthenticator::new().info();
        assert!(info.available);
        assert_eq!(info.method, Some(AuthMethod::Fake));
        assert_eq!(info.unavailable, None);
    }

    #[test]
    fn a_tripped_token_closes_the_fake_prompt_as_the_app() {
        let fake = FakeAuthenticator::new();
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(
            fake.verify(&confirm(), &token),
            AuthOutcome::Cancelled {
                by: CancelledBy::App
            }
        );
        assert_eq!(
            fake.asked(),
            vec![confirm()],
            "the request is still recorded"
        );
    }
}
