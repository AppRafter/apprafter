// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! External binaries the CLI shells out to, and the check that runs
//! before a command does anything it cannot take back.
//!
//! # Why this exists
//!
//! The CLI spawns `restic`, `kubectl`, `helm`, `git`, `ssh` and `cue`. When one
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
    /// Platform-agnostic install guidance, already wrapped: exactly
    /// [`render_install`] of [`Tool::install_header`] and [`Tool::hints`]
    /// (a unit test pins it), kept as text because the CLI prints it.
    pub install: &'static str,
    /// The first line of [`Tool::install`], e.g. `"Install helm:"`.
    pub install_header: &'static str,
    /// The per-system lines of [`Tool::install`], in print order.
    pub hints: &'static [InstallLine],
    /// Whether the CLI's core path is unusable without it.
    ///
    /// Only `kubectl` is `true`: every cluster-facing command spawns it,
    /// so its absence is not a warning about a feature, it is the CLI
    /// not working. The rest are feature-scoped — an operator who never
    /// backs up genuinely does not need `restic` — so they warn rather
    /// than fail, and the warning names the capability that is
    /// unavailable instead of implying the install is mandatory.
    ///
    /// This distinction is what `doctor` reports on; D11's complaint was
    /// that a missing `kubectl` printed "Ready to go" and exited 0.
    pub required: bool,
    /// Arguments that make the tool print its version. `ssh -V` writes
    /// to stderr and some tools exit non-zero, which `check_tool`
    /// tolerates — any output at all counts as present.
    ///
    /// PRESENCE ONLY. The version is asked for and then thrown away:
    /// nothing here declares or compares a minimum, and `RESTIC`'s
    /// `install` string below already promises one ("restic (>= 0.14)")
    /// that nothing enforces. That gap is not cosmetic — these are the
    /// binaries that run on the OPERATOR's machine, which is the one
    /// place with no pin at all. CI pins govern CI and image pins govern
    /// the cluster, but `cluster-bootstrap` shells out to the user's
    /// `helm` and `app validate` to the user's `cue`, and a cue older
    /// than the `language.version` the render workspace declares does
    /// not warn, it hard-rejects. Tracked as WI-369.
    pub version_args: &'static [&'static str],
}

/// Which system an install line is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOs {
    Windows,
    Macos,
    Debian,
    Arch,
    Nix,
    Other,
}

impl InstallOs {
    /// The label printed before the line, e.g. `macOS`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Macos => "macOS",
            Self::Debian => "Debian",
            Self::Arch => "Arch",
            Self::Nix => "Nix",
            Self::Other => "other",
        }
    }
}

/// One install line: a command, a URL, or "preinstalled".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallLine {
    pub os: InstallOs,
    pub text: &'static str,
}

/// `header`, then one `  • <label padded to 10> <text>` line per hint — the text `install`
/// carries and the CLI prints.
pub fn render_install(header: &str, hints: &[InstallLine]) -> String {
    let mut out = header.to_string();
    for h in hints {
        out.push_str(&format!("\n  • {:<10} {}", h.os.label(), h.text));
    }
    out
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
              • Windows    winget install restic.restic\n  \
              • other      https://restic.readthedocs.io/en/stable/020_installation.html",
    install_header: "Install restic (>= 0.14):",
    hints: &[
        InstallLine {
            os: InstallOs::Macos,
            text: "brew install restic",
        },
        InstallLine {
            os: InstallOs::Debian,
            text: "apt install restic",
        },
        InstallLine {
            os: InstallOs::Arch,
            text: "pacman -S restic",
        },
        InstallLine {
            os: InstallOs::Nix,
            text: "nix profile install nixpkgs#restic",
        },
        InstallLine {
            os: InstallOs::Windows,
            text: "winget install restic.restic",
        },
        InstallLine {
            os: InstallOs::Other,
            text: "https://restic.readthedocs.io/en/stable/020_installation.html",
        },
    ],
    required: false,
    version_args: &["version"],
};

/// kubectl — every command that talks to a cluster.
pub const KUBECTL: Tool = Tool {
    name: "kubectl",
    purpose: "talking to the cluster",
    install: "Install kubectl:\n  \
              • macOS      brew install kubectl\n  \
              • Debian     apt install kubectl\n  \
              • Nix        nix profile install nixpkgs#kubectl\n  \
              • Windows    winget install Kubernetes.kubectl\n  \
              • other      https://kubernetes.io/docs/tasks/tools/",
    install_header: "Install kubectl:",
    hints: &[
        InstallLine {
            os: InstallOs::Macos,
            text: "brew install kubectl",
        },
        InstallLine {
            os: InstallOs::Debian,
            text: "apt install kubectl",
        },
        InstallLine {
            os: InstallOs::Nix,
            text: "nix profile install nixpkgs#kubectl",
        },
        InstallLine {
            os: InstallOs::Windows,
            text: "winget install Kubernetes.kubectl",
        },
        InstallLine {
            os: InstallOs::Other,
            text: "https://kubernetes.io/docs/tasks/tools/",
        },
    ],
    required: true,
    version_args: &["version", "--client"],
};

/// Helm — chart installs during bootstrap.
pub const HELM: Tool = Tool {
    name: "helm",
    purpose: "installing platform charts",
    install: "Install helm:\n  \
              • macOS      brew install helm\n  \
              • Debian     apt install helm\n  \
              • Nix        nix profile install nixpkgs#kubernetes-helm\n  \
              • Windows    winget install Helm.Helm\n  \
              • other      https://helm.sh/docs/intro/install/",
    install_header: "Install helm:",
    hints: &[
        InstallLine {
            os: InstallOs::Macos,
            text: "brew install helm",
        },
        InstallLine {
            os: InstallOs::Debian,
            text: "apt install helm",
        },
        InstallLine {
            os: InstallOs::Nix,
            text: "nix profile install nixpkgs#kubernetes-helm",
        },
        InstallLine {
            os: InstallOs::Windows,
            text: "winget install Helm.Helm",
        },
        InstallLine {
            os: InstallOs::Other,
            text: "https://helm.sh/docs/intro/install/",
        },
    ],
    required: false,
    version_args: &["version", "--short"],
};

/// Git — repository probes and scaffolding.
pub const GIT: Tool = Tool {
    name: "git",
    purpose: "reading the application repository",
    install: "Install git:\n  \
              • macOS      xcode-select --install\n  \
              • Debian     apt install git\n  \
              • Nix        nix profile install nixpkgs#git\n  \
              • Windows    winget install Git.Git\n  \
              • other      https://git-scm.com/downloads",
    install_header: "Install git:",
    hints: &[
        InstallLine {
            os: InstallOs::Macos,
            text: "xcode-select --install",
        },
        InstallLine {
            os: InstallOs::Debian,
            text: "apt install git",
        },
        InstallLine {
            os: InstallOs::Nix,
            text: "nix profile install nixpkgs#git",
        },
        InstallLine {
            os: InstallOs::Windows,
            text: "winget install Git.Git",
        },
        InstallLine {
            os: InstallOs::Other,
            text: "https://git-scm.com/downloads",
        },
    ],
    required: false,
    version_args: &["--version"],
};

/// SSH — node preparation over the provider's public IP.
pub const SSH: Tool = Tool {
    name: "ssh",
    purpose: "reaching the node over SSH",
    install: "Install an OpenSSH client:\n  \
              • macOS      preinstalled\n  \
              • Debian     apt install openssh-client\n  \
              • Nix        nix profile install nixpkgs#openssh\n  \
              • Windows    built into Windows 10/11: Settings › Optional features › OpenSSH Client",
    install_header: "Install an OpenSSH client:",
    hints: &[
        InstallLine {
            os: InstallOs::Macos,
            text: "preinstalled",
        },
        InstallLine {
            os: InstallOs::Debian,
            text: "apt install openssh-client",
        },
        InstallLine {
            os: InstallOs::Nix,
            text: "nix profile install nixpkgs#openssh",
        },
        InstallLine {
            os: InstallOs::Windows,
            text: "built into Windows 10/11: Settings › Optional features › OpenSSH Client",
        },
    ],
    required: false,
    version_args: &["-V"],
};

/// Every tool this CLI can spawn.
///
/// `doctor` derives its checked list from this rather than carrying its
/// own, so a new dependency cannot be added without appearing there —
/// the gap D11 recorded, where `restic` had eight spawn sites, was fatal
/// on all of them, and was checked nowhere.
pub const ALL: &[Tool] = &[RESTIC, KUBECTL, HELM, GIT, SSH, CUE];

/// cue — `app validate`, which checks an application manifest locally.
pub const CUE: Tool = Tool {
    name: "cue",
    purpose: "validating application manifests",
    install: "Install cue:\n  \
              • macOS      brew install cue\n  \
              • Arch       pacman -S cue\n  \
              • Nix        nix profile install nixpkgs#cue\n  \
              • Windows    winget install CueLang.Cue\n  \
              • other      https://cuelang.org/docs/introduction/installation/",
    install_header: "Install cue:",
    hints: &[
        InstallLine {
            os: InstallOs::Macos,
            text: "brew install cue",
        },
        InstallLine {
            os: InstallOs::Arch,
            text: "pacman -S cue",
        },
        InstallLine {
            os: InstallOs::Nix,
            text: "nix profile install nixpkgs#cue",
        },
        InstallLine {
            os: InstallOs::Windows,
            text: "winget install CueLang.Cue",
        },
        InstallLine {
            os: InstallOs::Other,
            text: "https://cuelang.org/docs/introduction/installation/",
        },
    ],
    required: false,
    version_args: &["version"],
};

/// The file names `name` may have on disk: itself on Unix; on Windows
/// `name.exe` when `name` has no `.` in it, else `name` as given. That is
/// the rule Rust's `Command` applies when it searches `PATH` on Windows —
/// `.exe` is appended to a name without an extension, `PATHEXT` is not
/// read — so a tool found here is the file `Command` would start, and a
/// name given with its extension (`kubectl.exe`, a `kubectl.cmd` shim) is
/// looked for exactly as given.
pub fn executable_names(name: &str, windows: bool) -> Vec<String> {
    if windows && !name.contains('.') {
        vec![format!("{name}.exe")]
    } else {
        vec![name.to_string()]
    }
}

/// Resolve `name` against a `PATH`-shaped variable, under the file
/// names an executable of that name has on this platform
/// ([`executable_names`]).
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
    find_on_path_with(
        &executable_names(name, cfg!(windows)),
        path_var,
        is_executable,
    )
}

/// [`find_on_path`] with the candidate file names given explicitly, so
/// the Windows lookup is testable on any host. Directories are searched
/// in `PATH` order and, within one directory, `names` in order.
pub fn find_on_path_with<F>(names: &[String], path_var: &OsStr, is_executable: F) -> Option<PathBuf>
where
    F: Fn(&Path) -> bool,
{
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for name in names {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
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

/// Assert every tool in `tools` is available, in order.
///
/// Reports the **first** missing one rather than collecting all of them:
/// the reader installs it and re-runs, and a list of four things to
/// install reads as a bigger problem than "you are missing restic".
/// Order the slice so the most likely omission comes first.
pub fn preflight_tools(tools: &[&Tool], needed_by: &str) -> Result<(), CliError> {
    for tool in tools {
        preflight_tool(tool, needed_by)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// A `PATH` value in this platform's syntax (`:` on Unix, `;` on
    /// Windows), so the search tests mean the same thing on both.
    fn path_of(dirs: &[&str]) -> OsString {
        std::env::join_paths(dirs).expect("test dirs hold no separator")
    }

    /// The file `find_on_path` probes for `stem` on this platform:
    /// `stem` itself on Unix, `stem.exe` on Windows.
    fn exe(stem: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("{stem}.exe"))
        } else {
            PathBuf::from(stem)
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn it_returns_the_first_match_in_path_order() {
        // PATH order is the whole contract: an operator who puts a
        // newer binary earlier expects that one.
        let found = find_on_path("restic", &path_of(&["/a", "/b", "/c"]), |p| {
            p == exe("/b/restic") || p == exe("/c/restic")
        });
        assert_eq!(found, Some(exe("/b/restic")));
    }

    #[test]
    fn it_returns_none_when_nothing_matches() {
        assert!(find_on_path("restic", &path_of(&["/a", "/b"]), |_| false).is_none());
    }

    #[test]
    fn it_skips_empty_path_entries_rather_than_searching_cwd() {
        // A trailing colon means "cwd" to a POSIX shell. Resolving a
        // platform tool out of the working directory is a footgun, so
        // the empty entry must not become a candidate — note the probe
        // would happily accept a bare relative "restic".
        let found = find_on_path("restic", &path_of(&["/a", "", "/b"]), |p| {
            p == exe("restic")
        });
        assert!(found.is_none(), "empty PATH entry was searched: {found:?}");
    }

    #[test]
    fn executable_names_add_exe_on_windows_only() {
        assert_eq!(executable_names("kubectl", false), names(&["kubectl"]));
        assert_eq!(executable_names("kubectl", true), names(&["kubectl.exe"]));
        // A name that already carries the extension, in any case, is
        // not given a second one.
        assert_eq!(
            executable_names("kubectl.exe", true),
            names(&["kubectl.exe"])
        );
        assert_eq!(
            executable_names("KUBECTL.EXE", true),
            names(&["KUBECTL.EXE"])
        );
    }

    #[test]
    fn a_windows_name_with_any_extension_is_looked_for_as_given() {
        // `Command`'s own rule: a `.` anywhere in the name means it has
        // an extension, so nothing is appended — a `.cmd` shim named
        // with its extension is found, and so is a dotted name.
        assert_eq!(
            executable_names("kubectl.cmd", true),
            names(&["kubectl.cmd"])
        );
        assert_eq!(
            executable_names("k3s.kubectl", true),
            names(&["k3s.kubectl"])
        );
        // Unix never adds anything.
        assert_eq!(
            executable_names("kubectl.cmd", false),
            names(&["kubectl.cmd"])
        );
    }

    #[test]
    fn a_dot_exe_is_found_only_under_the_windows_names() {
        // The defect this pins: on Windows the binary on disk is
        // `kubectl.exe`, and probing for a bare `kubectl` reported every
        // installed tool as missing.
        let path = path_of(&["/a", "/b"]);
        let only_exe = |p: &Path| p == Path::new("/b/kubectl.exe");
        assert_eq!(
            find_on_path_with(&executable_names("kubectl", true), &path, only_exe),
            Some(PathBuf::from("/b/kubectl.exe"))
        );
        assert_eq!(
            find_on_path_with(&executable_names("kubectl", false), &path, only_exe),
            None
        );
        // And the Windows names do not fall back to an extensionless
        // file: `Command` could not start one.
        let only_bare = |p: &Path| p == Path::new("/b/kubectl");
        assert_eq!(
            find_on_path_with(&executable_names("kubectl", true), &path, only_bare),
            None
        );
    }

    #[test]
    fn find_on_path_probes_for_this_platforms_file_name() {
        // `find_on_path` is `find_on_path_with` over the names of the
        // platform this binary was built for.
        let found = find_on_path("kubectl", &path_of(&["/a", "/b"]), |p| {
            p == Path::new("/b/kubectl.exe")
        });
        assert_eq!(found.is_some(), cfg!(windows), "{found:?}");
    }

    #[test]
    fn the_install_text_is_the_rendering_of_header_and_hints() {
        for t in ALL {
            assert_eq!(
                t.install,
                render_install(t.install_header, t.hints),
                "`{}`",
                t.name
            );
        }
    }

    #[test]
    fn render_install_aligns_the_os_labels() {
        let lines = [
            InstallLine {
                os: InstallOs::Macos,
                text: "brew install x",
            },
            InstallLine {
                os: InstallOs::Other,
                text: "https://x",
            },
        ];
        assert_eq!(
            render_install("Install x:", &lines),
            "Install x:\n  • macOS      brew install x\n  • other      https://x"
        );
    }

    #[test]
    fn every_tool_names_its_windows_install() {
        // winget ids verified against microsoft/winget-pkgs manifests on 2026-10-09
        // (manifests/{r/restic/restic,k/Kubernetes/kubectl,h/Helm/Helm,g/Git/Git,c/CueLang/Cue});
        // a wrong id would be a published wrong instruction.
        let expected = [
            ("restic", "winget install restic.restic"),
            ("kubectl", "winget install Kubernetes.kubectl"),
            ("helm", "winget install Helm.Helm"),
            ("git", "winget install Git.Git"),
            (
                "ssh",
                "built into Windows 10/11: Settings › Optional features › OpenSSH Client",
            ),
            ("cue", "winget install CueLang.Cue"),
        ];
        assert_eq!(ALL.len(), expected.len());
        for (tool, (name, line)) in ALL.iter().zip(expected) {
            assert_eq!(tool.name, name);
            let windows: Vec<&str> = tool
                .hints
                .iter()
                .filter(|h| h.os == InstallOs::Windows)
                .map(|h| h.text)
                .collect();
            assert_eq!(windows, vec![line], "`{}`", tool.name);
            assert!(
                tool.install.contains(&format!("• Windows    {line}")),
                "`{}` install text lacks its Windows line:\n{}",
                tool.name,
                tool.install
            );
        }
    }

    #[test]
    fn cue_is_checked_last_and_is_optional() {
        // The doctor row order is ALL's order: restic, kubectl, helm, git, ssh, cue (overview §3.9).
        assert_eq!(ALL.last(), Some(&CUE));
        // Checked when the test compiles: a `required` cue would fail the build of the tests.
        const { assert!(!CUE.required, "only `app validate` needs cue") };
        assert_eq!(CUE.version_args, &["version"]);
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
                install_header: "",
                hints: &[],
                required: true,
                version_args: &["--version"],
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
            assert!(
                !t.version_args.is_empty(),
                "`{}` needs version args so `doctor` can report which one is installed",
                t.name
            );
        }
    }

    #[test]
    fn kubectl_is_the_only_required_tool() {
        // The judgement this table encodes, stated so a change to it is
        // deliberate: kubectl is spawned by every cluster-facing command,
        // so its absence is the CLI not working. The rest are
        // feature-scoped and an operator who never backs up genuinely
        // does not need restic — warning there, failing here.
        let required: Vec<&str> = ALL.iter().filter(|t| t.required).map(|t| t.name).collect();
        assert_eq!(required, vec!["kubectl"], "required set changed");
    }

    #[test]
    fn the_tool_table_covers_every_binary_the_cli_spawns() {
        // Guards the D11 gap directly: restic had eight spawn sites, was
        // fatal on all of them, and appeared in no checked list. If a
        // new binary is introduced, it belongs here before it is spawned.
        let names: Vec<&str> = ALL.iter().map(|t| t.name).collect();
        for expected in ["restic", "kubectl", "helm", "git", "ssh", "cue"] {
            assert!(names.contains(&expected), "`{expected}` missing from ALL");
        }
    }
}
