// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// miette-derive 7.6 reassigns named-field bindings in its generated Diagnostic impl and trips
// `unused_assignments` on every variant with fields (as in the core's error.rs); the lint
// fires on generated code we do not control.
#![allow(unused_assignments)]
//! The errors the desktop shell itself raises, and the one way they reach the webview.
//!
//! Every code is an [`apprafter_desktop_ipc::errors`] constant (a test holds them equal); a
//! core error passes through with its own code and fields. [`DesktopError::to_ui`] is the
//! projection every command returns.

use apprafter_core::{CoreError, UiError};
use apprafter_desktop_ipc::{OpId, UnavailableReason};
use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum DesktopError {
    #[error("AppRafter is locked")]
    #[diagnostic(
        code(apprafter::desktop::locked),
        help("Unlock AppRafter to continue.")
    )]
    Locked,

    #[error("operation {} has no pending plan", op_id.0)]
    #[diagnostic(
        code(apprafter::desktop::plan_not_found),
        help(
            "It already ran, was discarded, or was dropped when AppRafter locked; plan it again."
        )
    )]
    PlanNotFound { op_id: OpId },

    #[error("the plan for operation {} expired", op_id.0)]
    #[diagnostic(
        code(apprafter::desktop::plan_expired),
        help("A plan is valid for 10 minutes; plan it again.")
    )]
    PlanExpired { op_id: OpId },

    #[error("authentication was cancelled")]
    #[diagnostic(code(apprafter::desktop::auth_cancelled))]
    AuthCancelled,

    #[error("authentication failed")]
    #[diagnostic(code(apprafter::desktop::auth_failed))]
    AuthFailed { exhausted: bool },

    #[error(
        "device-owner authentication is unavailable here ({})",
        wire_name(reason)
    )]
    #[diagnostic(code(apprafter::desktop::auth_unavailable))]
    AuthUnavailable { reason: UnavailableReason },

    #[error("another authentication prompt is already open")]
    #[diagnostic(
        code(apprafter::desktop::auth_busy),
        help("Answer or close the open prompt, then try again.")
    )]
    AuthBusy,

    #[error("settings could not be saved: {0}")]
    #[diagnostic(code(apprafter::desktop::settings_io))]
    SettingsIo(String),

    #[error("internal error: {0}")]
    #[diagnostic(code(apprafter::desktop::internal))]
    Internal(String),

    #[error("AppRafter is quitting")]
    #[diagnostic(
        code(apprafter::desktop::closing),
        help("Nothing new starts while AppRafter quits; start it again to continue.")
    )]
    Closing,

    #[error(transparent)]
    #[diagnostic(transparent)]
    Core(#[from] CoreError),
}

/// How a reason is spelled on the wire (`no_backend`), so the message and
/// `fields.reason` agree.
fn wire_name(reason: &UnavailableReason) -> String {
    match serde_json::to_value(reason) {
        Ok(serde_json::Value::String(s)) => s,
        _ => format!("{reason:?}"),
    }
}

impl DesktopError {
    /// What a command returns: the diagnostic's code, message, help and causes, plus the
    /// structured `fields` the webview acts on (`opId`, `reason`, `exhausted` — camelCase,
    /// as every other key on the wire). A core error keeps the core's own projection.
    pub fn to_ui(&self) -> UiError {
        if let DesktopError::Core(core) = self {
            return UiError::from(core);
        }
        let mut ui = UiError::from_diagnostic(self);
        match self {
            DesktopError::PlanNotFound { op_id } | DesktopError::PlanExpired { op_id } => {
                ui.fields.insert("opId".into(), serde_json::json!(op_id.0));
            }
            DesktopError::AuthFailed { exhausted } => {
                ui.fields
                    .insert("exhausted".into(), serde_json::json!(exhausted));
            }
            DesktopError::AuthUnavailable { reason } => {
                ui.fields
                    .insert("reason".into(), serde_json::json!(wire_name(reason)));
            }
            DesktopError::Locked
            | DesktopError::AuthCancelled
            | DesktopError::AuthBusy
            | DesktopError::SettingsIo(_)
            | DesktopError::Internal(_)
            | DesktopError::Closing
            | DesktopError::Core(_) => {}
        }
        ui
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use apprafter_core::{CoreError, UiError};
    use apprafter_desktop_ipc::{errors, OpId, UnavailableReason};
    use serde_json::json;

    use super::DesktopError;

    /// One of each variant. `code_of` below is exhaustive, so a new variant cannot compile
    /// until it has a code here.
    fn one_of_each() -> Vec<DesktopError> {
        vec![
            DesktopError::Locked,
            DesktopError::PlanNotFound { op_id: OpId(7) },
            DesktopError::PlanExpired { op_id: OpId(7) },
            DesktopError::AuthCancelled,
            DesktopError::AuthFailed { exhausted: true },
            DesktopError::AuthUnavailable {
                reason: UnavailableReason::NoBackend,
            },
            DesktopError::AuthBusy,
            DesktopError::SettingsIo("disk full".into()),
            DesktopError::Internal("bug".into()),
            DesktopError::Closing,
        ]
    }

    fn code_of(e: &DesktopError) -> Option<&'static str> {
        Some(match e {
            DesktopError::Locked => errors::LOCKED,
            DesktopError::PlanNotFound { .. } => errors::PLAN_NOT_FOUND,
            DesktopError::PlanExpired { .. } => errors::PLAN_EXPIRED,
            DesktopError::AuthCancelled => errors::AUTH_CANCELLED,
            DesktopError::AuthFailed { .. } => errors::AUTH_FAILED,
            DesktopError::AuthUnavailable { .. } => errors::AUTH_UNAVAILABLE,
            DesktopError::AuthBusy => errors::AUTH_BUSY,
            DesktopError::SettingsIo(_) => errors::SETTINGS_IO,
            DesktopError::Internal(_) => errors::INTERNAL,
            DesktopError::Closing => errors::CLOSING,
            DesktopError::Core(_) => return None,
        })
    }

    #[test]
    fn every_variant_raises_its_ipc_constant_and_all_lists_exactly_those() {
        let mut seen = BTreeSet::new();
        for e in one_of_each() {
            let expected = code_of(&e).expect("a desktop variant");
            assert_eq!(e.to_ui().code.as_deref(), Some(expected), "{e:?}");
            seen.insert(expected);
        }
        assert_eq!(seen.len(), one_of_each().len(), "two variants share a code");
        assert_eq!(seen, errors::ALL.iter().copied().collect::<BTreeSet<_>>());
        assert_eq!(errors::ALL.len(), one_of_each().len());
    }

    #[test]
    fn plan_errors_carry_the_op_id_as_a_number() {
        for e in [
            DesktopError::PlanNotFound { op_id: OpId(42) },
            DesktopError::PlanExpired { op_id: OpId(42) },
        ] {
            let ui = e.to_ui();
            assert_eq!(ui.fields["opId"], json!(42), "{ui:?}");
            assert_eq!(
                ui.fields.len(),
                1,
                "camelCase like every other wire key: {ui:?}"
            );
            assert!(ui.message.contains("42"), "{}", ui.message);
            assert!(ui.help.is_some());
        }
    }

    #[test]
    fn auth_errors_carry_what_the_webview_acts_on() {
        let ui = DesktopError::AuthFailed { exhausted: true }.to_ui();
        assert_eq!(ui.fields["exhausted"], json!(true));
        let ui = DesktopError::AuthFailed { exhausted: false }.to_ui();
        assert_eq!(ui.fields["exhausted"], json!(false));
        let ui = DesktopError::AuthUnavailable {
            reason: UnavailableReason::PolicyMissing,
        }
        .to_ui();
        assert_eq!(ui.fields["reason"], json!("policy_missing"));
        assert!(ui.message.contains("policy_missing"), "{}", ui.message);
        for e in [
            DesktopError::Locked,
            DesktopError::AuthCancelled,
            DesktopError::AuthBusy,
            DesktopError::Closing,
        ] {
            assert!(e.to_ui().fields.is_empty(), "{e:?}");
        }
    }

    #[test]
    fn a_core_error_passes_through_with_its_code_and_fields() {
        let core = CoreError::TargetNotFound {
            name: "ghost".into(),
            available: vec!["prod".into()],
        };
        let expected = UiError::from(&core);
        let ui = DesktopError::from(core).to_ui();
        assert_eq!(ui, expected);
        assert_eq!(ui.code.as_deref(), Some("apprafter::target::not_found"));
        assert_eq!(ui.fields["available"], json!(["prod"]));
    }

    #[test]
    fn internal_and_settings_errors_say_what_happened() {
        let ui = DesktopError::Internal("the operation panicked: boom".into()).to_ui();
        assert_eq!(ui.message, "internal error: the operation panicked: boom");
        let ui = DesktopError::SettingsIo("disk full".into()).to_ui();
        assert_eq!(ui.message, "settings could not be saved: disk full");
    }
}
