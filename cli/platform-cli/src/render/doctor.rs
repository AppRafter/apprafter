// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter doctor`'s text from the core's report: one header per group, a line per check
//! (`✓` `⚠` `✗`, and `–` for a check that did not run), the CLI's hint per fix, and the summary,
//! whose totals leave skipped checks out (R8). The words of every hint that existed before the
//! core are kept exactly; the desktop turns the same fixes into actions.

use std::fmt::Write as _;

use apprafter_core::doctor::{
    Check, CheckFix, CheckId, CheckStatus, DoctorReport, GroupId, RenewWhy,
};
use apprafter_core::kube::KubeErrorKind;
use cli_core::diagnose::KubectlFailure;
use cli_core::style;

/// The whole report as `apprafter doctor` prints it.
pub(crate) fn render(report: &DoctorReport) -> String {
    let mut out = String::new();
    for group in &report.groups {
        match (&group.id, &report.target) {
            (GroupId::Target, Some(name)) => writeln!(out, "Checking target `{name}`..."),
            (GroupId::Target, None) => writeln!(out, "Checking target..."),
            (GroupId::Cluster, _) => writeln!(out, "Checking cluster..."),
            (GroupId::ThisComputer, _) => writeln!(out, "Checking environment..."),
        }
        .expect("writing to a String cannot fail");
        for check in &group.checks {
            line(&mut out, check);
        }
        out.push('\n');
    }
    out.push_str(&summary(report));
    out
}

/// `  <glyph> <title> (<detail>)`, then the hint on its own indented line. `owo-colors`
/// colours the glyph only on a TTY, so piped output and the goldens are plain.
fn line(out: &mut String, c: &Check) {
    let glyph = match c.status {
        CheckStatus::Pass => style::ok("✓"),
        CheckStatus::Warn => style::warn("⚠"),
        CheckStatus::Fail => style::fail("✗"),
        CheckStatus::Skipped => style::dim("–"),
    };
    // `writeln!` into a String cannot fail.
    let _ = match &c.detail {
        Some(d) => writeln!(out, "  {glyph} {} ({d})", c.title),
        None => writeln!(out, "  {glyph} {}", c.title),
    };
    if let Some(h) = hint(c) {
        let _ = writeln!(out, "      hint: {h}");
    }
}

/// The last line: skipped checks are in no total (R8).
fn summary(report: &DoctorReport) -> String {
    let (p, w, f) = (report.passed(), report.warned(), report.failed());
    let total = p + w + f;
    let blurb = report
        .target
        .as_ref()
        .map(|n| format!(" for target `{n}`"))
        .unwrap_or_default();
    if f > 0 {
        format!(
            "{total} checks{blurb}: {p} passed, {w} warning(s), {f} FAIL — fix the FAILs and \
             rerun `apprafter doctor`.\n"
        )
    } else if w > 0 {
        format!(
            "{total} checks{blurb}: {p} passed, {w} warning(s). Ready to go; review warnings if \
             they apply to your use case.\n"
        )
    } else {
        format!(
            "{total} checks{blurb}: {p} passed. All good — ready for `apprafter init` / \
             `apprafter apply`.\n"
        )
    }
}

/// The CLI's words for a check's fix.
pub(crate) fn hint(c: &Check) -> Option<String> {
    Some(match c.fix.as_ref()? {
        CheckFix::AddTarget { name: None, .. } => {
            "none configured. Run `apprafter target add <name>` to set one up, then re-run \
             `apprafter doctor` for the target-side checks."
                .to_string()
        }
        CheckFix::AddTarget {
            name: Some(n),
            available,
        } => format!(
            "available targets: {}. Run `apprafter target add {n}` to create.",
            available.join(", ")
        ),
        CheckFix::RenewToken { target, why } => {
            match why {
                RenewWhy::CredentialsFileMissing => format!(
                "run `apprafter target add {target} --renew --token <X>` to create credentials.yaml"
            ),
                RenewWhy::TokenMissing => {
                    format!("run `apprafter target add {target} --renew --token <X>` to add credentials")
                }
                RenewWhy::TokenMalformed => {
                    format!("run `apprafter target add {target} --renew --token <X>` with a fresh token")
                }
                RenewWhy::TokenRejected => format!(
                "token rejected; run `apprafter target add {target} --renew` with a fresh token \
                 from Hetzner Cloud Console → Security → API Tokens"
            ),
            }
        }
        CheckFix::Chmod { path, mode } => {
            format!("fix with `chmod {mode:o} {path}` so other local users can't read your token")
        }
        CheckFix::UnsupportedProvider { supported, .. } => format!(
            "supported in this build: {}. Future plugins may add more.",
            supported.join(", ")
        ),
        CheckFix::ProviderError { .. } => {
            "provider API returned an unexpected error — retry, then check Hetzner status page"
                .to_string()
        }
        CheckFix::ProviderUnreachable => {
            "could not reach the provider API — check DNS / network / proxy, or rerun with \
             `--no-ping` to skip this check"
                .to_string()
        }
        CheckFix::ConfigureSshKey { .. } => {
            "no SSH key in target config — `apprafter init` / `apply` will refuse until you set \
             one via `apprafter target add <name> --force --ssh-key <path>` or via the wizard"
                .to_string()
        }
        CheckFix::SshKeyMissing { .. } => {
            "file does not exist; the path stored in target config may be stale".to_string()
        }
        // The tool's own row: what it is needed for and how to install it. A detail means the
        // resolver found something it cannot run (a `.cmd` shim on Windows).
        CheckFix::InstallTool { tool } if c.id == CheckId::Tool => {
            let spec = tool.spec();
            let lead = if c.detail.is_none() {
                "not found"
            } else {
                "not runnable"
            };
            format!("{lead} — needed for {}.\n{}", spec.purpose, spec.install)
        }
        // Another row that needs a tool points at that tool's row for the install lines.
        CheckFix::InstallTool { tool } => format!(
            "`{0}` is needed to reach the cluster; the `{0}` row under `Checking environment` \
             says how to install it",
            tool.name()
        ),
        CheckFix::FetchKubeconfig { target } if c.status == CheckStatus::Fail => format!(
            "run `apprafter kubeconfig --target {target}` to fetch it from the node and cache it \
             encrypted"
        ),
        CheckFix::FetchKubeconfig { target } => format!(
            "run `apprafter kubeconfig --refresh --target {target}` to replace the unencrypted \
             copy with an encrypted one"
        ),
        // Not `--refresh`: it decrypts the cached copy first, so it cannot recover a lost key.
        CheckFix::AgeKeyMissing { path } => format!(
            "no age key at `{path}`, so the cached kubeconfig cannot be decrypted: restore the \
             key it was cached with, or point `APPRAFTER_AGE_KEY` at it"
        ),
        CheckFix::ClusterUnreachable { reason } => cluster_hint(*reason),
        CheckFix::NodeUnreachable { address } => format!(
            "nothing accepted a connection on port 22 at {address}: check that the server is \
             running (`apprafter target ip`) and that no firewall between you and it blocks \
             port 22"
        ),
        CheckFix::Dns { host } => format!(
            "the resolver could not answer for `{host}`: check your DNS settings, VPN or proxy"
        ),
        CheckFix::Explain { text } => text.clone(),
    })
}

/// The Kube API row's hint, from how kubectl's failure was classified.
fn cluster_hint(reason: KubeErrorKind) -> String {
    match reason {
        KubeErrorKind::Unreachable => KubectlFailure::Unreachable
            .hint()
            .unwrap_or("the cluster's API server did not answer")
            .to_string(),
        KubeErrorKind::Forbidden => {
            "the API server answered and refused the cached credential — a re-provisioned \
             cluster leaves the old kubeconfig behind; run `apprafter kubeconfig --refresh \
             --target <name>`"
                .to_string()
        }
        KubeErrorKind::KindNotServed | KubeErrorKind::ObjectNotFound | KubeErrorKind::Other => {
            "kubectl could not read the API server's version; its own message is in the detail \
             above"
                .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apprafter_core::doctor::CheckGroup;
    use apprafter_core::tools::ToolId;

    /// owo-colors colours only a TTY; strip ESC sequences so the test holds either way.
    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn check(
        id: CheckId,
        status: CheckStatus,
        title: &str,
        detail: Option<&str>,
        fix: Option<CheckFix>,
    ) -> Check {
        Check {
            id,
            tool: None,
            status,
            title: title.into(),
            detail: detail.map(str::to_string),
            fix,
        }
    }

    #[test]
    fn renders_groups_rows_hints_and_the_summary() {
        let kubectl = ToolId::Kubectl.spec();
        let report = DoctorReport {
            target: Some("prod".into()),
            groups: vec![
                CheckGroup {
                    id: GroupId::Target,
                    checks: vec![
                        check(
                            CheckId::ConfigReadable,
                            CheckStatus::Pass,
                            "Config file readable",
                            Some("/c/config.yaml"),
                            None,
                        ),
                        check(
                            CheckId::TokenVerified,
                            CheckStatus::Skipped,
                            "Token verified against provider API",
                            Some("not requested"),
                            None,
                        ),
                        check(
                            CheckId::SshKey,
                            CheckStatus::Fail,
                            "SSH key readable",
                            Some("/k.pub"),
                            Some(CheckFix::SshKeyMissing {
                                path: "/k.pub".into(),
                            }),
                        ),
                    ],
                },
                CheckGroup {
                    id: GroupId::ThisComputer,
                    checks: vec![Check {
                        tool: Some(ToolId::Kubectl),
                        ..check(
                            CheckId::Tool,
                            CheckStatus::Fail,
                            "`kubectl` on PATH",
                            None,
                            Some(CheckFix::InstallTool {
                                tool: ToolId::Kubectl,
                            }),
                        )
                    }],
                },
            ],
        };
        let want = format!(
            "Checking target `prod`...\n\
             \x20 ✓ Config file readable (/c/config.yaml)\n\
             \x20 – Token verified against provider API (not requested)\n\
             \x20 ✗ SSH key readable (/k.pub)\n\
             \x20     hint: file does not exist; the path stored in target config may be stale\n\
             \n\
             Checking environment...\n\
             \x20 ✗ `kubectl` on PATH\n\
             \x20     hint: not found — needed for {}.\n{}\n\
             \n\
             3 checks for target `prod`: 1 passed, 0 warning(s), 2 FAIL — fix the FAILs and rerun `apprafter doctor`.\n",
            kubectl.purpose, kubectl.install
        );
        assert_eq!(plain(&render(&report)), want);
    }

    #[test]
    fn the_cluster_group_has_its_own_header() {
        let report = DoctorReport {
            target: Some("prod".into()),
            groups: vec![CheckGroup {
                id: GroupId::Cluster,
                checks: vec![],
            }],
        };
        assert!(plain(&render(&report)).starts_with("Checking cluster...\n"));
    }

    #[test]
    fn no_target_has_the_bare_header_and_no_blurb() {
        let report = DoctorReport {
            target: None,
            groups: vec![CheckGroup {
                id: GroupId::Target,
                checks: vec![check(
                    CheckId::ActiveTarget,
                    CheckStatus::Warn,
                    "active target",
                    None,
                    Some(CheckFix::AddTarget {
                        name: None,
                        available: vec![],
                    }),
                )],
            }],
        };
        let out = plain(&render(&report));
        assert!(
            out.starts_with("Checking target...\n  ⚠ active target\n      hint: none configured."),
            "{out}"
        );
        assert!(
            out.ends_with(
                "1 checks: 0 passed, 1 warning(s). Ready to go; review warnings if they apply to your use case.\n"
            ),
            "{out}"
        );
    }

    #[test]
    fn a_clean_run_is_all_good() {
        let report = DoctorReport {
            target: Some("prod".into()),
            groups: vec![CheckGroup {
                id: GroupId::ThisComputer,
                checks: vec![
                    check(
                        CheckId::Dns,
                        CheckStatus::Pass,
                        "DNS",
                        Some("443/tcp"),
                        None,
                    ),
                    check(
                        CheckId::Dns,
                        CheckStatus::Skipped,
                        "x",
                        Some("not requested"),
                        None,
                    ),
                ],
            }],
        };
        assert!(plain(&render(&report)).ends_with(
            "1 checks for target `prod`: 1 passed. All good — ready for `apprafter init` / `apprafter apply`.\n"
        ));
    }

    #[test]
    fn todays_hints_keep_todays_words() {
        let cases: Vec<(CheckFix, &str)> =
            vec![
            (
                CheckFix::AddTarget {
                    name: Some("ghost".into()),
                    available: vec!["a".into(), "b".into()],
                },
                "available targets: a, b. Run `apprafter target add ghost` to create.",
            ),
            (
                CheckFix::AddTarget {
                    name: None,
                    available: vec![],
                },
                "none configured. Run `apprafter target add <name>` to set one up, then re-run \
                 `apprafter doctor` for the target-side checks.",
            ),
            (
                CheckFix::RenewToken {
                    target: "p".into(),
                    why: RenewWhy::CredentialsFileMissing,
                },
                "run `apprafter target add p --renew --token <X>` to create credentials.yaml",
            ),
            (
                CheckFix::RenewToken {
                    target: "p".into(),
                    why: RenewWhy::TokenMissing,
                },
                "run `apprafter target add p --renew --token <X>` to add credentials",
            ),
            (
                CheckFix::RenewToken {
                    target: "p".into(),
                    why: RenewWhy::TokenMalformed,
                },
                "run `apprafter target add p --renew --token <X>` with a fresh token",
            ),
            (
                CheckFix::RenewToken {
                    target: "p".into(),
                    why: RenewWhy::TokenRejected,
                },
                "token rejected; run `apprafter target add p --renew` with a fresh token from \
                 Hetzner Cloud Console → Security → API Tokens",
            ),
            (
                CheckFix::Chmod {
                    path: "/c".into(),
                    mode: 0o600,
                },
                "fix with `chmod 600 /c` so other local users can't read your token",
            ),
            (
                CheckFix::UnsupportedProvider {
                    provider: "x".into(),
                    supported: vec!["hetzner-cloud".into()],
                },
                "supported in this build: hetzner-cloud. Future plugins may add more.",
            ),
            (
                CheckFix::ProviderError { status: 500 },
                "provider API returned an unexpected error — retry, then check Hetzner status page",
            ),
            (
                CheckFix::ProviderUnreachable,
                "could not reach the provider API — check DNS / network / proxy, or rerun with \
                 `--no-ping` to skip this check",
            ),
            (
                CheckFix::ConfigureSshKey { target: "p".into() },
                "no SSH key in target config — `apprafter init` / `apply` will refuse until you \
                 set one via `apprafter target add <name> --force --ssh-key <path>` or via the \
                 wizard",
            ),
            (
                CheckFix::SshKeyMissing { path: "/k".into() },
                "file does not exist; the path stored in target config may be stale",
            ),
            (CheckFix::Explain { text: "as is".into() }, "as is"),
        ];
        for (fix, want) in cases {
            let c = check(
                CheckId::ConfigReadable,
                CheckStatus::Fail,
                "t",
                None,
                Some(fix.clone()),
            );
            assert_eq!(hint(&c).as_deref(), Some(want), "{fix:?}");
            assert!(!want.contains("file an issue"));
        }
    }

    #[test]
    fn a_tool_that_cannot_be_run_directly_says_so() {
        let helm = ToolId::Helm.spec();
        let c = Check {
            tool: Some(ToolId::Helm),
            ..check(
                CheckId::Tool,
                CheckStatus::Warn,
                "`helm` on PATH",
                Some("C:\\bin\\helm.cmd cannot be run directly"),
                Some(CheckFix::InstallTool { tool: ToolId::Helm }),
            )
        };
        assert_eq!(
            hint(&c).unwrap(),
            format!(
                "not runnable — needed for {}.\n{}",
                helm.purpose, helm.install
            )
        );
    }

    #[test]
    fn cluster_hints_name_the_remedy() {
        let fail = |id, fix| check(id, CheckStatus::Fail, "t", None, Some(fix));
        let warn = |id, fix| check(id, CheckStatus::Warn, "t", None, Some(fix));
        assert_eq!(
            hint(&fail(
                CheckId::KubeconfigCached,
                CheckFix::FetchKubeconfig { target: "p".into() }
            ))
            .unwrap(),
            "run `apprafter kubeconfig --target p` to fetch it from the node and cache it encrypted"
        );
        assert_eq!(
            hint(&warn(
                CheckId::KubeconfigCached,
                CheckFix::FetchKubeconfig { target: "p".into() }
            ))
            .unwrap(),
            "run `apprafter kubeconfig --refresh --target p` to replace the unencrypted copy \
             with an encrypted one"
        );
        let age = hint(&fail(
            CheckId::KubeApiReachable,
            CheckFix::AgeKeyMissing {
                path: "/k/age.key".into(),
            },
        ))
        .unwrap();
        assert!(
            age.contains("/k/age.key") && age.contains("APPRAFTER_AGE_KEY"),
            "{age}"
        );
        assert!(
            !age.contains("--refresh"),
            "refresh cannot recover a lost key (Risks)"
        );
        let unreachable = hint(&fail(
            CheckId::KubeApiReachable,
            CheckFix::ClusterUnreachable {
                reason: KubeErrorKind::Unreachable,
            },
        ))
        .unwrap();
        assert_eq!(unreachable, KubectlFailure::Unreachable.hint().unwrap());
        let forbidden = hint(&fail(
            CheckId::KubeApiReachable,
            CheckFix::ClusterUnreachable {
                reason: KubeErrorKind::Forbidden,
            },
        ))
        .unwrap();
        assert!(
            forbidden.contains("--refresh --target <name>"),
            "{forbidden}"
        );
        let skipped = check(
            CheckId::KubeApiReachable,
            CheckStatus::Skipped,
            "t",
            Some("`kubectl` not found"),
            Some(CheckFix::InstallTool {
                tool: ToolId::Kubectl,
            }),
        );
        assert!(hint(&skipped).unwrap().contains("Checking environment"));
        assert!(hint(&fail(
            CheckId::NodeSshReachable,
            CheckFix::NodeUnreachable {
                address: "1.2.3.4".into()
            }
        ))
        .unwrap()
        .contains("1.2.3.4"));
        assert!(
            hint(&fail(CheckId::Dns, CheckFix::Dns { host: "h".into() }))
                .unwrap()
                .contains("`h`")
        );
    }
}
