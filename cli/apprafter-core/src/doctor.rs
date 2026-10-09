// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Doctor (D.3 overview §3.9): the report a run produces, grouped, with a typed fix per row.
//! `run` arrives in D.3c.

use serde::Serialize;

use crate::kube::KubeErrorKind;
use crate::tools::ToolId;

/// Which target doctor checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorTarget {
    /// A target by name; one that does not exist is a FAIL row, not an error.
    Named(String),
    /// The CLI's default target, if any.
    CliDefault,
}

/// What a doctor run checks; `no_ping` comes from the context.
#[derive(Debug, Clone)]
pub struct DoctorArgs {
    pub target: DoctorTarget,
}

/// The groups, in report and print order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum GroupId {
    Target,
    Cluster,
    ThisComputer,
}

/// One row's verdict. `Skipped` is a check that did not run; it counts in no total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skipped,
}

/// Which check a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CheckId {
    ActiveTarget,
    TargetExists,
    ConfigReadable,
    CredentialsFile,
    ProviderSupported,
    TokenPresent,
    TokenFormat,
    TokenVerified,
    SshKey,
    KubeconfigCached,
    KubeApiReachable,
    NodeSshReachable,
    Tool,
    Dns,
}

/// Why a target's token should be renewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum RenewWhy {
    CredentialsFileMissing,
    TokenMissing,
    TokenMalformed,
    TokenRejected,
}

/// What would fix a row, as data: the CLI words it as today's hint, the desktop as an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CheckFix {
    AddTarget {
        name: Option<String>,
        available: Vec<String>,
    },
    RenewToken {
        target: String,
        why: RenewWhy,
    },
    Chmod {
        path: String,
        mode: u32,
    },
    UnsupportedProvider {
        provider: String,
        supported: Vec<String>,
    },
    ProviderError {
        status: u16,
    },
    ProviderUnreachable,
    ConfigureSshKey {
        target: String,
    },
    SshKeyMissing {
        path: String,
    },
    InstallTool {
        tool: ToolId,
    },
    FetchKubeconfig {
        target: String,
    },
    AgeKeyMissing {
        path: String,
    },
    /// `reason`, never `kind`: that is the tag.
    ClusterUnreachable {
        reason: KubeErrorKind,
    },
    NodeUnreachable {
        address: String,
    },
    Dns {
        host: String,
    },
    /// A neutral explanation (an OS error), printed verbatim.
    Explain {
        text: String,
    },
}

/// One doctor row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub id: CheckId,
    /// The tool a `tool` row is about.
    pub tool: Option<ToolId>,
    pub status: CheckStatus,
    /// The CLI's row name, e.g. "Config file readable".
    pub title: String,
    /// Neutral facts: a path, a version line, "Hetzner Cloud /v1/locations, 182 ms".
    pub detail: Option<String>,
    pub fix: Option<CheckFix>,
}

/// One group of rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct CheckGroup {
    pub id: GroupId,
    pub checks: Vec<Check>,
}

/// What a doctor run found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    /// The target checked, when there was one.
    pub target: Option<String>,
    pub groups: Vec<CheckGroup>,
}

impl DoctorReport {
    fn count(&self, status: CheckStatus) -> usize {
        self.groups
            .iter()
            .flat_map(|g| &g.checks)
            .filter(|c| c.status == status)
            .count()
    }

    pub fn passed(&self) -> usize {
        self.count(CheckStatus::Pass)
    }

    pub fn warned(&self) -> usize {
        self.count(CheckStatus::Warn)
    }

    pub fn failed(&self) -> usize {
        self.count(CheckStatus::Fail)
    }

    /// Rows that did not run; outside the pass / warn / fail totals.
    pub fn skipped(&self) -> usize {
        self.count(CheckStatus::Skipped)
    }

    /// Whether any row failed (the CLI exits 1 on it).
    pub fn has_failures(&self) -> bool {
        self.failed() > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_counts_skipped_apart() {
        let row = |status| Check {
            id: CheckId::Tool,
            tool: Some(crate::tools::ToolId::Git),
            status,
            title: "t".into(),
            detail: None,
            fix: None,
        };
        let report = DoctorReport {
            target: None,
            groups: vec![CheckGroup {
                id: GroupId::ThisComputer,
                checks: vec![
                    row(CheckStatus::Pass),
                    row(CheckStatus::Warn),
                    row(CheckStatus::Fail),
                    row(CheckStatus::Skipped),
                ],
            }],
        };
        assert_eq!(
            (
                report.passed(),
                report.warned(),
                report.failed(),
                report.skipped()
            ),
            (1, 1, 1, 1)
        );
        assert!(report.has_failures());
    }

    #[test]
    fn an_unreachable_cluster_fix_carries_its_reason_beside_the_tag() {
        // deviation 11: `reason`, because `kind` is the tag
        let fix = CheckFix::ClusterUnreachable {
            reason: crate::kube::KubeErrorKind::Unreachable,
        };
        assert_eq!(
            serde_json::to_value(&fix).unwrap(),
            serde_json::json!({"kind":"cluster_unreachable","reason":"unreachable"})
        );
    }
}
