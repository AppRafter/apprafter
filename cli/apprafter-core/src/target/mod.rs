// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The core target API (D.3 overview §3.7): the report and outcome types both clients show, the
//! name rule, the reads (`list`, `show`, `hetzner_token`, `public_address`) and the helpers the
//! mutations share.

mod add;
mod machine;
pub mod name;
mod pointer;
mod read;
mod remove;
mod rename;
#[cfg(test)]
pub(crate) mod testkit;

pub use add::{
    execute_add, execute_renew, plan_add, plan_renew, AddArgs, AddPayload, RenewArgs, RenewPayload,
};
pub use machine::{execute_machine, plan_machine, rebuild_recipe, MachineChoice, MachinePayload};
pub use name::{validate_name, NameProblem, TARGET_NAME_MAX_LEN};
pub use pointer::{execute_use, plan_use, UsePayload};
pub use read::{hetzner_token, list, public_address, show};
pub use remove::{execute_remove, plan_remove, RemovePayload};
pub use rename::{execute_rename, plan_rename, RenamePayload};

use cli_core::{StoreLock, StoreLockEvent};
use serde::Serialize;

use crate::context::Context;
use crate::error::{CoreResult, UiError};
use crate::op::{ChangeAction, Outcome, PlannedChange};
use crate::provider::Verification;
use crate::report::{Event, Reporter};
use crate::ssh::SshKeyInfo;
use crate::target_ref::{ActivePointerChange, TargetRef};

/// One row of the target list (read from `config.yaml` only: no credentials).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TargetSummary {
    pub name: String,
    pub provider: String,
    pub region: Option<String>,
    pub server_type: Option<String>,
    /// Stored verbatim (`solo`, `team`, `prod`, `regulated`, or anything else).
    pub default_tier: Option<String>,
    /// 1..=4 when `default_tier` names a tier.
    pub tier_level: Option<u8>,
    pub is_cli_default: bool,
}

/// A target whose files could not be read; listed, never hidden.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct UnreadableTarget {
    pub name: String,
    pub error: UiError,
}

/// Where the CLI's default target pointer (`config.yaml`) leads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CliDefaultPointer {
    Unset,
    Set {
        name: String,
    },
    /// The pointer names a target the store does not have.
    Missing {
        name: String,
    },
}

/// Every target in the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TargetListReport {
    pub targets: Vec<TargetSummary>,
    pub unreadable: Vec<UnreadableTarget>,
    pub cli_default: CliDefaultPointer,
}

/// The server a target's local state records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ProvisionedServer {
    pub server_id: u64,
    pub server_name: String,
    pub server_type: Option<String>,
}

/// What a target's local state (`state/<name>/.apprafter/state.json`) says about its server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ProvisionedState {
    NotProvisioned,
    Provisioned { server: ProvisionedServer },
    Unreadable { error: UiError },
}

/// Whether a token is stored, and its length — never the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TokenPresence {
    pub set: bool,
    pub chars: Option<u32>,
}

/// One target in full, as `target show` and the Target screen show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TargetReport {
    pub name: String,
    pub is_cli_default: bool,
    pub provider: String,
    pub region: Option<String>,
    pub server_type: Option<String>,
    pub default_tier: Option<String>,
    pub tier_level: Option<u8>,
    pub cluster_name: Option<String>,
    pub ssh_key: Option<SshKeyInfo>,
    pub token: TokenPresence,
    pub config_file: String,
    pub credentials_file: String,
    pub provisioned: ProvisionedState,
}

/// A provisioned server's public addresses, read from the provider by id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PublicAddress {
    pub server_id: u64,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
}

/// Whether a machine type was checked against the provider's catalogue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SkuCheck {
    Validated {
        sku: String,
        region: String,
        /// No region was given or stored, so the default one was checked.
        region_was_default: bool,
    },
    /// Not checked: the context asked for no provider round-trip.
    NotValidated { sku: String },
}

/// What `target add` did. Became the CLI default: `cli_default` is `Some`; already it:
/// `cli_default: None, is_cli_default: true`; neither: `is_cli_default: false`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TargetAdded {
    pub name: String,
    pub replaced: bool,
    pub is_cli_default: bool,
    /// `Some` only when the pointer moved.
    pub cli_default: Option<ActivePointerChange>,
    /// `Verified`, or `Skipped { NoPing }`.
    pub token: Verification,
    pub sku: Option<SkuCheck>,
}

/// What a renewal did: the token it saved, the SSH key it changed, or both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TargetRenewed {
    pub name: String,
    /// How the new token was checked (`Verified`, or `Skipped { NoPing }`); `None` when the
    /// stored credentials were kept as they were (not written).
    pub token: Option<Verification>,
    pub ssh_key_changed: bool,
}

/// What `target use` did; `pointer: None` when it was already the default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TargetUsed {
    pub name: String,
    pub pointer: Option<ActivePointerChange>,
}

/// What `target rename` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TargetRenamed {
    pub from: String,
    pub to: String,
    pub state_moved: bool,
    pub cli_default: Option<ActivePointerChange>,
}

/// What `target remove` did; `orphaned_server` keeps running at the provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TargetRemoved {
    pub name: String,
    pub state_removed: bool,
    pub orphaned_server: Option<ProvisionedServer>,
    pub cli_default: Option<ActivePointerChange>,
}

/// What `target machine` set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct MachineSet {
    pub name: String,
    pub sku: String,
    pub region: Option<String>,
    pub sku_check: SkuCheck,
}

/// The event a store-lock report becomes. The CLI prints a `Notice` verbatim and a `Warning`
/// as `warning: <message>`, which is exactly what it printed before the core existed.
pub fn store_lock_event(event: &StoreLockEvent<'_>) -> Event {
    match event {
        StoreLockEvent::Waiting { sentinel } => Event::Notice {
            message: format!(
                "waiting for another AppRafter process to release the target store ({})…",
                sentinel.display()
            ),
        },
        StoreLockEvent::Unlocked { sentinel, error } => Event::Warning {
            message: format!(
                "cannot lock the target store ({}): {error}; continuing without the lock",
                sentinel.display()
            ),
        },
    }
}

/// The store lock, creating the root (an add on a fresh store must lock it). Never held across
/// the network: every `execute_*` does its provider calls first.
pub(crate) fn lock_store(ctx: &Context, reporter: &dyn Reporter) -> CoreResult<StoreLock> {
    Ok(StoreLock::exclusive_or_wait(&ctx.store(), |e| {
        reporter.report(store_lock_event(&e))
    })?)
}

/// The store lock, or none when the root does not exist (a command that will fail on a missing
/// store must not create one by locking it).
pub(crate) fn lock_store_if_present(
    ctx: &Context,
    reporter: &dyn Reporter,
) -> CoreResult<Option<StoreLock>> {
    if ctx.config_root().exists() {
        lock_store(ctx, reporter).map(Some)
    } else {
        Ok(None)
    }
}

/// What `state/<name>/.apprafter/state.json` records about a server. Reads only that file —
/// the legacy `<cwd>/.apprafter` migration is the CLI's (spec §3.1). Lockless: state files are
/// replaced atomically.
pub fn provisioned(ctx: &Context, target: &TargetRef) -> CoreResult<Option<ProvisionedServer>> {
    let paths = cli_state::StatePaths::for_active_target(&ctx.store(), target.name());
    Ok(cli_state::State::load_or_default(&paths)?
        .hetzner_cloud
        .map(|h| ProvisionedServer {
            server_id: h.server_id,
            server_name: h.server_name,
            server_type: h.server_type,
        }))
}

/// The CLI-default pointer as on disk: `None` when `config.yaml` is absent or empty (R1) —
/// never `GlobalConfig::default()`'s `"default"`.
pub(crate) fn cli_default(ctx: &Context) -> CoreResult<Option<String>> {
    Ok(cli_core::resolve_active_target_name(&ctx.store(), None)?)
}

/// An operation stopped by its token before it changed anything.
pub(crate) fn cancelled<T>() -> Outcome<T> {
    Outcome::Cancelled {
        cleaned: Vec::new(),
        left: Vec::new(),
    }
}

/// One line of a plan.
pub(crate) fn change(
    kind: &str,
    object: &str,
    action: ChangeAction,
    detail: Option<String>,
) -> PlannedChange {
    PlannedChange {
        kind: kind.into(),
        object: object.into(),
        action,
        detail,
    }
}

#[cfg(test)]
mod helper_tests {
    use super::testkit::*;
    use super::*;
    use crate::report::{CollectReporter, Event};
    use std::time::{Duration, Instant};

    #[test]
    fn a_lock_event_is_a_notice_or_a_warning_with_todays_words() {
        let sentinel = std::path::Path::new("/s/.lock");
        assert_eq!(
            store_lock_event(&cli_core::StoreLockEvent::Waiting { sentinel }),
            Event::Notice {
                message:
                    "waiting for another AppRafter process to release the target store (/s/.lock)…"
                        .into()
            }
        );
        let error = std::io::Error::other("Read-only file system");
        assert_eq!(
            store_lock_event(&cli_core::StoreLockEvent::Unlocked {
                sentinel,
                error: &error
            }),
            Event::Warning {
                message: "cannot lock the target store (/s/.lock): Read-only file system; \
                          continuing without the lock"
                    .into()
            }
        );
    }

    #[test]
    fn a_held_store_is_waited_for_and_the_wait_is_reported() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        let held = cli_core::StoreLock::exclusive(&ctx.store()).unwrap();
        let reporter = std::sync::Arc::new(CollectReporter::new());
        let (r, c) = (reporter.clone(), ctx.clone());
        let waiter = std::thread::spawn(move || lock_store(&c, &*r).map(|_| ()));
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        while seen.is_empty() {
            assert!(Instant::now() < deadline, "the wait was never reported");
            seen.extend(reporter.take());
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(held);
        waiter.join().unwrap().unwrap();
        assert!(
            matches!(&seen[0], Event::Notice { message } if message.starts_with("waiting for another"))
        );
    }

    #[test]
    fn no_store_root_means_no_lock_and_no_root_created() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::for_desktop(dir.path().join("absent"), "http://unused");
        assert!(lock_store_if_present(&ctx, &crate::NullReporter)
            .unwrap()
            .is_none());
        assert!(!dir.path().join("absent").exists());
    }

    #[test]
    fn provisioned_reads_each_targets_own_state() {
        let (_d, ctx) = store(&["prod", "dev"], Some("prod"));
        seed_server(&ctx, "prod", 42, "platform-1", Some("cx22"));
        let prod = TargetRef::named(&ctx, "prod").unwrap();
        let dev = TargetRef::named(&ctx, "dev").unwrap();
        assert_eq!(
            provisioned(&ctx, &prod).unwrap(),
            Some(ProvisionedServer {
                server_id: 42,
                server_name: "platform-1".into(),
                server_type: Some("cx22".into())
            })
        );
        assert_eq!(provisioned(&ctx, &dev).unwrap(), None);
    }

    #[test]
    fn a_corrupt_state_is_the_state_corrupt_error() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        seed_state_raw(&ctx, "prod", "{not json");
        let e = provisioned(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap_err();
        assert_eq!(
            UiError::from(&e).code.as_deref(),
            Some("apprafter::state::corrupt")
        );
    }
}
