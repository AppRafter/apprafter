// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! SSH public keys (D.3 overview §3.7.4): the shapes a target's key and the key picker show,
//! [`public_key_candidates`] (what the picker offers), [`inspect_key`] (what a stored key path
//! holds) and [`check_readable`] (the check before a key path is saved). The key body is never
//! stored, only its path.

use std::path::{Path, PathBuf};

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

/// `<home>/.ssh/*.pub` (files only, no recursion), sorted by path. No home, no directory, or a
/// directory that cannot be listed is an empty list (the wizard then offers a typed path).
pub fn public_key_candidates(ctx: &Context) -> CoreResult<Vec<SshKeyCandidate>> {
    let Some(dir) = ctx.home_dir().map(|h| h.join(".ssh")) else {
        return Ok(Vec::new());
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("pub"))
        .collect();
    paths.sort();
    Ok(paths
        .into_iter()
        .map(|p| {
            let (algo, comment) = std::fs::read_to_string(&p)
                .map(|b| parse_key(&b))
                .unwrap_or((None, None));
            SshKeyCandidate {
                path: p.display().to_string(),
                display: cli_core::paths::abbreviate_home(&p, ctx.home_dir()),
                algo,
                comment,
            }
        })
        .collect())
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
    fn candidates_are_the_pub_files_under_home_ssh_sorted_with_algo_and_comment() {
        let home = tempfile::tempdir().unwrap();
        let ssh = home.path().join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        std::fs::write(ssh.join("work.pub"), "ssh-ed25519 AAAA me@work\n").unwrap();
        std::fs::write(ssh.join("bare.pub"), "ssh-rsa AAAA\n").unwrap();
        std::fs::write(ssh.join("junk.pub"), "garbage\n").unwrap();
        std::fs::write(ssh.join("id_ed25519"), "PRIVATE").unwrap();
        // no recursion, and a directory is no key even when its name ends in `.pub`
        std::fs::create_dir_all(ssh.join("old.pub")).unwrap();
        std::fs::create_dir_all(ssh.join("sub")).unwrap();
        std::fs::write(ssh.join("sub").join("nested.pub"), "ssh-rsa AAAA x").unwrap();
        let ctx = Context::for_desktop("/unused".into(), "http://unused")
            .with_home_dir(Some(home.path().into()));
        let c = public_key_candidates(&ctx).unwrap();
        // `~/.ssh\bare.pub` on CI's windows-latest leg (read_dir joins with the OS separator)
        let shown = |f: &str| format!("~/{}", std::path::Path::new(".ssh").join(f).display());
        let got: Vec<_> = c
            .iter()
            .map(|k| (k.display.clone(), k.algo.as_deref(), k.comment.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                (shown("bare.pub"), Some("ssh-rsa"), None),
                (shown("junk.pub"), None, None),
                (shown("work.pub"), Some("ssh-ed25519"), Some("me@work"))
            ]
        );
        assert_eq!(c[0].path, ssh.join("bare.pub").display().to_string());
    }

    #[test]
    fn no_home_or_no_ssh_dir_is_no_candidates() {
        let ctx = Context::for_desktop("/unused".into(), "http://unused").with_home_dir(None);
        assert!(public_key_candidates(&ctx).unwrap().is_empty());
        let empty = tempfile::tempdir().unwrap();
        let ctx = ctx.with_home_dir(Some(empty.path().into()));
        assert!(public_key_candidates(&ctx).unwrap().is_empty());
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
