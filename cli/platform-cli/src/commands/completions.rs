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

use std::io::Write;

use clap::CommandFactory;
use clap_complete::Shell;
use cli_core::Result;

use crate::cli::Cli;

/// Write the completion script for `shell` to stdout.
pub fn run(shell: Shell) -> Result<()> {
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

    let mut out = std::io::stdout();
    out.write_all(&script)?;
    out.flush()?;
    Ok(())
}
