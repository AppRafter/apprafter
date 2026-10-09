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
//!
//! [`codes`] names every code the core raises or the desktop acts on, and
//! [`UiError`]'s `fields` carry each variant's structured data — the core's
//! own and those of the pass-through `CliError`s the desktop acts on (a
//! Hetzner API status, a missing tool) — under camelCase keys, `status` as
//! a JSON number (D.3 overview R12).

use std::collections::BTreeMap;

use miette::Diagnostic;
use serde::Serialize;
use serde_json::json;
use thiserror::Error;

use crate::cancel::Cancelled;
use crate::kube::KubeErrorKind;
use crate::provider::TokenProblem;
use crate::ssh::SshKeyProblem;
use crate::target::NameProblem;

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

    /// A new target's name is taken.
    #[error("target `{name}` already exists")]
    #[diagnostic(code(apprafter::target::exists))]
    TargetExists { name: String },

    /// A renewal would store the token the target already has.
    #[error("the new token for `{name}` is the one already stored")]
    #[diagnostic(code(apprafter::target::renew_token_unchanged))]
    RenewTokenUnchanged { name: String },

    /// The message is the CLI's own reason text for the name, byte for byte.
    #[error("{}", problem.reason(name))]
    #[diagnostic(code(apprafter::target::invalid_name))]
    InvalidTargetName { name: String, problem: NameProblem },

    /// A rename to the name the target already has.
    #[error("source and destination target names are both `{name}` — nothing to rename")]
    #[diagnostic(code(apprafter::target::same_name))]
    SameTargetName { name: String },

    #[error("provider `{provider}` is not supported (supported: {})", supported.join(", "))]
    #[diagnostic(code(apprafter::target::unknown_provider))]
    UnknownProvider {
        provider: String,
        supported: Vec<String>,
    },

    /// A malformed provider token; the message never shows the token.
    #[error("invalid Hetzner Cloud token: {}", problem.reason())]
    #[diagnostic(code(apprafter::target::invalid_token))]
    InvalidToken { problem: TokenProblem },

    #[error("target `{name}` has no provider token stored")]
    #[diagnostic(code(apprafter::target::token_missing))]
    TokenNotStored { name: String },

    #[error("{}", problem.message(path, error.as_deref()))]
    #[diagnostic(code(apprafter::target::ssh_key_unreadable))]
    SshKeyUnreadable {
        path: String,
        problem: SshKeyProblem,
        /// The OS error, for `Unreadable`.
        error: Option<String>,
    },

    /// The target's local state records no server.
    #[error("target `{name}` has no provisioned server")]
    #[diagnostic(code(apprafter::target::not_provisioned))]
    NotProvisioned { name: String },

    /// The provider answered 404 for the server the local state records.
    #[error("the server recorded for `{name}` (id {server_id}) no longer exists at the provider")]
    #[diagnostic(code(apprafter::target::server_missing))]
    ServerMissing { name: String, server_id: u64 },

    /// A machine or region change on a target whose server exists.
    #[error(
        "target `{name}` has a provisioned server (`{server_name}`, id {server_id}), so its \
         machine or region cannot change"
    )]
    #[diagnostic(code(apprafter::target::provisioned))]
    TargetProvisioned {
        name: String,
        server_id: u64,
        server_name: String,
    },

    /// A provider read failed for a reason other than an API status (transport, timeout,
    /// parse); `CliError::Hetzner` passes through unwrapped so its `status` projects. The cause
    /// is a plain `#[source]`: miette-derive borrows a `diagnostic_source` as `&dyn Diagnostic`
    /// through `Borrow`, which `Box<CoreError>` does not provide.
    #[error("the {provider} API request {endpoint} failed")]
    #[diagnostic(code(apprafter::provider::request_failed))]
    ProviderRequestFailed {
        provider: String,
        endpoint: String,
        #[source]
        cause: Box<CoreError>,
    },

    /// A tool found only as a `.cmd` / `.bat` shim (Windows), which cannot be run directly.
    #[error("`{tool}` was found as {path}, which cannot be run directly (a .cmd or .bat shim)")]
    #[diagnostic(code(apprafter::env::tool_unsupported))]
    ToolUnsupported { tool: String, path: String },

    #[error("the cluster API request failed ({}): {detail}", kind.as_str())]
    #[diagnostic(code(apprafter::cluster::kube_failed))]
    Kube { kind: KubeErrorKind, detail: String },

    /// The age identity is absent, and a read never creates one.
    #[error("no age key at {path}; the cached secrets cannot be decrypted")]
    #[diagnostic(code(apprafter::secrets::age_key_missing))]
    AgeKeyMissing { path: String },
}

/// Every code the core raises or the desktop acts on: the core's own, and the pass-through
/// `CliError` codes the desktop turns into actions. The desktop exports [`ALL`] as its list of
/// known codes (D.3d).
pub mod codes {
    pub const NO_ACTIVE_TARGET: &str = "apprafter::target::no_active";
    pub const TARGET_NOT_FOUND: &str = "apprafter::target::not_found";
    pub const CANCELLED: &str = "apprafter::op::cancelled";
    pub const UNSAFE_OVERRIDE: &str = "apprafter::env::unsafe_override";
    pub const TARGET_EXISTS: &str = "apprafter::target::exists";
    pub const RENEW_TOKEN_UNCHANGED: &str = "apprafter::target::renew_token_unchanged";
    pub const INVALID_TARGET_NAME: &str = "apprafter::target::invalid_name";
    pub const SAME_TARGET_NAME: &str = "apprafter::target::same_name";
    pub const UNKNOWN_PROVIDER: &str = "apprafter::target::unknown_provider";
    pub const INVALID_TOKEN: &str = "apprafter::target::invalid_token";
    pub const TOKEN_NOT_STORED: &str = "apprafter::target::token_missing";
    pub const SSH_KEY_UNREADABLE: &str = "apprafter::target::ssh_key_unreadable";
    pub const NOT_PROVISIONED: &str = "apprafter::target::not_provisioned";
    pub const SERVER_MISSING: &str = "apprafter::target::server_missing";
    pub const TARGET_PROVISIONED: &str = "apprafter::target::provisioned";
    pub const PROVIDER_REQUEST_FAILED: &str = "apprafter::provider::request_failed";
    pub const TOOL_UNSUPPORTED: &str = "apprafter::env::tool_unsupported";
    pub const KUBE_FAILED: &str = "apprafter::cluster::kube_failed";
    pub const AGE_KEY_MISSING: &str = "apprafter::secrets::age_key_missing";
    /// Pass-through `CliError` codes.
    pub const HETZNER_API_ERROR: &str = "apprafter::provider::hetzner_api_error";
    pub const SERVER_TYPE_UNAVAILABLE: &str = "apprafter::provider::server_type_unavailable";
    pub const TOOL_NOT_FOUND: &str = "apprafter::env::tool_not_found";
    pub const CUE_NOT_FOUND: &str = "apprafter::env::cue_not_found";
    pub const STATE_CORRUPT: &str = "apprafter::state::corrupt";
    pub const BACKUP_JOB_ACTIVE: &str = "apprafter::backup::job_active";
    pub const TOKEN_REJECTED: &str = "apprafter::target::token_rejected";
    pub const PROVIDER_UNREACHABLE: &str = "apprafter::target::provider_unreachable";

    /// Every code above.
    pub const ALL: &[&str] = &[
        NO_ACTIVE_TARGET,
        TARGET_NOT_FOUND,
        CANCELLED,
        UNSAFE_OVERRIDE,
        TARGET_EXISTS,
        RENEW_TOKEN_UNCHANGED,
        INVALID_TARGET_NAME,
        SAME_TARGET_NAME,
        UNKNOWN_PROVIDER,
        INVALID_TOKEN,
        TOKEN_NOT_STORED,
        SSH_KEY_UNREADABLE,
        NOT_PROVISIONED,
        SERVER_MISSING,
        TARGET_PROVISIONED,
        PROVIDER_REQUEST_FAILED,
        TOOL_UNSUPPORTED,
        KUBE_FAILED,
        AGE_KEY_MISSING,
        HETZNER_API_ERROR,
        SERVER_TYPE_UNAVAILABLE,
        TOOL_NOT_FOUND,
        CUE_NOT_FOUND,
        STATE_CORRUPT,
        BACKUP_JOB_ACTIVE,
        TOKEN_REJECTED,
        PROVIDER_UNREACHABLE,
    ];
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
        let f = &mut ui.fields;
        let mut put = |k: &str, v: serde_json::Value| {
            f.insert(k.to_string(), v);
        };
        match e {
            CoreError::TargetNotFound { name, available } => {
                put("name", json!(name));
                put("available", json!(available));
            }
            CoreError::UnsafeOverride { var, .. } => put("var", json!(var)),
            CoreError::TargetExists { name }
            | CoreError::RenewTokenUnchanged { name }
            | CoreError::SameTargetName { name }
            | CoreError::TokenNotStored { name }
            | CoreError::NotProvisioned { name } => put("name", json!(name)),
            CoreError::InvalidTargetName { name, problem } => {
                put("name", json!(name));
                put("problem", json!(problem.as_str()));
            }
            CoreError::UnknownProvider {
                provider,
                supported,
            } => {
                put("provider", json!(provider));
                put("supported", json!(supported));
            }
            CoreError::InvalidToken { problem } => put("problem", json!(problem.as_str())),
            CoreError::SshKeyUnreadable { path, problem, .. } => {
                put("path", json!(path));
                put("problem", json!(problem.as_str()));
            }
            CoreError::ServerMissing { name, server_id } => {
                put("name", json!(name));
                put("serverId", json!(server_id));
            }
            CoreError::TargetProvisioned {
                name,
                server_id,
                server_name,
            } => {
                put("name", json!(name));
                put("serverId", json!(server_id));
                put("serverName", json!(server_name));
            }
            CoreError::ProviderRequestFailed {
                provider, endpoint, ..
            } => {
                put("provider", json!(provider));
                put("endpoint", json!(endpoint));
            }
            CoreError::ToolUnsupported { tool, path } => {
                put("tool", json!(tool));
                put("path", json!(path));
            }
            CoreError::Kube { kind, .. } => put("kind", json!(kind.as_str())),
            CoreError::AgeKeyMissing { path } => put("path", json!(path)),
            CoreError::Cli(inner) => project_cli(inner, &mut put),
            CoreError::NoActiveTarget | CoreError::Cancelled => {}
        }
        ui
    }
}

/// Fields of the pass-through `CliError`s the desktop acts on (D.3 overview §3.6.2). Keys are
/// camelCase (R12); `status` is a JSON number.
fn project_cli(e: &cli_core::CliError, put: &mut impl FnMut(&str, serde_json::Value)) {
    use cli_core::CliError as C;
    match e {
        C::Hetzner {
            endpoint,
            status,
            code,
            ..
        } => {
            put("status", json!(status));
            put("endpoint", json!(endpoint));
            put("apiCode", json!(code));
        }
        C::ProviderTokenRejected { provider, cause }
        | C::ProviderApiUnreachable { provider, cause } => {
            put("provider", json!(provider));
            let cause: &(dyn std::error::Error + 'static) = &**cause;
            if let Some(C::Hetzner { status, .. }) = cause.downcast_ref::<C>() {
                put("status", json!(status));
            }
        }
        C::ServerTypeUnavailable {
            requested,
            location,
            kind,
            context,
            ..
        } => {
            put("requested", json!(requested));
            put("location", json!(location));
            put("kind", json!(kind.as_str()));
            put("context", json!(context.as_str()));
        }
        C::ExternalToolNotFound {
            tool,
            needed_by,
            purpose,
            ..
        } => {
            put("tool", json!(tool));
            put("neededBy", json!(needed_by));
            put("purpose", json!(purpose));
        }
        C::CueNotFound => put("tool", json!("cue")),
        C::InvalidTargetConfig { path, .. } | C::InvalidState { path, .. } => {
            put("path", json!(path.display().to_string()))
        }
        _ => {}
    }
}

/// One sample of every `CoreError` variant, for the exhaustiveness tests here and in the CLI's
/// renderer (`platform-cli/src/render/core_error.rs`). Behind `test-support` too, so another
/// crate's tests can use the same list (only `platform-cli`'s dev-dependency enables it).
#[cfg(any(test, feature = "test-support"))]
pub mod samples {
    use super::CoreError;
    use crate::kube::KubeErrorKind;
    use crate::provider::TokenProblem;
    use crate::ssh::SshKeyProblem;
    use crate::target::NameProblem;

    /// How many variants `CoreError` declares: checked against `error.rs`'s own syntax tree
    /// (`the_samples_are_exactly_the_declared_variants`), as is [`one_of_each`].
    pub const VARIANTS: usize = 20;

    pub fn one_of_each() -> Vec<CoreError> {
        let s = |v: &str| v.to_string();
        vec![
            CoreError::NoActiveTarget,
            CoreError::TargetNotFound {
                name: s("ghost"),
                available: vec![s("prod")],
            },
            CoreError::Cancelled,
            CoreError::UnsafeOverride {
                var: "APPRAFTER_HCLOUD_BASE_URL",
                reason: s("not a loopback address"),
            },
            CoreError::Cli(cli_core::CliError::BackupJobActive { job: s("backup-1") }),
            CoreError::TargetExists { name: s("prod") },
            CoreError::RenewTokenUnchanged { name: s("prod") },
            CoreError::InvalidTargetName {
                name: s("-a"),
                problem: NameProblem::EdgeDash,
            },
            CoreError::SameTargetName { name: s("prod") },
            CoreError::UnknownProvider {
                provider: s("aws"),
                supported: vec![s("hetzner-cloud")],
            },
            CoreError::InvalidToken {
                problem: TokenProblem::WrongLength { got: 5 },
            },
            CoreError::TokenNotStored { name: s("prod") },
            CoreError::SshKeyUnreadable {
                path: s("/home/a/.ssh/id.pub"),
                problem: SshKeyProblem::Missing,
                error: None,
            },
            CoreError::NotProvisioned { name: s("prod") },
            CoreError::ServerMissing {
                name: s("prod"),
                server_id: 42,
            },
            CoreError::TargetProvisioned {
                name: s("prod"),
                server_id: 42,
                server_name: s("prod-node"),
            },
            CoreError::ProviderRequestFailed {
                provider: s("hetzner-cloud"),
                endpoint: s("GET /v1/locations"),
                cause: Box::new(CoreError::Cli(cli_core::CliError::Other(s(
                    "connection refused",
                )))),
            },
            CoreError::ToolUnsupported {
                tool: s("kubectl"),
                path: s(r"C:\tools\kubectl.cmd"),
            },
            CoreError::Kube {
                kind: KubeErrorKind::Unreachable,
                detail: s("no answer within 12 s"),
            },
            CoreError::AgeKeyMissing {
                path: s("/home/a/.config/apprafter/age.key"),
            },
        ]
    }

    /// No wildcard arm: adding a `CoreError` variant stops this from compiling until the variant
    /// is listed here. That only points at this module; what makes [`one_of_each`] sample the
    /// new variant (and so `codes::ALL` list its code) is the test that compares the samples
    /// with the variants `error.rs` declares.
    pub fn every_variant_is_sampled(e: &CoreError) {
        match e {
            CoreError::NoActiveTarget
            | CoreError::TargetNotFound { .. }
            | CoreError::Cancelled
            | CoreError::UnsafeOverride { .. }
            | CoreError::Cli(_)
            | CoreError::TargetExists { .. }
            | CoreError::RenewTokenUnchanged { .. }
            | CoreError::InvalidTargetName { .. }
            | CoreError::SameTargetName { .. }
            | CoreError::UnknownProvider { .. }
            | CoreError::InvalidToken { .. }
            | CoreError::TokenNotStored { .. }
            | CoreError::SshKeyUnreadable { .. }
            | CoreError::NotProvisioned { .. }
            | CoreError::ServerMissing { .. }
            | CoreError::TargetProvisioned { .. }
            | CoreError::ProviderRequestFailed { .. }
            | CoreError::ToolUnsupported { .. }
            | CoreError::Kube { .. }
            | CoreError::AgeKeyMissing { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::target::NameProblem;

    #[test]
    fn a_hetzner_error_projects_its_status_as_a_number() {
        for status in [401u16, 403] {
            let e = CoreError::from(cli_core::CliError::Hetzner {
                endpoint: "GET http://x/v1/server_types".into(),
                status,
                code: "unauthorized".into(),
                message: "m".into(),
            });
            let ui = UiError::from(&e);
            assert_eq!(ui.code.as_deref(), Some(codes::HETZNER_API_ERROR));
            assert_eq!(ui.fields["status"], json!(status));
            assert!(
                ui.fields["status"].is_u64(),
                "a JSON number, which errors.ts compares with ==="
            );
            assert_eq!(ui.fields["apiCode"], json!("unauthorized"));
            assert_eq!(ui.fields["endpoint"], json!("GET http://x/v1/server_types"));
        }
    }

    #[test]
    fn a_rejected_token_carries_the_provider_and_the_inner_status() {
        let e = CoreError::from(cli_core::CliError::ProviderTokenRejected {
            provider: "hetzner-cloud".into(),
            cause: Box::new(cli_core::CliError::Hetzner {
                endpoint: "e".into(),
                status: 401,
                code: "c".into(),
                message: "m".into(),
            }),
        });
        let ui = UiError::from(&e);
        assert_eq!(
            (ui.fields["provider"].clone(), ui.fields["status"].clone()),
            (json!("hetzner-cloud"), json!(401))
        );
    }

    /// The desktop learns which command refused the server type (overview §3.6.2).
    #[test]
    fn a_server_type_refusal_projects_its_context() {
        let ui = |context| {
            UiError::from(&CoreError::from(
                cli_core::CliError::ServerTypeUnavailable {
                    requested: "cx99".into(),
                    location: "nbg1".into(),
                    kind: cli_core::UnavailableKind::Unknown,
                    alternatives: String::new(),
                    context,
                },
            ))
            .fields["context"]
                .clone()
        };
        assert_eq!(
            ui(cli_core::SkuCheckFor::TargetMachine { name: "p".into() }),
            json!("target_machine")
        );
        assert_eq!(ui(cli_core::SkuCheckFor::Provision), json!("provision"));
    }

    #[test]
    fn pass_through_errors_project_their_fields() {
        let sku = UiError::from(&CoreError::from(
            cli_core::CliError::ServerTypeUnavailable {
                requested: "cx99".into(),
                location: "nbg1".into(),
                kind: cli_core::UnavailableKind::Retired,
                alternatives: String::new(),
                context: cli_core::SkuCheckFor::TargetAdd { name: "p".into() },
            },
        ));
        assert_eq!(
            (
                sku.fields["requested"].clone(),
                sku.fields["location"].clone(),
                sku.fields["kind"].clone()
            ),
            (json!("cx99"), json!("nbg1"), json!("retired"))
        );
        assert_eq!(sku.fields["context"], json!("target_add"));
        let tool = UiError::from(&CoreError::from(cli_core::CliError::ExternalToolNotFound {
            tool: "kubectl".into(),
            needed_by: "apprafter doctor".into(),
            purpose: "p".into(),
            install: "i".into(),
        }));
        assert_eq!(
            (
                tool.fields["tool"].clone(),
                tool.fields["neededBy"].clone(),
                tool.fields["purpose"].clone()
            ),
            (json!("kubectl"), json!("apprafter doctor"), json!("p"))
        );
        let config = UiError::from(&CoreError::from(cli_core::CliError::InvalidTargetConfig {
            path: "/s/targets/prod/config.yaml".into(),
            message: "m".into(),
        }));
        assert_eq!(config.fields["path"], json!("/s/targets/prod/config.yaml"));
        assert_eq!(
            UiError::from(&CoreError::from(cli_core::CliError::CueNotFound)).fields["tool"],
            json!("cue")
        );
        let state = UiError::from(&CoreError::from(cli_core::CliError::InvalidState {
            path: "/s/state.json".into(),
            message: "m".into(),
        }));
        assert_eq!(state.fields["path"], json!("/s/state.json"));
    }

    #[test]
    fn the_new_refusals_project_their_fields() {
        let ui = UiError::from(&CoreError::TargetProvisioned {
            name: "prod".into(),
            server_id: 42,
            server_name: "prod-node".into(),
        });
        assert_eq!(
            (
                ui.fields["name"].clone(),
                ui.fields["serverId"].clone(),
                ui.fields["serverName"].clone()
            ),
            (json!("prod"), json!(42), json!("prod-node"))
        );
        let bad = UiError::from(&CoreError::InvalidTargetName {
            name: "-a".into(),
            problem: NameProblem::EdgeDash,
        });
        assert_eq!(
            bad.message,
            "target name `-a` must not start or end with `-`"
        );
        assert_eq!(bad.fields["problem"], json!("edge_dash"));
    }

    /// A sample's variant, as its `Debug` output names it first (`TargetNotFound { .. }`).
    fn variant_name(e: &CoreError) -> String {
        format!("{e:?}")
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// Review finding 7: the variants `CoreError` declares, read from this file's syntax tree,
    /// are exactly the samples' (and `VARIANTS` counts them). `every_variant_is_sampled` only
    /// forces a match arm; this is what forces a new variant into `one_of_each`, and through
    /// `every_variant_has_a_listed_code` its code into `codes::ALL`.
    #[test]
    fn the_samples_are_exactly_the_declared_variants() {
        let file = syn::parse_file(include_str!("error.rs")).expect("error.rs parses");
        let declared: std::collections::BTreeSet<String> = file
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Enum(e) if e.ident == "CoreError" => {
                    Some(e.variants.iter().map(|v| v.ident.to_string()).collect())
                }
                _ => None,
            })
            .expect("error.rs declares CoreError");
        let sampled: std::collections::BTreeSet<String> =
            samples::one_of_each().iter().map(variant_name).collect();
        assert_eq!(sampled, declared, "one_of_each must sample every variant");
        assert_eq!(declared.len(), samples::VARIANTS);
    }

    /// Review finding 7: every variant projects exactly the fields the desktop reads (D.3
    /// overview §3.6.1), no more and no fewer.
    #[test]
    fn every_sample_projects_exactly_its_fields() {
        use crate::kube::KubeErrorKind;
        use crate::provider::TokenProblem;
        use crate::ssh::SshKeyProblem;
        let expected: BTreeMap<&str, serde_json::Value> = [
            ("NoActiveTarget", json!({})),
            (
                "TargetNotFound",
                json!({"name": "ghost", "available": ["prod"]}),
            ),
            ("Cancelled", json!({})),
            (
                "UnsafeOverride",
                json!({"var": "APPRAFTER_HCLOUD_BASE_URL"}),
            ),
            ("Cli", json!({})),
            ("TargetExists", json!({"name": "prod"})),
            ("RenewTokenUnchanged", json!({"name": "prod"})),
            (
                "InvalidTargetName",
                json!({"name": "-a", "problem": NameProblem::EdgeDash.as_str()}),
            ),
            ("SameTargetName", json!({"name": "prod"})),
            (
                "UnknownProvider",
                json!({"provider": "aws", "supported": ["hetzner-cloud"]}),
            ),
            (
                "InvalidToken",
                json!({"problem": TokenProblem::WrongLength { got: 5 }.as_str()}),
            ),
            ("TokenNotStored", json!({"name": "prod"})),
            (
                "SshKeyUnreadable",
                json!({"path": "/home/a/.ssh/id.pub", "problem": SshKeyProblem::Missing.as_str()}),
            ),
            ("NotProvisioned", json!({"name": "prod"})),
            ("ServerMissing", json!({"name": "prod", "serverId": 42})),
            (
                "TargetProvisioned",
                json!({"name": "prod", "serverId": 42, "serverName": "prod-node"}),
            ),
            (
                "ProviderRequestFailed",
                json!({"provider": "hetzner-cloud", "endpoint": "GET /v1/locations"}),
            ),
            (
                "ToolUnsupported",
                json!({"tool": "kubectl", "path": r"C:\tools\kubectl.cmd"}),
            ),
            ("Kube", json!({"kind": KubeErrorKind::Unreachable.as_str()})),
            (
                "AgeKeyMissing",
                json!({"path": "/home/a/.config/apprafter/age.key"}),
            ),
        ]
        .into_iter()
        .collect();
        assert_eq!(expected.len(), samples::VARIANTS);
        for e in samples::one_of_each() {
            let name = variant_name(&e);
            assert_eq!(
                json!(UiError::from(&e).fields),
                expected[name.as_str()],
                "{name}"
            );
        }
    }

    #[test]
    fn every_variant_has_a_listed_code() {
        use samples::{every_variant_is_sampled, one_of_each, VARIANTS};
        let samples = one_of_each();
        let kinds: std::collections::HashSet<_> =
            samples.iter().map(std::mem::discriminant).collect();
        assert_eq!(kinds.len(), VARIANTS);
        for e in &samples {
            every_variant_is_sampled(e);
            let code = e.code().expect("every variant has a code").to_string();
            assert!(
                codes::ALL.contains(&code.as_str()),
                "{code} missing from codes::ALL"
            );
        }
        let mut seen = std::collections::HashSet::new();
        assert!(codes::ALL.iter().all(|c| seen.insert(c)), "duplicate code");
    }

    #[test]
    fn pass_through_codes_are_the_cli_errors_own() {
        use cli_core::CliError as C;
        let hetzner = || C::Hetzner {
            endpoint: "e".into(),
            status: 500,
            code: "c".into(),
            message: "m".into(),
        };
        for (e, code) in [
            (hetzner(), codes::HETZNER_API_ERROR),
            (C::CueNotFound, codes::CUE_NOT_FOUND),
            (
                C::InvalidState {
                    path: "/x".into(),
                    message: "m".into(),
                },
                codes::STATE_CORRUPT,
            ),
            (
                C::BackupJobActive { job: "j".into() },
                codes::BACKUP_JOB_ACTIVE,
            ),
            (
                C::ServerTypeUnavailable {
                    requested: "cx99".into(),
                    location: "nbg1".into(),
                    kind: cli_core::UnavailableKind::Unknown,
                    alternatives: String::new(),
                    context: cli_core::SkuCheckFor::Provision,
                },
                codes::SERVER_TYPE_UNAVAILABLE,
            ),
            (
                C::ExternalToolNotFound {
                    tool: "t".into(),
                    needed_by: "n".into(),
                    purpose: "p".into(),
                    install: "i".into(),
                },
                codes::TOOL_NOT_FOUND,
            ),
            (
                C::ProviderTokenRejected {
                    provider: "hetzner-cloud".into(),
                    cause: Box::new(hetzner()),
                },
                codes::TOKEN_REJECTED,
            ),
            (
                C::ProviderApiUnreachable {
                    provider: "hetzner-cloud".into(),
                    cause: Box::new(hetzner()),
                },
                codes::PROVIDER_UNREACHABLE,
            ),
        ] {
            assert_eq!(miette::Diagnostic::code(&e).unwrap().to_string(), code);
        }
    }

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

    #[cfg(feature = "ts")]
    #[test]
    fn ui_error_has_a_typescript_declaration() {
        use ts_rs::TS;
        let decl = UiError::decl(&ts_rs::Config::new().with_large_int("number"));
        for field in [
            "code: string | null",
            "message: string",
            "help: string | null",
            "causes: Array<string>",
        ] {
            assert!(decl.contains(field), "{field} missing from {decl}");
        }
        assert!(decl.contains("fields:"), "{decl}");
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
