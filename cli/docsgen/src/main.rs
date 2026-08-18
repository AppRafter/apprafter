// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `docsgen generate | check | gate` — write the CLI reference, gate
//! it, or gate the hand-written documentation against the product.
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

/// `gate` failed on the documentation: findings to fix.
const FINDINGS: u8 = 1;
/// The gate itself broke — no `cue` on `PATH`, an unreadable page, a
/// diagnostic naming none of the documents under test.
///
/// A separate code because the two are different work: one is a page to
/// edit, the other is a checkout or a toolchain to repair, and a caller
/// that cannot tell them apart eventually treats both as "the docs are
/// wrong again".
const BROKEN: u8 = 2;

fn main() -> ExitCode {
    // `gate` owns its exit codes, so it is dispatched before the
    // Ok/Err-to-0/1 wrapper the other two share.
    if std::env::args().nth(1).as_deref() == Some("gate") {
        return gate();
    }
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
            Err(format!("docsgen: usage: docsgen <generate|check|gate> (got {got})").into())
        }
    }
}

/// Run the documentation drift gate and report it.
fn gate() -> ExitCode {
    let root = match docsgen::repo_root() {
        Ok(root) => root,
        Err(e) => {
            eprintln!("docsgen gate: {e}");
            return ExitCode::from(BROKEN);
        }
    };
    match docsgen::gate::run(&root) {
        Ok(findings) if findings.is_empty() => {
            eprintln!("docsgen gate OK: every claim in the in-scope documentation resolves");
            ExitCode::SUCCESS
        }
        Ok(findings) => {
            eprint!("{}", docsgen::gate::report(&findings));
            ExitCode::from(FINDINGS)
        }
        Err(e) => {
            eprintln!("docsgen gate BROKE (nothing was judged): {e}");
            ExitCode::from(BROKEN)
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
