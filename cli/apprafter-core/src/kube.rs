// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The cluster API (D.3 overview §3.9): one minimal trait, `Kube`, whose one implementation,
//! [`KubectlKube`], runs `kubectl` (decision 2); D.5 adds methods.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use cli_core::diagnose::classify_kubectl;
use serde::Serialize;

use crate::cancel::CancellationToken;
use crate::context::Context;
use crate::error::{CoreError, CoreResult};
use crate::process::run_bounded;
use crate::runtime::MaterialisedKubeconfig;
use crate::tools::ToolId;

/// How long past the request timeout kubectl may take to start, fail and exit.
const KUBECTL_GRACE: Duration = Duration::from_secs(2);
/// kubectl can print a whole HTML page through a misbehaving proxy; a detail is one short line.
const DETAIL_MAX_CHARS: usize = 300;

/// Why a cluster API request failed, as the CLI classifies `kubectl`'s errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum KubeErrorKind {
    Unreachable,
    Forbidden,
    KindNotServed,
    ObjectNotFound,
    Other,
}

/// One to one; a timeout is mapped to `Unreachable` by [`KubectlKube`].
impl From<cli_core::diagnose::KubectlFailure> for KubeErrorKind {
    fn from(f: cli_core::diagnose::KubectlFailure) -> Self {
        use cli_core::diagnose::KubectlFailure as F;
        match f {
            F::Unreachable => Self::Unreachable,
            F::Forbidden => Self::Forbidden,
            F::KindNotServed => Self::KindNotServed,
            F::ObjectNotFound => Self::ObjectNotFound,
            F::Other => Self::Other,
        }
    }
}

impl KubeErrorKind {
    /// The snake_case name the desktop's error fields carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::Forbidden => "forbidden",
            Self::KindNotServed => "kind_not_served",
            Self::ObjectNotFound => "object_not_found",
            Self::Other => "other",
        }
    }
}

/// The apiserver's version, and how long asking took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct KubeVersion {
    pub git_version: String,
    pub elapsed_ms: u64,
}

/// The cluster API, as the core uses it.
pub trait Kube: Send + Sync {
    /// `GET /version` within the context's request timeout. D.5 adds methods.
    fn server_version(&self, cancel: &CancellationToken) -> CoreResult<KubeVersion>;
}

/// [`Kube`] over `kubectl` (spec §3.1: the one implementation). Every call names the
/// materialised kubeconfig with `--kubeconfig` and `KUBECONFIG` — never the ambient one — and
/// runs with the resolver's `PATH`, bounded by the context's request timeout plus a grace.
/// The caller keeps the [`MaterialisedKubeconfig`] alive while it uses this value.
pub struct KubectlKube {
    ctx: Context,
    kubeconfig: PathBuf,
}

impl KubectlKube {
    /// Refuses (tool_not_found / tool_unsupported) when `kubectl` does not resolve.
    pub fn new(ctx: &Context, kubeconfig: &MaterialisedKubeconfig) -> CoreResult<Self> {
        ctx.tools().resolve(ToolId::Kubectl)?;
        Ok(Self {
            ctx: ctx.clone(),
            kubeconfig: kubeconfig.path().to_path_buf(),
        })
    }

    fn version_args(&self) -> Vec<OsString> {
        let secs = self.ctx.request_timeout().as_secs().max(1);
        vec![
            OsString::from("--kubeconfig"),
            self.kubeconfig.clone().into_os_string(),
            OsString::from(format!("--request-timeout={secs}s")),
            OsString::from("get"),
            OsString::from("--raw"),
            OsString::from("/version"),
        ]
    }
}

impl Kube for KubectlKube {
    fn server_version(&self, cancel: &CancellationToken) -> CoreResult<KubeVersion> {
        cancel.check()?;
        let mut cmd = self.ctx.tools().command(ToolId::Kubectl)?;
        cmd.args(self.version_args())
            .env("KUBECONFIG", &self.kubeconfig)
            .stdin(Stdio::null());
        let deadline = self.ctx.request_timeout() + KUBECTL_GRACE;
        let started = Instant::now();
        let out = match run_bounded(cmd, deadline, cancel) {
            Ok(out) => out,
            // Spawn ENOENT is tool_not_found for every tool (spec §3.1): ask the resolver again
            // so the error is the typed one it gives.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(self
                    .ctx
                    .tools()
                    .resolve(ToolId::Kubectl)
                    .err()
                    .unwrap_or_else(|| io_error(e)));
            }
            Err(e) => return Err(io_error(e)),
        };
        // `run_bounded` returns a cancelled child as killed, not as an error: this is the check
        // that turns it into `Cancelled`.
        cancel.check()?;
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if out.timed_out {
            // spec §3.1: a timeout is `unreachable`.
            return Err(CoreError::Kube {
                kind: KubeErrorKind::Unreachable,
                detail: format!("no answer within {} s", deadline.as_secs()),
            });
        }
        if out.status.is_some_and(|s| s.success()) {
            let stdout = String::from_utf8_lossy(&out.stdout);
            return git_version(&stdout)
                .map(|git_version| KubeVersion {
                    git_version,
                    elapsed_ms,
                })
                .ok_or_else(|| CoreError::Kube {
                    kind: KubeErrorKind::Other,
                    detail: format!(
                        "unexpected answer to /version: {}",
                        first_line(&stdout).unwrap_or_default()
                    ),
                });
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail =
            first_line(&stderr).unwrap_or_else(|| match out.status.and_then(|s| s.code()) {
                Some(code) => format!("kubectl exited {code} without a message"),
                None => "kubectl was stopped by a signal".to_string(),
            });
        // D.3a's `From<KubectlFailure> for KubeErrorKind`; a timeout never reaches here (above).
        Err(CoreError::Kube {
            kind: classify_kubectl(&stderr).into(),
            detail,
        })
    }
}

fn git_version(stdout: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    v.get("gitVersion")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The first non-empty line, trimmed and cut to [`DETAIL_MAX_CHARS`] characters.
fn first_line(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(DETAIL_MAX_CHARS).collect())
}

fn io_error(e: io::Error) -> CoreError {
    CoreError::from(cli_core::CliError::Io(e))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::context::PathSource;
    use crate::runtime::materialise_kubeconfig;
    use crate::target_ref::TargetRef;
    use cli_core::target::{Target, TargetConfig, TargetCredentials};
    use cli_state::{HetznerCloudState, State, StatePaths};

    struct Setup {
        _dir: tempfile::TempDir,
        bin: PathBuf,
        ctx: Context,
        target: TargetRef,
    }

    /// Target `prod` caching a plaintext kubeconfig (no key needed), and a context whose only
    /// tool directory is `bin`.
    fn setup(timeout: Duration) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let ctx = Context::for_desktop(dir.path().join("config"), "http://127.0.0.1:1")
            .with_tool_search_path(bin.clone().into_os_string(), PathSource::Explicit)
            .with_request_timeout(timeout);
        let t = Target {
            name: "prod".into(),
            config: TargetConfig {
                provider: "hetzner-cloud".into(),
                ..Default::default()
            },
            credentials: TargetCredentials::default(),
        };
        cli_core::save_target(&ctx.store(), &t).unwrap();
        let hetzner: HetznerCloudState = serde_json::from_value(serde_json::json!({
            "server_id": 7, "server_name": "n", "kubeconfig_yaml": "apiVersion: v1\nkind: Config\n"
        }))
        .unwrap();
        State {
            hetzner_cloud: Some(hetzner),
            ..Default::default()
        }
        .save(&StatePaths::for_active_target(&ctx.store(), "prod"))
        .unwrap();
        let target = TargetRef::named(&ctx, "prod").unwrap();
        Setup {
            _dir: dir,
            bin,
            ctx,
            target,
        }
    }

    /// A `/bin/sh` kubectl in `dir`. The `__probe` prologue and retry loop drain the ETXTBSY
    /// window (as `platform-cli`'s `backup.rs` `stub_kubectl` does): a sibling test thread that
    /// forks while the file is open for writing makes `execve` refuse until that child execs.
    /// Only shell builtins: the child's PATH is `dir` alone (GOTCHA-104).
    #[cfg(unix)]
    pub(crate) fn fake_kubectl(dir: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("kubectl");
        std::fs::write(
            &path,
            format!("#!/bin/sh\ncase \"$1\" in __probe) exit 0;; esac\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            match std::process::Command::new(&path).arg("__probe").status() {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => break,
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn server_version_runs_kubectl_with_the_materialised_kubeconfig_only() {
        let s = setup(Duration::from_secs(1));
        fake_kubectl(
            &s.bin,
            concat!(
                "for a in \"$@\"; do printf '%s\\n' \"$a\"; done > \"${0%/*}/argv\"\n",
                "printf '%s' \"$KUBECONFIG\" > \"${0%/*}/kubeconfig-env\"\n",
                "printf '%s' \"$PATH\" > \"${0%/*}/path-env\"\n",
                "printf '%s' '{\"major\":\"1\",\"minor\":\"31\",\"gitVersion\":\"v1.31.0+k3s1\"}'",
            ),
        );
        let file = materialise_kubeconfig(&s.ctx, &s.target).unwrap().unwrap();
        let v = KubectlKube::new(&s.ctx, &file)
            .unwrap()
            .server_version(&CancellationToken::new())
            .unwrap();
        assert_eq!(v.git_version, "v1.31.0+k3s1");
        let argv = std::fs::read_to_string(s.bin.join("argv")).unwrap();
        let path = file.path().display().to_string();
        assert_eq!(
            argv.lines().collect::<Vec<_>>(),
            [
                "--kubeconfig",
                path.as_str(),
                "--request-timeout=1s",
                "get",
                "--raw",
                "/version"
            ]
        );
        // Never the ambient KUBECONFIG: the child sees exactly the materialised file.
        assert_eq!(
            std::fs::read_to_string(s.bin.join("kubeconfig-env")).unwrap(),
            path
        );
        assert_eq!(
            std::fs::read_to_string(s.bin.join("path-env")).unwrap(),
            s.bin.display().to_string()
        );
    }

    #[cfg(unix)]
    #[test]
    fn stderr_is_classified_into_a_kind() {
        for (stderr, want) in [
            (
                "Unable to connect to the server: dial tcp 127.0.0.1:6443: connect: connection refused",
                KubeErrorKind::Unreachable,
            ),
            (
                "error: You must be logged in to the server (Unauthorized)",
                KubeErrorKind::Forbidden,
            ),
            (
                "Error from server (NotFound): the server could not find the requested resource",
                KubeErrorKind::KindNotServed,
            ),
            ("something nobody has seen before", KubeErrorKind::Other),
        ] {
            let s = setup(Duration::from_secs(1));
            fake_kubectl(&s.bin, &format!("echo '{stderr}' >&2\nexit 1"));
            let file = materialise_kubeconfig(&s.ctx, &s.target)
                .unwrap()
                .unwrap();
            match KubectlKube::new(&s.ctx, &file)
                .unwrap()
                .server_version(&CancellationToken::new())
            {
                Err(CoreError::Kube { kind, detail }) => {
                    assert_eq!(kind, want, "{stderr}");
                    assert_eq!(detail, stderr);
                }
                other => panic!("{stderr}: expected Kube, got {other:?}"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_kubectl_is_killed_at_the_deadline_and_reads_unreachable() {
        let s = setup(Duration::from_secs(1));
        // `sleep` is no shell builtin and the child's PATH is `bin` alone: a busy loop hangs.
        fake_kubectl(&s.bin, "while :; do :; done");
        let file = materialise_kubeconfig(&s.ctx, &s.target).unwrap().unwrap();
        let started = std::time::Instant::now();
        match KubectlKube::new(&s.ctx, &file)
            .unwrap()
            .server_version(&CancellationToken::new())
        {
            Err(CoreError::Kube {
                kind: KubeErrorKind::Unreachable,
                detail,
            }) => assert_eq!(detail, "no answer within 3 s"),
            other => panic!("expected an unreachable timeout, got {other:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "the bound is timeout + 2 s grace"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_cancelled_probe_returns_cancelled_promptly() {
        let s = setup(Duration::from_secs(5));
        fake_kubectl(&s.bin, "while :; do :; done");
        let file = materialise_kubeconfig(&s.ctx, &s.target).unwrap().unwrap();
        let cancel = CancellationToken::new();
        let trip = cancel.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            trip.cancel();
        });
        let started = std::time::Instant::now();
        let result = KubectlKube::new(&s.ctx, &file)
            .unwrap()
            .server_version(&cancel);
        t.join().unwrap();
        assert!(matches!(result, Err(CoreError::Cancelled)), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "cancel must not wait for the 7 s bound"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_answer_that_is_not_a_version_is_other() {
        let s = setup(Duration::from_secs(1));
        fake_kubectl(&s.bin, "echo hello");
        let file = materialise_kubeconfig(&s.ctx, &s.target).unwrap().unwrap();
        match KubectlKube::new(&s.ctx, &file)
            .unwrap()
            .server_version(&CancellationToken::new())
        {
            Err(CoreError::Kube {
                kind: KubeErrorKind::Other,
                detail,
            }) => {
                assert_eq!(detail, "unexpected answer to /version: hello")
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn new_refuses_without_kubectl() {
        let s = setup(Duration::from_secs(1));
        // The premise, and the one read of `bin` on Windows, where the fake-kubectl tests are
        // compiled out.
        assert_eq!(
            std::fs::read_dir(&s.bin).unwrap().count(),
            0,
            "`bin` is empty"
        );
        let file = materialise_kubeconfig(&s.ctx, &s.target).unwrap().unwrap();
        match KubectlKube::new(&s.ctx, &file) {
            Err(CoreError::Cli(cli_core::CliError::ExternalToolNotFound { tool, .. })) => {
                assert_eq!(tool, "kubectl")
            }
            Err(other) => panic!("expected tool_not_found, got {other:?}"),
            Ok(_) => panic!("expected tool_not_found, got a KubectlKube"),
        }
    }

    #[test]
    fn kube_error_kinds_mirror_the_kubectl_classification() {
        use cli_core::diagnose::KubectlFailure as F;
        assert_eq!(
            [
                F::Unreachable,
                F::Forbidden,
                F::KindNotServed,
                F::ObjectNotFound,
                F::Other
            ]
            .map(KubeErrorKind::from)
            .map(KubeErrorKind::as_str),
            [
                "unreachable",
                "forbidden",
                "kind_not_served",
                "object_not_found",
                "other"
            ]
        );
    }
}
