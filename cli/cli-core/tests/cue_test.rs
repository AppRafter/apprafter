// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for the CUE subprocess wrapper.
//!
//! The two cases (happy path + CueNotFound) live in a single test
//! function because the second case mutates `CUE_BIN`, which would
//! race with a parallel happy-path test in the same process.

use std::path::{Path, PathBuf};

use cli_core::{cue, CliError};

#[test]
fn export_smoke() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf();
    // Path 1: happy path. cue is invoked from the repo root
    // (which contains cue.mod/) with a path relative to it.
    // We REQUIRE `cue` on PATH — the v0.1.109 CI incident
    // (silent-skipped fixture/assertion mismatch sat for 5+
    // commits) burned the previous "skip if missing" pattern.
    let target = Path::new("./examples/applications");
    match cue::export_in(&repo_root, target) {
        Ok(value) => assert!(
            value.is_object(),
            "cue export should yield a JSON object, got {value:?}"
        ),
        Err(CliError::CueNotFound) => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        Err(other) => panic!("unexpected error from cue::export_in: {other}"),
    }

    // Path 2: force CUE_BIN to a definitely-missing path to
    // exercise the CueNotFound branch. Restore the env var on the
    // way out so subsequent tests in the same process are not
    // affected.
    let prev = std::env::var("CUE_BIN").ok();
    std::env::set_var("CUE_BIN", "/nonexistent/binary/cue-fake");
    let err = cue::export(Path::new(".")).unwrap_err();
    match prev {
        Some(v) => std::env::set_var("CUE_BIN", v),
        None => std::env::remove_var("CUE_BIN"),
    }
    assert!(matches!(err, CliError::CueNotFound), "got: {err}");
}
