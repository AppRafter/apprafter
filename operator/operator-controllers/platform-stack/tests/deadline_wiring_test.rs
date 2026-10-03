// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `PlatformController` runs every reconcile under its deadline (WI-400).
//!
//! `reconcile::run` builds a live `Controller` against an apiserver, which no
//! unit test drives, so this reads the call site: the one `Controller::run`
//! in the crate must hand kube-runtime `reconcile_with_deadline`, and that
//! wrapper must run `reconcile` inside
//! `operator_core::deadline::within(RECONCILE_DEADLINE, …)`. The behaviour of
//! the wrapper is proven in `reconcile::bounded_reconcile_tests`.

/// `source` from byte `at`, whitespace removed, `len` characters long.
fn squeezed(source: &str, at: usize, len: usize) -> String {
    source[at..]
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(len)
        .collect()
}

#[test]
fn the_controller_runs_every_reconcile_under_its_deadline() {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/reconcile.rs"))
        .expect("read src/reconcile.rs");
    let calls: Vec<String> = source
        .match_indices(".run(")
        .map(|(at, _)| squeezed(&source, at, 120))
        .collect();
    assert_eq!(calls.len(), 1, "one Controller::run call site: {calls:#?}");
    assert!(
        calls[0].starts_with(".run(reconcile_with_deadline,error_policy,"),
        "the controller must run the deadline wrapper: {}",
        calls[0]
    );
    let wrappers: Vec<String> = source
        .match_indices("async fn reconcile_with_deadline(")
        .map(|(at, _)| squeezed(&source, at, 400))
        .collect();
    assert_eq!(wrappers.len(), 1, "one wrapper: {wrappers:#?}");
    assert!(
        wrappers[0].contains(
            "operator_core::deadline::within(RECONCILE_DEADLINE,reconcile(stack.clone(),ctx.clone()))"
        ),
        "the wrapper must run the reconcile under its deadline: {}",
        wrappers[0]
    );
}
