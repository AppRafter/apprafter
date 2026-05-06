// SPDX-License-Identifier: FSL-1.1-MIT
use cli_providers::{DryRunProvider, Provider};

#[test]
fn dry_run_plan_lists_no_changes_on_default_state() {
    let provider = DryRunProvider::new();
    let plan = provider.plan().unwrap();
    assert!(plan.changes.is_empty(), "expected empty plan, got {plan:?}");
    assert_eq!(plan.summary(), "no changes");
}

#[test]
fn dry_run_apply_returns_zero_changes() {
    let provider = DryRunProvider::new();
    let outcome = provider.apply().unwrap();
    assert_eq!(outcome.applied, 0);
}
