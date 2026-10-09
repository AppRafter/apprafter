// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `target use`: make a target the CLI default, as plan and execute.

use crate::op::{ChangeAction, Outcome, Plan, PlanClass};
use crate::report::Reporter;
use crate::target::{cancelled, change, cli_default, lock_store_if_present, TargetUsed};
use crate::{ActivePointerChange, CancellationToken, Context, CoreResult, TargetRef};

/// What [`execute_use`] needs from its plan.
#[derive(Debug)]
pub struct UsePayload {
    name: String,
}

/// Reversible: `SetDefault CliDefault <name>` ("a → b", or "none → b" when `config.yaml` is
/// absent — R1); no change when `target` already is the default.
pub fn plan_use(ctx: &Context, target: &TargetRef) -> CoreResult<Plan<UsePayload>> {
    let name = target.name().to_string();
    let current = cli_default(ctx)?;
    let changes = if current.as_deref() == Some(name.as_str()) {
        Vec::new()
    } else {
        vec![change(
            "CliDefault",
            &name,
            ChangeAction::SetDefault,
            Some(format!("{} → {name}", current.as_deref().unwrap_or("none"))),
        )]
    };
    Ok(Plan {
        class: PlanClass::Reversible,
        title: format!("Make {name} the CLI default"),
        changes,
        payload: UsePayload { name },
    })
}

/// Re-reads the pointer under the lock; writes `config.yaml` only when it moves. The CLI calls
/// it even for an empty plan, so a read-only store still reports its lock warning.
pub fn execute_use(
    ctx: &Context,
    plan: Plan<UsePayload>,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<Outcome<TargetUsed>> {
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let name = plan.payload.name;
    let _lock = lock_store_if_present(ctx, reporter)?;
    TargetRef::named(ctx, &name)?;
    let from = cli_default(ctx)?;
    if from.as_deref() == Some(name.as_str()) {
        return Ok(Outcome::Completed {
            result: TargetUsed {
                name,
                pointer: None,
            },
        });
    }
    let store = ctx.store();
    let mut global = cli_core::load_global_config(&store)?.unwrap_or_default();
    global.active_target = name.clone();
    cli_core::save_global_config(&store, &global)?;
    Ok(Outcome::Completed {
        result: TargetUsed {
            pointer: Some(ActivePointerChange {
                from,
                to: Some(name.clone()),
            }),
            name,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;
    use crate::{CoreError, NullReporter};

    #[test]
    fn use_without_a_config_yaml_moves_the_pointer_from_none() {
        // R1
        let (_d, ctx) = store(&["default", "prod"], None);
        let plan = plan_use(&ctx, &TargetRef::named(&ctx, "default").unwrap()).unwrap();
        assert_eq!(plan.class, PlanClass::Reversible);
        assert_eq!(plan.changes[0].detail.as_deref(), Some("none → default"));
        let Outcome::Completed { result } =
            execute_use(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            result.pointer,
            Some(ActivePointerChange {
                from: None,
                to: Some("default".into())
            })
        );
        assert_eq!(cli_default(&ctx).unwrap().as_deref(), Some("default"));
    }

    #[test]
    fn use_of_the_current_default_plans_nothing_and_writes_nothing() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        let before = std::fs::metadata(ctx.store().global_config_file())
            .unwrap()
            .modified()
            .unwrap();
        let plan = plan_use(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        assert!(plan.changes.is_empty());
        let Outcome::Completed { result } =
            execute_use(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap()
        else {
            panic!()
        };
        assert_eq!(result.pointer, None);
        assert_eq!(
            std::fs::metadata(ctx.store().global_config_file())
                .unwrap()
                .modified()
                .unwrap(),
            before
        );
    }

    /// Overview §3.7.3: a cancelled `use` moves nothing.
    #[test]
    fn a_cancelled_use_keeps_the_default() {
        let (_d, ctx) = store(&["a", "b"], Some("a"));
        let plan = plan_use(&ctx, &TargetRef::named(&ctx, "b").unwrap()).unwrap();
        let got = execute_use(&ctx, plan, &NullReporter, &cancelled_token());
        assert!(matches!(got, Ok(Outcome::Cancelled { .. })), "{got:?}");
        assert_eq!(cli_default(&ctx).unwrap().as_deref(), Some("a"));
    }

    #[test]
    fn a_target_removed_after_the_plan_is_not_made_default() {
        let (_d, ctx) = store(&["a", "b"], Some("a"));
        let plan = plan_use(&ctx, &TargetRef::named(&ctx, "b").unwrap()).unwrap();
        cli_core::remove_target(&ctx.store(), "b").unwrap();
        assert!(matches!(
            execute_use(&ctx, plan, &NullReporter, &CancellationToken::new()),
            Err(CoreError::TargetNotFound { .. })
        ));
        assert_eq!(cli_default(&ctx).unwrap().as_deref(), Some("a"));
    }
}
