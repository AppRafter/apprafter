// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `target remove`: forget a target on this computer (the server, if any, keeps running at the
//! provider), as plan and execute.

use cli_core::{CliError, TargetStorePaths};

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
/// CliDefault <next>` (the alphabetically first other target that can be read) or `ClearDefault
/// CliDefault`.
///
/// A target whose files cannot be read is removed too (WI-458): its `Target` and `Credentials`
/// lines name the file that cannot be read, and its `LocalState` line says that a server the
/// state records cannot be checked or destroyed from here. A readable target's lines are as they
/// always were.
pub fn plan_remove(ctx: &Context, target: &TargetRef) -> CoreResult<Plan<RemovePayload>> {
    let name = target.name();
    let store = ctx.store();
    let files = TargetFiles::read(&store, name);
    let mut changes = vec![
        change("Target", name, ChangeAction::Delete, files.config.clone()),
        change(
            "Credentials",
            name,
            ChangeAction::Delete,
            files.credentials.clone(),
        ),
    ];
    if store.state_dir(name).exists() {
        let unchecked = "it cannot be checked or destroyed from here while the target's files \
                         cannot be read";
        let detail = match provisioned(ctx, target) {
            Ok(Some(s)) if files.readable() => format!(
                "records server {} (id {}); the server keeps running at the provider",
                s.server_name, s.server_id
            ),
            Ok(Some(s)) => format!(
                "records server {} (id {}); the server keeps running at the provider, and \
                 {unchecked}",
                s.server_name, s.server_id
            ),
            Ok(None) => "cached state".to_string(),
            Err(e) if files.readable() => format!("cached state (unreadable: {e})"),
            Err(e) => format!(
                "cached state (unreadable: {e}); a server it records keeps running at the \
                 provider, and {unchecked}"
            ),
        };
        changes.push(change(
            "LocalState",
            name,
            ChangeAction::Delete,
            Some(detail),
        ));
    }
    if cli_default(ctx)?.as_deref() == Some(name) {
        let next = next_default(&store, name)?;
        let skipped = next.skipped.join(", ");
        changes.push(match &next.to {
            Some(to) if next.skipped.is_empty() => change(
                "CliDefault",
                to,
                ChangeAction::SetDefault,
                Some(format!("{name} → {to}")),
            ),
            Some(to) => change(
                "CliDefault",
                to,
                ChangeAction::SetDefault,
                Some(format!(
                    "{name} → {to}, passing over {skipped}, which cannot be read"
                )),
            ),
            None => change(
                "CliDefault",
                name,
                ChangeAction::ClearDefault,
                (!next.skipped.is_empty())
                    .then(|| format!("no readable target left: {skipped} cannot be read")),
            ),
        });
    }
    Ok(Plan {
        class: PlanClass::Destructive,
        title: format!("Remove target {name} from this computer"),
        changes,
        payload: RemovePayload { name: name.into() },
    })
}

/// What stops each of a target's two files from being read, as `cli_core::load_target` reads
/// them; both `None` for a target it loads. Never a credentials file's text: its parse error
/// gives only where it failed (the text is the token).
struct TargetFiles {
    config: Option<String>,
    credentials: Option<String>,
}

impl TargetFiles {
    fn read(store: &TargetStorePaths, name: &str) -> Self {
        let config = match cli_core::target::load_target_config(store, name) {
            Ok(_) => None,
            // The directory is listed (`TargetRef::named`) without its config.yaml; one this
            // user cannot search is an I/O error, never missing (WI-458 review #3).
            Err(CliError::TargetNotFound { .. }) => Some("config.yaml is missing".to_string()),
            Err(e) => Some(format!("config.yaml cannot be read: {}", reason(&e))),
        };
        let credentials = cli_core::target::load_target_credentials(store, name)
            .err()
            .map(|e| format!("credentials.yaml cannot be read: {}", reason(&e)));
        Self {
            config,
            credentials,
        }
    }

    fn readable(&self) -> bool {
        self.config.is_none() && self.credentials.is_none()
    }
}

/// Why a target file cannot be read, without its path (the plan line names the file).
fn reason(e: &CliError) -> String {
    match e {
        CliError::InvalidTargetConfig { message, .. } => message.clone(),
        other => other.to_string(),
    }
}

/// Where the CLI default goes when the target it names is removed.
struct NextDefault {
    /// The alphabetically first other target that can be read; `None`: none can.
    to: Option<String>,
    /// The targets before it, alphabetically, that cannot be read.
    skipped: Vec<String>,
}

/// The alphabetically first target other than `removing` that `cli_core::load_target` loads —
/// both files, as every command that falls back on the default reads them — and those passed
/// over on the way (WI-458: a default that cannot be read fails every command that names no
/// target).
fn next_default(store: &TargetStorePaths, removing: &str) -> CoreResult<NextDefault> {
    let mut skipped = Vec::new();
    for n in cli_core::list_target_names(store)? {
        if n == removing {
            continue;
        }
        if cli_core::load_target(store, &n).is_ok() {
            return Ok(NextDefault {
                to: Some(n),
                skipped,
            });
        }
        skipped.push(n);
    }
    Ok(NextDefault { to: None, skipped })
}

/// Under the lock: re-check, remove `targets/<name>/` and `state/<name>/`, repoint the default to
/// the alphabetically first remaining target that can be read or delete `config.yaml`. Reports
/// what it actually did, including the server it leaves running and the targets the default
/// passed over. A target whose files cannot be read takes the same steps: nothing here reads
/// them.
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
    let mut skipped_unreadable = Vec::new();
    let cli_default = if cli_default(ctx)?.as_deref() == Some(name.as_str()) {
        let next = next_default(&store, &name)?;
        skipped_unreadable = next.skipped;
        match next.to {
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
                    std::fs::remove_file(&file).map_err(CliError::from)?;
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
            skipped_unreadable,
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
        // No target left at all: nothing was passed over, and the line says no more than that.
        assert_eq!(
            plan.changes.last().unwrap(),
            &change("CliDefault", "only", ChangeAction::ClearDefault, None)
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

    /// The detail of the plan's `kind` line.
    fn detail<'a>(plan: &'a Plan<RemovePayload>, kind: &str) -> Option<&'a str> {
        plan.changes
            .iter()
            .find(|c| c.kind == kind)
            .unwrap_or_else(|| panic!("no {kind} line in {:?}", plan.changes))
            .detail
            .as_deref()
    }

    /// WI-458: a target whose `config.yaml` cannot be read is removed like any other. The plan
    /// names the file it cannot read, and says the server its state records keeps running and
    /// cannot be checked or destroyed from here; the removal deletes everything and reports
    /// the server.
    #[test]
    fn an_unreadable_target_is_removed_and_the_plan_says_what_cannot_be_checked() {
        let (_d, ctx) = store(&["prod", "staging"], Some("staging"));
        std::fs::write(ctx.store().target_config_file("prod"), "provider: [").unwrap();
        seed_server(&ctx, "prod", 42, "platform-1", None);
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        assert_eq!(plan.class, PlanClass::Destructive);
        let target = detail(&plan, "Target").unwrap();
        assert!(
            target.starts_with("config.yaml cannot be read: "),
            "{target}"
        );
        assert_eq!(detail(&plan, "Credentials"), None);
        assert_eq!(
            detail(&plan, "LocalState"),
            Some(
                "records server platform-1 (id 42); the server keeps running at the provider, \
                 and it cannot be checked or destroyed from here while the target's files \
                 cannot be read"
            )
        );
        let Outcome::Completed { result } =
            execute_remove(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap()
        else {
            panic!()
        };
        assert_eq!(result.orphaned_server.map(|s| s.server_id), Some(42));
        assert!(result.state_removed);
        let store = ctx.store();
        assert!(!store.target_dir("prod").exists() && !store.state_dir("prod").exists());
        assert_eq!(cli_default(&ctx).unwrap().as_deref(), Some("staging"));
    }

    /// A good `config.yaml` beside a `credentials.yaml` that cannot be read is not readable:
    /// the plan names the credentials (never quoting them: their text is the token), and the
    /// server line says what the readable remove's cannot.
    #[test]
    fn a_target_whose_credentials_cannot_be_read_takes_the_unreadable_path() {
        let (_d, ctx) = store(&["prod"], None);
        std::fs::write(
            ctx.store().target_credentials_file("prod"),
            format!("hetzner_token:{TOKEN_A}\n"),
        )
        .unwrap();
        seed_server(&ctx, "prod", 42, "platform-1", None);
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        assert_eq!(detail(&plan, "Target"), None);
        let creds = detail(&plan, "Credentials").unwrap();
        assert!(
            creds
                .starts_with("credentials.yaml cannot be read: not a valid target credentials map"),
            "{creds}"
        );
        assert!(!format!("{:?}", plan.changes).contains(TOKEN_A));
        assert!(detail(&plan, "LocalState").unwrap().ends_with(
            "cannot be checked or destroyed from here while the target's files cannot be read"
        ));
        assert!(matches!(
            execute_remove(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap(),
            Outcome::Completed { .. }
        ));
        assert!(!ctx.store().target_dir("prod").exists());
    }

    /// A target directory without its `config.yaml` is listed, so it can be removed too.
    #[test]
    fn a_target_without_its_config_is_removed_and_the_plan_says_it_is_missing() {
        let (_d, ctx) = store(&["prod"], None);
        std::fs::remove_file(ctx.store().target_config_file("prod")).unwrap();
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        assert_eq!(detail(&plan, "Target"), Some("config.yaml is missing"));
        assert!(matches!(
            execute_remove(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap(),
            Outcome::Completed { .. }
        ));
        assert!(!ctx.store().target_dir("prod").exists());
    }

    /// WI-458 review #3: a target directory this user cannot search is not a target without its
    /// config.yaml. The plan says that each file cannot be read, and why (a 000 directory: the
    /// tests run as a user its mode binds).
    #[cfg(unix)]
    #[test]
    fn a_target_directory_that_cannot_be_searched_is_unreadable_never_missing() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, ctx) = store(&["prod"], None);
        let dir = ctx.store().target_dir("prod");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let plan = TargetRef::named(&ctx, "prod").and_then(|t| plan_remove(&ctx, &t));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let plan = plan.unwrap();
        for (kind, file) in [
            ("Target", "config.yaml"),
            ("Credentials", "credentials.yaml"),
        ] {
            let line = detail(&plan, kind).unwrap();
            assert!(
                line.starts_with(&format!("{file} cannot be read: io error: "))
                    && line.contains("Permission denied"),
                "{line}"
            );
        }
    }

    /// An unreadable target whose state cannot be read either: the plan says a server it might
    /// record cannot be checked.
    #[test]
    fn an_unreadable_target_with_a_corrupt_state_says_a_server_cannot_be_checked() {
        let (_d, ctx) = store(&["prod"], None);
        std::fs::write(ctx.store().target_config_file("prod"), "provider: [").unwrap();
        seed_state_raw(&ctx, "prod", "{");
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        let state = detail(&plan, "LocalState").unwrap();
        assert!(state.starts_with("cached state (unreadable: "), "{state}");
        assert!(
            state.ends_with(
                "a server it records keeps running at the provider, and it cannot be checked or \
                 destroyed from here while the target's files cannot be read"
            ),
            "{state}"
        );
    }

    /// WI-458 (from D.3d): the default moves to the alphabetically first target that can be
    /// read — both files, as `load_target` reads them — and the plan names those passed over.
    #[test]
    fn the_next_default_is_the_first_readable_target_and_the_plan_says_who_was_passed_over() {
        let (_d, ctx) = store(&["alpha", "beta", "prod", "staging"], Some("prod"));
        std::fs::write(ctx.store().target_config_file("alpha"), "provider: [").unwrap();
        std::fs::write(
            ctx.store().target_credentials_file("beta"),
            "hetzner_token: [",
        )
        .unwrap();
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        let ptr = plan.changes.last().unwrap();
        assert_eq!(
            (
                ptr.kind.as_str(),
                ptr.object.as_str(),
                ptr.action,
                ptr.detail.as_deref()
            ),
            (
                "CliDefault",
                "staging",
                ChangeAction::SetDefault,
                Some("prod → staging, passing over alpha, beta, which cannot be read")
            )
        );
        let Outcome::Completed { result } =
            execute_remove(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            result.cli_default,
            Some(ActivePointerChange {
                from: Some("prod".into()),
                to: Some("staging".into())
            })
        );
        assert_eq!(result.skipped_unreadable, ["alpha", "beta"]);
        assert_eq!(cli_default(&ctx).unwrap().as_deref(), Some("staging"));
    }

    /// The store the desktop's mock IPC holds (desktop/src/ipc/mock/fixtures.ts): removing its
    /// default, prod-eu, moves the default to lab, passing over broken. The mock's test asserts
    /// the same line (desktop/src/ipc/mock/targets.test.ts).
    #[test]
    fn the_mock_store_default_moves_past_broken_to_lab() {
        let (_d, ctx) = store(&["broken", "lab", "prod-eu", "staging"], Some("prod-eu"));
        std::fs::write(ctx.store().target_config_file("broken"), "- a\n").unwrap();
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "prod-eu").unwrap()).unwrap();
        assert_eq!(
            detail(&plan, "CliDefault"),
            Some("prod-eu → lab, passing over broken, which cannot be read")
        );
    }

    /// With no readable target left the default is cleared, and the plan line says why.
    #[test]
    fn with_no_readable_target_left_the_default_is_cleared_and_the_plan_says_why() {
        let (_d, ctx) = store(&["broken", "prod"], Some("prod"));
        std::fs::write(ctx.store().target_config_file("broken"), "provider: [").unwrap();
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        let ptr = plan.changes.last().unwrap();
        assert_eq!(
            (ptr.action, ptr.detail.as_deref()),
            (
                ChangeAction::ClearDefault,
                Some("no readable target left: broken cannot be read")
            )
        );
        let Outcome::Completed { result } =
            execute_remove(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            result.cli_default,
            Some(ActivePointerChange {
                from: Some("prod".into()),
                to: None
            })
        );
        assert_eq!(result.skipped_unreadable, ["broken"]);
        assert!(!ctx.store().global_config_file().exists());
        assert!(ctx.store().target_dir("broken").exists());
    }

    /// The readable remove's lines are as they were: no detail on the files, and a default that
    /// passes over nothing says only where it went; a default that did not move skips no one.
    #[test]
    fn the_readable_remove_says_what_it_always_said() {
        let (_d, ctx) = store(&["broken", "prod", "staging"], Some("staging"));
        std::fs::write(ctx.store().target_config_file("broken"), "provider: [").unwrap();
        let plan = plan_remove(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        assert_eq!(
            plan.changes,
            vec![
                change("Target", "prod", ChangeAction::Delete, None),
                change("Credentials", "prod", ChangeAction::Delete, None),
            ]
        );
        let Outcome::Completed { result } =
            execute_remove(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap()
        else {
            panic!()
        };
        assert!(result.skipped_unreadable.is_empty() && result.cli_default.is_none());
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
