// SPDX-License-Identifier: FSL-1.1-MIT
//! Integration tests for `cli_core::manifest::parse_application`.
//!
//! Skips at runtime when `cue` is absent from PATH (mirrors the
//! `manifest_test` / `cue_test` pattern).

use std::path::{Path, PathBuf};

use cli_core::manifest::{self, ApplicationManifest};
use cli_core::CliError;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

#[test]
fn parse_full_application_fixture() {
    let root = repo_root();
    let path = Path::new("./examples/applications");

    let parsed: ApplicationManifest = match manifest::parse_application(&root, path) {
        Ok(m) => m,
        Err(CliError::CueNotFound) => {
            eprintln!("skip: cue not on PATH");
            return;
        }
        Err(other) => panic!("unexpected: {other}"),
    };

    assert_eq!(parsed.api_version, "apprafter.io/v1alpha1");
    assert_eq!(parsed.kind, "Application");
    assert_eq!(parsed.metadata.name, "parser");

    let base = parsed.base.expect("base block decoded");
    assert_eq!(base.image.as_deref(), Some("ghcr.io/example/parser:latest"));
    assert_eq!(base.replicas, Some(3));

    let expose = base.expose.expect("expose decoded");
    assert_eq!(expose.port, 8080);
    assert_eq!(expose.public, Some(false));

    let env = base.env.expect("env decoded");
    assert_eq!(env.get("LOG_LEVEL").map(String::as_str), Some("info"));

    let envs = parsed.environments.expect("environments decoded");
    let dev = envs.get("dev").expect("dev override present");
    assert_eq!(dev.replicas, Some(1));
    let dev_expose = dev.expose.as_ref().expect("dev expose present");
    assert_eq!(dev_expose.network.as_deref(), Some("vpn"));

    let prod = envs.get("prod").expect("prod override present");
    assert_eq!(prod.replicas, Some(3));
}
