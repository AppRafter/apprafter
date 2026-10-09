// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! TEMPORARY. Today's `apprafter::cli::other` wording for the refusals the core now types, so the
//! move onto the core changes no golden. Bug 2 deletes this file.
//!
//! Each text is produced by the CLI's own code or copied from it, never from the core's
//! message, so a refusal no golden covers still reads exactly as before.

use apprafter_core::CoreError;
use cli_core::CliError;

use crate::render::core_error::report;

fn other(text: String) -> miette::Report {
    miette::Report::new(CliError::Other(text))
}

/// The arms every command shares; `token` is the one the command was given.
fn common(e: &CoreError, token: Option<&str>) -> Option<String> {
    Some(match e {
        CoreError::InvalidTargetName { name, .. } => {
            crate::commands::target::check_target_name(name).err()?
        }
        CoreError::InvalidToken { .. } => format!(
            "invalid Hetzner Cloud token: {}",
            cli_core::validate_hetzner_token_format(token?).err()?
        ),
        CoreError::SshKeyUnreadable {
            path,
            problem: apprafter_core::ssh::SshKeyProblem::Missing,
            ..
        } => format!("SSH key path `{path}` does not exist"),
        CoreError::SshKeyUnreadable { path, error, .. } => format!(
            "SSH key `{path}` is not readable: {}",
            error.as_deref().unwrap_or_default()
        ),
        _ => return None,
    })
}

/// `target rename`'s refusals as today: the two `rename_target` / `check_rename` texts, then
/// the shared ones; anything else through the core renderer.
pub(crate) fn rename(e: CoreError) -> miette::Report {
    let text = match &e {
        CoreError::SameTargetName { .. } => Some(
            "source and destination target names are identical — nothing to rename".to_string(),
        ),
        CoreError::TargetExists { name } => Some(format!(
            "target `{name}` already exists — pick a different name or remove the existing target first"
        )),
        other => common(other, None),
    };
    match text {
        Some(t) => other(t),
        None => report(e),
    }
}

/// `target add`'s refusals as today (`check_name_free`), then the shared ones over the token the command was given; anything else through the core renderer.
pub(crate) fn add(e: CoreError, token: &str) -> miette::Report {
    let text = match &e {
        CoreError::TargetExists { name } => Some(format!(
            "target `{name}` already exists — pass `--force` to overwrite or `--renew` to rotate credentials only"
        )),
        other => common(other, Some(token)),
    };
    match text {
        Some(t) => other(t),
        None => report(e),
    }
}

/// `target add --renew`'s refusals as today (`reject_identical_token`, `load_renewable`), then
/// the shared ones; anything else through the core renderer.
pub(crate) fn renew(e: CoreError, token: &str) -> miette::Report {
    let text = match &e {
        CoreError::RenewTokenUnchanged { name } => Some(format!(
            "`--renew` requires a NEW token, but the value provided is identical to the one already saved for target `{name}`. Generate a fresh token in the Hetzner Cloud Console → Security → API Tokens, then re-run `apprafter target add {name} --renew` with the new value."
        )),
        CoreError::TargetNotFound { name, .. } => Some(format!(
            "target `{name}` does not exist — drop `--renew` to create it fresh"
        )),
        other => common(other, Some(token)),
    };
    match text {
        Some(t) => other(t),
        None => report(e),
    }
}

/// `target machine`'s refusals as today: the provisioned refusal with its rebuild recipe, and
/// `resolve_hetzner_token`'s no-token text; then the shared ones; anything else through the
/// core renderer.
pub(crate) fn machine(e: CoreError) -> miette::Report {
    let text = match &e {
        CoreError::TargetProvisioned { name, .. } => Some(
            crate::commands::target_machine::provisioned_refusal_message(name),
        ),
        CoreError::TokenNotStored { name } => Some(format!(
            "target `{name}` has no Hetzner Cloud token stored. Run `apprafter target add {name} --renew --token <X>` to add one, or pass `--token`/`{}` for this invocation.",
            cli_core::HCLOUD_TOKEN_ENV
        )),
        other => common(other, None),
    };
    match text {
        Some(t) => other(t),
        None => report(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apprafter_core::CoreError;

    #[test]
    fn rename_refusals_render_todays_catch_all_text() {
        let text = |e| rename(e).to_string();
        assert_eq!(
            text(CoreError::SameTargetName { name: "a".into() }),
            "source and destination target names are identical — nothing to rename"
        );
        assert_eq!(
            text(CoreError::TargetExists { name: "b".into() }),
            "target `b` already exists — pick a different name or remove the existing target first"
        );
        let r = rename(CoreError::SameTargetName { name: "a".into() });
        assert_eq!(
            r.code().map(|c| c.to_string()).as_deref(),
            Some("apprafter::cli::other")
        );
    }

    #[test]
    fn an_invalid_name_renders_todays_reason() {
        let r = rename(CoreError::InvalidTargetName {
            name: "bad_name".into(),
            problem: apprafter_core::target::validate_name("bad_name").unwrap_err(),
        });
        assert_eq!(
            r.to_string(),
            "target name `bad_name` is invalid — allowed: alphanumeric + `-`"
        );
        assert_eq!(
            r.code().map(|c| c.to_string()).as_deref(),
            Some("apprafter::cli::other")
        );
    }

    #[test]
    fn machine_refusals_render_todays_text() {
        let r = machine(CoreError::TargetProvisioned {
            name: "prod".into(),
            server_id: 1,
            server_name: "p".into(),
        });
        assert_eq!(
            r.to_string(),
            crate::commands::target_machine::provisioned_refusal_message("prod")
        );
        assert!(r
            .to_string()
            .starts_with("`prod` already runs a provisioned cluster"));
        let r = machine(CoreError::TokenNotStored {
            name: "prod".into(),
        });
        assert_eq!(
            r.to_string(),
            "target `prod` has no Hetzner Cloud token stored. Run `apprafter target add prod \
             --renew --token <X>` to add one, or pass `--token`/`HCLOUD_TOKEN` for this \
             invocation."
        );
        assert_eq!(
            r.code().map(|c| c.to_string()).as_deref(),
            Some("apprafter::cli::other")
        );
    }

    #[test]
    fn add_and_renew_refusals_render_todays_text() {
        let t = |r: miette::Report| r.to_string();
        assert_eq!(
            t(add(CoreError::TargetExists { name: "prod".into() }, "x")),
            "target `prod` already exists — pass `--force` to overwrite or `--renew` to rotate credentials only"
        );
        assert_eq!(
            t(add(
                CoreError::InvalidToken {
                    problem: apprafter_core::provider::TokenProblem::WrongLength { got: 5 }
                },
                "short"
            )),
            "invalid Hetzner Cloud token: Hetzner Cloud tokens are 64 ASCII alphanumeric characters; got 5"
        );
        assert_eq!(
            t(add(
                CoreError::SshKeyUnreadable {
                    path: "/k.pub".into(),
                    problem: apprafter_core::ssh::SshKeyProblem::Missing,
                    error: None
                },
                "x"
            )),
            "SSH key path `/k.pub` does not exist"
        );
        assert_eq!(
            t(add(
                CoreError::SshKeyUnreadable {
                    path: "/k.pub".into(),
                    problem: apprafter_core::ssh::SshKeyProblem::Unreadable,
                    error: Some("Permission denied (os error 13)".into())
                },
                "x"
            )),
            "SSH key `/k.pub` is not readable: Permission denied (os error 13)"
        );
        assert!(t(renew(
            CoreError::RenewTokenUnchanged {
                name: "prod".into()
            },
            "x"
        ))
        .starts_with("`--renew` requires a NEW token"));
        assert_eq!(
            t(renew(
                CoreError::TargetNotFound {
                    name: "ghost".into(),
                    available: vec![]
                },
                "x"
            )),
            "target `ghost` does not exist — drop `--renew` to create it fresh"
        );
        for r in [
            add(CoreError::TargetExists { name: "p".into() }, "x"),
            renew(CoreError::RenewTokenUnchanged { name: "p".into() }, "x"),
        ] {
            assert_eq!(
                r.code().map(|c| c.to_string()).as_deref(),
                Some("apprafter::cli::other")
            );
        }
    }

    /// Everything the legacy table does not name renders as the core renderer does.
    #[test]
    fn other_refusals_keep_the_core_rendering() {
        let r = rename(CoreError::TargetNotFound {
            name: "ghost".into(),
            available: vec!["a".into(), "b".into()],
        });
        assert_eq!(
            r.code().map(|c| c.to_string()).as_deref(),
            Some("apprafter::target::not_found")
        );
    }
}
