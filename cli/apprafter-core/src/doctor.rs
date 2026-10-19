// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Doctor (D.3 overview §3.9): the report a run produces, grouped, with a typed fix per row,
//! and [`run`], the Read that produces it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cli_core::target::{validate_hetzner_token_format, TargetConfig};
use cli_core::{CliError, SlotRefusal};
use cli_state::{HetznerCloudState, State, StatePaths};
use serde::Serialize;

use crate::cancel::CancellationToken;
use crate::context::{Context, SecretString};
use crate::error::{CoreError, CoreResult};
use crate::kube::{Kube, KubeErrorKind, KubectlKube};
use crate::lock::limits::{LimitClient, SshFacts};
use crate::lock::{self, text, LockReport, MachineLimit, SlotRefusalView, SlotView, UnlockWindow};
use crate::net;
use crate::provider::{self, SUPPORTED_PROVIDERS};
use crate::report::{Event, Reporter};
use crate::runtime;
use crate::secrets::{
    CacheKeyView, DiagnosticAccess, LockSlotKind, OpenToken, SecretStore, SecretsMode, StoredToken,
    VaultAccess,
};
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

/// What a doctor run checks; `no_ping` comes from the context. `window` and `client` are what
/// only the client knows (D.4a T73): its unlock window on the store, and the facts behind the
/// machine-limit rows (its terminal, its display, the SSH key and agent).
#[derive(Debug, Clone)]
pub struct DoctorArgs {
    pub target: DoctorTarget,
    pub window: UnlockWindow,
    pub client: LimitClient,
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

/// One row's verdict. `Skipped` is a check that did not run, `Info` a fact that is no verdict (D.4a
/// T71): neither counts in a total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skipped,
    Info,
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
    // D.4a §3.13: the target-store lock's rows (d4a-e, d4a-f).
    Secrets,
    SshKeyExposure,
    X11Keystrokes,
    PtraceScope,
    WindowsSameUser,
    MacosTaskPort,
    TerminalMultiplexer,
    UnlockedTerminal,
    WindowHardening,
    SecretsInUse,
    SleepUnreported,
    /// A program running as you that sets out to get past the lock (O1, d4a-f): Info,
    /// static, on a locked or locking store.
    SameUserPrograms,
    /// Hibernation writes an open window's key to disk (overview rule 1, d4a-f): Info,
    /// static, on a locked or locking store.
    Hibernation,
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
    /// A clear token in a locked store's credentials file: never used (D.4a T42).
    TokenOutsideVault,
    /// A marker naming another store's vault (D.4a T34).
    TokenInAnotherVault,
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
    /// Any HTTP error status but 401 (renew) and 429 (rate-limited).
    ProviderError {
        status: u16,
    },
    /// The provider answered 429: it is rate-limiting requests.
    ProviderRateLimited,
    /// No answer: the name did not resolve, the connection failed or dropped, a timeout.
    ProviderUnreachable,
    /// No status and not for want of an answer: an answer that does not parse, or a request the
    /// HTTP client would not send; the detail says which.
    ProviderRequestFailed,
    ConfigureSshKey {
        target: String,
    },
    /// The stored key file is gone: the target it is stored on, so the fix (a key-only
    /// renewal) can name it, and the path as stored.
    SshKeyMissing {
        target: String,
        path: String,
    },
    /// The stored key file is not an OpenSSH public key — a private key (`private_key`), or
    /// anything else — so `apply` refuses to send it to the provider (GOTCHA-149): the target
    /// it is stored on and the path as stored, for the key-only renewal that fixes it.
    SshKeyNotPublic {
        target: String,
        path: String,
        private_key: bool,
    },
    InstallTool {
        tool: ToolId,
    },
    FetchKubeconfig {
        target: String,
    },
    /// No age key at `path`: the target whose cache it is, for the refetch under a new key that
    /// the fix names (WI-457 review #9). `sealed_to_lost_vault` (F13): the state records that
    /// the cache was sealed to a target store vault's key, not to the key at `path` — a vault no
    /// longer here, since only a store in files looks for that key — so no key put at `path`
    /// opens it.
    AgeKeyMissing {
        target: String,
        path: String,
        sealed_to_lost_vault: bool,
    },
    /// The cached kubeconfig does not decrypt with the age key at `path`: it was cached under
    /// another key (a lost one that a new key replaced) or is damaged. The target whose it is,
    /// for the refetch under the key there is (WI-457 review #8). `key` says which key (D.4a
    /// §3.13).
    CacheUndecryptable {
        target: String,
        path: String,
        key: CacheKeyView,
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
    /// The store's secrets are in files: locking them protects them (D.4a T72).
    ProtectSecrets,
    /// An earlier `lock on` (`locking`) or `lock off` (`unlocking`) did not finish.
    FinishLock {
        phase: SecretsMode,
    },
    /// The store is locked and nothing here holds its key: unlock, then run doctor again
    /// (D.4a T66).
    UnlockFirst,
    /// The SSH key that reaches the nodes is outside the lock: no passphrase, or in an ssh-agent
    /// (D.4a O1).
    ProtectSshKey {
        path: String,
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

    /// Facts that are no verdict (D.4a T71); outside the pass / warn / fail totals.
    pub fn informed(&self) -> usize {
        self.count(CheckStatus::Info)
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
const TITLE_KUBECONFIG_CACHED: &str = "Kubeconfig cached";
const TITLE_KUBE_API: &str = "Kube API reachable";
const TITLE_NODE_SSH: &str = "Node reachable over SSH";
/// The node row's port: the SSH the CLI's node preparation uses.
const SSH_PORT: u16 = 22;
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
    /// The target list cannot be read: another account owns `targets/` (WI-481), or this
    /// account cannot read the store there (d4a-h N6). The refusal, naming the folder, is the
    /// target's row.
    Refused {
        name: String,
        refusal: CliError,
    },
    /// The store's own file that would name a target cannot be read (d4a-h N6): its
    /// `config.yaml`, which names the CLI default, or with no default the target list. The
    /// refusal is the Target group's one row, about `path`.
    Unreadable {
        path: PathBuf,
        refusal: CliError,
    },
}

/// Doctor (spec §3.1: a Read with a reporter). Every problem is a row, never an error, a store
/// it cannot read at all included (d4a-h N6: its `config.yaml` or its target list is the
/// `Config file readable` row, and the run goes on); `Err` is cancellation. Lockless: the files it reads are replaced atomically (overview R3). The token row verifies
/// the STORED token (R4); a check that did not run is `Skipped` (R8).
///
/// Groups: Target always; Cluster only for a target that exists; This computer always. A Read
/// that still writes in one place: the Cluster group decrypts the cached kubeconfig into the
/// private runtime dir for the length of one probe and removes it, and a run starts by sweeping
/// copies a crashed run left there (R9). It never creates an age key (decision 2), and it
/// changes no secret file (WI-473): the store's own reads on Windows make a file other accounts
/// can open owner-only, but not in a doctor run, which reports such a file instead
/// (`cli_core::owner_only::without_tightening`).
pub fn run(
    ctx: &Context,
    args: DoctorArgs,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<DoctorReport> {
    cli_core::owner_only::without_tightening(|| run_reading_only(ctx, args, reporter, cancel))
}

/// [`run`], whose reads tighten nothing.
fn run_reading_only(
    ctx: &Context,
    args: DoctorArgs,
    reporter: &dyn Reporter,
    cancel: &CancellationToken,
) -> CoreResult<DoctorReport> {
    cancel.check()?;
    // D.4a T66: whatever the client's access, this run reads a locked store only through a
    // session already open here; any other read is `store_locked`, reported as a skipped row.
    let ctx = &diagnostic(ctx);
    // R9: leftovers of a crashed run go first; failing to sweep never fails the run. The
    // folder, then the OS error once (d4a-h N6).
    if let Err(e) = runtime::sweep_stale(ctx) {
        reporter.report(Event::Warning {
            message: format!(
                "cannot remove old kubeconfig copies from {}: {e}",
                ctx.runtime_dir().display()
            ),
        });
    }
    let DoctorArgs {
        target,
        window,
        client,
    } = args;
    let store = ctx.store();
    let name = match target {
        DoctorTarget::Named(name) => Ok(Some(name)),
        // R1: no `config.yaml` is no CLI default.
        DoctorTarget::CliDefault => cli_core::resolve_active_target_name(&store, None),
    };
    let (name, lookup) = match name {
        Ok(None) => (None, Lookup::NoCliDefault),
        Ok(Some(n)) => {
            let lookup = match TargetRef::named(ctx, &n) {
                Ok(target) => Lookup::Found(target),
                Err(CoreError::TargetNotFound { name, available }) => {
                    Lookup::Missing { name, available }
                }
                // WI-481, d4a-h N6: the target list another account owns, or this one cannot
                // read.
                Err(CoreError::Cli(refusal)) => Lookup::Refused {
                    name: n.clone(),
                    refusal,
                },
                Err(e) => return Err(e),
            };
            (Some(n), lookup)
        }
        // d4a-h N6: the `config.yaml` that names the CLI default, unreadable.
        Err(refusal) => (
            None,
            Lookup::Unreadable {
                path: store.global_config_file(),
                refusal,
            },
        ),
    };
    let total = if matches!(lookup, Lookup::Found(_)) {
        3
    } else {
        2
    };
    let mut groups = Vec::with_capacity(3);
    stage(reporter, 1, total, "Target");
    groups.push(CheckGroup {
        id: GroupId::Target,
        checks: target_group(ctx, &lookup, cancel)?,
    });
    if let Lookup::Found(target) = &lookup {
        cancel.check()?;
        stage(reporter, 2, total, "Cluster");
        groups.push(CheckGroup {
            id: GroupId::Cluster,
            checks: cluster_group(ctx, target, cancel)?,
        });
    }
    cancel.check()?;
    stage(reporter, total, total, "This computer");
    groups.push(CheckGroup {
        id: GroupId::ThisComputer,
        checks: this_computer_group(
            ctx,
            &window,
            &client,
            match &lookup {
                Lookup::Found(t) => Some(t.name()),
                _ => None,
            },
            cancel,
        )?,
    });
    // A token that tripped while the last check ran, after its own look at it: the run is
    // cancelled all the same, and an interrupted run has no report.
    cancel.check()?;
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

/// `ctx` as doctor reads it (D.4a T66): its client's access behind e's `DiagnosticAccess`,
/// asked only for a session already open — a terminal's unlock window, a key the app holds —
/// never for a prompt or a write key. The CLI's diagnostic context is already wrapped; the
/// desktop's is not, so the core wraps every run.
fn diagnostic(ctx: &Context) -> Context {
    let access: Arc<dyn VaultAccess> = Arc::new(DiagnosticAccess::over(ctx.vault_access().clone()));
    ctx.clone().with_vault_access(access)
}

/// The detail of a row that needs a locked store's secret, with nothing open here.
const STORE_LOCKED: &str = "the target store is locked";
/// The detail of a row whose target has no token this store uses (T42, T34, T43).
const NO_TOKEN_USED: &str = "no token this store uses";
/// The detail of the token row of a locked store's token, located but not read: without a ping
/// nothing is read through an unlock window (T56, T66).
const NOT_READ: &str = "in the vault; read only with the ping";

/// A row that needs a locked store's secret, with nothing open here (D.4a T66): skipped, with
/// the unlock as its fix.
fn locked_skip(base: Check) -> Check {
    Check {
        status: CheckStatus::Skipped,
        detail: Some(STORE_LOCKED.into()),
        fix: Some(CheckFix::UnlockFirst),
        ..base
    }
}

/// A row whose secret no lock state lets be read (an interrupted lock, a vault or
/// `store.yaml` that cannot be read, a slot this build refuses): skipped, with the refusal's
/// own words as its detail. The `secrets` row carries the way out; never a secret.
fn refused_skip(base: Check, e: &CliError) -> Check {
    Check {
        status: CheckStatus::Skipped,
        detail: Some(e.to_string()),
        fix: None,
        ..base
    }
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
    target_group_loading(ctx, lookup, cancel, load)
}

/// [`target_group`], its read of a found target's files by `load`: the store as the lookup
/// left it is never held, so a test lands a removal, and what follows it, between the lookup
/// and this read (WI-483: the races are forced there, never sampled).
fn target_group_loading(
    ctx: &Context,
    lookup: &Lookup,
    cancel: &CancellationToken,
    load: impl FnOnce(&Context, &str) -> cli_core::Result<Loaded>,
) -> CoreResult<Vec<Check>> {
    let target = match lookup {
        Lookup::NoCliDefault => {
            let available = match cli_core::list_target_names(&ctx.store()) {
                Ok(available) => available,
                // d4a-h N6: the list that would offer a target, unreadable, is the row.
                Err(e) => {
                    return Ok(vec![unreadable_config(
                        ctx.store().targets_dir().display().to_string(),
                        &e,
                    )])
                }
            };
            return Ok(vec![Check {
                fix: Some(CheckFix::AddTarget {
                    name: None,
                    available,
                }),
                ..row(CheckId::ActiveTarget, CheckStatus::Warn, "active target")
            }]);
        }
        Lookup::Unreadable { path, refusal } => {
            return Ok(vec![unreadable_config(path.display().to_string(), refusal)])
        }
        Lookup::Missing { name, available } => {
            return Ok(vec![missing_target(name, available.clone())])
        }
        Lookup::Refused { name, refusal } => {
            return Ok(vec![unreadable_config(
                ctx.store().target_config_file(name).display().to_string(),
                refusal,
            )])
        }
        Lookup::Found(target) => target,
    };
    let store = ctx.store();
    let name = target.name();
    let config_file = store.target_config_file(name).display().to_string();
    let loaded = match load(ctx, name) {
        Ok(t) => t,
        // Removed between the lookup and this read; and the list that names what is left may
        // be unreadable by then too (d4a-h N6): that is the row, never a stop.
        Err(CliError::TargetNotFound { .. }) => {
            return Ok(vec![match cli_core::list_target_names(&store) {
                Ok(available) => missing_target(name, available),
                Err(e) => unreadable_config(config_file, &e),
            }]);
        }
        // WI-481: a folder another account owns that this account cannot open is its refusal,
        // naming the folder (`store_privacy::foreign_folder_behind`), not "Permission denied".
        Err(e) => return Ok(vec![unreadable_config(config_file, &e)]),
    };
    Ok(vec![
        Check {
            detail: Some(config_file),
            ..row(CheckId::ConfigReadable, CheckStatus::Pass, TITLE_CONFIG)
        },
        credentials_file(&ctx.secrets(), name),
        provider_supported(&loaded),
        token_format(&loaded),
        token_verified(ctx, &loaded, cancel)?,
        ssh_key(&loaded),
    ])
}

/// The failed "Config file readable" row of a store file that cannot be read: the error's own
/// words as its fix. A path of the store this account cannot read (d4a-h N6) says it as a
/// terminal shows that error: the path, the OS error once, then its help (`ls -ld`, d4a-h N3).
fn unreadable_config(config_file: String, e: &CliError) -> Check {
    let text = match e {
        CliError::StorePathIo { source, .. } => format!("{e}: {source}. {}", help_of(e)),
        e => e.to_string(),
    };
    Check {
        detail: Some(config_file),
        fix: Some(CheckFix::Explain { text }),
        ..row(CheckId::ConfigReadable, CheckStatus::Fail, TITLE_CONFIG)
    }
}

/// A target as doctor checks it: its config and what reading its STORED token gave (never an
/// override, R4).
struct Loaded {
    name: String,
    config: TargetConfig,
    token: TokenRead,
}

/// What doctor's read of a stored token gave (D.4a T66). Never a prompt.
enum TokenRead {
    /// The stored token, or none stored.
    Stored(Option<SecretString>),
    /// A locked store, and nothing open here holds its key.
    Locked,
    /// A locked store's token in its vault, located and not read: a run without a ping reads
    /// nothing through an unlock window, whose every served item restarts it (T56, T66).
    NotRead,
    /// A record the store refuses to use (T42, T34, T43), or a lock state in which no secret
    /// is read: the refusal says which.
    Refused(CliError),
}

/// Both of `name`'s files in `cli_core::load_target`'s order — the config, then the token
/// through the secret store — so a file that cannot be read is the same row as before. On a
/// locked store the token is read only through a session already open (`open_token`), and
/// only for the ping: without one it is located, never read through a window (T56).
fn load(ctx: &Context, name: &str) -> cli_core::Result<Loaded> {
    let config = cli_core::target::load_target_config(&ctx.store(), name)?;
    let secrets = ctx.secrets();
    let token = if ctx.no_ping() && matches!(secrets.mode(), Ok(SecretsMode::Locked)) {
        match secrets.token_presence(name) {
            Ok(StoredToken::None) => TokenRead::Stored(None),
            Ok(StoredToken::OutsideVault) => TokenRead::Refused(CliError::TokenOutsideVault {
                target: name.to_string(),
                path: secrets.credentials_file(name),
            }),
            Ok(StoredToken::InAnotherVault { id }) => {
                TokenRead::Refused(CliError::TokenInAnotherVault {
                    target: name.to_string(),
                    id: id.to_string(),
                })
            }
            Ok(_) => TokenRead::NotRead,
            Err(e) => refusal(e)?,
        }
    } else {
        match secrets.open_token(name) {
            Ok(OpenToken::Token(token)) => TokenRead::Stored(token),
            Ok(OpenToken::Locked) => TokenRead::Locked,
            Err(e) => refusal(e)?,
        }
    };
    Ok(Loaded {
        name: name.to_string(),
        config,
        token,
    })
}

/// A token read's error as doctor's rows report it: a locked store with nothing open, or a
/// record the store refuses (T42, T34, T43), or a lock state in which no secret is read; any
/// other error is the config row's, as before.
fn refusal(e: CliError) -> cli_core::Result<TokenRead> {
    match e {
        CliError::StoreLocked { .. } => Ok(TokenRead::Locked),
        e @ (CliError::TokenOutsideVault { .. }
        | CliError::TokenInAnotherVault { .. }
        | CliError::TokenMissingFromVault { .. }
        | CliError::LockIncomplete { .. }
        | CliError::VaultUnreadable { .. }
        | CliError::SlotRefused { .. }
        | CliError::StoreFileUnreadable { .. }
        | CliError::StoreNotPrivate { .. }) => Ok(TokenRead::Refused(e)),
        e => Err(e),
    }
}

fn credentials_file(secrets: &SecretStore, name: &str) -> Check {
    let path = secrets.credentials_file(name);
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
    #[cfg(windows)]
    {
        // WI-473: its DACL, as Unix's row reads the mode.
        if let Some(wide) = wide_file(CheckId::CredentialsFile, &path, "Credentials file") {
            return wide;
        }
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

#[cfg(windows)]
const RESTRICT_ON_READ_FIX: &str = "Other accounts on this computer may be able to open it. \
     AppRafter makes it yours alone the next time it reads it; doctor only reports.";
#[cfg(windows)]
const RESTRICT_REFUSED_FIX: &str = "Other accounts on this computer may be able to open it, and \
     your account may not change who can: another account owns it, or its permissions keep even \
     its owner from changing them. An administrator can make your account its owner with full \
     control; AppRafter then makes it yours alone the next time it reads it.";

/// The row of a secret file other accounts may open (WI-473), a warning, or `None` when only
/// this account can or there is no file. Unix: a mode that grants group or others anything,
/// with the chmod; Windows: a DACL that is not owner-only (`cli_core::owner_only::exposure`),
/// with whether the next read can make it so. Each read from the file's metadata or security
/// descriptor alone: never its bytes, and nothing is changed. `what` names the file.
#[cfg(unix)]
fn wide_file(id: CheckId, path: &Path, what: &str) -> Option<Check> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).ok()?.permissions().mode() & 0o777;
    if mode & 0o077 == 0 {
        return None;
    }
    let shown = path.display().to_string();
    Some(Check {
        detail: Some(shown.clone()),
        fix: Some(CheckFix::Chmod {
            path: shown,
            mode: 0o600,
        }),
        ..row(
            id,
            CheckStatus::Warn,
            format!("{what} mode 0600 (got {mode:o})"),
        )
    })
}

/// The row of a secret file other accounts may open (WI-473), a warning, or `None` when only
/// this account can or there is no file. Unix: a mode that grants group or others anything,
/// with the chmod; Windows: a DACL that is not owner-only (`cli_core::owner_only::exposure`),
/// with whether the next read can make it so. Each read from the file's metadata or security
/// descriptor alone: never its bytes, and nothing is changed. `what` names the file.
#[cfg(windows)]
fn wide_file(id: CheckId, path: &Path, what: &str) -> Option<Check> {
    use cli_core::owner_only::{exposure, Exposure};
    let (title, fix) = match exposure(path) {
        Ok(Exposure::OwnerOnly) => return None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Ok(Exposure::Wide { restrictable }) => (
            format!("{what} not restricted to your account"),
            if restrictable {
                RESTRICT_ON_READ_FIX.to_string()
            } else {
                RESTRICT_REFUSED_FIX.to_string()
            },
        ),
        // Never-mask: a DACL that cannot be read is said so, not passed over.
        Err(e) => (
            format!("{what}: who can open it is unknown"),
            format!("Its permissions could not be read: {e}"),
        ),
    };
    Some(Check {
        detail: Some(path.display().to_string()),
        fix: Some(explain(&fix)),
        ..row(id, CheckStatus::Warn, title)
    })
}

/// The target store's folders other accounts can change (WI-475), then its secret files that
/// other accounts own or may open (WI-473, WI-475), a warning each, in this order: each folder
/// ([`folder_rows`]); then each target's `credentials.yaml` (the checked target's mode or DACL
/// has its own row) and `state.json`, then the age key, each its owner before its permissions.
/// The vault's are the lock's rows ([`vault_file_rows`]). Read from metadata and security
/// descriptors alone: nothing is changed.
fn secret_file_rows(ctx: &Context, checked: Option<&str>) -> Vec<Check> {
    let store = ctx.store();
    let secrets = ctx.secrets();
    let mut rows = folder_rows(&store);
    for name in cli_core::list_target_names(&store).unwrap_or_default() {
        let credentials = secrets.credentials_file(&name);
        let what = format!("Credentials file of target `{name}`");
        rows.extend(foreign_file(&credentials, &what));
        if Some(name.as_str()) != checked {
            rows.extend(wide_file(CheckId::Secrets, &credentials, &what));
        }
        let state = StatePaths::for_active_target(&store, &name).state_file();
        let what = format!("State file of target `{name}`");
        rows.extend(foreign_file(&state, &what));
        rows.extend(wide_file(CheckId::Secrets, &state, &what));
    }
    rows.extend(foreign_file(ctx.age_key_path(), "Age key"));
    rows.extend(wide_file(CheckId::Secrets, ctx.age_key_path(), "Age key"));
    rows
}

#[cfg(unix)]
const FOREIGN_FOLDER_FIX: &str = "Another account owns this folder of the target store, so it \
     can replace the files in it: a token, a cached kubeconfig or a vault of its own would send \
     your next deploy to a project or a cluster of its choosing. AppRafter uses and writes none \
     of this store's secrets until it is fixed. Move the store to a folder only you can write \
     (APPRAFTER_CONFIG_DIR), or have an administrator make your account its owner (`sudo \
     chown`).";
#[cfg(windows)]
const FOREIGN_FOLDER_FIX: &str = "Another account owns this folder of the target store, so it \
     can give itself the right to replace the files in it, whatever its permissions say: a \
     token, a cached kubeconfig or a vault of its own would send your next deploy to a project \
     or a cluster of its choosing. AppRafter uses and writes none of this store's secrets until \
     it is fixed. Move the store to a folder only you can write (APPRAFTER_CONFIG_DIR), or have \
     an administrator make your account its owner (the folder's Properties › Security › \
     Advanced).";
#[cfg(unix)]
const WRITABLE_FOLDER_FIX: &str = "Other accounts can create, replace or remove files in this \
     folder of the target store: a token, a cached kubeconfig or a vault of theirs would send \
     your next deploy to a project or a cluster of their choosing. AppRafter uses and writes \
     none of this store's secrets until it is fixed. Remove their write access with `chmod \
     go-w` on the folder, or move the store to a folder only you can write \
     (APPRAFTER_CONFIG_DIR).";
#[cfg(windows)]
const WRITABLE_FOLDER_FIX: &str = "Other accounts can create, replace or remove files in this \
     folder of the target store: a token, a cached kubeconfig or a vault of theirs would send \
     your next deploy to a project or a cluster of their choosing. AppRafter uses and writes \
     none of this store's secrets until it is fixed. Remove their write access in the folder's \
     Properties › Security, or move the store to a folder only you can write \
     (APPRAFTER_CONFIG_DIR).";
#[cfg(unix)]
const FOREIGN_FILE_FIX: &str = "Another account owns this file, so it can change what AppRafter \
     reads from it. Have an administrator make your account its owner (`sudo chown`), or put \
     back a copy of your own.";
#[cfg(windows)]
const FOREIGN_FILE_FIX: &str = "Another account owns this file, so it can give itself the right \
     to change what AppRafter reads from it, whatever its permissions say. Have an administrator \
     make your account its owner (the file's Properties › Security › Advanced), or put back a \
     copy of your own.";

/// The rows of the store's folders another account owns or can write (WI-475), a warning each,
/// in `cli_core::store_privacy::folders`' order: a folder may have both. A folder whose owner
/// or permissions cannot be read is said so, never passed over (the store's refusal fails
/// closed on it too), and so is one whose folders cannot be listed, after its own rows (WI-481:
/// it used to hide every row behind one about the store's root). Nothing is changed.
fn folder_rows(store: &cli_core::TargetStorePaths) -> Vec<Check> {
    let folders = match cli_core::store_privacy::folders(store) {
        Ok(folders) => folders,
        Err(e) => {
            return vec![unknown_folder(
                store.root(),
                "Target store folder",
                &e.to_string(),
            )]
        }
    };
    let mut rows = Vec::new();
    for folder in folders {
        let what = if folder.is_root() {
            "Target store folder".to_string()
        } else {
            format!("Target store folder `{}`", folder.rel)
        };
        let control = match cli_core::owner_only::control(&folder.path) {
            Ok(control) => control,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                rows.push(unknown_folder(&folder.path, &what, &e.to_string()));
                continue;
            }
        };
        let shown = folder.path.display().to_string();
        if control.foreign_owner {
            rows.push(Check {
                detail: Some(shown.clone()),
                fix: Some(explain(FOREIGN_FOLDER_FIX)),
                ..row(
                    CheckId::Secrets,
                    CheckStatus::Warn,
                    format!("{what} belongs to another account"),
                )
            });
        }
        if control.writable_by_others {
            let mode = control
                .mode
                .map(|m| format!(" (mode {m:o})"))
                .unwrap_or_default();
            rows.push(Check {
                detail: Some(shown.clone()),
                fix: Some(explain(WRITABLE_FOLDER_FIX)),
                ..row(
                    CheckId::Secrets,
                    CheckStatus::Warn,
                    format!("{what} can be changed by other accounts{mode}"),
                )
            });
        }
        if let Some(why) = &folder.unlisted {
            rows.push(Check {
                detail: Some(shown),
                fix: Some(explain(&format!(
                    "The folders in it could not be listed: {}. Who can change them is unknown, \
                     so AppRafter uses and writes none of this store's secrets until they can be.",
                    why.message
                ))),
                ..row(
                    CheckId::Secrets,
                    CheckStatus::Warn,
                    format!("{what}: who can change the folders in it is unknown"),
                )
            });
        }
    }
    rows
}

fn unknown_folder(path: &Path, what: &str, e: &str) -> Check {
    Check {
        detail: Some(path.display().to_string()),
        fix: Some(explain(&format!(
            "Its owner and permissions could not be read: {e}. AppRafter uses and writes none \
             of this store's secrets until they can be."
        ))),
        ..row(
            CheckId::Secrets,
            CheckStatus::Warn,
            format!("{what}: who can change it is unknown"),
        )
    }
}

/// The row of a secret file another account owns (WI-475), a warning, or `None` when this
/// account does (or there is no file, or its owner cannot be read: [`wide_file`] says that on
/// Windows). Read from its metadata or security descriptor alone; nothing is changed.
fn foreign_file(path: &Path, what: &str) -> Option<Check> {
    let control = cli_core::owner_only::control(path).ok()?;
    if !control.foreign_owner {
        return None;
    }
    Some(Check {
        detail: Some(path.display().to_string()),
        fix: Some(explain(FOREIGN_FILE_FIX)),
        ..row(
            CheckId::Secrets,
            CheckStatus::Warn,
            format!("{what} belongs to another account"),
        )
    })
}

fn provider_supported(target: &Loaded) -> Check {
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

fn token_format(target: &Loaded) -> Check {
    let token = match &target.token {
        TokenRead::Stored(Some(token)) => token.expose(),
        TokenRead::Stored(None) => {
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
            }
        }
        TokenRead::Locked => {
            return locked_skip(row(
                CheckId::TokenFormat,
                CheckStatus::Skipped,
                "Token format valid",
            ))
        }
        TokenRead::NotRead => {
            return Check {
                detail: Some(NOT_READ.into()),
                ..row(
                    CheckId::TokenFormat,
                    CheckStatus::Skipped,
                    "Token format valid",
                )
            }
        }
        TokenRead::Refused(e) => return refused_token(target, e),
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

/// The token row of a record the store refuses to use (D.4a T42, T34, T43) — a failed
/// presence, with the renewal that replaces it (`target add --renew`, d's Task 13b) — or of a
/// lock state in which no secret is read, skipped with the refusal's words. Never the
/// record's bytes.
fn refused_token(target: &Loaded, e: &CliError) -> Check {
    let present = |detail: &str, fix: CheckFix| Check {
        detail: Some(detail.into()),
        fix: Some(fix),
        ..row(
            CheckId::TokenPresent,
            CheckStatus::Fail,
            "Hetzner token present",
        )
    };
    let renew = |why| CheckFix::RenewToken {
        target: target.name.clone(),
        why,
    };
    match e {
        CliError::TokenOutsideVault { .. } => present(
            "in clear in credentials.yaml, which a locked store never uses",
            renew(RenewWhy::TokenOutsideVault),
        ),
        CliError::TokenInAnotherVault { .. } => present(
            "credentials.yaml names another target store's vault",
            renew(RenewWhy::TokenInAnotherVault),
        ),
        CliError::TokenMissingFromVault { .. } => present(
            "the vault holds no token for this target",
            CheckFix::Explain {
                text: miette::Diagnostic::help(e)
                    .map(|h| h.to_string())
                    .unwrap_or_else(|| e.to_string()),
            },
        ),
        _ => refused_skip(
            row(
                CheckId::TokenFormat,
                CheckStatus::Skipped,
                "Token format valid",
            ),
            e,
        ),
    }
}

fn token_verified(ctx: &Context, target: &Loaded, cancel: &CancellationToken) -> CoreResult<Check> {
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
    let token = match &target.token {
        TokenRead::Stored(Some(token)) => token,
        TokenRead::Stored(None) => return Ok(skipped("no token stored".into())),
        TokenRead::Locked => return Ok(skipped(STORE_LOCKED.into())),
        // Only a run without a ping locates rather than reads, and it returned above.
        TokenRead::NotRead => return Ok(skipped("not requested".into())),
        TokenRead::Refused(
            CliError::TokenOutsideVault { .. }
            | CliError::TokenInAnotherVault { .. }
            | CliError::TokenMissingFromVault { .. },
        ) => return Ok(skipped(NO_TOKEN_USED.into())),
        TokenRead::Refused(e) => return Ok(skipped(e.to_string())),
    };
    let provider = target.config.provider.as_str();
    if !SUPPORTED_PROVIDERS.contains(&provider) {
        return Ok(skipped(format!("no validator for provider `{provider}`")));
    }
    cancel.check()?;
    // `ping`, not `verification`: the failure row keeps the API's own message (deviation 1).
    match provider::ping(ctx, provider, token, cancel) {
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

/// The token row's failure, one for each way the ping classifies one (`provider::ping`): 401
/// (renew), 429 (rate-limited), another status, no answer, and an answer it could not read — a
/// provider that answered is never "unreachable" (WI-453 follow-up).
fn ping_failure(name: &str, e: &CoreError) -> Check {
    let fail = row(
        CheckId::TokenVerified,
        CheckStatus::Fail,
        TITLE_TOKEN_VERIFIED,
    );
    let (detail, fix) = match e {
        CoreError::Cli(CliError::ProviderTokenRejected { cause, .. }) => (
            hetzner_status(cause.as_ref()).map_or_else(
                || cause.to_string(),
                |(status, message)| format!("HTTP {status}: {message}"),
            ),
            status_fix(name, 401),
        ),
        CoreError::Cli(CliError::Hetzner {
            status, message, ..
        }) => (
            format!("HTTP {status}: {message}"),
            status_fix(name, *status),
        ),
        CoreError::Cli(CliError::ProviderApiUnreachable { cause, .. }) => {
            (cause.to_string(), CheckFix::ProviderUnreachable)
        }
        CoreError::ProviderRequestFailed { cause, .. } => {
            (format!("{e}: {cause}"), CheckFix::ProviderRequestFailed)
        }
        _ => {
            return Check {
                fix: Some(CheckFix::Explain {
                    text: e.to_string(),
                }),
                ..fail
            }
        }
    };
    Check {
        detail: Some(detail),
        fix: Some(fix),
        ..fail
    }
}

/// The fix for an HTTP error status from the provider, on the token row and the node row alike:
/// 401 renews the token, 429 waits, any other is the provider's error.
fn status_fix(name: &str, status: u16) -> CheckFix {
    match status {
        401 => CheckFix::RenewToken {
            target: name.to_string(),
            why: RenewWhy::TokenRejected,
        },
        429 => CheckFix::ProviderRateLimited,
        status => CheckFix::ProviderError { status },
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

fn ssh_key(target: &Loaded) -> Check {
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
            fix: Some(CheckFix::SshKeyMissing {
                target: target.name.clone(),
                path: shown,
            }),
            ..row(CheckId::SshKey, CheckStatus::Fail, TITLE_SSH_KEY)
        };
    }
    match std::fs::read_to_string(path) {
        // What `apply` would send the provider: one OpenSSH public key line, or the row fails
        // as `apply` would refuse (GOTCHA-149) — a private key by name.
        Ok(body) => match cli_core::ssh_key::parse_public_key(&body) {
            Ok(key) => Check {
                detail: Some(format!("{shown} ({})", key.algo)),
                ..row(CheckId::SshKey, CheckStatus::Pass, TITLE_SSH_KEY)
            },
            Err(why) => Check {
                detail: Some(shown.clone()),
                fix: Some(CheckFix::SshKeyNotPublic {
                    target: target.name.clone(),
                    path: shown,
                    private_key: why == cli_core::ssh_key::NotAPublicKey::PrivateKey,
                }),
                ..row(CheckId::SshKey, CheckStatus::Fail, TITLE_SSH_KEY)
            },
        },
        Err(e) => Check {
            detail: Some(shown),
            fix: Some(CheckFix::Explain {
                text: format!("cannot read file: {e}"),
            }),
            ..row(CheckId::SshKey, CheckStatus::Fail, TITLE_SSH_KEY)
        },
    }
}

fn cluster_group(
    ctx: &Context,
    target: &TargetRef,
    cancel: &CancellationToken,
) -> CoreResult<Vec<Check>> {
    let paths = StatePaths::for_active_target(&ctx.store(), target.name());
    let skip_all = |detail: &str| {
        [
            (CheckId::KubeconfigCached, TITLE_KUBECONFIG_CACHED),
            (CheckId::KubeApiReachable, TITLE_KUBE_API),
            (CheckId::NodeSshReachable, TITLE_NODE_SSH),
        ]
        .into_iter()
        .map(|(id, title)| Check {
            detail: Some(detail.to_string()),
            ..row(id, CheckStatus::Skipped, title)
        })
        .collect::<Vec<_>>()
    };
    let state = match State::load_or_default(&paths) {
        Ok(state) => state,
        Err(e) => {
            let mut rows = skip_all("state unreadable");
            rows[0] = Check {
                detail: Some(paths.state_file().display().to_string()),
                fix: Some(CheckFix::Explain {
                    text: e.to_string(),
                }),
                ..row(
                    CheckId::KubeconfigCached,
                    CheckStatus::Fail,
                    TITLE_KUBECONFIG_CACHED,
                )
            };
            return Ok(rows);
        }
    };
    let Some(server) = state.hetzner_cloud else {
        return Ok(skip_all("no provisioned server"));
    };
    let cached = kubeconfig_cached(target.name(), &server);
    cancel.check()?;
    let api = kube_api(ctx, target, &server, cancel)?;
    cancel.check()?;
    let node = node_ssh(ctx, target, cancel, SSH_PORT)?;
    Ok(vec![cached, api, node])
}

/// Which slot holds the cached kubeconfig. spec §4.5: doctor WARNs while a plaintext slot
/// remains, and the fix re-fetches it encrypted.
fn kubeconfig_cached(name: &str, s: &HetznerCloudState) -> Check {
    let base = row(
        CheckId::KubeconfigCached,
        CheckStatus::Pass,
        TITLE_KUBECONFIG_CACHED,
    );
    let fetch = Some(CheckFix::FetchKubeconfig {
        target: name.to_string(),
    });
    match (s.kubeconfig_age.is_some(), s.kubeconfig_yaml.is_some()) {
        (true, false) => Check {
            detail: Some("encrypted".into()),
            ..base
        },
        (true, true) => Check {
            status: CheckStatus::Warn,
            detail: Some("encrypted, and an unencrypted legacy copy remains".into()),
            fix: fetch,
            ..base
        },
        (false, true) => Check {
            status: CheckStatus::Warn,
            detail: Some("stored unencrypted (legacy)".into()),
            fix: fetch,
            ..base
        },
        (false, false) => Check {
            status: CheckStatus::Fail,
            detail: Some(format!(
                "none cached for server `{}` (id {})",
                s.server_name, s.server_id
            )),
            fix: fetch,
            ..base
        },
    }
}

/// Order: a cache to probe → the cache key, through the secret store, read WITHOUT creating one
/// (only for the encrypted slot) → kubectl resolves → decrypt into the runtime dir → `/version` within the bound. The
/// file is removed when `file` drops, at the end of this function; nothing is written when a
/// step before the decrypt says the probe cannot run.
fn kube_api(
    ctx: &Context,
    target: &TargetRef,
    s: &HetznerCloudState,
    cancel: &CancellationToken,
) -> CoreResult<Check> {
    let base = row(
        CheckId::KubeApiReachable,
        CheckStatus::Skipped,
        TITLE_KUBE_API,
    );
    if s.kubeconfig_age.is_none() && s.kubeconfig_yaml.is_none() {
        return Ok(Check {
            detail: Some("no cached kubeconfig".into()),
            ..base
        });
    }
    if s.kubeconfig_age.is_some() {
        match ctx.secrets().cache_key(target.name()) {
            Ok(_) => {}
            Err(CliError::AgeKeyMissing { path, .. }) => {
                return Ok(age_key_missing(base, target, path, s))
            }
            // D.4a T66: a locked store with nothing open here — skipped, never asked.
            Err(CliError::StoreLocked { .. }) => return Ok(locked_skip(base)),
            Err(
                e @ (CliError::LockIncomplete { .. }
                | CliError::VaultUnreadable { .. }
                | CliError::SlotRefused { .. }
                | CliError::StoreFileUnreadable { .. }
                | CliError::StoreNotPrivate { .. }),
            ) => return Ok(refused_skip(base, &e)),
            Err(e) => {
                return Ok(Check {
                    status: CheckStatus::Fail,
                    fix: Some(CheckFix::Explain {
                        text: e.to_string(),
                    }),
                    ..base
                });
            }
        }
    }
    if let Err(e) = ctx.tools().resolve(ToolId::Kubectl) {
        let detail = match &e {
            CoreError::ToolUnsupported { .. } => e.to_string(),
            _ => "`kubectl` not found".to_string(),
        };
        return Ok(Check {
            detail: Some(detail),
            fix: Some(CheckFix::InstallTool {
                tool: ToolId::Kubectl,
            }),
            ..base
        });
    }
    let file = match runtime::materialise_kubeconfig(ctx, target) {
        Ok(Some(file)) => file,
        Ok(None) => {
            return Ok(Check {
                detail: Some("no cached kubeconfig".into()),
                ..base
            })
        }
        Err(CoreError::AgeKeyMissing { path }) => {
            return Ok(age_key_missing(base, target, path, s))
        }
        Err(CoreError::Cli(CliError::StoreLocked { .. })) => return Ok(locked_skip(base)),
        // WI-475: a kubeconfig another account could have put there is never run.
        Err(CoreError::Cli(e @ CliError::StoreNotPrivate { .. })) => {
            return Ok(refused_skip(base, &e))
        }
        // A key that does not open the cache: its own fix, which names the refetch (review #8).
        Err(CoreError::Cli(cli_core::CliError::CacheUndecryptable {
            target: whose,
            path,
            key,
            ..
        })) => {
            return Ok(Check {
                status: CheckStatus::Fail,
                detail: None,
                fix: Some(CheckFix::CacheUndecryptable {
                    target: whose,
                    path,
                    key: key.into(),
                }),
                ..base
            })
        }
        Err(e) => {
            return Ok(Check {
                status: CheckStatus::Fail,
                detail: Some("the cached kubeconfig cannot be read".into()),
                fix: Some(CheckFix::Explain {
                    text: e.to_string(),
                }),
                ..base
            });
        }
    };
    let kube = match KubectlKube::new(ctx, &file) {
        Ok(kube) => kube,
        Err(e) => {
            return Ok(Check {
                detail: Some(e.to_string()),
                fix: Some(CheckFix::InstallTool {
                    tool: ToolId::Kubectl,
                }),
                ..base
            })
        }
    };
    match kube.server_version(cancel) {
        Ok(v) => Ok(Check {
            status: CheckStatus::Pass,
            detail: Some(format!("{} · {} ms", v.git_version, v.elapsed_ms)),
            ..base
        }),
        Err(CoreError::Cancelled) => Err(CoreError::Cancelled),
        Err(CoreError::Kube { kind, detail }) => Ok(Check {
            status: CheckStatus::Fail,
            detail: Some(detail),
            fix: Some(CheckFix::ClusterUnreachable { reason: kind }),
            ..base
        }),
        Err(e) => Ok(Check {
            status: CheckStatus::Fail,
            fix: Some(CheckFix::Explain {
                text: e.to_string(),
            }),
            ..base
        }),
    }
}

fn age_key_missing(base: Check, target: &TargetRef, path: String, s: &HetznerCloudState) -> Check {
    Check {
        status: CheckStatus::Fail,
        detail: None,
        fix: Some(CheckFix::AgeKeyMissing {
            target: target.name().to_string(),
            path,
            sealed_to_lost_vault: matches!(
                s.kubeconfig_sealed_to,
                Some(cli_state::SealedTo::StoreVault { .. })
            ),
        }),
        ..base
    }
}

/// `public_address` (GET /v1/servers/{id}, the token per R4) then a bounded TCP connect. IPv4
/// only: Hetzner's IPv6 is a /64 prefix, not an address to dial.
fn node_ssh(
    ctx: &Context,
    target: &TargetRef,
    cancel: &CancellationToken,
    port: u16,
) -> CoreResult<Check> {
    let base = row(
        CheckId::NodeSshReachable,
        CheckStatus::Skipped,
        TITLE_NODE_SSH,
    );
    let skipped = |detail: &str| Check {
        detail: Some(detail.to_string()),
        ..base.clone()
    };
    if ctx.no_ping() {
        return Ok(skipped("not requested"));
    }
    let address = match crate::target::public_address(ctx, target, cancel) {
        Ok(address) => address,
        Err(CoreError::Cancelled) => return Err(CoreError::Cancelled),
        Err(CoreError::NotProvisioned { .. }) => return Ok(skipped("no provisioned server")),
        Err(CoreError::TokenNotStored { .. }) => return Ok(skipped("no token stored")),
        // D.4a T66: the token is read only through a session already open here.
        Err(CoreError::Cli(CliError::StoreLocked { .. })) => return Ok(locked_skip(base.clone())),
        Err(CoreError::Cli(
            CliError::TokenOutsideVault { .. }
            | CliError::TokenInAnotherVault { .. }
            | CliError::TokenMissingFromVault { .. },
        )) => return Ok(skipped(NO_TOKEN_USED)),
        Err(CoreError::Cli(
            e @ (CliError::LockIncomplete { .. }
            | CliError::VaultUnreadable { .. }
            | CliError::SlotRefused { .. }
            | CliError::StoreFileUnreadable { .. }
            | CliError::StoreNotPrivate { .. }),
        )) => return Ok(refused_skip(base.clone(), &e)),
        Err(e) => return Ok(provider_failure(base, target.name(), e)),
    };
    let Some(ip) = address.ipv4 else {
        return Ok(skipped("the server has no public IPv4 address"));
    };
    cancel.check()?;
    match net::tcp_probe(&ip, port, ctx.request_timeout(), cancel) {
        Ok(_) => Ok(Check {
            status: CheckStatus::Pass,
            detail: Some(format!("port {port} · {ip}")),
            ..base
        }),
        Err(e) => {
            cancel.check()?;
            Ok(Check {
                status: CheckStatus::Fail,
                detail: Some(format!("port {port} · {ip}: {e}")),
                fix: Some(CheckFix::NodeUnreachable { address: ip }),
                ..base
            })
        }
    }
}

/// The node row when the provider could not say where the server is: an HTTP status (401
/// renews, 429 waits, any other is the provider's error), no answer (the transport failure as the
/// detail, as on the token row), an answer it could not read (the parse error as the detail; WI-453
/// follow-up: it was "unreachable"), or — `ServerMissing` and anything else — the core's own
/// sentence.
fn provider_failure(base: Check, name: &str, e: CoreError) -> Check {
    let (text, fix) = match &e {
        CoreError::Cli(CliError::Hetzner { status, .. }) => {
            (e.to_string(), status_fix(name, *status))
        }
        CoreError::Cli(CliError::ProviderApiUnreachable { cause, .. }) => {
            (cause.to_string(), CheckFix::ProviderUnreachable)
        }
        CoreError::ProviderRequestFailed { cause, .. } => {
            (format!("{e}: {cause}"), CheckFix::ProviderRequestFailed)
        }
        _ => {
            return Check {
                status: CheckStatus::Fail,
                fix: Some(CheckFix::Explain {
                    text: e.to_string(),
                }),
                ..base
            }
        }
    };
    Check {
        status: CheckStatus::Fail,
        detail: Some(text),
        fix: Some(fix),
        ..base
    }
}

fn this_computer_group(
    ctx: &Context,
    window: &UnlockWindow,
    client: &LimitClient,
    checked: Option<&str>,
    cancel: &CancellationToken,
) -> CoreResult<Vec<Check>> {
    // Concurrent probes, each killed at TOOL_PROBE_TIMEOUT, in ToolId::ALL order.
    let toolchain = tools::toolchain(ctx, cancel)?;
    let mut out: Vec<Check> = toolchain.tools.iter().map(tool_check).collect();
    cancel.check()?;
    // The lookup ends within 50 ms of a cancel; `run` turns the row it leaves into `Cancelled`.
    out.push(dns_check(ctx, cancel));
    // D.4a §3.13: the lock's rows, after the tools and DNS. The ssh-agent query among them
    // waits up to 2 s and cannot be interrupted: a Ctrl-C is honoured before it.
    cancel.check()?;
    out.extend(lock_rows(ctx, window, client, checked));
    out.extend(secret_file_rows(ctx, checked));
    Ok(out)
}

/// The lock's rows (D.4a §3.13, T71–T73, O1): the store's lock state, the facts beside a locked
/// store, and — on a locked or locking store — what this computer lets other programs running
/// as you do and what the lock does not cover. They read `store.yaml`, the vault's header
/// (parsed, never opened), the targets' markers, the vault file's metadata and the client's
/// facts: never a secret, never a prompt, never a key derived.
fn lock_rows(
    ctx: &Context,
    window: &UnlockWindow,
    client: &LimitClient,
    checked: Option<&str>,
) -> Vec<Check> {
    let mode = match ctx.secrets().mode() {
        Ok(mode) => mode,
        Err(e) => {
            // T31: a store whose `store.yaml` cannot be read is treated as locked, so this
            // computer's limits show too (rule 1).
            let limits = lock::limits::detect_limits(ctx, client, window);
            return std::iter::once(store_file_unreadable(&e))
                .chain(limits.iter().filter_map(limit_check))
                .collect();
        }
    };
    let shows_limits = matches!(mode, SecretsMode::Locked | SecretsMode::Locking);
    // A store whose limits are not shown asks no ssh-agent (its 2 s bound is for a row).
    let asked = if shows_limits {
        client.clone()
    } else {
        without_ssh(client)
    };
    let report = lock::status(ctx, window.clone(), &asked);
    let mut rows = vec![secrets_row(mode, &report, window)];
    if let (SecretsMode::Locked, Ok(r)) = (mode, &report) {
        rows.extend(locked_facts(ctx, r, checked));
    }
    if shows_limits {
        if let UnlockWindow::Open { hardening, .. } = window {
            if let Some(line) = text::hardening_line(hardening) {
                rows.push(Check {
                    fix: Some(explain(HARDENING_FIX)),
                    ..row(CheckId::WindowHardening, CheckStatus::Warn, line)
                });
            }
        }
        let limits = match &report {
            Ok(r) => r.limits.clone(),
            // A vault that cannot be read hides none of this computer's limits (rule 1).
            Err(_) => lock::limits::detect_limits(ctx, client, window),
        };
        rows.extend(limits.iter().filter_map(limit_check));
    }
    rows
}

/// `client` without its SSH facts: nothing asks the ssh-agent.
fn without_ssh(client: &LimitClient) -> LimitClient {
    match client {
        LimitClient::Cli { terminal, .. } => LimitClient::Cli {
            terminal: terminal.clone(),
            ssh: SshFacts::default(),
        },
        LimitClient::Desktop {
            session_events,
            display,
            ..
        } => LimitClient::Desktop {
            session_events: *session_events,
            display: *display,
            ssh: SshFacts::default(),
        },
    }
}

fn explain(text: &str) -> CheckFix {
    CheckFix::Explain {
        text: text.to_string(),
    }
}

/// A refusal's own help, for a row's explanation: the store's words, never a secret.
fn help_of(e: &CliError) -> String {
    miette::Diagnostic::help(e)
        .map(|h| h.to_string())
        .unwrap_or_else(|| e.to_string())
}

/// The `secrets` row (§3.13).
fn secrets_row(mode: SecretsMode, report: &CoreResult<LockReport>, window: &UnlockWindow) -> Check {
    match (mode, report) {
        (SecretsMode::Files, _) => Check {
            fix: Some(CheckFix::ProtectSecrets),
            ..row(
                CheckId::Secrets,
                CheckStatus::Info,
                "Secrets in files (readable by any program running as you)",
            )
        },
        (SecretsMode::Locking | SecretsMode::Unlocking, _) => Check {
            fix: Some(CheckFix::FinishLock { phase: mode }),
            ..row(
                CheckId::Secrets,
                CheckStatus::Fail,
                if mode == SecretsMode::Locking {
                    "Secrets: turning the lock on did not finish"
                } else {
                    "Secrets: turning the lock off did not finish"
                },
            )
        },
        (SecretsMode::Locked, Ok(r)) => Check {
            detail: Some(text::window_words(window)),
            ..row(
                CheckId::Secrets,
                CheckStatus::Pass,
                format!("Secrets locked ({})", text::slot_count(&r.slots)),
            )
        },
        (SecretsMode::Locked, Err(e)) => Check {
            detail: Some(e.to_string()),
            fix: Some(explain(&match e {
                CoreError::Cli(cli) => help_of(cli),
                other => other.to_string(),
            })),
            ..row(
                CheckId::Secrets,
                CheckStatus::Fail,
                "Secrets locked, but the vault cannot be read",
            )
        },
    }
}

/// `store.yaml` cannot be read (T31): the store is treated as locked, and this is why.
fn store_file_unreadable(e: &CliError) -> Check {
    Check {
        detail: Some(e.to_string()),
        fix: Some(explain(&help_of(e))),
        ..row(
            CheckId::Secrets,
            CheckStatus::Fail,
            "Secrets: whether they are locked cannot be read",
        )
    }
}

const SLOT_DRIFT_FIX: &str = "store.yaml only mirrors the vault's slots for display: the vault \
     decides what opens it, so an edit there adds or removes no slot. `apprafter lock status` \
     lists the vault's own.";
#[cfg(unix)]
const FOREIGN_OWNER_FIX: &str = "AppRafter running as you did not write a vault another account \
     owns. Restore it from a backup you trust.";
const NO_RECOVERY_FIX: &str = "Without a recovery key, a forgotten lock password loses every \
     secret in the vault. While the lock password still opens it, run `apprafter lock off` and \
     then `apprafter lock on`, which makes a new recovery key.";
const HARDENING_FIX: &str = "The operating system refused one of the protections the unlock \
     window's holder puts on its key; the window works without it. `apprafter lock now` ends \
     the window.";

/// The facts beside a locked store (§3.13), each a warning: a slot list in `store.yaml` unlike
/// the vault's (T29), a slot this build refuses or no recovery-key slot (T22, O8), the vault
/// file's mode or owner (T27), an override variable set (T35), another target's token this
/// store never uses (T42, T34) and another target's kubeconfig in clear (T38, overview rule 2:
/// never used, always reported). The target `checked` has its own token and kubeconfig rows.
fn locked_facts(ctx: &Context, r: &LockReport, checked: Option<&str>) -> Vec<Check> {
    let secrets = ctx.secrets();
    let mut rows = Vec::new();
    if let Ok(Some(file)) = secrets.store_file() {
        let mirrored: Vec<(LockSlotKind, &str)> = file
            .slots
            .iter()
            .map(|s| (s.kind, s.label.as_str()))
            .collect();
        let held: Vec<(LockSlotKind, &str)> =
            r.slots.iter().map(|s| (s.kind, s.label.as_str())).collect();
        if mirrored != held {
            rows.push(Check {
                fix: Some(explain(SLOT_DRIFT_FIX)),
                ..row(
                    CheckId::Secrets,
                    CheckStatus::Warn,
                    "The slot list in store.yaml differs from the vault's",
                )
            });
        }
    }
    rows.extend(slot_facts(&r.slots, ctx.config_root()));
    rows.extend(vault_file_rows(ctx));
    let o = ctx.overrides();
    rows.extend(override_rows(
        o.hetzner_token.is_some(),
        o.age_key.is_some(),
        ctx.config_root(),
    ));
    for name in cli_core::list_target_names(&ctx.store()).unwrap_or_default() {
        if Some(name.as_str()) == checked {
            continue;
        }
        let token = match secrets.token_presence(&name) {
            Ok(StoredToken::OutsideVault) => Some((
                RenewWhy::TokenOutsideVault,
                "holds a token in clear, which this locked store never uses",
            )),
            Ok(StoredToken::InAnotherVault { .. }) => Some((
                RenewWhy::TokenInAnotherVault,
                "names another target store's vault",
            )),
            _ => None,
        };
        if let Some((why, what)) = token {
            rows.push(Check {
                fix: Some(CheckFix::RenewToken {
                    target: name.clone(),
                    why,
                }),
                ..row(
                    CheckId::Secrets,
                    CheckStatus::Warn,
                    format!("Target `{name}`'s credentials file {what}"),
                )
            });
        }
        // A state file that cannot be read is that target's own doctor run's to report.
        let paths = StatePaths::for_active_target(&ctx.store(), &name);
        let clear_kubeconfig = State::load_or_default(&paths)
            .ok()
            .and_then(|state| state.hetzner_cloud)
            .is_some_and(|s| s.kubeconfig_yaml.is_some());
        if clear_kubeconfig {
            rows.push(Check {
                fix: Some(CheckFix::FetchKubeconfig {
                    target: name.clone(),
                }),
                ..row(
                    CheckId::Secrets,
                    CheckStatus::Warn,
                    format!(
                        "Target `{name}`'s state file holds a kubeconfig in clear, which this \
                         locked store never uses"
                    ),
                )
            });
        }
    }
    rows
}

/// A slot this build refuses (T22) and a vault with no recovery-key slot (O8), each a warning:
/// a refused password slot is either faster to guess offline than this build allows or cannot
/// open the vault, and without a recovery key a forgotten lock password loses the vault. Read
/// from the report's slot views: nothing is derived.
fn slot_facts(slots: &[SlotView], store: &Path) -> Vec<Check> {
    let mut rows: Vec<Check> = slots
        .iter()
        .filter_map(|s| {
            let (reason, what) = match s.refused? {
                SlotRefusalView::KdfTooWeak => (
                    SlotRefusal::KdfTooWeak,
                    "weaker than this AppRafter accepts: a copy of the vault is faster to guess \
                     than it allows",
                ),
                SlotRefusalView::KdfOutOfRange => (
                    SlotRefusal::KdfOutOfRange,
                    "beyond what this AppRafter computes: it cannot open the vault",
                ),
            };
            let refusal = CliError::SlotRefused {
                store: store.to_path_buf(),
                reason,
            };
            Some(Check {
                fix: Some(explain(&help_of(&refusal))),
                ..row(
                    CheckId::Secrets,
                    CheckStatus::Warn,
                    format!("The `{}` slot uses key-derivation settings {what}", s.label),
                )
            })
        })
        .collect();
    if !slots.iter().any(|s| s.kind == LockSlotKind::RecoveryKey) {
        rows.push(Check {
            fix: Some(explain(NO_RECOVERY_FIX)),
            ..row(
                CheckId::Secrets,
                CheckStatus::Warn,
                "The vault has no recovery-key slot",
            )
        });
    }
    rows
}

/// The vault file's owner and DACL on Windows, as every secret file's ([`foreign_file`],
/// WI-475; [`wide_file`], WI-473): a warning each, never a refusal of the file (T27).
#[cfg(windows)]
fn vault_file_rows(ctx: &Context) -> Vec<Check> {
    let path = ctx.config_root().join(crate::vault::VAULT_FILE);
    foreign_file(&path, "Vault file")
        .into_iter()
        .chain(wide_file(CheckId::Secrets, &path, "Vault file"))
        .collect()
}

/// The vault file's mode and owner (T27: a wider mode or a foreign owner is a warning, never a
/// refusal). The owner is compared with the store directory's: no process identity is read. On
/// Windows its DACL, as every secret file's ([`wide_file`], WI-473).
#[cfg(unix)]
fn vault_file_rows(ctx: &Context) -> Vec<Check> {
    use std::os::unix::fs::MetadataExt;
    let path = ctx.config_root().join(crate::vault::VAULT_FILE);
    // Unreadable: the `secrets` row already says why.
    let Ok(meta) = std::fs::metadata(&path) else {
        return Vec::new();
    };
    let shown = path.display().to_string();
    let mut rows: Vec<Check> = wide_file(CheckId::Secrets, &path, "Vault file")
        .into_iter()
        .collect();
    if let Ok(root) = std::fs::metadata(ctx.config_root()) {
        if root.uid() != meta.uid() {
            rows.push(Check {
                detail: Some(shown),
                fix: Some(explain(FOREIGN_OWNER_FIX)),
                ..row(
                    CheckId::Secrets,
                    CheckStatus::Warn,
                    "The vault file belongs to another account than its target store",
                )
            });
        }
    }
    rows
}

/// `HCLOUD_TOKEN` / `APPRAFTER_AGE_KEY` set beside a locked store (T35): named, never shown,
/// with the refusal's own help (T70: a store of its own for a value from the environment).
fn override_rows(hcloud: bool, age_key: bool, store: &Path) -> Vec<Check> {
    [
        (hcloud, cli_core::HCLOUD_TOKEN_ENV),
        (age_key, cli_core::secrets::AGE_KEY_ENV),
    ]
    .into_iter()
    .filter(|(set, _)| *set)
    .map(|(_, var)| {
        let refusal = CliError::OverrideOnLockedStore {
            var,
            store: store.to_path_buf(),
        };
        Check {
            fix: Some(explain(&help_of(&refusal))),
            ..row(
                CheckId::Secrets,
                CheckStatus::Warn,
                format!("{var} is set, and this locked store refuses it"),
            )
        }
    })
    .collect()
}

const X11_FIX: &str = "Under X11 any program running as you can read what you type, the lock \
     password included. A Wayland session, with a Wayland-native terminal, closes this.";
const APP_ON_XWAYLAND_FIX: &str = "AppRafter runs as an X11 program here, under Xwayland: \
     other X programs running as you can read what you type into it, the lock password included. \
     Start it natively on Wayland, without `GDK_BACKEND=x11`, to close this.";
const PTRACE_FIX: &str = "Set the kernel.yama.ptrace_scope sysctl to 1, so that only a parent \
     process can attach to another.";
const WINDOWS_FIX: &str = "Any program running as you can attach itself to an unlocked console. \
     Unlock in a console of its own, keep the window short, and run AI agents under a separate \
     Standard Windows account.";
const MACOS_FIX: &str = "Whether such a program can read another's memory depends on this \
     Mac's debugging settings. Run AI agents under a separate Standard macOS account.";
const INHERITED_FIX: &str = "Start AI agents in another terminal, or end the window with \
     `apprafter lock now`.";
const SLEEP_FIX: &str = "Lock the app yourself before the computer sleeps, and keep its \
     auto-lock short.";
const IN_USE_FIX: &str = "`apprafter open argocd` also puts the Argo CD password on the \
     clipboard and forwards a local port any program on this computer can connect to, and a \
     kubeconfig saved with `apprafter kubeconfig` stays where it was saved, outside the lock.";
const SAME_USER_FIX: &str = "The lock stops an AI agent that oversteps; one that sets out to get \
     past it — an agent turned by a prompt injection, for example — it only slows. Run AI agents \
     under a separate OS account without administrator rights, or in a sandbox that cannot \
     reach your terminals, files or ssh-agent.";
const HIBERNATION_FIX: &str = "End every unlock window before this computer hibernates: run \
     `apprafter lock now` in each terminal you unlocked, and lock the app.";

/// A limit of this computer as a doctor row (§3.13): its id, the weight and the sentence of
/// `lock status`'s line (`lock::text`), and its fix. The holder base's fallback is a `lock
/// status` line only.
fn limit_check(l: &MachineLimit) -> Option<Check> {
    let fix = match l {
        MachineLimit::X11Session | MachineLimit::Xwayland { .. } => explain(X11_FIX),
        MachineLimit::AppOnXwayland => explain(APP_ON_XWAYLAND_FIX),
        MachineLimit::PtraceScope { .. } => explain(PTRACE_FIX),
        MachineLimit::WindowsSameUser => explain(WINDOWS_FIX),
        MachineLimit::MacosTaskPort => explain(MACOS_FIX),
        MachineLimit::Multiplexer { name } => CheckFix::Explain {
            text: format!(
                "Programs running as you can send keystrokes to this terminal through {name}'s \
                 own commands, into an unlocked window too. Unlock in a terminal outside {name}."
            ),
        },
        MachineLimit::InheritedWindow => explain(INHERITED_FIX),
        MachineLimit::SleepUnreported => explain(SLEEP_FIX),
        MachineLimit::SshKeyUsable { path, .. } => CheckFix::ProtectSshKey { path: path.clone() },
        MachineLimit::SecretsInUse => explain(IN_USE_FIX),
        MachineLimit::SameUserPrograms => explain(SAME_USER_FIX),
        MachineLimit::Hibernation => explain(HIBERNATION_FIX),
        MachineLimit::RuntimeDirFallback { .. } => return None,
    };
    let id = match l {
        MachineLimit::X11Session | MachineLimit::Xwayland { .. } | MachineLimit::AppOnXwayland => {
            CheckId::X11Keystrokes
        }
        MachineLimit::PtraceScope { .. } => CheckId::PtraceScope,
        MachineLimit::WindowsSameUser => CheckId::WindowsSameUser,
        MachineLimit::MacosTaskPort => CheckId::MacosTaskPort,
        MachineLimit::Multiplexer { .. } => CheckId::TerminalMultiplexer,
        MachineLimit::InheritedWindow => CheckId::UnlockedTerminal,
        MachineLimit::SleepUnreported => CheckId::SleepUnreported,
        MachineLimit::SshKeyUsable { .. } => CheckId::SshKeyExposure,
        MachineLimit::SecretsInUse => CheckId::SecretsInUse,
        MachineLimit::SameUserPrograms => CheckId::SameUserPrograms,
        MachineLimit::Hibernation => CheckId::Hibernation,
        MachineLimit::RuntimeDirFallback { .. } => return None,
    };
    let line = text::limit_line(l)?;
    let status = if line.warn {
        CheckStatus::Warn
    } else {
        CheckStatus::Info
    };
    Some(Check {
        fix: Some(fix),
        ..row(id, status, line.text)
    })
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

fn dns_check(ctx: &Context, cancel: &CancellationToken) -> Check {
    let host = api_host(ctx.hcloud_base_url());
    let title = format!("DNS resolves `{host}`");
    let netloc = if host.contains(':') {
        format!("[{host}]:{DNS_PORT}")
    } else {
        format!("{host}:{DNS_PORT}")
    };
    match net::resolve_with_deadline(&netloc, ctx.request_timeout(), cancel) {
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
    use cli_core::target::{GlobalConfig, Target, TargetConfig, TargetCredentials};
    use cli_state::{HetznerCloudState, State, StatePaths};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::lock::limits::{DisplayKind, LimitClient, SshFacts, TerminalFacts};
    use crate::lock::{HolderHardening, MachineLimit, UnlockWindow};

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
        doctor_with(ctx, target, UnlockWindow::NoTerminal, quiet())
    }

    /// The CLI's client with no terminal and no SSH facts: no limit row depends on the host
    /// beyond what `detect_limits` reads itself (Yama, the OS).
    fn quiet() -> LimitClient {
        LimitClient::Cli {
            terminal: None,
            ssh: SshFacts::default(),
        }
    }

    fn doctor_with(
        ctx: &Context,
        target: DoctorTarget,
        window: UnlockWindow,
        client: LimitClient,
    ) -> DoctorReport {
        run(
            ctx,
            DoctorArgs {
                target,
                window,
                client,
            },
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

    /// WI-473 (Unix): the store's other secret files whose mode lets group or others in are a
    /// warning each, with the chmod — another target's credentials, each state file, the age
    /// key — read from their metadata, and left as they were. Owner-only files are no row.
    #[cfg(unix)]
    #[test]
    fn wide_secret_files_are_a_warning_each_with_the_chmod() {
        use std::os::unix::fs::PermissionsExt;
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        add(&ctx, "dev", "hetzner-cloud", Some(TOKEN), None);
        add(&ctx, "qa", "hetzner-cloud", Some(TOKEN), None);
        for t in ["dev", "prod", "qa"] {
            State::default()
                .save(&StatePaths::for_active_target(&ctx.store(), t))
                .unwrap();
        }
        cli_core::secrets::load_or_create_identity(ctx.age_key_path()).unwrap();
        let healthy = group(&doctor(&ctx, named("prod")), GroupId::ThisComputer);
        assert!(
            healthy.iter().all(|c| !c.title.contains("mode 0600")),
            "{healthy:?}"
        );
        let wide = [
            ctx.store().target_credentials_file("dev"),
            StatePaths::for_active_target(&ctx.store(), "prod").state_file(),
            ctx.age_key_path().to_path_buf(),
        ];
        for p in &wide {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let rows: Vec<Check> = group(&doctor(&ctx, named("prod")), GroupId::ThisComputer)
            .into_iter()
            .filter(|c| c.title.contains("mode 0600"))
            .collect();
        let expected = [
            "Credentials file of target `dev` mode 0600 (got 644)",
            "State file of target `prod` mode 0600 (got 644)",
            "Age key mode 0600 (got 644)",
        ];
        assert_eq!(
            rows.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            expected
        );
        for (row, path) in rows.iter().zip(&wide) {
            assert_eq!(row.id, CheckId::Secrets);
            assert_eq!(row.status, CheckStatus::Warn);
            let shown = path.display().to_string();
            assert_eq!(row.detail.as_deref(), Some(shown.as_str()));
            assert_eq!(
                row.fix,
                Some(CheckFix::Chmod {
                    path: shown,
                    mode: 0o600
                })
            );
            let mode = std::fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o644, "doctor changes nothing: {path:?}");
        }
    }

    /// WI-473 (Windows): a secret file other accounts may open is a warning, the checked
    /// target's credentials on their own row, the rest in This computer; the fix says whether
    /// the next read can make it owner-only. Doctor reads the token and the state through the
    /// store all the same, and leaves every DACL as it was.
    #[cfg(windows)]
    #[test]
    fn wide_secret_files_are_a_warning_each_and_doctor_changes_none() {
        use cli_core::owner_only::testing;
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        add(&ctx, "dev", "hetzner-cloud", Some(TOKEN), None);
        for t in ["dev", "prod"] {
            State::default()
                .save(&StatePaths::for_active_target(&ctx.store(), t))
                .unwrap();
        }
        cli_core::secrets::load_or_create_identity(ctx.age_key_path()).unwrap();
        let healthy = doctor(&ctx, named("prod"));
        let creds = group(&healthy, GroupId::Target)[1].clone();
        assert_eq!(
            (creds.status, creds.title.as_str()),
            (CheckStatus::Pass, TITLE_CREDENTIALS)
        );
        assert!(
            group(&healthy, GroupId::ThisComputer)
                .iter()
                .all(|c| !c.title.contains("restricted")),
            "{healthy:?}"
        );
        let prod_creds = ctx.store().target_credentials_file("prod");
        let wide = [
            ctx.store().target_credentials_file("dev"),
            StatePaths::for_active_target(&ctx.store(), "prod").state_file(),
        ];
        for p in wide.iter().chain([&prod_creds]) {
            testing::widen(p).unwrap();
        }
        testing::widen_for_good(ctx.age_key_path()).unwrap();
        let before: Vec<_> = wide
            .iter()
            .chain([&prod_creds, &ctx.age_key_path().to_path_buf()])
            .map(|p| testing::of_path(p).unwrap())
            .collect();
        let r = doctor(&ctx, named("prod"));
        let creds = group(&r, GroupId::Target)[1].clone();
        assert_eq!(creds.id, CheckId::CredentialsFile);
        assert_eq!(creds.status, CheckStatus::Warn);
        assert_eq!(
            creds.title,
            "Credentials file not restricted to your account"
        );
        assert_eq!(
            creds.detail.as_deref(),
            Some(prod_creds.display().to_string().as_str())
        );
        assert_eq!(creds.fix, Some(explain(RESTRICT_ON_READ_FIX)));
        let rows: Vec<Check> = group(&r, GroupId::ThisComputer)
            .into_iter()
            .filter(|c| c.title.contains("restricted"))
            .collect();
        assert_eq!(
            rows.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            [
                "Credentials file of target `dev` not restricted to your account",
                "State file of target `prod` not restricted to your account",
                "Age key not restricted to your account",
            ]
        );
        assert!(rows
            .iter()
            .all(|c| c.id == CheckId::Secrets && c.status == CheckStatus::Warn));
        assert_eq!(rows[0].fix, Some(explain(RESTRICT_ON_READ_FIX)));
        assert_eq!(rows[2].fix, Some(explain(RESTRICT_REFUSED_FIX)));
        assert_eq!(
            rows[2].detail.as_deref(),
            Some(ctx.age_key_path().display().to_string().as_str())
        );
        let after: Vec<_> = wide
            .iter()
            .chain([&prod_creds, &ctx.age_key_path().to_path_buf()])
            .map(|p| testing::of_path(p).unwrap())
            .collect();
        assert_eq!(after, before, "doctor changes no DACL");
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

    /// WI-453 follow-up: a provider that answered is not unreachable. A 429 is its own fix (wait,
    /// then try again), an answer that does not parse a request failure with the parse error as
    /// the detail; both were `ProviderUnreachable`.
    #[test]
    fn a_rate_limit_and_an_unreadable_answer_fail_with_their_own_fix() {
        let token_row = |status: usize, body: &str| {
            let mut server = mockito::Server::new();
            server
                .mock("GET", "/v1/locations")
                .with_status(status)
                .with_header("content-type", "application/json")
                .with_body(body)
                .create();
            let f = fx();
            let ctx = ctx(&f, &server.url());
            add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
            let t = group(&doctor(&ctx, named("prod")), GroupId::Target);
            t.into_iter()
                .find(|c| c.id == CheckId::TokenVerified)
                .unwrap()
        };
        let limited = token_row(
            429,
            r#"{"error":{"code":"rate_limit_exceeded","message":"slow down"}}"#,
        );
        assert_eq!(limited.status, CheckStatus::Fail);
        assert_eq!(limited.detail.as_deref(), Some("HTTP 429: slow down"));
        assert_eq!(limited.fix, Some(CheckFix::ProviderRateLimited));
        let unreadable = token_row(200, r#"{"nope":1}"#);
        assert_eq!(unreadable.status, CheckStatus::Fail);
        assert_eq!(unreadable.fix, Some(CheckFix::ProviderRequestFailed));
        let detail = unreadable.detail.as_deref().unwrap();
        assert!(
            detail.starts_with(
                "the hetzner-cloud API request GET /v1/locations failed: parse list_locations \
                 response: "
            ),
            "{detail}"
        );
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
        // WI-453: the detail named the URL twice ("talking to X: X: Connection Failed").
        let detail = v.detail.as_deref().unwrap();
        let url = format!("{OFFLINE}/v1/locations");
        assert!(
            detail.starts_with(&format!(
                "transport error talking to {url}: Connection Failed"
            )),
            "{detail}"
        );
        assert_eq!(detail.matches(&url).count(), 1, "{detail}");
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
        // The target's name with the path: the fix that changes the key needs both.
        assert_eq!(
            gone[5].fix,
            Some(CheckFix::SshKeyMissing {
                target: "gone".into(),
                path: f.root.join("home/nope.pub").display().to_string(),
            })
        );
    }

    /// GOTCHA-149: the row passed for any readable file, printing a private key's `-----BEGIN`
    /// as its type; `apply` now refuses to send such a file, so the row fails as `apply` would,
    /// a private key by name, and passes only a public key.
    #[test]
    fn the_ssh_key_row_fails_a_private_key_and_anything_but_a_public_key() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        let cases = [
            ("private", "-----BEGIN OPENSSH PRIVATE KEY-----\nb3Blbg==\n-----END OPENSSH PRIVATE KEY-----\n", Some(true)),
            ("junk", "not-a-key\n", Some(false)),
            ("good", "ssh-ed25519 AAAA me@host\n", None),
        ];
        for (name, body, private_key) in cases {
            let key = f.root.join(format!("keys/{name}"));
            std::fs::create_dir_all(key.parent().unwrap()).unwrap();
            std::fs::write(&key, body).unwrap();
            add(&ctx, name, "hetzner-cloud", Some(TOKEN), Some(key.clone()));
            let rows = group(&doctor(&ctx, named(name)), GroupId::Target);
            let shown = key.display().to_string();
            match private_key {
                Some(private_key) => {
                    assert_eq!(rows[5].status, CheckStatus::Fail, "{name}");
                    assert_eq!(rows[5].detail.as_deref(), Some(shown.as_str()));
                    assert_eq!(
                        rows[5].fix,
                        Some(CheckFix::SshKeyNotPublic {
                            target: name.into(),
                            path: shown,
                            private_key,
                        })
                    );
                }
                None => {
                    assert_eq!(rows[5].status, CheckStatus::Pass);
                    assert_eq!(
                        rows[5].detail.as_deref(),
                        Some(format!("{shown} (ssh-ed25519)").as_str())
                    );
                }
            }
        }
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
        // D.4a §3.13 (d4a-f): the lock's rows follow DNS, starting with `secrets`.
        let dns = env
            .iter()
            .position(|c| c.id == CheckId::Dns)
            .expect("a DNS row");
        assert_eq!(env[dns + 1].id, CheckId::Secrets, "{env:?}");
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
        let dns = group(&r, GroupId::ThisComputer)
            .into_iter()
            .find(|c| c.id == CheckId::Dns)
            .expect("a DNS row");
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
        let dns = group(&r, GroupId::ThisComputer)
            .into_iter()
            .find(|c| c.id == CheckId::Dns)
            .expect("a DNS row");
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
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        let stages = |target: DoctorTarget| -> Vec<(u32, u32, String)> {
            let reporter = CollectReporter::new();
            run(
                &ctx,
                DoctorArgs {
                    target,
                    window: UnlockWindow::NoTerminal,
                    client: quiet(),
                },
                &reporter,
                &CancellationToken::new(),
            )
            .unwrap();
            reporter
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
                .collect()
        };
        assert_eq!(
            stages(DoctorTarget::CliDefault),
            [
                (1, 2, "Target".to_string()),
                (2, 2, "This computer".to_string())
            ]
        );
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        assert_eq!(
            stages(named("prod")),
            [
                (1, 3, "Target".to_string()),
                (2, 3, "Cluster".to_string()),
                (3, 3, "This computer".to_string())
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
                window: UnlockWindow::NoTerminal,
                client: quiet(),
            },
            &NullReporter,
            &cancel,
        );
        assert!(matches!(r, Err(CoreError::Cancelled)), "{r:?}");
    }

    /// A run with no target whose DNS row resolves through `lookup`, with a 10 s request
    /// timeout: what the run returned, and how long it took.
    fn run_with_lookup(
        lookup: impl Fn(&str) -> std::io::Result<Vec<std::net::SocketAddr>> + Send + Sync + 'static,
        cancel: &CancellationToken,
    ) -> (CoreResult<DoctorReport>, Duration) {
        let f = fx();
        let ctx = ctx(&f, "https://doctor-dns.invalid")
            .with_no_ping(true)
            .with_request_timeout(Duration::from_secs(10));
        let _stub = net::stub_lookup(lookup);
        let started = std::time::Instant::now();
        let r = run(
            &ctx,
            DoctorArgs {
                target: DoctorTarget::CliDefault,
                window: UnlockWindow::NoTerminal,
                client: quiet(),
            },
            &NullReporter,
            cancel,
        );
        (r, started.elapsed())
    }

    /// Review findings 2, 5 and 7: a Ctrl-C while the DNS row waits on a resolver that never
    /// answers ends the run at once with `Cancelled` (exit 130, no report), not with a report at
    /// the lookup's deadline.
    #[test]
    fn a_cancel_during_the_dns_lookup_cancels_the_run_at_once() {
        let cancel = CancellationToken::new();
        let trip = cancel.clone();
        let (r, took) = run_with_lookup(
            move |_| {
                trip.cancel();
                std::thread::sleep(Duration::from_secs(10));
                Ok(vec![])
            },
            &cancel,
        );
        assert!(matches!(r, Err(CoreError::Cancelled)), "{r:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
    }

    /// The same, for a cancel that lands while the last row is finishing: its answer still
    /// arrives, and the run is cancelled all the same, never a report.
    #[test]
    fn a_cancel_that_lands_as_the_last_row_finishes_still_cancels_the_run() {
        let cancel = CancellationToken::new();
        let trip = cancel.clone();
        let (r, _) = run_with_lookup(
            move |_| {
                trip.cancel();
                Ok(vec!["127.0.0.1:443".parse().unwrap()])
            },
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

    // ---- the Cluster group (Task 8) ----

    const YAML: &str = "apiVersion: v1\nkind: Config\nclusters: []\n";

    fn seed_state(ctx: &Context, name: &str, server: serde_json::Value) {
        let hetzner: HetznerCloudState = serde_json::from_value(server).unwrap();
        State {
            hetzner_cloud: Some(hetzner),
            ..Default::default()
        }
        .save(&StatePaths::for_active_target(&ctx.store(), name))
        .unwrap();
    }

    fn cluster(r: &DoctorReport) -> Vec<Check> {
        group(r, GroupId::Cluster)
    }

    /// Creates the age key at the context's path (a test fixture; doctor never does).
    fn encrypt(ctx: &Context) -> String {
        let id = cli_core::secrets::load_or_create_identity(ctx.age_key_path()).unwrap();
        cli_core::secrets::encrypt_for_recipient(YAML, &id.to_public()).unwrap()
    }

    #[test]
    fn the_cluster_group_runs_only_for_an_existing_target() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        let missing = doctor(&ctx, named("ghost"));
        assert!(missing.groups.iter().all(|g| g.id != GroupId::Cluster));
        let none = doctor(&ctx, DoctorTarget::CliDefault);
        assert!(none.groups.iter().all(|g| g.id != GroupId::Cluster));
        let found = doctor(&ctx, named("prod"));
        assert_eq!(
            found.groups.iter().map(|g| g.id).collect::<Vec<_>>(),
            [GroupId::Target, GroupId::Cluster, GroupId::ThisComputer]
        );
        assert_eq!(
            ids(&cluster(&found)),
            [
                CheckId::KubeconfigCached,
                CheckId::KubeApiReachable,
                CheckId::NodeSshReachable
            ]
        );
    }

    #[test]
    fn an_unprovisioned_target_skips_all_three() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        let c = cluster(&doctor(&ctx, named("prod")));
        assert!(
            c.iter().all(|c| c.status == CheckStatus::Skipped
                && c.detail.as_deref() == Some("no provisioned server")),
            "{c:?}"
        );
        assert_eq!(
            c.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            [
                "Kubeconfig cached",
                "Kube API reachable",
                "Node reachable over SSH"
            ]
        );
    }

    #[test]
    fn an_unreadable_state_fails_the_first_row_and_skips_the_rest() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        let file = StatePaths::for_active_target(&ctx.store(), "prod").state_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{ not json").unwrap();
        let c = cluster(&doctor(&ctx, named("prod")));
        assert_eq!(c[0].status, CheckStatus::Fail);
        assert_eq!(
            c[0].detail.as_deref(),
            Some(file.display().to_string().as_str())
        );
        assert!(
            matches!(c[0].fix, Some(CheckFix::Explain { .. })),
            "{:?}",
            c[0]
        );
        for row in &c[1..] {
            assert_eq!(
                (row.status, row.detail.as_deref()),
                (CheckStatus::Skipped, Some("state unreadable"))
            );
        }
    }

    // ---- WI-475: a store another account can change ----

    /// This computer's rows that name who else can change the store, in order.
    fn store_rows(r: &DoctorReport) -> Vec<Check> {
        group(r, GroupId::ThisComputer)
            .into_iter()
            .filter(|c| c.title.contains("another account") || c.title.contains("other accounts"))
            .collect()
    }

    /// A store with targets `dev` and `prod`, each with a state file, and the age key.
    fn two_targets(f: &Fx) -> Context {
        let ctx = ctx(f, OFFLINE).with_no_ping(true);
        for t in ["dev", "prod"] {
            add(&ctx, t, "hetzner-cloud", Some(TOKEN), None);
            State::default()
                .save(&StatePaths::for_active_target(&ctx.store(), t))
                .unwrap();
        }
        cli_core::secrets::load_or_create_identity(ctx.age_key_path()).unwrap();
        ctx
    }

    /// WI-475: a store that is this account's alone has no such row (the goldens' stores).
    #[test]
    fn a_store_of_this_account_alone_has_no_owner_or_write_row() {
        let f = fx();
        let ctx = two_targets(&f);
        let r = doctor(&ctx, named("prod"));
        assert!(store_rows(&r).is_empty(), "{:?}", store_rows(&r));
        assert_eq!(
            group(&r, GroupId::Target)[3].status,
            CheckStatus::Pass,
            "the token is read"
        );
    }

    /// WI-475: each folder of the store and each secret file another account owns (pretended,
    /// in this test's own temp dir) is a warning naming it, the folders first; doctor reads no
    /// token from such a store — the token rows are skipped with the refusal — and changes
    /// nothing.
    #[test]
    fn what_another_account_owns_is_a_warning_each_and_no_token_is_read() {
        let f = fx();
        let ctx = two_targets(&f);
        let store = ctx.store();
        let foreign = [
            ctx.config_root().to_path_buf(),
            store.target_dir("prod"),
            store.target_credentials_file("dev"),
            StatePaths::for_active_target(&store, "prod").state_file(),
            ctx.age_key_path().to_path_buf(),
        ];
        let before = crate::target::testkit::snapshot(&f.root);
        let _guards: Vec<_> = foreign
            .iter()
            .map(|p| cli_core::owner_only::pretend::foreign_owner(p))
            .collect();
        let r = doctor(&ctx, named("prod"));
        let rows = store_rows(&r);
        assert_eq!(
            rows.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            [
                "Target store folder belongs to another account",
                "Target store folder `targets/prod` belongs to another account",
                "Credentials file of target `dev` belongs to another account",
                "State file of target `prod` belongs to another account",
                "Age key belongs to another account",
            ]
        );
        for (row, path) in rows.iter().zip(&foreign) {
            assert_eq!((row.id, row.status), (CheckId::Secrets, CheckStatus::Warn));
            assert_eq!(
                row.detail.as_deref(),
                Some(path.display().to_string().as_str())
            );
        }
        assert_eq!(rows[0].fix, Some(explain(FOREIGN_FOLDER_FIX)));
        assert_eq!(rows[2].fix, Some(explain(FOREIGN_FILE_FIX)));
        let token = &group(&r, GroupId::Target)[3];
        assert_eq!(
            (token.id, token.status),
            (CheckId::TokenFormat, CheckStatus::Skipped)
        );
        let refusal = CliError::StoreNotPrivate {
            path: ctx.config_root().to_path_buf(),
            problem: cli_core::StoreFolderProblem::ForeignOwner,
        };
        assert_eq!(token.detail, Some(refusal.to_string()));
        assert_eq!(crate::target::testkit::snapshot(&f.root), before);
    }

    /// WI-481 (Unix, d4a-h P-3): a folder another account owns (pretended) and this account
    /// cannot open (mode 000, as such a folder made 0700 is to it) is reported, never an error
    /// and never a bare "Permission denied": the target's config row is the refusal naming the
    /// folder, and the folder rows name it — with a row for the folders in it that could not be
    /// listed, where doctor used to show one row about the store's root instead of them all.
    #[cfg(unix)]
    #[test]
    fn a_folder_another_account_owns_and_keeps_closed_is_named_in_its_rows() {
        let f = fx();
        let ctx = two_targets(&f);
        let store = ctx.store();
        let folder_rows = |r: &DoctorReport| -> Vec<String> {
            group(r, GroupId::ThisComputer)
                .into_iter()
                .filter(|c| c.id == CheckId::Secrets && c.title.starts_with("Target store folder"))
                .map(|c| c.title)
                .collect()
        };
        let refusal = |path: &Path| {
            CliError::StoreNotPrivate {
                path: path.to_path_buf(),
                problem: cli_core::StoreFolderProblem::ForeignOwner,
            }
            .to_string()
        };
        let config_row = |r: &DoctorReport| {
            let rows = group(r, GroupId::Target);
            let row = rows[0].clone();
            assert_eq!(
                (row.id, row.status),
                (CheckId::ConfigReadable, CheckStatus::Fail)
            );
            assert_eq!(
                row.detail.as_deref(),
                Some(
                    store
                        .target_config_file("prod")
                        .display()
                        .to_string()
                        .as_str()
                )
            );
            (rows.len(), row.fix)
        };
        // The report of a doctor run while `path` is closed to this account (mode 000), the
        // folder's mode put back before anything is asserted.
        let closed = |path: &Path| {
            let _closed = cli_core::owner_only::testing::ModeGuard::closed(path);
            doctor(&ctx, named("prod"))
        };

        // A target's folder: its config row and its folder row name it.
        let prod = store.target_dir("prod");
        {
            let _foreign = cli_core::owner_only::pretend::foreign_owner(&prod);
            let r = closed(&prod);
            assert_eq!(
                config_row(&r),
                (
                    1,
                    Some(CheckFix::Explain {
                        text: refusal(&prod)
                    })
                )
            );
            assert_eq!(
                folder_rows(&r),
                ["Target store folder `targets/prod` belongs to another account"]
            );
        }

        // `targets` itself: the target list cannot be read; the refusal is the target's row.
        let targets = store.targets_dir();
        {
            let _foreign = cli_core::owner_only::pretend::foreign_owner(&targets);
            let r = closed(&targets);
            assert_eq!(
                config_row(&r),
                (
                    1,
                    Some(CheckFix::Explain {
                        text: refusal(&targets)
                    })
                )
            );
            assert!(!r.groups.iter().any(|g| g.id == GroupId::Cluster));
            assert_eq!(
                folder_rows(&r),
                [
                    "Target store folder `targets` belongs to another account",
                    "Target store folder `targets`: who can change the folders in it is unknown",
                ]
            );
        }

        // A target's state folder, which the walk lists: named, and the walk goes on past it.
        let state = store.state_dir("dev");
        let _foreign = cli_core::owner_only::pretend::foreign_owner(&state);
        let r = closed(&state);
        assert_eq!(
            folder_rows(&r),
            [
                "Target store folder `state/dev` belongs to another account",
                "Target store folder `state/dev`: who can change the folders in it is unknown",
            ]
        );
        let unlisted = group(&r, GroupId::ThisComputer)
            .into_iter()
            .find(|c| {
                c.title
                    .ends_with("who can change the folders in it is unknown")
            })
            .unwrap();
        assert_eq!(
            unlisted.detail.as_deref(),
            Some(state.display().to_string().as_str())
        );
    }

    /// WI-475 (Unix): each folder others can write is a warning with its mode — the group's
    /// write bit when the group is not this account's own — and doctor leaves every mode as
    /// it was.
    #[cfg(unix)]
    #[test]
    fn a_folder_others_can_write_is_a_warning_with_its_mode_and_is_left_as_it_was() {
        use std::os::unix::fs::PermissionsExt;
        let f = fx();
        let ctx = two_targets(&f);
        let store = ctx.store();
        let apprafter = StatePaths::for_active_target(&store, "prod").state_dir();
        let wide = [
            (ctx.config_root().to_path_buf(), 0o777, ""),
            (store.target_dir("dev"), 0o775, " `targets/dev`"),
            (apprafter.clone(), 0o1757, " `state/prod/.apprafter`"),
        ];
        for (path, mode, _) in &wide {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(*mode)).unwrap();
        }
        let _shared = cli_core::owner_only::pretend::group_alone(false);
        let rows = store_rows(&doctor(&ctx, named("prod")));
        let expected: Vec<String> = wide
            .iter()
            .map(|(_, mode, what)| {
                format!(
                    "Target store folder{what} can be changed by other accounts (mode {mode:o})"
                )
            })
            .collect();
        assert_eq!(
            rows.iter().map(|c| c.title.clone()).collect::<Vec<_>>(),
            expected
        );
        for (row, (path, mode, _)) in rows.iter().zip(&wide) {
            assert_eq!((row.id, row.status), (CheckId::Secrets, CheckStatus::Warn));
            assert_eq!(
                row.detail.as_deref(),
                Some(path.display().to_string().as_str())
            );
            assert_eq!(row.fix, Some(explain(WRITABLE_FOLDER_FIX)));
            let now = std::fs::metadata(path).unwrap().permissions().mode() & 0o7777;
            assert_eq!(now, *mode, "doctor changes nothing: {path:?}");
        }
        drop(_shared);
        let _alone = cli_core::owner_only::pretend::group_alone(true);
        let titles: Vec<String> = store_rows(&doctor(&ctx, named("prod")))
            .into_iter()
            .map(|c| c.title)
            .collect();
        assert_eq!(titles, [expected[0].clone(), expected[2].clone()]);
        for (path, _, _) in &wide {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// WI-475 (Windows): a folder whose DACL lets another account write is a warning, with no
    /// mode.
    #[cfg(windows)]
    #[test]
    fn a_folder_others_can_write_is_a_warning_and_is_left_as_it_was() {
        use cli_core::owner_only::testing;
        let f = fx();
        let ctx = two_targets(&f);
        let me = testing::this_user_sid().unwrap();
        let folder = ctx.store().target_dir("dev");
        testing::set_dacl(
            &folder,
            &format!("D:P(A;OICI;FA;;;{me})(A;OICI;0x1301bf;;;BU)"),
        )
        .unwrap();
        let before = testing::of_path(&folder).unwrap();
        let rows = store_rows(&doctor(&ctx, named("prod")));
        assert_eq!(
            rows.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            ["Target store folder `targets/dev` can be changed by other accounts"]
        );
        assert_eq!(
            rows[0].detail.as_deref(),
            Some(folder.display().to_string().as_str())
        );
        assert_eq!(rows[0].fix, Some(explain(WRITABLE_FOLDER_FIX)));
        assert_eq!(
            testing::of_path(&folder).unwrap(),
            before,
            "doctor changes nothing"
        );
        testing::set_dacl(&folder, &format!("D:P(A;OICI;FA;;;{me})")).unwrap();
    }

    /// WI-475: doctor never runs a kubeconfig another account could have put in the store —
    /// encrypted or in clear — nor pings with its token: those rows are skipped with the
    /// refusal, and nothing is decrypted to disk.
    #[cfg(unix)]
    #[test]
    fn a_kubeconfig_in_a_store_others_can_change_is_never_run() {
        let f = fx();
        let ran = f.root.join("kubectl-ran");
        crate::kube::tests::fake_kubectl(
            &f.root.join("bin"),
            &format!(
                "case \"$1\" in version) echo 'Client Version: v1.31.0'; exit 0;; esac\n\
                 touch {}\nprintf '%s' '{{\"gitVersion\":\"v1\"}}'",
                ran.display()
            ),
        );
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        for name in ["sealed", "clear"] {
            add(&ctx, name, "hetzner-cloud", Some(TOKEN), None);
        }
        let armored = encrypt(&ctx);
        seed_state(
            &ctx,
            "sealed",
            serde_json::json!({"server_id": 1, "server_name": "a", "kubeconfig_age": armored}),
        );
        seed_state(
            &ctx,
            "clear",
            serde_json::json!({"server_id": 2, "server_name": "b", "kubeconfig_yaml": YAML}),
        );
        let _foreign = cli_core::owner_only::pretend::foreign_owner(&ctx.store().targets_dir());
        let refusal = CliError::StoreNotPrivate {
            path: ctx.store().targets_dir(),
            problem: cli_core::StoreFolderProblem::ForeignOwner,
        }
        .to_string();
        for name in ["sealed", "clear"] {
            let c = cluster(&doctor(&ctx, named(name)));
            assert_eq!(c[1].id, CheckId::KubeApiReachable);
            assert_eq!(
                (c[1].status, c[1].detail.as_deref()),
                (CheckStatus::Skipped, Some(refusal.as_str())),
                "{name}"
            );
        }
        assert!(!ran.exists(), "kubectl ran with the store's kubeconfig");
        assert!(
            std::fs::read_dir(ctx.runtime_dir()).map_or(true, |mut d| d.next().is_none()),
            "nothing is decrypted to disk"
        );
    }

    #[test]
    fn a_server_without_a_cached_kubeconfig_fails_with_a_fetch_fix() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        seed_state(
            &ctx,
            "prod",
            serde_json::json!({"server_id": 7, "server_name": "apprafter-prod"}),
        );
        let c = cluster(&doctor(&ctx, named("prod")));
        assert_eq!(c[0].status, CheckStatus::Fail);
        assert_eq!(
            c[0].detail.as_deref(),
            Some("none cached for server `apprafter-prod` (id 7)")
        );
        assert_eq!(
            c[0].fix,
            Some(CheckFix::FetchKubeconfig {
                target: "prod".into()
            })
        );
        assert_eq!(
            (c[1].status, c[1].detail.as_deref()),
            (CheckStatus::Skipped, Some("no cached kubeconfig"))
        );
        assert_eq!(c[2].status, CheckStatus::Skipped);
        assert_eq!(c[2].detail.as_deref(), Some("not requested"));
    }

    #[test]
    fn plaintext_copies_warn_and_the_encrypted_one_passes() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        for name in ["legacy", "both", "sealed"] {
            add(&ctx, name, "hetzner-cloud", Some(TOKEN), None);
        }
        let armored = encrypt(&ctx);
        seed_state(
            &ctx,
            "legacy",
            serde_json::json!({"server_id": 1, "server_name": "a", "kubeconfig_yaml": YAML}),
        );
        seed_state(
            &ctx,
            "both",
            serde_json::json!({"server_id": 2, "server_name": "b", "kubeconfig_yaml": YAML, "kubeconfig_age": armored}),
        );
        seed_state(
            &ctx,
            "sealed",
            serde_json::json!({"server_id": 3, "server_name": "c", "kubeconfig_age": armored}),
        );
        let first = |n: &str| cluster(&doctor(&ctx, named(n)))[0].clone();
        let legacy = first("legacy");
        assert_eq!(
            (legacy.status, legacy.detail.as_deref()),
            (CheckStatus::Warn, Some("stored unencrypted (legacy)"))
        );
        assert_eq!(
            legacy.fix,
            Some(CheckFix::FetchKubeconfig {
                target: "legacy".into()
            })
        );
        let both = first("both");
        assert_eq!(
            (both.status, both.detail.as_deref()),
            (
                CheckStatus::Warn,
                Some("encrypted, and an unencrypted legacy copy remains")
            )
        );
        let sealed = first("sealed");
        assert_eq!(
            (sealed.status, sealed.detail.as_deref(), sealed.fix),
            (CheckStatus::Pass, Some("encrypted"), None)
        );
    }

    #[test]
    fn a_missing_age_key_fails_and_is_never_created() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        seed_state(
            &ctx,
            "prod",
            serde_json::json!({"server_id": 7, "server_name": "n",
                "kubeconfig_age": "-----BEGIN AGE ENCRYPTED FILE-----\n-----END AGE ENCRYPTED FILE-----\n"}),
        );
        let c = cluster(&doctor(&ctx, named("prod")));
        assert_eq!(c[1].status, CheckStatus::Fail);
        assert_eq!(
            c[1].fix,
            Some(CheckFix::AgeKeyMissing {
                target: "prod".into(),
                path: ctx.age_key_path().display().to_string(),
                sealed_to_lost_vault: false,
            })
        );
        assert!(
            !ctx.age_key_path().exists(),
            "a Read never writes a key (decision 2)"
        );
    }

    /// F13: a cache the state records as sealed to a target store vault's key — `lock on`
    /// re-sealed it, and the vault was moved aside since (`#locked-store-lost`) — says so, so
    /// the fix does not send the person after a shared key that never held it.
    #[test]
    fn a_missing_age_key_beside_a_cache_sealed_to_a_lost_vault_says_so() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        seed_state(
            &ctx,
            "prod",
            serde_json::json!({"server_id": 7, "server_name": "n",
                "kubeconfig_age": "-----BEGIN AGE ENCRYPTED FILE-----\n-----END AGE ENCRYPTED FILE-----\n",
                "kubeconfig_sealed_to": {"kind": "store_vault", "store_id": "3f9c0a1b7d2e4c56"}}),
        );
        let c = cluster(&doctor(&ctx, named("prod")));
        assert_eq!(c[1].status, CheckStatus::Fail);
        assert_eq!(
            c[1].fix,
            Some(CheckFix::AgeKeyMissing {
                target: "prod".into(),
                path: ctx.age_key_path().display().to_string(),
                sealed_to_lost_vault: true,
            })
        );
    }

    /// WI-457 review #8: a cache the key there does not open (cached under a lost key that a
    /// new one replaced) fails with its own fix, naming the target whose kubeconfig to fetch
    /// again under this key, instead of the age library's raw text; nothing is left behind.
    #[cfg(unix)]
    #[test]
    fn a_cache_under_another_key_fails_with_the_target_to_fetch_again() {
        let f = fx();
        crate::kube::tests::fake_kubectl(
            &f.root.join("bin"),
            "case \"$1\" in version) echo 'Client Version: v1.31.0'; exit 0;; esac\nexit 1",
        );
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        let other = cli_core::secrets::load_or_create_identity(&f.root.join("other.key")).unwrap();
        let armored = cli_core::secrets::encrypt_for_recipient(YAML, &other.to_public()).unwrap();
        cli_core::secrets::load_or_create_identity(ctx.age_key_path()).unwrap();
        seed_state(
            &ctx,
            "prod",
            serde_json::json!({"server_id": 7, "server_name": "n", "kubeconfig_age": armored}),
        );
        let c = cluster(&doctor(&ctx, named("prod")));
        assert_eq!(c[1].id, CheckId::KubeApiReachable);
        assert_eq!(c[1].status, CheckStatus::Fail);
        assert_eq!(c[1].detail, None);
        assert_eq!(
            c[1].fix,
            Some(CheckFix::CacheUndecryptable {
                target: "prod".into(),
                path: ctx.age_key_path().display().to_string(),
                key: CacheKeyView::SharedFile,
            })
        );
        assert!(
            std::fs::read_dir(ctx.runtime_dir()).map_or(true, |mut d| d.next().is_none()),
            "nothing is decrypted to disk"
        );
    }

    #[test]
    fn kubectl_missing_skips_the_api_check_and_writes_no_kubeconfig() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true); // `bin` is empty
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        seed_state(
            &ctx,
            "prod",
            serde_json::json!({"server_id": 7, "server_name": "n", "kubeconfig_yaml": YAML}),
        );
        let c = cluster(&doctor(&ctx, named("prod")));
        assert_eq!(c[1].status, CheckStatus::Skipped);
        assert_eq!(c[1].detail.as_deref(), Some("`kubectl` not found"));
        assert_eq!(
            c[1].fix,
            Some(CheckFix::InstallTool {
                tool: ToolId::Kubectl
            })
        );
        assert!(
            !ctx.runtime_dir().exists(),
            "nothing is decrypted to disk when it cannot be used"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_api_check_passes_through_kubectl_and_leaves_no_file() {
        let f = fx();
        crate::kube::tests::fake_kubectl(
            &f.root.join("bin"),
            "case \"$1\" in version) echo 'Client Version: v1.31.0'; exit 0;; esac\n\
             printf '%s' '{\"gitVersion\":\"v1.31.0+k3s1\"}'",
        );
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        let armored = encrypt(&ctx);
        seed_state(
            &ctx,
            "prod",
            serde_json::json!({"server_id": 7, "server_name": "n", "kubeconfig_age": armored}),
        );
        let c = cluster(&doctor(&ctx, named("prod")));
        assert_eq!(c[1].status, CheckStatus::Pass, "{:?}", c[1]);
        let d = c[1].detail.as_deref().unwrap();
        assert!(
            d.starts_with("v1.31.0+k3s1 · ") && d.ends_with(" ms"),
            "{d}"
        );
        let left: Vec<_> = std::fs::read_dir(ctx.runtime_dir()).unwrap().collect();
        assert!(
            left.is_empty(),
            "the decrypted copy is removed after the probe: {left:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unreachable_api_fails_with_its_kind() {
        let f = fx();
        crate::kube::tests::fake_kubectl(
            &f.root.join("bin"),
            "echo 'Unable to connect to the server: dial tcp 127.0.0.1:6443: connect: connection refused' >&2\nexit 1",
        );
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        seed_state(
            &ctx,
            "prod",
            serde_json::json!({"server_id": 7, "server_name": "n", "kubeconfig_yaml": YAML}),
        );
        let c = cluster(&doctor(&ctx, named("prod")));
        assert_eq!(c[1].status, CheckStatus::Fail);
        assert_eq!(
            c[1].detail.as_deref(),
            Some("Unable to connect to the server: dial tcp 127.0.0.1:6443: connect: connection refused")
        );
        assert_eq!(
            c[1].fix,
            Some(CheckFix::ClusterUnreachable {
                reason: KubeErrorKind::Unreachable
            })
        );
    }

    fn server_mock(
        server: &mut mockito::Server,
        status: usize,
        ipv4: Option<&str>,
    ) -> mockito::Mock {
        let body = serde_json::json!({"server": {"id": 7, "name": "n", "status": "running", "labels": {},
            "public_net": {"ipv4": ipv4.map(|ip| serde_json::json!({"ip": ip})), "ipv6": null}}});
        server
            .mock("GET", "/v1/servers/7")
            .with_status(status)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create()
    }

    fn provisioned(f: &Fx, base: &str, token: Option<&str>) -> (Context, TargetRef) {
        let ctx = ctx(f, base);
        add(&ctx, "prod", "hetzner-cloud", token, None);
        seed_state(
            &ctx,
            "prod",
            serde_json::json!({"server_id": 7, "server_name": "n"}),
        );
        let target = TargetRef::named(&ctx, "prod").unwrap();
        (ctx, target)
    }

    #[test]
    fn the_node_check_connects_to_the_public_ipv4() {
        let mut server = mockito::Server::new();
        let _m = server_mock(&mut server, 200, Some("127.0.0.1"));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let f = fx();
        let (ctx, target) = provisioned(&f, &server.url(), Some(TOKEN));
        let c = node_ssh(&ctx, &target, &CancellationToken::new(), port).unwrap();
        assert_eq!(c.status, CheckStatus::Pass, "{c:?}");
        assert_eq!(c.detail, Some(format!("port {port} · 127.0.0.1")));
    }

    #[test]
    fn the_node_check_fails_when_nothing_listens() {
        let mut server = mockito::Server::new();
        let _m = server_mock(&mut server, 200, Some("127.0.0.1"));
        // Bound and dropped: the port is closed again.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let f = fx();
        let (ctx, target) = provisioned(&f, &server.url(), Some(TOKEN));
        let c = node_ssh(&ctx, &target, &CancellationToken::new(), port).unwrap();
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(
            c.detail
                .as_deref()
                .unwrap()
                .starts_with(&format!("port {port} · 127.0.0.1: ")),
            "{c:?}"
        );
        assert_eq!(
            c.fix,
            Some(CheckFix::NodeUnreachable {
                address: "127.0.0.1".into()
            })
        );
    }

    #[test]
    fn a_server_without_a_public_ipv4_skips_the_node_check() {
        let mut server = mockito::Server::new();
        let _m = server_mock(&mut server, 200, None);
        let f = fx();
        let (ctx, target) = provisioned(&f, &server.url(), Some(TOKEN));
        let c = node_ssh(&ctx, &target, &CancellationToken::new(), 22).unwrap();
        assert_eq!(
            (c.status, c.detail.as_deref()),
            (
                CheckStatus::Skipped,
                Some("the server has no public IPv4 address")
            )
        );
    }

    #[test]
    fn a_server_gone_at_the_provider_fails_and_no_token_skips() {
        let mut server = mockito::Server::new();
        let _m = server_mock(&mut server, 404, None);
        let f = fx();
        let (ctx, target) = provisioned(&f, &server.url(), Some(TOKEN));
        let gone = node_ssh(&ctx, &target, &CancellationToken::new(), 22).unwrap();
        assert_eq!(gone.status, CheckStatus::Fail);
        assert!(
            matches!(gone.fix, Some(CheckFix::Explain { .. })),
            "{gone:?}"
        );
        let f2 = fx();
        let (ctx2, target2) = provisioned(&f2, &server.url(), None);
        let skipped = node_ssh(&ctx2, &target2, &CancellationToken::new(), 22).unwrap();
        assert_eq!(
            (skipped.status, skipped.detail.as_deref()),
            (CheckStatus::Skipped, Some("no token stored"))
        );
    }

    #[test]
    fn a_rejected_token_fails_the_node_check_with_a_renew_fix() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/v1/servers/7")
            .with_status(401)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":{"code":"unauthorized","message":"unable to authenticate"}}"#)
            .create();
        let f = fx();
        let (ctx, target) = provisioned(&f, &server.url(), Some(TOKEN));
        let c = node_ssh(&ctx, &target, &CancellationToken::new(), 22).unwrap();
        assert_eq!(c.status, CheckStatus::Fail);
        assert_eq!(
            c.fix,
            Some(CheckFix::RenewToken {
                target: "prod".into(),
                why: RenewWhy::TokenRejected
            })
        );
    }

    /// WI-453 follow-up: the node row tells a provider that answered from one that did not, as
    /// the token row does — an answer that does not parse is a request failure (it was
    /// `ProviderUnreachable`), a 429 is the rate limit, another status the provider's error.
    #[test]
    fn an_answer_fails_the_node_check_with_its_own_fix() {
        let node_row = |status: usize, body: &str| {
            let mut server = mockito::Server::new();
            server
                .mock("GET", "/v1/servers/7")
                .with_status(status)
                .with_header("content-type", "application/json")
                .with_body(body)
                .create();
            let f = fx();
            let (ctx, target) = provisioned(&f, &server.url(), Some(TOKEN));
            node_ssh(&ctx, &target, &CancellationToken::new(), 22).unwrap()
        };
        let unreadable = node_row(200, r#"{"nope":1}"#);
        assert_eq!(unreadable.status, CheckStatus::Fail);
        assert_eq!(
            unreadable.fix,
            Some(CheckFix::ProviderRequestFailed),
            "{unreadable:?}"
        );
        let detail = unreadable.detail.as_deref().unwrap();
        assert!(
            detail.starts_with(
                "the hetzner-cloud API request GET /v1/servers/7 failed: parse get_server \
                 response: "
            ),
            "{detail}"
        );
        let limited = node_row(
            429,
            r#"{"error":{"code":"rate_limit_exceeded","message":"slow down"}}"#,
        );
        assert_eq!(
            limited.fix,
            Some(CheckFix::ProviderRateLimited),
            "{limited:?}"
        );
        let outage = node_row(
            503,
            r#"{"error":{"code":"unavailable","message":"maintenance"}}"#,
        );
        assert_eq!(
            outage.fix,
            Some(CheckFix::ProviderError { status: 503 }),
            "{outage:?}"
        );
    }

    /// WI-453: a dead API fails the node row as it fails the token row — `ProviderUnreachable`,
    /// the transport failure as the detail — not as a request failure with no cause.
    #[test]
    fn a_dead_api_fails_the_node_check_as_unreachable() {
        let f = fx();
        let (ctx, target) = provisioned(&f, OFFLINE, Some(TOKEN));
        let c = node_ssh(&ctx, &target, &CancellationToken::new(), 22).unwrap();
        assert_eq!(c.status, CheckStatus::Fail);
        assert_eq!(c.fix, Some(CheckFix::ProviderUnreachable), "{c:?}");
        let detail = c.detail.as_deref().unwrap();
        let url = format!("{OFFLINE}/v1/servers/7");
        assert!(
            detail.starts_with(&format!(
                "transport error talking to {url}: Connection Failed"
            )),
            "{detail}"
        );
        assert_eq!(detail.matches(&url).count(), 1, "{detail}");
    }

    #[test]
    fn no_ping_skips_the_node_check_without_asking_the_provider() {
        let mut server = mockito::Server::new();
        let m = server.mock("GET", mockito::Matcher::Any).expect(0).create();
        let f = fx();
        let (ctx, target) = provisioned(&f, &server.url(), Some(TOKEN));
        let c = node_ssh(
            &ctx.with_no_ping(true),
            &target,
            &CancellationToken::new(),
            22,
        )
        .unwrap();
        assert_eq!(
            (c.status, c.detail.as_deref()),
            (CheckStatus::Skipped, Some("not requested"))
        );
        m.assert();
    }

    /// D.4a T44: doctor sweeps the copies a gone process left — by owner, never by age: a long
    /// session's copy stays however old it is.
    #[test]
    fn stale_runtime_copies_are_swept_at_the_start_of_a_run() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        std::fs::create_dir_all(ctx.runtime_dir()).unwrap();
        let gone = ctx.runtime_dir().join(format!(
            "kubeconfig-{}-1-0.yaml",
            crate::process::owner_tests::NO_SUCH_PID
        ));
        let me = std::process::id();
        let start = crate::process::started(me).unwrap().raw();
        let live = ctx
            .runtime_dir()
            .join(format!("kubeconfig-{me}-{start}-0.yaml"));
        std::fs::write(&gone, "x").unwrap();
        std::fs::write(&live, "x").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&live)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(7200))
            .unwrap();
        doctor(&ctx, DoctorTarget::CliDefault);
        assert!(!gone.exists() && live.exists());
    }

    /// d4a-h N6 (Unix): a store this account cannot read is reported, never a stop. With the
    /// store's root closed (mode 000), `doctor` and `doctor --target prod` each fail the
    /// `Config file readable` row in the store's words — the path, the OS error once, the
    /// `ls -ld` help (d4a-h N3) — and This computer's rows still run; the sweep of the runtime
    /// folder under the root warns with the OS error alone, never `io error:` (GOTCHA-314).
    /// With only `targets` closed and no CLI default, the target list is that row.
    #[cfg(unix)]
    #[test]
    fn a_store_this_account_cannot_read_is_a_row_and_doctor_goes_on() {
        let f = fx();
        let ctx = two_targets(&f);
        set_default(&ctx, "prod");
        std::fs::create_dir_all(ctx.runtime_dir()).unwrap();
        let store = ctx.store();
        // Every word of the report, and every warning, as one text each.
        let words = |r: &DoctorReport| -> Vec<String> {
            r.groups
                .iter()
                .flat_map(|g| &g.checks)
                .map(|c| format!("{} | {:?} | {:?}", c.title, c.detail, c.fix))
                .collect()
        };
        let warnings = |reporter: &CollectReporter| -> Vec<String> {
            reporter
                .take()
                .into_iter()
                .filter_map(|e| match e {
                    Event::Warning { message } => Some(message),
                    _ => None,
                })
                .collect()
        };
        // The one Target row: a failed `Config file readable` with `detail`, naming `path`.
        let config_row = |r: &DoctorReport, detail: &Path, path: &Path, denied: &str| {
            let rows = group(r, GroupId::Target);
            assert_eq!(rows.len(), 1, "{rows:?}");
            let row = &rows[0];
            assert_eq!(
                (row.id, row.status, row.title.as_str()),
                (CheckId::ConfigReadable, CheckStatus::Fail, TITLE_CONFIG)
            );
            assert_eq!(
                row.detail.as_deref(),
                Some(detail.display().to_string().as_str())
            );
            let Some(CheckFix::Explain { text }) = &row.fix else {
                panic!("{row:?}");
            };
            assert!(
                text.starts_with(&format!("cannot read {}: {denied}. ", path.display())),
                "{text}"
            );
            assert_eq!(text.matches(denied).count(), 1, "{text}");
            assert!(
                text.contains("`ls -ld`") && !text.contains("chmod 0600"),
                "{text}"
            );
        };
        let run_on = |target: DoctorTarget| {
            let reporter = CollectReporter::new();
            let r = run(
                &ctx,
                DoctorArgs {
                    target,
                    window: UnlockWindow::NoTerminal,
                    client: quiet(),
                },
                &reporter,
                &CancellationToken::new(),
            );
            (r, warnings(&reporter))
        };

        // The store's root closed: the CLI default and a named target, each read closed.
        let root = store.root().to_path_buf();
        let (default, named_prod, denied) = {
            let _closed = cli_core::owner_only::testing::ModeGuard::closed(&root);
            let denied = std::fs::read_dir(&root).unwrap_err().to_string();
            (
                run_on(DoctorTarget::CliDefault),
                run_on(named("prod")),
                denied,
            )
        };
        for (r, said) in [&default, &named_prod] {
            let r = r.as_ref().expect("doctor reports a store it cannot read");
            assert!(!r.groups.iter().any(|g| g.id == GroupId::Cluster));
            assert!(!group(r, GroupId::ThisComputer).is_empty());
            assert!(r.has_failures());
            assert_eq!(
                said,
                &[format!(
                    "cannot remove old kubeconfig copies from {}: {denied}",
                    ctx.runtime_dir().display()
                )]
            );
            let all = words(r);
            assert!(!all.iter().any(|w| w.contains("io error")), "{all:#?}");
        }
        let r = default.0.unwrap();
        assert_eq!(r.target, None);
        let config = store.global_config_file();
        config_row(&r, &config, &config, &denied);
        let r = named_prod.0.unwrap();
        assert_eq!(r.target.as_deref(), Some("prod"));
        config_row(
            &r,
            &store.target_config_file("prod"),
            &store.targets_dir(),
            &denied,
        );

        // Only `targets` closed, and no CLI default: the list that would name the targets is
        // the row.
        set_default(&ctx, "");
        let targets = store.targets_dir();
        let _closed = cli_core::owner_only::testing::ModeGuard::closed(&targets);
        let (r, _) = run_on(DoctorTarget::CliDefault);
        let r = r.expect("doctor reports a target list it cannot read");
        config_row(&r, &targets, &targets, &denied);
    }

    /// WI-483 (Unix): a target removed between doctor's lookup and its read of the target's
    /// files is the `Target … exists` row, offering what is left; and when the list that would
    /// offer it cannot be read by then either (d4a-h N6's double race), the target's `Config
    /// file readable` row in the store's words, never a stop. Both land exactly there through
    /// the read's seam (`target_group_loading`), on the real read and the real list: forced,
    /// never sampled.
    #[cfg(unix)]
    #[test]
    fn a_target_removed_between_lookup_and_read_is_a_row_and_an_unreadable_list_then_too() {
        let f = fx();
        let ctx = two_targets(&f);
        let store = ctx.store();
        let cancel = CancellationToken::new();
        let found = || Lookup::Found(TargetRef::named(&ctx, "prod").unwrap());
        // The first race: `prod` goes after the lookup found it; the real read then says so.
        let removed_then_read = |ctx: &Context, name: &str| {
            std::fs::remove_dir_all(store.target_dir(name)).unwrap();
            let read = load(ctx, name);
            assert!(
                matches!(read, Err(CliError::TargetNotFound { .. })),
                "the read of a removed target is TargetNotFound"
            );
            read
        };

        // The list still readable: what is left is offered.
        let checks = target_group_loading(&ctx, &found(), &cancel, removed_then_read)
            .expect("a removed target is a row");
        assert_eq!(
            checks,
            vec![missing_target("prod", vec!["dev".to_string()])]
        );

        // The second race: the target list closed after the read, before doctor lists it.
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        let targets = store.targets_dir();
        let mut closed = None;
        let checks = target_group_loading(&ctx, &found(), &cancel, |ctx, name| {
            let read = removed_then_read(ctx, name);
            closed = Some(cli_core::owner_only::testing::ModeGuard::closed(&targets));
            read
        })
        .expect("a list it cannot read is a row too");
        assert!(
            closed.is_some(),
            "the list was closed between the read and the listing"
        );
        let denied = std::fs::read_dir(&targets).unwrap_err().to_string();
        assert_eq!(checks.len(), 1, "{checks:?}");
        let row = &checks[0];
        assert_eq!(
            (row.id, row.status, row.title.as_str()),
            (CheckId::ConfigReadable, CheckStatus::Fail, TITLE_CONFIG)
        );
        assert_eq!(
            row.detail.as_deref(),
            Some(
                store
                    .target_config_file("prod")
                    .display()
                    .to_string()
                    .as_str()
            )
        );
        let Some(CheckFix::Explain { text }) = &row.fix else {
            panic!("{row:?}");
        };
        assert!(
            text.starts_with(&format!("cannot read {}: {denied}. ", targets.display())),
            "{text}"
        );
        assert!(text.contains("`ls -ld`"), "{text}");
    }

    /// D.4a T71: an Info row is a fact, in no total.
    #[test]
    fn an_info_row_is_a_fact_in_no_total() {
        let row = |status| Check {
            id: CheckId::Secrets,
            tool: None,
            status,
            title: "t".into(),
            detail: None,
            fix: Some(CheckFix::ProtectSecrets),
        };
        let report = DoctorReport {
            target: None,
            groups: vec![CheckGroup {
                id: GroupId::ThisComputer,
                checks: vec![
                    row(CheckStatus::Info),
                    row(CheckStatus::Pass),
                    row(CheckStatus::Warn),
                ],
            }],
        };
        assert_eq!(
            (
                report.passed(),
                report.warned(),
                report.failed(),
                report.skipped(),
                report.informed()
            ),
            (1, 1, 0, 0, 1)
        );
        assert!(!report.has_failures());
        assert_eq!(serde_json::to_value(CheckStatus::Info).unwrap(), "info");
        assert_eq!(
            serde_json::to_value(CheckFix::FinishLock {
                phase: SecretsMode::Unlocking
            })
            .unwrap(),
            serde_json::json!({"kind": "finish_lock", "phase": "unlocking"})
        );
        assert_eq!(
            serde_json::to_value(CheckFix::CacheUndecryptable {
                target: "p".into(),
                path: "/s/vault".into(),
                key: CacheKeyView::StoreVault,
            })
            .unwrap(),
            serde_json::json!({"kind": "cache_undecryptable", "target": "p", "path": "/s/vault", "key": "store_vault"})
        );
        assert_eq!(
            serde_json::to_value(CheckId::SameUserPrograms).unwrap(),
            "same_user_programs"
        );
        assert_eq!(
            serde_json::to_value(CheckId::Hibernation).unwrap(),
            "hibernation"
        );
    }

    const SEALED_STUB: &str =
        "-----BEGIN AGE ENCRYPTED FILE-----\nx\n-----END AGE ENCRYPTED FILE-----\n";

    /// No row's title or detail carries `secret`.
    fn shows_no(r: &DoctorReport, secret: &str) -> bool {
        r.groups.iter().flat_map(|g| g.checks.iter()).all(|c| {
            !c.title.contains(secret) && !c.detail.as_deref().unwrap_or("").contains(secret)
        })
    }

    /// D.4a T66 (d4a-f): on a locked store with nothing open, doctor asks its client for nothing
    /// (`NeverAsks` panics if asked) and skips every row that needs a secret, saying why, with
    /// the unlock as the fix; the rows that need none still run.
    #[test]
    fn a_locked_store_with_nothing_open_skips_what_needs_a_secret_and_asks_nothing() {
        let f = fx();
        crate::secrets::testing::locked_store(&f.root.join("config"), &[("prod", Some(TOKEN))]);
        let ctx = ctx(&f, OFFLINE).with_vault_access(Arc::new(crate::secrets::testing::NeverAsks));
        seed_state(
            &ctx,
            "prod",
            serde_json::json!({"server_id": 7, "server_name": "n", "kubeconfig_age": SEALED_STUB}),
        );
        let r = doctor(&ctx, named("prod"));
        let t = group(&r, GroupId::Target);
        assert_eq!(
            (t[0].id, t[0].status),
            (CheckId::ConfigReadable, CheckStatus::Pass)
        );
        let format = t.iter().find(|c| c.id == CheckId::TokenFormat).unwrap();
        assert_eq!(
            (format.status, format.detail.as_deref(), format.fix.clone()),
            (
                CheckStatus::Skipped,
                Some("the target store is locked"),
                Some(CheckFix::UnlockFirst)
            )
        );
        let verified = t.iter().find(|c| c.id == CheckId::TokenVerified).unwrap();
        assert_eq!(
            (verified.status, verified.detail.as_deref()),
            (CheckStatus::Skipped, Some("the target store is locked"))
        );
        let c = cluster(&r);
        for row in [&c[1], &c[2]] {
            assert_eq!(
                (row.status, row.detail.as_deref(), row.fix.clone()),
                (
                    CheckStatus::Skipped,
                    Some("the target store is locked"),
                    Some(CheckFix::UnlockFirst)
                ),
                "{:?}",
                row.id
            );
        }
        assert!(shows_no(&r, TOKEN));
    }

    /// T66: with a session open (a key this client holds), doctor reads the token through it
    /// and pings with it, and the Kube API row gets the cache's key through it: it goes on to
    /// look for `kubectl` (none on this fixture's search path), which it would not reach on a
    /// store it could not open.
    #[test]
    fn a_locked_store_with_a_session_open_is_checked_through_it() {
        let mut server = mockito::Server::new();
        let m = server
            .mock("GET", "/v1/locations")
            .match_header("authorization", format!("Bearer {TOKEN}").as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"locations":[]}"#)
            .create();
        let f = fx();
        let locked =
            crate::secrets::testing::locked_store(&f.root.join("config"), &[("prod", Some(TOKEN))]);
        let ctx = ctx(&f, &server.url()).with_vault_access(locked.access());
        let sealed = cli_core::secrets::encrypt_for_recipient(YAML, locked.recipient()).unwrap();
        seed_state(
            &ctx,
            "prod",
            serde_json::json!({"server_id": 7, "server_name": "n", "kubeconfig_age": sealed}),
        );
        let r = doctor(&ctx, named("prod"));
        let t = group(&r, GroupId::Target);
        let format = t.iter().find(|c| c.id == CheckId::TokenFormat).unwrap();
        assert_eq!(format.detail.as_deref(), Some("64 chars, alphanumeric"));
        assert_eq!(
            t.iter()
                .find(|c| c.id == CheckId::TokenVerified)
                .unwrap()
                .status,
            CheckStatus::Pass
        );
        let api = &cluster(&r)[1];
        assert_eq!(
            (api.id, api.status, api.detail.as_deref()),
            (
                CheckId::KubeApiReachable,
                CheckStatus::Skipped,
                Some("`kubectl` not found")
            ),
            "{api:?}"
        );
        assert!(shows_no(&r, TOKEN));
        m.assert();
    }

    /// T42, T34, T43 (d4a-f): a record a locked store never uses — a clear token (`prod`),
    /// another store's marker (`dev`), its own marker with no vault entry (`qa`) — fails the
    /// token row with the way to replace it, and the ping and the node lookup are skipped as
    /// "no token this store uses", before any network (this fixture's API answers nothing).
    /// The clear token is never shown.
    #[test]
    fn a_token_the_locked_store_never_uses_fails_its_row_and_skips_the_ping() {
        let f = fx();
        let locked = crate::secrets::testing::locked_store(
            &f.root.join("config"),
            &[("prod", None), ("dev", None), ("qa", None)],
        );
        let ctx = ctx(&f, OFFLINE).with_vault_access(locked.access());
        std::fs::write(
            ctx.store().target_credentials_file("prod"),
            format!("hetzner_token: {TOKEN}\n"),
        )
        .unwrap();
        let other = crate::secrets::StoreId::parse("0123456789abcdef").unwrap();
        crate::secrets::marker::write_marker(&ctx.store(), "dev", &other).unwrap();
        crate::secrets::marker::write_marker(&ctx.store(), "qa", &locked.store_id).unwrap();
        for name in ["prod", "dev", "qa"] {
            seed_state(
                &ctx,
                name,
                serde_json::json!({"server_id": 7, "server_name": "n"}),
            );
        }
        let renew = |target: &str, why| {
            Some(CheckFix::RenewToken {
                target: target.into(),
                why,
            })
        };
        for (name, want) in [
            ("prod", renew("prod", RenewWhy::TokenOutsideVault)),
            ("dev", renew("dev", RenewWhy::TokenInAnotherVault)),
            ("qa", None),
        ] {
            let r = doctor(&ctx, named(name));
            let t = group(&r, GroupId::Target);
            let present = t
                .iter()
                .find(|c| c.id == CheckId::TokenPresent)
                .unwrap_or_else(|| panic!("{name}: no token row: {t:?}"));
            assert_eq!(present.status, CheckStatus::Fail, "{name}");
            match want {
                Some(fix) => assert_eq!(present.fix, Some(fix), "{name}"),
                None => assert!(
                    matches!(&present.fix, Some(CheckFix::Explain { text }) if text.contains("rename it back")),
                    "{name}: {:?}",
                    present.fix
                ),
            }
            let verified = t.iter().find(|c| c.id == CheckId::TokenVerified).unwrap();
            assert_eq!(
                (verified.status, verified.detail.as_deref()),
                (CheckStatus::Skipped, Some("no token this store uses")),
                "{name}"
            );
            let node = &cluster(&r)[2];
            assert_eq!(
                (node.id, node.status, node.detail.as_deref()),
                (
                    CheckId::NodeSshReachable,
                    CheckStatus::Skipped,
                    Some("no token this store uses")
                ),
                "{name}"
            );
            assert!(shows_no(&r, TOKEN), "{name}");
        }
        // Without a ping the token is only located (Step 3's (3)): the two records a file
        // shows are refused alike; an own marker with no entry needs the vault, and reads
        // "in the vault".
        let quiet = ctx.clone().with_no_ping(true);
        for (name, why) in [
            ("prod", RenewWhy::TokenOutsideVault),
            ("dev", RenewWhy::TokenInAnotherVault),
        ] {
            let t = group(&doctor(&quiet, named(name)), GroupId::Target);
            let present = t.iter().find(|c| c.id == CheckId::TokenPresent).unwrap();
            assert_eq!(present.fix, renew(name, why), "{name} --no-ping");
        }
    }

    /// T66, §3.13 (d4a-f): an interrupted `lock on` lets no secret be read: the token, Kube API
    /// and node rows are skipped with the refusal's own words — not a failure of theirs, and
    /// not a pointer to a row that might read as a pass.
    #[test]
    fn an_interrupted_lock_skips_what_needs_a_secret_with_the_refusals_words() {
        let f = fx();
        let root = f.root.join("config");
        crate::secrets::testing::locked_store(&root, &[("prod", Some(TOKEN))]);
        let yaml = root.join(crate::secrets::STORE_FILE);
        let text = std::fs::read_to_string(&yaml).unwrap();
        assert!(text.contains("secrets: locked\n"), "{text}");
        std::fs::write(
            &yaml,
            text.replace("secrets: locked\n", "secrets: locking\n"),
        )
        .unwrap();
        let ctx = ctx(&f, OFFLINE).with_vault_access(Arc::new(crate::secrets::testing::NeverAsks));
        seed_state(
            &ctx,
            "prod",
            serde_json::json!({"server_id": 7, "server_name": "n", "kubeconfig_age": SEALED_STUB}),
        );
        let r = doctor(&ctx, named("prod"));
        let t = group(&r, GroupId::Target);
        let c = cluster(&r);
        for row in [
            t.iter().find(|c| c.id == CheckId::TokenFormat).unwrap(),
            t.iter().find(|c| c.id == CheckId::TokenVerified).unwrap(),
            &c[1],
            &c[2],
        ] {
            assert_eq!(row.status, CheckStatus::Skipped, "{row:?}");
            assert!(
                row.detail
                    .as_deref()
                    .unwrap_or("")
                    .contains("did not finish"),
                "{row:?}"
            );
        }
        assert!(shows_no(&r, TOKEN));
    }

    /// A client with a session open whose token reads are counted: each is an item a
    /// terminal's window would serve, and so restart (T56). It never prompts: `session` and
    /// `write_key` panic, so a doctor run that reached past `open_session` would fail here.
    #[derive(Debug)]
    struct CountingWindow {
        inner: Arc<dyn crate::secrets::VaultAccess>,
        tokens: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[derive(Debug)]
    struct CountingSession {
        inner: Arc<dyn crate::secrets::VaultSession>,
        tokens: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl crate::secrets::VaultSession for CountingSession {
        fn hetzner_token(
            &self,
            vault: &[u8],
            target: &str,
        ) -> cli_core::Result<Option<SecretString>> {
            self.tokens
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.hetzner_token(vault, target)
        }

        fn open_cache(
            &self,
            vault: &[u8],
            secret: cli_core::secrets::CachedSecret,
            target: &str,
            armored: &str,
        ) -> cli_core::Result<crate::Zeroizing<String>> {
            self.inner.open_cache(vault, secret, target, armored)
        }

        fn recipient(&self, vault: &[u8]) -> cli_core::Result<cli_core::secrets::Recipient> {
            self.inner.recipient(vault)
        }
    }

    impl crate::secrets::VaultAccess for CountingWindow {
        fn session(
            &self,
            _: &crate::secrets::VaultRequest<'_>,
        ) -> cli_core::Result<Arc<dyn crate::secrets::VaultSession>> {
            panic!("a diagnostic asked for a session that is not open")
        }

        fn write_key(
            &self,
            _: &crate::secrets::VaultRequest<'_>,
        ) -> cli_core::Result<crate::secrets::KeySession> {
            panic!("a diagnostic asked for a write key")
        }

        fn open_session(
            &self,
            req: &crate::secrets::VaultRequest<'_>,
        ) -> cli_core::Result<Option<Arc<dyn crate::secrets::VaultSession>>> {
            Ok(self.inner.open_session(req)?.map(|inner| {
                Arc::new(CountingSession {
                    inner,
                    tokens: self.tokens.clone(),
                }) as Arc<dyn crate::secrets::VaultSession>
            }))
        }
    }

    /// T56, T66 (d4a-f; e's verifier, finding 4): every token read through a terminal's window
    /// is an item it serves, and restarts it. Doctor reads the token through a window only
    /// for its ping: without one it reads none — the token row says where the token is — and
    /// with one it does.
    #[test]
    fn a_doctor_without_a_ping_reads_no_token_through_a_window() {
        let f = fx();
        let locked =
            crate::secrets::testing::locked_store(&f.root.join("config"), &[("prod", Some(TOKEN))]);
        let tokens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let window = Arc::new(CountingWindow {
            inner: locked.access(),
            tokens: tokens.clone(),
        });
        let quiet = ctx(&f, OFFLINE)
            .with_vault_access(window.clone())
            .with_no_ping(true);
        let r = doctor(&quiet, named("prod"));
        let format = group(&r, GroupId::Target)
            .into_iter()
            .find(|c| c.id == CheckId::TokenFormat)
            .unwrap();
        assert_eq!(
            (format.status, format.detail.as_deref(), format.fix.clone()),
            (
                CheckStatus::Skipped,
                Some("in the vault; read only with the ping"),
                None
            )
        );
        assert_eq!(
            tokens.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a --no-ping doctor read a token through the window"
        );
        doctor(&ctx(&f, OFFLINE).with_vault_access(window), named("prod"));
        assert!(
            tokens.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the ping reads its token through the window"
        );
    }

    /// The "This computer" rows from the `secrets` row on.
    fn lock_part(r: &DoctorReport) -> Vec<Check> {
        let all = group(r, GroupId::ThisComputer);
        let at = all
            .iter()
            .position(|c| c.id == CheckId::Secrets)
            .expect("a secrets row");
        all[at..].to_vec()
    }

    fn open_window(left_secs: u64, memory_locked: bool) -> UnlockWindow {
        UnlockWindow::Open {
            left_secs,
            minutes: 5,
            holder_version: env!("CARGO_PKG_VERSION").into(),
            hardening: HolderHardening {
                non_dumpable: Some(true),
                deny_attach: None,
                core_off: true,
                memory_locked,
                process_dacl: None,
            },
        }
    }

    fn in_tmux() -> LimitClient {
        LimitClient::Cli {
            terminal: Some(TerminalFacts {
                multiplexer: Some("tmux".into()),
                x_client: None,
                display: DisplayKind::None,
            }),
            ssh: SshFacts::default(),
        }
    }

    fn locked_at(f: &Fx, targets: &[(&str, Option<&str>)]) -> PathBuf {
        let root = f.root.join("config");
        crate::secrets::testing::locked_store(&root, targets);
        root
    }

    /// D.4a T71, §3.13: a store in files is one fact, not a verdict — Info, its fix `lock on`,
    /// in no total — and no limit of this computer: those qualify a lock.
    #[test]
    fn a_store_in_files_is_one_fact_and_no_limit() {
        let f = fx();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        add(&ctx, "prod", "hetzner-cloud", Some(TOKEN), None);
        let r = doctor_with(&ctx, named("prod"), UnlockWindow::NoTerminal, in_tmux());
        let rows = lock_part(&r);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            (rows[0].status, rows[0].title.as_str(), rows[0].fix.clone()),
            (
                CheckStatus::Info,
                "Secrets in files (readable by any program running as you)",
                Some(CheckFix::ProtectSecrets)
            )
        );
        assert_eq!(r.informed(), 1);
    }

    /// §3.13, T73, O1: a locked store names its slots and this terminal's window, and lists
    /// what this computer lets programs running as you do and what the lock does not cover.
    #[test]
    fn a_locked_store_names_its_slots_its_window_and_its_limits() {
        let f = fx();
        locked_at(&f, &[("prod", Some(TOKEN))]);
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        let rows = lock_part(&doctor_with(
            &ctx,
            named("prod"),
            open_window(170, true),
            in_tmux(),
        ));
        assert_eq!(
            (
                rows[0].status,
                rows[0].title.as_str(),
                rows[0].detail.as_deref()
            ),
            (
                CheckStatus::Pass,
                "Secrets locked (1 slot: recovery key)",
                Some("unlocked in this terminal, 3 min left")
            )
        );
        let by = |id| {
            rows.iter()
                .find(|c| c.id == id)
                .unwrap_or_else(|| panic!("no {id:?} row: {rows:?}"))
        };
        assert_eq!(
            by(CheckId::TerminalMultiplexer).title,
            "This terminal runs inside tmux: programs running as you can type into it"
        );
        for id in [
            CheckId::UnlockedTerminal,
            CheckId::SecretsInUse,
            CheckId::SameUserPrograms,
            CheckId::Hibernation,
        ] {
            assert_eq!(by(id).status, CheckStatus::Info, "{id:?}");
        }
        assert!(rows.iter().all(|c| c.id != CheckId::WindowHardening));
        #[cfg(windows)]
        assert_eq!(by(CheckId::WindowsSameUser).status, CheckStatus::Warn);
        #[cfg(target_os = "macos")]
        assert_eq!(by(CheckId::MacosTaskPort).status, CheckStatus::Info);
    }

    /// §3.13: an interrupted `lock on` or `lock off` fails with the way to finish it; the
    /// limits show while locking, not while unlocking (the store is going back to files).
    #[test]
    fn an_interrupted_lock_fails_with_the_way_to_finish_it() {
        for (phase, title, limits) in [
            (
                SecretsMode::Locking,
                "Secrets: turning the lock on did not finish",
                true,
            ),
            (
                SecretsMode::Unlocking,
                "Secrets: turning the lock off did not finish",
                false,
            ),
        ] {
            let f = fx();
            let root = locked_at(&f, &[("prod", Some(TOKEN))]);
            let yaml = root.join(crate::secrets::STORE_FILE);
            let text = std::fs::read_to_string(&yaml).unwrap();
            assert!(text.contains("secrets: locked\n"), "{text}");
            std::fs::write(
                &yaml,
                text.replace(
                    "secrets: locked\n",
                    &format!("secrets: {}\n", phase.as_str()),
                ),
            )
            .unwrap();
            let ctx = ctx(&f, OFFLINE).with_no_ping(true);
            let rows = lock_part(&doctor_with(
                &ctx,
                named("prod"),
                UnlockWindow::Closed,
                quiet(),
            ));
            assert_eq!(
                (rows[0].status, rows[0].title.as_str(), rows[0].fix.clone()),
                (
                    CheckStatus::Fail,
                    title,
                    Some(CheckFix::FinishLock { phase })
                )
            );
            assert_eq!(
                rows.iter().any(|c| c.id == CheckId::SecretsInUse),
                limits,
                "{phase:?}"
            );
        }
    }

    /// Rules 1 and 3, §3.13: a vault that cannot be read fails the `secrets` row with the
    /// store's own help, and hides none of this computer's limits.
    #[test]
    fn a_vault_that_cannot_be_read_fails_and_the_limits_still_show() {
        let f = fx();
        let root = locked_at(&f, &[("prod", Some(TOKEN))]);
        std::fs::write(root.join(crate::vault::VAULT_FILE), b"not a vault").unwrap();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        let rows = lock_part(&doctor_with(
            &ctx,
            named("prod"),
            UnlockWindow::Closed,
            quiet(),
        ));
        assert_eq!(
            (rows[0].status, rows[0].title.as_str()),
            (
                CheckStatus::Fail,
                "Secrets locked, but the vault cannot be read"
            )
        );
        assert!(
            matches!(&rows[0].fix, Some(CheckFix::Explain { text }) if text.contains("backup")),
            "{:?}",
            rows[0].fix
        );
        assert!(
            rows.iter().any(|c| c.id == CheckId::SecretsInUse),
            "{rows:?}"
        );
    }

    /// T31, rule 1: a `store.yaml` that cannot be read fails the `secrets` row with the
    /// store's own help — the store is treated as locked, so this computer's limits show too.
    #[test]
    fn a_store_file_that_cannot_be_read_fails_and_the_limits_still_show() {
        let f = fx();
        let root = locked_at(&f, &[("prod", Some(TOKEN))]);
        std::fs::write(
            root.join(crate::secrets::STORE_FILE),
            b"secrets: [not yaml\n",
        )
        .unwrap();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        let rows = lock_part(&doctor_with(
            &ctx,
            named("prod"),
            UnlockWindow::Closed,
            quiet(),
        ));
        assert_eq!(
            (rows[0].status, rows[0].title.as_str()),
            (
                CheckStatus::Fail,
                "Secrets: whether they are locked cannot be read"
            )
        );
        assert!(
            matches!(&rows[0].fix, Some(CheckFix::Explain { text }) if text.contains("never delete it")),
            "{:?}",
            rows[0].fix
        );
        assert!(
            rows.iter().any(|c| c.id == CheckId::SecretsInUse),
            "{rows:?}"
        );
    }

    /// §3.13: the facts beside a locked store, each a warning with its fix — a slot list in
    /// store.yaml unlike the vault's, a vault file others can read, another target's clear
    /// token or foreign marker, another target's clear kubeconfig — and the target checked is
    /// its own rows', never repeated here. A healthy vault (its recovery-key slot, no refused
    /// slot) adds no slot fact.
    #[test]
    fn a_locked_store_states_the_facts_beside_it() {
        let f = fx();
        let root = locked_at(&f, &[("prod", None), ("dev", None), ("qa", None)]);
        let yaml = root.join(crate::secrets::STORE_FILE);
        let text = std::fs::read_to_string(&yaml).unwrap();
        assert!(text.contains("label: recovery key"), "{text}");
        std::fs::write(
            &yaml,
            text.replace("label: recovery key", "label: spare key"),
        )
        .unwrap();
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        for t in ["prod", "dev"] {
            std::fs::write(
                ctx.store().target_credentials_file(t),
                format!("hetzner_token: {TOKEN}\n"),
            )
            .unwrap();
            seed_state(
                &ctx,
                t,
                serde_json::json!({"server_id": 7, "server_name": "n", "kubeconfig_yaml": YAML}),
            );
        }
        let other = crate::secrets::StoreId::parse("0123456789abcdef").unwrap();
        crate::secrets::marker::write_marker(&ctx.store(), "qa", &other).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.join(crate::vault::VAULT_FILE),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        }
        let rows = lock_part(&doctor_with(
            &ctx,
            named("prod"),
            UnlockWindow::Closed,
            quiet(),
        ));
        let warns: Vec<&str> = rows
            .iter()
            .filter(|c| c.id == CheckId::Secrets && c.status == CheckStatus::Warn)
            .map(|c| c.title.as_str())
            .collect();
        let fix_of = |title: &str| {
            rows.iter()
                .find(|c| c.title == title)
                .unwrap_or_else(|| panic!("no row {title:?}: {warns:?}"))
                .fix
                .clone()
        };
        assert!(
            warns.contains(&"The slot list in store.yaml differs from the vault's"),
            "{warns:?}"
        );
        assert_eq!(
            fix_of("Target `dev`'s credentials file holds a token in clear, which this locked store never uses"),
            Some(CheckFix::RenewToken { target: "dev".into(), why: RenewWhy::TokenOutsideVault })
        );
        assert_eq!(
            fix_of("Target `qa`'s credentials file names another target store's vault"),
            Some(CheckFix::RenewToken {
                target: "qa".into(),
                why: RenewWhy::TokenInAnotherVault
            })
        );
        assert_eq!(
            fix_of("Target `dev`'s state file holds a kubeconfig in clear, which this locked store never uses"),
            Some(CheckFix::FetchKubeconfig { target: "dev".into() })
        );
        #[cfg(unix)]
        assert!(
            warns.contains(&"Vault file mode 0600 (got 644)"),
            "{warns:?}"
        );
        assert!(
            warns
                .iter()
                .all(|w| !w.contains("key-derivation") && !w.contains("recovery-key slot")),
            "{warns:?}"
        );
        assert!(rows.iter().all(|c| !c.title.contains("`prod`")), "{rows:?}");
        assert!(rows.iter().all(
            |c| !c.title.contains(TOKEN) && !c.detail.as_deref().unwrap_or("").contains(TOKEN)
        ));
    }

    /// T22, O8 (d4a-f): a slot this build refuses is a warning saying what that means, with the
    /// refusal's own help; a vault with no recovery-key slot is a warning; a vault with its
    /// recovery key and no refused slot is no row. (A refused slot cannot be made here without
    /// deriving a weak key: the rows are built from the report's slot views.)
    #[test]
    fn a_refused_slot_or_no_recovery_key_is_a_warning() {
        use crate::lock::{SlotRefusalView as V, SlotView};
        use crate::secrets::LockSlotKind as K;
        let slot = |kind, label: &str, refused| SlotView {
            kind,
            label: label.into(),
            refused,
        };
        let store = Path::new("/s");
        let healthy = [
            slot(K::Password, "lock password", None),
            slot(K::RecoveryKey, "recovery key", None),
        ];
        assert!(slot_facts(&healthy, store).is_empty());
        let weak = slot_facts(
            &[
                slot(K::Password, "lock password", Some(V::KdfTooWeak)),
                slot(K::RecoveryKey, "recovery key", None),
            ],
            store,
        );
        assert_eq!(
            weak.iter().map(|c| (c.status, c.title.as_str())).collect::<Vec<_>>(),
            [(
                CheckStatus::Warn,
                "The `lock password` slot uses key-derivation settings weaker than this AppRafter accepts: a copy of the vault is faster to guess than it allows"
            )]
        );
        assert!(
            matches!(&weak[0].fix, Some(CheckFix::Explain { text }) if text.contains("`apprafter lock rekey`")),
            "{:?}",
            weak[0].fix
        );
        let lone = slot_facts(
            &[slot(K::Password, "lock password", Some(V::KdfOutOfRange))],
            store,
        );
        assert_eq!(
            lone.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            [
                "The `lock password` slot uses key-derivation settings beyond what this AppRafter computes: it cannot open the vault",
                "The vault has no recovery-key slot"
            ]
        );
    }

    /// T35, §3.13: an override variable set beside a locked store is a warning that names the
    /// variable — never its value — with the refusal's own help.
    #[test]
    fn an_override_beside_a_locked_store_is_named_never_shown() {
        let rows = override_rows(true, true, Path::new("/s"));
        assert_eq!(
            rows.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            [
                "HCLOUD_TOKEN is set, and this locked store refuses it",
                "APPRAFTER_AGE_KEY is set, and this locked store refuses it"
            ]
        );
        assert!(rows.iter().all(|c| matches!(&c.fix, Some(CheckFix::Explain { text }) if text.contains("APPRAFTER_CONFIG_DIR"))));
        assert!(override_rows(false, false, Path::new("/s")).is_empty());
    }

    /// T54, T72: a holder that could not lock its key's page is a warning on a locked store.
    #[test]
    fn a_weakened_holder_is_a_warning() {
        let f = fx();
        locked_at(&f, &[("prod", Some(TOKEN))]);
        let ctx = ctx(&f, OFFLINE).with_no_ping(true);
        let rows = lock_part(&doctor_with(
            &ctx,
            named("prod"),
            open_window(60, false),
            quiet(),
        ));
        let row = rows
            .iter()
            .find(|c| c.id == CheckId::WindowHardening)
            .expect("a window_hardening row");
        assert_eq!(
            (row.status, row.title.as_str()),
            (
                CheckStatus::Warn,
                "The unlock window's key is less protected than usual: its memory is not locked against paging"
            )
        );
    }

    /// §3.13: every limit's row — its id, its weight (the line's) and a fix; the holder base's
    /// fallback is a `lock status` line only.
    #[test]
    fn every_limit_becomes_its_row() {
        use MachineLimit as L;
        let ssh = L::SshKeyUsable {
            path: "~/k".into(),
            unencrypted: true,
            in_agent: Some(false),
        };
        let cases = [
            (
                L::X11Session,
                Some((CheckId::X11Keystrokes, CheckStatus::Warn)),
            ),
            (
                L::Xwayland {
                    terminal_is_x_client: Some(true),
                },
                Some((CheckId::X11Keystrokes, CheckStatus::Warn)),
            ),
            (
                L::Xwayland {
                    terminal_is_x_client: None,
                },
                Some((CheckId::X11Keystrokes, CheckStatus::Info)),
            ),
            (
                L::AppOnXwayland,
                Some((CheckId::X11Keystrokes, CheckStatus::Warn)),
            ),
            (
                L::PtraceScope { value: Some(0) },
                Some((CheckId::PtraceScope, CheckStatus::Warn)),
            ),
            (
                L::PtraceScope { value: None },
                Some((CheckId::PtraceScope, CheckStatus::Warn)),
            ),
            (
                L::WindowsSameUser,
                Some((CheckId::WindowsSameUser, CheckStatus::Warn)),
            ),
            (
                L::MacosTaskPort,
                Some((CheckId::MacosTaskPort, CheckStatus::Info)),
            ),
            (
                L::Multiplexer {
                    name: "tmux".into(),
                },
                Some((CheckId::TerminalMultiplexer, CheckStatus::Info)),
            ),
            (
                L::InheritedWindow,
                Some((CheckId::UnlockedTerminal, CheckStatus::Info)),
            ),
            (
                L::SleepUnreported,
                Some((CheckId::SleepUnreported, CheckStatus::Warn)),
            ),
            (
                ssh.clone(),
                Some((CheckId::SshKeyExposure, CheckStatus::Warn)),
            ),
            (
                L::SecretsInUse,
                Some((CheckId::SecretsInUse, CheckStatus::Info)),
            ),
            (
                L::SameUserPrograms,
                Some((CheckId::SameUserPrograms, CheckStatus::Info)),
            ),
            (
                L::Hibernation,
                Some((CheckId::Hibernation, CheckStatus::Info)),
            ),
            (
                L::RuntimeDirFallback {
                    path: "/s/run".into(),
                },
                None,
            ),
        ];
        for (limit, want) in cases {
            let got = limit_check(&limit);
            assert_eq!(got.as_ref().map(|c| (c.id, c.status)), want, "{limit:?}");
            if let Some(c) = got {
                assert_eq!(
                    Some(c.title.clone()),
                    crate::lock::text::limit_line(&limit).map(|l| l.text)
                );
                assert!(c.fix.is_some(), "{limit:?}");
            }
        }
        assert_eq!(
            limit_check(&ssh).unwrap().fix,
            Some(CheckFix::ProtectSshKey { path: "~/k".into() })
        );
    }
}
