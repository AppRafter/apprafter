// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The Windows credential dialog, where Windows Hello cannot prompt or the owner turned it off:
//! `CredUIPromptForWindowsCredentialsW` asks for the account's password, `LogonUserW` checks it.
//!
//! One check, in this order:
//! 1. The back-off ([`Backoff`], one per [`CredentialCheck`], so per process): while it refuses,
//!    `Failed { exhausted: true }` without a dialog, with how long it still will
//!    (`retry_in_ms`). Checks run one at a time: one asked while another's dialog is open is
//!    `Busy`.
//! 2. The owner: the account running the app, `GetUserNameExW(NameSamCompatible)`
//!    (`DOMAIN\user`), never a name from the UI.
//! 3. The dialog, modal and parented to the app's window, listing the current user only
//!    (`CREDUIWIN_ENUMERATE_CURRENT_USER`). Closed by the user: `Cancelled { by: User }`. Not
//!    shown at all: `Unavailable { NoBackend }`. Its buffer is unpacked
//!    (`CredUnPackAuthenticationBufferW`), then wiped and freed ([`SecretBuffer`]).
//! 4. The name the dialog returns must be the owner's, compared case-insensitively
//!    ([`same_account`]); another account's credentials are `Failed`.
//! 5. `LogonUserW` for the owner's domain and name with the password, as an interactive logon,
//!    with a real token out-parameter (a null one makes it succeed for any password), which is
//!    closed at once. Its refusal maps through [`map_windows_logon`].
//!
//! Cancel: Windows offers no way to close this dialog from outside, so a tripped token cannot
//! close it. Whatever the dialog returns once the token has tripped is discarded unchecked and
//! the check is `Cancelled { by: App }`; the dialog stays until the user closes it.
//!
//! Back-off: every dialog that came back with credentials and did not end `Verified` is a
//! failure — another account's name, a buffer that could not be unpacked, a password
//! `LogonUserW` refuses, an account it will not log on. A dialog closed or not shown is not,
//! and neither is one whose answer the app discarded: no password was tried. The failure that
//! starts a refusal answers `Failed { exhausted: true }` with the refusal's time, as every check
//! the refusal turns away does with what is left of it. Failed `LogonUserW` calls also count
//! toward the account-lockout policy, a domain account's too, which the back-off keeps the app
//! from running into.
//!
//! Memory: the password stays UTF-16 in a [`Zeroizing`] buffer, wiped when the check ends; the
//! dialog's packed buffer is wiped before it is freed. `LogonUserW` gets the buffer itself.
//!
//! Threads: the dialog runs on a thread of its own in a single-threaded COM apartment, the
//! apartment of a thread that owns windows; the caller, a blocking worker, waits for it.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::{Mutex, TryLockError};

use ::windows::core::{HSTRING, PCWSTR, PWSTR};
use ::windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_CANCELLED, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA, HANDLE,
    HWND, NO_ERROR, WIN32_ERROR,
};
use ::windows::Win32::Security::Authentication::Identity::{GetUserNameExW, NameSamCompatible};
use ::windows::Win32::Security::Credentials::{
    CredUIPromptForWindowsCredentialsW, CredUnPackAuthenticationBufferW,
    CREDUIWIN_ENUMERATE_CURRENT_USER, CREDUI_INFOW, CREDUI_MAX_USERNAME_LENGTH,
    CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PACK_PROTECTED_CREDENTIALS,
};
use ::windows::Win32::Security::{LogonUserW, LOGON32_LOGON_INTERACTIVE, LOGON32_PROVIDER_DEFAULT};
use ::windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use apprafter_core::CancellationToken;
use apprafter_desktop_ipc::{AuthOutcome, CancelledBy, UnavailableReason};
use zeroize::{Zeroize, Zeroizing};

use crate::outcome::{map_windows_logon, Backoff, Refused};

/// The dialog's title.
pub const CAPTION: &str = "AppRafter Desktop";

const APP_CANCELLED: AuthOutcome = AuthOutcome::Cancelled {
    by: CancelledBy::App,
};
const FAILED: AuthOutcome = AuthOutcome::Failed {
    exhausted: false,
    retry_in_ms: None,
};

/// What the back-off answers while it refuses: no attempt is left, for `refused`'s time.
const fn exhausted(refused: Refused) -> AuthOutcome {
    AuthOutcome::Failed {
        exhausted: true,
        retry_in_ms: Some(refused.remaining_ms),
    }
}

const fn unavailable(reason: UnavailableReason) -> AuthOutcome {
    AuthOutcome::Unavailable { reason }
}

/// What the dialog returned: an account name as the dialog spelled it (`DOMAIN\user`), and the
/// password, NUL-terminated UTF-16, wiped when dropped.
pub struct Credentials {
    pub user: String,
    pub password: Zeroizing<Vec<u16>>,
}

/// How the dialog ended.
pub enum Prompted {
    /// The user entered credentials.
    Entered(Credentials),
    /// The user entered something whose buffer could not be unpacked into a name and a
    /// password (e.g. a credential provider that has no password).
    Unreadable,
    /// The user closed the dialog (`ERROR_CANCELLED`).
    Cancelled,
    /// The dialog could not be shown: the Win32 error.
    NotShown(u32),
}

/// The Windows calls a check makes: the system's in the app, a script in the tests.
pub trait CredentialUi: Send + Sync {
    /// The account running the app, `DOMAIN\user`; `None` when Windows does not say.
    fn owner(&self) -> Option<String>;
    /// Shows the dialog in front of `window` with `message`, and blocks until it closes.
    fn prompt(&self, window: isize, message: &str) -> Prompted;
    /// Checks `password` (NUL-terminated UTF-16) for `domain\user`: `Ok`, or the Win32 error.
    fn logon(&self, domain: &str, user: &str, password: &[u16]) -> Result<(), u32>;
}

/// Whether the name the dialog returned is the owner's: account names compare without regard to
/// case. Unicode lowercase is close to, not the same as, Windows' own case table; a name only
/// Windows would call equal is refused (the owner retries), and one only this would call equal
/// is harmless: the password is then checked against the owner's account, not the name typed.
pub fn same_account(entered: &str, owner: &str) -> bool {
    entered.to_lowercase() == owner.to_lowercase()
}

/// `DOMAIN\user` split at its backslash; `None` unless both parts are there.
pub fn split_account(sam_compatible: &str) -> Option<(&str, &str)> {
    sam_compatible
        .split_once('\\')
        .filter(|(domain, user)| !domain.is_empty() && !user.is_empty())
}

/// The credential-dialog check with this process's back-off.
pub struct CredentialCheck {
    ui: Box<dyn CredentialUi>,
    backoff: Mutex<Backoff>,
}

impl CredentialCheck {
    pub fn new(ui: Box<dyn CredentialUi>) -> Self {
        Self {
            ui,
            backoff: Mutex::default(),
        }
    }

    /// One check (the module docs give the steps). `now_monotonic_ms` is the monotonic clock
    /// the back-off counts in. Blocks until the dialog closes; a token already tripped asks
    /// nothing.
    pub fn verify(
        &self,
        window: isize,
        message: &str,
        cancel: &CancellationToken,
        now_monotonic_ms: u64,
    ) -> AuthOutcome {
        if cancel.is_cancelled() {
            return APP_CANCELLED;
        }
        let mut backoff = match self.backoff.try_lock() {
            Ok(backoff) => backoff,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return AuthOutcome::Busy,
        };
        if let Err(refused) = backoff.allow(now_monotonic_ms) {
            return exhausted(refused);
        }
        let Some(owner) = self.ui.owner() else {
            return unavailable(UnavailableReason::NotConfigured);
        };
        let Some((domain, user)) = split_account(&owner) else {
            return unavailable(UnavailableReason::NotConfigured);
        };
        let entered = match self.ui.prompt(window, message) {
            Prompted::Cancelled if cancel.is_cancelled() => return APP_CANCELLED,
            Prompted::Cancelled => {
                return AuthOutcome::Cancelled {
                    by: CancelledBy::User,
                }
            }
            Prompted::NotShown(_) => return unavailable(UnavailableReason::NoBackend),
            // The app closed the check: what came back is dropped (and wiped) unchecked.
            _ if cancel.is_cancelled() => return APP_CANCELLED,
            Prompted::Unreadable => None,
            Prompted::Entered(credentials) => Some(credentials),
        };
        let outcome = match entered {
            Some(credentials) if same_account(&credentials.user, &owner) => {
                match self.ui.logon(domain, user, &credentials.password) {
                    Ok(()) => AuthOutcome::Verified,
                    Err(code) => map_windows_logon(code),
                }
            }
            _ => FAILED,
        };
        let outcome = if outcome == AuthOutcome::Verified {
            backoff.record_success();
            outcome
        } else {
            backoff.record_failure(now_monotonic_ms);
            // A wrong password that starts the refusal says how long it lasts; Windows' own
            // account lockout keeps its unknown end.
            match (outcome, backoff.allow(now_monotonic_ms)) {
                (FAILED, Err(refused)) => exhausted(refused),
                (other, _) => other,
            }
        };
        // Tripped while `LogonUserW` ran: the attempt counted, the answer is the app's cancel.
        if cancel.is_cancelled() {
            APP_CANCELLED
        } else {
            outcome
        }
    }
}

/// A buffer the OS allocated that holds a secret, wiped and then handed back to `free` when
/// dropped (what CredUI asks of its caller: `SecureZeroMemory`, then `CoTaskMemFree`).
/// `zeroize` writes the zeros as `SecureZeroMemory` does, with volatile stores the compiler may
/// not remove.
pub struct SecretBuffer {
    pointer: *mut c_void,
    size: usize,
    free: unsafe fn(*mut c_void),
}

impl SecretBuffer {
    /// # Safety
    ///
    /// `pointer` is null, or points to `size` bytes that are readable and writable, that
    /// nothing else reads or writes until this is dropped, and that `free` releases.
    pub unsafe fn new(pointer: *mut c_void, size: usize, free: unsafe fn(*mut c_void)) -> Self {
        Self {
            pointer,
            size,
            free,
        }
    }

    pub fn pointer(&self) -> *const c_void {
        self.pointer
    }

    /// In bytes.
    pub fn size(&self) -> usize {
        self.size
    }
}

impl Drop for SecretBuffer {
    fn drop(&mut self) {
        if self.pointer.is_null() {
            return;
        }
        // SAFETY: `new`'s contract: `size` writable bytes at a non-null `pointer` (checked
        // above) that only this owns; bytes need no alignment.
        unsafe { std::slice::from_raw_parts_mut(self.pointer.cast::<u8>(), self.size) }.zeroize();
        // SAFETY: `new`'s contract: `free` releases this allocation, exactly once, here.
        unsafe { (self.free)(self.pointer) }
    }
}

/// `CoTaskMemFree` as a [`SecretBuffer`]'s `free`.
///
/// # Safety
///
/// `pointer` was allocated with `CoTaskMemAlloc` (as CredUI's buffer is) and is not used again.
unsafe fn co_task_mem_free(pointer: *mut c_void) {
    // SAFETY: the function's contract.
    unsafe { CoTaskMemFree(Some(pointer.cast_const())) }
}

/// This thread in a single-threaded COM apartment for as long as the value lives; it is not
/// `Send`, so it ends on the thread that entered.
struct Apartment {
    entered: bool,
    _thread: PhantomData<*const ()>,
}

impl Apartment {
    fn single_threaded() -> Self {
        // SAFETY: no pointer argument (the reserved one is null). Success, including S_FALSE
        // (already in this apartment), is balanced by `CoUninitialize` on drop, on this thread;
        // a failure (RPC_E_CHANGED_MODE) is not, as COM requires.
        let result =
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        Self {
            entered: result.is_ok(),
            _thread: PhantomData,
        }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.entered {
            // SAFETY: balances this thread's successful `CoInitializeEx` (see `single_threaded`).
            unsafe { CoUninitialize() }
        }
    }
}

/// NUL-terminated UTF-16.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The text before the first NUL, if it is UTF-16.
fn from_wide(buffer: &[u16]) -> Option<String> {
    let end = buffer.iter().position(|&unit| unit == 0)?;
    String::from_utf16(&buffer[..end]).ok()
}

/// The system's credential dialog and logon.
pub struct SystemCredentialUi;

impl CredentialUi for SystemCredentialUi {
    fn owner(&self) -> Option<String> {
        let mut size = 0u32;
        // SAFETY: no buffer and a size of 0 ask for the size; `size` is a valid out-parameter.
        let sized = unsafe { GetUserNameExW(NameSamCompatible, None, &mut size) };
        // SAFETY: reads this thread's last error, set by the call above.
        if sized || unsafe { GetLastError() } != ERROR_MORE_DATA || size == 0 {
            return None;
        }
        let mut name = vec![0u16; size as usize];
        // SAFETY: `name` holds `size` UTF-16 units, the size the first call asked for
        // (terminator included), and outlives the call; `size` is a valid in/out-parameter.
        let named =
            unsafe { GetUserNameExW(NameSamCompatible, Some(PWSTR(name.as_mut_ptr())), &mut size) };
        if named {
            from_wide(&name)
        } else {
            None
        }
    }

    fn prompt(&self, window: isize, message: &str) -> Prompted {
        let message = message.to_owned();
        let dialog = std::thread::Builder::new()
            .name("apprafter-credui".to_owned())
            .spawn(move || {
                let _apartment = Apartment::single_threaded();
                prompt_here(window, &message)
            });
        match dialog.map(|dialog| dialog.join()) {
            Ok(Ok(prompted)) => prompted,
            // No thread, or it panicked: no dialog, or none that answered.
            _ => Prompted::NotShown(ERROR_INVALID_PARAMETER.0),
        }
    }

    fn logon(&self, domain: &str, user: &str, password: &[u16]) -> Result<(), u32> {
        if !password.contains(&0) {
            return Err(ERROR_INVALID_PARAMETER.0);
        }
        let mut token = HANDLE::default();
        // SAFETY: the three strings are NUL-terminated (the HSTRINGs by construction, the
        // password checked above) and outlive the call; `token` is a valid out-parameter, and a
        // real one (with a null one LogonUserW succeeds whatever the password).
        let result = unsafe {
            LogonUserW(
                &HSTRING::from(user),
                &HSTRING::from(domain),
                PCWSTR(password.as_ptr()),
                LOGON32_LOGON_INTERACTIVE,
                LOGON32_PROVIDER_DEFAULT,
                &mut token,
            )
        };
        if !token.is_invalid() {
            // SAFETY: a handle LogonUserW opened for this call, closed once and not used after.
            let _ = unsafe { CloseHandle(token) };
        }
        result.map_err(|error| {
            WIN32_ERROR::from_error(&error).map_or(error.code().0 as u32, |code| code.0)
        })
    }
}

/// The dialog on this thread: shown, unpacked, and its buffer wiped and freed.
fn prompt_here(window: isize, message: &str) -> Prompted {
    let message = wide(message);
    let caption = wide(CAPTION);
    let info = CREDUI_INFOW {
        cbSize: std::mem::size_of::<CREDUI_INFOW>() as u32,
        hwndParent: HWND(window as *mut c_void),
        pszMessageText: PCWSTR(message.as_ptr()),
        pszCaptionText: PCWSTR(caption.as_ptr()),
        ..CREDUI_INFOW::default()
    };
    let mut package = 0u32;
    let mut buffer: *mut c_void = std::ptr::null_mut();
    let mut size = 0u32;
    // SAFETY: `info` and the strings it points to outlive the call, which blocks until the
    // dialog closes; every out-parameter is a valid local. No input buffer is passed.
    let code = unsafe {
        CredUIPromptForWindowsCredentialsW(
            Some(&info),
            0,
            &mut package,
            None,
            0,
            &mut buffer,
            &mut size,
            None,
            CREDUIWIN_ENUMERATE_CURRENT_USER,
        )
    };
    match WIN32_ERROR(code) {
        NO_ERROR => {
            // SAFETY: on success CredUI hands the caller `size` bytes at `buffer`, allocated
            // with CoTaskMemAlloc, which nothing else touches from here on.
            let buffer = unsafe { SecretBuffer::new(buffer, size as usize, co_task_mem_free) };
            unpack(&buffer).map_or(Prompted::Unreadable, Prompted::Entered)
        }
        ERROR_CANCELLED => Prompted::Cancelled,
        other => Prompted::NotShown(other.0),
    }
}

/// The name (`DOMAIN\user`) and the password in the dialog's buffer.
fn unpack(buffer: &SecretBuffer) -> Option<Credentials> {
    let size = u32::try_from(buffer.size()).ok()?;
    // The name includes the domain; the password can be as long as a credential blob holds
    // (wincred.h's CREDUI_MAX_PASSWORD_LENGTH). One more unit each for the terminator.
    let mut user = vec![0u16; CREDUI_MAX_USERNAME_LENGTH as usize + 1];
    let mut user_len = user.len() as u32;
    let mut password = Zeroizing::new(vec![0u16; CRED_MAX_CREDENTIAL_BLOB_SIZE as usize / 2 + 1]);
    let mut password_len = password.len() as u32;
    // SAFETY: the input is CredUI's `size` bytes, alive for the call; each output buffer holds
    // the number of units its length says and outlives the call. No domain buffer: the name
    // then carries the domain.
    let unpacked = unsafe {
        CredUnPackAuthenticationBufferW(
            CRED_PACK_PROTECTED_CREDENTIALS,
            buffer.pointer(),
            size,
            Some(PWSTR(user.as_mut_ptr())),
            &mut user_len,
            None,
            None,
            Some(PWSTR(password.as_mut_ptr())),
            &mut password_len,
        )
    };
    unpacked.ok()?;
    if !password.contains(&0) {
        return None;
    }
    Some(Credentials {
        user: from_wide(&user)?,
        password,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;

    const OWNER: &str = r"DESKTOP-1\Ada";
    const SECRET: &str = "correct horse battery staple";
    const WINDOW: isize = 77;
    const MESSAGE: &str = "unlock";
    const LOGON_FAILURE: u32 = 1326;
    const ACCOUNT_DISABLED: u32 = 1331;
    const ACCOUNT_LOCKED_OUT: u32 = 1909;

    /// What the back-off answers while it refuses, for `ms` more.
    const fn refused_for(ms: u64) -> AuthOutcome {
        AuthOutcome::Failed {
            exhausted: true,
            retry_in_ms: Some(ms),
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Owner,
        Prompt(isize, String),
        /// The domain, the name, and the password as text.
        Logon(String, String, String),
    }

    /// What a scripted dialog does.
    #[derive(Debug, Clone, Copy)]
    enum Dialog {
        Enter(&'static str, &'static str),
        /// Returns the credentials after the app's token tripped while it was open.
        EnterAfterCancel(&'static str, &'static str),
        Unreadable,
        Cancel,
        /// Closed by the user after the app's token tripped while it was open.
        CancelAfterCancel,
        NotShown,
    }

    /// One dialog and what `LogonUserW` then answers.
    type Step = (Dialog, Result<(), u32>);

    /// Windows from a script: one step per dialog.
    struct FakeUi {
        owner: Option<&'static str>,
        steps: Mutex<VecDeque<Step>>,
        /// The current step's logon answer.
        logon: Mutex<Option<Result<(), u32>>>,
        /// The check's token, which `EnterAfterCancel` trips, and so does the logon when
        /// `trip_in_logon`.
        token: Mutex<Option<CancellationToken>>,
        trip_in_logon: bool,
        calls: Mutex<Vec<Call>>,
    }

    impl FakeUi {
        fn new(steps: &[Step]) -> Arc<Self> {
            Arc::new(Self {
                owner: Some(OWNER),
                steps: Mutex::new(steps.iter().copied().collect()),
                logon: Mutex::default(),
                token: Mutex::default(),
                trip_in_logon: false,
                calls: Mutex::default(),
            })
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        fn trip(&self) {
            if let Some(token) = &*self.token.lock().unwrap() {
                token.cancel();
            }
        }
    }

    fn credentials(user: &str, password: &str) -> Credentials {
        let mut wide = password.encode_utf16().collect::<Vec<_>>();
        wide.push(0);
        Credentials {
            user: user.to_owned(),
            password: Zeroizing::new(wide),
        }
    }

    impl CredentialUi for Arc<FakeUi> {
        fn owner(&self) -> Option<String> {
            self.calls.lock().unwrap().push(Call::Owner);
            self.owner.map(str::to_owned)
        }

        fn prompt(&self, window: isize, message: &str) -> Prompted {
            self.calls
                .lock()
                .unwrap()
                .push(Call::Prompt(window, message.to_owned()));
            let (dialog, logon) = self.steps.lock().unwrap().pop_front().expect("a step");
            *self.logon.lock().unwrap() = Some(logon);
            match dialog {
                Dialog::Enter(user, password) => Prompted::Entered(credentials(user, password)),
                Dialog::EnterAfterCancel(user, password) => {
                    self.trip();
                    Prompted::Entered(credentials(user, password))
                }
                Dialog::Unreadable => Prompted::Unreadable,
                Dialog::Cancel => Prompted::Cancelled,
                Dialog::CancelAfterCancel => {
                    self.trip();
                    Prompted::Cancelled
                }
                Dialog::NotShown => Prompted::NotShown(1400),
            }
        }

        fn logon(&self, domain: &str, user: &str, password: &[u16]) -> Result<(), u32> {
            let end = password.iter().position(|&unit| unit == 0).unwrap();
            self.calls.lock().unwrap().push(Call::Logon(
                domain.to_owned(),
                user.to_owned(),
                String::from_utf16(&password[..end]).unwrap(),
            ));
            if self.trip_in_logon {
                self.trip();
            }
            self.logon.lock().unwrap().take().expect("a dialog first")
        }
    }

    fn checker(ui: &Arc<FakeUi>) -> CredentialCheck {
        CredentialCheck::new(Box::new(Arc::clone(ui)))
    }

    /// One check at `now` with a fresh token the fake can trip.
    fn verify(check: &CredentialCheck, ui: &FakeUi, now: u64) -> AuthOutcome {
        let token = CancellationToken::new();
        *ui.token.lock().unwrap() = Some(token.clone());
        check.verify(WINDOW, MESSAGE, &token, now)
    }

    fn prompt_call() -> Call {
        Call::Prompt(WINDOW, MESSAGE.to_owned())
    }

    fn logon_call(password: &str) -> Call {
        Call::Logon(
            "DESKTOP-1".to_owned(),
            "Ada".to_owned(),
            password.to_owned(),
        )
    }

    fn failed_at(at: &[u64]) -> Backoff {
        let mut backoff = Backoff::new();
        for &now in at {
            backoff.record_failure(now);
        }
        backoff
    }

    #[test]
    fn the_owner_s_password_is_checked_against_the_owner_s_account() {
        for typed in [OWNER, r"desktop-1\ada", r"DESKTOP-1\ADA"] {
            let ui = FakeUi::new(&[(Dialog::Enter(typed, SECRET), Ok(()))]);
            let check = checker(&ui);
            assert_eq!(verify(&check, &ui, 0), AuthOutcome::Verified, "{typed}");
            assert_eq!(
                ui.calls(),
                [Call::Owner, prompt_call(), logon_call(SECRET)],
                "the owner's own spelling reaches LogonUserW, not {typed:?}"
            );
        }
    }

    #[test]
    fn another_account_s_credentials_fail_without_a_logon() {
        for typed in [r"DESKTOP-1\Bob", "Ada", r"OTHER\Ada", "ada@example.com", ""] {
            let ui = FakeUi::new(&[(Dialog::Enter(typed, SECRET), Ok(()))]);
            let check = checker(&ui);
            assert_eq!(verify(&check, &ui, 0), FAILED, "{typed}");
            assert_eq!(ui.calls(), [Call::Owner, prompt_call()], "{typed}");
        }
    }

    #[test]
    fn logon_refusals_map_through_the_logon_table() {
        for (code, outcome) in [
            (LOGON_FAILURE, FAILED),
            // Windows' own lockout: its end is the policy's, which the app is not told.
            (
                ACCOUNT_LOCKED_OUT,
                AuthOutcome::Failed {
                    exhausted: true,
                    retry_in_ms: None,
                },
            ),
            (
                ACCOUNT_DISABLED,
                unavailable(UnavailableReason::NotPermittedHere),
            ),
        ] {
            let ui = FakeUi::new(&[(Dialog::Enter(OWNER, SECRET), Err(code))]);
            assert_eq!(verify(&checker(&ui), &ui, 0), outcome, "{code}");
        }
    }

    #[test]
    fn a_dialog_the_user_closes_is_the_user_s_cancel_and_costs_nothing() {
        let ui = FakeUi::new(&[(Dialog::Cancel, Ok(())); 5]);
        let check = checker(&ui);
        for at in 0..5 {
            assert_eq!(
                verify(&check, &ui, at),
                AuthOutcome::Cancelled {
                    by: CancelledBy::User
                }
            );
        }
        assert_eq!(*check.backoff.lock().unwrap(), Backoff::new());
    }

    /// The app cannot close the dialog; the user closing it afterwards is still the app's
    /// cancel.
    #[test]
    fn a_dialog_closed_after_the_app_cancelled_is_the_app_s_cancel() {
        let ui = FakeUi::new(&[(Dialog::CancelAfterCancel, Ok(()))]);
        let check = checker(&ui);
        assert_eq!(verify(&check, &ui, 0), APP_CANCELLED);
        assert_eq!(*check.backoff.lock().unwrap(), Backoff::new());
    }

    #[test]
    fn a_dialog_that_cannot_open_is_no_backend_and_costs_nothing() {
        let ui = FakeUi::new(&[(Dialog::NotShown, Ok(()))]);
        let check = checker(&ui);
        assert_eq!(
            verify(&check, &ui, 0),
            unavailable(UnavailableReason::NoBackend)
        );
        assert_eq!(*check.backoff.lock().unwrap(), Backoff::new());
    }

    #[test]
    fn an_unreadable_buffer_fails_without_a_logon() {
        let ui = FakeUi::new(&[(Dialog::Unreadable, Ok(()))]);
        assert_eq!(verify(&checker(&ui), &ui, 0), FAILED);
        assert_eq!(ui.calls(), [Call::Owner, prompt_call()]);
    }

    #[test]
    fn without_an_owner_there_is_nothing_to_check() {
        for owner in [None, Some("Ada"), Some(r"\Ada"), Some(r"DESKTOP-1\")] {
            let ui = Arc::new(FakeUi {
                owner,
                steps: Mutex::new(VecDeque::from([(Dialog::Enter(OWNER, SECRET), Ok(()))])),
                logon: Mutex::default(),
                token: Mutex::default(),
                trip_in_logon: false,
                calls: Mutex::default(),
            });
            assert_eq!(
                verify(&checker(&ui), &ui, 0),
                unavailable(UnavailableReason::NotConfigured),
                "{owner:?}"
            );
            assert_eq!(ui.calls(), [Call::Owner], "no dialog for {owner:?}");
        }
    }

    #[test]
    fn a_token_tripped_before_asks_nothing() {
        let ui = FakeUi::new(&[(Dialog::Enter(OWNER, SECRET), Ok(()))]);
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(
            checker(&ui).verify(WINDOW, MESSAGE, &token, 0),
            APP_CANCELLED
        );
        assert!(ui.calls().is_empty());
    }

    /// The dialog cannot be closed from outside: what it returns once the token has tripped
    /// is discarded unchecked, and costs no attempt.
    #[test]
    fn credentials_from_a_cancelled_check_are_never_tried() {
        for dialog in [
            Dialog::EnterAfterCancel(OWNER, SECRET),
            Dialog::EnterAfterCancel(r"DESKTOP-1\Bob", SECRET),
        ] {
            let ui = FakeUi::new(&[(dialog, Ok(())); 4]);
            let check = checker(&ui);
            for at in 0..4 {
                assert_eq!(verify(&check, &ui, at), APP_CANCELLED, "{dialog:?}");
            }
            assert!(
                !ui.calls()
                    .iter()
                    .any(|call| matches!(call, Call::Logon(..))),
                "{dialog:?}"
            );
            assert_eq!(*check.backoff.lock().unwrap(), Backoff::new(), "{dialog:?}");
        }
    }

    #[test]
    fn a_cancel_during_the_logon_is_the_app_s_and_the_attempt_counts() {
        let ui = Arc::new(FakeUi {
            owner: Some(OWNER),
            steps: Mutex::new(VecDeque::from([
                (Dialog::Enter(OWNER, "wrong"), Err(LOGON_FAILURE)),
                (Dialog::Enter(OWNER, SECRET), Ok(())),
            ])),
            logon: Mutex::default(),
            token: Mutex::default(),
            trip_in_logon: true,
            calls: Mutex::default(),
        });
        let check = checker(&ui);
        assert_eq!(verify(&check, &ui, 5), APP_CANCELLED);
        assert_eq!(*check.backoff.lock().unwrap(), failed_at(&[5]));
        assert_eq!(verify(&check, &ui, 6), APP_CANCELLED);
        assert_eq!(
            *check.backoff.lock().unwrap(),
            Backoff::new(),
            "the right password counted as a success"
        );
    }

    #[test]
    fn three_failures_refuse_for_thirty_seconds_without_a_dialog() {
        let ui = FakeUi::new(&[(Dialog::Enter(OWNER, "wrong"), Err(LOGON_FAILURE)); 4]);
        let check = checker(&ui);
        assert_eq!(verify(&check, &ui, 1_000), FAILED);
        assert_eq!(verify(&check, &ui, 2_000), FAILED);
        assert_eq!(
            verify(&check, &ui, 3_000),
            refused_for(30_000),
            "the third starts the refusal"
        );
        let asked = ui.calls().len();
        assert_eq!(verify(&check, &ui, 20_000), refused_for(13_000));
        assert_eq!(verify(&check, &ui, 32_999), refused_for(1));
        assert_eq!(ui.calls().len(), asked, "refused without a dialog");
        assert_eq!(verify(&check, &ui, 33_000), FAILED, "the refusal is over");
    }

    #[test]
    fn every_kind_of_failure_counts_and_a_success_starts_again() {
        let ui = FakeUi::new(&[
            (Dialog::Enter(r"DESKTOP-1\Bob", SECRET), Ok(())),
            (Dialog::Unreadable, Ok(())),
            (Dialog::Enter(OWNER, SECRET), Err(ACCOUNT_DISABLED)),
        ]);
        let check = checker(&ui);
        assert_eq!(verify(&check, &ui, 0), FAILED);
        assert_eq!(verify(&check, &ui, 0), FAILED);
        assert_eq!(
            verify(&check, &ui, 0),
            unavailable(UnavailableReason::NotPermittedHere),
            "the account's own refusal stays what it is"
        );
        assert_eq!(
            verify(&check, &ui, 0),
            refused_for(30_000),
            "the third failure refused"
        );

        let ui = FakeUi::new(&[
            (Dialog::Enter(OWNER, "wrong"), Err(LOGON_FAILURE)),
            (Dialog::Enter(OWNER, "wrong"), Err(LOGON_FAILURE)),
            (Dialog::Enter(OWNER, SECRET), Ok(())),
            (Dialog::Enter(OWNER, "wrong"), Err(LOGON_FAILURE)),
            (Dialog::Enter(OWNER, "wrong"), Err(LOGON_FAILURE)),
        ]);
        let check = checker(&ui);
        assert_eq!(verify(&check, &ui, 0), FAILED);
        assert_eq!(verify(&check, &ui, 0), FAILED);
        assert_eq!(verify(&check, &ui, 0), AuthOutcome::Verified);
        assert_eq!(verify(&check, &ui, 0), FAILED);
        assert_eq!(verify(&check, &ui, 0), FAILED, "two since the success");
    }

    #[test]
    fn a_second_check_while_a_dialog_is_open_is_busy() {
        let ui = FakeUi::new(&[(Dialog::Enter(OWNER, SECRET), Ok(()))]);
        let check = checker(&ui);
        let open = check.backoff.lock().unwrap();
        assert_eq!(verify(&check, &ui, 0), AuthOutcome::Busy);
        assert!(ui.calls().is_empty());
        drop(open);
        assert_eq!(verify(&check, &ui, 0), AuthOutcome::Verified);
    }

    #[test]
    fn account_names_compare_without_case() {
        for (entered, owner, same) in [
            (r"DESKTOP-1\Ada", r"DESKTOP-1\Ada", true),
            (r"desktop-1\ada", r"DESKTOP-1\Ada", true),
            (r"BÜRO\JÜRGEN", r"büro\jürgen", true),
            (r"DESKTOP-1\Ada", r"DESKTOP-1\Adam", false),
            (r"DESKTOP-1\Ada", "Ada", false),
            (r"DESKTOP-1\Ada ", r"DESKTOP-1\Ada", false),
            ("", r"DESKTOP-1\Ada", false),
        ] {
            assert_eq!(same_account(entered, owner), same, "{entered:?} {owner:?}");
        }
    }

    #[test]
    fn a_sam_compatible_name_splits_at_its_backslash() {
        assert_eq!(split_account(r"DESKTOP-1\Ada"), Some(("DESKTOP-1", "Ada")));
        assert_eq!(split_account(r"CORP\ada.l"), Some(("CORP", "ada.l")));
        for name in ["Ada", r"\Ada", r"CORP\", "", r"\"] {
            assert_eq!(split_account(name), None, "{name:?}");
        }
    }

    #[test]
    fn wide_strings_round_trip_up_to_the_terminator() {
        assert_eq!(wide("Ada"), [65, 100, 97, 0]);
        assert_eq!(
            from_wide(&wide(r"BÜRO\Jürgen")).as_deref(),
            Some(r"BÜRO\Jürgen")
        );
        assert_eq!(from_wide(&[65, 0, 66, 0]).as_deref(), Some("A"));
        assert_eq!(from_wide(&[65, 66]), None, "no terminator");
        assert_eq!(from_wide(&[0xD800, 0]), None, "a lone surrogate");
    }

    /// What the fake `free` found in the buffers it was handed.
    static FREED: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
    const FAKE_LEN: usize = 32;

    /// A `free` for a leaked `Box<[u8; FAKE_LEN]>` that records the bytes it receives.
    unsafe fn record_and_free(pointer: *mut c_void) {
        // SAFETY: the test leaked exactly such a box at `pointer`.
        let boxed = unsafe { Box::from_raw(pointer.cast::<[u8; FAKE_LEN]>()) };
        FREED.lock().unwrap().push(boxed.to_vec());
    }

    #[test]
    fn a_secret_buffer_is_wiped_before_it_is_freed() {
        let pointer = Box::into_raw(Box::new([0xA5u8; FAKE_LEN])).cast::<c_void>();
        // SAFETY: a leaked box of FAKE_LEN bytes, which `record_and_free` releases.
        let buffer = unsafe { SecretBuffer::new(pointer, FAKE_LEN, record_and_free) };
        assert_eq!(buffer.size(), FAKE_LEN);
        assert_eq!(buffer.pointer(), pointer.cast_const());
        drop(buffer);
        assert_eq!(*FREED.lock().unwrap(), [vec![0u8; FAKE_LEN]]);
    }

    #[test]
    fn a_null_secret_buffer_frees_nothing() {
        unsafe fn never(_: *mut c_void) {
            panic!("nothing to free");
        }
        // SAFETY: null, which owns nothing.
        drop(unsafe { SecretBuffer::new(std::ptr::null_mut(), FAKE_LEN, never) });
    }

    /// CredUI's allocator and the real `free`: no prompt.
    #[test]
    fn a_com_allocated_secret_buffer_is_released() {
        use ::windows::Win32::System::Com::CoTaskMemAlloc;
        // SAFETY: a plain allocation, checked for null below.
        let pointer = unsafe { CoTaskMemAlloc(FAKE_LEN) };
        assert!(!pointer.is_null());
        // SAFETY: FAKE_LEN bytes CoTaskMemAlloc just returned.
        unsafe { std::ptr::write_bytes(pointer.cast::<u8>(), 0x5A, FAKE_LEN) };
        // SAFETY: the same bytes, owned by nothing else, which CoTaskMemFree releases.
        drop(unsafe { SecretBuffer::new(pointer, FAKE_LEN, co_task_mem_free) });
    }

    /// The account running the tests, as Windows names it: no prompt.
    #[test]
    fn the_owner_is_the_sam_compatible_name_of_the_running_account() {
        let owner = SystemCredentialUi.owner().expect("a signed-in account");
        assert!(split_account(&owner).is_some(), "{owner:?}");
    }

    #[test]
    fn a_password_without_a_terminator_never_reaches_logon_user() {
        assert_eq!(
            SystemCredentialUi.logon("DOMAIN", "user", &[65, 66]),
            Err(ERROR_INVALID_PARAMETER.0)
        );
    }
}
