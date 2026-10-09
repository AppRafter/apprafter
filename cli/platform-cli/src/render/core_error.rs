// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `CoreError` → `miette::Report`, with the CLI's help for each code (overview §3.6.4), so the
//! CLI's output changes only on purpose.

use std::fmt;

use apprafter_core::CoreError;
use cli_core::CliError;
use miette::Diagnostic;

/// `e` as the CLI shows it. The pass-through `CliError`s render as themselves, and the two
/// variants the core maps off `CliError` (`TargetNotFound`, `NoActiveTarget`) render through
/// that `CliError` again, so their text and help are today's byte for byte; every other
/// variant keeps the core's message, code and cause chain, with the CLI's help ([`cli_help`]).
pub(crate) fn report(e: CoreError) -> miette::Report {
    match e {
        CoreError::Cli(inner) => miette::Report::new(inner),
        CoreError::TargetNotFound { name, available } => {
            miette::Report::new(CliError::TargetNotFound {
                name,
                available: available.join(", "),
            })
        }
        CoreError::NoActiveTarget => miette::Report::new(CliError::NoActiveTarget),
        other => {
            let help = cli_help(&other).unwrap_or_default();
            miette::Report::new(WithCliHelp {
                inner: Box::new(other),
                help,
            })
        }
    }
}

/// [`report`] with `help` in place of the CLI's help — for a command whose way forward differs
/// (renew's `TargetNotFound`: "drop `--renew` to create it fresh", overview §3.6.4). Message,
/// code and cause chain stay exactly what `report` shows.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "first caller: D.3b's `target add --renew` arm")
)]
pub(crate) fn report_with_help(e: CoreError, help: &str) -> miette::Report {
    let inner: Box<dyn Diagnostic + Send + Sync> = match e {
        CoreError::Cli(inner) => Box::new(inner),
        CoreError::TargetNotFound { name, available } => Box::new(CliError::TargetNotFound {
            name,
            available: available.join(", "),
        }),
        CoreError::NoActiveTarget => Box::new(CliError::NoActiveTarget),
        other => Box::new(other),
    };
    miette::Report::new(WithCliHelp {
        inner,
        help: help.to_string(),
    })
}

/// The CLI's help for `e`; `None` for the three variants `report` renders through their own
/// `CliError`. Exhaustive: a new variant does not compile until it has CLI help.
pub(crate) fn cli_help(e: &CoreError) -> Option<String> {
    Some(match e {
        CoreError::Cli(_) | CoreError::TargetNotFound { .. } | CoreError::NoActiveTarget => {
            return None
        }
        CoreError::Cancelled => "The command was interrupted; nothing after that point ran.".into(),
        CoreError::UnsafeOverride { .. } => {
            "Unset the variable, or give it a value the message above accepts.".into()
        }
        CoreError::TargetExists { name } => format!(
            "Pass `--force` to replace it (fields you do not pass are kept) or `--renew` to \
             rotate only its token; `apprafter target show {name}` shows what is stored."
        ),
        CoreError::RenewTokenUnchanged { name } => format!(
            "Generate a fresh token in the Hetzner Cloud Console → Security → API Tokens, then \
             re-run `apprafter target add {name} --renew` with the new value."
        ),
        CoreError::InvalidTargetName { .. } => format!(
            "A target name is 1–{} characters of ASCII letters, digits and `-`, and does not \
             start or end with `-`.",
            apprafter_core::target::TARGET_NAME_MAX_LEN
        ),
        CoreError::SameTargetName { .. } => {
            "Name a different destination; nothing was renamed.".into()
        }
        CoreError::UnknownProvider { supported, .. } => format!(
            "Supported providers: {}. Pass one of them with `--provider`.",
            supported.join(", ")
        ),
        CoreError::InvalidToken { .. } => {
            "Copy the token again from the Hetzner Cloud Console → Security → API Tokens: 64 \
             ASCII letters and digits, no prefix, no trailing newline."
                .into()
        }
        CoreError::TokenNotStored { name } => format!(
            "Run `apprafter target add {name} --renew --token <X>` to store one, or set \
             `HCLOUD_TOKEN` for this invocation."
        ),
        CoreError::SshKeyUnreadable { .. } => {
            "Point `--ssh-key` at a readable public key file (for example \
             ~/.ssh/id_ed25519.pub), or leave it out."
                .into()
        }
        CoreError::NotProvisioned { name } => {
            format!("Target `{name}` has no server yet: `apprafter up` provisions one.")
        }
        CoreError::ServerMissing { .. } => {
            "The server was deleted outside AppRafter, or the token belongs to another Hetzner \
             project. Check the Hetzner Cloud Console; `apprafter import --force` rebuilds the \
             local state from the servers labelled `apprafter=true`."
                .into()
        }
        CoreError::TargetProvisioned { .. } => {
            "There is no in-place resize. Rebuild from a backup:\n\n    apprafter backup \
             create\n    apprafter restore --reprovision --server-type <sku>\n\n(`target \
             machine` and `target add --force` change the machine only on a target that has \
             not provisioned yet.)"
                .into()
        }
        CoreError::ProviderRequestFailed { .. } => {
            "The provider API did not answer; nothing was changed. `apprafter doctor` checks \
             reachability and DNS; retry once it passes."
                .into()
        }
        CoreError::ToolUnsupported { tool, .. } => format!(
            "Install the `.exe` build of `{tool}`: a `.cmd` or `.bat` shim cannot be run \
             directly."
        ),
        CoreError::Kube { .. } => {
            "Check that the cluster is up; `apprafter kubeconfig --refresh` fetches its \
             kubeconfig again."
                .into()
        }
        // No `kubeconfig --refresh` here: today's `kubeconfig.rs:49` decrypts the cache with
        // `load_or_create_identity` before it looks at `--refresh`, so with the key lost it
        // creates a new key (a write) and then fails to decrypt (D.3c's gotcha, Task 11 Step 4).
        CoreError::AgeKeyMissing { .. } => {
            "Restore the age key file the cache was encrypted with, or set `APPRAFTER_AGE_KEY` \
             to where it is."
                .into()
        }
    })
}

/// An error with the CLI's help: everything else — message, code, cause chain — is the inner
/// diagnostic's own (a `CoreError`, or the `CliError` `report_with_help` maps it to).
#[derive(Debug)]
struct WithCliHelp {
    inner: Box<dyn Diagnostic + Send + Sync>,
    help: String,
}

impl fmt::Display for WithCliHelp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&*self.inner, f)
    }
}

impl std::error::Error for WithCliHelp {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

impl Diagnostic for WithCliHelp {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.inner.code()
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(&self.help))
    }

    fn diagnostic_source(&self) -> Option<&dyn Diagnostic> {
        self.inner.diagnostic_source()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apprafter_core::error::samples::{one_of_each, VARIANTS};

    #[test]
    fn every_variant_renders_with_a_code_and_cli_help() {
        let samples = one_of_each();
        assert_eq!(
            samples.len(),
            VARIANTS,
            "the core's one list of samples (Task 12)"
        );
        for e in samples {
            let shown = e.to_string();
            let report = report(e);
            let code = report.code().map(|c| c.to_string()).unwrap_or_default();
            let help = report.help().map(|h| h.to_string()).unwrap_or_default();
            assert!(code.starts_with("apprafter::"), "{shown}: {code}");
            assert!(!help.is_empty(), "{code} has no help");
            assert!(!help.contains("file an issue"), "{code}: {help}");
        }
    }

    #[test]
    fn not_found_and_no_active_keep_the_cli_text() {
        let r = report(CoreError::TargetNotFound {
            name: "ghost".into(),
            available: vec!["a".into(), "b".into()],
        });
        assert_eq!(r.to_string(), "target `ghost` not found (available: a, b)");
        assert!(r
            .help()
            .unwrap()
            .to_string()
            .contains("apprafter target list"));
        let r = report(CoreError::NoActiveTarget);
        assert!(r
            .to_string()
            .starts_with("no active target — run `apprafter target add"));
    }

    #[test]
    fn a_cli_error_renders_as_itself() {
        let r = report(CoreError::from(cli_core::CliError::BackupJobActive {
            job: "j".into(),
        }));
        assert_eq!(
            r.code().unwrap().to_string(),
            "apprafter::backup::job_active"
        );
    }

    #[test]
    fn the_help_names_the_target() {
        let r = report(CoreError::NotProvisioned {
            name: "prod".into(),
        });
        assert!(r.help().unwrap().to_string().contains("`prod`"));
    }

    #[test]
    fn a_missing_age_key_is_never_sent_to_kubeconfig_refresh() {
        // `kubeconfig --refresh` would create a new age key before it fails (kubeconfig.rs:49)
        let help = report(CoreError::AgeKeyMissing {
            path: "/k/age.key".into(),
        })
        .help()
        .unwrap()
        .to_string();
        assert!(help.contains("APPRAFTER_AGE_KEY"), "{help}");
        assert!(!help.contains("--refresh"), "{help}");
    }

    #[test]
    fn a_per_command_help_replaces_only_the_help() {
        // renew's TargetNotFound (overview §3.6.4): same message and code, its own way forward
        let missing = || CoreError::TargetNotFound {
            name: "ghost".into(),
            available: vec![],
        };
        let plain = report(missing());
        let r = report_with_help(missing(), "drop `--renew` to create it fresh");
        assert_eq!(r.to_string(), plain.to_string());
        assert_eq!(
            r.code().map(|c| c.to_string()),
            plain.code().map(|c| c.to_string())
        );
        assert_eq!(
            r.help().unwrap().to_string(),
            "drop `--renew` to create it fresh"
        );
        let r = report_with_help(CoreError::TargetExists { name: "p".into() }, "h");
        assert_eq!(r.code().unwrap().to_string(), "apprafter::target::exists");
        assert_eq!(r.help().unwrap().to_string(), "h");
        // The mapping onto the CLI's own error shows where the two messages differ: the core's
        // `NoActiveTarget` says only "no active target". (Its `TargetNotFound` message is the
        // CLI's byte for byte, so that arm cannot be told apart here.)
        let r = report_with_help(CoreError::NoActiveTarget, "h");
        assert_eq!(r.to_string(), report(CoreError::NoActiveTarget).to_string());
        assert_eq!(r.help().unwrap().to_string(), "h");
    }
}
