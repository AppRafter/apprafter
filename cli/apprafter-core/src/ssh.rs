// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! SSH public keys (D.3 overview §3.7.4): the shapes a target's key and the key picker show.
//! Their functions (`public_key_candidates`, `inspect_key`, `check_readable`) arrive in D.3b.

use serde::Serialize;

/// A target's SSH public key as the clients show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SshKeyInfo {
    pub path: String,
    /// The path with the home directory shown as `~/`.
    pub display: String,
    pub exists: bool,
    /// The key type, e.g. `ssh-ed25519`, when the file reads as a public key.
    pub algo: Option<String>,
}

/// A public key found under `~/.ssh`, offered by the key picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SshKeyCandidate {
    pub path: String,
    pub display: String,
    pub algo: Option<String>,
    pub comment: Option<String>,
}

/// Why an SSH key path cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshKeyProblem {
    Missing,
    Unreadable,
}

impl SshKeyProblem {
    /// The snake_case name the desktop's error fields carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
        }
    }

    /// The CLI's `verify_ssh_key_readable` texts.
    pub fn message(self, path: &str, error: Option<&str>) -> String {
        match self {
            Self::Missing => format!("SSH key path `{path}` does not exist"),
            Self::Unreadable => format!(
                "SSH key `{path}` is not readable: {}",
                error.unwrap_or("unknown error")
            ),
        }
    }
}
