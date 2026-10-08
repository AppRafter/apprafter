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
    NotPermittedHere,
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
    /// `exhausted` when the OS allows no further attempt.
    Failed {
        exhausted: bool,
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
                AuthOutcome::Failed { exhausted: true },
                r#"{"outcome":"failed","exhausted":true}"#,
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
