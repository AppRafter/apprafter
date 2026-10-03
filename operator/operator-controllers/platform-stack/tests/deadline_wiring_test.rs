// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `PlatformController` runs every reconcile under its deadline (WI-400).
//!
//! `reconcile::run` builds a live `Controller` against an apiserver, which no
//! unit test drives, so this reads the call site: the one `Controller::run`
//! in the crate must hand kube-runtime `reconcile` wrapped in
//! `operator_core::deadline::within(RECONCILE_DEADLINE, …)`. The behaviour of
//! that wrap is proven in `reconcile::bounded_reconcile_tests`.

#[test]
fn the_controller_runs_every_reconcile_under_its_deadline() {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/reconcile.rs"))
        .expect("read src/reconcile.rs");
    let calls: Vec<String> = source
        .match_indices(".run(")
        .map(|(at, _)| {
            source[at..]
                .chars()
                .filter(|c| !c.is_whitespace())
                .take(120)
                .collect()
        })
        .collect();
    assert_eq!(calls.len(), 1, "one Controller::run call site: {calls:#?}");
    assert!(
        calls[0].starts_with(
            ".run(|obj,ctx|operator_core::deadline::within(RECONCILE_DEADLINE,reconcile(obj,ctx)),error_policy,"
        ),
        "the reconcile must run under its deadline: {}",
        calls[0]
    );
}
