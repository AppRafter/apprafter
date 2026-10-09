// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! How each OS's answer becomes an [`AuthOutcome`]: pure functions over plain values that
//! mirror the OS results, with no `cfg`, so the CI of every OS tests every OS's mapping.
//!
//! A backend converts what its OS returned into the input here (an integer code, or polkit's
//! answer) and returns what the function says, with one exception that belongs to the backend:
//! a prompt the app closed itself (lock-on-sleep, quit) is `Cancelled { by: App }` from the
//! backend's own record, whatever the OS then reports. polkit reports a
//! `CancelCheckAuthorization` as the agent's dialog being dismissed, as the user closing it
//! would be, and a PAM stack can turn the conversation's refusal into `PAM_AUTH_ERR`.
//!
//! Every value an OS header names is a variant, matched without a wildcard, so a value added
//! here later needs a decision. A value no header names falls back to what each function
//! documents, and nothing but the OS's own success is ever `Verified`.

use std::collections::HashMap;

use apprafter_desktop_ipc::{AuthOutcome, CancelledBy, UnavailableReason};
use UnavailableReason::{
    DisabledByPolicy, NoAgent, NoBackend, NotConfigured, NotInteractive, NotPermittedHere,
    PasswordExpired, PolicyMissing,
};

const fn cancelled(by: CancelledBy) -> AuthOutcome {
    AuthOutcome::Cancelled { by }
}

const fn unavailable(reason: UnavailableReason) -> AuthOutcome {
    AuthOutcome::Unavailable { reason }
}

/// A refusal of the OS's own, `exhausted` when it allows no attempt for now: no OS says for how
/// long, so it carries no `retry_in_ms` (only the app's [`Backoff`] knows its end).
const fn failed(exhausted: bool) -> AuthOutcome {
    AuthOutcome::Failed {
        exhausted,
        retry_in_ms: None,
    }
}

/// `Windows.Security.Credentials.UI.UserConsentVerificationResult`, the answer of
/// `UserConsentVerifier::RequestVerificationForWindowAsync`. Values as windows 0.62.2 generates
/// them from the Windows metadata (`src/Windows/Security/Credentials/UI/mod.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloResult {
    Verified,
    DeviceNotPresent,
    NotConfiguredForUser,
    DisabledByPolicy,
    DeviceBusy,
    RetriesExhausted,
    Canceled,
    /// A value windows 0.62.2 does not name.
    Unknown(i32),
}

impl HelloResult {
    /// From the raw value (`UserConsentVerificationResult.0`).
    pub const fn from_raw(raw: i32) -> Self {
        match raw {
            0 => Self::Verified,
            1 => Self::DeviceNotPresent,
            2 => Self::NotConfiguredForUser,
            3 => Self::DisabledByPolicy,
            4 => Self::DeviceBusy,
            5 => Self::RetriesExhausted,
            6 => Self::Canceled,
            other => Self::Unknown(other),
        }
    }
}

/// Windows Hello's verification result as an outcome. A value windows 0.62.2 does not name is
/// `Failed { exhausted: false }`.
pub fn map_windows_hello(raw: i32) -> AuthOutcome {
    match HelloResult::from_raw(raw) {
        HelloResult::Verified => AuthOutcome::Verified,
        HelloResult::DeviceNotPresent | HelloResult::NotConfiguredForUser => {
            unavailable(NotConfigured)
        }
        HelloResult::DisabledByPolicy => unavailable(DisabledByPolicy),
        HelloResult::DeviceBusy => AuthOutcome::Busy,
        HelloResult::RetriesExhausted => failed(true),
        HelloResult::Canceled => cancelled(CancelledBy::User),
        HelloResult::Unknown(_) => failed(false),
    }
}

/// `Windows.Security.Credentials.UI.UserConsentVerifierAvailability`, the answer of
/// `UserConsentVerifier::CheckAvailabilityAsync`, which must be asked before every
/// verification. Values from the same windows 0.62.2 source as [`HelloResult`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloAvailability {
    Available,
    DeviceNotPresent,
    NotConfiguredForUser,
    DisabledByPolicy,
    DeviceBusy,
    /// A value windows 0.62.2 does not name.
    Unknown(i32),
}

impl HelloAvailability {
    /// From the raw value (`UserConsentVerifierAvailability.0`).
    pub const fn from_raw(raw: i32) -> Self {
        match raw {
            0 => Self::Available,
            1 => Self::DeviceNotPresent,
            2 => Self::NotConfiguredForUser,
            3 => Self::DisabledByPolicy,
            4 => Self::DeviceBusy,
            other => Self::Unknown(other),
        }
    }
}

/// Which Windows prompt verifies the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsRoute {
    /// Windows Hello can prompt.
    Hello,
    /// Hello cannot here: the Windows credential dialog asks for the account's password.
    Credential,
    /// Hello is answering another request: [`AuthOutcome::Busy`], without a prompt.
    Busy,
}

/// Where Hello's availability sends a request. Anything but `Available` and `DeviceBusy`,
/// including a value windows 0.62.2 does not name, is the credential dialog: it verifies the
/// account's password itself, so falling back to it never lets a request through unchecked.
pub fn map_windows_availability(raw: i32) -> WindowsRoute {
    match HelloAvailability::from_raw(raw) {
        HelloAvailability::Available => WindowsRoute::Hello,
        HelloAvailability::DeviceBusy => WindowsRoute::Busy,
        HelloAvailability::DeviceNotPresent
        | HelloAvailability::NotConfiguredForUser
        | HelloAvailability::DisabledByPolicy
        | HelloAvailability::Unknown(_) => WindowsRoute::Credential,
    }
}

/// The Win32 errors `LogonUserW` sets (`GetLastError`) when it refuses an account or its
/// password: the `ERROR_*` values of `winerror.h`, as windows 0.62.2 names them
/// (`src/Windows/Win32/Foundation/mod.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogonError {
    /// `ERROR_LOGON_FAILURE`: the user name or the password is wrong.
    LogonFailure,
    /// `ERROR_ACCOUNT_RESTRICTION`, e.g. a local account without a password, which Windows
    /// lets sign in at the console only.
    AccountRestriction,
    InvalidLogonHours,
    InvalidWorkstation,
    PasswordExpired,
    AccountDisabled,
    /// `ERROR_LOGON_TYPE_NOT_GRANTED`: the account may not sign in interactively.
    LogonTypeNotGranted,
    AccountExpired,
    PasswordMustChange,
    /// `ERROR_ACCOUNT_LOCKED_OUT`: the lockout policy counted too many wrong passwords.
    AccountLockedOut,
    /// Any other code.
    Other(u32),
}

impl LogonError {
    /// From the Win32 error code.
    pub const fn from_code(code: u32) -> Self {
        match code {
            1326 => Self::LogonFailure,
            1327 => Self::AccountRestriction,
            1328 => Self::InvalidLogonHours,
            1329 => Self::InvalidWorkstation,
            1330 => Self::PasswordExpired,
            1331 => Self::AccountDisabled,
            1385 => Self::LogonTypeNotGranted,
            1793 => Self::AccountExpired,
            1907 => Self::PasswordMustChange,
            1909 => Self::AccountLockedOut,
            other => Self::Other(other),
        }
    }
}

/// A `LogonUserW` refusal as an outcome. A wrong password, and any code not named here, is
/// `Failed { exhausted: false }`; a locked-out account is `Failed { exhausted: true }`. A password
/// that has expired or must be changed at the next sign-in is `Unavailable { PasswordExpired }`:
/// the owner typed the right one and must change it first, and is told so. An account Windows
/// would not sign in here whatever the password (disabled, expired, outside its hours or
/// workstations, not granted an interactive logon) is `Unavailable { NotPermittedHere }`, final
/// for now: no way of asking changes it. An account without a password is
/// `Unavailable { NotConfigured }`: there is no password to check.
pub fn map_windows_logon(code: u32) -> AuthOutcome {
    match LogonError::from_code(code) {
        LogonError::LogonFailure | LogonError::Other(_) => failed(false),
        LogonError::AccountLockedOut => failed(true),
        LogonError::AccountRestriction => unavailable(NotConfigured),
        LogonError::PasswordExpired | LogonError::PasswordMustChange => {
            unavailable(PasswordExpired)
        }
        LogonError::InvalidLogonHours
        | LogonError::InvalidWorkstation
        | LogonError::AccountDisabled
        | LogonError::LogonTypeNotGranted
        | LogonError::AccountExpired => unavailable(NotPermittedHere),
    }
}

/// `LAError`: the `code` of an `NSError` in `LAErrorDomain`, from `LAContext`'s
/// `canEvaluatePolicy:error:` or `evaluatePolicy:localizedReason:reply:`. Values as
/// objc2-local-authentication 0.3.2 generates them from Apple's `LAError.h`
/// (`src/generated/LAError.rs`, `src/generated/LAPublicDefines.rs`). The deprecated
/// `TouchIDNotAvailable`, `TouchIDNotEnrolled`, `TouchIDLockout` and `WatchNotAvailable` share
/// their values with the `Biometry*` names and `CompanionNotAvailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaErrorCode {
    AuthenticationFailed,
    UserCancel,
    UserFallback,
    SystemCancel,
    PasscodeNotSet,
    BiometryNotAvailable,
    BiometryNotEnrolled,
    BiometryLockout,
    AppCancel,
    InvalidContext,
    CompanionNotAvailable,
    BiometryNotPaired,
    BiometryDisconnected,
    InvalidDimensions,
    NotInteractive,
    /// A value `LAError.h` does not name.
    Unknown(isize),
}

impl LaErrorCode {
    /// From `NSError.code` (an `NSInteger`).
    pub const fn from_code(code: isize) -> Self {
        match code {
            -1 => Self::AuthenticationFailed,
            -2 => Self::UserCancel,
            -3 => Self::UserFallback,
            -4 => Self::SystemCancel,
            -5 => Self::PasscodeNotSet,
            -6 => Self::BiometryNotAvailable,
            -7 => Self::BiometryNotEnrolled,
            -8 => Self::BiometryLockout,
            -9 => Self::AppCancel,
            -10 => Self::InvalidContext,
            -11 => Self::CompanionNotAvailable,
            -12 => Self::BiometryNotPaired,
            -13 => Self::BiometryDisconnected,
            -14 => Self::InvalidDimensions,
            -1004 => Self::NotInteractive,
            other => Self::Unknown(other),
        }
    }
}

/// A LocalAuthentication error as an outcome. A code `LAError.h` does not name is
/// `Failed { exhausted: false }`.
///
/// `deviceOwnerAuthentication` falls back to the account password by itself, so the biometry
/// and companion codes should not reach the app; if one does, the device is not set up for
/// what was asked. `invalidate()` fails an evaluation in progress with `AppCancel` and any later
/// one on that context with `InvalidContext`: both are the app closing the prompt.
pub fn map_la_error(code: isize) -> AuthOutcome {
    match LaErrorCode::from_code(code) {
        LaErrorCode::UserCancel | LaErrorCode::UserFallback => cancelled(CancelledBy::User),
        LaErrorCode::AppCancel | LaErrorCode::InvalidContext => cancelled(CancelledBy::App),
        LaErrorCode::SystemCancel => cancelled(CancelledBy::System),
        LaErrorCode::BiometryLockout => failed(true),
        LaErrorCode::PasscodeNotSet
        | LaErrorCode::BiometryNotAvailable
        | LaErrorCode::BiometryNotEnrolled
        | LaErrorCode::CompanionNotAvailable
        | LaErrorCode::BiometryNotPaired
        | LaErrorCode::BiometryDisconnected => unavailable(NotConfigured),
        LaErrorCode::NotInteractive => unavailable(NotInteractive),
        // `InvalidDimensions` is about embedded UI, which the app does not use.
        LaErrorCode::AuthenticationFailed
        | LaErrorCode::InvalidDimensions
        | LaErrorCode::Unknown(_) => failed(false),
    }
}

/// The details key polkitd sets when the authentication agent's dialog was closed without an
/// answer (`polkitbackendinteractiveauthority.c`, polkit 127). polkit's own client reads only
/// whether the key is present (`polkit_authorization_result_get_dismissed`).
pub const POLKIT_DISMISSED: &str = "polkit.dismissed";

/// What polkitd answered to `org.freedesktop.PolicyKit1.Authority.CheckAuthorization`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolkitAnswer {
    /// The `AuthorizationResult` `(bba{ss})`: `is_authorized`, `is_challenge`, and whether
    /// the details carry [`POLKIT_DISMISSED`].
    Answered {
        authorized: bool,
        challenge: bool,
        dismissed: bool,
    },
    /// The call failed with a D-Bus error.
    Error(PolkitError),
}

impl PolkitAnswer {
    /// From the result's fields as the D-Bus reply carries them.
    pub fn from_result(
        is_authorized: bool,
        is_challenge: bool,
        details: &HashMap<String, String>,
    ) -> Self {
        Self::Answered {
            authorized: is_authorized,
            challenge: is_challenge,
            dismissed: details.contains_key(POLKIT_DISMISSED),
        }
    }
}

/// The D-Bus errors polkitd raises (`org.freedesktop.PolicyKit1.Error.*`: the first four in
/// `src/polkit/polkiterror.c`, `CancellationIdNotUnique` in
/// `src/polkitbackend/polkitbackendauthority.c`, polkit 127). There is no error for a missing
/// authentication agent: that is an answer, `is_challenge` without `is_authorized`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolkitError {
    /// For `CheckAuthorization`: the action is not registered ("Action %s is not
    /// registered"), or the subject's user cannot be found, which cannot happen to the app's
    /// own bus name while it is connected.
    Failed,
    Cancelled,
    NotSupported,
    /// The caller may not ask about this subject, or may not pass details.
    NotAuthorized,
    /// Another check with the same cancellation id is still open.
    CancellationIdNotUnique,
    /// Not polkit's: no polkitd on the system bus, a call that timed out.
    Other,
}

impl PolkitError {
    /// From the D-Bus error name.
    pub fn from_name(name: &str) -> Self {
        match name {
            "org.freedesktop.PolicyKit1.Error.Failed" => Self::Failed,
            "org.freedesktop.PolicyKit1.Error.Cancelled" => Self::Cancelled,
            "org.freedesktop.PolicyKit1.Error.NotSupported" => Self::NotSupported,
            "org.freedesktop.PolicyKit1.Error.NotAuthorized" => Self::NotAuthorized,
            "org.freedesktop.PolicyKit1.Error.CancellationIdNotUnique" => {
                Self::CancellationIdNotUnique
            }
            _ => Self::Other,
        }
    }
}

/// polkit's answer as an outcome. The order matters only for combinations polkitd does not
/// send (it sets `polkit.dismissed` with neither flag): authorized wins, then dismissed, then
/// challenge.
///
/// polkitd 127 raises no `Cancelled` for `CheckAuthorization` itself: a
/// `CancelCheckAuthorization` cancels the agent's session and comes back as a dismissal. The
/// backend therefore answers an app cancel from its own record (see the module docs).
pub fn map_polkit(answer: PolkitAnswer) -> AuthOutcome {
    match answer {
        PolkitAnswer::Answered {
            authorized: true, ..
        } => AuthOutcome::Verified,
        PolkitAnswer::Answered {
            dismissed: true, ..
        } => cancelled(CancelledBy::User),
        // "no suitable authentication agent was available" (the `is_challenge` docs in
        // `data/org.freedesktop.PolicyKit1.Authority.xml`): no dialog appeared.
        PolkitAnswer::Answered {
            challenge: true, ..
        } => unavailable(NoAgent),
        // Refused without a prompt, e.g. an inactive session under `allow_inactive=no`; whether
        // the password field stands in or the refusal is final is the authenticator's to decide
        // from the session (crate::linux::OsAuthenticator).
        PolkitAnswer::Answered { .. } => unavailable(NotPermittedHere),
        PolkitAnswer::Error(error) => match error {
            PolkitError::Failed => unavailable(PolicyMissing),
            PolkitError::Cancelled => cancelled(CancelledBy::App),
            PolkitError::NotSupported | PolkitError::Other => unavailable(NoBackend),
            PolkitError::NotAuthorized => unavailable(NotPermittedHere),
            PolkitError::CancellationIdNotUnique => AuthOutcome::Busy,
        },
    }
}

/// A Linux-PAM return code (`<security/_pam_types.h>`, Linux-PAM 1.7.1): every value
/// `pam_start(3)` and `pam_authenticate(3)` document, and `PAM_CONV_ERR`, which a
/// conversation returns to refuse a prompt. OpenPAM numbers them differently; PAM is used on
/// Linux only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PamCode {
    Success,
    SystemErr,
    BufErr,
    AuthErr,
    CredInsufficient,
    AuthinfoUnavail,
    UserUnknown,
    Maxtries,
    ConvErr,
    Abort,
    /// A code neither function documents, or none the header names.
    Other(i32),
}

impl PamCode {
    /// From the raw return value.
    pub const fn from_raw(raw: i32) -> Self {
        match raw {
            0 => Self::Success,
            4 => Self::SystemErr,
            5 => Self::BufErr,
            7 => Self::AuthErr,
            8 => Self::CredInsufficient,
            9 => Self::AuthinfoUnavail,
            10 => Self::UserUnknown,
            11 => Self::Maxtries,
            19 => Self::ConvErr,
            26 => Self::Abort,
            other => Self::Other(other),
        }
    }
}

/// A PAM return code as an outcome. `PAM_CONV_ERR` (the conversation refused, as the app's does
/// when it closes the prompt) and `PAM_ABORT` (the stack gave up) are `Cancelled { by: App }`.
/// Every code but those, `PAM_SUCCESS` and `PAM_MAXTRIES`, named or not, is
/// `Failed { exhausted: false }`.
pub fn map_pam(raw: i32) -> AuthOutcome {
    match PamCode::from_raw(raw) {
        PamCode::Success => AuthOutcome::Verified,
        PamCode::Maxtries => failed(true),
        PamCode::ConvErr | PamCode::Abort => cancelled(CancelledBy::App),
        PamCode::AuthErr
        | PamCode::UserUnknown
        | PamCode::SystemErr
        | PamCode::BufErr
        | PamCode::CredInsufficient
        | PamCode::AuthinfoUnavail
        | PamCode::Other(_) => failed(false),
    }
}

/// Refuses password attempts for [`Backoff::REFUSAL_MS`] after [`Backoff::FAILURES`] failures,
/// and a success starts the count again. The Windows credential dialog and PAM keep one each:
/// a failed `LogonUserW` counts toward the account-lockout policy (a domain account's too), and
/// PAM's own delay after a failure is a few seconds. While it refuses, a backend answers
/// `Failed { exhausted: true }` without a prompt, with how long it still will
/// ([`Refused::remaining_ms`] as `retry_in_ms`), and so does the failure that starts it.
///
/// It lives in memory, so a restart forgets it, and that is accepted: what bounds guessing
/// across restarts is the OS's own. On Linux that is PAM's delay after every failure
/// (`pam_unix`'s ~2 s, or `pam_faildelay`'s), which no restart lifts. The polkit path has no
/// back-off of the app's at all: each dialog is one authentication by polkit's agent helper,
/// through polkit's own PAM stack and that stack's delay. Open: whether a failure on the PAM
/// path also counts toward `pam_faillock` where a distribution's stack includes it, which
/// would keep a lockout across processes — its tally directory is root's, and a check from the
/// unprivileged app has not been tried against it.
///
/// Time is a monotonic millisecond reading the caller passes in (the shell's
/// `Clock::monotonic_ms`). Monotonic only, unlike the shell's deadlines, which take the larger
/// of the monotonic and the forward wall elapsed time so that they come early, never late:
/// here early is the wrong way, and a wall clock stepped either way must not lift a refusal.
/// The cost is that a monotonic clock may not count a suspend, so a refusal can outlast its
/// 30 s by the time the machine slept: acceptable for a window this short.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Backoff {
    /// Failures since the last success or refusal.
    failures: u32,
    /// The monotonic reading the current or last refusal ends at.
    refused_until: Option<u64>,
}

/// An attempt [`Backoff`] refuses, and how long it still will.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
    pub remaining_ms: u64,
}

impl Backoff {
    /// Failures that start a refusal.
    pub const FAILURES: u32 = 3;
    /// How long a refusal lasts.
    pub const REFUSAL_MS: u64 = 30_000;

    pub fn new() -> Self {
        Self::default()
    }

    /// Whether an attempt may run at `now_monotonic_ms`. A reading earlier than the one that
    /// started the refusal is still refused.
    pub fn allow(&self, now_monotonic_ms: u64) -> Result<(), Refused> {
        match self.refused_until {
            Some(until) if now_monotonic_ms < until => Err(Refused {
                remaining_ms: until - now_monotonic_ms,
            }),
            _ => Ok(()),
        }
    }

    /// An attempt failed at `now_monotonic_ms`; the [`Backoff::FAILURES`]th starts a refusal
    /// and the count again.
    pub fn record_failure(&mut self, now_monotonic_ms: u64) {
        self.failures += 1;
        if self.failures >= Self::FAILURES {
            self.failures = 0;
            self.refused_until = Some(now_monotonic_ms.saturating_add(Self::REFUSAL_MS));
        }
    }

    /// An attempt succeeded: no failure counts any more, and no refusal holds.
    pub fn record_success(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use apprafter_desktop_ipc::{AuthOutcome, CancelledBy, UnavailableReason};

    use super::{
        map_la_error, map_pam, map_polkit, map_windows_availability, map_windows_hello,
        map_windows_logon, Backoff, HelloAvailability, HelloResult, LaErrorCode, LogonError,
        PamCode, PolkitAnswer, PolkitError, Refused, WindowsRoute, POLKIT_DISMISSED,
    };

    const VERIFIED: AuthOutcome = AuthOutcome::Verified;
    const BUSY: AuthOutcome = AuthOutcome::Busy;

    const fn cancelled(by: CancelledBy) -> AuthOutcome {
        AuthOutcome::Cancelled { by }
    }

    /// No OS's answer says how long a lockout lasts.
    const fn failed(exhausted: bool) -> AuthOutcome {
        AuthOutcome::Failed {
            exhausted,
            retry_in_ms: None,
        }
    }

    const fn unavailable(reason: UnavailableReason) -> AuthOutcome {
        AuthOutcome::Unavailable { reason }
    }

    #[test]
    fn every_windows_hello_result_has_its_outcome() {
        use HelloResult as R;
        use UnavailableReason::{DisabledByPolicy, NotConfigured};
        for (raw, named, outcome) in [
            (0, R::Verified, VERIFIED),
            (1, R::DeviceNotPresent, unavailable(NotConfigured)),
            (2, R::NotConfiguredForUser, unavailable(NotConfigured)),
            (3, R::DisabledByPolicy, unavailable(DisabledByPolicy)),
            (4, R::DeviceBusy, BUSY),
            (5, R::RetriesExhausted, failed(true)),
            (6, R::Canceled, cancelled(CancelledBy::User)),
            // Unnamed: never verified, and worth another try.
            (7, R::Unknown(7), failed(false)),
            (-1, R::Unknown(-1), failed(false)),
            (i32::MAX, R::Unknown(i32::MAX), failed(false)),
            (i32::MIN, R::Unknown(i32::MIN), failed(false)),
        ] {
            assert_eq!(HelloResult::from_raw(raw), named, "{raw}");
            assert_eq!(map_windows_hello(raw), outcome, "{raw} ({named:?})");
        }
    }

    #[test]
    fn hello_availability_routes_to_hello_the_credential_dialog_or_busy() {
        use HelloAvailability as A;
        use WindowsRoute::{Busy, Credential, Hello};
        for (raw, named, route) in [
            (0, A::Available, Hello),
            (1, A::DeviceNotPresent, Credential),
            (2, A::NotConfiguredForUser, Credential),
            (3, A::DisabledByPolicy, Credential),
            (4, A::DeviceBusy, Busy),
            // Unnamed: the credential dialog, which checks the password itself.
            (5, A::Unknown(5), Credential),
            (-1, A::Unknown(-1), Credential),
            (i32::MAX, A::Unknown(i32::MAX), Credential),
            (i32::MIN, A::Unknown(i32::MIN), Credential),
        ] {
            assert_eq!(HelloAvailability::from_raw(raw), named, "{raw}");
            assert_eq!(map_windows_availability(raw), route, "{raw} ({named:?})");
        }
    }

    #[test]
    fn every_logon_error_has_its_outcome() {
        use LogonError as E;
        use UnavailableReason::{NotConfigured, NotPermittedHere, PasswordExpired};
        for (code, named, outcome) in [
            (1326, E::LogonFailure, failed(false)),
            // No password to check: Windows signs such an account in at the console only.
            (1327, E::AccountRestriction, unavailable(NotConfigured)),
            (1328, E::InvalidLogonHours, unavailable(NotPermittedHere)),
            (1329, E::InvalidWorkstation, unavailable(NotPermittedHere)),
            // The right password, expired: the owner must change it, and is told so.
            (1330, E::PasswordExpired, unavailable(PasswordExpired)),
            (1331, E::AccountDisabled, unavailable(NotPermittedHere)),
            (1385, E::LogonTypeNotGranted, unavailable(NotPermittedHere)),
            (1793, E::AccountExpired, unavailable(NotPermittedHere)),
            (1907, E::PasswordMustChange, unavailable(PasswordExpired)),
            (1909, E::AccountLockedOut, failed(true)),
            // Not named: never verified, and worth another try.
            (0, E::Other(0), failed(false)),
            (5, E::Other(5), failed(false)),
            (1311, E::Other(1311), failed(false)),
            (1325, E::Other(1325), failed(false)),
            (1910, E::Other(1910), failed(false)),
            (u32::MAX, E::Other(u32::MAX), failed(false)),
        ] {
            assert_eq!(LogonError::from_code(code), named, "{code}");
            assert_eq!(map_windows_logon(code), outcome, "{code} ({named:?})");
        }
    }

    #[test]
    fn every_la_error_has_its_outcome() {
        use CancelledBy::{App, System, User};
        use LaErrorCode as E;
        use UnavailableReason::{NotConfigured, NotInteractive};
        for (code, named, outcome) in [
            (-1, E::AuthenticationFailed, failed(false)),
            (-2, E::UserCancel, cancelled(User)),
            // The fallback button: the user ended the prompt.
            (-3, E::UserFallback, cancelled(User)),
            (-4, E::SystemCancel, cancelled(System)),
            (-5, E::PasscodeNotSet, unavailable(NotConfigured)),
            (-6, E::BiometryNotAvailable, unavailable(NotConfigured)),
            (-7, E::BiometryNotEnrolled, unavailable(NotConfigured)),
            (-8, E::BiometryLockout, failed(true)),
            // `invalidate()`: during an evaluation AppCancel, before one InvalidContext.
            (-9, E::AppCancel, cancelled(App)),
            (-10, E::InvalidContext, cancelled(App)),
            (-11, E::CompanionNotAvailable, unavailable(NotConfigured)),
            (-12, E::BiometryNotPaired, unavailable(NotConfigured)),
            (-13, E::BiometryDisconnected, unavailable(NotConfigured)),
            // Embedded UI, which the app does not use: as if unnamed.
            (-14, E::InvalidDimensions, failed(false)),
            (-1004, E::NotInteractive, unavailable(NotInteractive)),
            // Unnamed: never verified, and worth another try.
            (0, E::Unknown(0), failed(false)),
            (1, E::Unknown(1), failed(false)),
            (-15, E::Unknown(-15), failed(false)),
            (-1000, E::Unknown(-1000), failed(false)),
            (-1003, E::Unknown(-1003), failed(false)),
            (-1005, E::Unknown(-1005), failed(false)),
            (isize::MAX, E::Unknown(isize::MAX), failed(false)),
            (isize::MIN, E::Unknown(isize::MIN), failed(false)),
        ] {
            assert_eq!(LaErrorCode::from_code(code), named, "{code}");
            assert_eq!(map_la_error(code), outcome, "{code} ({named:?})");
        }
    }

    #[test]
    fn every_polkit_result_has_its_outcome() {
        use UnavailableReason::{NoAgent, NotPermittedHere};
        // All eight (authorized, challenge, dismissed). polkitd sends only the first, fifth,
        // seventh and eighth; the others pin the order.
        for ((authorized, challenge, dismissed), outcome) in [
            ((true, false, false), VERIFIED),
            ((true, true, false), VERIFIED),
            ((true, false, true), VERIFIED),
            ((true, true, true), VERIFIED),
            ((false, false, true), cancelled(CancelledBy::User)),
            ((false, true, true), cancelled(CancelledBy::User)),
            // No agent could answer: no dialog appeared.
            ((false, true, false), unavailable(NoAgent)),
            // Refused outright, e.g. an inactive session under `allow_inactive=no`.
            ((false, false, false), unavailable(NotPermittedHere)),
        ] {
            let answer = PolkitAnswer::Answered {
                authorized,
                challenge,
                dismissed,
            };
            assert_eq!(map_polkit(answer), outcome, "{answer:?}");
        }
    }

    #[test]
    fn every_polkit_error_has_its_outcome() {
        use PolkitError as E;
        use UnavailableReason::{NoBackend, NotPermittedHere, PolicyMissing};
        for (name, named, outcome) in [
            (
                "org.freedesktop.PolicyKit1.Error.Failed",
                E::Failed,
                unavailable(PolicyMissing),
            ),
            (
                "org.freedesktop.PolicyKit1.Error.Cancelled",
                E::Cancelled,
                cancelled(CancelledBy::App),
            ),
            (
                "org.freedesktop.PolicyKit1.Error.NotSupported",
                E::NotSupported,
                unavailable(NoBackend),
            ),
            (
                "org.freedesktop.PolicyKit1.Error.NotAuthorized",
                E::NotAuthorized,
                unavailable(NotPermittedHere),
            ),
            (
                "org.freedesktop.PolicyKit1.Error.CancellationIdNotUnique",
                E::CancellationIdNotUnique,
                BUSY,
            ),
            // Not polkit's, or near misses: names compare exactly.
            (
                "org.freedesktop.DBus.Error.ServiceUnknown",
                E::Other,
                unavailable(NoBackend),
            ),
            (
                "org.freedesktop.DBus.Error.NoReply",
                E::Other,
                unavailable(NoBackend),
            ),
            (
                "org.freedesktop.PolicyKit1.Error.failed",
                E::Other,
                unavailable(NoBackend),
            ),
            (
                "org.freedesktop.PolicyKit1.Error.Failed ",
                E::Other,
                unavailable(NoBackend),
            ),
            ("Cancelled", E::Other, unavailable(NoBackend)),
            ("", E::Other, unavailable(NoBackend)),
        ] {
            assert_eq!(PolkitError::from_name(name), named, "{name:?}");
            assert_eq!(map_polkit(PolkitAnswer::Error(named)), outcome, "{name:?}");
        }
    }

    #[test]
    fn dismissed_is_the_presence_of_the_polkit_detail() {
        let answered = |authorized, challenge, dismissed| PolkitAnswer::Answered {
            authorized,
            challenge,
            dismissed,
        };
        let mut details = HashMap::new();
        assert_eq!(
            PolkitAnswer::from_result(false, true, &details),
            answered(false, true, false)
        );
        assert_eq!(
            PolkitAnswer::from_result(true, false, &details),
            answered(true, false, false)
        );
        details.insert(
            "polkit.retains_authorization_after_challenge".to_owned(),
            "1".to_owned(),
        );
        assert_eq!(
            PolkitAnswer::from_result(false, false, &details),
            answered(false, false, false),
            "another detail is not a dismissal"
        );
        details.insert(POLKIT_DISMISSED.to_owned(), "true".to_owned());
        assert_eq!(
            PolkitAnswer::from_result(false, false, &details),
            answered(false, false, true)
        );
        details.insert(POLKIT_DISMISSED.to_owned(), String::new());
        assert_eq!(
            PolkitAnswer::from_result(false, false, &details),
            answered(false, false, true),
            "the key counts, not its value"
        );
        assert_eq!(POLKIT_DISMISSED, "polkit.dismissed");
    }

    #[test]
    fn every_pam_code_has_its_outcome() {
        use CancelledBy::App;
        use PamCode as P;
        for (raw, named, outcome) in [
            (0, P::Success, VERIFIED),
            (4, P::SystemErr, failed(false)),
            (5, P::BufErr, failed(false)),
            (7, P::AuthErr, failed(false)),
            (8, P::CredInsufficient, failed(false)),
            (9, P::AuthinfoUnavail, failed(false)),
            (10, P::UserUnknown, failed(false)),
            (11, P::Maxtries, failed(true)),
            (19, P::ConvErr, cancelled(App)),
            (26, P::Abort, cancelled(App)),
            // Named by the header, but neither function documents them.
            (1, P::Other(1), failed(false)),
            (3, P::Other(3), failed(false)),
            (6, P::Other(6), failed(false)),
            (24, P::Other(24), failed(false)),
            (30, P::Other(30), failed(false)),
            (31, P::Other(31), failed(false)),
            // Named nowhere.
            (32, P::Other(32), failed(false)),
            (-1, P::Other(-1), failed(false)),
            (i32::MAX, P::Other(i32::MAX), failed(false)),
            (i32::MIN, P::Other(i32::MIN), failed(false)),
        ] {
            assert_eq!(PamCode::from_raw(raw), named, "{raw}");
            assert_eq!(map_pam(raw), outcome, "{raw} ({named:?})");
        }
    }

    const fn refused(remaining_ms: u64) -> Result<(), Refused> {
        Err(Refused { remaining_ms })
    }

    #[test]
    fn three_failures_refuse_for_thirty_seconds() {
        assert_eq!((Backoff::FAILURES, Backoff::REFUSAL_MS), (3, 30_000));
        let mut backoff = Backoff::new();
        assert_eq!(backoff.allow(0), Ok(()));
        backoff.record_failure(1_000);
        assert_eq!(backoff.allow(1_000), Ok(()));
        backoff.record_failure(2_000);
        assert_eq!(backoff.allow(2_000), Ok(()));
        backoff.record_failure(3_000);
        assert_eq!(backoff.allow(3_000), refused(30_000));
        assert_eq!(backoff.allow(20_000), refused(13_000));
        assert_eq!(backoff.allow(32_999), refused(1));
        assert_eq!(backoff.allow(33_000), Ok(()));
        assert_eq!(backoff.allow(1_000_000), Ok(()));
    }

    #[test]
    fn after_a_refusal_three_new_failures_start_the_next() {
        let mut backoff = Backoff::new();
        for at in [1_000, 2_000, 3_000] {
            backoff.record_failure(at);
        }
        assert!(backoff.allow(32_999).is_err());
        backoff.record_failure(40_000);
        backoff.record_failure(41_000);
        assert_eq!(backoff.allow(41_000), Ok(()), "the refusal reset the count");
        backoff.record_failure(42_000);
        assert_eq!(backoff.allow(42_000), refused(30_000));
        assert_eq!(backoff.allow(72_000), Ok(()));
    }

    #[test]
    fn a_success_starts_the_count_again() {
        let mut backoff = Backoff::new();
        backoff.record_failure(1_000);
        backoff.record_failure(2_000);
        backoff.record_success();
        backoff.record_failure(3_000);
        backoff.record_failure(4_000);
        assert_eq!(backoff.allow(4_000), Ok(()), "two since the success");
        backoff.record_failure(5_000);
        assert_eq!(backoff.allow(5_000), refused(30_000));
        backoff.record_success();
        assert_eq!(backoff.allow(5_000), Ok(()), "a success ends a refusal too");
        assert_eq!(backoff, Backoff::new());
    }

    #[test]
    fn failures_count_however_far_apart() {
        let mut backoff = Backoff::new();
        for at in [0, 3_600_000, 86_400_000] {
            backoff.record_failure(at);
        }
        assert_eq!(backoff.allow(86_400_000), refused(30_000));
    }

    #[test]
    fn a_reading_from_before_the_refusal_is_still_refused() {
        let mut backoff = Backoff::new();
        for _ in 0..3 {
            backoff.record_failure(100_000);
        }
        assert_eq!(backoff.allow(99_999), refused(30_001));
        assert_eq!(backoff.allow(0), refused(130_000));
    }

    #[test]
    fn a_refusal_near_the_end_of_the_clock_saturates() {
        let mut backoff = Backoff::new();
        for _ in 0..3 {
            backoff.record_failure(u64::MAX - 10);
        }
        assert_eq!(backoff.allow(u64::MAX - 10), refused(10));
        assert_eq!(backoff.allow(u64::MAX - 1), refused(1));
    }
}
