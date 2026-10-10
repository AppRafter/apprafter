// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `target machine`: set the server type (and, from the picker, the region) of a target that
//! has not provisioned yet, as plan and execute.

use crate::op::{ChangeAction, Outcome, Plan, PlanClass};
use crate::report::Reporter;
use crate::target::{
    cancelled, change, hetzner_token, lock_store_if_present, provisioned, MachineSet, SkuCheck,
};
use crate::{CancellationToken, Context, CoreError, CoreResult, TargetRef};

/// The rebuild of the provisioned target `name` on another machine (the operator guide's Route
/// A), one command per line — the one recipe the CLI prints under `TargetProvisioned` and the
/// desktop shows in a provisioned target's Machine row (D.3d review #20: it was copied by hand
/// and had drifted). `target use` first: `backup create` and `destroy` act on the active target,
/// which need not be `name`. `destroy` before the restore: `restore --reprovision` runs `up`,
/// which reuses a live server of the cluster's name and ignores `--server-type` on it.
pub fn rebuild_recipe(name: &str) -> [String; 4] {
    [
        format!("apprafter target use {name}"),
        "apprafter backup create --repo <repo>".to_string(),
        "apprafter destroy --yes".to_string(),
        "apprafter restore <repo> --reprovision --server-type <sku>".to_string(),
    ]
}

/// The machine a `target machine` call picks (overview §3.7.3): a server type, and a region
/// only when the picker moved it. `Debug` only: the payload derives `Debug` over it.
#[derive(Debug)]
pub struct MachineChoice {
    pub sku: String,
    pub region: Option<String>,
}

/// What [`execute_machine`] needs from its plan.
#[derive(Debug)]
pub struct MachinePayload {
    name: String,
    choice: MachineChoice,
}

/// [`CoreError::TargetProvisioned`] when the target's state records a server: there is no
/// in-place resize, so a new type would only be shadowed by the recorded one.
fn refuse_if_provisioned(ctx: &Context, target: &TargetRef) -> CoreResult<()> {
    match provisioned(ctx, target)? {
        Some(s) => Err(CoreError::TargetProvisioned {
            name: target.name().into(),
            server_id: s.server_id,
            server_name: s.server_name,
        }),
        None => Ok(()),
    }
}

/// Bounded. Refuses a provisioned target before anything else. Changes: `Update Target`
/// ("server type: cx22 → cx32"), and a second `Update Target` ("region: nbg1 → hel1") when the
/// choice moves the region.
pub fn plan_machine(
    ctx: &Context,
    target: &TargetRef,
    choice: MachineChoice,
) -> CoreResult<Plan<MachinePayload>> {
    refuse_if_provisioned(ctx, target)?;
    let name = target.name();
    let cfg = cli_core::target::load_target_config(&ctx.store(), name)?;
    let mut changes = vec![change(
        "Target",
        name,
        ChangeAction::Update,
        Some(format!(
            "server type: {} → {}",
            cfg.server_type.as_deref().unwrap_or("not set"),
            choice.sku
        )),
    )];
    if let Some(r) = choice
        .region
        .as_deref()
        .filter(|r| cfg.region.as_deref() != Some(*r))
    {
        changes.push(change(
            "Target",
            name,
            ChangeAction::Update,
            Some(format!(
                "region: {} → {r}",
                cfg.region.as_deref().unwrap_or("not set")
            )),
        ));
    }
    Ok(Plan {
        class: PlanClass::Bounded,
        title: format!("Set the machine of {name}"),
        changes,
        payload: MachinePayload {
            name: name.into(),
            choice,
        },
    })
}

/// Validates the server type against the provider's catalogue unless `no_ping` — in the chosen
/// region, else the stored one, else [`crate::machine::DEFAULT_REGION`]; token: the CLI
/// override, else the stored one (R4). Then, under the lock, re-reads the target, re-checks that
/// it has still not provisioned, and patches only the machine fields: an edit made meanwhile
/// (a token renewed) is kept.
pub fn execute_machine(
    ctx: &Context,
    plan: Plan<MachinePayload>,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<Outcome<MachineSet>> {
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let MachinePayload { name, choice } = plan.payload;
    let target = TargetRef::named(ctx, &name)?;
    let stored_region = cli_core::target::load_target_config(&ctx.store(), &name)?.region;
    let (region, region_was_default) = match (choice.region.clone(), stored_region) {
        (Some(r), _) | (None, Some(r)) => (r, false),
        (None, None) => (crate::machine::DEFAULT_REGION.to_string(), true),
    };
    let sku_check = if ctx.no_ping() {
        SkuCheck::NotValidated {
            sku: choice.sku.clone(),
        }
    } else {
        let token = hetzner_token(ctx, &target)?;
        crate::machine::check_sku(
            ctx,
            &token,
            &choice.sku,
            &region,
            cli_core::SkuCheckFor::TargetMachine { name: name.clone() },
            cancel,
        )?;
        SkuCheck::Validated {
            sku: choice.sku.clone(),
            region,
            region_was_default,
        }
    };
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let store = ctx.store();
    let _lock = lock_store_if_present(ctx, reporter)?;
    let mut t = cli_core::load_target(&store, &name)?;
    refuse_if_provisioned(ctx, &target)?;
    t.config.server_type = Some(choice.sku.clone());
    if let Some(r) = &choice.region {
        t.config.region = Some(r.clone());
    }
    cli_core::save_target(&store, &t)?;
    Ok(Outcome::Completed {
        result: MachineSet {
            name,
            sku: choice.sku,
            region: choice.region,
            sku_check,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;
    use crate::NullReporter;

    fn choice(sku: &str, region: Option<&str>) -> MachineChoice {
        MachineChoice {
            sku: sku.into(),
            region: region.map(Into::into),
        }
    }

    #[test]
    fn a_provisioned_target_is_refused_by_the_plan() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        seed_server(&ctx, "prod", 42, "platform-1", None);
        match plan_machine(
            &ctx,
            &TargetRef::named(&ctx, "prod").unwrap(),
            choice("cx32", None),
        ) {
            Err(CoreError::TargetProvisioned {
                name,
                server_id: 42,
                server_name,
            }) => assert_eq!(
                (name.as_str(), server_name.as_str()),
                ("prod", "platform-1")
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn no_ping_records_without_a_request() {
        let mut s = mockito::Server::new();
        let t = route(&mut s, "/v1/server_types", 200, SERVER_TYPES, TOKEN_A)
            .expect(0)
            .create();
        let (_d, ctx) = store_at(&["prod"], Some("prod"), &s.url());
        let ctx = ctx.with_no_ping(true);
        let plan = plan_machine(
            &ctx,
            &TargetRef::named(&ctx, "prod").unwrap(),
            choice("cx32", None),
        )
        .unwrap();
        let Outcome::Completed { result } =
            execute_machine(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            result.sku_check,
            SkuCheck::NotValidated { sku: "cx32".into() }
        );
        t.assert();
    }

    #[test]
    fn a_sku_is_validated_in_the_stored_region_or_the_default_and_a_rejected_one_is_never_written()
    {
        let mut s = mockito::Server::new();
        let _t = route(&mut s, "/v1/server_types", 200, SERVER_TYPES, TOKEN_A).create();
        let (_d, ctx) = store_at(&["prod"], Some("prod"), &s.url());
        edit(&ctx, "prod", |t| t.config.region = None);
        let p = TargetRef::named(&ctx, "prod").unwrap();
        let Outcome::Completed { result } = execute_machine(
            &ctx,
            plan_machine(&ctx, &p, choice("cx22", None)).unwrap(),
            &NullReporter,
            &CancellationToken::new(),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            result.sku_check,
            SkuCheck::Validated {
                sku: "cx22".into(),
                region: "nbg1".into(),
                region_was_default: true
            }
        );
        assert!(execute_machine(
            &ctx,
            plan_machine(&ctx, &p, choice("cx99", None)).unwrap(),
            &NullReporter,
            &CancellationToken::new()
        )
        .is_err());
        assert_eq!(
            cli_core::load_target(&ctx.store(), "prod")
                .unwrap()
                .config
                .server_type
                .as_deref(),
            Some("cx22")
        );
    }

    /// A target that stores a region has its SKU checked there, not in the default region,
    /// and a region chosen in the picker wins over the stored one. cx22 is sold in nbg1 only,
    /// so the region the check ran in decides the outcome — every other machine test stores
    /// nbg1, the default, and could not tell the two apart.
    #[test]
    fn a_sku_is_validated_in_the_stored_region_unless_the_choice_names_one() {
        let mut s = mockito::Server::new();
        let _t = route(&mut s, "/v1/server_types", 200, SERVER_TYPES, TOKEN_A).create();
        let (_d, ctx) = store_at(&["prod"], Some("prod"), &s.url());
        edit(&ctx, "prod", |t| t.config.region = Some("fsn1".into()));
        let p = TargetRef::named(&ctx, "prod").unwrap();
        let run = |c| {
            execute_machine(
                &ctx,
                plan_machine(&ctx, &p, c).unwrap(),
                &NullReporter,
                &CancellationToken::new(),
            )
        };
        let stored = || cli_core::load_target(&ctx.store(), "prod").unwrap().config;

        let e = run(choice("cx22", None)).unwrap_err();
        let ui = crate::error::UiError::from(&e);
        assert_eq!(
            ui.code.as_deref(),
            Some("apprafter::provider::server_type_unavailable")
        );
        assert_eq!(ui.fields["location"], serde_json::json!("fsn1"));
        assert_eq!(stored().server_type, None, "a refused SKU is never written");

        let Outcome::Completed { result } = run(choice("cx32", None)).unwrap() else {
            panic!()
        };
        assert_eq!(
            result.sku_check,
            SkuCheck::Validated {
                sku: "cx32".into(),
                region: "fsn1".into(),
                region_was_default: false
            }
        );

        let Outcome::Completed { result } = run(choice("cx22", Some("nbg1"))).unwrap() else {
            panic!()
        };
        assert_eq!(
            result.sku_check,
            SkuCheck::Validated {
                sku: "cx22".into(),
                region: "nbg1".into(),
                region_was_default: false
            }
        );
        let c = stored();
        assert_eq!(
            (c.region.as_deref(), c.server_type.as_deref()),
            (Some("nbg1"), Some("cx22"))
        );
    }

    /// Overview §3.7.3: a `target machine` cancelled before its SKU check, or while it runs,
    /// writes nothing.
    #[test]
    fn a_cancelled_machine_writes_nothing() {
        let mut s = mockito::Server::new();
        let cancel = CancellationToken::new();
        let _t =
            route_cancelling(&mut s, "/v1/server_types", SERVER_TYPES, TOKEN_A, &cancel).create();
        let (_d, ctx) = store_at(&["prod"], Some("prod"), &s.url());
        let p = TargetRef::named(&ctx, "prod").unwrap();
        for (when, token) in [
            ("before", cancelled_token()),
            ("during the SKU check", cancel),
        ] {
            let plan = plan_machine(&ctx, &p, choice("cx22", Some("nbg1"))).unwrap();
            let got = execute_machine(&ctx, plan, &NullReporter, &token);
            assert!(
                matches!(got, Ok(Outcome::Cancelled { .. })),
                "{when}: {got:?}"
            );
            assert_eq!(
                cli_core::load_target(&ctx.store(), "prod")
                    .unwrap()
                    .config
                    .server_type,
                None,
                "{when}"
            );
        }
    }

    #[test]
    fn the_cli_override_token_validates_the_sku() {
        // R4: override first
        let mut s = mockito::Server::new();
        let t = route(&mut s, "/v1/server_types", 200, SERVER_TYPES, TOKEN_B)
            .expect(1)
            .create();
        let (dir, _) = store_at(&["prod"], Some("prod"), &s.url());
        let env = crate::MapEnv::new()
            .with(
                "APPRAFTER_CONFIG_DIR",
                dir.path().join("store").to_str().unwrap(),
            )
            .with("APPRAFTER_HCLOUD_BASE_URL", &s.url())
            .with("HCLOUD_TOKEN", TOKEN_B);
        let ctx = Context::from_cli_env(&env).unwrap();
        let p = TargetRef::named(&ctx, "prod").unwrap();
        execute_machine(
            &ctx,
            plan_machine(&ctx, &p, choice("cx32", None)).unwrap(),
            &NullReporter,
            &CancellationToken::new(),
        )
        .unwrap();
        t.assert();
    }

    #[test]
    fn the_save_keeps_a_token_renewed_meanwhile_and_refuses_a_target_provisioned_meanwhile() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        let ctx = ctx.with_no_ping(true);
        let p = TargetRef::named(&ctx, "prod").unwrap();
        let plan = plan_machine(&ctx, &p, choice("cx32", Some("hel1"))).unwrap();
        edit(&ctx, "prod", |t| {
            t.credentials.hetzner_token = Some(TOKEN_B.into())
        });
        execute_machine(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap();
        let t = cli_core::load_target(&ctx.store(), "prod").unwrap();
        assert_eq!(
            (
                t.credentials.hetzner_token.as_deref(),
                t.config.region.as_deref(),
                t.config.server_type.as_deref()
            ),
            (Some(TOKEN_B), Some("hel1"), Some("cx32"))
        );
        let plan = plan_machine(&ctx, &p, choice("cx22", None)).unwrap();
        seed_server(&ctx, "prod", 1, "platform-1", None);
        assert!(matches!(
            execute_machine(&ctx, plan, &NullReporter, &CancellationToken::new()),
            Err(CoreError::TargetProvisioned { .. })
        ));
    }

    #[test]
    fn a_target_removed_meanwhile_is_not_recreated() {
        let (_d, ctx) = store(&["prod", "x"], Some("x"));
        let ctx = ctx.with_no_ping(true);
        let plan = plan_machine(
            &ctx,
            &TargetRef::named(&ctx, "prod").unwrap(),
            choice("cx22", None),
        )
        .unwrap();
        cli_core::remove_target(&ctx.store(), "prod").unwrap();
        assert!(matches!(
            execute_machine(&ctx, plan, &NullReporter, &CancellationToken::new()),
            Err(CoreError::TargetNotFound { .. })
        ));
        assert!(!ctx.store().target_dir("prod").exists());
    }
}
