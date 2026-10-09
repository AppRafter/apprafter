// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The core target API (D.3 overview §3.7): the report and outcome types both clients show, the
//! name rule, and the reads `hetzner_token` and `public_address`; `list`, `show` and the
//! mutations arrive in D.3b.

pub mod name;
mod read;

pub use name::{validate_name, NameProblem, TARGET_NAME_MAX_LEN};
pub use read::{hetzner_token, public_address};

use serde::Serialize;

use crate::error::UiError;
use crate::provider::Verification;
use crate::ssh::SshKeyInfo;
use crate::target_ref::ActivePointerChange;

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

/// What renewing a target's token did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TargetRenewed {
    pub name: String,
    pub token: Verification,
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
