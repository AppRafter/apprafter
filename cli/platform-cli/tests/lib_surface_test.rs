// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The lib target exists so `docsgen` can project the clap tree
//! (spec §3.3). These assertions are the contract that refactor
//! must keep.

use clap::CommandFactory;

#[test]
fn docs_api_exposes_the_clap_tree() {
    let cmd = apprafter::docs_api::Cli::command();
    assert_eq!(cmd.get_name(), "apprafter");
    let top: Vec<&str> = cmd.get_subcommands().map(|c| c.get_name()).collect();
    for expected in ["target", "app", "secret", "backup", "platform"] {
        assert!(
            top.contains(&expected),
            "missing top-level command {expected}; got {top:?}"
        );
    }
}

#[test]
fn docs_api_exposes_the_cue_validator() {
    // Same entry point `apprafter app validate` uses — the snippet
    // gate calls exactly this.
    let missing = std::path::Path::new("/nonexistent/Application.cue");
    assert!(apprafter::docs_api::validate_manifest(missing).is_err());
}
