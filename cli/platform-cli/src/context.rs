// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The CLI's `apprafter_core::Context`: its whole environment, read here and nowhere else in
//! the core-backed arms (ADR 0067 §2). The core-backed arms build it with [`cli_context`]; an
//! arm that still returns `cli_core::Result` (a legacy arm) with [`legacy_context`]. Both are
//! one builder, which gives every context the CLI's vault access (`crate::vault_access`);
//! `doctor`, `whoami`, `lock status` and `status`'s `secrets:` line take [`diagnostic_context`],
//! which reads through this terminal's open window and never asks (D.4a T66).

use std::ffi::OsString;
use std::sync::Arc;

use apprafter_core::secrets::DiagnosticAccess;
use apprafter_core::{Context, CoreError, CoreResult, EnvSource};
use cli_core::CliError;

/// The process environment. The one `std::env::var` + `std::env::var_os` pair the core-backed
/// arms read through (counted by the core's env-read ratchet).
pub(crate) struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn var_os(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key)
    }
}

/// The CLI's one `Context` builder, with the CLI's vault access (D.4a d): a locked store's
/// secrets are asked for at the controlling terminal, once per command.
fn build() -> CoreResult<Context> {
    let ctx = Context::from_cli_env(&ProcessEnv)?;
    let access = crate::vault_access::process();
    access.set_home(ctx.home_dir());
    crate::holder_window::install(crate::holder_window::HolderPlace::of(&ctx));
    Ok(ctx.with_vault_access(access))
}

/// The context of a diagnostic — `doctor`, `whoami`, `lock status`, `status`'s `secrets:` line —
/// which never asks for a secret (D.4a T66): a locked store opens there only through this
/// terminal's open unlock window (the CLI's access, asked only for what it has open), never at
/// a prompt; anything else is `store_locked`, terminal or not. What it reads through the window restarts the
/// window, as any command's read does (T56).
pub(crate) fn diagnostic_context() -> miette::Result<Context> {
    let ctx = Context::from_cli_env(&ProcessEnv).map_err(crate::render::core_error::report)?;
    let access = crate::vault_access::process();
    access.set_home(ctx.home_dir());
    crate::holder_window::install(crate::holder_window::HolderPlace::of(&ctx));
    Ok(ctx.with_vault_access(Arc::new(DiagnosticAccess::over(access))))
}

/// Built by each core-backed arm, not once in `run()`: a command that never touches the core
/// cannot fail on it.
pub(crate) fn cli_context() -> miette::Result<Context> {
    build().map_err(crate::render::core_error::report)
}

/// [`cli_context`] for a legacy arm: the same context, its one build error (no config root)
/// as the `CliError` it wraps.
pub(crate) fn legacy_context() -> cli_core::Result<Context> {
    build().map_err(into_cli_error)
}

/// WI-478 (d4a-h F1's class): a legacy command that asks for a secret — a repository token, a
/// backup passphrase — and only then reads the target store (its cached kubeconfig, a token)
/// refuses a store other accounts can change before it asks, as that read would after the
/// prompt (`store_not_private`). Read only: owners and permissions of the store's folders.
pub(crate) fn refuse_a_store_others_can_change() -> cli_core::Result<()> {
    cli_core::store_privacy::ensure_private(&legacy_context()?.store())
}

/// A core error as a legacy arm's `CliError`: a wrapped `CliError` as it was; the core's
/// `TargetNotFound`, `NoActiveTarget`, `TokenNotStored` and `Cancelled` as the `CliError`s
/// `CoreError::from` makes them from (the names joined back with ", "); any other as its
/// message.
pub(crate) fn into_cli_error(e: CoreError) -> CliError {
    match e {
        CoreError::Cli(e) => e,
        CoreError::TargetNotFound { name, available } => CliError::TargetNotFound {
            name,
            available: available.join(", "),
        },
        CoreError::NoActiveTarget => CliError::NoActiveTarget,
        // D.4a T15: the core's code on a legacy arm.
        CoreError::TokenNotStored { name } => CliError::TokenNotStored { name },
        CoreError::Cancelled => CliError::Cancelled,
        other => CliError::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_core_error_becomes_the_cli_error_it_was_made_from() {
        assert!(matches!(
            into_cli_error(CoreError::Cli(CliError::Other("x".into()))),
            CliError::Other(m) if m == "x"
        ));
        match into_cli_error(CoreError::from(CliError::TargetNotFound {
            name: "ghost".into(),
            available: "a, b".into(),
        })) {
            CliError::TargetNotFound { name, available } => {
                assert_eq!((name.as_str(), available.as_str()), ("ghost", "a, b"));
            }
            e => panic!("{e:?}"),
        }
        assert!(matches!(
            into_cli_error(CoreError::NoActiveTarget),
            CliError::NoActiveTarget
        ));
        assert!(matches!(
            into_cli_error(CoreError::TokenNotStored { name: "p".into() }),
            CliError::TokenNotStored { name } if name == "p"
        ));
    }

    /// D.4a T15: a legacy arm shows `token_missing` with the code, message and help the core's
    /// own arms show (`render::core_error::cli_help`).
    #[test]
    fn a_missing_token_keeps_the_cores_code_on_a_legacy_arm() {
        use miette::Diagnostic;
        let core = CoreError::TokenNotStored {
            name: "prod".into(),
        };
        let help = crate::render::core_error::cli_help(&core).unwrap();
        let message = core.to_string();
        let cli = into_cli_error(core);
        assert!(matches!(&cli, CliError::TokenNotStored { name } if name == "prod"));
        assert_eq!(
            cli.code().map(|c| c.to_string()).as_deref(),
            Some("apprafter::target::token_missing")
        );
        assert_eq!(cli.to_string(), message);
        assert_eq!(cli.help().map(|h| h.to_string()), Some(help));
    }

    /// D.4a T61: a cancel keeps its code and its CLI help on a legacy arm.
    #[test]
    fn a_cancel_keeps_the_cores_code_on_a_legacy_arm() {
        use miette::Diagnostic;
        let help = crate::render::core_error::cli_help(&CoreError::Cancelled).unwrap();
        let cli = into_cli_error(CoreError::Cancelled);
        assert!(matches!(cli, CliError::Cancelled));
        assert_eq!(cli.help().map(|h| h.to_string()), Some(help));
    }
}
