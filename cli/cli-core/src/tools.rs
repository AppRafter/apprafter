// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! External binaries the CLI shells out to, and the check that runs
//! before a command does anything it cannot take back.
//!
//! # Why this exists
//!
//! The CLI spawns `restic`, `kubectl`, `helm`, `git` and `ssh`. When one
//! is missing the spawn fails with `os error 2`, and the audit recorded
//! as D11 in `docs/measurements/day2-followups.md` found that the check
//! for it runs *after* the expensive part of the command in eight
//! places. The reported case:
//!
//! ```text
//! $ apprafter backup list
//! > Backup passphrase: ********
//! Error: apprafter::cli::other
//!   × spawn restic: No such file or directory (os error 2)
//! ```
//!
//! The operator typed a secret into a command that could not have
//! worked. The sharpest instance is worse: `restore --reprovision`
//! gates the passphrase deliberately — there is a comment explaining
//! that a bad passphrase must not leave a re-provisioned cluster
//! half-restored — and does not gate the binary, so a missing `restic`
//! costs a paid, provisioned Hetzner cluster before anything notices.
//!
//! So the rule this module exists to enforce is an ordering one:
//! **[`preflight_tool`] runs before any prompt, any cluster round-trip
//! and any billable provider call.**
//!
//! # Why a PATH scan and not a spawn
//!
//! `preflight_restic_version` (the one pre-existing check of this shape)
//! runs `restic version` and reads `ErrorKind::NotFound` off the spawn.
//! That works, but it costs a process per command and assumes the tool
//! has a cheap, side-effect-free subcommand. Resolving the name against
//! `PATH` answers the same question without executing anything, and the
//! resolution itself is a pure function ([`find_on_path`]) that tests
//! without touching a filesystem.
//!
//! Version checking is deliberately *not* folded in here. "Is it
//! installed" and "is it new enough" fail differently and are worth
//! different messages; `preflight_restic_version` keeps owning the
//! second question for the one command that needs it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::error::CliError;

/// One external binary the CLI depends on, with the install line shown
/// when it is missing.
///
/// The install hint is per-tool rather than generic because "install
/// restic" and "install kubectl" have nothing useful in common, and a
/// message that says only "not found" leaves the reader exactly where
/// the raw `os error 2` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool {
    /// Executable name as spawned (no path, no extension).
    pub name: &'static str,
    /// What the CLI uses it for — one clause, lowercase, no trailing
    /// stop. Rendered as "`{name}` is required by `{needed_by}`".
    pub purpose: &'static str,
    /// Platform-agnostic install guidance, already wrapped.
    pub install: &'static str,
}

/// Restic — every backup and restore path.
pub const RESTIC: Tool = Tool {
    name: "restic",
    purpose: "backup and restore",
    install: "Install restic (>= 0.14):\n  \
              • macOS      brew install restic\n  \
              • Debian     apt install restic\n  \
              • Arch       pacman -S restic\n  \
              • Nix        nix profile install nixpkgs#restic\n  \
              • other      https://restic.readthedocs.io/en/stable/020_installation.html",
};

/// kubectl — every command that talks to a cluster.
pub const KUBECTL: Tool = Tool {
    name: "kubectl",
    purpose: "talking to the cluster",
    install: "Install kubectl:\n  \
              • macOS      brew install kubectl\n  \
              • Debian     apt install kubectl\n  \
              • Nix        nix profile install nixpkgs#kubectl\n  \
              • other      https://kubernetes.io/docs/tasks/tools/",
};

/// Helm — chart installs during bootstrap.
pub const HELM: Tool = Tool {
    name: "helm",
    purpose: "installing platform charts",
    install: "Install helm:\n  \
              • macOS      brew install helm\n  \
              • Debian     apt install helm\n  \
              • Nix        nix profile install nixpkgs#kubernetes-helm\n  \
              • other      https://helm.sh/docs/intro/install/",
};

/// Git — repository probes and scaffolding.
pub const GIT: Tool = Tool {
    name: "git",
    purpose: "reading the application repository",
    install: "Install git:\n  \
              • macOS      xcode-select --install\n  \
              • Debian     apt install git\n  \
              • Nix        nix profile install nixpkgs#git\n  \
              • other      https://git-scm.com/downloads",
};

/// SSH — node preparation over the provider's public IP.
pub const SSH: Tool = Tool {
    name: "ssh",
    purpose: "reaching the node over SSH",
    install: "Install an OpenSSH client:\n  \
              • macOS      preinstalled\n  \
              • Debian     apt install openssh-client\n  \
              • Nix        nix profile install nixpkgs#openssh",
};

/// Every tool this CLI can spawn.
///
/// `doctor` derives its checked list from this rather than carrying its
/// own, so a new dependency cannot be added without appearing there —
/// the gap D11 recorded, where `restic` had eight spawn sites, was fatal
/// on all of them, and was checked nowhere.
pub const ALL: &[Tool] = &[RESTIC, KUBECTL, HELM, GIT, SSH];

/// Resolve `name` against a `PATH`-shaped variable.
///
/// Pure: `is_executable` decides what counts, so the search order and
/// the empty-entry handling are testable without a filesystem. An empty
/// `PATH` entry means "the current directory" to POSIX shells; it is
/// deliberately **skipped** here rather than honoured, because resolving
/// a platform tool out of the user's cwd is a footgun, not a feature.
pub fn find_on_path<F>(name: &str, path_var: &OsStr, is_executable: F) -> Option<PathBuf>
where
    F: Fn(&Path) -> bool,
{
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Whether `path` is a file this process could execute.
///
/// On Unix that is "regular file with any execute bit set". Elsewhere
/// the mode bits do not exist, so existence as a file is the best
/// available answer.
pub fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Assert `tool` is available, naming the command that needs it.
///
/// Call this **first** in any command that will spawn `tool` — before a
/// prompt, before a kubeconfig, before a provider call. `needed_by` is
/// the user-facing command path (`"apprafter backup list"`), so the
/// error names the thing the reader typed rather than a function.
pub fn preflight_tool(tool: &Tool, needed_by: &str) -> Result<PathBuf, CliError> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    find_on_path(tool.name, &path, is_executable_file).ok_or_else(|| {
        CliError::ExternalToolNotFound {
            tool: tool.name.to_string(),
            needed_by: needed_by.to_string(),
            purpose: tool.purpose.to_string(),
            install: tool.install.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn it_returns_the_first_match_in_path_order() {
        // PATH order is the whole contract: an operator who puts a
        // newer binary earlier expects that one.
        let found = find_on_path("restic", &os("/a:/b:/c"), |p| {
            p == Path::new("/b/restic") || p == Path::new("/c/restic")
        });
        assert_eq!(found, Some(PathBuf::from("/b/restic")));
    }

    #[test]
    fn it_returns_none_when_nothing_matches() {
        assert!(find_on_path("restic", &os("/a:/b"), |_| false).is_none());
    }

    #[test]
    fn it_skips_empty_path_entries_rather_than_searching_cwd() {
        // A trailing colon means "cwd" to a POSIX shell. Resolving a
        // platform tool out of the working directory is a footgun, so
        // the empty entry must not become a candidate — note the probe
        // would happily accept a bare relative "restic".
        let found = find_on_path("restic", &os("/a::/b"), |p| p == Path::new("restic"));
        assert!(found.is_none(), "empty PATH entry was searched: {found:?}");
    }

    #[test]
    fn a_missing_tool_names_the_command_that_needed_it() {
        // The failure the whole module exists for: the reader must
        // learn which command they typed is blocked, not which
        // function returned.
        let err = preflight_tool(
            &Tool {
                name: "definitely-not-a-real-binary-9f3a",
                purpose: "a test",
                install: "install it",
            },
            "apprafter backup list",
        )
        .expect_err("a nonexistent binary must not resolve");
        let rendered = err.to_string();
        assert!(rendered.contains("apprafter backup list"), "{rendered}");
        assert!(
            rendered.contains("definitely-not-a-real-binary-9f3a"),
            "{rendered}"
        );
    }

    #[test]
    fn every_tool_carries_an_install_hint_and_a_purpose() {
        // A "not found" message with no install line leaves the reader
        // exactly where the raw `os error 2` did.
        for t in ALL {
            assert!(!t.name.is_empty());
            assert!(
                t.install.len() > 20,
                "`{}` needs a real install hint, not a label",
                t.name
            );
            assert!(
                !t.purpose.is_empty() && !t.purpose.ends_with('.'),
                "`{}` purpose is a clause without a trailing stop",
                t.name
            );
        }
    }

    #[test]
    fn the_tool_table_covers_every_binary_the_cli_spawns() {
        // Guards the D11 gap directly: restic had eight spawn sites, was
        // fatal on all of them, and appeared in no checked list. If a
        // new binary is introduced, it belongs here before it is spawned.
        let names: Vec<&str> = ALL.iter().map(|t| t.name).collect();
        for expected in ["restic", "kubectl", "helm", "git", "ssh"] {
            assert!(names.contains(&expected), "`{expected}` missing from ALL");
        }
    }
}
