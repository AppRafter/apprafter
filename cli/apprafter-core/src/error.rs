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
//! adds the variants the core itself raises. Messages carry no client
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

    /// An error from the CLI's shared crates, passed through unchanged.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Cli(#[from] cli_core::CliError),
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
        if let CoreError::TargetNotFound { name, available } = e {
            ui.fields.insert("name".into(), serde_json::json!(name));
            ui.fields
                .insert("available".into(), serde_json::json!(available));
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
    fn cancelled_converts_from_the_token_error() {
        let e: CoreError = Cancelled.into();
        assert_eq!(
            UiError::from(&e).code.as_deref(),
            Some("apprafter::op::cancelled")
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
