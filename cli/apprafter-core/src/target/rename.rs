// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `target rename`: rename a target (its state and the CLI default follow), as plan and execute.

use crate::op::{ChangeAction, Outcome, Plan, PlanClass};
use crate::report::Reporter;
use crate::target::{
    cancelled, change, cli_default, lock_store_if_present, validate_name, TargetRenamed,
};
use crate::{ActivePointerChange, CancellationToken, Context, CoreError, CoreResult, TargetRef};

/// What [`execute_rename`] needs from its plan.
#[derive(Debug)]
pub struct RenamePayload {
    from: String,
    to: String,
}

/// Bounded. Refuses an invalid `to` ([`CoreError::InvalidTargetName`]), `to == from`
/// ([`CoreError::SameTargetName`]) and a taken `to` ([`CoreError::TargetExists`]), in that order,
/// before anything is touched. Changes: `Rename Target <from>`; `Rename LocalState <from>` when
/// `state/<from>` exists; `SetDefault CliDefault <to>` when `from` is the default.
pub fn plan_rename(ctx: &Context, from: &TargetRef, to: &str) -> CoreResult<Plan<RenamePayload>> {
    validate_name(to).map_err(|problem| CoreError::InvalidTargetName {
        name: to.into(),
        problem,
    })?;
    let from_name = from.name();
    if from_name == to {
        return Err(CoreError::SameTargetName { name: to.into() });
    }
    let store = ctx.store();
    if store.target_dir(to).exists() {
        return Err(CoreError::TargetExists { name: to.into() });
    }
    let arrow = Some(format!("{from_name} → {to}"));
    let mut changes = vec![change(
        "Target",
        from_name,
        ChangeAction::Rename,
        arrow.clone(),
    )];
    if store.state_dir(from_name).exists() {
        changes.push(change(
            "LocalState",
            from_name,
            ChangeAction::Rename,
            arrow.clone(),
        ));
    }
    if cli_default(ctx)?.as_deref() == Some(from_name) {
        changes.push(change("CliDefault", to, ChangeAction::SetDefault, arrow));
    }
    Ok(Plan {
        class: PlanClass::Bounded,
        title: format!("Rename target {from_name} to {to}"),
        changes,
        payload: RenamePayload {
            from: from_name.into(),
            to: to.into(),
        },
    })
}

/// Under the lock: re-checks that `from` still exists and `to` is still free, renames the
/// target (and its state, by `cli_core::rename_target`), and repoints the default when it named
/// `from`. Reports what it actually did.
pub fn execute_rename(
    ctx: &Context,
    plan: Plan<RenamePayload>,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<Outcome<TargetRenamed>> {
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let RenamePayload { from, to } = plan.payload;
    let store = ctx.store();
    let _lock = lock_store_if_present(ctx, reporter)?;
    TargetRef::named(ctx, &from)?; // removed since the plan
    if store.target_dir(&to).exists() {
        return Err(CoreError::TargetExists { name: to });
    }
    let state_moved = store.state_dir(&from).exists();
    cli_core::rename_target(&store, &from, &to)?;
    let mut cli_default = None;
    if let Some(mut global) = cli_core::load_global_config(&store)? {
        if global.active_target == from {
            global.active_target = to.clone();
            cli_core::save_global_config(&store, &global)?;
            cli_default = Some(ActivePointerChange {
                from: Some(from.clone()),
                to: Some(to.clone()),
            });
        }
    }
    Ok(Outcome::Completed {
        result: TargetRenamed {
            from,
            to,
            state_moved,
            cli_default,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;
    use crate::NullReporter;

    #[test]
    fn rename_refuses_identical_invalid_and_taken_names_before_touching_the_store() {
        let (_d, ctx) = store(&["a", "b"], Some("a"));
        let a = TargetRef::named(&ctx, "a").unwrap();
        assert!(matches!(
            plan_rename(&ctx, &a, "a"),
            Err(CoreError::SameTargetName { .. })
        ));
        assert!(matches!(
            plan_rename(&ctx, &a, "../evil"),
            Err(CoreError::InvalidTargetName { .. })
        ));
        assert!(
            matches!(plan_rename(&ctx, &a, "b"), Err(CoreError::TargetExists { ref name }) if name == "b")
        );
    }

    #[test]
    fn renaming_the_default_moves_its_state_and_the_pointer() {
        let (_d, ctx) = store(&["a", "b"], Some("a"));
        seed_server(&ctx, "a", 1, "platform-1", None);
        let plan = plan_rename(&ctx, &TargetRef::named(&ctx, "a").unwrap(), "c").unwrap();
        assert_eq!(plan.class, PlanClass::Bounded);
        assert_eq!(
            plan.changes
                .iter()
                .map(|c| c.kind.as_str())
                .collect::<Vec<_>>(),
            ["Target", "LocalState", "CliDefault"]
        );
        let Outcome::Completed { result } =
            execute_rename(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap()
        else {
            panic!()
        };
        assert!(result.state_moved);
        assert_eq!(
            result.cli_default,
            Some(ActivePointerChange {
                from: Some("a".into()),
                to: Some("c".into())
            })
        );
        assert!(ctx.store().state_dir("c").exists() && !ctx.store().target_dir("a").exists());
    }

    /// Overview §3.7.3: a cancelled rename moves nothing — not the target, its state or the
    /// default.
    #[test]
    fn a_cancelled_rename_moves_nothing() {
        let (_d, ctx) = store(&["a"], Some("a"));
        seed_server(&ctx, "a", 1, "platform-1", None);
        let plan = plan_rename(&ctx, &TargetRef::named(&ctx, "a").unwrap(), "c").unwrap();
        let got = execute_rename(&ctx, plan, &NullReporter, &cancelled_token());
        assert!(matches!(got, Ok(Outcome::Cancelled { .. })), "{got:?}");
        let store = ctx.store();
        assert!(store.target_dir("a").exists() && store.state_dir("a").exists());
        assert!(!store.target_dir("c").exists() && !store.state_dir("c").exists());
        assert_eq!(cli_default(&ctx).unwrap().as_deref(), Some("a"));
    }

    #[test]
    fn a_destination_created_after_the_plan_is_refused_under_the_lock() {
        let (_d, ctx) = store(&["a"], Some("a"));
        let plan = plan_rename(&ctx, &TargetRef::named(&ctx, "a").unwrap(), "b").unwrap();
        edit_new(&ctx, "b"); // another process, meanwhile
        assert!(matches!(
            execute_rename(&ctx, plan, &NullReporter, &CancellationToken::new()),
            Err(CoreError::TargetExists { .. })
        ));
        assert!(ctx.store().target_dir("a").exists());
    }
}
