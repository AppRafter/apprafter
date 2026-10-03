// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Every Controller this crate's `run()` builds hands kube-runtime the
//! DEADLINE wrapper, never the bare reconcile (WI-400, GOTCHA-51).
//!
//! # Why a source test
//!
//! The wrappers are tested directly — each abandons its pass at its deadline
//! and writes no status. What those tests cannot see is whether `run()` uses
//! them: `.run(reconcile::reconcile, …)` compiles exactly as well as
//! `.run(reconcile::reconcile_with_deadline, …)`, and a Controller needs a
//! watch stream to drive at all. The wiring is one line per controller, so
//! the guard reads that line.

use std::fs;
use std::path::Path;

/// `src/lib.rs` with every whitespace character removed, so the assertions
/// survive rustfmt re-wrapping a call across lines.
fn lib_src_compact() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("lib.rs");
    fs::read_to_string(path)
        .expect("read src/lib.rs")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

#[test]
fn the_claim_controller_runs_the_deadline_wrapper() {
    let src = lib_src_compact();
    assert!(
        src.contains(".run(reconcile::reconcile_with_deadline,reconcile::error_policy,"),
        "the ResourceClaim Controller must run reconcile::reconcile_with_deadline"
    );
    assert!(
        !src.contains(".run(reconcile::reconcile,"),
        "the bare claim reconcile is wired into a Controller"
    );
}

#[test]
fn the_shared_volume_controller_runs_the_deadline_wrapper() {
    let src = lib_src_compact();
    assert!(
        src.contains(
            ".run(shared_volume::reconcile_shared_volume_with_deadline,shared_volume::error_policy_sv,"
        ),
        "the SharedVolume Controller must run shared_volume::reconcile_shared_volume_with_deadline"
    );
    assert!(
        !src.contains(".run(shared_volume::reconcile_shared_volume,"),
        "the bare SharedVolume reconcile is wired into a Controller"
    );
}
