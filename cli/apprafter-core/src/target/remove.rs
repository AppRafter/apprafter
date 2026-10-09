// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `target remove`: forget a target on this computer (the server, if any, keeps running at the
//! provider), as plan and execute.

use cli_core::TargetStorePaths;

use crate::op::{ChangeAction, Outcome, Plan, PlanClass};
use crate::report::Reporter;
use crate::target::{
    cancelled, change, cli_default, lock_store_if_present, provisioned, TargetRemoved,
};
use crate::{ActivePointerChange, CancellationToken, Context, CoreResult, TargetRef};

/// What [`execute_remove`] needs from its plan.
#[derive(Debug)]
pub struct RemovePayload {
    name: String,
}

/// Destructive: `Delete Target`, `Delete Credentials`, `Delete LocalState` when `state/<name>`
/// exists (naming the server it records, which keeps running at the provider; a corrupt state
/// says so and does not block the remove), and — when `target` is the default — `SetDefault
/// CliDefault <next>` (the alphabetically first other target) or `ClearDefault CliDefault`.
pub fn plan_remove(ctx: &Context, target: &TargetRef) -> CoreResult<Plan<RemovePayload>> {
    let name = target.name();
    let store = ctx.store();
    let mut changes = vec![
        change("Target", name, ChangeAction::Delete, None),
        change("Credentials", name, ChangeAction::Delete, None),
    ];
    if store.state_dir(name).exists() {
        let detail = match provisioned(ctx, target) {
            Ok(Some(s)) => format!(
                "records server {} (id {}); the server keeps running at the provider",
                s.server_name, s.server_id
            ),
            Ok(None) => "cached state".to_string(),
            Err(e) => format!("cached state (unreadable: {e})"),
        };
        changes.push(change(
            "LocalState",
            name,
            ChangeAction::Delete,
            Some(detail),
        ));
    }
    if cli_default(ctx)?.as_deref() == Some(name) {
        changes.push(match next_default(&store, name)? {
            Some(next) => change(
                "CliDefault",
                &next,
                ChangeAction::SetDefault,
                Some(format!("{name} → {next}")),
            ),
            None => change("CliDefault", name, ChangeAction::ClearDefault, None),
        });
    }
    Ok(Plan {
        class: PlanClass::Destructive,
        title: format!("Remove target {name} from this computer"),
        changes,
        payload: RemovePayload { name: name.into() },
    })
}

/// The alphabetically first target other than `removing`.
fn next_default(store: &TargetStorePaths, removing: &str) -> CoreResult<Option<String>> {
    Ok(cli_core::list_target_names(store)?
        .into_iter()
        .find(|n| n != removing))
}

/// Under the lock: re-check, remove `targets/<name>/` and `state/<name>/`, repoint the default to
/// the alphabetically first remaining target or delete `config.yaml`. Reports what it actually
/// did, including the server it leaves running.
pub fn execute_remove(
    ctx: &Context,
    plan: Plan<RemovePayload>,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<Outcome<TargetRemoved>> {
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let name = plan.payload.name;
    let store = ctx.store();
    let _lock = lock_store_if_present(ctx, reporter)?;
    let target = TargetRef::named(ctx, &name)?;
    let orphaned_server = provisioned(ctx, &target).ok().flatten(); // unreadable: the plan said so
    let state_removed = store.state_dir(&name).exists();
    cli_core::remove_target(&store, &name)?;
    let cli_default = if cli_default(ctx)?.as_deref() == Some(name.as_str()) {
        match next_default(&store, &name)? {
            Some(next) => {
                let mut global = cli_core::load_global_config(&store)?.unwrap_or_default();
                global.active_target = next.clone();
                cli_core::save_global_config(&store, &global)?;
                Some(ActivePointerChange {
                    from: Some(name.clone()),
                    to: Some(next),
                })
            }
            None => {
                let file = store.global_config_file();
                if file.exists() {
                    std::fs::remove_file(&file).map_err(cli_core::CliError::from)?;
                }
                Some(ActivePointerChange {
                    from: Some(name.clone()),
                    to: None,
                })
            }
        }
    } else {
        None
    };
    Ok(Outcome::Completed {
        result: TargetRemoved {
            name,
            state_removed,
            orphaned_server,
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
    fn the_remove_plan_is_destructive_and_names_a_recorded_server() {
        let (_d, ctx) = store(&["prod", "staging"], Some("prod"));
        seed_server(&ctx, "prod", 42, "platform-1", Some("cx22"));
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        assert_eq!(plan.class, PlanClass::Destructive);
        let state = plan
            .changes
            .iter()
            .find(|c| c.kind == "LocalState")
            .unwrap();
        assert_eq!(
            state.detail.as_deref(),
            Some("records server platform-1 (id 42); the server keeps running at the provider")
        );
        let ptr = plan
            .changes
            .iter()
            .find(|c| c.kind == "CliDefault")
            .unwrap();
        assert_eq!(
            (ptr.action, ptr.detail.as_deref()),
            (ChangeAction::SetDefault, Some("prod → staging"))
        );
    }

    #[test]
    fn removing_the_default_repoints_alphabetically_and_reports_the_orphan() {
        let (_d, ctx) = store(&["b", "c", "a"], Some("b"));
        seed_server(&ctx, "b", 9, "platform-9", None);
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "b").unwrap()).unwrap();
        let Outcome::Completed { result } =
            execute_remove(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap()
        else {
            panic!()
        };
        assert_eq!(result.orphaned_server.map(|s| s.server_id), Some(9));
        assert!(result.state_removed);
        assert_eq!(
            result.cli_default,
            Some(ActivePointerChange {
                from: Some("b".into()),
                to: Some("a".into())
            })
        );
    }

    /// The server is read again under the lock: one recorded after the plan (an `apply` in
    /// another terminal writes the state without the lock) is still reported as left running.
    #[test]
    fn a_server_recorded_after_the_plan_is_reported() {
        let (_d, ctx) = store(&["prod", "x"], Some("x"));
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        assert!(plan.changes.iter().all(|c| c.kind != "LocalState"));
        seed_server(&ctx, "prod", 42, "platform-1", None);
        let Outcome::Completed { result } =
            execute_remove(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap()
        else {
            panic!()
        };
        assert_eq!(result.orphaned_server.map(|s| s.server_id), Some(42));
        assert!(result.state_removed);
    }

    #[test]
    fn removing_the_last_target_deletes_the_pointer_file() {
        let (_d, ctx) = store(&["only"], Some("only"));
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "only").unwrap()).unwrap();
        assert_eq!(
            plan.changes.last().unwrap().action,
            ChangeAction::ClearDefault
        );
        let Outcome::Completed { result } =
            execute_remove(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            result.cli_default,
            Some(ActivePointerChange {
                from: Some("only".into()),
                to: None
            })
        );
        assert!(!ctx.store().global_config_file().exists());
    }

    #[test]
    fn a_target_named_default_on_a_store_without_a_pointer_is_not_the_default() {
        // R1
        let (_d, ctx) = store(&["default", "x"], None);
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "default").unwrap()).unwrap();
        assert!(plan.changes.iter().all(|c| c.kind != "CliDefault"));
        let Outcome::Completed { result } =
            execute_remove(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap()
        else {
            panic!()
        };
        assert_eq!(result.cli_default, None);
        assert!(!ctx.store().global_config_file().exists());
    }

    /// Overview §3.7.3: a cancelled remove deletes nothing and keeps the default.
    #[test]
    fn a_cancelled_remove_deletes_nothing() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        seed_server(&ctx, "prod", 42, "platform-1", None);
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        let got = execute_remove(&ctx, plan, &NullReporter, &cancelled_token());
        assert!(matches!(got, Ok(Outcome::Cancelled { .. })), "{got:?}");
        let store = ctx.store();
        assert!(store.target_dir("prod").exists() && store.state_dir("prod").exists());
        assert_eq!(cli_default(&ctx).unwrap().as_deref(), Some("prod"));
    }

    #[test]
    fn a_corrupt_state_does_not_block_the_remove_and_the_plan_says_so() {
        let (_d, ctx) = store(&["prod"], None);
        seed_state_raw(&ctx, "prod", "{");
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        assert!(plan.changes.iter().any(|c| c
            .detail
            .as_deref()
            .is_some_and(|d| d.starts_with("cached state (unreadable:"))));
        assert!(matches!(
            execute_remove(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap(),
            Outcome::Completed { .. }
        ));
    }
}
