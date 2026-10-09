// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Windows: Windows Hello, and the Windows credential dialog where Hello cannot prompt or the
//! owner turned it off (the `hello` setting).
//!
//! A request:
//! 1. With the `hello` setting on, Hello's availability (`CheckAvailabilityAsync`, which must
//!    come first). Available: Hello's prompt. Busy: `Busy`, without a prompt. Anything else,
//!    including availability that cannot be asked: the credential dialog.
//! 2. Hello's prompt, parented to the app's window ([`hello`]). Its answer maps through
//!    [`map_windows_hello`], except that a Hello that turns out not to be set up after all, or
//!    whose prompt cannot be started (Windows before build 22000 has no interop), falls back to
//!    the credential dialog, as the availability would have: the dialog checks the account's
//!    password itself, so a fallback never lets a request through unchecked.
//! 3. The credential dialog ([`credui`]), with the back-off.
//!
//! Both prompts need the app's window, which the shell passes in once it exists
//! ([`OsAuthenticator::set_window`]): without one, a request is `Unavailable { NotInteractive }`
//! and opens nothing, since an unparented prompt can open behind the app.
//!
//! Every method blocks; call them on a blocking worker, never on an async worker or the main
//! thread (see [`hello`]).

pub mod credui;
pub mod hello;

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::Instant;

use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{AuthInfo, AuthMethod, AuthOutcome, CancelledBy, UnavailableReason};

use self::credui::{CredentialCheck, CredentialUi, SystemCredentialUi};
use self::hello::{Asked, Hello, SystemHello};
use crate::outcome::{map_windows_availability, map_windows_hello, WindowsRoute};
use crate::Action;

const APP_CANCELLED: AuthOutcome = AuthOutcome::Cancelled {
    by: CancelledBy::App,
};

/// The text both prompts show for `action`, under their own "Windows Security" title.
pub const fn message(action: Action) -> &'static str {
    match action {
        Action::Unlock => "Unlock AppRafter Desktop",
        Action::Confirm => "Approve or run a destructive AppRafter operation",
    }
}

/// A monotonic millisecond reading for the back-off.
type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

/// Device-owner authentication on Windows. The shell keeps one, so the back-off is per process.
pub struct OsAuthenticator {
    hello: Box<dyn Hello>,
    credential: CredentialCheck,
    clock: Clock,
    /// The app's window handle (`HWND`), 0 until the shell passes it.
    window: AtomicIsize,
    /// The `hello` setting: Hello when it can prompt, else the credential dialog.
    prefer_hello: AtomicBool,
}

impl Default for OsAuthenticator {
    fn default() -> Self {
        Self::new()
    }
}

impl OsAuthenticator {
    /// The system's Windows Hello and credential dialog, with the `hello` setting on and no
    /// window yet.
    pub fn new() -> Self {
        let started = Instant::now();
        Self::with(
            Box::new(SystemHello),
            Box::new(SystemCredentialUi),
            Box::new(move || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
        )
    }

    fn with(hello: Box<dyn Hello>, credential: Box<dyn CredentialUi>, clock: Clock) -> Self {
        Self {
            hello,
            credential: CredentialCheck::new(credential),
            clock,
            window: AtomicIsize::new(0),
            prefer_hello: AtomicBool::new(true),
        }
    }

    /// The app's main window (its `HWND`, as an integer), which every prompt is parented to.
    pub fn set_window(&self, hwnd: isize) {
        self.window.store(hwnd, Ordering::SeqCst);
    }

    /// The `hello` setting: on, Windows Hello when it can prompt; off, always the credential
    /// dialog.
    pub fn set_hello(&self, on: bool) {
        self.prefer_hello.store(on, Ordering::SeqCst);
    }

    /// Hello's availability as a route; anything that cannot be asked is the credential dialog.
    fn hello_route(&self, cancel: &CancellationToken) -> Result<WindowsRoute, AuthOutcome> {
        match self.hello.availability(cancel) {
            Asked::Answered(raw) => Ok(map_windows_availability(raw)),
            Asked::NotStarted | Asked::Failed => Ok(WindowsRoute::Credential),
            Asked::Cancelled => Err(APP_CANCELLED),
        }
    }

    /// What the lock screen and the settings show, probed without a prompt: Hello where it can
    /// prompt and the `hello` setting is on, else the credential dialog, which is always there.
    /// `biometrics_choice` says Hello can prompt, so the settings offer the `hello` switch.
    pub fn info(&self) -> AuthInfo {
        let hello = matches!(
            self.hello_route(&CancellationToken::new()),
            Ok(WindowsRoute::Hello | WindowsRoute::Busy)
        );
        let method = if hello && self.prefer_hello.load(Ordering::SeqCst) {
            AuthMethod::WindowsHello
        } else {
            AuthMethod::WindowsCredential
        };
        AuthInfo {
            available: true,
            method: Some(method),
            unavailable: None,
            biometrics_choice: hello,
            password_field: false,
        }
    }

    /// Asks Windows to authenticate the owner for `action` (the module docs give the steps).
    /// Blocks until the prompt answers; `cancel` closes Hello's prompt, and makes whatever the
    /// credential dialog returns `Cancelled { by: App }`. A token already tripped opens
    /// nothing.
    pub fn verify(&self, action: Action, cancel: &CancellationToken) -> AuthOutcome {
        if cancel.is_cancelled() {
            return APP_CANCELLED;
        }
        let window = self.window.load(Ordering::SeqCst);
        if window == 0 {
            return AuthOutcome::Unavailable {
                reason: UnavailableReason::NotInteractive,
            };
        }
        let route = if self.prefer_hello.load(Ordering::SeqCst) {
            match self.hello_route(cancel) {
                Ok(route) => route,
                Err(outcome) => return outcome,
            }
        } else {
            WindowsRoute::Credential
        };
        match route {
            WindowsRoute::Busy => AuthOutcome::Busy,
            WindowsRoute::Credential => self.by_dialog(window, action, cancel),
            WindowsRoute::Hello => match self.hello.verify(window, message(action), cancel) {
                Asked::Answered(raw) => match map_windows_hello(raw) {
                    AuthOutcome::Unavailable { .. } => self.by_dialog(window, action, cancel),
                    outcome => outcome,
                },
                Asked::NotStarted => self.by_dialog(window, action, cancel),
                Asked::Failed => AuthOutcome::Failed { exhausted: false },
                Asked::Cancelled => APP_CANCELLED,
            },
        }
    }

    fn by_dialog(&self, window: isize, action: Action, cancel: &CancellationToken) -> AuthOutcome {
        self.credential
            .verify(window, message(action), cancel, (self.clock)())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, Mutex};

    use super::credui::{Credentials, Prompted};
    use super::*;
    use zeroize::Zeroizing;

    const WINDOW: isize = 0x5150;
    const OWNER: &str = r"DESKTOP-1\Ada";

    const AVAILABLE: i32 = 0;
    const DEVICE_NOT_PRESENT: i32 = 1;
    const NOT_CONFIGURED_FOR_USER: i32 = 2;
    const DISABLED_BY_POLICY: i32 = 3;
    const DEVICE_BUSY: i32 = 4;

    const VERIFIED: i32 = 0;
    const RETRIES_EXHAUSTED: i32 = 5;
    const CANCELED: i32 = 6;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Availability,
        Hello(isize, String),
        Dialog(isize, String),
        Logon,
    }

    type Calls = Arc<Mutex<Vec<Call>>>;

    /// Hello from a script: the availability's answer, then each verification's in turn.
    struct FakeHello {
        availability: Asked,
        verify: Mutex<VecDeque<Asked>>,
        calls: Calls,
    }

    impl Hello for FakeHello {
        fn availability(&self, _cancel: &CancellationToken) -> Asked {
            self.calls.lock().unwrap().push(Call::Availability);
            self.availability
        }

        fn verify(&self, window: isize, message: &str, _cancel: &CancellationToken) -> Asked {
            self.calls
                .lock()
                .unwrap()
                .push(Call::Hello(window, message.to_owned()));
            self.verify
                .lock()
                .unwrap()
                .pop_front()
                .expect("a scripted answer")
        }
    }

    /// A credential dialog that returns the owner's password, and a logon that answers `logon`.
    struct FakeDialog {
        logon: Result<(), u32>,
        calls: Calls,
    }

    impl CredentialUi for FakeDialog {
        fn owner(&self) -> Option<String> {
            Some(OWNER.to_owned())
        }

        fn prompt(&self, window: isize, message: &str) -> Prompted {
            self.calls
                .lock()
                .unwrap()
                .push(Call::Dialog(window, message.to_owned()));
            Prompted::Entered(Credentials {
                user: OWNER.to_owned(),
                password: Zeroizing::new(vec![0x61, 0]),
            })
        }

        fn logon(&self, _domain: &str, _user: &str, _password: &[u16]) -> Result<(), u32> {
            self.calls.lock().unwrap().push(Call::Logon);
            self.logon
        }
    }

    struct Fixture {
        auth: OsAuthenticator,
        calls: Calls,
        clock: Arc<AtomicU64>,
    }

    fn fixture(availability: Asked, verify: &[Asked], logon: Result<(), u32>) -> Fixture {
        let calls = Calls::default();
        let clock = Arc::new(AtomicU64::new(0));
        let auth = {
            let clock = Arc::clone(&clock);
            OsAuthenticator::with(
                Box::new(FakeHello {
                    availability,
                    verify: Mutex::new(verify.iter().copied().collect()),
                    calls: Arc::clone(&calls),
                }),
                Box::new(FakeDialog {
                    logon,
                    calls: Arc::clone(&calls),
                }),
                Box::new(move || clock.load(Ordering::SeqCst)),
            )
        };
        auth.set_window(WINDOW);
        Fixture { auth, calls, clock }
    }

    impl Fixture {
        fn verify(&self, action: Action) -> AuthOutcome {
            self.auth.verify(action, &CancellationToken::new())
        }

        fn calls(&self) -> Vec<Call> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }
    }

    fn dialog(action: Action) -> Call {
        Call::Dialog(WINDOW, message(action).to_owned())
    }

    fn hello(action: Action) -> Call {
        Call::Hello(WINDOW, message(action).to_owned())
    }

    fn info(method: AuthMethod, biometrics_choice: bool) -> AuthInfo {
        AuthInfo {
            available: true,
            method: Some(method),
            unavailable: None,
            biometrics_choice,
            password_field: false,
        }
    }

    #[test]
    fn where_hello_is_available_it_prompts_in_front_of_the_window() {
        let f = fixture(
            Asked::Answered(AVAILABLE),
            &[Asked::Answered(VERIFIED)],
            Ok(()),
        );
        assert_eq!(f.auth.info(), info(AuthMethod::WindowsHello, true));
        assert_eq!(f.verify(Action::Confirm), AuthOutcome::Verified);
        assert_eq!(
            f.calls(),
            [
                Call::Availability,
                Call::Availability,
                hello(Action::Confirm)
            ],
            "the availability comes first, every time"
        );
    }

    #[test]
    fn hello_s_answers_are_the_outcome() {
        for (answer, outcome) in [
            (
                CANCELED,
                AuthOutcome::Cancelled {
                    by: CancelledBy::User,
                },
            ),
            (RETRIES_EXHAUSTED, AuthOutcome::Failed { exhausted: true }),
            (DEVICE_BUSY, AuthOutcome::Busy),
            (99, AuthOutcome::Failed { exhausted: false }),
        ] {
            let f = fixture(
                Asked::Answered(AVAILABLE),
                &[Asked::Answered(answer)],
                Ok(()),
            );
            assert_eq!(f.verify(Action::Unlock), outcome, "{answer}");
            assert_eq!(
                f.calls(),
                [Call::Availability, hello(Action::Unlock)],
                "{answer}: no dialog"
            );
        }
    }

    #[test]
    fn where_hello_cannot_prompt_the_credential_dialog_asks() {
        for availability in [
            Asked::Answered(DEVICE_NOT_PRESENT),
            Asked::Answered(NOT_CONFIGURED_FOR_USER),
            Asked::Answered(DISABLED_BY_POLICY),
            Asked::Answered(-1),
            Asked::NotStarted,
            Asked::Failed,
        ] {
            let f = fixture(availability, &[], Ok(()));
            assert_eq!(
                f.auth.info(),
                info(AuthMethod::WindowsCredential, false),
                "{availability:?}"
            );
            assert_eq!(f.verify(Action::Unlock), AuthOutcome::Verified);
            assert_eq!(
                f.calls(),
                [
                    Call::Availability,
                    Call::Availability,
                    dialog(Action::Unlock),
                    Call::Logon
                ],
                "{availability:?}"
            );
        }
    }

    #[test]
    fn a_busy_hello_is_busy_without_a_prompt() {
        let f = fixture(Asked::Answered(DEVICE_BUSY), &[], Ok(()));
        assert_eq!(f.auth.info(), info(AuthMethod::WindowsHello, true));
        assert_eq!(f.verify(Action::Unlock), AuthOutcome::Busy);
        assert_eq!(f.calls(), [Call::Availability, Call::Availability]);
    }

    /// Hello said available, then could not prompt after all: the dialog asks instead.
    #[test]
    fn a_hello_that_cannot_prompt_after_all_falls_back_to_the_dialog() {
        for answer in [
            Asked::Answered(DEVICE_NOT_PRESENT),
            Asked::Answered(NOT_CONFIGURED_FOR_USER),
            Asked::Answered(DISABLED_BY_POLICY),
            Asked::NotStarted,
        ] {
            let f = fixture(Asked::Answered(AVAILABLE), &[answer], Err(1326));
            assert_eq!(
                f.verify(Action::Confirm),
                AuthOutcome::Failed { exhausted: false },
                "{answer:?}: the dialog's answer"
            );
            assert_eq!(
                f.calls(),
                [
                    Call::Availability,
                    hello(Action::Confirm),
                    dialog(Action::Confirm),
                    Call::Logon
                ],
                "{answer:?}"
            );
        }
    }

    /// A prompt that started and then failed may have been seen: no second prompt.
    #[test]
    fn a_hello_prompt_that_fails_is_failed() {
        let f = fixture(Asked::Answered(AVAILABLE), &[Asked::Failed], Ok(()));
        assert_eq!(
            f.verify(Action::Unlock),
            AuthOutcome::Failed { exhausted: false }
        );
        assert_eq!(f.calls(), [Call::Availability, hello(Action::Unlock)]);
    }

    #[test]
    fn a_cancelled_hello_is_the_app_s_cancel() {
        let f = fixture(Asked::Answered(AVAILABLE), &[Asked::Cancelled], Ok(()));
        assert_eq!(f.verify(Action::Unlock), APP_CANCELLED);
        let f = fixture(Asked::Cancelled, &[], Ok(()));
        assert_eq!(f.verify(Action::Unlock), APP_CANCELLED);
        assert_eq!(
            f.calls(),
            [Call::Availability],
            "no prompt after the cancel"
        );
    }

    #[test]
    fn with_the_hello_setting_off_the_dialog_always_asks() {
        let f = fixture(Asked::Answered(AVAILABLE), &[], Ok(()));
        f.auth.set_hello(false);
        assert_eq!(
            f.auth.info(),
            info(AuthMethod::WindowsCredential, true),
            "Hello can prompt, so the switch stays offered"
        );
        assert_eq!(f.verify(Action::Confirm), AuthOutcome::Verified);
        assert_eq!(
            f.calls(),
            [Call::Availability, dialog(Action::Confirm), Call::Logon],
            "Hello is not asked to verify"
        );
        f.auth.set_hello(true);
        assert_eq!(f.auth.info(), info(AuthMethod::WindowsHello, true));
    }

    #[test]
    fn without_a_window_nothing_opens() {
        let f = fixture(Asked::Answered(AVAILABLE), &[], Ok(()));
        f.auth.set_window(0);
        for prefer_hello in [true, false] {
            f.auth.set_hello(prefer_hello);
            assert_eq!(
                f.verify(Action::Unlock),
                AuthOutcome::Unavailable {
                    reason: UnavailableReason::NotInteractive
                }
            );
        }
        assert!(f.calls().is_empty());
    }

    #[test]
    fn a_token_tripped_before_asks_nothing() {
        let f = fixture(Asked::Answered(AVAILABLE), &[], Ok(()));
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(f.auth.verify(Action::Unlock, &token), APP_CANCELLED);
        assert!(f.calls().is_empty());
    }

    /// The dialog's back-off reads the authenticator's clock.
    #[test]
    fn the_dialog_s_back_off_counts_in_the_clock() {
        let f = fixture(Asked::Answered(DISABLED_BY_POLICY), &[], Err(1326));
        for at in [1_000, 2_000] {
            f.clock.store(at, Ordering::SeqCst);
            assert_eq!(
                f.verify(Action::Unlock),
                AuthOutcome::Failed { exhausted: false }
            );
        }
        f.clock.store(3_000, Ordering::SeqCst);
        assert_eq!(
            f.verify(Action::Unlock),
            AuthOutcome::Failed { exhausted: true }
        );
        f.clock.store(32_999, Ordering::SeqCst);
        f.calls();
        assert_eq!(
            f.verify(Action::Unlock),
            AuthOutcome::Failed { exhausted: true }
        );
        assert_eq!(f.calls(), [Call::Availability], "refused without a dialog");
        f.clock.store(33_000, Ordering::SeqCst);
        assert_eq!(
            f.verify(Action::Unlock),
            AuthOutcome::Failed { exhausted: false }
        );
    }

    #[test]
    fn the_messages_name_the_action() {
        assert_eq!(message(Action::Unlock), "Unlock AppRafter Desktop");
        assert_eq!(
            message(Action::Confirm),
            "Approve or run a destructive AppRafter operation"
        );
        assert_eq!(credui::CAPTION, "AppRafter Desktop");
    }

    /// The clock starts at the authenticator's creation and only moves forward.
    #[test]
    fn the_system_clock_is_monotonic() {
        let auth = OsAuthenticator::new();
        let first = (auth.clock)();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = (auth.clock)();
        assert!(first < 1_000, "{first}");
        assert!(second >= first + 20, "{first} {second}");
        assert_eq!(auth.window.load(Ordering::SeqCst), 0, "no window yet");
        assert!(auth.prefer_hello.load(Ordering::SeqCst), "Hello by default");
    }
}
