// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `docsgen generate | check` — write the CLI reference, or gate it.
//!
//! Argument parsing here is hand-rolled rather than clap-derived: this
//! binary reads the `apprafter` clap tree, and a second clap parser in
//! the same process is one more thing to keep straight when the surface
//! it reports on is exactly what a reader is trying to reason about.
//! Two words, no flags.

use clap::CommandFactory;
use docsgen::render::{render_all, DIR};
use std::error::Error;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let root = docsgen::repo_root()?;
    let cli = apprafter::docs_api::Cli::command();
    match std::env::args().nth(1).as_deref() {
        Some("generate") => generate(&cli, &root),
        Some("check") => docsgen::check::check(&cli, &root),
        other => {
            let got = other.unwrap_or("(nothing)");
            Err(format!("docsgen: usage: docsgen <generate|check> (got {got})").into())
        }
    }
}

/// Write every artefact, creating parent directories.
///
/// Deliberately write-only: it does not delete a file it no longer
/// generates. A generator that removes files it does not recognise is
/// one typo in `DIR` away from deleting a documentation tree, and the
/// stray check already turns a leftover page into a hard failure with
/// the file named — a reviewable `git rm` beats a silent deletion.
fn generate(cli: &clap::Command, root: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let artefacts = render_all(cli, root);
    for a in &artefacts {
        if let Some(parent) = a.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&a.path, &a.text)?;
        println!(
            "wrote {}",
            a.path.strip_prefix(root).unwrap_or(&a.path).display()
        );
    }
    println!("docsgen generate: {} artefact(s) in {DIR}", artefacts.len());
    Ok(())
}
