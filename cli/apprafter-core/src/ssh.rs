// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! SSH public keys (D.3 overview §3.7.4): the shapes a target's key and the key picker show,
//! [`inspect_key`] (what a stored key path holds) and [`check_readable`] (the check before a key
//! path is saved). The key body is never stored, only its path.

use std::path::Path;

use serde::Serialize;

use crate::context::Context;
use crate::error::{CoreError, CoreResult};

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

/// The first line of an OpenSSH public key: `<algo> <base64> [comment…]` → the algo and the
/// comment; neither when the line has fewer than two fields.
fn parse_key(body: &str) -> (Option<String>, Option<String>) {
    let parts: Vec<&str> = body
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect();
    if parts.len() < 2 {
        return (None, None);
    }
    let comment = parts[2..].join(" ");
    (
        Some(parts[0].to_string()),
        (!comment.is_empty()).then_some(comment),
    )
}

/// What the key file at `path` holds. Never fails on a missing or unreadable file: `exists`
/// and `algo` say what was found.
pub fn inspect_key(ctx: &Context, path: &Path) -> CoreResult<SshKeyInfo> {
    let (algo, _) = std::fs::read_to_string(path)
        .map(|b| parse_key(&b))
        .unwrap_or((None, None));
    Ok(SshKeyInfo {
        path: path.display().to_string(),
        display: cli_core::paths::abbreviate_home(path, ctx.home_dir()),
        exists: path.exists(),
        algo,
    })
}

/// The check `target add` and renew make before saving a key path (the body is never stored):
/// [`CoreError::SshKeyUnreadable`] when the file is missing or cannot be read.
pub fn check_readable(path: &Path) -> CoreResult<()> {
    if !path.exists() {
        return Err(CoreError::SshKeyUnreadable {
            path: path.display().to_string(),
            problem: SshKeyProblem::Missing,
            error: None,
        });
    }
    std::fs::read_to_string(path)
        .map(|_| ())
        .map_err(|e| CoreError::SshKeyUnreadable {
            path: path.display().to_string(),
            problem: SshKeyProblem::Unreadable,
            error: Some(e.to_string()),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_readable_types_missing_and_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.pub");
        match check_readable(&missing).unwrap_err() {
            CoreError::SshKeyUnreadable {
                problem: SshKeyProblem::Missing,
                error: None,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        // a directory cannot be read as a key
        match check_readable(dir.path()).unwrap_err() {
            CoreError::SshKeyUnreadable {
                problem: SshKeyProblem::Unreadable,
                error: Some(_),
                ..
            } => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn inspect_key_reports_a_missing_file_without_failing() {
        let ctx = Context::for_desktop("/unused".into(), "http://unused")
            .with_home_dir(Some("/home/op".into()));
        let k = inspect_key(&ctx, std::path::Path::new("/home/op/.ssh/gone.pub")).unwrap();
        assert_eq!(
            (k.display.as_str(), k.exists, k.algo),
            ("~/.ssh/gone.pub", false, None)
        );
    }
}
