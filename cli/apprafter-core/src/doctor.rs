// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Doctor (D.3 overview §3.9): the report a run produces, grouped, with a typed fix per row,
//! and [`run`], the Read that produces it.

use cli_core::target::{load_target, validate_hetzner_token_format, Target, TargetStorePaths};
use cli_core::CliError;
use serde::Serialize;

use crate::cancel::CancellationToken;
use crate::context::{Context, SecretString};
use crate::error::{CoreError, CoreResult};
use crate::kube::KubeErrorKind;
use crate::net;
use crate::provider::{self, SUPPORTED_PROVIDERS};
use crate::report::{Event, Reporter};
use crate::target_ref::TargetRef;
use crate::tools::{self, ToolId, ToolProblem, ToolStatus};

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

const TITLE_CONFIG: &str = "Config file readable";
const TITLE_CREDENTIALS: &str = "Credentials file present";
const TITLE_TOKEN_VERIFIED: &str = "Token verified against provider API";
const TITLE_SSH_KEY: &str = "SSH key readable";
/// The DNS row names the provider API's HTTPS port, as it always has.
const DNS_PORT: u16 = 443;
/// The host the DNS row resolves when the API base names none (the CLI's `DEFAULT_API_HOST`).
const DEFAULT_API_HOST: &str = "api.hetzner.cloud";

/// What `run` found for the target it was asked about.
enum Lookup {
    NoCliDefault,
    Found(TargetRef),
    Missing {
        name: String,
        available: Vec<String>,
    },
}

/// Doctor (spec §3.1: a Read with a reporter). Every problem is a row, never an error; `Err`
/// is only cancellation or a store it cannot read at all (`config.yaml`, the target list).
/// Lockless: the files it reads are replaced atomically (overview R3). The token row verifies
/// the STORED token (R4); a check that did not run is `Skipped` (R8).
pub fn run(
    ctx: &Context,
    args: DoctorArgs,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<DoctorReport> {
    cancel.check()?;
    let name = match args.target {
        DoctorTarget::Named(name) => Some(name),
        // R1: no `config.yaml` is no CLI default.
        DoctorTarget::CliDefault => cli_core::resolve_active_target_name(&ctx.store(), None)?,
    };
    let lookup = match &name {
        None => Lookup::NoCliDefault,
        Some(n) => match TargetRef::named(ctx, n) {
            Ok(target) => Lookup::Found(target),
            Err(CoreError::TargetNotFound { name, available }) => {
                Lookup::Missing { name, available }
            }
            Err(e) => return Err(e),
        },
    };
    let total = 2;
    let mut groups = Vec::with_capacity(3);
    stage(reporter, 1, total, "Target");
    groups.push(CheckGroup {
        id: GroupId::Target,
        checks: target_group(ctx, &lookup, cancel)?,
    });
    cancel.check()?;
    stage(reporter, total, total, "This computer");
    groups.push(CheckGroup {
        id: GroupId::ThisComputer,
        checks: this_computer_group(ctx, cancel)?,
    });
    Ok(DoctorReport {
        target: name,
        groups,
    })
}

fn stage(reporter: &dyn Reporter, index: u32, total: u32, title: &str) {
    reporter.report(Event::Stage {
        index,
        total,
        title: title.to_string(),
    });
}

fn row(id: CheckId, status: CheckStatus, title: impl Into<String>) -> Check {
    Check {
        id,
        tool: None,
        status,
        title: title.into(),
        detail: None,
        fix: None,
    }
}

fn missing_target(name: &str, available: Vec<String>) -> Check {
    Check {
        fix: Some(CheckFix::AddTarget {
            name: Some(name.to_string()),
            available,
        }),
        ..row(
            CheckId::TargetExists,
            CheckStatus::Fail,
            format!("Target `{name}` exists"),
        )
    }
}

fn target_group(
    ctx: &Context,
    lookup: &Lookup,
    cancel: &CancellationToken,
) -> CoreResult<Vec<Check>> {
    let target = match lookup {
        Lookup::NoCliDefault => {
            return Ok(vec![Check {
                fix: Some(CheckFix::AddTarget {
                    name: None,
                    available: cli_core::list_target_names(&ctx.store())?,
                }),
                ..row(CheckId::ActiveTarget, CheckStatus::Warn, "active target")
            }]);
        }
        Lookup::Missing { name, available } => {
            return Ok(vec![missing_target(name, available.clone())])
        }
        Lookup::Found(target) => target,
    };
    let store = ctx.store();
    let name = target.name();
    let config_file = store.target_config_file(name).display().to_string();
    let loaded = match load_target(&store, name) {
        Ok(t) => t,
        // Removed between the lookup and this read.
        Err(CliError::TargetNotFound { .. }) => {
            return Ok(vec![missing_target(
                name,
                cli_core::list_target_names(&store)?,
            )]);
        }
        Err(e) => {
            return Ok(vec![Check {
                detail: Some(config_file),
                fix: Some(CheckFix::Explain {
                    text: e.to_string(),
                }),
                ..row(CheckId::ConfigReadable, CheckStatus::Fail, TITLE_CONFIG)
            }]);
        }
    };
    Ok(vec![
        Check {
            detail: Some(config_file),
            ..row(CheckId::ConfigReadable, CheckStatus::Pass, TITLE_CONFIG)
        },
        credentials_file(&store, name),
        provider_supported(&loaded),
        token_format(&loaded),
        token_verified(ctx, &loaded, cancel)?,
        ssh_key(&loaded),
    ])
}

fn credentials_file(store: &TargetStorePaths, name: &str) -> Check {
    let path = store.target_credentials_file(name);
    let shown = path.display().to_string();
    if !path.exists() {
        return Check {
            detail: Some(shown),
            fix: Some(CheckFix::RenewToken {
                target: name.to_string(),
                why: RenewWhy::CredentialsFileMissing,
            }),
            ..row(
                CheckId::CredentialsFile,
                CheckStatus::Fail,
                TITLE_CREDENTIALS,
            )
        };
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = match std::fs::metadata(&path) {
            Ok(meta) => meta.permissions().mode() & 0o777,
            Err(e) => {
                return Check {
                    detail: Some(shown),
                    fix: Some(CheckFix::Explain {
                        text: format!("cannot stat file: {e}"),
                    }),
                    ..row(
                        CheckId::CredentialsFile,
                        CheckStatus::Fail,
                        TITLE_CREDENTIALS,
                    )
                };
            }
        };
        if mode != 0o600 {
            return Check {
                detail: Some(shown.clone()),
                fix: Some(CheckFix::Chmod {
                    path: shown,
                    mode: 0o600,
                }),
                ..row(
                    CheckId::CredentialsFile,
                    CheckStatus::Warn,
                    format!("Credentials file mode 0600 (got {mode:o})"),
                )
            };
        }
        Check {
            detail: Some(shown),
            ..row(
                CheckId::CredentialsFile,
                CheckStatus::Pass,
                "Credentials file present (mode 0600)",
            )
        }
    }
    #[cfg(not(unix))]
    {
        Check {
            detail: Some(shown),
            ..row(
                CheckId::CredentialsFile,
                CheckStatus::Pass,
                TITLE_CREDENTIALS,
            )
        }
    }
}

fn provider_supported(target: &Target) -> Check {
    let p = &target.config.provider;
    let title = format!("Provider `{p}` supported");
    if SUPPORTED_PROVIDERS.contains(&p.as_str()) {
        return row(CheckId::ProviderSupported, CheckStatus::Pass, title);
    }
    Check {
        fix: Some(CheckFix::UnsupportedProvider {
            provider: p.clone(),
            supported: SUPPORTED_PROVIDERS.iter().map(|s| s.to_string()).collect(),
        }),
        ..row(CheckId::ProviderSupported, CheckStatus::Fail, title)
    }
}

fn token_format(target: &Target) -> Check {
    let Some(token) = target.credentials.hetzner_token.as_deref() else {
        return Check {
            fix: Some(CheckFix::RenewToken {
                target: target.name.clone(),
                why: RenewWhy::TokenMissing,
            }),
            ..row(
                CheckId::TokenPresent,
                CheckStatus::Fail,
                "Hetzner token present",
            )
        };
    };
    match validate_hetzner_token_format(token) {
        Ok(()) => Check {
            detail: Some(format!("{} chars, alphanumeric", token.len())),
            ..row(
                CheckId::TokenFormat,
                CheckStatus::Pass,
                "Token format valid",
            )
        },
        // The reason names a length or a character class, never the token.
        Err(reason) => Check {
            detail: Some(reason),
            fix: Some(CheckFix::RenewToken {
                target: target.name.clone(),
                why: RenewWhy::TokenMalformed,
            }),
            ..row(
                CheckId::TokenFormat,
                CheckStatus::Fail,
                "Token format valid",
            )
        },
    }
}

fn token_verified(ctx: &Context, target: &Target, cancel: &CancellationToken) -> CoreResult<Check> {
    let skipped = |detail: String| Check {
        detail: Some(detail),
        ..row(
            CheckId::TokenVerified,
            CheckStatus::Skipped,
            TITLE_TOKEN_VERIFIED,
        )
    };
    if ctx.no_ping() {
        return Ok(skipped("not requested".into()));
    }
    let Some(token) = target.credentials.hetzner_token.as_deref() else {
        return Ok(skipped("no token stored".into()));
    };
    let provider = target.config.provider.as_str();
    if !SUPPORTED_PROVIDERS.contains(&provider) {
        return Ok(skipped(format!("no validator for provider `{provider}`")));
    }
    cancel.check()?;
    // `ping`, not `verification`: the failure row keeps the API's own message (deviation 1).
    match provider::ping(ctx, provider, &SecretString::new(token), cancel) {
        Ok(elapsed) => Ok(Check {
            detail: Some(format!(
                "Hetzner Cloud /v1/locations, {} ms",
                elapsed.as_millis()
            )),
            ..row(
                CheckId::TokenVerified,
                CheckStatus::Pass,
                TITLE_TOKEN_VERIFIED,
            )
        }),
        Err(CoreError::Cancelled) => Err(CoreError::Cancelled),
        Err(e) => Ok(ping_failure(&target.name, &e)),
    }
}

/// Today's three failure rows: 401 (renew), another HTTP status, and no answer.
fn ping_failure(name: &str, e: &CoreError) -> Check {
    let fail = row(
        CheckId::TokenVerified,
        CheckStatus::Fail,
        TITLE_TOKEN_VERIFIED,
    );
    let cause = match e {
        CoreError::Cli(CliError::ProviderTokenRejected { cause, .. })
        | CoreError::Cli(CliError::ProviderApiUnreachable { cause, .. }) => Some(cause.as_ref()),
        _ => None,
    };
    match cause.and_then(hetzner_status) {
        Some((401, message)) => Check {
            detail: Some(format!("HTTP 401: {message}")),
            fix: Some(CheckFix::RenewToken {
                target: name.to_string(),
                why: RenewWhy::TokenRejected,
            }),
            ..fail
        },
        Some((status, message)) => Check {
            detail: Some(format!("HTTP {status}: {message}")),
            fix: Some(CheckFix::ProviderError { status }),
            ..fail
        },
        None => Check {
            detail: Some(cause.map_or_else(|| e.to_string(), |c| c.to_string())),
            fix: Some(CheckFix::ProviderUnreachable),
            ..fail
        },
    }
}

/// The Hetzner status and message inside a ping error's cause, when it carries one.
fn hetzner_status(
    cause: &(dyn miette::Diagnostic + Send + Sync + 'static),
) -> Option<(u16, String)> {
    let err: &(dyn std::error::Error + 'static) = cause;
    match err.downcast_ref::<CliError>()? {
        CliError::Hetzner {
            status, message, ..
        } => Some((*status, message.clone())),
        _ => None,
    }
}

fn ssh_key(target: &Target) -> Check {
    let Some(path) = target.config.ssh_key_path.as_ref() else {
        return Check {
            fix: Some(CheckFix::ConfigureSshKey {
                target: target.name.clone(),
            }),
            ..row(
                CheckId::SshKey,
                CheckStatus::Warn,
                "SSH key path configured",
            )
        };
    };
    let shown = path.display().to_string();
    if !path.exists() {
        return Check {
            detail: Some(shown.clone()),
            fix: Some(CheckFix::SshKeyMissing { path: shown }),
            ..row(CheckId::SshKey, CheckStatus::Fail, TITLE_SSH_KEY)
        };
    }
    match std::fs::read_to_string(path) {
        // The first whitespace-delimited token, as today: presence, not validity (the
        // operator troubleshooting page documents exactly this).
        Ok(body) => {
            let algo = body.split_whitespace().next().unwrap_or("(unknown)");
            Check {
                detail: Some(format!("{shown} ({algo})")),
                ..row(CheckId::SshKey, CheckStatus::Pass, TITLE_SSH_KEY)
            }
        }
        Err(e) => Check {
            detail: Some(shown),
            fix: Some(CheckFix::Explain {
                text: format!("cannot read file: {e}"),
            }),
            ..row(CheckId::SshKey, CheckStatus::Fail, TITLE_SSH_KEY)
        },
    }
}

fn this_computer_group(ctx: &Context, cancel: &CancellationToken) -> CoreResult<Vec<Check>> {
    // Concurrent probes, each killed at TOOL_PROBE_TIMEOUT, in ToolId::ALL order.
    let toolchain = tools::toolchain(ctx, cancel)?;
    let mut out: Vec<Check> = toolchain.tools.iter().map(tool_check).collect();
    cancel.check()?;
    out.push(dns_check(ctx));
    Ok(out)
}

/// One tool row from the resolver's probe (D.3a semantics: a run that exited 0 has no
/// problem, its version the first line it printed; any other run is `NoVersionOutput`).
fn tool_check(s: &ToolStatus) -> Check {
    let name = s.tool.name();
    let base = Check {
        tool: Some(s.tool),
        ..row(
            CheckId::Tool,
            CheckStatus::Pass,
            format!("`{name}` on PATH"),
        )
    };
    let missing = if s.required {
        CheckStatus::Fail
    } else {
        CheckStatus::Warn
    };
    match &s.problem {
        None => Check {
            detail: Some(
                s.version
                    .clone()
                    .unwrap_or_else(|| "version unknown".into()),
            ),
            ..base
        },
        Some(ToolProblem::NotFound) => Check {
            status: missing,
            fix: Some(CheckFix::InstallTool { tool: s.tool }),
            ..base
        },
        Some(ToolProblem::Unsupported { path }) => Check {
            status: missing,
            detail: Some(format!("{path} cannot be run directly")),
            fix: Some(CheckFix::InstallTool { tool: s.tool }),
            ..base
        },
        // It is installed and runs: a warning, whatever `required` says. The detail is why,
        // in the tool's own words (decision 3).
        Some(ToolProblem::NoVersionOutput { exit, detail }) => Check {
            status: CheckStatus::Warn,
            detail: detail.clone(),
            fix: Some(CheckFix::Explain {
                text: match exit {
                    Some(code) => format!("`{name}` exited {code} without printing a version"),
                    None => format!("`{name}` was stopped by a signal before printing a version"),
                },
            }),
            ..base
        },
        Some(ToolProblem::TimedOut) => Check {
            status: CheckStatus::Warn,
            fix: Some(CheckFix::Explain {
                text: format!(
                    "`{name}` did not print its version within {} s",
                    tools::TOOL_PROBE_TIMEOUT.as_secs()
                ),
            }),
            ..base
        },
        Some(ToolProblem::SpawnFailed { error }) => Check {
            status: missing,
            fix: Some(CheckFix::Explain {
                text: format!("cannot start `{name}`: {error}"),
            }),
            ..base
        },
    }
}

fn dns_check(ctx: &Context) -> Check {
    let host = api_host(ctx.hcloud_base_url());
    let title = format!("DNS resolves `{host}`");
    let netloc = if host.contains(':') {
        format!("[{host}]:{DNS_PORT}")
    } else {
        format!("{host}:{DNS_PORT}")
    };
    match net::resolve_with_deadline(&netloc, ctx.request_timeout()) {
        Ok(addrs) if !addrs.is_empty() => Check {
            detail: Some(format!("{DNS_PORT}/tcp")),
            ..row(CheckId::Dns, CheckStatus::Pass, title)
        },
        Ok(_) => Check {
            fix: Some(CheckFix::Explain {
                text: "resolver returned an empty address list".into(),
            }),
            ..row(CheckId::Dns, CheckStatus::Fail, title)
        },
        Err(e) => Check {
            detail: Some(e.to_string()),
            fix: Some(CheckFix::Dns { host }),
            ..row(CheckId::Dns, CheckStatus::Fail, title)
        },
    }
}

/// The host of a base URL: scheme, userinfo, port, path, query and IPv6 brackets stripped;
/// [`DEFAULT_API_HOST`] when nothing is left. The CLI doctor's `api_host` (D.3a), ported rule
/// for rule, so the DNS row is the same even for `APPRAFTER_HCLOUD_BASE_URL=""`.
fn api_host(base_url: &str) -> String {
    let rest = base_url.split_once("://").map_or(base_url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host_port.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host_port.split(':').next().unwrap_or_default(),
    };
    let host = if host.is_empty() {
        DEFAULT_API_HOST
    } else {
        host
    };
    host.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::PathSource;
    use crate::report::{CollectReporter, NullReporter};
    use cli_core::target::{GlobalConfig, TargetConfig, TargetCredentials};
    use std::path::PathBuf;
    use std::time::Duration;

    const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OFFLINE: &str = "http://127.0.0.1:1";

    struct Fx {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    fn fx() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        for d in ["home", "bin"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        Fx { _dir: dir, root }
    }

    fn ctx(f: &Fx, base_url: &str) -> Context {
        Context::for_desktop(f.root.join("config"), base_url)
            .with_home_dir(Some(f.root.join("home")))
            .with_tool_search_path(f.root.join("bin").into_os_string(), PathSource::Explicit)
            .with_request_timeout(Duration::from_secs(2))
    }

    fn add(
        ctx: &Context,
        name: &str,
        provider: &str,
        token: Option<&str>,
        ssh_key: Option<PathBuf>,
    ) {
        let t = Target {
            name: name.into(),
            config: TargetConfig {
                provider: provider.into(),
                ssh_key_path: ssh_key,
                ..Default::default()
            },
            credentials: TargetCredentials {
                hetzner_token: token.map(str::to_string),
            },
        };
        cli_core::save_target(&ctx.store(), &t).unwrap();
    }

    fn set_default(ctx: &Context, name: &str) {
        cli_core::save_global_config(
            &ctx.store(),
            &GlobalConfig {
                active_target: name.into(),
                version: cli_core::TARGET_STORE_VERSION,
            },
        )
        .unwrap();
    }

    fn doctor(ctx: &Context, target: DoctorTarget) -> DoctorReport {
        run(
            ctx,
            DoctorArgs { target },
            &NullReporter,
            &CancellationToken::new(),
        )
        .unwrap()
    }

    fn group(r: &DoctorReport, id: GroupId) -> Vec<Check> {
        r.groups
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.checks.clone())
            .unwrap_or_default()
    }

    fn ids(checks: &[Check]) -> Vec<CheckId> {
        checks.iter().map(|c| c.id).collect()
    }

    fn key(f: &Fx) -> PathBuf {
        let p = f.root.join("home/id_ed25519.pub");
        std::fs::write(&p, "ssh-ed25519 AAAA test@host\n").unwrap();
        p
    }

    fn named(name: &str) -> DoctorTarget {
        DoctorTarget::Named(name.into())
    }

    #[test]
    fn no_cli_default_is_an_active_target_warning_and_the_environment_still_runs() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        let r = doctor(&ctx, DoctorTarget::CliDefault);
        assert_eq!(r.target, None);
        assert_eq!(
            r.groups.iter().map(|g| g.id).collect::<Vec<_>>(),
            [GroupId::Target, GroupId::ThisComputer]
        );
        let t = group(&r, GroupId::Target);
        assert_eq!(ids(&t), [CheckId::ActiveTarget]);
        assert_eq!(t[0].status, CheckStatus::Warn);
        assert_eq!(t[0].title, "active target");
        assert_eq!(
            t[0].fix,
            Some(CheckFix::AddTarget {
                name: None,
                available: vec![]
            })
        );
    }

    #[test]
    fn a_named_target_that_does_not_exist_is_a_fail_row_not_an_error() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        let r = doctor(&ctx, named("ghost"));
        assert_eq!(r.target.as_deref(), Some("ghost"));
        let t = group(&r, GroupId::Target);
        assert_eq!(ids(&t), [CheckId::TargetExists]);
        assert_eq!(t[0].status, CheckStatus::Fail);
        assert_eq!(t[0].title, "Target `ghost` exists");
        assert_eq!(
            t[0].fix,
            Some(CheckFix::AddTarget {
                name: Some("ghost".into()),
                available: vec!["prod".into()]
            })
        );
    }

    #[test]
    fn a_dangling_cli_default_is_the_same_fail_row() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        set_default(&ctx, "gone");
        let r = doctor(&ctx, DoctorTarget::CliDefault);
        assert_eq!(r.target.as_deref(), Some("gone"));
        assert_eq!(ids(&group(&r, GroupId::Target)), [CheckId::TargetExists]);
    }

    #[test]
    fn a_healthy_target_has_todays_rows_in_todays_order() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), Some(key(&f)));
        set_default(&ctx, "prod");
        let t = group(&doctor(&ctx, DoctorTarget::CliDefault), GroupId::Target);
        assert_eq!(
            ids(&t),
            [
                CheckId::ConfigReadable,
                CheckId::CredentialsFile,
                CheckId::ProviderSupported,
                CheckId::TokenFormat,
                CheckId::TokenVerified,
                CheckId::SshKey
            ]
        );
        assert_eq!(t[0].title, "Config file readable");
        assert_eq!(t[2].title, "Provider `hetzner-cloud` supported");
        assert_eq!(t[3].detail.as_deref(), Some("64 chars, alphanumeric"));
        assert_eq!(t[4].status, CheckStatus::Skipped);
        assert_eq!(t[4].detail.as_deref(), Some("not requested"));
        assert_eq!(t[5].status, CheckStatus::Pass);
        assert!(
            t[5].detail
                .as_deref()
                .unwrap()
                .ends_with("id_ed25519.pub (ssh-ed25519)"),
            "{:?}",
            t[5].detail
        );
    }

    #[cfg(unix)]
    #[test]
    fn credentials_mode_other_than_0600_warns_with_a_chmod_fix() {
        use std::os::unix::fs::PermissionsExt;
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        let creds = ctx.store().target_credentials_file("prod");
        std::fs::set_permissions(&creds, std::fs::Permissions::from_mode(0o644)).unwrap();
        let c = group(&doctor(&ctx, named("prod")), GroupId::Target)[1].clone();
        assert_eq!(c.status, CheckStatus::Warn);
        assert_eq!(c.title, "Credentials file mode 0600 (got 644)");
        assert_eq!(
            c.fix,
            Some(CheckFix::Chmod {
                path: creds.display().to_string(),
                mode: 0o600
            })
        );
    }

    #[test]
    fn a_missing_token_fails_present_and_skips_verification() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE); // pinging allowed: the skip comes from the missing token
        add(&ctx, "prod", "hetzner-cloud", None, None);
        let t = group(&doctor(&ctx, named("prod")), GroupId::Target);
        let present = t.iter().find(|c| c.id == CheckId::TokenPresent).unwrap();
        assert_eq!(present.status, CheckStatus::Fail);
        assert_eq!(
            present.fix,
            Some(CheckFix::RenewToken {
                target: "prod".into(),
                why: RenewWhy::TokenMissing
            })
        );
        let verified = t.iter().find(|c| c.id == CheckId::TokenVerified).unwrap();
        assert_eq!(verified.status, CheckStatus::Skipped);
        assert_eq!(verified.detail.as_deref(), Some("no token stored"));
    }

    #[test]
    fn a_malformed_token_fails_its_format() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some("short"), None);
        let t = group(&doctor(&ctx, named("prod")), GroupId::Target);
        let format = t.iter().find(|c| c.id == CheckId::TokenFormat).unwrap();
        assert_eq!(format.status, CheckStatus::Fail);
        assert_eq!(
            format.fix,
            Some(CheckFix::RenewToken {
                target: "prod".into(),
                why: RenewWhy::TokenMalformed
            })
        );
        assert!(
            !format
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("short"),
            "a token never reaches a detail"
        );
    }

    #[test]
    fn verification_passes_against_the_api_and_reports_the_time() {
        let mut server = mockito::Server::new();
        let m = server
            .mock("GET", "/v1/locations")
            .match_header("authorization", format!("Bearer {TOKEN}").as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"locations":[]}"#)
            .create();
        let f = fx();
        let ctx = ctx(&f, &server.url());
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        let t = group(&doctor(&ctx, named("prod")), GroupId::Target);
        let v = t.iter().find(|c| c.id == CheckId::TokenVerified).unwrap();
        assert_eq!(v.status, CheckStatus::Pass);
        let d = v.detail.as_deref().unwrap();
        assert!(
            d.starts_with("Hetzner Cloud /v1/locations, ") && d.ends_with(" ms"),
            "{d}"
        );
        m.assert();
    }

    #[test]
    fn a_rejected_token_fails_with_the_api_message_and_a_renew_fix() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/v1/locations")
            .with_status(401)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":{"code":"unauthorized","message":"unable to authenticate"}}"#)
            .create();
        let f = fx();
        let ctx = ctx(&f, &server.url());
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        let t = group(&doctor(&ctx, named("prod")), GroupId::Target);
        let v = t.iter().find(|c| c.id == CheckId::TokenVerified).unwrap();
        assert_eq!(v.status, CheckStatus::Fail);
        assert_eq!(
            v.detail.as_deref(),
            Some("HTTP 401: unable to authenticate")
        );
        assert_eq!(
            v.fix,
            Some(CheckFix::RenewToken {
                target: "prod".into(),
                why: RenewWhy::TokenRejected
            })
        );
    }

    #[test]
    fn another_api_status_fails_with_a_provider_error_fix() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/v1/locations")
            .with_status(503)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":{"code":"unavailable","message":"maintenance"}}"#)
            .create();
        let f = fx();
        let ctx = ctx(&f, &server.url());
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        let t = group(&doctor(&ctx, named("prod")), GroupId::Target);
        let v = t.iter().find(|c| c.id == CheckId::TokenVerified).unwrap();
        assert_eq!(v.status, CheckStatus::Fail);
        assert_eq!(v.detail.as_deref(), Some("HTTP 503: maintenance"));
        assert_eq!(v.fix, Some(CheckFix::ProviderError { status: 503 }));
    }

    #[test]
    fn an_unreachable_api_fails_with_provider_unreachable() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        let t = group(&doctor(&ctx, named("prod")), GroupId::Target);
        let v = t.iter().find(|c| c.id == CheckId::TokenVerified).unwrap();
        assert_eq!(v.status, CheckStatus::Fail);
        assert_eq!(v.fix, Some(CheckFix::ProviderUnreachable));
        assert!(
            !v.detail.as_deref().unwrap_or_default().contains(TOKEN),
            "{:?}",
            v.detail
        );
    }

    #[test]
    fn an_unsupported_provider_fails_its_row_and_skips_verification() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE);
        add(&ctx, "prod", "aws-bedrock", Some(TOKEN), None);
        let t = group(&doctor(&ctx, named("prod")), GroupId::Target);
        let p = t
            .iter()
            .find(|c| c.id == CheckId::ProviderSupported)
            .unwrap();
        assert_eq!(p.status, CheckStatus::Fail);
        assert_eq!(
            p.fix,
            Some(CheckFix::UnsupportedProvider {
                provider: "aws-bedrock".into(),
                supported: vec!["hetzner-cloud".into()]
            })
        );
        let v = t.iter().find(|c| c.id == CheckId::TokenVerified).unwrap();
        assert_eq!(v.status, CheckStatus::Skipped);
        assert_eq!(
            v.detail.as_deref(),
            Some("no validator for provider `aws-bedrock`")
        );
    }

    #[test]
    fn ssh_key_rows_warn_when_unset_and_fail_when_missing() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "unset", "hetzner-cloud", Some(TOKEN), None);
        add(
            &ctx,
            "gone",
            "hetzner-cloud",
            Some(TOKEN),
            Some(f.root.join("home/nope.pub")),
        );
        let unset = group(&doctor(&ctx, named("unset")), GroupId::Target);
        assert_eq!(
            unset[5].fix,
            Some(CheckFix::ConfigureSshKey {
                target: "unset".into()
            })
        );
        assert_eq!(unset[5].status, CheckStatus::Warn);
        let gone = group(&doctor(&ctx, named("gone")), GroupId::Target);
        assert_eq!(gone[5].status, CheckStatus::Fail);
        assert!(matches!(gone[5].fix, Some(CheckFix::SshKeyMissing { .. })));
    }

    #[test]
    fn this_computer_has_one_row_per_tool_in_order_then_dns() {
        let f = fx();
        let r = doctor(
            &ctx(&f, OFFLINE).with_no_ping(true),
            DoctorTarget::CliDefault,
        );
        let env = group(&r, GroupId::ThisComputer);
        let tools: Vec<&str> = env
            .iter()
            .filter(|c| c.id == CheckId::Tool)
            .map(|c| c.tool.unwrap().name())
            .collect();
        let cli: Vec<&str> = cli_core::tools::ALL.iter().map(|t| t.name).collect();
        assert_eq!(tools, cli, "every binary the CLI spawns is checked (D11)");
        assert_eq!(tools, ["restic", "kubectl", "helm", "git", "ssh", "cue"]);
        assert_eq!(env.last().unwrap().id, CheckId::Dns);
        // Empty search path: the required tool fails, the rest warn.
        let kubectl = env
            .iter()
            .find(|c| c.tool == Some(ToolId::Kubectl))
            .unwrap();
        assert_eq!(kubectl.status, CheckStatus::Fail);
        assert_eq!(
            kubectl.fix,
            Some(CheckFix::InstallTool {
                tool: ToolId::Kubectl
            })
        );
        assert!(env
            .iter()
            .filter(|c| c.id == CheckId::Tool && c.tool != Some(ToolId::Kubectl))
            .all(|c| c.status == CheckStatus::Warn));
    }

    #[test]
    fn tool_rows_map_every_probe_outcome() {
        let status =
            |problem: Option<ToolProblem>, version: Option<&str>, required: bool| ToolStatus {
                tool: ToolId::Helm,
                required,
                purpose: "installing platform charts".into(),
                path: None,
                version: version.map(str::to_string),
                problem,
                install: vec![],
            };
        let pass = tool_check(&status(None, Some("v3.16.0"), false));
        assert_eq!(
            (pass.status, pass.detail.as_deref(), pass.title.as_str()),
            (CheckStatus::Pass, Some("v3.16.0"), "`helm` on PATH")
        );
        assert_eq!(
            tool_check(&status(None, None, false)).detail.as_deref(),
            Some("version unknown")
        );
        assert_eq!(
            tool_check(&status(Some(ToolProblem::NotFound), None, true)).status,
            CheckStatus::Fail
        );
        assert_eq!(
            tool_check(&status(Some(ToolProblem::NotFound), None, false)).status,
            CheckStatus::Warn
        );
        let shim = tool_check(&status(
            Some(ToolProblem::Unsupported {
                path: "C:\\bin\\helm.cmd".into(),
            }),
            None,
            false,
        ));
        assert_eq!(
            shim.detail.as_deref(),
            Some("C:\\bin\\helm.cmd cannot be run directly")
        );
        assert_eq!(shim.fix, Some(CheckFix::InstallTool { tool: ToolId::Helm }));
        // Decision 3: a tool with no version says why, as the row's detail; the fix names the
        // exit. A required tool that runs but answers no version is a warning, not missing.
        let quiet = tool_check(&status(
            Some(ToolProblem::NoVersionOutput {
                exit: Some(3),
                detail: Some("mise ERROR No version is set for command helm".into()),
            }),
            None,
            true,
        ));
        assert_eq!(quiet.status, CheckStatus::Warn);
        assert_eq!(
            quiet.detail.as_deref(),
            Some("mise ERROR No version is set for command helm")
        );
        assert_eq!(
            quiet.fix,
            Some(CheckFix::Explain {
                text: "`helm` exited 3 without printing a version".into()
            })
        );
        let killed = tool_check(&status(
            Some(ToolProblem::NoVersionOutput {
                exit: None,
                detail: None,
            }),
            None,
            false,
        ));
        assert_eq!(
            (killed.status, killed.detail.as_deref()),
            (CheckStatus::Warn, None)
        );
        assert_eq!(
            killed.fix,
            Some(CheckFix::Explain {
                text: "`helm` was stopped by a signal before printing a version".into()
            })
        );
        assert_eq!(
            tool_check(&status(Some(ToolProblem::TimedOut), None, true)).status,
            CheckStatus::Warn
        );
        let broken = tool_check(&status(
            Some(ToolProblem::SpawnFailed {
                error: "permission denied".into(),
            }),
            None,
            true,
        ));
        assert_eq!(broken.status, CheckStatus::Fail);
        assert_eq!(
            broken.fix,
            Some(CheckFix::Explain {
                text: "cannot start `helm`: permission denied".into()
            })
        );
    }

    #[test]
    fn the_dns_row_resolves_the_api_host_of_the_base_url() {
        assert_eq!(api_host("https://api.hetzner.cloud"), "api.hetzner.cloud");
        assert_eq!(api_host("http://127.0.0.1:1"), "127.0.0.1");
        assert_eq!(api_host("http://127.0.0.1:41234/v1"), "127.0.0.1");
        assert_eq!(api_host("http://[::1]:8080/v1"), "::1");
        assert_eq!(api_host("http://user:pw@127.0.0.1:9"), "127.0.0.1");
        assert_eq!(api_host("https://user@example.test/x?y"), "example.test");
        // D.3a's fallback, ported: nothing left is the production host.
        assert_eq!(api_host(""), "api.hetzner.cloud");
        assert_eq!(api_host("http://"), "api.hetzner.cloud");
        let f = fx();
        let r = doctor(
            &ctx(&f, OFFLINE).with_no_ping(true),
            DoctorTarget::CliDefault,
        );
        let dns = group(&r, GroupId::ThisComputer).last().unwrap().clone();
        assert_eq!(dns.title, "DNS resolves `127.0.0.1`");
        assert_eq!(dns.status, CheckStatus::Pass);
        assert_eq!(dns.detail.as_deref(), Some("443/tcp"));
    }

    #[test]
    fn a_name_that_cannot_resolve_fails_the_dns_row() {
        // RFC 6761: `.invalid` never resolves.
        let f = fx();
        let r = doctor(
            &ctx(&f, "https://doctor-probe-host.invalid").with_no_ping(true),
            DoctorTarget::CliDefault,
        );
        let dns = group(&r, GroupId::ThisComputer).last().unwrap().clone();
        assert_eq!(dns.status, CheckStatus::Fail);
        assert_eq!(
            dns.fix,
            Some(CheckFix::Dns {
                host: "doctor-probe-host.invalid".into()
            })
        );
    }

    #[test]
    fn one_stage_per_group() {
        let f = fx();
        let reporter = CollectReporter::new();
        run(
            &ctx(&f, OFFLINE).with_no_ping(true),
            DoctorArgs {
                target: DoctorTarget::CliDefault,
            },
            &reporter,
            &CancellationToken::new(),
        )
        .unwrap();
        let stages: Vec<(u32, u32, String)> = reporter
            .take()
            .into_iter()
            .filter_map(|e| match e {
                Event::Stage {
                    index,
                    total,
                    title,
                } => Some((index, total, title)),
                _ => None,
            })
            .collect();
        assert_eq!(
            stages,
            [
                (1, 2, "Target".to_string()),
                (2, 2, "This computer".to_string())
            ]
        );
    }

    #[test]
    fn a_tripped_token_cancels_the_run() {
        let f = fx();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let r = run(
            &ctx(&f, OFFLINE),
            DoctorArgs {
                target: DoctorTarget::CliDefault,
            },
            &NullReporter,
            &cancel,
        );
        assert!(matches!(r, Err(CoreError::Cancelled)), "{r:?}");
    }

    #[test]
    fn skipped_rows_count_in_no_total() {
        let row = |status| Check {
            id: CheckId::Tool,
            tool: None,
            status,
            title: "x".into(),
            detail: None,
            fix: None,
        };
        let r = DoctorReport {
            target: None,
            groups: vec![CheckGroup {
                id: GroupId::Target,
                checks: vec![
                    row(CheckStatus::Pass),
                    row(CheckStatus::Skipped),
                    row(CheckStatus::Warn),
                ],
            }],
        };
        assert_eq!(
            (r.passed(), r.warned(), r.failed(), r.skipped()),
            (1, 1, 0, 1)
        );
        assert!(!r.has_failures());
    }

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
