// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! whoami (D.3 overview §3.7.5): who the clients act as and which target the CLI's default
//! points at. `whoami` arrives in D.3b.

use serde::Serialize;

use crate::provider::Verification;
use crate::ssh::SshKeyInfo;

/// Who the clients act as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Identity {
    /// No account: the self-hosted mode.
    AnonymousSelfHosted,
}

/// The CLI default target, as whoami shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct WhoamiTarget {
    pub name: String,
    pub provider: String,
    pub verification: Verification,
    pub region: Option<String>,
    pub server_type: Option<String>,
    pub default_tier: Option<String>,
    pub cluster_name: Option<String>,
    pub ssh_key: Option<SshKeyInfo>,
}

/// Where the CLI's default target pointer leads. The GUI shows `Missing` as a row; the CLI
/// turns it into its `TargetNotFound` error after the identity line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CliDefaultTarget {
    None,
    Missing {
        name: String,
        available: Vec<String>,
    },
    Found {
        target: WhoamiTarget,
    },
}

/// What whoami reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct WhoamiReport {
    pub identity: Identity,
    pub cli_default: CliDefaultTarget,
}
