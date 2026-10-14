// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `target add` (a new target, or a `--force` overwrite that keeps every field it is not given)
//! and `target add --renew` (a target's token, its SSH key, or both), as plan and execute.

use std::path::{Path, PathBuf};

use cli_core::ssh_key::KeySource;
use cli_core::{CliError, GlobalConfig, TargetConfig};

use crate::context::SecretString;
use crate::op::{ChangeAction, Outcome, Plan, PlanClass, PlannedChange};
use crate::provider::{SkipReason, Verification};
use crate::report::Reporter;
use crate::target::{
    cancelled, change, cli_default, lock_store, lock_store_for, provisioned, readable_config,
    validate_name, SkuCheck, TargetAdded, TargetRenewed,
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

/// What `target add --renew` asks for (overview §3.7.3): a new token, a new SSH key path, or
/// both. `token: None` keeps the stored credentials (no ping, the file is not written).
#[derive(Debug)]
pub struct RenewArgs {
    pub token: Option<SecretString>,
    pub ssh_key: Option<PathBuf>,
}

/// What [`execute_add`] needs from its plan.
#[derive(Debug)]
pub struct AddPayload {
    args: AddArgs,
}

/// What [`execute_renew`] needs from its plan: the new token and the new key path, each only
/// when it differed from the stored one when planned.
#[derive(Debug)]
pub struct RenewPayload {
    name: String,
    provider: String,
    token: Option<SecretString>,
    ssh_key: Option<PathBuf>,
    /// The record the new token replaces although this store never used it (D.4a T42, T34,
    /// T43), when planned.
    replaces: Option<Unused>,
}

impl RenewPayload {
    /// What the new token replaces although this store never used it — a token kept in
    /// clear on a locked store, another store's marker, a marker without a vault entry —
    /// in words for a person: shown before and after the write, so a token planted in
    /// clear is never replaced unseen. `None` for a renewal over a token the store uses, or
    /// one that writes no token.
    pub fn replaces(&self) -> Option<String> {
        self.token
            .as_ref()
            .and(self.replaces.as_ref())
            .map(Unused::what)
    }
}

/// The stored `config.yaml` of `name`, or `None` when there is no such target. Both of the
/// target's files are read, as the CLI's name check always read them ([`readable_config`]):
/// a target whose credentials file cannot be read is that error, `force` or not — never
/// "exists", and never overwritten. The token itself is not read.
fn stored_config(ctx: &Context, name: &str) -> CoreResult<Option<TargetConfig>> {
    match readable_config(ctx, name) {
        Ok(config) => Ok(Some(config)),
        Err(cli_core::CliError::TargetNotFound { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Local checks only (R2), in today's order: name, provider, token format, SSH key, name free
/// (unless `force`). A new name is Bounded (`Create Target`, `Create Credentials`, and
/// `SetDefault CliDefault` when `config.yaml` is absent) — with `force` too (deviation 3). A
/// forced overwrite of a stored target is Destructive: one `Update` or `Keep Target` per field
/// ([`force_changes`]), then `Replace Credentials`; on a target whose state records a server it
/// refuses a region or server-type change ([`CoreError::TargetProvisioned`], the guard `target
/// machine` uses).
pub fn plan_add(ctx: &Context, args: AddArgs) -> CoreResult<Plan<AddPayload>> {
    validate_name(&args.name).map_err(|problem| CoreError::InvalidTargetName {
        name: args.name.clone(),
        problem,
    })?;
    crate::provider::require_supported(&args.provider)?;
    crate::provider::TokenProblem::check(args.token.expose())
        .map_err(|problem| CoreError::InvalidToken { problem })?;
    if let Some(p) = &args.ssh_key {
        // A new target's key is fixed by running the same add again, an existing one's by
        // `--renew`: the refusal's help says which (D.3d verification).
        let source = if ctx.store().target_config_file(&args.name).exists() {
            KeySource::TargetFile
        } else {
            KeySource::NewTargetFile
        };
        crate::ssh::check_readable(p, source)?;
    }
    let store = ctx.store();
    let stored = stored_config(ctx, &args.name)?;
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
        Some(stored) => {
            refuse_machine_change_if_provisioned(ctx, stored, &args)?;
            (
                PlanClass::Destructive,
                format!("Overwrite target {n}"),
                force_changes(ctx, stored, &args),
            )
        }
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

/// `--force` over a stored target: a flag replaces its field, every other field keeps its stored
/// value, the firewall (no flag sets it) is always carried over. A struct literal on purpose: a
/// new `TargetConfig` field fails to compile here until someone decides how `--force` treats it.
fn merge_force(stored: &TargetConfig, a: &AddArgs) -> TargetConfig {
    TargetConfig {
        provider: a.provider.clone(),
        region: a.region.clone().or_else(|| stored.region.clone()),
        default_tier: a.tier.clone().or_else(|| stored.default_tier.clone()),
        cluster_name: a
            .cluster_name
            .clone()
            .or_else(|| stored.cluster_name.clone()),
        ssh_key_path: a.ssh_key.clone().or_else(|| stored.ssh_key_path.clone()),
        firewall: stored.firewall.clone(),
        server_type: a.server_type.clone().or_else(|| stored.server_type.clone()),
    }
}

/// A new target from the flags alone. A struct literal for the same reason as [`merge_force`].
fn new_config(a: &AddArgs) -> TargetConfig {
    TargetConfig {
        provider: a.provider.clone(),
        region: a.region.clone(),
        default_tier: a.tier.clone(),
        cluster_name: a.cluster_name.clone(),
        ssh_key_path: a.ssh_key.clone(),
        firewall: None,
        server_type: a.server_type.clone(),
    }
}

/// The flags move the machine: a region or server type that differs from the stored one.
fn changes_machine(stored: &TargetConfig, a: &AddArgs) -> bool {
    a.region
        .as_deref()
        .is_some_and(|r| stored.region.as_deref() != Some(r))
        || a.server_type
            .as_deref()
            .is_some_and(|s| stored.server_type.as_deref() != Some(s))
}

/// [`CoreError::TargetProvisioned`] when the flags move the machine of a target whose state
/// records a server: there is no in-place resize (the guard `target machine` uses). The state is
/// read only when the machine would move.
fn refuse_machine_change_if_provisioned(
    ctx: &Context,
    stored: &TargetConfig,
    a: &AddArgs,
) -> CoreResult<()> {
    if !changes_machine(stored, a) {
        return Ok(());
    }
    let t = TargetRef::named(ctx, &a.name)?;
    match provisioned(ctx, &t)? {
        Some(s) => Err(CoreError::TargetProvisioned {
            name: a.name.clone(),
            server_id: s.server_id,
            server_name: s.server_name,
        }),
        None => Ok(()),
    }
}

/// One `Update` / `Keep Target` per field, unset-and-unpassed fields omitted, then the firewall
/// (always kept, when set). `plan_add` adds `Replace Credentials` after them.
fn force_changes(ctx: &Context, stored: &TargetConfig, a: &AddArgs) -> Vec<PlannedChange> {
    let key = |p: &Path| cli_core::paths::abbreviate_home(p, ctx.home_dir());
    let fields = [
        ("region", stored.region.clone(), a.region.clone()),
        ("tier", stored.default_tier.clone(), a.tier.clone()),
        (
            "cluster name",
            stored.cluster_name.clone(),
            a.cluster_name.clone(),
        ),
        (
            "ssh key",
            stored.ssh_key_path.as_deref().map(key),
            a.ssh_key.as_deref().map(key),
        ),
        (
            "server type",
            stored.server_type.clone(),
            a.server_type.clone(),
        ),
    ];
    let mut out: Vec<PlannedChange> = fields
        .into_iter()
        .filter_map(|(label, s, f)| match (s, f) {
            (None, None) => None,
            (Some(s), None) => Some(change(
                "Target",
                &a.name,
                ChangeAction::Keep,
                Some(format!("{label}: {s}")),
            )),
            (Some(s), Some(f)) if s == f => Some(change(
                "Target",
                &a.name,
                ChangeAction::Keep,
                Some(format!("{label}: {f}")),
            )),
            (s, Some(f)) => Some(change(
                "Target",
                &a.name,
                ChangeAction::Update,
                Some(format!(
                    "{label}: {} → {f}",
                    s.as_deref().unwrap_or("not set")
                )),
            )),
        })
        .collect();
    if let Some(fw) = &stored.firewall {
        out.push(change(
            "Target",
            &a.name,
            ChangeAction::Keep,
            Some(format!(
                "firewall: Cloudflare origin {}",
                if fw.cloudflare_origin { "on" } else { "off" }
            )),
        ));
    }
    out
}

/// Ping (unless `no_ping`; even after a wizard verify — R2), the SKU check (server types only,
/// in the flag's region, else — with `force` — the stored one, else
/// [`crate::machine::DEFAULT_REGION`]), then what the token write needs (before the lock: a
/// slow step never runs under it, T41), then, under the lock (always taken, creating the root
/// — D.3b's deviation 1), re-check, save `config.yaml` and store the token through the secret
/// store. A forced overwrite re-reads the stored target
/// under the lock, runs the provisioned guard again and merges onto it ([`merge_force`]), so an
/// edit made during the ping is kept. The pointer moves only when `config.yaml` is absent; the
/// outcome reports what happened.
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
            let stored_region = if a.force {
                stored_config(ctx, &a.name)?.and_then(|c| c.region)
            } else {
                None
            };
            let (region, region_was_default) = match a.region.clone().or(stored_region) {
                Some(r) => (r, false),
                None => (crate::machine::DEFAULT_REGION.to_string(), true),
            };
            crate::machine::check_sku(
                ctx,
                &a.token,
                sku,
                &region,
                cli_core::SkuCheckFor::TargetAdd {
                    name: a.name.clone(),
                },
                cancel,
            )?;
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
    let secrets = ctx.secrets();
    let key = secrets.prepare_write()?;
    let lock = lock_store(ctx, reporter)?;
    let stored = stored_config(ctx, &a.name)?;
    let replaced = stored.is_some();
    if replaced && !a.force {
        return Err(CoreError::TargetExists { name: a.name });
    }
    let config = match &stored {
        Some(stored) => {
            refuse_machine_change_if_provisioned(ctx, stored, &a)?;
            merge_force(stored, &a)
        }
        None => new_config(&a),
    };
    // `config.yaml`, then the token: the order `save_target` wrote them in.
    cli_core::target::save_target_config(&store, &a.name, &config)?;
    secrets.put_token(&key, &lock, &a.name, &a.token)?;
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

/// Bounded. A renewal changes whatever differs from what is stored, and nothing else: a token
/// equal to the stored one counts as no token (so a key change beside it is not refused), and a
/// key path equal to the stored one is no change. Local checks, in order: the target exists (both
/// of its files), the token's format, the SSH key is readable, then something would change —
/// else [`CoreError::RenewTokenUnchanged`] when a token was given (the stored one), or
/// [`CoreError::RenewNothingToChange`] when none was. Changes: `Replace Credentials` ("API
/// token") for a new token, `Update Target` ("ssh key: a → b") for a new key path.
pub fn plan_renew(
    ctx: &Context,
    target: &TargetRef,
    args: RenewArgs,
) -> CoreResult<Plan<RenewPayload>> {
    let name = target.name();
    let config = cli_core::target::load_target_config(&ctx.store(), name)?;
    let (stored, replaces) = renewal_record(ctx, name)?;
    if let Some(token) = &args.token {
        crate::provider::TokenProblem::check(token.expose())
            .map_err(|problem| CoreError::InvalidToken { problem })?;
    }
    if let Some(p) = &args.ssh_key {
        crate::ssh::check_readable(p, KeySource::TargetFile)?;
    }
    let token_given = args.token.is_some();
    let (token, ssh_key) = what_differs(&config, stored.as_ref(), args.token, args.ssh_key);
    if token.is_none() && ssh_key.is_none() {
        return Err(nothing_to_change(name, token_given));
    }
    let mut changes = Vec::new();
    if token.is_some() {
        // D.4a T42, T34, T43: a record this store never used is named, never replaced unseen.
        let detail = match &replaces {
            Some(unused) => format!("API token, replacing {}", unused.what()),
            None => "API token".to_string(),
        };
        changes.push(change(
            "Credentials",
            name,
            ChangeAction::Replace,
            Some(detail),
        ));
    }
    if let Some(p) = &ssh_key {
        let show = |p: &Path| cli_core::paths::abbreviate_home(p, ctx.home_dir());
        changes.push(change(
            "Target",
            name,
            ChangeAction::Update,
            Some(format!(
                "ssh key: {} → {}",
                config
                    .ssh_key_path
                    .as_deref()
                    .map(show)
                    .unwrap_or_else(|| "not set".into()),
                show(p)
            )),
        ));
    }
    let title = match (&token, &ssh_key) {
        (Some(_), None) => format!("Rotate the API token of {name}"),
        (None, _) => format!("Change the SSH key of {name}"),
        (Some(_), Some(_)) => format!("Rotate the API token and change the SSH key of {name}"),
    };
    Ok(Plan {
        class: PlanClass::Bounded,
        title,
        changes,
        payload: RenewPayload {
            name: name.into(),
            provider: config.provider,
            token,
            ssh_key,
            replaces,
        },
    })
}

/// The token and the key path of a renewal that differ from what is stored (`stored`, the
/// token; `config`, the key path); `None` for each that does not (or was not given).
fn what_differs(
    config: &TargetConfig,
    stored: Option<&SecretString>,
    token: Option<SecretString>,
    ssh_key: Option<PathBuf>,
) -> (Option<SecretString>, Option<PathBuf>) {
    (
        token.filter(|tok| stored.map(SecretString::expose) != Some(tok.expose())),
        ssh_key.filter(|p| config.ssh_key_path.as_ref() != Some(p)),
    )
}

/// The refusal of a renewal that would change nothing, by what it was given.
fn nothing_to_change(name: &str, token_given: bool) -> CoreError {
    let name = name.to_string();
    if token_given {
        CoreError::RenewTokenUnchanged { name }
    } else {
        CoreError::RenewNothingToChange { name }
    }
}

/// A record a renewal replaces although this store never used it (D.4a T42, T34, T43).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Unused {
    /// A token in clear in a locked store's credentials file (T42).
    InClear,
    /// A marker naming another store's vault (T34; O8's way back, on a store in files).
    OtherVault { id: String },
    /// A marker of this store whose vault holds no entry for the target (T43).
    NoVaultEntry,
}

impl Unused {
    /// What the new token replaces, as a renewal names it.
    fn what(&self) -> String {
        match self {
            Unused::InClear => {
                "a token kept in clear, which this locked store never used".to_string()
            }
            Unused::OtherVault { id } => format!(
                "a marker for the vault of the target store with id {id}, which this store \
                 never used"
            ),
            Unused::NoVaultEntry => {
                "a marker whose entry the vault of this store does not hold".to_string()
            }
        }
    }
}

/// The stored token a renewal compares the new one with, and the record it replaces although
/// this store never used it (see [`stored_for_renewal`]).
fn renewal_record(ctx: &Context, name: &str) -> CoreResult<(Option<SecretString>, Option<Unused>)> {
    match ctx.secrets().stored_token(name) {
        Ok(stored) => Ok((stored, None)),
        Err(CliError::TokenOutsideVault { .. }) => Ok((None, Some(Unused::InClear))),
        Err(CliError::TokenInAnotherVault { id, .. }) => {
            Ok((None, Some(Unused::OtherVault { id })))
        }
        Err(CliError::TokenMissingFromVault { .. }) => Ok((None, Some(Unused::NoVaultEntry))),
        Err(other) => Err(other.into()),
    }
}

/// The token stored for `name`, as a renewal compares the new one with it (D.4a T42, T34,
/// T43): what [`plan_renew`], [`execute_renew`] and the CLI's `target add --renew` read.
///
/// A record this store refuses to use is no stored token here, so a renewal replaces it: a
/// token in clear in a locked store's credentials file (`token_outside_vault`), a marker
/// naming another store's vault (`token_in_another_vault`, also on a store in files, the
/// state O8's manual way back leaves), or a marker of this store whose vault holds no entry
/// for the target (`token_missing_from_vault`). Every help for those three sends the person
/// to `target add <name> --renew`, which relies on this. Anything else the store says is the
/// error it was — `store_locked`, `lock_incomplete`, `vault_unreadable`, an unreadable file —
/// never "no token". The write that follows still takes the store's write key (on a locked
/// store it asks, O11) and puts the vault entry on disk before the marker replaces the file.
pub fn stored_for_renewal(ctx: &Context, name: &str) -> CoreResult<Option<SecretString>> {
    renewal_record(ctx, name).map(|(stored, _)| stored)
}

/// Ping the new token (only when there is one, and not with `no_ping`), then prepare the token
/// write (before the lock, T41) and, under the lock, re-read the target and change only what still differs ([`plan_renew`]'s rules, refused the
/// same way when nothing does): an edit made during the ping is kept. With no new token left
/// only `config.yaml` is written, so the credentials file stays as it was, byte for byte, and
/// the outcome's `token` is `None`.
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
        replaces: _,
    } = plan.payload;
    let verification = match &token {
        None => None,
        Some(_) if ctx.no_ping() => Some(Verification::Skipped {
            reason: SkipReason::NoPing,
        }),
        Some(token) => Some(Verification::Verified {
            elapsed_ms: crate::provider::ping(ctx, &provider, token, cancel)?.as_millis() as u64,
        }),
    };
    if cancel.is_cancelled() {
        return Ok(cancelled());
    }
    let store = ctx.store();
    let secrets = ctx.secrets();
    let key = secrets.prepare_write()?;
    let lock = lock_store_for(ctx, reporter, &name)?;
    let mut config = cli_core::target::load_target_config(&store, &name)?;
    let stored = stored_for_renewal(ctx, &name)?;
    let token_given = token.is_some();
    let (token, ssh_key) = what_differs(&config, stored.as_ref(), token, ssh_key);
    if token.is_none() && ssh_key.is_none() {
        return Err(nothing_to_change(&name, token_given));
    }
    let ssh_key_changed = ssh_key.is_some();
    if let Some(p) = ssh_key {
        config.ssh_key_path = Some(p);
    }
    // `config.yaml` first, then the token, as `save_target` wrote them; without a new token
    // the credentials file is not written, so it stays as it was, byte for byte.
    cli_core::target::save_target_config(&store, &name, &config)?;
    let token = match token {
        Some(token) => {
            secrets.put_token(&key, &lock, &name, &token)?;
            verification
        }
        None => None,
    };
    Ok(Outcome::Completed {
        result: TargetRenewed {
            name,
            token,
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

    /// WI-475: in a store another account can change (pretended, in this test's own temp dir),
    /// an add and a renewal planned before are refused before anything is written — no new
    /// target's folder, no pointer, no token — naming the folder.
    #[test]
    fn a_store_another_account_can_change_is_refused_before_anything_is_written() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        let ctx = ctx.with_no_ping(true);
        let prod = TargetRef::named(&ctx, "prod").unwrap();
        let add = plan_add(&ctx, args("dev", TOKEN_B)).unwrap();
        let renew = plan_renew(
            &ctx,
            &prod,
            RenewArgs {
                token: Some(SecretString::new(TOKEN_B)),
                ssh_key: None,
            },
        )
        .unwrap();
        let before = crate::target::testkit::snapshot(ctx.config_root());
        let targets = ctx.store().targets_dir();
        let _foreign = cli_core::owner_only::pretend::foreign_owner(&targets);
        let cancel = CancellationToken::new();
        for got in [
            execute_add(&ctx, add, &NullReporter, &cancel).err(),
            execute_renew(&ctx, renew, &NullReporter, &cancel).err(),
        ] {
            match got {
                Some(CoreError::Cli(cli_core::CliError::StoreNotPrivate { path, .. })) => {
                    assert_eq!(path, targets)
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(crate::target::testkit::snapshot(ctx.config_root()), before);
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

    /// Bug 8: `--force` replaced the whole target from the flags, so the Cloudflare origin
    /// firewall toggle (no `target add` flag sets it) was turned off and every field the
    /// command did not repeat was wiped.
    #[test]
    fn force_keeps_every_field_it_is_not_given_and_always_the_firewall() {
        let (dir, ctx) = store(&["prod"], Some("prod"));
        let ctx = ctx.with_no_ping(true);
        let key = dir.path().join("k.pub");
        std::fs::write(&key, "ssh-ed25519 AAAA k\n").unwrap();
        edit(&ctx, "prod", |t| {
            t.config.cluster_name = Some("edge-1".into());
            t.config.ssh_key_path = Some(key.clone());
            t.config.server_type = Some("cx22".into());
            t.config.firewall = Some(cli_core::target::FirewallConfig {
                cloudflare_origin: true,
            });
        });
        let plan = plan_add(
            &ctx,
            AddArgs {
                force: true,
                region: Some("hel1".into()),
                ..args("prod", TOKEN_B)
            },
        )
        .unwrap();
        assert_eq!(plan.class, PlanClass::Destructive);
        let details: Vec<_> = plan
            .changes
            .iter()
            .filter_map(|c| c.detail.clone().map(|d| (c.action, d)))
            .collect();
        assert!(details.contains(&(ChangeAction::Update, "region: nbg1 → hel1".into())));
        assert!(details.contains(&(ChangeAction::Keep, "tier: solo".into())));
        assert!(details.contains(&(ChangeAction::Keep, "cluster name: edge-1".into())));
        assert!(details.contains(&(ChangeAction::Keep, "server type: cx22".into())));
        assert!(details.contains(&(ChangeAction::Keep, "firewall: Cloudflare origin on".into())));
        assert!(details.contains(&(ChangeAction::Replace, "API token".into())));
        execute_add(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap();
        let t = cli_core::load_target(&ctx.store(), "prod").unwrap();
        let c = t.config;
        assert_eq!(
            (
                c.region.as_deref(),
                c.default_tier.as_deref(),
                c.cluster_name.as_deref(),
                c.server_type.as_deref()
            ),
            (Some("hel1"), Some("solo"), Some("edge-1"), Some("cx22"))
        );
        assert_eq!(c.ssh_key_path, Some(key));
        assert_eq!(
            c.firewall,
            Some(cli_core::target::FirewallConfig {
                cloudflare_origin: true
            })
        );
        assert_eq!(t.credentials.hetzner_token.as_deref(), Some(TOKEN_B));
    }

    /// A flag that names the stored value is no change, so it reads as kept.
    #[test]
    fn force_with_the_stored_value_keeps_it_and_omits_what_was_never_set() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        let plan = plan_add(
            &ctx,
            AddArgs {
                force: true,
                region: Some("nbg1".into()),
                ..args("prod", TOKEN_B)
            },
        )
        .unwrap();
        let target: Vec<_> = plan
            .changes
            .iter()
            .filter(|c| c.kind == "Target")
            .map(|c| (c.action, c.detail.clone().unwrap_or_default()))
            .collect();
        assert_eq!(
            target,
            [
                (ChangeAction::Keep, "region: nbg1".to_string()),
                (ChangeAction::Keep, "tier: solo".to_string()),
            ],
            "unset-and-unpassed fields (cluster name, ssh key, server type, firewall) are omitted"
        );
    }

    #[test]
    fn force_refuses_a_region_or_sku_change_on_a_provisioned_target_but_not_a_token_change() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        let ctx = ctx.with_no_ping(true);
        seed_server(&ctx, "prod", 42, "platform-1", Some("cx22"));
        assert!(matches!(
            plan_add(
                &ctx,
                AddArgs {
                    force: true,
                    region: Some("hel1".into()),
                    ..args("prod", TOKEN_B)
                }
            ),
            Err(CoreError::TargetProvisioned { server_id: 42, .. })
        ));
        assert!(matches!(
            plan_add(
                &ctx,
                AddArgs {
                    force: true,
                    server_type: Some("cx32".into()),
                    ..args("prod", TOKEN_B)
                }
            ),
            Err(CoreError::TargetProvisioned { .. })
        ));
        assert!(
            plan_add(
                &ctx,
                AddArgs {
                    force: true,
                    region: Some("nbg1".into()),
                    ..args("prod", TOKEN_B)
                }
            )
            .is_ok(),
            "same region is no change"
        );
        run(
            &ctx,
            AddArgs {
                force: true,
                ..args("prod", TOKEN_B)
            },
        )
        .unwrap();
        assert_eq!(
            cli_core::load_target(&ctx.store(), "prod")
                .unwrap()
                .credentials
                .hetzner_token
                .as_deref(),
            Some(TOKEN_B)
        );
    }

    /// The guard runs again under the lock: a server recorded while the plan waited (another
    /// terminal's `up`) still refuses the machine change.
    #[test]
    fn the_provisioned_guard_runs_again_under_the_lock() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        let ctx = ctx.with_no_ping(true);
        let plan = plan_add(
            &ctx,
            AddArgs {
                force: true,
                region: Some("hel1".into()),
                ..args("prod", TOKEN_B)
            },
        )
        .unwrap();
        seed_server(&ctx, "prod", 42, "platform-1", None);
        assert!(matches!(
            execute_add(&ctx, plan, &NullReporter, &CancellationToken::new()),
            Err(CoreError::TargetProvisioned { server_id: 42, .. })
        ));
        let c = cli_core::load_target(&ctx.store(), "prod").unwrap();
        assert_eq!(
            (
                c.config.region.as_deref(),
                c.credentials.hetzner_token.as_deref()
            ),
            (Some("nbg1"), Some(TOKEN_A)),
            "nothing written"
        );
    }

    #[test]
    fn the_merge_happens_again_under_the_lock() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        let ctx = ctx.with_no_ping(true);
        let plan = plan_add(
            &ctx,
            AddArgs {
                force: true,
                ..args("prod", TOKEN_B)
            },
        )
        .unwrap();
        // during the ping
        edit(&ctx, "prod", |t| {
            t.config.firewall = Some(cli_core::target::FirewallConfig {
                cloudflare_origin: true,
            })
        });
        execute_add(&ctx, plan, &NullReporter, &CancellationToken::new()).unwrap();
        assert!(cli_core::load_target(&ctx.store(), "prod")
            .unwrap()
            .config
            .firewall
            .is_some_and(|f| f.cloudflare_origin));
    }

    /// Deviation 3: `--force` on a name that does not exist is a plain add.
    #[test]
    fn force_on_a_new_name_is_a_bounded_add() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        let plan = plan_add(
            &ctx,
            AddArgs {
                force: true,
                ..args("fresh", TOKEN_B)
            },
        )
        .unwrap();
        assert_eq!(plan.class, PlanClass::Bounded);
        assert_eq!(plan.changes[0].action, ChangeAction::Create);
    }

    /// The SKU check of a forced overwrite without `--region` runs in the stored region, not
    /// the default one.
    #[test]
    fn a_forced_sku_is_checked_in_the_stored_region() {
        let mut s = mockito::Server::new();
        let _l = route(&mut s, "/v1/locations", 200, LOCATIONS, TOKEN_B).create();
        let _t = route(&mut s, "/v1/server_types", 200, SERVER_TYPES, TOKEN_B).create();
        let (_d, ctx) = store_at(&["prod"], Some("prod"), &s.url());
        edit(&ctx, "prod", |t| t.config.region = Some("fsn1".into()));
        let r = run(
            &ctx,
            AddArgs {
                force: true,
                server_type: Some("cx32".into()),
                ..args("prod", TOKEN_B)
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

    /// Overview §3.7.3: `execute_add` checks its token before the ping and again before it takes
    /// the lock, and a cancelled add writes nothing — not even the store root. Only the second
    /// check sees a cancel that lands during the ping or the SKU check: the provider calls check
    /// before they send, not after.
    #[test]
    fn a_cancelled_add_writes_nothing_whether_cancelled_before_or_during_its_requests() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = |api: &str| Context::for_desktop(dir.path().join("store"), api);
        let nothing_written = |ctx: &Context| assert!(!ctx.store().root().exists());

        let mut s = mockito::Server::new();
        let ping = route(&mut s, "/v1/locations", 200, LOCATIONS, TOKEN_A)
            .expect(0)
            .create();
        let ctx = fresh(&s.url());
        let plan = plan_add(&ctx, args("prod", TOKEN_A)).unwrap();
        let got = execute_add(&ctx, plan, &NullReporter, &cancelled_token());
        assert!(matches!(got, Ok(Outcome::Cancelled { .. })), "{got:?}");
        ping.assert();
        nothing_written(&ctx);

        let mut s = mockito::Server::new();
        let cancel = CancellationToken::new();
        let _ping = route_cancelling(&mut s, "/v1/locations", LOCATIONS, TOKEN_A, &cancel).create();
        let ctx = fresh(&s.url());
        let plan = plan_add(&ctx, args("prod", TOKEN_A)).unwrap();
        let got = execute_add(&ctx, plan, &NullReporter, &cancel);
        assert!(
            matches!(got, Ok(Outcome::Cancelled { .. })),
            "during the ping: {got:?}"
        );
        nothing_written(&ctx);

        let mut s = mockito::Server::new();
        let cancel = CancellationToken::new();
        let _ping = route(&mut s, "/v1/locations", 200, LOCATIONS, TOKEN_A).create();
        let _types =
            route_cancelling(&mut s, "/v1/server_types", SERVER_TYPES, TOKEN_A, &cancel).create();
        let ctx = fresh(&s.url());
        let plan = plan_add(
            &ctx,
            AddArgs {
                server_type: Some("cx22".into()),
                ..args("prod", TOKEN_A)
            },
        )
        .unwrap();
        let got = execute_add(&ctx, plan, &NullReporter, &cancel);
        assert!(
            matches!(got, Ok(Outcome::Cancelled { .. })),
            "during the SKU check: {got:?}"
        );
        nothing_written(&ctx);
    }

    /// The same two checks in `execute_renew`: a cancelled renewal keeps the stored token.
    #[test]
    fn a_cancelled_renew_keeps_the_stored_token() {
        let mut s = mockito::Server::new();
        let cancel = CancellationToken::new();
        let _ping = route_cancelling(&mut s, "/v1/locations", LOCATIONS, TOKEN_B, &cancel).create();
        let (_d, ctx) = store_at(&["work"], Some("work"), &s.url());
        let w = TargetRef::named(&ctx, "work").unwrap();
        let plan = || {
            plan_renew(
                &ctx,
                &w,
                RenewArgs {
                    token: Some(SecretString::new(TOKEN_B)),
                    ssh_key: None,
                },
            )
            .unwrap()
        };
        let stored_token = || {
            cli_core::load_target(&ctx.store(), "work")
                .unwrap()
                .credentials
                .hetzner_token
        };
        for (when, token) in [("before", cancelled_token()), ("during the ping", cancel)] {
            let got = execute_renew(&ctx, plan(), &NullReporter, &token);
            assert!(
                matches!(got, Ok(Outcome::Cancelled { .. })),
                "{when}: {got:?}"
            );
            assert_eq!(stored_token().as_deref(), Some(TOKEN_A), "{when}");
        }
    }

    /// `--renew --ssh-key`: the plan names the key change (`~/` paths), the save writes the new
    /// path and the outcome says the key changed; passing the stored path again is no change.
    #[test]
    fn a_renew_with_a_new_ssh_key_saves_it_and_the_stored_key_is_no_change() {
        let (dir, ctx) = store(&["work"], Some("work"));
        let ctx = ctx.with_no_ping(true);
        let ssh = dir.path().join("home").join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        let (old, new) = (ssh.join("old.pub"), ssh.join("new.pub"));
        for k in [&old, &new] {
            std::fs::write(k, "ssh-ed25519 AAAA me@x\n").unwrap();
        }
        edit(&ctx, "work", |t| t.config.ssh_key_path = Some(old.clone()));
        let w = TargetRef::named(&ctx, "work").unwrap();
        let renew = |token: &str, key: &Path| {
            plan_renew(
                &ctx,
                &w,
                RenewArgs {
                    token: Some(SecretString::new(token)),
                    ssh_key: Some(key.to_path_buf()),
                },
            )
            .unwrap()
        };
        let target_changes = |plan: &Plan<RenewPayload>| -> Vec<(ChangeAction, String)> {
            plan.changes
                .iter()
                .filter(|c| c.kind == "Target")
                .map(|c| (c.action, c.detail.clone().unwrap_or_default()))
                .collect()
        };
        let shown = |f: &str| format!("~/{}", Path::new(".ssh").join(f).display());
        let execute =
            |plan| match execute_renew(&ctx, plan, &NullReporter, &CancellationToken::new())
                .unwrap()
            {
                Outcome::Completed { result } => result,
                o => panic!("{o:?}"),
            };

        let plan = renew(TOKEN_B, &new);
        assert_eq!(
            target_changes(&plan),
            [(
                ChangeAction::Update,
                format!("ssh key: {} → {}", shown("old.pub"), shown("new.pub"))
            )]
        );
        assert_eq!(
            plan.title,
            "Rotate the API token and change the SSH key of work"
        );
        let both = execute(plan);
        assert!(both.ssh_key_changed);
        assert_eq!(
            both.token,
            Some(Verification::Skipped {
                reason: SkipReason::NoPing
            }),
            "the credentials were written"
        );
        let t = cli_core::load_target(&ctx.store(), "work").unwrap();
        assert_eq!(
            (
                t.config.ssh_key_path.as_deref(),
                t.credentials.hetzner_token.as_deref()
            ),
            (Some(new.as_path()), Some(TOKEN_B))
        );

        let plan = renew(TOKEN_A, &new);
        assert_eq!(target_changes(&plan), [], "the stored key is no change");
        assert_eq!(plan.title, "Rotate the API token of work");
        assert!(!execute(plan).ssh_key_changed);
        assert_eq!(
            cli_core::load_target(&ctx.store(), "work")
                .unwrap()
                .config
                .ssh_key_path,
            Some(new)
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
                    token: Some(SecretString::new(TOKEN_A)),
                    ssh_key: None
                }
            ),
            Err(CoreError::RenewTokenUnchanged { .. })
        ));
        let plan = plan_renew(
            &ctx,
            &w,
            RenewArgs {
                token: Some(SecretString::new(TOKEN_B)),
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
                token: Some(SecretString::new(TOKEN_A)),
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
    /// A target `work` whose stored key is `~/.ssh/old.pub`, and a readable `~/.ssh/new.pub`;
    /// its credentials file is hand-written (a comment serde never writes), so a save that
    /// rewrote it would show. The Hetzner API is `api`.
    fn keyed_store(api: &str) -> (tempfile::TempDir, Context, PathBuf, PathBuf, String) {
        let (dir, ctx) = store_at(&["work"], Some("work"), api);
        let ssh = dir.path().join("home").join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        let (old, new) = (ssh.join("old.pub"), ssh.join("new.pub"));
        for k in [&old, &new] {
            std::fs::write(k, "ssh-ed25519 AAAA me@x\n").unwrap();
        }
        edit(&ctx, "work", |t| t.config.ssh_key_path = Some(old.clone()));
        let creds = format!("# pasted by hand\nhetzner_token: {TOKEN_A}\n");
        std::fs::write(ctx.store().target_credentials_file("work"), &creds).unwrap();
        (dir, ctx, old, new, creds)
    }

    /// D.3d verification (finding C): a refused key file says whether it was for a new target
    /// (fixed by the same add with the `.pub`) or an existing one (fixed by `--renew`): a first
    /// add is NewTargetFile, an add over a stored target (`--force`, or refused as existing) and
    /// a renewal are TargetFile.
    #[test]
    fn a_refused_key_file_says_whether_its_target_is_new() {
        let (dir, ctx) = store(&["prod"], Some("prod"));
        let private = dir.path().join("id_ed25519");
        std::fs::write(
            &private,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3Blbg==\n-----END OPENSSH PRIVATE KEY-----\n",
        )
        .unwrap();
        let source_of = |e: CoreError| match e {
            CoreError::Cli(cli_core::CliError::SshKeyNotPublic { from, .. }) => from,
            other => panic!("{other:?}"),
        };
        let add = |name: &str, force: bool| {
            plan_add(
                &ctx,
                AddArgs {
                    ssh_key: Some(private.clone()),
                    force,
                    ..args(name, TOKEN_A)
                },
            )
            .unwrap_err()
        };
        assert_eq!(source_of(add("lab", false)), KeySource::NewTargetFile);
        assert_eq!(source_of(add("prod", true)), KeySource::TargetFile);
        assert_eq!(source_of(add("prod", false)), KeySource::TargetFile);
        let renew = plan_renew(
            &ctx,
            &TargetRef::named(&ctx, "prod").unwrap(),
            renew_args(None, Some(&private)),
        )
        .unwrap_err();
        assert_eq!(source_of(renew), KeySource::TargetFile);
    }

    fn renew_args(token: Option<&str>, key: Option<&Path>) -> RenewArgs {
        RenewArgs {
            token: token.map(SecretString::new),
            ssh_key: key.map(Path::to_path_buf),
        }
    }

    fn renewed(ctx: &Context, plan: Plan<RenewPayload>) -> TargetRenewed {
        match execute_renew(ctx, plan, &NullReporter, &CancellationToken::new()).unwrap() {
            Outcome::Completed { result } => result,
            o => panic!("{o:?}"),
        }
    }

    /// WI-452: a key-only renewal (no token, or the stored one) plans only the key, asks the
    /// provider nothing, and leaves the credentials file as it was, byte for byte.
    #[test]
    fn a_key_only_renew_changes_the_key_and_never_touches_the_credentials() {
        for token in [None, Some(TOKEN_A)] {
            let mut s = mockito::Server::new();
            let ping = s
                .mock("GET", "/v1/locations")
                .match_query(mockito::Matcher::Any)
                .expect(0)
                .create();
            let (_d, ctx, old, new, creds) = keyed_store(&s.url());
            let w = TargetRef::named(&ctx, "work").unwrap();
            let plan = plan_renew(&ctx, &w, renew_args(token, Some(&new))).unwrap();
            assert_eq!(plan.class, PlanClass::Bounded, "{token:?}");
            assert_eq!(plan.title, "Change the SSH key of work", "{token:?}");
            let shown = |p: &Path| cli_core::paths::abbreviate_home(p, ctx.home_dir());
            assert_eq!(
                plan.changes
                    .iter()
                    .map(|c| (
                        c.kind.as_str(),
                        c.action,
                        c.detail.clone().unwrap_or_default()
                    ))
                    .collect::<Vec<_>>(),
                [(
                    "Target",
                    ChangeAction::Update,
                    format!("ssh key: {} → {}", shown(&old), shown(&new))
                )],
                "{token:?}: no Credentials change"
            );
            let r = renewed(&ctx, plan);
            assert_eq!((r.token, r.ssh_key_changed), (None, true), "{token:?}");
            assert_eq!(
                std::fs::read_to_string(ctx.store().target_credentials_file("work")).unwrap(),
                creds,
                "{token:?}: the credentials file is untouched"
            );
            assert_eq!(
                cli_core::load_target_config(&ctx.store(), "work")
                    .unwrap()
                    .ssh_key_path,
                Some(new),
                "{token:?}"
            );
            ping.assert();
        }
    }

    /// A new token with a new key: both planned, the token pinged, both saved.
    #[test]
    fn a_rotation_with_a_new_key_pings_and_saves_both() {
        let mut s = mockito::Server::new();
        let ping = route(&mut s, "/v1/locations", 200, LOCATIONS, TOKEN_B)
            .expect(1)
            .create();
        let (_d, ctx, _old, new, _creds) = keyed_store(&s.url());
        let w = TargetRef::named(&ctx, "work").unwrap();
        let plan = plan_renew(&ctx, &w, renew_args(Some(TOKEN_B), Some(&new))).unwrap();
        assert_eq!(
            plan.changes
                .iter()
                .map(|c| (c.kind.as_str(), c.action))
                .collect::<Vec<_>>(),
            [
                ("Credentials", ChangeAction::Replace),
                ("Target", ChangeAction::Update)
            ]
        );
        let r = renewed(&ctx, plan);
        assert!(
            matches!(r.token, Some(Verification::Verified { .. })),
            "{:?}",
            r.token
        );
        assert!(r.ssh_key_changed);
        let t = cli_core::load_target(&ctx.store(), "work").unwrap();
        assert_eq!(
            (
                t.credentials.hetzner_token.as_deref(),
                t.config.ssh_key_path
            ),
            (Some(TOKEN_B), Some(new))
        );
        ping.assert();
    }

    /// Nothing would change: with the stored token it is today's refusal, without a token its
    /// own; no key and the stored key read alike.
    #[test]
    fn a_renew_that_would_change_nothing_is_refused_by_what_was_given() {
        let (_d, ctx, old, _new, creds) = keyed_store("http://127.0.0.1:1");
        let w = TargetRef::named(&ctx, "work").unwrap();
        for key in [None, Some(old.as_path())] {
            assert!(
                matches!(
                    plan_renew(&ctx, &w, renew_args(None, key)),
                    Err(CoreError::RenewNothingToChange { ref name }) if name == "work"
                ),
                "{key:?}"
            );
            assert!(
                matches!(
                    plan_renew(&ctx, &w, renew_args(Some(TOKEN_A), key)),
                    Err(CoreError::RenewTokenUnchanged { .. })
                ),
                "{key:?}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(ctx.store().target_credentials_file("work")).unwrap(),
            creds
        );
    }

    /// Under the lock the change is worked out again: a key set to the planned one meanwhile
    /// leaves nothing to do (refused, nothing written); a token stored meanwhile leaves only the
    /// key, so the credentials are not written and the outcome says so.
    #[test]
    fn under_the_lock_a_renew_changes_only_what_still_differs() {
        let (_d, ctx, _old, new, _creds) = keyed_store("http://127.0.0.1:1");
        let ctx = ctx.with_no_ping(true);
        let w = TargetRef::named(&ctx, "work").unwrap();
        let plan = plan_renew(&ctx, &w, renew_args(None, Some(&new))).unwrap();
        edit(&ctx, "work", |t| t.config.ssh_key_path = Some(new.clone()));
        assert!(matches!(
            execute_renew(&ctx, plan, &NullReporter, &CancellationToken::new()),
            Err(CoreError::RenewNothingToChange { .. })
        ));

        let (_d, ctx, _old, new, _creds) = keyed_store("http://127.0.0.1:1");
        let ctx = ctx.with_no_ping(true);
        let w = TargetRef::named(&ctx, "work").unwrap();
        let plan = plan_renew(&ctx, &w, renew_args(Some(TOKEN_B), Some(&new))).unwrap();
        edit(&ctx, "work", |t| {
            t.credentials.hetzner_token = Some(TOKEN_B.into())
        });
        let r = renewed(&ctx, plan);
        assert_eq!((r.token, r.ssh_key_changed), (None, true));

        let (_d, ctx, _old, _new, _creds) = keyed_store("http://127.0.0.1:1");
        let ctx = ctx.with_no_ping(true);
        let w = TargetRef::named(&ctx, "work").unwrap();
        let plan = plan_renew(&ctx, &w, renew_args(Some(TOKEN_B), None)).unwrap();
        edit(&ctx, "work", |t| {
            t.credentials.hetzner_token = Some(TOKEN_B.into())
        });
        assert!(matches!(
            execute_renew(&ctx, plan, &NullReporter, &CancellationToken::new()),
            Err(CoreError::RenewTokenUnchanged { .. })
        ));
    }

    /// A key that cannot be read is refused whatever else is given, before the nothing-to-change
    /// check: a new path, and the stored path once its file is gone (passing it is no change, but
    /// it is not a key either).
    #[test]
    fn an_unreadable_key_is_refused_before_the_nothing_to_change_check() {
        let (dir, ctx, old, _new, _creds) = keyed_store("http://127.0.0.1:1");
        let w = TargetRef::named(&ctx, "work").unwrap();
        let missing = dir.path().join("missing.pub");
        std::fs::remove_file(&old).unwrap();
        for key in [&missing, &old] {
            for token in [None, Some(TOKEN_A), Some(TOKEN_B)] {
                assert!(
                    matches!(
                        plan_renew(&ctx, &w, renew_args(token, Some(key))),
                        Err(CoreError::SshKeyUnreadable { .. })
                    ),
                    "{key:?} {token:?}"
                );
            }
        }
    }

    /// The Credentials change of a renewal plan, as it names what it writes.
    fn credentials_detail(plan: &Plan<RenewPayload>) -> Option<String> {
        plan.changes
            .iter()
            .find(|c| c.kind == "Credentials")
            .and_then(|c| c.detail.clone())
    }

    /// A locked store at `<dir>/store` holding `targets`, a context that opens it with its key.
    fn locked_ctx(
        dir: &Path,
        targets: &[(&str, Option<&str>)],
    ) -> (Context, crate::secrets::testing::LockedTestStore) {
        let root = dir.join("store");
        let made = crate::secrets::testing::locked_store(&root, targets);
        let ctx = Context::for_desktop(root, "http://127.0.0.1:1")
            .with_home_dir(Some(dir.join("home")))
            .with_vault_access(made.access())
            .with_no_ping(true);
        (ctx, made)
    }

    /// D.4a T42 (d4a-d Task 13b): a clear token in a locked store's credentials file is never
    /// used — and a renewal replaces it, naming it in its plan: the vault takes the new token,
    /// the file becomes the marker again, and neither token is left in clear.
    #[test]
    fn a_renewal_replaces_a_clear_token_on_a_locked_store() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, _made) = locked_ctx(dir.path(), &[("prod", None)]);
        let creds = ctx.store().target_credentials_file("prod");
        std::fs::write(&creds, format!("hetzner_token: {TOKEN_A}\n")).unwrap();
        assert!(matches!(
            ctx.secrets().stored_token("prod"),
            Err(CliError::TokenOutsideVault { .. })
        ));
        assert!(stored_for_renewal(&ctx, "prod").unwrap().is_none());
        let target = TargetRef::named(&ctx, "prod").unwrap();
        let plan = plan_renew(&ctx, &target, renew_args(Some(TOKEN_B), None)).unwrap();
        assert_eq!(
            credentials_detail(&plan).as_deref(),
            Some("API token, replacing a token kept in clear, which this locked store never used")
        );
        assert_eq!(
            plan.payload.replaces().as_deref(),
            Some("a token kept in clear, which this locked store never used")
        );
        renewed(&ctx, plan);
        assert_eq!(
            ctx.secrets()
                .stored_token("prod")
                .unwrap()
                .unwrap()
                .expose(),
            TOKEN_B
        );
        assert_eq!(
            ctx.secrets().token_presence("prod").unwrap(),
            crate::secrets::StoredToken::InVault
        );
        let file = std::fs::read_to_string(&creds).unwrap();
        assert!(!file.contains(TOKEN_A) && !file.contains(TOKEN_B), "{file}");
    }

    /// O8's manual way back (d4a-d Task 13b): a store whose vault and store.yaml were moved
    /// aside is in files, and each credentials file still names the old vault; a renewal
    /// replaces that marker with the token typed, and names the marker it replaces.
    #[test]
    fn a_renewal_replaces_another_stores_marker_in_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let made = crate::secrets::testing::locked_store(&root, &[("prod", Some(TOKEN_A))]);
        std::fs::rename(
            root.join(crate::vault::VAULT_FILE),
            dir.path().join("vault.aside"),
        )
        .unwrap();
        std::fs::rename(
            root.join(crate::secrets::STORE_FILE),
            dir.path().join("store.aside"),
        )
        .unwrap();
        let ctx = Context::for_desktop(root, "http://127.0.0.1:1")
            .with_home_dir(Some(dir.path().join("home")))
            .with_no_ping(true);
        assert!(matches!(
            ctx.secrets().stored_token("prod"),
            Err(CliError::TokenInAnotherVault { .. })
        ));
        let target = TargetRef::named(&ctx, "prod").unwrap();
        let plan = plan_renew(&ctx, &target, renew_args(Some(TOKEN_B), None)).unwrap();
        assert_eq!(
            credentials_detail(&plan),
            Some(format!(
                "API token, replacing a marker for the vault of the target store with id {}, \
                 which this store never used",
                made.store_id
            ))
        );
        renewed(&ctx, plan);
        assert_eq!(
            ctx.secrets()
                .stored_token("prod")
                .unwrap()
                .unwrap()
                .expose(),
            TOKEN_B
        );
    }

    /// D.4a T43 (d4a-d Task 13b): a marker of this store whose vault has no entry for the
    /// target (an older AppRafter renamed it) — the renewal writes the entry.
    #[test]
    fn a_renewal_fills_a_vault_entry_an_older_rename_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, made) = locked_ctx(dir.path(), &[("prod", None)]);
        crate::secrets::marker::write_marker(&ctx.store(), "prod", &made.store_id).unwrap();
        assert!(matches!(
            ctx.secrets().stored_token("prod"),
            Err(CliError::TokenMissingFromVault { .. })
        ));
        let target = TargetRef::named(&ctx, "prod").unwrap();
        let plan = plan_renew(&ctx, &target, renew_args(Some(TOKEN_B), None)).unwrap();
        assert_eq!(
            credentials_detail(&plan).as_deref(),
            Some("API token, replacing a marker whose entry the vault of this store does not hold")
        );
        renewed(&ctx, plan);
        assert_eq!(
            ctx.secrets()
                .stored_token("prod")
                .unwrap()
                .unwrap()
                .expose(),
            TOKEN_B
        );
    }

    /// d4a-d Task 13b: only those three records are no stored token to a renewal; whatever
    /// else the store says still refuses it — a locked store nothing here can open is
    /// `store_locked` (never "no token", so a renewal never writes past a vault it could not
    /// read), an interrupted lock is `lock_incomplete`. A normal renewal names nothing.
    #[test]
    fn a_renewal_still_refuses_what_the_store_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let (opened, _made) = locked_ctx(dir.path(), &[("prod", Some(TOKEN_A))]);
        let target = TargetRef::named(&opened, "prod").unwrap();
        let plan = plan_renew(&opened, &target, renew_args(Some(TOKEN_B), None)).unwrap();
        assert_eq!(credentials_detail(&plan).as_deref(), Some("API token"));
        assert_eq!(plan.payload.replaces(), None);
        let bare = Context::for_desktop(opened.config_root().to_path_buf(), "http://127.0.0.1:1")
            .with_home_dir(Some(dir.path().join("home")))
            .with_no_ping(true);
        let target = TargetRef::named(&bare, "prod").unwrap();
        assert!(matches!(
            plan_renew(&bare, &target, renew_args(Some(TOKEN_B), None)).unwrap_err(),
            CoreError::Cli(CliError::StoreLocked { .. })
        ));
        assert!(matches!(
            stored_for_renewal(&bare, "prod").unwrap_err(),
            CoreError::Cli(CliError::StoreLocked { .. })
        ));
        let root = opened.config_root().to_path_buf();
        let mut file = crate::secrets::read_store_file(&root).unwrap().unwrap();
        file.secrets = crate::secrets::SecretsMode::Locking;
        let held = cli_core::StoreLock::exclusive(&opened.store()).unwrap();
        crate::secrets::write_store_file(&root, &file, &held).unwrap();
        drop(held);
        assert!(matches!(
            plan_renew(&opened, &target, renew_args(Some(TOKEN_B), None)).unwrap_err(),
            CoreError::Cli(CliError::LockIncomplete {
                phase: cli_core::LockPhase::Locking,
                ..
            })
        ));
    }

    /// d4a-d Task 13b: a key-only renewal over a record this store never used changes the key
    /// alone — the record stays as it was, byte for byte, and nothing claims to replace it.
    #[test]
    fn a_key_only_renewal_leaves_a_record_the_store_never_used() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, _made) = locked_ctx(dir.path(), &[("prod", None)]);
        let creds = ctx.store().target_credentials_file("prod");
        let clear = format!("hetzner_token: {TOKEN_A}\n");
        std::fs::write(&creds, &clear).unwrap();
        let key = dir.path().join("new.pub");
        std::fs::write(&key, "ssh-ed25519 AAAA me@x\n").unwrap();
        let target = TargetRef::named(&ctx, "prod").unwrap();
        let plan = plan_renew(&ctx, &target, renew_args(None, Some(&key))).unwrap();
        assert_eq!(credentials_detail(&plan), None);
        assert_eq!(plan.payload.replaces(), None);
        renewed(&ctx, plan);
        assert_eq!(std::fs::read_to_string(&creds).unwrap(), clear);
    }
}
