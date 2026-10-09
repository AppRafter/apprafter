// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! TEMPORARY. Today's `apprafter::cli::other` wording for the refusals the core now types, so the
//! move onto the core changes no golden. Bug 5 deletes the `UnknownProvider` arm, bug 2 this file.
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
