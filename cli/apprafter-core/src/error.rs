// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// miette-derive 7.6 reassigns named-field bindings in its generated
// Diagnostic impl and trips `unused_assignments` on every variant, as in
// cli-core's error.rs; the lint fires on generated code we do not control.
#![allow(unused_assignments)]
//! Errors of the shared core, and their serialisable projection for the
//! desktop.
//!
//! [`CoreError`] wraps `cli_core::CliError` (left unchanged: it is also the
//! error type of the backup engine and reaches the in-cluster runner) and
//! adds the variants the core itself raises. A code has one shape: the
//! `CliError` variants the core also raises (`TargetNotFound`,
//! `NoActiveTarget`) are mapped onto the core's own on the way in. Messages carry no client
//! wording ("pass `--yes`", "run `apprafter …`"): the CLI adds those hints
//! when it renders, the desktop turns codes into actions.

use std::collections::BTreeMap;

use miette::Diagnostic;
use serde::Serialize;
use thiserror::Error;

use crate::cancel::Cancelled;

/// The result type of every core operation.
pub type CoreResult<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error, Diagnostic)]
pub enum CoreError {
    /// No target was named and the store has no active target.
    #[error("no active target")]
    #[diagnostic(code(apprafter::target::no_active))]
    NoActiveTarget,

    /// A target was named that the store does not contain.
    #[error("target `{name}` not found (available: {})", available.join(", "))]
    #[diagnostic(code(apprafter::target::not_found))]
    TargetNotFound {
        name: String,
        /// Configured target names, sorted; empty on a fresh store.
        available: Vec<String>,
    },

    /// The operation's [`CancellationToken`](crate::CancellationToken) was
    /// tripped.
    #[error("operation cancelled")]
    #[diagnostic(code(apprafter::op::cancelled))]
    Cancelled,

    /// The environment asked for something the client's policy forbids, e.g.
    /// a non-loopback provider API base in a desktop test build. Refused
    /// rather than ignored: a walk that silently fell back to the real API
    /// would send whatever token the store holds there.
    #[error("{var} is not allowed here: {reason}")]
    #[diagnostic(code(apprafter::env::unsafe_override))]
    UnsafeOverride { var: &'static str, reason: String },

    /// An error from the CLI's shared crates, passed through unchanged —
    /// except `TargetNotFound` and `NoActiveTarget`, which [`From`] maps
    /// onto the core's own.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Cli(cli_core::CliError),
}

/// One shape per error code: `apprafter::target::not_found` always arrives
/// as [`CoreError::TargetNotFound`] with `available` as a list, never as a
/// wrapped `CliError::TargetNotFound` whose `available` is one joined
/// string. Splitting that string on `", "` is exact: the CLI only creates
/// target names in `[A-Za-z0-9-]+`, and an empty string means an empty
/// store. `apprafter::target::no_active` likewise always arrives as
/// [`CoreError::NoActiveTarget`]. Every other `CliError` is wrapped
/// unchanged.
impl From<cli_core::CliError> for CoreError {
    fn from(e: cli_core::CliError) -> Self {
        match e {
            cli_core::CliError::TargetNotFound { name, available } => CoreError::TargetNotFound {
                name,
                available: available
                    .split(", ")
                    .filter(|n| !n.is_empty())
                    .map(str::to_string)
                    .collect(),
            },
            cli_core::CliError::NoActiveTarget => CoreError::NoActiveTarget,
            other => CoreError::Cli(other),
        }
    }
}

impl From<Cancelled> for CoreError {
    fn from(_: Cancelled) -> Self {
        CoreError::Cancelled
    }
}

/// A serialisable view of any diagnostic, for the desktop's IPC.
///
/// `code` is the full miette code (`apprafter::target::not_found`); the
/// desktop maps known codes to actions and shows the rest verbatim.
/// `fields` carries the structured data some variants have, so the UI never
/// parses prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UiError {
    pub code: Option<String>,
    pub message: String,
    pub help: Option<String>,
    pub causes: Vec<String>,
    pub fields: BTreeMap<String, serde_json::Value>,
}

impl UiError {
    /// Project any diagnostic: its message, code, help, and the cause chain
    /// (the `source()` chain, then a `diagnostic_source` chain if it adds
    /// anything new).
    pub fn from_diagnostic(d: &dyn Diagnostic) -> Self {
        let mut causes: Vec<String> = Vec::new();
        let mut next = d.source();
        while let Some(e) = next {
            causes.push(e.to_string());
            next = e.source();
        }
        if let Some(ds) = d.diagnostic_source() {
            let mut cur: Option<&dyn std::error::Error> = Some(ds);
            while let Some(e) = cur {
                let text = e.to_string();
                if !causes.contains(&text) {
                    causes.push(text);
                }
                cur = e.source();
            }
        }
        UiError {
            code: d.code().map(|c| c.to_string()),
            message: d.to_string(),
            help: d.help().map(|h| h.to_string()),
            causes,
            fields: BTreeMap::new(),
        }
    }
}

impl From<&CoreError> for UiError {
    fn from(e: &CoreError) -> Self {
        let mut ui = UiError::from_diagnostic(e);
        match e {
            CoreError::TargetNotFound { name, available } => {
                ui.fields.insert("name".into(), serde_json::json!(name));
                ui.fields
                    .insert("available".into(), serde_json::json!(available));
            }
            CoreError::UnsafeOverride { var, .. } => {
                ui.fields.insert("var".into(), serde_json::json!(var));
            }
            _ => {}
        }
        ui
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_not_found_renders_and_projects_its_fields() {
        let e = CoreError::TargetNotFound {
            name: "ghost".into(),
            available: vec!["prod".into(), "staging".into()],
        };
        assert_eq!(
            e.to_string(),
            "target `ghost` not found (available: prod, staging)"
        );
        let ui = UiError::from(&e);
        assert_eq!(ui.code.as_deref(), Some("apprafter::target::not_found"));
        assert_eq!(ui.fields["name"], serde_json::json!("ghost"));
        assert_eq!(
            ui.fields["available"],
            serde_json::json!(["prod", "staging"])
        );
    }

    #[test]
    fn target_not_found_on_an_empty_store_lists_nothing() {
        let e = CoreError::TargetNotFound {
            name: "x".into(),
            available: vec![],
        };
        assert_eq!(e.to_string(), "target `x` not found (available: )");
    }

    #[test]
    fn no_active_target_has_its_own_code() {
        let ui = UiError::from(&CoreError::NoActiveTarget);
        assert_eq!(ui.code.as_deref(), Some("apprafter::target::no_active"));
        assert_eq!(ui.message, "no active target");
    }

    #[test]
    fn cli_errors_pass_through_with_their_code_and_help() {
        let inner = cli_core::CliError::BackupJobActive {
            job: "nightly-1".into(),
        };
        let expected_message = inner.to_string();
        let e = CoreError::from(inner);
        let ui = UiError::from(&e);
        assert_eq!(ui.code.as_deref(), Some("apprafter::backup::job_active"));
        assert_eq!(ui.message, expected_message);
        assert!(
            ui.help.is_some(),
            "the typed CliError help survives the wrap"
        );
    }

    #[test]
    fn a_cli_target_not_found_takes_the_core_shape_with_a_list() {
        let e = CoreError::from(cli_core::CliError::TargetNotFound {
            name: "ghost".into(),
            available: "dev, work".into(),
        });
        match &e {
            CoreError::TargetNotFound { name, available } => {
                assert_eq!(name, "ghost");
                assert_eq!(available, &vec!["dev".to_string(), "work".to_string()]);
            }
            other => panic!("expected the core's TargetNotFound, got {other:?}"),
        }
        assert_eq!(
            e.to_string(),
            "target `ghost` not found (available: dev, work)"
        );
        let ui = UiError::from(&e);
        assert_eq!(ui.code.as_deref(), Some("apprafter::target::not_found"));
        assert_eq!(ui.fields["available"], serde_json::json!(["dev", "work"]));
    }

    #[test]
    fn a_cli_target_not_found_on_an_empty_store_has_an_empty_list() {
        let e = CoreError::from(cli_core::CliError::TargetNotFound {
            name: "x".into(),
            available: String::new(),
        });
        assert!(matches!(
            &e,
            CoreError::TargetNotFound { available, .. } if available.is_empty()
        ));
        assert_eq!(UiError::from(&e).fields["available"], serde_json::json!([]));
    }

    #[test]
    fn a_cli_no_active_target_takes_the_core_shape() {
        let e = CoreError::from(cli_core::CliError::NoActiveTarget);
        assert!(matches!(e, CoreError::NoActiveTarget), "{e:?}");
        let ui = UiError::from(&e);
        assert_eq!(ui.code.as_deref(), Some("apprafter::target::no_active"));
        assert_eq!(ui.message, "no active target");
    }

    #[test]
    fn every_other_cli_error_is_wrapped() {
        let e = CoreError::from(cli_core::CliError::BackupJobActive {
            job: "nightly-1".into(),
        });
        assert!(matches!(e, CoreError::Cli(_)), "{e:?}");
    }

    #[test]
    fn an_io_error_carries_its_os_message_as_the_only_cause() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied: /x");
        let ui = UiError::from(&CoreError::from(cli_core::CliError::Io(io)));
        assert_eq!(ui.code.as_deref(), Some("apprafter::io::error"));
        assert_eq!(ui.message, "io error: denied: /x");
        assert_eq!(ui.causes, vec!["denied: /x".to_string()]);
    }

    #[test]
    fn a_rejected_token_carries_the_provider_error_once() {
        let provider_error = cli_core::CliError::Hetzner {
            endpoint: "GET /v1/locations".into(),
            status: 401,
            code: "unauthorized".into(),
            message: "invalid token".into(),
        };
        let provider_text = provider_error.to_string();
        let ui = UiError::from(&CoreError::from(
            cli_core::CliError::ProviderTokenRejected {
                provider: "hetzner-cloud".into(),
                cause: Box::new(provider_error),
            },
        ));
        assert_eq!(
            ui.code.as_deref(),
            Some("apprafter::target::token_rejected")
        );
        assert_eq!(
            ui.message,
            "provider `hetzner-cloud` rejected the supplied token"
        );
        // Reached through both `source()` and `diagnostic_source()`, listed
        // once; the message itself is not repeated as a cause.
        assert_eq!(ui.causes, vec![provider_text]);
    }

    #[test]
    fn cancelled_converts_from_the_token_error() {
        let e: CoreError = Cancelled.into();
        assert_eq!(
            UiError::from(&e).code.as_deref(),
            Some("apprafter::op::cancelled")
        );
    }

    #[test]
    fn an_unsafe_override_names_its_variable_as_a_field() {
        let e = CoreError::UnsafeOverride {
            var: "APPRAFTER_HCLOUD_BASE_URL",
            reason: "not loopback".into(),
        };
        assert_eq!(
            e.to_string(),
            "APPRAFTER_HCLOUD_BASE_URL is not allowed here: not loopback"
        );
        let ui = UiError::from(&e);
        assert_eq!(ui.code.as_deref(), Some("apprafter::env::unsafe_override"));
        assert_eq!(
            ui.fields["var"],
            serde_json::json!("APPRAFTER_HCLOUD_BASE_URL")
        );
    }

    #[test]
    fn ui_error_serialises_with_stable_keys() {
        let ui = UiError::from(&CoreError::NoActiveTarget);
        let v = serde_json::to_value(&ui).unwrap();
        for key in ["code", "message", "help", "causes", "fields"] {
            assert!(v.get(key).is_some(), "missing key {key} in {v}");
        }
    }
}
