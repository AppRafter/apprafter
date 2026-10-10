// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! SSH public keys (D.3 overview §3.7.4): the shapes a target's key and the key picker show,
//! [`public_key_candidates`] (what the picker offers), [`inspect_key`] (what a stored key path
//! holds) and [`check_readable`] (the check before a key path is saved). The key body is never
//! stored, only its path. A key is an OpenSSH public key line or nothing
//! (`cli_core::ssh_key`, GOTCHA-149): the provider is sent whatever the file holds, so a
//! private key is refused by name and never shown as a key.

use std::path::{Path, PathBuf};

use serde::Serialize;

use cli_core::ssh_key::{parse_public_key, NotAPublicKey};

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
    /// The key type, e.g. `ssh-ed25519`, when the file reads as an OpenSSH public key; never
    /// otherwise.
    pub algo: Option<String>,
    /// Why the file cannot be the target's key; `None` when it is a public key.
    pub problem: Option<SshKeyProblem>,
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

/// Why an SSH key path cannot be used. [`CoreError::SshKeyUnreadable`] carries the first two
/// (a key file `check_readable` cannot read); a file it reads that is not a public key is
/// `CliError::SshKeyNotPublic`. [`inspect_key`] reports any of the four.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SshKeyProblem {
    Missing,
    Unreadable,
    /// A private key: its public half is the `.pub` file next to it.
    PrivateKey,
    /// Readable, and not one OpenSSH public key line.
    NotPublicKey,
}

impl SshKeyProblem {
    /// The snake_case name the desktop's error fields carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
            Self::PrivateKey => "private_key",
            Self::NotPublicKey => "not_public_key",
        }
    }

    /// The CLI's `verify_ssh_key_readable` texts, and `CliError::SshKeyNotPublic`'s.
    pub fn message(self, path: &str, error: Option<&str>) -> String {
        match self {
            Self::Missing => format!("SSH key path `{path}` does not exist"),
            Self::Unreadable => format!(
                "SSH key `{path}` is not readable: {}",
                error.unwrap_or("unknown error")
            ),
            Self::PrivateKey | Self::NotPublicKey => not_public(self, Path::new(path)).to_string(),
        }
    }
}

/// The refusal of a readable key file at `path` that is not a public key.
fn not_public(problem: SshKeyProblem, path: &Path) -> cli_core::CliError {
    let why = if problem == SshKeyProblem::PrivateKey {
        NotAPublicKey::PrivateKey
    } else {
        NotAPublicKey::Other
    };
    why.refusal(cli_core::ssh_key::file_origin(path))
}

impl From<NotAPublicKey> for SshKeyProblem {
    fn from(why: NotAPublicKey) -> Self {
        match why {
            NotAPublicKey::PrivateKey => Self::PrivateKey,
            NotAPublicKey::Other => Self::NotPublicKey,
        }
    }
}

/// A typed key path with a leading `~/` expanded into `home` (the context's home directory), as
/// a shell would before the CLI saw it: the CLI wizard's typed path and the desktop's (whose
/// field suggests `~/.ssh/id_ed25519.pub`) both pass through here before a key is inspected or
/// saved, so the stored path is the absolute one. Other tilde forms (`~user/`) are left as
/// typed, so the path stays predictable; with no home, the input is returned as typed.
pub fn expand_tilde(input: &str, home: Option<&Path>) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = home {
            // One component per `join`, so the platform's separator sits between each of them:
            // the saved path reads `C:\Users\a\.ssh\id.pub` on Windows, not `…\.ssh/id.pub`.
            return rest
                .split('/')
                .filter(|part| !part.is_empty())
                .fold(home.to_path_buf(), |path, part| path.join(part));
        }
    }
    PathBuf::from(input)
}

/// An OpenSSH public key's algo and comment; neither for anything else.
fn parse_key(body: &str) -> (Option<String>, Option<String>) {
    match parse_public_key(body) {
        Ok(line) => (Some(line.algo), line.comment),
        Err(_) => (None, None),
    }
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

/// What the key file at `path` holds. Never fails on a missing, unreadable or wrong file:
/// `exists`, `algo` and `problem` say what was found — `algo` only for a public key.
pub fn inspect_key(ctx: &Context, path: &Path) -> CoreResult<SshKeyInfo> {
    let (algo, problem) = match std::fs::read_to_string(path) {
        Ok(body) => match parse_public_key(&body) {
            Ok(line) => (Some(line.algo), None),
            Err(why) => (None, Some(why.into())),
        },
        Err(_) if !path.exists() => (None, Some(SshKeyProblem::Missing)),
        Err(_) => (None, Some(SshKeyProblem::Unreadable)),
    };
    Ok(SshKeyInfo {
        path: path.display().to_string(),
        display: cli_core::paths::abbreviate_home(path, ctx.home_dir()),
        exists: path.exists(),
        algo,
        problem,
    })
}

/// The check `target add` and renew make before saving a key path (the body is never stored):
/// [`CoreError::SshKeyUnreadable`] when the file is missing or cannot be read, and
/// `CliError::SshKeyNotPublic` when it is a private key or anything but one OpenSSH public key
/// line — that file would be sent to the provider as is (GOTCHA-149).
pub fn check_readable(path: &Path) -> CoreResult<()> {
    if !path.exists() {
        return Err(CoreError::SshKeyUnreadable {
            path: path.display().to_string(),
            problem: SshKeyProblem::Missing,
            error: None,
        });
    }
    let body = std::fs::read_to_string(path).map_err(|e| CoreError::SshKeyUnreadable {
        path: path.display().to_string(),
        problem: SshKeyProblem::Unreadable,
        error: Some(e.to_string()),
    })?;
    parse_public_key(&body)
        .map(|_| ())
        .map_err(|why| CoreError::Cli(not_public(why.into(), path)))
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

    /// `~/` expands against the context's home and nothing else: no home leaves the input as
    /// typed, and `~user/` is never expanded.
    #[test]
    fn tilde_expands_against_the_contexts_home_only() {
        let home = Path::new("/home/op");
        assert_eq!(
            expand_tilde("~/.ssh/k.pub", Some(home)),
            PathBuf::from("/home/op/.ssh/k.pub")
        );
        assert_eq!(
            expand_tilde("~/.ssh/k.pub", None),
            PathBuf::from("~/.ssh/k.pub")
        );
        assert_eq!(expand_tilde("~bob/k", Some(home)), PathBuf::from("~bob/k"));
        assert_eq!(
            expand_tilde("/etc/ssh/host_key.pub", Some(home)),
            PathBuf::from("/etc/ssh/host_key.pub")
        );
        // Written with the platform's separator throughout (the display a GUI shows and a
        // config file keeps), whatever separator the input used after `~`.
        assert_eq!(
            expand_tilde("~/.ssh//k.pub", Some(home))
                .display()
                .to_string(),
            home.join(".ssh").join("k.pub").display().to_string()
        );
    }

    #[test]
    fn inspect_key_reports_a_missing_file_without_failing() {
        let ctx = Context::for_desktop("/unused".into(), "http://unused")
            .with_home_dir(Some("/home/op".into()));
        let k = inspect_key(&ctx, std::path::Path::new("/home/op/.ssh/gone.pub")).unwrap();
        assert_eq!(
            (k.display.as_str(), k.exists, k.algo, k.problem),
            ("~/.ssh/gone.pub", false, None, Some(SshKeyProblem::Missing))
        );
    }

    const PRIVATE: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjE=\n-----END OPENSSH PRIVATE KEY-----\n";

    /// GOTCHA-149: a private key's first line split into fields read as the type
    /// `-----BEGIN`, and the desktop showed it as a key. Only a public key has a type now, and
    /// the problem says why any other file is not one.
    #[test]
    fn inspect_key_names_a_type_only_for_a_public_key_and_says_why_not() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::for_desktop("/unused".into(), "http://unused")
            .with_home_dir(Some(dir.path().into()));
        let file = |name: &str, body: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, body).unwrap();
            p
        };
        let seen = |p: &std::path::Path| {
            let k = inspect_key(&ctx, p).unwrap();
            (k.exists, k.algo, k.problem)
        };
        assert_eq!(
            seen(&file("id_ed25519", PRIVATE)),
            (true, None, Some(SshKeyProblem::PrivateKey))
        );
        assert_eq!(
            seen(&file("notes.pub", "not-a-key\n")),
            (true, None, Some(SshKeyProblem::NotPublicKey))
        );
        assert_eq!(
            seen(&file("id_ed25519.pub", "ssh-ed25519 AAAA me@host\n")),
            (true, Some("ssh-ed25519".into()), None)
        );
        assert_eq!(
            seen(dir.path()),
            (true, None, Some(SshKeyProblem::Unreadable)),
            "a directory cannot be read as a key"
        );
    }

    /// GOTCHA-149: add and renew refuse to save a path whose file is a private key (by name,
    /// pointing at the public half) or not a public key at all; the file is never quoted.
    #[test]
    fn check_readable_refuses_a_private_key_and_anything_but_a_public_key() {
        let dir = tempfile::tempdir().unwrap();
        for (body, private) in [(PRIVATE, true), ("ssh-dss AAAA old\n", false)] {
            let p = dir.path().join("key");
            std::fs::write(&p, body).unwrap();
            let err = check_readable(&p).unwrap_err();
            let ui = crate::error::UiError::from(&err);
            assert_eq!(
                ui.code.as_deref(),
                Some("apprafter::target::ssh_key_not_public")
            );
            assert_eq!(
                ui.fields["privateKey"],
                serde_json::json!(private),
                "{body}"
            );
            assert!(!ui.message.contains(body.lines().nth(1).unwrap_or("AAAA")));
            assert!(
                ui.message
                    .starts_with(&format!("SSH key `{}` is ", p.display())),
                "{}",
                ui.message
            );
        }
        let p = dir.path().join("key.pub");
        std::fs::write(&p, "ssh-ed25519 AAAA me@host\n").unwrap();
        check_readable(&p).unwrap();
    }
}
