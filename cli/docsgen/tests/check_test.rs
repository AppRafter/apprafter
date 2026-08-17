// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The gate must fail on an edited generated file, a missing one, and
//! a stale page for a command that no longer exists. Without the last
//! case a removed command leaves a published page behind.

use clap::CommandFactory;
use docsgen::check::check;
use docsgen::render::render_all;

fn seeded_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for a in render_all(&apprafter::docs_api::Cli::command(), dir.path()) {
        std::fs::create_dir_all(a.path.parent().unwrap()).unwrap();
        std::fs::write(&a.path, &a.text).unwrap();
    }
    dir
}

#[test]
fn a_freshly_generated_tree_passes() {
    let dir = seeded_root();
    assert!(check(&apprafter::docs_api::Cli::command(), dir.path()).is_ok());
}

#[test]
fn an_edited_generated_file_fails_with_the_first_differing_line() {
    let dir = seeded_root();
    let page = dir.path().join("docs/reference/cli/secret.md");
    let tampered = std::fs::read_to_string(&page)
        .unwrap()
        .replace("apprafter-system", "default");
    std::fs::write(&page, tampered).unwrap();

    let err = check(&apprafter::docs_api::Cli::command(), dir.path()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("secret.md"), "must name the file: {msg}");
    assert!(
        msg.contains("apprafter-system"),
        "must show the drift: {msg}"
    );
}

#[test]
fn a_missing_generated_file_fails() {
    let dir = seeded_root();
    std::fs::remove_file(dir.path().join("docs/reference/cli/commands.json")).unwrap();
    assert!(check(&apprafter::docs_api::Cli::command(), dir.path()).is_err());
}

#[test]
fn a_stale_page_for_a_removed_command_fails() {
    let dir = seeded_root();
    std::fs::write(dir.path().join("docs/reference/cli/ghost.md"), "# ghost\n").unwrap();
    let err = check(&apprafter::docs_api::Cli::command(), dir.path()).unwrap_err();
    assert!(err.to_string().contains("ghost.md"));
}
