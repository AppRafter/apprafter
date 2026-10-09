// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Device-owner authentication, behind one trait so the lock and the destructive-op gesture
//! never depend on an OS. A release build has [`SystemAuthenticator`], the OS's own
//! (`apprafter_os_auth`): Windows Hello or the credential dialog, macOS LocalAuthentication,
//! Linux polkit with PAM behind the app's own password field. Where none of those exists it has
//! [`NoAuthenticator`] — unavailable, so the lock fails closed (it cannot be enabled). A test
//! build has `FakeAuthenticator` (compiled only with the `test-build` feature, so no link: a doc
//! build without the feature would not find it). Which one is [`choice`]'s answer, a pure
//! function of the build's [`AllowListEnv`], so the release answer is tested in every build.
//!
//! The password from the app's own field is a [`Zeroizing`] string from the moment a command
//! receives it, wiped when dropped. Nothing logs it, prints it or keeps it.

use std::fmt;
use std::sync::Arc;

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{AuthInfo, AuthOutcome, Settings, UnavailableReason};
use apprafter_os_auth::Action;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use apprafter_os_auth::OsAuthenticator;
use zeroize::Zeroizing;

use crate::env::{AllowListEnv, TEST_PASSWORD_ENV};
use crate::ops::Clock;

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

/// The OS's action for `purpose`, which picks the prompt's text (and, on Linux, the polkit
/// action).
pub fn action(purpose: &AuthPurpose) -> Action {
    match purpose {
        AuthPurpose::Unlock => Action::Unlock,
        AuthPurpose::Confirm { .. } => Action::Confirm,
    }
}

/// What a check of the app's own password field answered: the outcome, and what the OS said on
/// the way (PAM's messages, such as "Password expired"; never a secret).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordAnswer {
    pub outcome: AuthOutcome,
    pub messages: Vec<String>,
}

impl PasswordAnswer {
    /// The OS prompts itself here, so the app's field is not its way: nothing was checked.
    pub const NOT_HERE: PasswordAnswer = PasswordAnswer {
        outcome: AuthOutcome::Unavailable {
            reason: UnavailableReason::NotPermittedHere,
        },
        messages: Vec::new(),
    };
}

impl From<AuthOutcome> for PasswordAnswer {
    /// An OS prompt's answer: it says nothing besides.
    fn from(outcome: AuthOutcome) -> Self {
        Self {
            outcome,
            messages: Vec::new(),
        }
    }
}

pub trait Authenticator: Send + Sync {
    fn info(&self) -> AuthInfo;
    /// Blocks until the OS prompt answers; `cancel` closes the prompt (lock-on-sleep, quit).
    /// Never call on an async worker or the main thread.
    fn verify(&self, purpose: &AuthPurpose, cancel: &CancellationToken) -> AuthOutcome;
    /// Checks the password the owner typed into the app's own field, which the page shows where
    /// the OS cannot prompt ([`AuthInfo::password_field`], Linux's PAM path). Blocks as
    /// [`verify`](Self::verify) does. Where the OS prompts itself this is
    /// [`PasswordAnswer::NOT_HERE`], and the password is not looked at: an OS whose policy asks
    /// for more than the user's own password must not be got round through the field.
    fn verify_password(
        &self,
        _purpose: &AuthPurpose,
        password: Zeroizing<String>,
        _cancel: &CancellationToken,
    ) -> PasswordAnswer {
        drop(password);
        PasswordAnswer::NOT_HERE
    }
    /// The app has just locked (every transition to locked): an authenticator forgets what it
    /// learnt while unlocked. Called under the lock machine's lock, so it must be quick and
    /// must never call back into the machine.
    fn locked(&self) {}
    /// The settings in use: at start, and after every save.
    fn apply_settings(&self, _settings: &Settings) {}
    /// Windows: the main window (its `HWND`), which every prompt is parented to; until it is
    /// known, a prompt opens nothing. Elsewhere nothing to do.
    fn set_window(&self, _hwnd: isize) {}
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

/// What [`SystemAuthenticator`] asks of the OS's authenticator
/// ([`apprafter_os_auth::OsAuthenticator`]): one implementation per OS in the app, a recording
/// fake in the tests. So what the shell adds — the action for each purpose, its clock, which
/// setting reaches the OS — is tested on every OS without asking this machine's OS anything.
/// What an OS does not have is a default that does nothing.
pub(crate) trait Backend: Send + Sync {
    fn info(&self) -> AuthInfo;
    fn verify(&self, action: Action, cancel: &CancellationToken) -> AuthOutcome;
    /// Linux's PAM path; elsewhere the field is not the OS's way.
    fn verify_password(
        &self,
        _action: Action,
        password: Zeroizing<String>,
        _cancel: &CancellationToken,
        _now_monotonic_ms: u64,
    ) -> PasswordAnswer {
        drop(password);
        PasswordAnswer::NOT_HERE
    }
    /// Linux: forget that the last polkit dialog found no authentication agent.
    fn forget_missing_agent(&self) {}
    /// Windows: the `hello` setting.
    fn set_hello(&self, _on: bool) {}
    /// Windows: the window prompts are parented to.
    fn set_window(&self, _hwnd: isize) {}
}

/// The OS's own authentication (the module docs list it per OS).
///
/// - Every purpose is asked as its [`action`]. The password field's check reads the shell's
///   monotonic clock, which PAM's back-off counts in.
/// - Linux: each lock forgets a missing polkit agent ([`Authenticator::locked`]), so an agent
///   the user started since the last "no agent" answer is asked again on the lock screen.
/// - Windows: the `hello` setting chooses Hello or the credential dialog, and the prompts are
///   parented to the main window once it exists ([`Authenticator::set_window`]).
pub struct SystemAuthenticator {
    os: Box<dyn Backend>,
    clock: Arc<dyn Clock>,
}

impl SystemAuthenticator {
    /// The OS's authenticator, reading `clock` for the password back-off.
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self::with(Box::new(OsAuthenticator::new()), clock)
    }

    pub(crate) fn with(os: Box<dyn Backend>, clock: Arc<dyn Clock>) -> Self {
        Self { os, clock }
    }
}

impl Authenticator for SystemAuthenticator {
    fn info(&self) -> AuthInfo {
        self.os.info()
    }

    fn verify(&self, purpose: &AuthPurpose, cancel: &CancellationToken) -> AuthOutcome {
        self.os.verify(action(purpose), cancel)
    }

    fn verify_password(
        &self,
        purpose: &AuthPurpose,
        password: Zeroizing<String>,
        cancel: &CancellationToken,
    ) -> PasswordAnswer {
        self.os
            .verify_password(action(purpose), password, cancel, self.clock.monotonic_ms())
    }

    fn locked(&self) {
        self.os.forget_missing_agent();
    }

    fn apply_settings(&self, settings: &Settings) {
        self.os.set_hello(settings.hello);
    }

    fn set_window(&self, hwnd: isize) {
        self.os.set_window(hwnd);
    }
}

#[cfg(target_os = "linux")]
impl Backend for OsAuthenticator {
    fn info(&self) -> AuthInfo {
        OsAuthenticator::info(self)
    }

    fn verify(&self, action: Action, cancel: &CancellationToken) -> AuthOutcome {
        OsAuthenticator::verify(self, action, cancel)
    }

    fn verify_password(
        &self,
        action: Action,
        password: Zeroizing<String>,
        cancel: &CancellationToken,
        now_monotonic_ms: u64,
    ) -> PasswordAnswer {
        said(OsAuthenticator::verify_password(
            self,
            action,
            password,
            cancel,
            now_monotonic_ms,
        ))
    }

    fn forget_missing_agent(&self) {
        OsAuthenticator::reset_agent_memory(self);
    }
}

/// A PAM check as the shell's answer.
#[cfg(target_os = "linux")]
fn said(check: apprafter_os_auth::linux::PasswordCheck) -> PasswordAnswer {
    PasswordAnswer {
        outcome: check.outcome,
        messages: check.messages,
    }
}

#[cfg(target_os = "macos")]
impl Backend for OsAuthenticator {
    fn info(&self) -> AuthInfo {
        OsAuthenticator::info(self)
    }

    fn verify(&self, action: Action, cancel: &CancellationToken) -> AuthOutcome {
        OsAuthenticator::verify(self, action, cancel)
    }
}

#[cfg(windows)]
impl Backend for OsAuthenticator {
    fn info(&self) -> AuthInfo {
        OsAuthenticator::info(self)
    }

    fn verify(&self, action: Action, cancel: &CancellationToken) -> AuthOutcome {
        OsAuthenticator::verify(self, action, cancel)
    }

    fn set_hello(&self, on: bool) {
        OsAuthenticator::set_hello(self, on);
    }

    fn set_window(&self, hwnd: isize) {
        OsAuthenticator::set_window(self, hwnd);
    }
}

/// Which authenticator a build uses.
#[derive(Clone, PartialEq, Eq)]
pub enum Choice {
    /// The OS's own: every release build.
    System,
    /// The test build's scripted fake, with the password its own field accepts, if the walk set
    /// one (`APPRAFTER_DESKTOP_TEST_PASSWORD`).
    Fake { password: Option<String> },
}

impl fmt::Debug for Choice {
    /// Never the password, not even a test build's.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Choice::System => f.write_str("System"),
            Choice::Fake { password } => f
                .debug_struct("Fake")
                .field("password", &password.as_ref().map(|_| "…"))
                .finish(),
        }
    }
}

/// The authenticator for `env`'s build: the OS's in a release, the fake in a test build
/// ([`AllowListEnv::test_build`], which follows the `test-build` feature). Pure, so the release
/// answer is tested in a test build too.
pub fn choice(env: &AllowListEnv) -> Choice {
    if env.test_build() {
        Choice::Fake {
            password: env
                .var_os(TEST_PASSWORD_ENV)
                .and_then(|p| p.into_string().ok()),
        }
    } else {
        Choice::System
    }
}

/// The authenticator `choice` names; the OS's reads `clock`.
pub fn authenticator(choice: Choice, clock: Arc<dyn Clock>) -> Arc<dyn Authenticator> {
    match choice {
        Choice::System => system(clock),
        #[cfg(any(test, feature = "test-build"))]
        Choice::Fake { password } => {
            let fake = FakeAuthenticator::new();
            Arc::new(match password {
                Some(password) => fake.with_password(password),
                None => fake,
            })
        }
        // `choice` follows the build's own flag, so a release never names the fake; were it to,
        // nothing verifies the owner: fail closed.
        #[cfg(not(any(test, feature = "test-build")))]
        Choice::Fake { .. } => Arc::new(NoAuthenticator),
    }
}

/// The OS's authenticator where `apprafter_os_auth` has one, else none.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn system(clock: Arc<dyn Clock>) -> Arc<dyn Authenticator> {
    Arc::new(SystemAuthenticator::new(clock))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn system(_clock: Arc<dyn Clock>) -> Arc<dyn Authenticator> {
    Arc::new(NoAuthenticator)
}

#[cfg(any(test, feature = "test-build"))]
pub use fake::FakeAuthenticator;

#[cfg(any(test, feature = "test-build"))]
mod fake {
    use std::collections::VecDeque;
    use std::fmt;
    use std::sync::{Mutex, MutexGuard};

    use apprafter_core::CancellationToken;
    use apprafter_desktop_ipc::{AuthInfo, AuthMethod, AuthOutcome, CancelledBy};
    use zeroize::Zeroizing;

    use super::{AuthPurpose, Authenticator, PasswordAnswer};

    /// The test build's authenticator: answers from a script, `Verified` once the script
    /// runs out, and remembers every purpose it was asked for. A request whose token has
    /// already tripped is answered `Cancelled { by: App }` without using the script, as a
    /// real prompt closed by the app would be.
    ///
    /// With a password ([`with_password`](Self::with_password)) it also has the app's own
    /// password field, as Linux's PAM path does: the field checks that password (the script, if
    /// it has an answer, comes first), and every answer but `Verified` says what
    /// [`saying`](Self::saying) set, as PAM would. Without one the field is not its way
    /// ([`PasswordAnswer::NOT_HERE`]).
    #[derive(Default)]
    pub struct FakeAuthenticator {
        script: Mutex<VecDeque<AuthOutcome>>,
        asked: Mutex<Vec<AuthPurpose>>,
        password: Option<Zeroizing<String>>,
        said: Mutex<Vec<String>>,
    }

    impl fmt::Debug for FakeAuthenticator {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("FakeAuthenticator")
                .field("asked", &*lock(&self.asked))
                .field("password_field", &self.password.is_some())
                .finish_non_exhaustive()
        }
    }

    impl FakeAuthenticator {
        pub fn new() -> Self {
            Self::default()
        }

        /// The app's own password field, accepting `password`.
        pub fn with_password(self, password: impl Into<String>) -> Self {
            Self {
                password: Some(Zeroizing::new(password.into())),
                ..self
            }
        }

        /// Answer the next unanswered request with `outcome`.
        pub fn then(&self, outcome: AuthOutcome) -> &Self {
            lock(&self.script).push_back(outcome);
            self
        }

        /// What the password field says with every answer but `Verified`.
        pub fn saying(&self, messages: &[&str]) -> &Self {
            *lock(&self.said) = messages.iter().map(|m| (*m).to_owned()).collect();
            self
        }

        /// Every purpose asked so far, in order, through the prompt or the field.
        pub fn asked(&self) -> Vec<AuthPurpose> {
            lock(&self.asked).clone()
        }
    }

    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|p| p.into_inner())
    }

    const APP_CANCELLED: AuthOutcome = AuthOutcome::Cancelled {
        by: CancelledBy::App,
    };

    impl Authenticator for FakeAuthenticator {
        fn info(&self) -> AuthInfo {
            AuthInfo {
                available: true,
                method: Some(AuthMethod::Fake),
                unavailable: None,
                biometrics_choice: false,
                password_field: self.password.is_some(),
            }
        }

        fn verify(&self, purpose: &AuthPurpose, cancel: &CancellationToken) -> AuthOutcome {
            lock(&self.asked).push(purpose.clone());
            if cancel.is_cancelled() {
                return APP_CANCELLED;
            }
            lock(&self.script)
                .pop_front()
                .unwrap_or(AuthOutcome::Verified)
        }

        fn verify_password(
            &self,
            purpose: &AuthPurpose,
            password: Zeroizing<String>,
            cancel: &CancellationToken,
        ) -> PasswordAnswer {
            let Some(accepted) = &self.password else {
                return PasswordAnswer::NOT_HERE;
            };
            lock(&self.asked).push(purpose.clone());
            if cancel.is_cancelled() {
                return APP_CANCELLED.into();
            }
            let outcome = lock(&self.script).pop_front().unwrap_or_else(|| {
                if *password == **accepted {
                    AuthOutcome::Verified
                } else {
                    AuthOutcome::Failed { exhausted: false }
                }
            });
            let messages = match outcome {
                AuthOutcome::Verified => Vec::new(),
                _ => lock(&self.said).clone(),
            };
            PasswordAnswer { outcome, messages }
        }
    }
}

/// The OS's authenticator as a script that records what it is asked, for the shell's own
/// tests: a [`SystemAuthenticator`] over it is the release authenticator without the OS.
#[cfg(test)]
pub(crate) mod test_os {
    use std::sync::{Arc, Mutex};

    use apprafter_core::CancellationToken;
    use apprafter_desktop_ipc::{AuthInfo, AuthOutcome};
    use apprafter_os_auth::Action;
    use zeroize::Zeroizing;

    use super::{Backend, PasswordAnswer, SystemAuthenticator};
    use crate::ops::test_clock::ManualClock;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Call {
        Info,
        Verify(Action),
        /// The password the OS got, and the clock.
        Password(Action, String, u64),
        ForgetAgent,
        Hello(bool),
        Window(isize),
    }

    pub(crate) type Calls = Arc<Mutex<Vec<Call>>>;

    /// Reports `info`, answers every prompt `verify` and every password `password`.
    pub(crate) struct ScriptedOs {
        info: AuthInfo,
        verify: AuthOutcome,
        password: PasswordAnswer,
        calls: Calls,
    }

    impl ScriptedOs {
        /// An OS without a password path of its own.
        pub(crate) fn new(info: AuthInfo, verify: AuthOutcome) -> Self {
            Self {
                info,
                verify,
                password: PasswordAnswer::NOT_HERE,
                calls: Calls::default(),
            }
        }

        /// Its password path answers `answer`.
        pub(crate) fn saying(self, answer: PasswordAnswer) -> Self {
            Self {
                password: answer,
                ..self
            }
        }

        pub(crate) fn calls(&self) -> Calls {
            self.calls.clone()
        }

        fn record(&self, call: Call) {
            self.calls.lock().unwrap().push(call);
        }
    }

    impl Backend for ScriptedOs {
        fn info(&self) -> AuthInfo {
            self.record(Call::Info);
            self.info.clone()
        }

        fn verify(&self, action: Action, _cancel: &CancellationToken) -> AuthOutcome {
            self.record(Call::Verify(action));
            self.verify
        }

        fn verify_password(
            &self,
            action: Action,
            password: Zeroizing<String>,
            _cancel: &CancellationToken,
            now_monotonic_ms: u64,
        ) -> PasswordAnswer {
            self.record(Call::Password(
                action,
                password.to_string(),
                now_monotonic_ms,
            ));
            self.password.clone()
        }

        fn forget_missing_agent(&self) {
            self.record(Call::ForgetAgent);
        }

        fn set_hello(&self, on: bool) {
            self.record(Call::Hello(on));
        }

        fn set_window(&self, hwnd: isize) {
            self.record(Call::Window(hwnd));
        }
    }

    /// The release authenticator over `os`, and what `os` is asked.
    pub(crate) fn system(os: ScriptedOs) -> (Arc<SystemAuthenticator>, Calls) {
        let calls = os.calls();
        let auth = SystemAuthenticator::with(Box::new(os), Arc::new(ManualClock::at(0)));
        (Arc::new(auth), calls)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::sync::Arc;

    use apprafter_core::CancellationToken;
    use apprafter_desktop_ipc::{
        AuthInfo, AuthMethod, AuthOutcome, CancelledBy, Settings, UnavailableReason,
    };
    use apprafter_os_auth::Action;
    use zeroize::Zeroizing;

    use super::test_os::{Call, Calls, ScriptedOs};
    use super::{
        action, authenticator, choice, AuthPurpose, Authenticator, Backend, Choice,
        FakeAuthenticator, NoAuthenticator, PasswordAnswer, SystemAuthenticator,
    };
    use crate::env::AllowListEnv;
    use crate::ops::test_clock::ManualClock;
    use crate::ops::Clock;

    fn confirm() -> AuthPurpose {
        AuthPurpose::Confirm {
            target: Some("prod".into()),
            verb: "delete".into(),
        }
    }

    fn password(text: &str) -> Zeroizing<String> {
        Zeroizing::new(text.to_owned())
    }

    fn env(test_build: bool, pairs: &[(&str, &str)]) -> AllowListEnv {
        let map: BTreeMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        AllowListEnv::with_lookup(test_build, move |k| map.get(k).cloned())
    }

    /// The release answer is tested in this build too, whichever features it has.
    #[test]
    fn a_release_uses_the_os_and_a_test_build_the_fake() {
        let walk = [("APPRAFTER_DESKTOP_TEST_PASSWORD", "open sesame")];
        assert_eq!(choice(&env(false, &[])), Choice::System);
        assert_eq!(
            choice(&env(false, &walk)),
            Choice::System,
            "a release has no test password, set or not"
        );
        assert_eq!(choice(&env(true, &[])), Choice::Fake { password: None });
        assert_eq!(
            choice(&env(true, &walk)),
            Choice::Fake {
                password: Some("open sesame".into())
            }
        );
        let printed = format!("{:?}", choice(&env(true, &walk)));
        assert!(!printed.contains("sesame"), "{printed}");
    }

    #[test]
    fn the_fake_choice_is_the_fake_with_its_password_field_if_one_was_set() {
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::at(0));
        let plain = authenticator(Choice::Fake { password: None }, clock.clone());
        assert_eq!(plain.info(), FakeAuthenticator::new().info());
        let with = authenticator(
            Choice::Fake {
                password: Some("open sesame".into()),
            },
            clock,
        );
        assert!(with.info().password_field);
        let token = CancellationToken::new();
        let answer = with.verify_password(&AuthPurpose::Unlock, password("open sesame"), &token);
        assert_eq!(answer.outcome, AuthOutcome::Verified);
    }

    #[test]
    fn every_purpose_is_asked_as_its_action() {
        assert_eq!(action(&AuthPurpose::Unlock), Action::Unlock);
        assert_eq!(action(&confirm()), Action::Confirm);
    }

    #[test]
    fn without_a_backend_nothing_is_available_or_verified() {
        let info = NoAuthenticator.info();
        assert!(!info.available);
        assert_eq!(info.method, None);
        assert_eq!(info.unavailable, Some(UnavailableReason::NoBackend));
        assert!(!info.biometrics_choice && !info.password_field);
        for purpose in [AuthPurpose::Unlock, confirm()] {
            let token = CancellationToken::new();
            assert_eq!(
                NoAuthenticator.verify(&purpose, &token),
                AuthOutcome::Unavailable {
                    reason: UnavailableReason::NoBackend
                }
            );
            assert_eq!(
                NoAuthenticator.verify_password(&purpose, password("guess"), &token),
                PasswordAnswer::NOT_HERE,
                "no field where nothing could check it"
            );
        }
        assert_eq!(
            PasswordAnswer::NOT_HERE.outcome,
            AuthOutcome::Unavailable {
                reason: UnavailableReason::NotPermittedHere
            }
        );
    }

    fn system() -> (SystemAuthenticator, Calls, Arc<ManualClock>) {
        let os = ScriptedOs::new(
            AuthInfo {
                method: Some(AuthMethod::Pam),
                password_field: true,
                ..FakeAuthenticator::new().info()
            },
            AuthOutcome::Cancelled {
                by: CancelledBy::User,
            },
        )
        .saying(PasswordAnswer {
            outcome: AuthOutcome::Failed { exhausted: true },
            messages: vec!["Password expired".into()],
        });
        let calls = os.calls();
        let clock = Arc::new(ManualClock::at(1_700_000_000_000));
        (
            SystemAuthenticator::with(Box::new(os), clock.clone()),
            calls,
            clock,
        )
    }

    #[test]
    fn the_os_is_asked_for_each_purpose_s_action_and_answers_as_it_is() {
        let (auth, calls, _) = system();
        let token = CancellationToken::new();
        assert_eq!(auth.info().method, Some(AuthMethod::Pam));
        assert_eq!(
            auth.verify(&AuthPurpose::Unlock, &token),
            AuthOutcome::Cancelled {
                by: CancelledBy::User
            }
        );
        auth.verify(&confirm(), &token);
        assert_eq!(
            *calls.lock().unwrap(),
            [
                Call::Info,
                Call::Verify(Action::Unlock),
                Call::Verify(Action::Confirm)
            ]
        );
    }

    #[test]
    fn the_password_reaches_the_os_with_the_shell_s_monotonic_clock() {
        let (auth, calls, clock) = system();
        clock.advance(42);
        let token = CancellationToken::new();
        let answer = auth.verify_password(&confirm(), password("hunter2"), &token);
        assert_eq!(
            answer,
            PasswordAnswer {
                outcome: AuthOutcome::Failed { exhausted: true },
                messages: vec!["Password expired".into()],
            },
            "what the OS said comes back with its answer"
        );
        // ManualClock's monotonic reading starts at 1_000, far from any wall time.
        assert_eq!(
            *calls.lock().unwrap(),
            [Call::Password(Action::Confirm, "hunter2".into(), 1_042)]
        );
    }

    #[test]
    fn a_lock_forgets_a_missing_agent_and_the_settings_and_window_reach_the_os() {
        let (auth, calls, _) = system();
        auth.locked();
        for hello in [false, true] {
            auth.apply_settings(&Settings {
                hello,
                ..Settings::default()
            });
        }
        auth.set_window(0x5150);
        assert_eq!(
            *calls.lock().unwrap(),
            [
                Call::ForgetAgent,
                Call::Hello(false),
                Call::Hello(true),
                Call::Window(0x5150)
            ]
        );
    }

    /// An OS without a password path: the trait's defaults, the password unseen.
    #[test]
    fn where_the_os_prompts_itself_the_field_is_not_its_way() {
        struct Prompts;
        impl Backend for Prompts {
            fn info(&self) -> AuthInfo {
                FakeAuthenticator::new().info()
            }
            fn verify(&self, _action: Action, _cancel: &CancellationToken) -> AuthOutcome {
                AuthOutcome::Verified
            }
        }
        let auth = SystemAuthenticator::with(Box::new(Prompts), Arc::new(ManualClock::at(0)));
        let token = CancellationToken::new();
        for purpose in [AuthPurpose::Unlock, confirm()] {
            assert_eq!(
                auth.verify_password(&purpose, password("guess"), &token),
                PasswordAnswer::NOT_HERE
            );
        }
        // And the hooks an OS does not have do nothing.
        auth.locked();
        auth.apply_settings(&Settings::default());
        auth.set_window(1);
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
        assert!(!info.password_field);
    }

    #[test]
    fn a_tripped_token_closes_the_fake_prompt_as_the_app() {
        let fake = FakeAuthenticator::new().with_password("pw");
        let token = CancellationToken::new();
        token.cancel();
        let app = AuthOutcome::Cancelled {
            by: CancelledBy::App,
        };
        assert_eq!(fake.verify(&confirm(), &token), app);
        assert_eq!(
            fake.verify_password(&confirm(), password("pw"), &token),
            app.into()
        );
        assert_eq!(
            fake.asked(),
            vec![confirm(), confirm()],
            "the requests are still recorded"
        );
    }

    #[test]
    fn the_fake_s_field_checks_its_password_and_says_what_pam_would() {
        let fake = FakeAuthenticator::new().with_password("open sesame");
        fake.saying(&["Authentication failure"]);
        assert!(fake.info().password_field);
        let token = CancellationToken::new();
        assert_eq!(
            fake.verify_password(&AuthPurpose::Unlock, password("open sesame"), &token),
            AuthOutcome::Verified.into(),
            "nothing said with a yes"
        );
        assert_eq!(
            fake.verify_password(&confirm(), password("open sesame!"), &token),
            PasswordAnswer {
                outcome: AuthOutcome::Failed { exhausted: false },
                messages: vec!["Authentication failure".into()],
            }
        );
        // The script comes first, for the field too.
        fake.then(AuthOutcome::Busy);
        assert_eq!(
            fake.verify_password(&AuthPurpose::Unlock, password("open sesame"), &token)
                .outcome,
            AuthOutcome::Busy
        );
        assert_eq!(
            fake.asked(),
            vec![AuthPurpose::Unlock, confirm(), AuthPurpose::Unlock]
        );
        let printed = format!("{fake:?}");
        assert!(!printed.contains("sesame"), "{printed}");
    }

    #[test]
    fn without_a_password_the_fake_s_field_is_not_its_way() {
        let fake = FakeAuthenticator::new();
        let answer = fake.verify_password(
            &AuthPurpose::Unlock,
            password("anything"),
            &CancellationToken::new(),
        );
        assert_eq!(answer, PasswordAnswer::NOT_HERE);
        assert!(fake.asked().is_empty(), "nothing was checked");
    }
}
