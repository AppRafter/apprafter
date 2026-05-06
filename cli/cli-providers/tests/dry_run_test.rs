// SPDX-License-Identifier: FSL-1.1-MIT
use cli_providers::{Action, DryRunProvider, Plan, Provider};

#[test]
fn dry_run_plan_lists_no_changes_on_default_state() {
    let provider = DryRunProvider::new();
    let plan = provider.plan().unwrap();
    assert!(plan.actions.is_empty(), "expected empty plan, got {plan:?}");
    assert_eq!(plan.summary(), "no changes");
}

#[test]
fn dry_run_apply_returns_zero_changes() {
    let provider = DryRunProvider::new();
    let outcome = provider.apply().unwrap();
    assert_eq!(outcome.applied, 0);
}

#[test]
fn dry_run_destroy_returns_zero_changes() {
    let provider = DryRunProvider::new();
    let outcome = provider.destroy().unwrap();
    assert_eq!(outcome.destroyed, 0);
}

#[test]
fn plan_summary_pluralises() {
    let p = Plan {
        actions: vec![Action::Noop],
    };
    assert_eq!(p.summary(), "1 action");
    let p2 = Plan {
        actions: vec![Action::Noop, Action::Noop],
    };
    assert_eq!(p2.summary(), "2 actions");
}

#[test]
fn plan_summary_handles_mixed_actions() {
    let p = Plan {
        actions: vec![
            Action::CreateSshKey("k".into()),
            Action::CreateServer("s".into()),
        ],
    };
    assert_eq!(p.summary(), "2 actions");
}
