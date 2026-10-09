// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `target add` (a new target, or today's `--force` overwrite) and `target add --renew` (rotate
//! a target's token), as plan and execute.

use std::path::{Path, PathBuf};

use cli_core::{GlobalConfig, Target, TargetConfig, TargetCredentials, TargetStorePaths};

use crate::context::SecretString;
use crate::op::{ChangeAction, Outcome, Plan, PlanClass};
use crate::provider::{SkipReason, Verification};
use crate::report::Reporter;
use crate::target::{
    cancelled, change, cli_default, lock_store, lock_store_if_present, validate_name, SkuCheck,
    TargetAdded, TargetRenewed,
};
use crate::{ActivePointerChange, CancellationToken, Context, CoreError, CoreResult, TargetRef};

/// What `target add` asks for (overview §3.7.3). `Debug` only, never `Serialize`: the token is a
/// [`SecretString`], whose `Debug` never shows the value.
#[derive(Debug)]
pub struct AddArgs {
    pub name: String,
    pub provider: String,
    pub token: SecretString,
    pub ssh_key: Option<PathBuf>,
    pub region: Option<String>,
    pub tier: Option<String>,
    pub cluster_name: Option<String>,
    pub server_type: Option<String>,
    /// Replace a target of the same name (the CLI's `--force`; the GUI never offers it).
    pub force: bool,
}

/// What `target add --renew` asks for (overview §3.7.3): a new token, and optionally a new SSH
/// key path.
#[derive(Debug)]
pub struct RenewArgs {
    pub token: SecretString,
    pub ssh_key: Option<PathBuf>,
}

/// What [`execute_add`] needs from its plan.
#[derive(Debug)]
pub struct AddPayload {
    args: AddArgs,
}

/// What [`execute_renew`] needs from its plan.
#[derive(Debug)]
pub struct RenewPayload {
    name: String,
    provider: String,
    token: SecretString,
    ssh_key: Option<PathBuf>,
}

/// The stored `config.yaml` of `name`, or `None` when there is no such target. Both of the
/// target's files are read, as the CLI's name check always read them: a target whose
/// credentials file cannot be read is that error, `force` or not — never "exists", and never
/// overwritten. (The token itself is dropped here.)
fn stored_config(store: &TargetStorePaths, name: &str) -> CoreResult<Option<TargetConfig>> {
    match cli_core::load_target(store, name) {
        Ok(t) => Ok(Some(t.config)),
        Err(cli_core::CliError::TargetNotFound { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Local checks only (R2), in today's order: name, provider, token format, SSH key, name free
/// (unless `force`). A new name is Bounded (`Create Target`, `Create Credentials`, and
/// `SetDefault CliDefault` when `config.yaml` is absent); a forced overwrite is Destructive
/// (`Replace Target`, `Replace Credentials`).
pub fn plan_add(ctx: &Context, args: AddArgs) -> CoreResult<Plan<AddPayload>> {
    validate_name(&args.name).map_err(|problem| CoreError::InvalidTargetName {
        name: args.name.clone(),
        problem,
    })?;
    crate::provider::require_supported(&args.provider)?;
    crate::provider::TokenProblem::check(args.token.expose())
        .map_err(|problem| CoreError::InvalidToken { problem })?;
    if let Some(p) = &args.ssh_key {
        crate::ssh::check_readable(p)?;
    }
    let store = ctx.store();
    let stored = stored_config(&store, &args.name)?;
    if stored.is_some() && !args.force {
        return Err(CoreError::TargetExists { name: args.name });
    }
    let n = args.name.clone();
    let (class, title, mut changes) = match &stored {
        None => (
            PlanClass::Bounded,
            format!("Add target {n}"),
            vec![change(
                "Target",
                &n,
                ChangeAction::Create,
                Some(create_detail(ctx, &args)),
            )],
        ),
        Some(_) => (
            PlanClass::Destructive,
            format!("Overwrite target {n}"),
            vec![change("Target", &n, ChangeAction::Replace, None)],
        ),
    };
    changes.push(change(
        "Credentials",
        &n,
        if stored.is_some() {
            ChangeAction::Replace
        } else {
            ChangeAction::Create
        },
        Some("API token".into()),
    ));
    if cli_core::load_global_config(&store)?.is_none() {
        changes.push(change(
            "CliDefault",
            &n,
            ChangeAction::SetDefault,
            Some(format!("none → {n}")),
        ));
    }
    Ok(Plan {
        class,
        title,
        changes,
        payload: AddPayload { args },
    })
}

/// "provider hetzner-cloud, region nbg1, tier solo, server type cx22, ssh key ~/.ssh/id.pub" —
/// the fields that are set, in that order.
fn create_detail(ctx: &Context, a: &AddArgs) -> String {
    let mut parts = vec![format!("provider {}", a.provider)];
    if let Some(v) = &a.region {
        parts.push(format!("region {v}"));
    }
    if let Some(v) = &a.tier {
        parts.push(format!("tier {v}"));
    }
    if let Some(v) = &a.cluster_name {
        parts.push(format!("cluster name {v}"));
    }
    if let Some(v) = &a.server_type {
        parts.push(format!("server type {v}"));
    }
    if let Some(p) = &a.ssh_key {
        parts.push(format!(
            "ssh key {}",
            cli_core::paths::abbreviate_home(p, ctx.home_dir())
        ));
    }
    parts.join(", ")
}

/// Ping (unless `no_ping`; even after a wizard verify — R2), the SKU check (server types only,
/// in the flag's region or [`crate::machine::DEFAULT_REGION`]), then, under the lock (always
/// taken, creating the root — this plan's deviation 1), re-check the name and save. The pointer
/// moves only when `config.yaml` is absent; the outcome reports what happened. Today's
/// `--force`: the target is rebuilt from the arguments.
pub fn execute_add(
    ctx: &Context,
    plan: Plan<AddPayload>,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<Outcome<TargetAdded>> {
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let a = plan.payload.args;
    let token = if ctx.no_ping() {
        Verification::Skipped {
            reason: SkipReason::NoPing,
        }
    } else {
        let d = crate::provider::ping(ctx, &a.provider, &a.token, cancel)?;
        Verification::Verified {
            elapsed_ms: d.as_millis() as u64,
        }
    };
    let sku = match &a.server_type {
        None => None,
        Some(sku) if ctx.no_ping() => Some(SkuCheck::NotValidated { sku: sku.clone() }),
        Some(sku) => {
            let (region, region_was_default) = match &a.region {
                Some(r) => (r.clone(), false),
                None => (crate::machine::DEFAULT_REGION.to_string(), true),
            };
            crate::machine::check_sku(ctx, &a.token, sku, &region, cancel)?;
            Some(SkuCheck::Validated {
                sku: sku.clone(),
                region,
                region_was_default,
            })
        }
    };
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let store = ctx.store();
    let _lock = lock_store(ctx, reporter)?;
    let replaced = stored_config(&store, &a.name)?.is_some();
    if replaced && !a.force {
        return Err(CoreError::TargetExists { name: a.name });
    }
    // Today's `--force`: rebuilt from the arguments (this plan's Task 25 makes it a merge).
    let config = TargetConfig {
        provider: a.provider,
        region: a.region,
        default_tier: a.tier,
        cluster_name: a.cluster_name,
        ssh_key_path: a.ssh_key,
        firewall: None,
        server_type: a.server_type,
    };
    cli_core::save_target(
        &store,
        &Target {
            name: a.name.clone(),
            config,
            credentials: TargetCredentials {
                hetzner_token: Some(a.token.expose().to_string()),
            },
        },
    )?;
    let pointer = if cli_core::load_global_config(&store)?.is_none() {
        cli_core::save_global_config(
            &store,
            &GlobalConfig {
                active_target: a.name.clone(),
                version: cli_core::TARGET_STORE_VERSION,
            },
        )?;
        Some(ActivePointerChange {
            from: None,
            to: Some(a.name.clone()),
        })
    } else {
        None
    };
    let is_cli_default = cli_default(ctx)?.as_deref() == Some(a.name.as_str());
    Ok(Outcome::Completed {
        result: TargetAdded {
            name: a.name,
            replaced,
            is_cli_default,
            cli_default: pointer,
            token,
            sku,
        },
    })
}

/// Bounded. Local checks, in today's order: the target exists, the token's format, the token
/// differs from the stored one ([`CoreError::RenewTokenUnchanged`]), the SSH key is readable.
/// Changes: `Replace Credentials` ("API token"), and `Update Target` ("ssh key: a → b") when a
/// different key path is given.
pub fn plan_renew(
    ctx: &Context,
    target: &TargetRef,
    args: RenewArgs,
) -> CoreResult<Plan<RenewPayload>> {
    let name = target.name();
    let t = cli_core::load_target(&ctx.store(), name)?;
    crate::provider::TokenProblem::check(args.token.expose())
        .map_err(|problem| CoreError::InvalidToken { problem })?;
    if t.credentials.hetzner_token.as_deref() == Some(args.token.expose()) {
        return Err(CoreError::RenewTokenUnchanged { name: name.into() });
    }
    if let Some(p) = &args.ssh_key {
        crate::ssh::check_readable(p)?;
    }
    let mut changes = vec![change(
        "Credentials",
        name,
        ChangeAction::Replace,
        Some("API token".into()),
    )];
    if let Some(p) = args
        .ssh_key
        .as_deref()
        .filter(|p| t.config.ssh_key_path.as_deref() != Some(*p))
    {
        let show = |p: &Path| cli_core::paths::abbreviate_home(p, ctx.home_dir());
        changes.push(change(
            "Target",
            name,
            ChangeAction::Update,
            Some(format!(
                "ssh key: {} → {}",
                t.config
                    .ssh_key_path
                    .as_deref()
                    .map(show)
                    .unwrap_or_else(|| "not set".into()),
                show(p)
            )),
        ));
    }
    Ok(Plan {
        class: PlanClass::Bounded,
        title: format!("Rotate the API token of {name}"),
        changes,
        payload: RenewPayload {
            name: name.into(),
            provider: t.config.provider,
            token: args.token,
            ssh_key: args.ssh_key,
        },
    })
}

/// Ping the new token (unless `no_ping`), then under the lock re-read, re-check (the target
/// exists, the token is still a new one) and patch only the credentials (+ the key path): an
/// edit made during the ping is kept.
pub fn execute_renew(
    ctx: &Context,
    plan: Plan<RenewPayload>,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<Outcome<TargetRenewed>> {
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let RenewPayload {
        name,
        provider,
        token,
        ssh_key,
    } = plan.payload;
    let verification = if ctx.no_ping() {
        Verification::Skipped {
            reason: SkipReason::NoPing,
        }
    } else {
        Verification::Verified {
            elapsed_ms: crate::provider::ping(ctx, &provider, &token, cancel)?.as_millis() as u64,
        }
    };
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let store = ctx.store();
    let _lock = lock_store_if_present(ctx, reporter)?;
    let mut t = cli_core::load_target(&store, &name)?;
    if t.credentials.hetzner_token.as_deref() == Some(token.expose()) {
        return Err(CoreError::RenewTokenUnchanged { name });
    }
    let ssh_key_changed = ssh_key
        .as_ref()
        .is_some_and(|p| t.config.ssh_key_path.as_ref() != Some(p));
    if let Some(p) = ssh_key {
        t.config.ssh_key_path = Some(p);
    }
    t.credentials = TargetCredentials {
        hetzner_token: Some(token.expose().to_string()),
    };
    cli_core::save_target(&store, &t)?;
    Ok(Outcome::Completed {
        result: TargetRenewed {
            name,
            token: verification,
            ssh_key_changed,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;
    use crate::error::UiError;
    use crate::NullReporter;

    fn args(name: &str, token: &str) -> AddArgs {
        AddArgs {
            name: name.into(),
            provider: "hetzner-cloud".into(),
            token: SecretString::new(token),
            ssh_key: None,
            region: None,
            tier: None,
            cluster_name: None,
            server_type: None,
            force: false,
        }
    }

    fn run(ctx: &Context, a: AddArgs) -> CoreResult<TargetAdded> {
        let plan = plan_add(ctx, a)?;
        match execute_add(ctx, plan, &NullReporter, &CancellationToken::new())? {
            Outcome::Completed { result } => Ok(result),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn the_plan_refuses_in_todays_order_without_any_request() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        assert!(matches!(
            plan_add(&ctx, args("bad_name", TOKEN_A)),
            Err(CoreError::InvalidTargetName { .. })
        ));
        assert!(matches!(
            plan_add(
                &ctx,
                AddArgs {
                    provider: "aws".into(),
                    ..args("x", TOKEN_A)
                }
            ),
            Err(CoreError::UnknownProvider { .. })
        ));
        assert!(matches!(
            plan_add(&ctx, args("x", "short")),
            Err(CoreError::InvalidToken { .. })
        ));
        assert!(matches!(
            plan_add(
                &ctx,
                AddArgs {
                    ssh_key: Some("/nope.pub".into()),
                    ..args("x", TOKEN_A)
                }
            ),
            Err(CoreError::SshKeyUnreadable { .. })
        ));
        assert!(matches!(
            plan_add(&ctx, args("prod", TOKEN_B)),
            Err(CoreError::TargetExists { .. })
        ));
    }

    /// The name check reads both of the target's files, as the CLI's always did: a target whose
    /// credentials file cannot be read is refused with that error, `force` or not — never
    /// reported as merely existing, and never overwritten.
    #[test]
    fn an_existing_target_with_unreadable_credentials_is_refused_force_or_not() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        let creds = ctx.store().target_credentials_file("prod");
        std::fs::write(&creds, "hetzner_token: [unclosed").unwrap();
        for force in [false, true] {
            let err = plan_add(
                &ctx,
                AddArgs {
                    force,
                    ..args("prod", TOKEN_B)
                },
            )
            .expect_err("unreadable credentials");
            assert!(
                matches!(
                    &err,
                    CoreError::Cli(cli_core::CliError::InvalidTargetConfig { path, .. })
                        if path == &creds
                ),
                "force={force}: {err:?}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(&creds).unwrap(),
            "hetzner_token: [unclosed"
        );
    }

    #[test]
    fn a_first_add_is_bounded_and_takes_the_empty_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let ctx =
            Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:1").with_no_ping(true);
        let plan = plan_add(&ctx, args("prod", TOKEN_A)).unwrap();
        assert_eq!(plan.class, PlanClass::Bounded);
        assert_eq!(
            plan.changes
                .iter()
                .map(|c| (c.kind.as_str(), c.action))
                .collect::<Vec<_>>(),
            [
                ("Target", ChangeAction::Create),
                ("Credentials", ChangeAction::Create),
                ("CliDefault", ChangeAction::SetDefault)
            ]
        );
        assert!(
            !format!("{plan:?}").contains(TOKEN_A)
                && !serde_json::to_string(&plan).unwrap().contains(TOKEN_A)
        );
        let r = {
            let Outcome::Completed { result } =
                execute_add(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap()
            else {
                panic!()
            };
            result
        };
        assert_eq!(
            (r.cli_default, r.is_cli_default, r.replaced),
            (
                Some(ActivePointerChange {
                    from: None,
                    to: Some("prod".into())
                }),
                true,
                false
            )
        );
        assert_eq!(
            r.token,
            Verification::Skipped {
                reason: SkipReason::NoPing
            }
        );
    }

    #[test]
    fn the_create_detail_names_the_set_fields_only() {
        let (dir, ctx) = store(&[], None);
        let key = dir.path().join("home").join(".ssh").join("id.pub");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, "ssh-ed25519 AAAA me@x\n").unwrap();
        let plan = plan_add(
            &ctx,
            AddArgs {
                region: Some("fsn1".into()),
                tier: Some("solo".into()),
                server_type: Some("cx22".into()),
                ssh_key: Some(key),
                ..args("prod", TOKEN_A)
            },
        )
        .unwrap();
        let shown = format!(
            "~/{}",
            std::path::Path::new(".ssh").join("id.pub").display()
        );
        assert_eq!(
            plan.changes[0].detail.as_deref(),
            Some(
                format!(
                    "provider hetzner-cloud, region fsn1, tier solo, server type cx22, ssh key {shown}"
                )
                .as_str()
            )
        );
        assert_eq!(plan.changes[1].detail.as_deref(), Some("API token"));
        assert_eq!(plan.changes[2].detail.as_deref(), Some("none → prod"));
    }

    #[test]
    fn the_three_pointer_outcomes_bug_1_needs() {
        let (_d, ctx) = store(&["prod", "staging"], Some("prod"));
        let ctx = ctx.with_no_ping(true);
        let active = run(
            &ctx,
            AddArgs {
                force: true,
                ..args("prod", TOKEN_B)
            },
        )
        .unwrap();
        assert_eq!(
            (
                active.cli_default.clone(),
                active.is_cli_default,
                active.replaced
            ),
            (None, true, true)
        );
        let inactive = run(
            &ctx,
            AddArgs {
                force: true,
                ..args("staging", TOKEN_B)
            },
        )
        .unwrap();
        assert_eq!(
            (inactive.cli_default, inactive.is_cli_default),
            (None, false)
        );
    }

    #[test]
    fn an_add_does_not_overwrite_a_target_created_since_its_plan() {
        let dir = tempfile::tempdir().unwrap();
        let ctx =
            Context::for_desktop(dir.path().join("store"), "http://127.0.0.1:1").with_no_ping(true);
        let plan = plan_add(&ctx, args("work", TOKEN_A)).unwrap();
        run(&ctx, args("work", TOKEN_B)).unwrap(); // meanwhile, during the ping
        assert!(matches!(
            execute_add(&ctx, plan, &NullReporter, &CancellationToken::new()),
            Err(CoreError::TargetExists { .. })
        ));
        assert_eq!(
            cli_core::load_target(&ctx.store(), "work")
                .unwrap()
                .credentials
                .hetzner_token
                .as_deref(),
            Some(TOKEN_B)
        );
    }

    #[test]
    fn execute_pings_and_classifies_a_401_as_a_rejected_token() {
        let mut s = mockito::Server::new();
        let _l = route(
            &mut s,
            "/v1/locations",
            401,
            r#"{"error":{"code":"unauthorized","message":"no"}}"#,
            TOKEN_A,
        )
        .create();
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::for_desktop(dir.path().join("store"), s.url());
        let e = run(&ctx, args("prod", TOKEN_A)).unwrap_err();
        assert_eq!(
            UiError::from(&e).code.as_deref(),
            Some("apprafter::target::token_rejected")
        );
        assert!(
            !ctx.store().target_dir("prod").exists(),
            "nothing saved after a rejected ping"
        );
    }

    #[test]
    fn a_sku_is_checked_in_the_flag_region_or_the_default() {
        let mut s = mockito::Server::new();
        let _l = route(&mut s, "/v1/locations", 200, LOCATIONS, TOKEN_A).create();
        let _t = route(&mut s, "/v1/server_types", 200, SERVER_TYPES, TOKEN_A).create();
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::for_desktop(dir.path().join("store"), s.url());
        let r = run(
            &ctx,
            AddArgs {
                server_type: Some("cx22".into()),
                ..args("prod", TOKEN_A)
            },
        )
        .unwrap();
        assert_eq!(
            r.sku,
            Some(SkuCheck::Validated {
                sku: "cx22".into(),
                region: "nbg1".into(),
                region_was_default: true
            })
        );
        let r = run(
            &ctx,
            AddArgs {
                server_type: Some("cx32".into()),
                region: Some("fsn1".into()),
                ..args("b", TOKEN_A)
            },
        )
        .unwrap();
        assert_eq!(
            r.sku,
            Some(SkuCheck::Validated {
                sku: "cx32".into(),
                region: "fsn1".into(),
                region_was_default: false
            })
        );
    }

    #[test]
    fn a_renew_refuses_the_same_token_and_patches_the_target_as_it_is_at_the_save() {
        let (_d, ctx) = store(&["work"], Some("work"));
        let ctx = ctx.with_no_ping(true);
        let w = TargetRef::named(&ctx, "work").unwrap();
        assert!(matches!(
            plan_renew(
                &ctx,
                &w,
                RenewArgs {
                    token: SecretString::new(TOKEN_A),
                    ssh_key: None
                }
            ),
            Err(CoreError::RenewTokenUnchanged { .. })
        ));
        let plan = plan_renew(
            &ctx,
            &w,
            RenewArgs {
                token: SecretString::new(TOKEN_B),
                ssh_key: None,
            },
        )
        .unwrap();
        assert_eq!(plan.class, PlanClass::Bounded);
        edit(&ctx, "work", |t| t.config.server_type = Some("cx32".into())); // `target machine` meanwhile
        execute_renew(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap();
        let t = cli_core::load_target(&ctx.store(), "work").unwrap();
        assert_eq!(
            (
                t.credentials.hetzner_token.as_deref(),
                t.config.server_type.as_deref()
            ),
            (Some(TOKEN_B), Some("cx32"))
        );
        let plan = plan_renew(
            &ctx,
            &w,
            RenewArgs {
                token: SecretString::new(TOKEN_A),
                ssh_key: None,
            },
        )
        .unwrap();
        cli_core::remove_target(&ctx.store(), "work").unwrap();
        assert!(matches!(
            execute_renew(&ctx, plan, &NullReporter, &CancellationToken::new()),
            Err(CoreError::TargetNotFound { .. })
        ));
        assert!(!ctx.store().target_dir("work").exists());
    }
}
