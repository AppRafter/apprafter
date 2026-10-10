// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Device-owner authentication results: one shape for every OS backend (D.2d).

use serde::Serialize;

/// Who closed an authentication prompt before it answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CancelledBy {
    User,
    /// The app itself, e.g. on lock-on-sleep or quit.
    App,
    System,
}

/// Why the OS cannot authenticate the device owner here; the UI says which and what to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    NotConfigured,
    DisabledByPolicy,
    NoAgent,
    PolicyMissing,
    ImplicitGrant,
    /// The OS refuses here, for good as things stand: an administrator's polkit rule saying NO
    /// in the owner's active session, or the account's own restrictions (Windows: its logon
    /// hours or workstations, the account disabled or expired). Asking again, either way, does
    /// not change it.
    NotPermittedHere,
    /// The app's own password field was used where the OS prompts itself (polkit can prompt;
    /// macOS and Windows always do): the OS's prompt is the way, and asking through it works.
    UseSystemPrompt,
    /// The mirror of `UseSystemPrompt`: the OS's prompt cannot be used here, but the app's own
    /// password field can, and `AppInfo` now offers it (Linux: polkit refused outside an active
    /// local session, where its own defaults refuse and PAM stands in). Asking through the
    /// field works.
    UsePasswordField,
    /// The account's password has expired, or must be changed at the next sign-in (Windows'
    /// `LogonUserW`): the right password is refused until the owner changes it. Asking again
    /// works once it is changed, so a plan waits for that try.
    PasswordExpired,
    NoPamService,
    NotInteractive,
    NoBackend,
}

/// How one authentication request ended; the same for every OS backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AuthOutcome {
    Verified,
    Cancelled {
        by: CancelledBy,
    },
    /// `exhausted` when no further attempt is allowed for now.
    Failed {
        exhausted: bool,
        /// With `exhausted`, when the app's own back-off refuses: how long it still will, in
        /// milliseconds of the monotonic clock it counts in. `None` otherwise — a wrong
        /// password with attempts left, or the OS's own lockout (Windows Hello's, Touch ID's,
        /// an account lockout, PAM's), whose end the app is not told.
        #[serde(rename = "retryInMs")]
        retry_in_ms: Option<u64>,
    },
    /// Another prompt is already open.
    Busy,
    Unavailable {
        reason: UnavailableReason,
    },
}

/// Which OS mechanism authenticates the device owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    WindowsHello,
    WindowsCredential,
    MacLocalAuthentication,
    Polkit,
    Pam,
    /// The test build's scripted authenticator.
    Fake,
}

/// What the webview knows about authentication here, to build the lock screen and settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AuthInfo {
    pub available: bool,
    pub method: Option<AuthMethod>,
    /// Set when `available` is false.
    pub unavailable: Option<UnavailableReason>,
    /// The OS offers a choice between biometrics and the password (the `hello` setting).
    pub biometrics_choice: bool,
    /// The webview shows its own password field because the OS cannot prompt.
    pub password_field: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_carries_its_reason() {
        let outcome = AuthOutcome::Unavailable {
            reason: UnavailableReason::PolicyMissing,
        };
        assert_eq!(
            serde_json::to_string(&outcome).unwrap(),
            r#"{"outcome":"unavailable","reason":"policy_missing"}"#
        );
    }

    #[test]
    fn every_outcome_is_tagged_by_outcome() {
        for (outcome, wire) in [
            (AuthOutcome::Verified, r#"{"outcome":"verified"}"#),
            (
                AuthOutcome::Cancelled {
                    by: CancelledBy::User,
                },
                r#"{"outcome":"cancelled","by":"user"}"#,
            ),
            (
                AuthOutcome::Failed {
                    exhausted: false,
                    retry_in_ms: None,
                },
                r#"{"outcome":"failed","exhausted":false,"retryInMs":null}"#,
            ),
            (
                AuthOutcome::Failed {
                    exhausted: true,
                    retry_in_ms: Some(29_500),
                },
                r#"{"outcome":"failed","exhausted":true,"retryInMs":29500}"#,
            ),
            (AuthOutcome::Busy, r#"{"outcome":"busy"}"#),
        ] {
            assert_eq!(serde_json::to_string(&outcome).unwrap(), wire);
        }
    }

    #[test]
    fn every_unavailable_reason_is_snake_case() {
        for (reason, wire) in [
            (UnavailableReason::NotConfigured, "not_configured"),
            (UnavailableReason::DisabledByPolicy, "disabled_by_policy"),
            (UnavailableReason::NoAgent, "no_agent"),
            (UnavailableReason::PolicyMissing, "policy_missing"),
            (UnavailableReason::ImplicitGrant, "implicit_grant"),
            (UnavailableReason::NotPermittedHere, "not_permitted_here"),
            (UnavailableReason::UseSystemPrompt, "use_system_prompt"),
            (UnavailableReason::UsePasswordField, "use_password_field"),
            (UnavailableReason::PasswordExpired, "password_expired"),
            (UnavailableReason::NoPamService, "no_pam_service"),
            (UnavailableReason::NotInteractive, "not_interactive"),
            (UnavailableReason::NoBackend, "no_backend"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), wire);
        }
    }

    #[test]
    fn auth_info_is_camel_case_with_snake_case_values() {
        let info = AuthInfo {
            available: true,
            method: Some(AuthMethod::MacLocalAuthentication),
            unavailable: None,
            biometrics_choice: true,
            password_field: false,
        };
        assert_eq!(
            serde_json::to_string(&info).unwrap(),
            r#"{"available":true,"method":"mac_local_authentication","unavailable":null,"biometricsChoice":true,"passwordField":false}"#
        );
        let none = AuthInfo {
            available: false,
            method: None,
            unavailable: Some(UnavailableReason::NoBackend),
            biometrics_choice: false,
            password_field: false,
        };
        assert_eq!(
            serde_json::to_string(&none).unwrap(),
            r#"{"available":false,"method":null,"unavailable":"no_backend","biometricsChoice":false,"passwordField":false}"#
        );
    }
}
