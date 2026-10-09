// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The external tools the core runs (D.3 overview §3.8): which they are, how to install them,
//! and what probing them found. The resolver and the toolchain report arrive in D.3a's later
//! tasks; D.3c wires them into doctor.

use serde::Serialize;

use crate::context::PathSource;

/// One external tool, in the CLI's probe order in [`ToolId::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ToolId {
    Kubectl,
    Helm,
    Restic,
    Git,
    Ssh,
    Cue,
}

impl ToolId {
    /// Every tool, in the order doctor and the toolchain panel list them.
    pub const ALL: [ToolId; 6] = [
        ToolId::Restic,
        ToolId::Kubectl,
        ToolId::Helm,
        ToolId::Git,
        ToolId::Ssh,
        ToolId::Cue,
    ];

    /// The CLI's definition of the tool: name, purpose, install lines, version arguments.
    pub fn spec(self) -> &'static cli_core::tools::Tool {
        use cli_core::tools as t;
        match self {
            Self::Restic => &t::RESTIC,
            Self::Kubectl => &t::KUBECTL,
            Self::Helm => &t::HELM,
            Self::Git => &t::GIT,
            Self::Ssh => &t::SSH,
            Self::Cue => &t::CUE,
        }
    }

    /// The executable name, e.g. `kubectl`.
    pub fn name(self) -> &'static str {
        self.spec().name
    }
}

/// Which system an install hint is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HintOs {
    Windows,
    Macos,
    Debian,
    Arch,
    Nix,
    Other,
}

impl From<cli_core::tools::InstallOs> for HintOs {
    fn from(os: cli_core::tools::InstallOs) -> Self {
        use cli_core::tools::InstallOs as I;
        match os {
            I::Windows => Self::Windows,
            I::Macos => Self::Macos,
            I::Debian => Self::Debian,
            I::Arch => Self::Arch,
            I::Nix => Self::Nix,
            I::Other => Self::Other,
        }
    }
}

/// One install line: a command, a URL, or "preinstalled".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InstallHint {
    pub os: HintOs,
    pub command: String,
}

/// Why a tool is not usable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ToolProblem {
    NotFound,
    /// Found only as a `.cmd` / `.bat` shim, which cannot be run directly (Windows).
    Unsupported {
        path: String,
    },
    /// It ran but printed no version; `exit` is its exit code, when it had one.
    NoVersionOutput {
        exit: Option<i32>,
    },
    TimedOut,
    SpawnFailed {
        error: String,
    },
}

/// What probing one tool found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub tool: ToolId,
    pub required: bool,
    pub purpose: String,
    pub path: Option<String>,
    /// The first line of the tool's version output.
    pub version: Option<String>,
    pub problem: Option<ToolProblem>,
    pub install: Vec<InstallHint>,
}

/// Every tool, and where they were looked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ToolchainReport {
    pub tools: Vec<ToolStatus>,
    pub search_path: Vec<String>,
    pub search_path_source: PathSource,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_id_has_its_cli_spec_in_probe_order() {
        let names: Vec<_> = ToolId::ALL.iter().map(|t| t.name()).collect();
        assert_eq!(names, ["restic", "kubectl", "helm", "git", "ssh", "cue"]);
        assert!(ToolId::Kubectl.spec().required);
    }
}
