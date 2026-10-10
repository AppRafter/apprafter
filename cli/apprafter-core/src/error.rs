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
//! when it renders, the desktop turns codes into actions. Nor does [`UiError`]'s
//! `help`: a pass-through `CliError`'s own help is the CLI's, so the projection
//! carries a source-neutral one in its place (`neutral_help`), or none.
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

    /// A renewal would store the token the target already has, and changes no SSH key.
    #[error("the new token for `{name}` is the one already stored")]
    #[diagnostic(code(apprafter::target::renew_token_unchanged))]
    RenewTokenUnchanged { name: String },

    /// A renewal with no token would change nothing: no SSH key, or the one already stored.
    #[error("renewing `{name}` would change nothing: no new token and no new SSH key")]
    #[diagnostic(code(apprafter::target::renew_nothing_to_change))]
    RenewNothingToChange { name: String },

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

    /// A provider read failed for a reason other than an API status or no answer — an answer
    /// that does not parse, a request the HTTP client refused to send; `CliError::Hetzner` and
    /// `CliError::ProviderApiUnreachable` pass through unwrapped (`provider::read_error`), so a
    /// status projects and a dead API has one code on every path (WI-453). The cause is a plain
    /// `#[source]`: miette-derive borrows a `diagnostic_source` as `&dyn Diagnostic` through
    /// `Borrow`, which `Box<CoreError>` does not provide.
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
    pub const RENEW_NOTHING_TO_CHANGE: &str = "apprafter::target::renew_nothing_to_change";
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
    /// A key file add and renew refuse (`ssh::check_readable`, GOTCHA-149).
    pub const SSH_KEY_NOT_PUBLIC: &str = "apprafter::target::ssh_key_not_public";
    /// A target store file that cannot be read; `fields.target` names the target whose own file
    /// it is (the desktop then offers to remove it, WI-458).
    pub const INVALID_TARGET_CONFIG: &str = "apprafter::target::invalid_config";
    /// A filesystem error. On one of a target's own files `fields.target` names the target and
    /// `fields.path` the file (the desktop then offers to remove it, WI-458 review #6).
    pub const IO_ERROR: &str = "apprafter::io::error";

    /// Every code above.
    pub const ALL: &[&str] = &[
        NO_ACTIVE_TARGET,
        TARGET_NOT_FOUND,
        CANCELLED,
        UNSAFE_OVERRIDE,
        TARGET_EXISTS,
        RENEW_TOKEN_UNCHANGED,
        RENEW_NOTHING_TO_CHANGE,
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
        SSH_KEY_NOT_PUBLIC,
        INVALID_TARGET_CONFIG,
        IO_ERROR,
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
        // A pass-through `CliError`'s own help names CLI commands and flags, which the GUI does
        // not have: the projection carries a source-neutral help instead (decision 4: the CLI's
        // renderer owns the CLI's help, and renders it from the diagnostic, never from here).
        // The core's own variants declare no help.
        if let CoreError::Cli(inner) = e {
            ui.help = neutral_help(inner);
        }
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
            | CoreError::RenewNothingToChange { name }
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

/// The help a pass-through `CliError` projects: the diagnosis its CLI help gives, without the
/// commands and flags (`apprafter target add … --renew`, `--no-ping`), for the errors the
/// desktop's flows reach — target, provider, token, catalogue, tool, doctor, whoami — and the
/// codes it acts on. `None` where there is no neutral text: the message and the causes still
/// show. Exhaustive, so a new `CliError` variant is decided here.
fn neutral_help(e: &cli_core::CliError) -> Option<String> {
    use cli_core::CliError as C;
    use cli_core::SkuCheckFor;
    Some(match e {
        // Each status with only what can cause it (D.3d review #10), and no target assumed: a
        // wizard's token belongs to none yet.
        C::Hetzner { .. } => "The Hetzner Cloud API refused the request:\n\
             • 401 unauthorized — the token is wrong, or it was revoked or rotated: use a \
               current one.\n\
             • 403 forbidden — the token lacks Read & Write (it is Read-only), or the project \
               forbids the call, for example at one of its limits.\n\
             • 429 rate limit — too many requests: wait, then try again.\n\
             • 5xx — an outage at the provider; check https://status.hetzner.com/."
            .into(),
        // The check is a read (GET /v1/locations): a Read-only token passes it, and a token
        // with a trailing newline never reaches it (the format check refuses that first).
        C::ProviderTokenRejected { .. } => format!(
            "The provider did not accept this token (401 unauthorized): it was mistyped, or it \
             was revoked or rotated, or its project was deleted. A token is shown only once, \
             when it is created: paste it again from where you saved it, or create a new one \
             with Read & Write permission in {}.",
            cli_core::target::HETZNER_API_TOKENS_PAGE
        ),
        // Any request that got no answer (WI-453), not only the credential check.
        C::ProviderApiUnreachable { .. } => {
            "The request could not complete because the provider's API was unreachable. This \
             is not a credentials problem: the token may still be valid once the API \
             recovers.\n\
             • Check the provider's status page (https://status.hetzner.com/ for \
               hetzner-cloud).\n\
             • Behind a VPN or a corporate proxy, make sure https://api.hetzner.cloud/ is \
               reachable."
                .into()
        }
        C::ServerTypeUnavailable {
            requested,
            location,
            kind,
            context,
            ..
        } => match context {
            SkuCheckFor::Provision => kind.why(requested, location),
            SkuCheckFor::TargetAdd { .. } | SkuCheckFor::TargetMachine { .. } => {
                format!("{} Nothing was saved.", kind.why(requested, location))
            }
        },
        C::ExternalToolNotFound { install, .. } => format!(
            "{install}\n\nNothing was sent anywhere and no credential was used: this check runs \
             before any work starts, so trying again after installing is safe."
        ),
        C::CueNotFound => {
            "AppRafter runs `cue` to read manifests and could not find it on PATH: install CUE."
                .into()
        }
        C::InvalidState { .. } => {
            "The state file named above failed to parse; an earlier provision or import wrote \
             it. Rebuilding it from the provider's resources labelled `apprafter=true` moves the \
             unreadable file aside rather than deleting it."
                .into()
        }
        // The fix depends on where the key came from (D.3d verification): a file the reader
        // chose is chosen again; the environment's key and a manifest's outrank the target's,
        // so each is fixed where it is.
        C::SshKeyNotPublic {
            private_key, from, ..
        } => {
            use cli_core::ssh_key::KeySource;
            let what = if *private_key {
                "AppRafter sends a target's SSH key to the provider, so it never takes a private \
                 key."
            } else {
                "AppRafter sends a target's SSH key to the provider as an OpenSSH public key: \
                 one line `<type> <base64> [comment]`, of type ssh-ed25519, ssh-rsa, an \
                 ecdsa-sha2 curve, or a security-key (sk-) type."
            };
            let fix = match from {
                KeySource::NewTargetFile | KeySource::TargetFile => {
                    "Choose the public key, the `.pub` file next to a private key. Nothing was \
                     saved or sent."
                        .to_string()
                }
                KeySource::Env => "`APPRAFTER_SSH_PUBLIC_KEY` in AppRafter's environment holds \
                     the key's text, and it outranks the target's key: set it to the public key \
                     line (`ssh-keygen -y -f <private key>` prints it), or unset it so that the \
                     target's key is used. Nothing was sent."
                    .to_string(),
                KeySource::Manifest { index } => format!(
                    "The Infrastructure manifest's `sshKeys[{index}].public_key` holds the key's \
                     text: replace it with the public key line (`ssh-keygen -y -f <private key>` \
                     prints it). The manifest's keys outrank the environment's and the target's. \
                     Nothing was sent."
                ),
            };
            format!("{what} {fix}")
        }
        // A target's own file: or remove the target and add it again, which works on a target
        // that cannot be read (WI-458), and deletes its local state too (review #0/#2). No
        // removal repairs the store's own config.yaml (D.3d review #9), so its help offers none.
        C::InvalidTargetConfig { path, target, .. } => {
            let fix = format!(
                "{} could not be read as a target configuration: it was edited by hand or \
                 written by an incompatible version. Fix it by hand (it is a small YAML file), \
                 or restore it from a backup.",
                path.display()
            );
            match target {
                Some(name) => format!(
                    "{fix} Otherwise remove target `{name}` and add it again, its token \
                     included. The removal also deletes the target's local state: the record \
                     of its server, the cached kubeconfig and the Argo CD password. If a server \
                     is recorded, fix or restore the file first. After the target is added \
                     again, the record of its server can be rebuilt from the provider."
                ),
                None => fix,
            }
        }
        C::Io(_) => "A filesystem or network error. The OS message above usually names the \
             failing path or socket: a missing directory, wrong permissions, a full disk, or a \
             closed socket."
            .into(),
        // The OS message names no file here: the help does (WI-458 review #6).
        C::TargetFileIo { path, target, .. } => format!(
            "{} is a file of target `{target}`, and it could not be read: most often its \
             permissions, or its folder's, do not let this user read it.",
            path.display()
        ),
        C::Json(_) => "A JSON file could not be read or written, most often a target's state \
             file. If it was edited by hand or copied across versions, rebuilding the state \
             from the provider replaces it."
            .into(),
        C::BackupJobActive { job } => format!(
            "{job} has not finished; start the next run once it has. Two runs at once do not \
             both finish: two backups need the same helper pods, and a backup and a check each \
             fail on the other's repository lock."
        ),
        // CLI input policy and CLI-only commands (their help is about the command line), and
        // errors no desktop flow reaches yet, whose help names CLI commands. `TargetNotFound`
        // and `NoActiveTarget` arrive as the core's own variants (`From`).
        C::CueExport { .. }
        | C::Restic { .. }
        | C::BackupRepoProbe { .. }
        | C::BackupRunnerUnschedulable { .. }
        | C::Kubectl { .. }
        | C::ServerTypeNotSelected
        | C::TargetNotFound { .. }
        | C::NoActiveTarget
        | C::Yaml(_)
        | C::CompletionInstall(_)
        | C::ConfirmationRequired { .. }
        | C::UsageRefused { .. }
        | C::Other(_) => return None,
    })
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
        C::InvalidTargetConfig { path, target, .. } => {
            put("path", json!(path.display().to_string()));
            if let Some(name) = target {
                put("target", json!(name));
            }
        }
        C::TargetFileIo { path, target, .. } => {
            put("path", json!(path.display().to_string()));
            put("target", json!(target));
        }
        C::InvalidState { path, .. } => put("path", json!(path.display().to_string())),
        C::SshKeyNotPublic {
            origin,
            private_key,
            ..
        } => {
            put("origin", json!(origin));
            put("privateKey", json!(private_key));
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
    pub const VARIANTS: usize = 21;

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
            CoreError::RenewNothingToChange { name: s("prod") },
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
                // No answer is `ProviderApiUnreachable` (WI-453): what is left is an answer
                // that does not parse.
                cause: Box::new(CoreError::Cli(cli_core::CliError::Other(s(
                    "parse list_locations response: missing field `locations`",
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

    /// The errors whose real projections the desktop exports as `fixtures/ui-errors.json`
    /// (desktop/ipc/tests/export.rs), by fixture name: the frontend's rules are tested on these,
    /// never on hand-made objects (bug 11). `tokenRejected` is what `provider::ping` makes of a
    /// 401 (a verify, a renew), the Hetzner error as its cause.
    pub fn ui_fixtures() -> Vec<(&'static str, CoreError)> {
        use cli_core::{CliError, SkuCheckFor, UnavailableKind};
        let hetzner = |status: u16, code: &str, message: &str| CliError::Hetzner {
            endpoint: "GET /v1/locations".into(),
            status,
            code: code.into(),
            message: message.into(),
        };
        vec![
            (
                "hetzner401",
                hetzner(401, "unauthorized", "unable to authenticate").into(),
            ),
            (
                "hetzner403",
                hetzner(403, "forbidden", "insufficient permissions").into(),
            ),
            (
                "tokenRejected",
                CliError::ProviderTokenRejected {
                    provider: "hetzner-cloud".into(),
                    cause: Box::new(hetzner(401, "unauthorized", "unable to authenticate")),
                }
                .into(),
            ),
            (
                "targetExists",
                CoreError::TargetExists {
                    name: "prod".into(),
                },
            ),
            (
                "serverTypeUnavailable",
                CliError::ServerTypeUnavailable {
                    requested: "cx22".into(),
                    location: "nbg1".into(),
                    kind: UnavailableKind::Retired,
                    alternatives: "cpx22".into(),
                    context: SkuCheckFor::TargetAdd {
                        name: "prod".into(),
                    },
                }
                .into(),
            ),
            (
                "toolNotFound",
                CliError::ExternalToolNotFound {
                    tool: "kubectl".into(),
                    needed_by: "doctor".into(),
                    purpose: "talks to the cluster".into(),
                    install: "Install kubectl.".into(),
                }
                .into(),
            ),
        ]
    }

    /// One `CliError` of every variant that can be built here (not `Yaml`: `serde_yaml` is not
    /// a dependency of this crate, and its projection has no help), with every `(kind,
    /// context)` of a server-type refusal and every tool's real install lines, for the guard
    /// that no projected help names a CLI command or flag.
    pub fn cli_errors() -> Vec<cli_core::CliError> {
        use cli_core::ssh_key::KeySource;
        use cli_core::{CliError as C, SkuCheckFor, UnavailableKind};
        let s = |v: &str| v.to_string();
        let hetzner = || C::Hetzner {
            endpoint: s("GET /v1/locations"),
            status: 401,
            code: s("unauthorized"),
            message: s("m"),
        };
        let mut out = vec![
            C::CueNotFound,
            C::CueExport {
                exit: 1,
                stderr: s("e"),
            },
            C::Restic {
                verb: s("backup"),
                exit: Some(1),
                stderr: s("e"),
                hint: s("h"),
            },
            C::BackupRepoProbe {
                repo: s("r"),
                cat_stderr: s("c"),
                init_stderr: s("i"),
                hint: s("h"),
            },
            C::BackupRunnerUnschedulable {
                job: s("j"),
                what: s("w"),
                help: s("h"),
            },
            C::BackupJobActive { job: s("backup-1") },
            C::Kubectl {
                verb: s("get"),
                resource: s("pods"),
                exit: Some(1),
                stderr: s("e"),
                hint: s("h"),
            },
            hetzner(),
            C::ServerTypeNotSelected,
            C::InvalidState {
                path: "/s/state.json".into(),
                message: s("m"),
            },
            C::InvalidTargetConfig {
                path: "/s/targets/prod/config.yaml".into(),
                message: s("m"),
                target: Some(s("prod")),
            },
            C::TargetNotFound {
                name: s("ghost"),
                available: s("prod"),
            },
            C::NoActiveTarget,
            C::ProviderTokenRejected {
                provider: s("hetzner-cloud"),
                cause: Box::new(hetzner()),
            },
            C::ProviderApiUnreachable {
                provider: s("hetzner-cloud"),
                cause: Box::new(hetzner()),
            },
            C::Io(std::io::Error::other("denied")),
            C::TargetFileIo {
                path: "/s/targets/prod/credentials.yaml".into(),
                target: s("prod"),
                source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            },
            C::Json(serde_json::from_str::<u8>("x").unwrap_err()),
            C::CompletionInstall(s("no destination")),
            C::ConfirmationRequired {
                action: s("removing target `prod`"),
            },
            C::UsageRefused {
                message: s("m"),
                help: s("h"),
            },
            C::SshKeyNotPublic {
                origin: s("SSH key `/home/a/.ssh/id_ed25519`"),
                private_key: true,
                from: KeySource::NewTargetFile,
            },
            C::SshKeyNotPublic {
                origin: s("SSH key `/home/a/notes.pub`"),
                private_key: false,
                from: KeySource::TargetFile,
            },
            C::SshKeyNotPublic {
                origin: s("`APPRAFTER_SSH_PUBLIC_KEY`"),
                private_key: true,
                from: KeySource::Env,
            },
            C::SshKeyNotPublic {
                origin: s("the manifest's `sshKeys[1]`"),
                private_key: true,
                from: KeySource::Manifest { index: 1 },
            },
            C::Other(s("o")),
        ];
        for kind in [
            UnavailableKind::Unknown,
            UnavailableKind::NotOfferedInRegion,
            UnavailableKind::Retired,
            UnavailableKind::OutOfCapacity,
        ] {
            for context in [
                SkuCheckFor::Provision,
                SkuCheckFor::TargetAdd { name: s("prod") },
                SkuCheckFor::TargetMachine { name: s("prod") },
            ] {
                out.push(C::ServerTypeUnavailable {
                    requested: s("cx22"),
                    location: s("nbg1"),
                    kind,
                    alternatives: s("cpx22"),
                    context,
                });
            }
        }
        out.extend(
            cli_core::tools::ALL
                .iter()
                .map(|t| C::ExternalToolNotFound {
                    tool: s(t.name),
                    needed_by: s("doctor"),
                    purpose: s(t.purpose),
                    install: s(t.install),
                }),
        );
        out
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
            | CoreError::RenewNothingToChange { .. }
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
            target: Some("prod".into()),
        }));
        assert_eq!(config.fields["path"], json!("/s/targets/prod/config.yaml"));
        // WI-458: whose file it is, so the desktop offers the removal only for a target's own.
        assert_eq!(config.fields["target"], json!("prod"));
        let store = UiError::from(&CoreError::from(cli_core::CliError::InvalidTargetConfig {
            path: "/s/config.yaml".into(),
            message: "m".into(),
            target: None,
        }));
        assert_eq!(store.fields.get("target"), None);
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
            ("RenewNothingToChange", json!({"name": "prod"})),
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
            (
                C::SshKeyNotPublic {
                    origin: "SSH key `/k`".into(),
                    private_key: true,
                    from: cli_core::ssh_key::KeySource::TargetFile,
                },
                codes::SSH_KEY_NOT_PUBLIC,
            ),
            (
                C::InvalidTargetConfig {
                    path: "/s/targets/prod/config.yaml".into(),
                    message: "m".into(),
                    target: Some("prod".into()),
                },
                codes::INVALID_TARGET_CONFIG,
            ),
            (C::Io(std::io::Error::other("x")), codes::IO_ERROR),
            (
                C::TargetFileIo {
                    path: "/s/targets/prod/config.yaml".into(),
                    target: "prod".into(),
                    source: std::io::Error::other("x"),
                },
                codes::IO_ERROR,
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
    fn cli_errors_pass_through_with_their_code_and_a_neutral_help() {
        let inner = cli_core::CliError::BackupJobActive {
            job: "nightly-1".into(),
        };
        let expected_message = inner.to_string();
        let cli_help = miette::Diagnostic::help(&inner).unwrap().to_string();
        let e = CoreError::from(inner);
        let ui = UiError::from(&e);
        assert_eq!(ui.code.as_deref(), Some("apprafter::backup::job_active"));
        assert_eq!(ui.message, expected_message);
        let help = ui.help.expect("a neutral help");
        assert_ne!(help, cli_help, "the CLI's help stays the CLI's");
        assert!(help.contains("nightly-1"), "{help}");
    }

    /// What a help the GUI shows may not say: a CLI command (`apprafter …`) or a flag (a
    /// backticked `--…`, alone or inside a command) — the GUI has neither. The first offending
    /// text, or `None`.
    fn cli_wording(help: &str) -> Option<String> {
        if help.contains("apprafter ") {
            return Some("apprafter ".into());
        }
        if help.contains("`--") {
            return Some("`--".into());
        }
        help.split('`')
            .skip(1)
            .step_by(2)
            .find(|span| span.split_whitespace().any(|w| w.starts_with("--")))
            .map(str::to_string)
    }

    #[test]
    fn the_cli_wording_check_finds_commands_and_flags_and_nothing_else() {
        for cli in [
            "run `apprafter target add <name> --renew --token <new>` to refresh it",
            "Pass `--no-ping` to skip the check",
            "Re-run `apprafter doctor` to confirm",
            "pass `x --flag`",
        ] {
            assert!(cli_wording(cli).is_some(), "{cli}");
        }
        for neutral in [
            "It must say `Read & Write` next to the project.",
            "resources labelled `apprafter=true`",
            "AppRafter runs `cue` to read manifests",
            "xcode-select --install",
        ] {
            assert_eq!(cli_wording(neutral), None, "{neutral}");
        }
    }

    /// WI-452 (decision 4): no help the projection carries names a CLI command or flag — over
    /// every `CoreError` variant, every desktop fixture, and every `CliError` the core passes
    /// through. The codes the desktop's D.3 flows reach keep a help of their own.
    #[test]
    fn no_projected_help_names_a_cli_command_or_flag() {
        let mut cases: Vec<(String, CoreError)> = samples::one_of_each()
            .into_iter()
            .map(|e| (variant_name(&e), e))
            .collect();
        cases.extend(
            samples::ui_fixtures()
                .into_iter()
                .map(|(name, e)| (name.to_string(), e)),
        );
        cases.extend(samples::cli_errors().into_iter().map(|e| {
            let code = miette::Diagnostic::code(&e).map(|c| c.to_string());
            (format!("Cli {code:?}"), CoreError::Cli(e))
        }));
        let mut helped = std::collections::BTreeSet::new();
        for (name, e) in &cases {
            let ui = UiError::from(e);
            if let Some(help) = &ui.help {
                assert_eq!(cli_wording(help), None, "{name}: {help}");
                helped.insert(ui.code.clone().unwrap_or_default());
            }
        }
        for code in [
            codes::HETZNER_API_ERROR,
            codes::TOKEN_REJECTED,
            codes::PROVIDER_UNREACHABLE,
            codes::SERVER_TYPE_UNAVAILABLE,
            codes::TOOL_NOT_FOUND,
            codes::CUE_NOT_FOUND,
            codes::STATE_CORRUPT,
            codes::BACKUP_JOB_ACTIVE,
            codes::SSH_KEY_NOT_PUBLIC,
            codes::INVALID_TARGET_CONFIG,
            codes::IO_ERROR,
        ] {
            assert!(helped.contains(code), "{code} has no neutral help");
        }
    }

    /// The neutral help keeps the CLI help's diagnosis: why a token is rejected, what each
    /// Hetzner status means, why a server type cannot be ordered and that nothing was saved.
    #[test]
    fn a_neutral_help_keeps_the_diagnosis() {
        let help = |e: cli_core::CliError| UiError::from(&CoreError::Cli(e)).help.unwrap();
        let rejected = help(cli_core::CliError::ProviderTokenRejected {
            provider: "hetzner-cloud".into(),
            cause: Box::new(cli_core::CliError::Other("401".into())),
        });
        for why in ["mistyped", "rotated", "revoked", "create a new one"] {
            assert!(rejected.contains(why), "{why}: {rejected}");
        }
        // D.3d review #10: nothing that cannot produce a 401 on the read-only ping — a token's
        // scope (a Read-only token passes it), a trailing newline (the format check refuses it
        // first) — and no step that cannot be done (the console shows a token only once).
        for not_why in ["scope", "newline", "Copy the token again"] {
            assert!(!rejected.contains(not_why), "{not_why}: {rejected}");
        }
        assert!(rejected.contains("only once"), "{rejected}");
        // Where a new token comes from, in the one shared wording (WI-454).
        assert!(
            rejected.contains(cli_core::target::HETZNER_API_TOKENS_PAGE),
            "{rejected}"
        );
        // WI-453: every core read that gets no answer is `provider_unreachable` now, a
        // catalogue's as much as the token check's, so its help may not say it was the check.
        let unreachable = help(cli_core::CliError::ProviderApiUnreachable {
            provider: "hetzner-cloud".into(),
            cause: Box::new(cli_core::TransportFailure {
                endpoint: "https://api.hetzner.cloud/v1/server_types".into(),
                detail: "Dns Failed".into(),
            }),
        });
        assert!(
            !unreachable.contains("credential check could not"),
            "{unreachable}"
        );
        for why in [
            "unreachable",
            "not a credentials problem",
            "status.hetzner.com",
        ] {
            assert!(unreachable.contains(why), "{why}: {unreachable}");
        }
        let hetzner = help(cli_core::CliError::Hetzner {
            endpoint: "e".into(),
            status: 401,
            code: "c".into(),
            message: "m".into(),
        });
        for status in ["401", "403", "429", "5xx"] {
            assert!(hetzner.contains(status), "{status}: {hetzner}");
        }
        // 403 is the token's permission (Read-only) or the project's; no target to renew is
        // assumed (a wizard's token has none yet), and a quota is no 429 matter.
        let line = |status: &str| {
            hetzner
                .lines()
                .find(|l| l.contains(status))
                .unwrap_or_else(|| panic!("{status}: {hetzner}"))
                .to_string()
        };
        assert!(line("403").contains("Read & Write"), "{hetzner}");
        assert!(!hetzner.contains("target's token"), "{hetzner}");
        assert!(!line("429").contains("quota"), "{hetzner}");
        let sku = help(cli_core::CliError::ServerTypeUnavailable {
            requested: "cx22".into(),
            location: "nbg1".into(),
            kind: cli_core::UnavailableKind::Retired,
            alternatives: String::new(),
            context: cli_core::SkuCheckFor::TargetMachine {
                name: "prod".into(),
            },
        });
        assert_eq!(
            sku,
            "Hetzner no longer sells `cx22`; pick another type. Nothing was saved."
        );
    }

    /// D.3d verification (finding C), the GUI's half: the fix follows where the key came from.
    /// A file is chosen again; the environment's key and a manifest's outrank the target's, so
    /// each is fixed where it is, and choosing another file is never offered for them.
    #[test]
    fn the_neutral_ssh_key_help_offers_the_fix_its_source_takes() {
        use cli_core::ssh_key::KeySource;
        let help = |source: KeySource| {
            UiError::from(&CoreError::Cli(cli_core::CliError::SshKeyNotPublic {
                origin: "the key".into(),
                private_key: true,
                from: source,
            }))
            .help
            .unwrap()
        };
        for source in [KeySource::NewTargetFile, KeySource::TargetFile] {
            let h = help(source);
            assert!(h.contains("`.pub` file next to a private key"), "{h}");
            assert!(!h.contains("APPRAFTER_SSH_PUBLIC_KEY"), "{h}");
        }
        let env = help(KeySource::Env);
        assert!(
            env.contains("`APPRAFTER_SSH_PUBLIC_KEY`") && env.contains("unset it"),
            "{env}"
        );
        let manifest = help(KeySource::Manifest { index: 2 });
        assert!(manifest.contains("`sshKeys[2].public_key`"), "{manifest}");
        for h in [&env, &manifest] {
            assert!(!h.contains(".pub` file"), "{h}");
            assert!(h.contains("ssh-keygen -y -f"), "{h}");
        }
    }

    /// A target's own file is fixed by hand, restored, or — WI-458: a target that cannot be read
    /// is removed like any other — the target is removed and added again; the help names the
    /// file. Removing a target never repairs the store's own `config.yaml` (D.3d review #9), so
    /// its help offers no removal.
    #[test]
    fn an_unreadable_target_file_is_fixed_restored_or_removed_and_added_again() {
        let ui = UiError::from(&CoreError::Cli(cli_core::CliError::InvalidTargetConfig {
            path: "/s/targets/prod/credentials.yaml".into(),
            message: "not a valid target credentials map (line 1, column 1)".into(),
            target: Some("prod".into()),
        }));
        let help = ui.help.unwrap();
        assert!(help.contains("/s/targets/prod/credentials.yaml"), "{help}");
        assert!(
            help.contains("by hand") && help.contains("restore"),
            "{help}"
        );
        assert!(
            help.contains("remove target `prod`") && help.contains("add it again"),
            "{help}"
        );
        // WI-458 review #0/#2: what the removal deletes beside the two files, to fix or restore
        // the file first when a server is recorded, and that the record can be rebuilt — in
        // words the GUI stands behind (no CLI command: the guard above).
        for says in [
            "local state",
            "record of its server",
            "cached kubeconfig",
            "Argo CD password",
            "If a server is recorded, fix or restore the file first",
            "rebuilt from the provider",
        ] {
            assert!(help.contains(says), "{says}: {help}");
        }
        assert!(!help.contains("both of its files"), "{help}");
        let store = UiError::from(&CoreError::Cli(cli_core::CliError::InvalidTargetConfig {
            path: "/s/config.yaml".into(),
            message: "missing field `version`".into(),
            target: None,
        }));
        let help = store.help.unwrap();
        assert!(
            help.contains("/s/config.yaml") && help.contains("restore"),
            "{help}"
        );
        assert!(
            !help.contains("remove") && !help.contains("add it again"),
            "{help}"
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
        assert!(ui.fields.is_empty(), "{:?}", ui.fields);
    }

    /// WI-458 review #6: an I/O error on one of a target's own files is the same code and
    /// message as any I/O error, and its fields name the target and the file — what the desktop
    /// offers that target's removal on — and its help names the file, which the OS message does
    /// not.
    #[test]
    fn an_io_error_on_a_targets_own_file_names_the_target() {
        let source = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let ui = UiError::from(&CoreError::from(cli_core::CliError::TargetFileIo {
            path: "/s/targets/prod/credentials.yaml".into(),
            target: "prod".into(),
            source,
        }));
        assert_eq!(ui.code.as_deref(), Some(codes::IO_ERROR));
        assert_eq!(ui.message, "io error: denied");
        assert_eq!(ui.causes, vec!["denied".to_string()]);
        assert_eq!(
            json!(ui.fields),
            json!({"path": "/s/targets/prod/credentials.yaml", "target": "prod"})
        );
        let help = ui.help.unwrap();
        assert!(
            help.contains("/s/targets/prod/credentials.yaml") && help.contains("`prod`"),
            "{help}"
        );
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
