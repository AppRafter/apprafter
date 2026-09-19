// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter completion <shell>` — emit a shell completion script.
//!
//! # The shell set is the generator's, not a list kept here
//!
//! The `shell` argument is [`clap_complete::Shell`] itself rather than
//! a local enum that maps onto it. A local enum would be a **second**
//! statement of which shells are supported, kept by hand, about a
//! capability owned somewhere else — and `Shell` is `#[non_exhaustive]`,
//! so that second list would silently freeze at today's set the next
//! time the generator grows one. Taking the type verbatim means
//! `--help`'s possible-values, the parser and the generator are all the
//! same fact, and the set moves when the dependency does.
//!
//! Today that is five: `bash`, `elvish`, `fish`, `powershell`, `zsh`.
//! Each emits a real script — `cli/platform-cli/tests/completions_test.rs`
//! asserts every one of them non-trivial and naming commands this CLI
//! actually has, so none of the five is a claim nobody checked.
//!
//! **Supported is not the same as documented.** The published install
//! recipes cover `bash`, `zsh` and `fish` — the three a reader of these
//! guides plausibly runs — and no more, because a recipe for a shell
//! whose conventions we have not verified would be invented rather than
//! written. The other two still get a script; they do not get a recipe.
//!
//! # A hidden subcommand still completes, and that is upstream's call
//!
//! `#[command(hide = true)]` keeps `auth` out of `--help`; it does not
//! keep it out of these scripts. Every one of the five offers it, so
//! `apprafter a<TAB>` lists a command the help screen deliberately does
//! not. The behaviour is `clap_complete`'s: its generators filter hidden
//! *possible values* (`filter(|pv| !pv.is_hide_set())`, in the bash, zsh
//! and fish backends) and nothing else — a hidden subcommand or arg is
//! emitted, on the reading that completion describes what parses rather
//! than what is advertised.
//!
//! Recorded rather than worked around. clap exposes no way to remove a
//! subcommand from a built tree, so suppressing it means rebuilding the
//! tree by hand — and a hand-rebuilt `Command` silently drops whatever
//! attribute the rebuild forgot, which is a worse defect than the one it
//! fixes. If the visibility of `auth` is what matters, the decision to
//! revisit is `hide` itself, not this module.
//!
//! # Only bash and zsh complete this command's own argument
//!
//! `<SHELL>` is a positional with a fixed value set, and only the bash
//! and zsh backends emit the values of a *positional*: bash writes them
//! into its `opts=` list, zsh into `:(bash elvish fish powershell zsh)`.
//! The fish, elvish and PowerShell backends emit possible values for
//! **flags** only — walked with a real fish 4.6, `apprafter completion
//! <TAB>` falls back to filename completion there. Upstream behaviour
//! again, and it shows nowhere else in this CLI: every other fixed-set
//! argument is a flag or a plain `String` validated at run time, so this
//! is the only positional `value_enum` in the tree.
//!
//! # Why the script comes off the same tree the binary parses
//!
//! [`crate::run`] parses with `examples::attach(Cli::command())`, so the
//! completion script is generated from that same construction. Building
//! a second, bare tree here would put the shipped parser and the shipped
//! completions one refactor away from disagreeing — a command added
//! behind a builder call would complete for nobody, and nothing would
//! say so.
//!
//! # `--install` writes the file, and only for the three with a recipe
//!
//! Redirecting the script by hand is three things a reader has to get
//! right at once — the directory, the file name, and creating the
//! directory first, which does not exist on a clean machine and whose
//! absence fails the redirect with `No such file or directory` on the
//! one command whose example IS the instruction. [`LAYOUTS`] is that
//! knowledge held once, and `--install` performs it.
//!
//! The table covers `bash`, `zsh` and `fish` and stops there, for the
//! reason the shell set above already gives: a destination for a shell
//! whose conventions nobody here verified would be invented rather than
//! known, and a completion script written to a plausible-looking wrong
//! path completes nothing while reporting success. The other two are
//! refused by name, with the redirect they can still use.
//!
//! # Why the script still goes to stdout under `--install`
//!
//! A child process cannot add a completion to the shell that spawned
//! it: `compdef` and `complete` are builtins, and they change the shell
//! that runs them. The only thing that reaches the live shell is text
//! the shell itself reads — so `--install` keeps stdout the script, and
//!
//! ```text
//! source <(apprafter completion bash --install)
//! ```
//!
//! installs for future shells and applies to this one in one line.
//!
//! Suppressed when stdout is a terminal, which is the one case where
//! nothing is reading it: the file has already been written, and
//! several hundred lines of shell scrolling the report off the screen
//! is not a second way of being useful. The report goes to stderr in
//! both cases — on stdout it would be fed to the shell as code.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::CommandFactory;
use clap::ValueEnum;
use clap_complete::Shell;
use cli_core::{style, CliError, Result};

use crate::cli::Cli;

/// The base directory a shell's completion directory hangs off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Root {
    /// `$XDG_DATA_HOME`, else `~/.local/share`.
    Data,
    /// `$XDG_CONFIG_HOME`, else `~/.config`.
    Config,
    /// `$HOME` itself.
    Home,
}

/// Where one shell reads `apprafter`'s completions from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    root: Root,
    /// Path under that root, including the file name the shell expects
    /// — the name is not decoration: bash and fish key the file to the
    /// command, zsh to the completion function (`_apprafter`).
    relative: &'static str,
}

/// Every shell `--install` can write for, and where.
///
/// The same three the published recipes cover, and the same paths:
/// `docs/dev-guide/quickstart.md` teaches the hand-written redirect for
/// a reader who wants to see it, and `examples::EXAMPLES` prints them
/// under `--help`. If one moves, all three move together — which is
/// what `completions_test.rs` pins, against the filesystem rather than
/// against this table.
const LAYOUTS: &[(Shell, Layout)] = &[
    (
        Shell::Bash,
        Layout {
            root: Root::Data,
            relative: "bash-completion/completions/apprafter",
        },
    ),
    (
        Shell::Zsh,
        Layout {
            // Not under XDG: zsh reads completion functions off `fpath`,
            // and no XDG directory is on it by default, so the path is
            // the reader's own `~/.zfunc` convention plus the `fpath`
            // line that makes it real. Reported, never edited into a
            // shell rc by us.
            root: Root::Home,
            relative: ".zfunc/_apprafter",
        },
    ),
    (
        Shell::Fish,
        Layout {
            root: Root::Config,
            relative: "fish/completions/apprafter.fish",
        },
    ),
];

/// The layout for `shell`, if it has one.
fn layout(shell: Shell) -> Option<Layout> {
    LAYOUTS
        .iter()
        .find(|(candidate, _)| *candidate == shell)
        .map(|(_, layout)| *layout)
}

/// The shells `--install` writes for, spelled as the argument takes
/// them. Derived from [`LAYOUTS`] so a refusal cannot name a set the
/// command does not actually serve.
fn installable() -> Vec<String> {
    Shell::value_variants()
        .iter()
        .filter(|shell| layout(**shell).is_some())
        .map(|shell| shell.to_string())
        .collect()
}

/// Whether the script still goes to stdout.
///
/// Without `--install` it always does: printing it is the command.
/// With `--install` it goes only where something is reading it, which
/// is what makes `source <(apprafter completion bash --install)` one
/// line instead of two. See the module header.
fn prints_script(install: bool, stdout_is_terminal: bool) -> bool {
    !install || !stdout_is_terminal
}

/// Resolve one base directory, naming what could not be resolved.
fn root_path(root: Root) -> Result<PathBuf> {
    let (resolved, spelled) = match root {
        Root::Data => (dirs::data_dir(), "$XDG_DATA_HOME (or ~/.local/share)"),
        Root::Config => (dirs::config_dir(), "$XDG_CONFIG_HOME (or ~/.config)"),
        Root::Home => (dirs::home_dir(), "$HOME"),
    };
    resolved.ok_or_else(|| {
        CliError::CompletionInstall(format!(
            "cannot work out where to install: {spelled} does not resolve \
             to a directory. Redirect the script yourself instead."
        ))
    })
}

/// The file `shell`'s script has to land on, or a refusal naming the
/// shells that do install.
fn destination(shell: Shell) -> Result<PathBuf> {
    let layout = layout(shell).ok_or_else(|| {
        CliError::CompletionInstall(format!(
            "`--install` has no destination for {shell}: the shells with a \
             published install path are {}. {shell} still gets a script — \
             redirect it to wherever your shell reads completions from: \
             `apprafter completion {shell} > <file>`.",
            installable().join(", ")
        ))
    })?;
    Ok(root_path(layout.root)?.join(layout.relative))
}

/// Write `script` to `destination`, creating its directory.
///
/// Through a temporary file in the same directory and a rename, so a
/// full disk or a killed process cannot leave a HALF-WRITTEN script
/// where the shell reads one: a truncated completion script is not an
/// absent completion, it is a syntax error every new shell evaluates.
fn write_script(destination: &Path, script: &[u8]) -> Result<()> {
    let directory = destination.parent().ok_or_else(|| {
        CliError::CompletionInstall(format!(
            "{} has no parent directory to write into",
            destination.display()
        ))
    })?;
    std::fs::create_dir_all(directory)?;

    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(script)?;
    file.flush()?;
    // `NamedTempFile` creates at 0600. A completion script is not a
    // secret and the shell that reads it may not be this user's login
    // shell, so it lands with the mode an ordinary file would have.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))?;
    }
    file.persist(destination).map_err(|error| {
        CliError::CompletionInstall(format!(
            "could not replace {}: {error}",
            destination.display()
        ))
    })?;
    Ok(())
}

/// What the reader still has to know after the file is written: what
/// makes a NEW shell read it, and how to get it into THIS one.
///
/// Every line is true of a shell nobody here can reach from inside this
/// process — `fpath` and `compinit` live in the reader's own rc, and a
/// running shell picks up a builtin only by evaluating text itself.
fn report(shell: Shell, destination: &Path) -> Vec<String> {
    let path = destination.display().to_string();
    let mut lines = vec![format!(
        "{} {shell} completions written to {path}",
        style::ok("✓")
    )];
    match shell {
        Shell::Zsh => {
            let directory = destination
                .parent()
                .map(|parent| parent.display().to_string())
                .unwrap_or_else(|| "~/.zfunc".to_string());
            lines.push(format!(
                "  a new shell reads it once {directory} is on fpath — add to \
                 ~/.zshrc, ABOVE compinit:"
            ));
            lines.push(format!("      fpath=({directory} $fpath)"));
            lines.push("      autoload -Uz compinit && compinit".to_string());
        }
        Shell::Fish => lines.push("  fish reads it at the next prompt".to_string()),
        _ => lines.push(
            "  a new shell reads it automatically (needs the bash-completion \
             package)"
                .to_string(),
        ),
    }
    lines.push(format!(
        "  this shell, now: {}",
        style::info(&format!("source {path}"))
    ));
    lines
}

/// Write the completion script for `shell` to stdout, and — with
/// `install` — to the file that shell reads completions from.
pub fn run(shell: Shell, install: bool) -> Result<()> {
    let mut command = crate::examples::attach(Cli::command());

    // A completion script hard-codes the name it completes. Reading it
    // off the built command rather than writing "apprafter" a second
    // time keeps a rename from producing a script that completes a
    // binary nobody has.
    let binary = command.get_name().to_string();

    // Rendered into memory first. `clap_complete::generate` writes
    // through a `dyn Write` and panics on a write error, which on a
    // closed pipe (`apprafter completion bash | head`) would turn an
    // ordinary shell idiom into a panic message; a `Vec` cannot fail,
    // and the one write that can is ours to report as a typed IO error.
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut command, binary, &mut script);

    // Installed BEFORE anything is printed. A refusal (no destination
    // for this shell, no resolvable home) has to be the whole outcome
    // of the run, not a message trailing a script that already went to
    // the terminal as if the command had worked.
    if install {
        let destination = destination(shell)?;
        write_script(&destination, &script)?;
        let mut stderr = std::io::stderr();
        for line in report(shell, &destination) {
            writeln!(stderr, "{line}")?;
        }
        stderr.flush()?;
    }

    if prints_script(install, std::io::stdout().is_terminal()) {
        let mut out = std::io::stdout();
        out.write_all(&script)?;
        out.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shells with a destination are exactly the shells the
    /// published recipes cover. Both sides are read from the code that
    /// owns them, so adding a fourth layout without a recipe — or a
    /// recipe without a layout — is what this fails on.
    #[test]
    fn the_installable_set_is_bash_zsh_and_fish() {
        assert_eq!(installable(), vec!["bash", "fish", "zsh"]);
    }

    #[test]
    fn each_layout_names_the_file_its_shell_looks_for() {
        // The file NAME is the half a wrong recipe gets wrong silently:
        // a bash script under `_apprafter`, or a zsh function under the
        // command's own name, is read by nobody.
        assert_eq!(
            layout(Shell::Bash).map(|l| (l.root, l.relative)),
            Some((Root::Data, "bash-completion/completions/apprafter"))
        );
        assert_eq!(
            layout(Shell::Zsh).map(|l| (l.root, l.relative)),
            Some((Root::Home, ".zfunc/_apprafter"))
        );
        assert_eq!(
            layout(Shell::Fish).map(|l| (l.root, l.relative)),
            Some((Root::Config, "fish/completions/apprafter.fish"))
        );
    }

    #[test]
    fn a_shell_without_a_published_recipe_has_no_layout() {
        assert_eq!(layout(Shell::Elvish), None);
        assert_eq!(layout(Shell::PowerShell), None);
    }

    #[test]
    fn the_script_is_printed_unless_install_wrote_it_and_a_terminal_is_watching() {
        // Row by row: printing IS the command without the flag; with
        // it, stdout is for whatever is reading it, and a terminal is
        // not reading it.
        assert!(prints_script(false, true));
        assert!(prints_script(false, false));
        assert!(prints_script(true, false));
        assert!(!prints_script(true, true));
    }

    #[test]
    fn a_refusal_names_every_shell_that_does_install() {
        let refused = destination(Shell::Elvish).expect_err("elvish has no destination");
        let message = refused.to_string();
        for shell in installable() {
            assert!(message.contains(&shell), "{message}");
        }
        assert!(message.contains("elvish"), "{message}");
    }

    #[test]
    fn a_refusal_is_a_deliberate_answer_not_a_defect_report() {
        // Through the catch-all variant this rendered under a `help:`
        // telling the reader to FILE AN ISSUE about recurring wording —
        // advice written for a message nobody planned, on the one
        // outcome this command decides on purpose.
        let refused = destination(Shell::PowerShell).expect_err("powershell has no destination");
        assert_eq!(
            miette::Diagnostic::code(&refused).map(|code| code.to_string()),
            Some("apprafter::completion::install".to_string())
        );
        let help = miette::Diagnostic::help(&refused)
            .map(|help| help.to_string())
            .unwrap_or_default();
        assert!(
            !help.contains("file an issue"),
            "a refused shell is not a bug report: {help}"
        );
        assert!(
            help.contains("apprafter completion"),
            "the help does not name the form that always works: {help}"
        );
    }

    #[test]
    fn the_zsh_report_carries_the_fpath_wiring_the_path_alone_does_not_give() {
        let lines = report(Shell::Zsh, Path::new("/home/someone/.zfunc/_apprafter")).join("\n");
        assert!(lines.contains("/home/someone/.zfunc/_apprafter"), "{lines}");
        assert!(
            lines.contains("fpath=(/home/someone/.zfunc $fpath)"),
            "{lines}"
        );
        assert!(lines.contains("compinit"), "{lines}");
        assert!(
            lines.contains("source /home/someone/.zfunc/_apprafter"),
            "{lines}"
        );
    }
}
