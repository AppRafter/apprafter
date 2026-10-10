// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Persistent target store for the `apprafter` CLI.
//!
//! A **Target** is a named bundle of `(provider, region,
//! credentials, defaults)` — one deployment destination. Multiple
//! targets coexist; exactly one is "active" at a time. The Target
//! abstraction is generic enough to add AWS / OpenBao / Managed
//! AppRafter Cloud later without renaming the namespace.
//!
//! ## File layout (`cli-dx-task.md` §4)
//!
//! ```text
//! $XDG_CONFIG_HOME/apprafter/          # resolved via dirs::config_dir
//! ├── .lock                            # StoreLock sentinel, always empty
//! ├── config.yaml                      # GlobalConfig (active_target + version)
//! ├── targets/
//! │   ├── default/
//! │   │   ├── config.yaml              # TargetConfig (non-secret)
//! │   │   └── credentials.yaml         # TargetCredentials, mode 0600
//! │   └── work/
//! │       ├── config.yaml
//! │       └── credentials.yaml
//! ├── auth/                            # reserved for `apprafter auth`
//! │   └── .keep                        # placeholder; structure TBD when Managed lands
//! └── state/
//!     └── <target>/                    # per-target runtime caches (kubeconfig, etc.)
//! ```
//!
//! ## Scope of this module (v0.1.72 — Track A.2)
//!
//! Foundation only: types + load/save IO + atomic file replace +
//! mode 0600 enforcement on credentials. **No CLI commands wired**
//! yet — those land in Track A.3 (`target add` non-interactive),
//! A.4 (interactive wizard), A.5 (`list / use / show / rename /
//! remove`), and A.8 (resolution chain plumbed into existing
//! `init` / `apply` / `cluster-bootstrap`). The wire format is
//! YAML; the env-var fallback (`HCLOUD_TOKEN` etc.) lives on
//! unchanged and stays the highest-priority override in the
//! resolution chain spec'd in `cli-dx-task.md` §7.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{CliError, Result};

/// On-disk version of `config.yaml`. Bumped if the format ever
/// changes incompatibly so future loaders can migrate forward.
pub const TARGET_STORE_VERSION: u32 = 1;

const GLOBAL_CONFIG_FILE: &str = "config.yaml";
const TARGETS_DIR: &str = "targets";
const AUTH_DIR: &str = "auth";
const STATE_DIR_NAME: &str = "state";
const TARGET_CONFIG_FILE: &str = "config.yaml";
const TARGET_CREDENTIALS_FILE: &str = "credentials.yaml";
const AUTH_KEEP_FILE: &str = ".keep";
const LOCK_FILE: &str = ".lock";

/// Env-var that overrides `default_config_root()`. Primary use is
/// integration tests pointing the store at a `tempfile::TempDir`
/// without touching the developer's real `~/.config/apprafter/`;
/// power users can also redirect their store for compartmentalised
/// experimentation. Honoured ahead of `dirs::config_dir()` so the
/// override is reliable even on macOS where `dirs::config_dir()`
/// returns `~/Library/Application Support/` instead of `~/.config/`.
pub const CONFIG_DIR_ENV: &str = "APPRAFTER_CONFIG_DIR";

/// Resolve the target-store root. Order:
///
/// 1. `$APPRAFTER_CONFIG_DIR` (used verbatim — no `apprafter/`
///    suffix appended, so tests can point straight at their tempdir).
/// 2. `dirs::config_dir().join("apprafter")` (XDG on Linux, native
///    on macOS/Windows).
///
/// Errors when neither path resolves (no `HOME`, no
/// `XDG_CONFIG_HOME`, no env override) — should never happen on a
/// sane install, but `Result` is the safe surface.
pub fn default_config_root() -> Result<PathBuf> {
    config_root_from_override(std::env::var(CONFIG_DIR_ENV).ok())
}

/// The target-store root for an override the caller passes explicitly — the
/// value of `APPRAFTER_CONFIG_DIR` as the caller read it. A non-empty value
/// is used verbatim; `None` or an empty value falls back to
/// `dirs::config_dir().join("apprafter")`.
///
/// It never reads `APPRAFTER_CONFIG_DIR` itself, so a caller that reads its
/// environment through something other than `std::env` (the shared core's
/// `Context` builder, ADR 0067) resolves the root exactly as
/// [`default_config_root`] does. It is not pure, though: the fallback
/// consults the platform config directory through `dirs` (HOME /
/// `XDG_CONFIG_HOME` on Unix, the Known Folder API on Windows).
pub fn config_root_from_override(custom: Option<String>) -> Result<PathBuf> {
    if let Some(custom) = custom {
        if !custom.is_empty() {
            return Ok(PathBuf::from(custom));
        }
    }
    dirs::config_dir()
        .map(|p| p.join("apprafter"))
        .ok_or_else(|| {
            CliError::Other("cannot resolve config dir (HOME / XDG_CONFIG_HOME both unset?)".into())
        })
}

/// The length of a Hetzner Cloud API token: 64 ASCII alphanumeric characters.
pub const HETZNER_TOKEN_LEN: usize = 64;

/// Validate a Hetzner Cloud API token's surface format. Cheap
/// pre-flight before the real `GET /v1/locations` ping that
/// arrives in Track A.4 — here we only catch obvious typos and
/// wrong-credential-pasted-into-wrong-field mistakes (e.g. AWS
/// access key landed in `--token`).
///
/// **Format.** Hetzner Cloud tokens copied from the Cloud Console
/// → Security → API Tokens panel are 64 ASCII alphanumeric
/// characters with no fixed prefix. The `HCLOUD_TOKEN` env var
/// name is a Hetzner convention; the value inside it is just the
/// bare 64 chars. `cli-dx-task.md` §11 originally documented an
/// `hcloud_` prefix that doesn't exist in practice — the spec was
/// amended in v0.1.74 to match reality, and the v0.1.73 validator
/// that rejected real tokens has been replaced with this one.
///
/// Returns `Ok(())` on success or a humane string the caller wraps
/// into a `CliError::Other` with surrounding context. We keep the
/// return shape `Result<_, String>` rather than `Result<_, CliError>`
/// because validators are pure: no IO, no errno; the caller knows
/// best how to phrase the surrounding error ("invalid token for
/// --token flag" vs. "invalid token in credentials.yaml").
pub fn validate_hetzner_token_format(token: &str) -> std::result::Result<(), String> {
    if token.len() != HETZNER_TOKEN_LEN {
        return Err(format!(
            "Hetzner Cloud tokens are {HETZNER_TOKEN_LEN} ASCII alphanumeric characters; got {}",
            token.len()
        ));
    }
    if !token.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(
            "Hetzner Cloud tokens are ASCII alphanumeric — found a non-[A-Za-z0-9] character (whitespace? a dash? something pasted with surrounding quotes?)"
                .to_string(),
        );
    }
    Ok(())
}

/// Locator for every path inside the target store. Carries a
/// `root` instead of touching the filesystem at construction so
/// unit tests can point `root` at a `tempfile::TempDir` and run
/// the full load/save round-trip without writing into the user's
/// real `~/.config/apprafter/`.
#[derive(Debug, Clone)]
pub struct TargetStorePaths {
    root: PathBuf,
}

impl TargetStorePaths {
    /// Use the supplied root. Pass `default_config_root()?` in
    /// production code (`TargetStorePaths::for_root(default_config_root()?)`);
    /// pass a `TempDir.path().to_path_buf()` in tests. A
    /// `::default()` convenience constructor would clash with the
    /// `Default` trait signature (we'd need `Result<Self>`), so we
    /// keep root selection explicit at the call site.
    pub fn for_root(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn global_config_file(&self) -> PathBuf {
        self.root.join(GLOBAL_CONFIG_FILE)
    }

    pub fn targets_dir(&self) -> PathBuf {
        self.root.join(TARGETS_DIR)
    }

    pub fn target_dir(&self, name: &str) -> PathBuf {
        self.targets_dir().join(name)
    }

    pub fn target_config_file(&self, name: &str) -> PathBuf {
        self.target_dir(name).join(TARGET_CONFIG_FILE)
    }

    pub fn target_credentials_file(&self, name: &str) -> PathBuf {
        self.target_dir(name).join(TARGET_CREDENTIALS_FILE)
    }

    pub fn auth_dir(&self) -> PathBuf {
        self.root.join(AUTH_DIR)
    }

    pub fn auth_keep_file(&self) -> PathBuf {
        self.auth_dir().join(AUTH_KEEP_FILE)
    }

    /// Per-target runtime caches (kubeconfig, age-encrypted secrets,
    /// etc.) live under `<root>/state/<target>/`. Track A.8 wires
    /// the existing per-CWD `.apprafter/state.json` flow over here.
    pub fn state_dir(&self, name: &str) -> PathBuf {
        self.root.join(STATE_DIR_NAME).join(name)
    }

    /// The sentinel [`StoreLock`] locks. A dot-file at the root, so it is
    /// neither a target nor a file anything reads.
    pub fn lock_file(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }
}

/// What taking a [`StoreLock`] reports, for the caller to show in its own
/// way (the CLI prints a stderr line; the core will have its own channel).
#[derive(Debug)]
pub enum StoreLockEvent<'a> {
    /// Another process holds the lock, and the call is about to wait for
    /// it. Reported once, before the wait.
    Waiting {
        /// The sentinel, `<root>/.lock`.
        sentinel: &'a Path,
    },
    /// The store cannot be locked here — the sentinel can be neither
    /// opened nor created (a read-only store), or the filesystem has no
    /// locks — so the guard returned holds nothing, and the edit goes
    /// ahead unlocked, as every edit did before the lock existed.
    Unlocked {
        /// The sentinel, `<root>/.lock`.
        sentinel: &'a Path,
        /// Why the store could not be locked.
        error: &'a std::io::Error,
    },
}

/// The panic a second lock on one thread raises. Both `flock` and
/// `LockFileEx` treat a second handle of the same process as a stranger,
/// so the second acquire would wait for the first forever.
const NESTED_LOCK_PANIC: &str = "a target-store lock is already held on this thread: take one \
     StoreLock per thread, and drop it before taking another (a second one would wait for the \
     first forever)";

/// How often [`StoreLock::try_exclusive_for`] retries a held lock.
const LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(50);

thread_local! {
    /// [`StoreLock`]s alive on this thread: never more than one.
    static LOCKS_ON_THIS_THREAD: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// An advisory, exclusive lock over the whole target store, held for the
/// life of the value. Take it around every read-modify-write of the store
/// (load → change → save), so the CLI and AppRafter Desktop editing the
/// same store never lose each other's update (ADR 0067).
///
/// Never hold it across a slow step — a provider call, a prompt: every
/// other edit of the store waits for it. Do the slow step first, then take
/// the lock, re-read what the change needs, re-check what the step relied
/// on, change, save.
///
/// One per thread: taking a second while one is alive on the same thread
/// panics rather than deadlocking. Release it before taking it again.
///
/// It locks a sentinel, `<root>/.lock`, never a data file: data files are
/// replaced by rename (`atomic_write`), so a lock on one would guard an
/// inode that is about to disappear, and on Windows byte-range locks are
/// mandatory, so locking a data file would make a plain read fail.
///
/// A store that cannot be locked — read-only, or on a filesystem without
/// locks — yields a guard that holds nothing ([`StoreLockEvent::Unlocked`],
/// [`StoreLock::is_held`]): the edit goes ahead unlocked, and a read-only
/// store fails at the save that needs writing, not at the lock.
#[must_use = "the store is locked only while this value is alive"]
pub struct StoreLock {
    /// The locked sentinel; `None` when the store could not be locked.
    file: Option<fs::File>,
}

impl StoreLock {
    /// Block until the store is exclusively locked, reporting nothing.
    /// Creates the root and the sentinel if missing. A store that cannot be
    /// locked yields an unheld guard ([`StoreLock::is_held`]); use
    /// [`StoreLock::exclusive_or_wait`] to be told when, and when the call
    /// waits.
    pub fn exclusive(paths: &TargetStorePaths) -> Result<StoreLock> {
        Self::exclusive_or_wait(paths, |_| {})
    }

    /// [`StoreLock::exclusive`], reporting to `on_event`: once, before
    /// waiting for another process ([`StoreLockEvent::Waiting`]), and when
    /// the store cannot be locked ([`StoreLockEvent::Unlocked`]).
    pub fn exclusive_or_wait(
        paths: &TargetStorePaths,
        mut on_event: impl FnMut(StoreLockEvent<'_>),
    ) -> Result<StoreLock> {
        match acquire(paths, None, &mut on_event)? {
            Some(lock) => Ok(lock),
            None => unreachable!("a lock taken without a deadline is never given up"),
        }
    }

    /// Like [`StoreLock::exclusive_or_wait`], but wait at most `within`
    /// for another process to release the lock, retrying every 50 ms;
    /// `Ok(None)` when it is still held then.
    pub fn try_exclusive_for(
        paths: &TargetStorePaths,
        within: std::time::Duration,
        mut on_event: impl FnMut(StoreLockEvent<'_>),
    ) -> Result<Option<StoreLock>> {
        acquire(paths, Some(within), &mut on_event)
    }

    /// Whether the store is actually locked: `false` for the guard of a
    /// store that cannot be locked ([`StoreLockEvent::Unlocked`]).
    pub fn is_held(&self) -> bool {
        self.file.is_some()
    }

    fn new(file: Option<fs::File>) -> StoreLock {
        LOCKS_ON_THIS_THREAD.with(|n| n.set(n.get() + 1));
        StoreLock { file }
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        LOCKS_ON_THIS_THREAD.with(|n| n.set(n.get().saturating_sub(1)));
    }
}

/// Take the store lock, waiting for another process forever (`within:
/// None`) or at most `within` (`Ok(None)` past it).
fn acquire(
    paths: &TargetStorePaths,
    within: Option<std::time::Duration>,
    on_event: &mut dyn FnMut(StoreLockEvent<'_>),
) -> Result<Option<StoreLock>> {
    assert!(
        LOCKS_ON_THIS_THREAD.with(|n| n.get()) == 0,
        "{NESTED_LOCK_PANIC}"
    );
    let sentinel = paths.lock_file();

    fs::create_dir_all(paths.root()).map_err(|e| lock_error(&sentinel, e))?;
    let file = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&sentinel)
    {
        Ok(file) => file,
        // `flock` and `LockFileEx` need no write access: a sentinel this
        // process may not write is still locked through a read-only
        // handle. One it cannot create either (a read-only store) leaves
        // the store unlockable.
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            match fs::File::open(&sentinel) {
                Ok(file) => file,
                Err(_) => return unlocked(&sentinel, e, on_event),
            }
        }
        Err(e) => return Err(lock_error(&sentinel, e)),
    };

    match file.try_lock() {
        Ok(()) => return Ok(Some(StoreLock::new(Some(file)))),
        Err(fs::TryLockError::WouldBlock) => on_event(StoreLockEvent::Waiting {
            sentinel: &sentinel,
        }),
        Err(fs::TryLockError::Error(e)) if has_no_locks(&e) => {
            return unlocked(&sentinel, e, on_event)
        }
        Err(fs::TryLockError::Error(e)) => return Err(lock_error(&sentinel, e)),
    }
    let locked = match within {
        None => file.lock().map(|()| true),
        Some(within) => poll_lock(&file, within),
    };
    match locked {
        Ok(true) => Ok(Some(StoreLock::new(Some(file)))),
        Ok(false) => Ok(None),
        Err(e) if has_no_locks(&e) => unlocked(&sentinel, e, on_event),
        Err(e) => Err(lock_error(&sentinel, e)),
    }
}

/// The guard of a store that cannot be locked, reported to `on_event`.
fn unlocked(
    sentinel: &Path,
    error: std::io::Error,
    on_event: &mut dyn FnMut(StoreLockEvent<'_>),
) -> Result<Option<StoreLock>> {
    on_event(StoreLockEvent::Unlocked {
        sentinel,
        error: &error,
    });
    Ok(Some(StoreLock::new(None)))
}

/// Retry `try_lock` every [`LOCK_POLL`] until it succeeds (`true`) or
/// `within` has passed (`false`).
fn poll_lock(file: &fs::File, within: std::time::Duration) -> std::io::Result<bool> {
    let deadline = std::time::Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        std::thread::sleep(LOCK_POLL.min(left));
        match file.try_lock() {
            Ok(()) => return Ok(true),
            Err(fs::TryLockError::WouldBlock) if left.is_zero() => return Ok(false),
            Err(fs::TryLockError::WouldBlock) => {}
            Err(fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// Whether a lock call failed because the filesystem has no locks (some
/// network and FUSE filesystems), rather than for a reason worth stopping
/// on.
fn has_no_locks(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    if let Some(code) = e.raw_os_error() {
        return [libc::ENOLCK, libc::EOPNOTSUPP, libc::ENOTSUP].contains(&code);
    }
    false
}

/// A lock failure that stops the command, naming the sentinel.
fn lock_error(sentinel: &Path, e: std::io::Error) -> CliError {
    CliError::Io(std::io::Error::new(
        e.kind(),
        format!("cannot lock the target store ({}): {e}", sentinel.display()),
    ))
}

/// `config.yaml` at the root of the target store. Tracks the
/// active target name + a schema version for forward-compat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalConfig {
    /// Name of the currently-active target. Operational commands
    /// (`apply`, `cluster-bootstrap`, etc.) act against this
    /// unless explicitly overridden by `--target <name>`.
    pub active_target: String,
    /// On-disk format revision. Bumped when fields are removed or
    /// re-shaped; new fields can be added at the same version as
    /// long as they have serde defaults.
    pub version: u32,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            active_target: "default".into(),
            version: TARGET_STORE_VERSION,
        }
    }
}

/// Per-target firewall toggles (1.83h). Persisted in the target store so a
/// manifest-free `apprafter apply`/`up` honors them. Absent ⇒ defaults off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FirewallConfig {
    /// Restrict the node's 80/443 to Cloudflare's IP ranges (origin firewall, 1.83d).
    pub cloudflare_origin: bool,
}

/// Non-secret target configuration. Stored alongside credentials
/// in the same target directory but in a separate file so a user
/// can inspect / version-control / share `config.yaml` while the
/// secret half stays mode 0600.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetConfig {
    /// Provider identifier — e.g. `hetzner-cloud`. Mirrors the
    /// `Infrastructure.spec.provider` field in the manifest layer
    /// so resolution is straightforward.
    pub provider: String,
    /// Provider-specific region (e.g. Hetzner `nbg1`).
    pub region: Option<String>,
    /// Default tier identifier (`solo` / `team` / `prod` /
    /// `regulated`). Used as a hint by `init` / `bootstrap-all`.
    pub default_tier: Option<String>,
    /// Default cluster name; falls back to `platform-1` when not
    /// set (mirrors current `commands/apply.rs` behaviour).
    pub cluster_name: Option<String>,
    /// Path to the SSH public key used for server provisioning.
    /// Stays a path (not the key body) so the source-of-truth is
    /// the user's `~/.ssh/` and Track A.1 of operator never copies
    /// or re-renders the key material.
    pub ssh_key_path: Option<PathBuf>,
    /// 1.83h: cloud-firewall toggles for this target (e.g. the Cloudflare
    /// origin firewall). `#[serde(default)]` keeps legacy configs loading.
    pub firewall: Option<FirewallConfig>,
    /// 2.16h: preferred Hetzner server type (e.g. `cx22`, `ccx23`). When set,
    /// this participates in the provisioning-axis resolution chain as the
    /// "target preference" rung — below an explicit flag or manifest value,
    /// above the ambient `HCLOUD_SERVER_TYPE` env var. `None` means "not
    /// configured here; continue down the chain". `#[serde(default)]` keeps
    /// pre-2.16h `config.yaml` files loading without the key present.
    pub server_type: Option<String>,
}

/// Secret target credentials. Stored at mode 0600. **Never** derive
/// `Debug` on this struct — accidental `println!("{:?}", creds)`
/// would leak the token. The manual `Debug` impl below redacts.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetCredentials {
    /// Hetzner Cloud API token (`hcloud_...`). `Option` because
    /// future provider plugins may store a different credential
    /// shape (AWS access key, OpenBao token, etc.) and we don't
    /// want a forced-empty field at the top.
    pub hetzner_token: Option<String>,
}

impl std::fmt::Debug for TargetCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TargetCredentials")
            .field(
                "hetzner_token",
                &self.hetzner_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// A single Target — name + non-secret config + secret credentials.
/// Composed in memory for ergonomics; on disk lives across two
/// files (per `cli-dx-task.md` §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub config: TargetConfig,
    pub credentials: TargetCredentials,
}

// ---------------------------------------------------------------
// Global config IO
// ---------------------------------------------------------------

/// Read `<root>/config.yaml`. Returns `Ok(None)` when absent
/// (first-run case); `Err` only on corrupt YAML or filesystem
/// failure so the caller can distinguish "no store yet" from
/// "store broken".
pub fn load_global_config(paths: &TargetStorePaths) -> Result<Option<GlobalConfig>> {
    let path = paths.global_config_file();
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)?;
    let cfg: GlobalConfig =
        serde_yaml::from_slice(&bytes).map_err(|err| CliError::InvalidTargetConfig {
            path: path.clone(),
            message: err.to_string(),
            target: None,
        })?;
    Ok(Some(cfg))
}

/// Write `<root>/config.yaml` atomically. Creates `<root>/` first
/// if missing (and the `auth/` placeholder so the directory tree
/// matches the spec the moment any write happens).
pub fn save_global_config(paths: &TargetStorePaths, cfg: &GlobalConfig) -> Result<()> {
    fs::create_dir_all(paths.root())?;
    ensure_auth_placeholder(paths)?;
    let yaml = serde_yaml::to_string(cfg)?;
    atomic_write(&paths.global_config_file(), yaml.as_bytes(), false)
}

/// Best-effort: which target should non-credential code paths
/// (apply / destroy / import) consult for fallback config like
/// provider / region / cluster_name?
///
/// Returns `Some(name)` when:
///  1. `target_override` is supplied (the `--target` flag), OR
///  2. `GlobalConfig.active_target` exists and is non-empty.
///
/// Returns `None` for a fresh store with no targets — callers
/// then fall through to defaults / errors.
///
/// Mirrors the same precedence the credential resolver uses, so
/// operational commands look at the *same* target for both
/// config and credentials within one invocation.
pub fn resolve_active_target_name(
    paths: &TargetStorePaths,
    target_override: Option<&str>,
) -> Result<Option<String>> {
    if let Some(n) = target_override {
        return Ok(Some(n.to_string()));
    }
    Ok(load_global_config(paths)?
        .map(|g| g.active_target)
        .filter(|s| !s.is_empty()))
}

/// Convenience helper used by `apply`/`destroy`/`import` to load
/// the active target's non-secret config when state.json is empty.
/// Returns `None` (not Err) for any failure — these consumers
/// treat the target store strictly as a fallback, so a broken
/// target file shouldn't take down operational commands that
/// could otherwise succeed from env or state.json. Real errors
/// surface through the credential resolver which IS allowed to
/// fail.
pub fn load_active_target_config(
    paths: &TargetStorePaths,
    target_override: Option<&str>,
) -> Option<TargetConfig> {
    let name = resolve_active_target_name(paths, target_override).ok()??;
    load_target(paths, &name).ok().map(|t| t.config)
}

// ---------------------------------------------------------------
// Per-target IO
// ---------------------------------------------------------------

/// Read one target's `config.yaml` only — never its credentials, so a listing reads no secret.
/// Missing target → `CliError::TargetNotFound` with the names present, as [`load_target`].
pub fn load_target_config(paths: &TargetStorePaths, name: &str) -> Result<TargetConfig> {
    let cfg_path = paths.target_config_file(name);
    if !cfg_path.exists() {
        let available = list_target_names(paths).unwrap_or_default().join(", ");
        return Err(CliError::TargetNotFound {
            name: name.to_string(),
            available,
        });
    }
    let cfg_bytes = fs::read(&cfg_path)?;
    serde_yaml::from_slice(&cfg_bytes).map_err(|err| CliError::InvalidTargetConfig {
        path: cfg_path.clone(),
        message: err.to_string(),
        target: Some(name.to_string()),
    })
}

/// Read both halves (`config.yaml` + `credentials.yaml`) of one
/// target. Missing target → `CliError::TargetNotFound` with the
/// list of names currently present so error messages can be
/// helpful without an extra round-trip.
pub fn load_target(paths: &TargetStorePaths, name: &str) -> Result<Target> {
    let config = load_target_config(paths, name)?;

    let creds_path = paths.target_credentials_file(name);
    let credentials = if creds_path.exists() {
        let bytes = fs::read(&creds_path)?;
        serde_yaml::from_slice::<TargetCredentials>(&bytes).map_err(|err| {
            CliError::InvalidTargetConfig {
                path: creds_path.clone(),
                message: credentials_parse_message(&err),
                target: Some(name.to_string()),
            }
        })?
    } else {
        TargetCredentials::default()
    };

    Ok(Target {
        name: name.to_string(),
        config,
        credentials,
    })
}

/// What a credentials file that does not parse is said to be: where it failed, never serde's
/// own text, which quotes the scalar it could not read — in this file, the token (a hand edit
/// with no space after the colon, `hetzner_token:<token>`, is one plain scalar).
fn credentials_parse_message(err: &serde_yaml::Error) -> String {
    match err.location() {
        Some(at) => format!(
            "not a valid target credentials map (line {}, column {})",
            at.line(),
            at.column()
        ),
        None => "not a valid target credentials map".to_string(),
    }
}

/// Persist both halves of a target. `config.yaml` is mode 0644
/// (world-readable, ok for non-secret data); `credentials.yaml`
/// is mode 0600 on Unix — `atomic_write(..., true)` enforces it
/// before publishing the file under its final name, so there is
/// no race window where the credentials are briefly readable by
/// other local users.
pub fn save_target(paths: &TargetStorePaths, target: &Target) -> Result<()> {
    save_target_config(paths, &target.name, &target.config)?;
    let creds_yaml = serde_yaml::to_string(&target.credentials)?;
    atomic_write(
        &paths.target_credentials_file(&target.name),
        creds_yaml.as_bytes(),
        true,
    )?;
    Ok(())
}

/// Persist a target's `config.yaml` (mode 0644) and nothing else: its `credentials.yaml` is
/// neither read nor written, so a change that keeps the token leaves that file as it was,
/// byte for byte (`target add --renew --ssh-key` without a new token).
pub fn save_target_config(
    paths: &TargetStorePaths,
    name: &str,
    config: &TargetConfig,
) -> Result<()> {
    fs::create_dir_all(paths.target_dir(name))?;
    ensure_auth_placeholder(paths)?;
    let cfg_yaml = serde_yaml::to_string(config)?;
    atomic_write(&paths.target_config_file(name), cfg_yaml.as_bytes(), false)
}

/// Names of every target directory under `<root>/targets/`,
/// sorted lexicographically so callers get a stable order without
/// each one re-sorting. Returns `Ok(vec![])` if the directory
/// doesn't exist yet.
pub fn list_target_names(paths: &TargetStorePaths) -> Result<Vec<String>> {
    let dir = paths.targets_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            // Skip filesystem-level hidden / scratch dirs (`.tmp`,
            // `.DS_Store`-style) so they never end up in error
            // messages or in the `target list` output. Dot-prefix
            // is the standard Unix hidden marker.
            if !name.starts_with('.') {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Atomically rename a target directory (`from` → `to`). Refuses
/// if `from` doesn't exist (`TargetNotFound`) or `to` already
/// exists (typed `Other` so the caller's error message can suggest
/// using a different name without raising the "did you mean..."
/// surface that `TargetNotFound` carries). Best-effort: the
/// per-target state cache (`state/<from>/`) is moved alongside
/// when present.
///
/// Does **not** update `GlobalConfig.active_target` — that lives
/// at the CLI surface where pointer-update policy is decided. The
/// caller is expected to call `save_global_config` after `rename_target`
/// if `active_target == from`.
pub fn rename_target(paths: &TargetStorePaths, from: &str, to: &str) -> Result<()> {
    let from_dir = paths.target_dir(from);
    if !from_dir.exists() {
        let available = list_target_names(paths).unwrap_or_default().join(", ");
        return Err(CliError::TargetNotFound {
            name: from.to_string(),
            available,
        });
    }
    let to_dir = paths.target_dir(to);
    if to_dir.exists() {
        return Err(CliError::Other(format!(
            "target `{to}` already exists — pick a different name or remove the existing target first"
        )));
    }
    fs::rename(&from_dir, &to_dir)?;

    // Move the per-target state cache too, when present. This is
    // best-effort: if state move fails halfway we leave the
    // target rename committed because un-rolling the directory
    // rename would itself create a window where neither path
    // exists.
    let from_state = paths.state_dir(from);
    if from_state.exists() {
        let to_state = paths.state_dir(to);
        // Ensure parent of `state_dir` exists; fs::rename refuses
        // when the parent of the destination is missing.
        if let Some(parent) = to_state.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&from_state, &to_state)?;
    }
    Ok(())
}

/// Remove a target directory and everything under it
/// (`config.yaml`, `credentials.yaml`, and any per-target state
/// caches). Returns `TargetNotFound` if there's nothing to remove
/// — callers that want idempotent delete should match on that
/// variant.
pub fn remove_target(paths: &TargetStorePaths, name: &str) -> Result<()> {
    let dir = paths.target_dir(name);
    if !dir.exists() {
        let available = list_target_names(paths).unwrap_or_default().join(", ");
        return Err(CliError::TargetNotFound {
            name: name.to_string(),
            available,
        });
    }
    fs::remove_dir_all(&dir)?;
    // Best-effort cleanup of the per-target state cache; ignore
    // not-exists since the user may have never run an operation
    // that produced state.
    let state_dir = paths.state_dir(name);
    if state_dir.exists() {
        fs::remove_dir_all(&state_dir)?;
    }
    Ok(())
}

// ---------------------------------------------------------------
// Internal: atomic write + permission enforcement
// ---------------------------------------------------------------

/// Write `bytes` to `final_path` atomically, through
/// [`crate::atomic_replace`]: a temp file in the same directory
/// (`.apprafter-tgt-XXXXXX.tmp`), written, given its mode and fsynced
/// under the temp name, then renamed over `final_path`. Readers see the
/// old file or the new one, never a partial write or an over-permissive
/// mode, and on Windows a reader holding the old file open does not make
/// the replace fail.
///
/// `secret = true` enforces mode 0600 (owner read/write only) on
/// Unix, `false` 0644. On Windows the mode is ignored (NTFS ACLs
/// inherit from the parent; tightening per-file is out of scope
/// for v0.1.72).
fn atomic_write(final_path: &Path, bytes: &[u8], secret: bool) -> Result<()> {
    let parent = final_path.parent().ok_or_else(|| {
        CliError::Other(format!(
            "internal: atomic_write called with rootless path {}",
            final_path.display()
        ))
    })?;
    fs::create_dir_all(parent)?;
    let mode = if secret { 0o600 } else { 0o644 };
    crate::atomic_replace(final_path, bytes, ".apprafter-tgt-", Some(mode))?;
    Ok(())
}

fn ensure_auth_placeholder(paths: &TargetStorePaths) -> Result<()> {
    fs::create_dir_all(paths.auth_dir())?;
    let keep = paths.auth_keep_file();
    if !keep.exists() {
        // Empty marker so the reserved `auth/` directory shows up
        // for users who explore the store, and so future Managed
        // login code can rely on the directory existing.
        fs::write(&keep, b"")?;
    }
    Ok(())
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_paths() -> (tempfile::TempDir, TargetStorePaths) {
        let dir = tempdir().unwrap();
        let paths = TargetStorePaths::for_root(dir.path().to_path_buf());
        (dir, paths)
    }

    #[test]
    fn default_config_root_points_at_user_config_dir_under_apprafter() {
        // Sanity guard: we never accidentally rebase the store on
        // some unrelated dir. Only assert that the leaf segment is
        // `apprafter/` because the parent depends on host XDG.
        let p = default_config_root().expect("dirs::config_dir should work in test env");
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some("apprafter"),
            "expected root to end in apprafter/, got {}",
            p.display()
        );
    }

    #[test]
    fn paths_compose_per_spec_directory_layout() {
        // Pins the on-disk shape against cli-dx-task.md §4 so a
        // rename of one constant doesn't silently drift the spec.
        let (_dir, paths) = make_paths();
        let root = paths.root().to_path_buf();
        assert_eq!(paths.global_config_file(), root.join("config.yaml"));
        assert_eq!(paths.targets_dir(), root.join("targets"));
        assert_eq!(
            paths.target_config_file("work"),
            root.join("targets/work/config.yaml")
        );
        assert_eq!(
            paths.target_credentials_file("work"),
            root.join("targets/work/credentials.yaml")
        );
        assert_eq!(paths.auth_dir(), root.join("auth"));
        assert_eq!(paths.auth_keep_file(), root.join("auth/.keep"));
        assert_eq!(paths.state_dir("work"), root.join("state/work"));
    }

    /// D.3d review #5: serde_yaml's text quotes the scalar it could not read, and in a
    /// credentials file that scalar is the token — a hand edit with no space after the colon
    /// (`hetzner_token:<token>`) is one plain scalar, and so is a bare token line. The error
    /// names the file and where in it, never what it holds; it reaches a terminal, the desktop's
    /// error panel, and doctor's row.
    #[test]
    fn a_credentials_file_that_does_not_parse_is_named_by_its_place_never_its_text() {
        let (_dir, paths) = make_paths();
        let token = "t0k3n".repeat(13);
        save_target(
            &paths,
            &Target {
                name: "prod".into(),
                config: TargetConfig {
                    provider: "hetzner-cloud".into(),
                    ..Default::default()
                },
                credentials: TargetCredentials::default(),
            },
        )
        .unwrap();
        let creds = paths.target_credentials_file("prod");
        for (body, line, column) in [
            (format!("hetzner_token:{token}\n"), 1, 1),
            (format!("{token}\n"), 1, 1),
            (format!("hetzner_token: [{token}]\n"), 1, 16),
            (format!("# a token\nhetzner_token: [{token}\n"), 2, 16),
        ] {
            fs::write(&creds, &body).unwrap();
            let err = load_target(&paths, "prod").expect_err(&body);
            let shown = err.to_string();
            assert!(!shown.contains(&token), "the token is quoted: {shown}");
            match err {
                CliError::InvalidTargetConfig {
                    path,
                    message,
                    target,
                } => {
                    assert_eq!(path, creds);
                    assert_eq!(target.as_deref(), Some("prod"), "the file's target");
                    assert_eq!(
                        message,
                        format!(
                            "not a valid target credentials map (line {line}, column {column})"
                        ),
                        "{body}"
                    );
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn load_global_config_returns_none_on_fresh_store() {
        let (_dir, paths) = make_paths();
        let loaded = load_global_config(&paths).expect("missing file is not an error");
        assert!(loaded.is_none());
    }

    /// A config-only save rewrites `config.yaml` and leaves `credentials.yaml` exactly as it
    /// was, a hand-written comment included, and creates none where there was none.
    #[test]
    fn a_config_only_save_never_touches_the_credentials_file() {
        let (_dir, paths) = make_paths();
        let config = |key: &str| TargetConfig {
            provider: "hetzner-cloud".into(),
            ssh_key_path: Some(key.into()),
            ..Default::default()
        };
        save_target_config(&paths, "work", &config("/k/old.pub")).unwrap();
        assert!(!paths.target_credentials_file("work").exists());
        let creds = "# rotated by hand\nhetzner_token: 'abc'\n";
        fs::write(paths.target_credentials_file("work"), creds).unwrap();
        save_target_config(&paths, "work", &config("/k/new.pub")).unwrap();
        assert_eq!(
            fs::read_to_string(paths.target_credentials_file("work")).unwrap(),
            creds
        );
        assert_eq!(
            load_target_config(&paths, "work").unwrap().ssh_key_path,
            Some("/k/new.pub".into())
        );
    }

    #[test]
    fn save_then_load_global_round_trips_active_target() {
        let (_dir, paths) = make_paths();
        let cfg = GlobalConfig {
            active_target: "work".into(),
            version: TARGET_STORE_VERSION,
        };
        save_global_config(&paths, &cfg).expect("save ok");
        let loaded = load_global_config(&paths)
            .expect("load ok")
            .expect("present");
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn save_global_creates_auth_placeholder_directory() {
        // The reserved `auth/` directory must appear on first
        // write so future Managed-login code can rely on it
        // existing.
        let (_dir, paths) = make_paths();
        save_global_config(&paths, &GlobalConfig::default()).expect("save ok");
        assert!(paths.auth_dir().is_dir(), "auth/ must exist after save");
        assert!(
            paths.auth_keep_file().exists(),
            "auth/.keep marker must exist after save"
        );
    }

    #[test]
    fn load_global_config_returns_invalid_target_config_on_corrupt_yaml() {
        let (_dir, paths) = make_paths();
        fs::create_dir_all(paths.root()).unwrap();
        fs::write(paths.global_config_file(), b"not: valid: yaml: : :").unwrap();
        let err = load_global_config(&paths).expect_err("corrupt yaml must error");
        match err {
            CliError::InvalidTargetConfig { path, target, .. } => {
                assert_eq!(path, paths.global_config_file());
                assert_eq!(target, None, "the store's own file belongs to no target");
            }
            other => panic!("expected InvalidTargetConfig, got {other:?}"),
        }
    }

    /// A target's own `config.yaml` that does not parse names its target, so the help can offer
    /// re-adding that target (the store's `config.yaml` names none).
    #[test]
    fn a_corrupt_target_config_names_its_target() {
        let (_dir, paths) = make_paths();
        fs::create_dir_all(paths.target_dir("prod")).unwrap();
        fs::write(paths.target_config_file("prod"), b"not: valid: yaml: : :").unwrap();
        for err in [
            load_target_config(&paths, "prod").unwrap_err(),
            load_target(&paths, "prod").unwrap_err(),
        ] {
            match err {
                CliError::InvalidTargetConfig { path, target, .. } => {
                    assert_eq!(path, paths.target_config_file("prod"));
                    assert_eq!(target.as_deref(), Some("prod"));
                }
                other => panic!("expected InvalidTargetConfig, got {other:?}"),
            }
        }
    }

    #[test]
    fn save_then_load_target_round_trips_both_halves() {
        let (_dir, paths) = make_paths();
        let target = Target {
            name: "default".into(),
            config: TargetConfig {
                provider: "hetzner-cloud".into(),
                region: Some("nbg1".into()),
                default_tier: Some("solo".into()),
                cluster_name: Some("platform-1".into()),
                ssh_key_path: Some(PathBuf::from("/home/me/.ssh/id_ed25519.pub")),
                firewall: Some(FirewallConfig {
                    cloudflare_origin: true,
                }),
                server_type: None,
            },
            credentials: TargetCredentials {
                hetzner_token: Some(
                    "hcloud_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into(),
                ),
            },
        };
        save_target(&paths, &target).expect("save ok");
        let loaded = load_target(&paths, "default").expect("load ok");
        assert_eq!(loaded, target);
    }

    #[test]
    fn load_target_returns_target_not_found_with_available_list() {
        let (_dir, paths) = make_paths();
        // Populate two targets so the error message has something
        // to suggest.
        for name in ["default", "work"] {
            let t = Target {
                name: name.into(),
                config: TargetConfig {
                    provider: "hetzner-cloud".into(),
                    ..Default::default()
                },
                credentials: TargetCredentials::default(),
            };
            save_target(&paths, &t).expect("save ok");
        }
        let err = load_target(&paths, "personal").expect_err("missing target must error");
        match err {
            CliError::TargetNotFound { name, available } => {
                assert_eq!(name, "personal");
                assert_eq!(available, "default, work");
            }
            other => panic!("expected TargetNotFound, got {other:?}"),
        }
    }

    #[test]
    fn load_target_tolerates_missing_credentials_file() {
        // Credentials file may legitimately be absent (e.g. a user
        // pulled config.yaml from dotfiles but hasn't run
        // `target add --renew` yet). Loader must return a target
        // with empty credentials, not error.
        let (_dir, paths) = make_paths();
        fs::create_dir_all(paths.target_dir("dotfiles-only")).unwrap();
        let cfg = TargetConfig {
            provider: "hetzner-cloud".into(),
            region: Some("nbg1".into()),
            ..Default::default()
        };
        fs::write(
            paths.target_config_file("dotfiles-only"),
            serde_yaml::to_string(&cfg).unwrap(),
        )
        .unwrap();
        let loaded = load_target(&paths, "dotfiles-only").expect("load ok");
        assert_eq!(loaded.config, cfg);
        assert!(loaded.credentials.hetzner_token.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn credentials_file_lands_at_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, paths) = make_paths();
        let target = Target {
            name: "work".into(),
            config: TargetConfig {
                provider: "hetzner-cloud".into(),
                ..Default::default()
            },
            credentials: TargetCredentials {
                hetzner_token: Some("hcloud_secret".into()),
            },
        };
        save_target(&paths, &target).expect("save ok");
        let mode = fs::metadata(paths.target_credentials_file("work"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "credentials.yaml must be 0600 (owner-only), got {mode:o}"
        );
        // Sibling config.yaml is non-secret; allow group/world
        // readable (0644 is what the umask gives us).
        let cfg_mode = fs::metadata(paths.target_config_file("work"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            cfg_mode, 0o644,
            "config.yaml must be 0644 (group/world readable), got {cfg_mode:o}"
        );
    }

    #[test]
    fn the_store_lock_is_exclusive_across_handles() {
        let dir = tempfile::tempdir().unwrap();
        let paths = TargetStorePaths::for_root(dir.path().to_path_buf());
        let held = StoreLock::exclusive(&paths).unwrap();
        let other = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(paths.root().join(".lock"))
            .unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(held);
        other.try_lock().expect("free once the holder drops");
    }

    #[test]
    fn the_store_lock_creates_the_root_and_its_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("fresh");
        let paths = TargetStorePaths::for_root(root.clone());
        let _g = StoreLock::exclusive(&paths).unwrap();
        assert!(root.join(".lock").is_file());
    }

    #[test]
    fn the_sentinel_is_not_a_target() {
        let dir = tempfile::tempdir().unwrap();
        let paths = TargetStorePaths::for_root(dir.path().to_path_buf());
        let _g = StoreLock::exclusive(&paths).unwrap();
        assert!(list_target_names(&paths).unwrap().is_empty());
    }

    /// What a [`StoreLockEvent`] said, owned, so a test can compare it.
    #[derive(Debug, PartialEq, Eq)]
    enum Seen {
        Waiting(PathBuf),
        Unlocked(PathBuf, std::io::ErrorKind),
    }

    fn seen(event: StoreLockEvent<'_>) -> Seen {
        match event {
            StoreLockEvent::Waiting { sentinel } => Seen::Waiting(sentinel.to_path_buf()),
            StoreLockEvent::Unlocked { sentinel, error } => {
                Seen::Unlocked(sentinel.to_path_buf(), error.kind())
            }
        }
    }

    /// Hold the store lock on another thread until the returned sender
    /// sends (or drops). Returns once the lock is held.
    fn hold_elsewhere(
        paths: &TargetStorePaths,
    ) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let paths = paths.clone();
        let holder = std::thread::spawn(move || {
            let _lock = StoreLock::exclusive(&paths).unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        held_rx.recv().unwrap();
        (release_tx, holder)
    }

    #[test]
    fn a_free_lock_is_taken_without_a_word() {
        let (_dir, paths) = make_paths();
        let mut events = Vec::new();
        let lock = StoreLock::exclusive_or_wait(&paths, |e| events.push(seen(e))).unwrap();
        assert!(lock.is_held());
        assert!(events.is_empty(), "{events:?}");
    }

    /// The release happens inside the callback, so the test proves the
    /// order — "waiting" is reported before the wait — without a sleep.
    #[test]
    fn a_held_lock_is_reported_once_and_then_waited_for() {
        let (_dir, paths) = make_paths();
        let (release, holder) = hold_elsewhere(&paths);
        let mut events = Vec::new();
        let lock = StoreLock::exclusive_or_wait(&paths, |e| {
            events.push(seen(e));
            release.send(()).unwrap();
        })
        .unwrap();
        holder.join().unwrap();
        assert!(lock.is_held());
        assert_eq!(events, vec![Seen::Waiting(paths.lock_file())]);
    }

    #[test]
    fn try_exclusive_for_gives_up_on_a_lock_held_past_its_bound() {
        let (_dir, paths) = make_paths();
        let (release, holder) = hold_elsewhere(&paths);
        let mut events = Vec::new();
        let started = std::time::Instant::now();
        let got =
            StoreLock::try_exclusive_for(&paths, std::time::Duration::from_millis(200), |e| {
                events.push(seen(e))
            })
            .unwrap();
        assert!(got.is_none(), "the lock was held throughout");
        assert!(started.elapsed() >= std::time::Duration::from_millis(200));
        assert_eq!(events, vec![Seen::Waiting(paths.lock_file())]);
        release.send(()).unwrap();
        holder.join().unwrap();
        // Giving up holds nothing on this thread: the next lock is fine.
        let again = StoreLock::exclusive(&paths).unwrap();
        assert!(again.is_held());
    }

    #[test]
    fn try_exclusive_for_takes_a_lock_released_within_its_bound() {
        let (_dir, paths) = make_paths();
        let (release, holder) = hold_elsewhere(&paths);
        let got = StoreLock::try_exclusive_for(&paths, std::time::Duration::from_secs(30), |_| {
            release.send(()).unwrap()
        })
        .unwrap()
        .expect("released after the first try");
        holder.join().unwrap();
        assert!(got.is_held());
    }

    /// A second lock on one thread would wait for the first forever:
    /// `flock` and `LockFileEx` see the second handle as a stranger.
    #[test]
    fn a_second_lock_on_the_same_thread_panics_instead_of_deadlocking() {
        let (_dir, paths) = make_paths();
        let first = StoreLock::exclusive(&paths).unwrap();
        let nested = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            StoreLock::exclusive(&paths).map(|_| ())
        }));
        let payload = nested.expect_err("a nested lock must panic");
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(
            message.contains("one StoreLock per thread"),
            "the panic names the rule: {message:?}"
        );
        let bounded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            StoreLock::try_exclusive_for(&paths, std::time::Duration::ZERO, |_| {}).map(|_| ())
        }));
        assert!(bounded.is_err(), "the bounded form refuses as well");
        drop(first);
        let after = StoreLock::exclusive(&paths).unwrap();
        assert!(after.is_held(), "released, the lock can be taken again");
    }

    #[test]
    fn locks_on_other_threads_do_not_count_as_nested() {
        let (_dir, paths) = make_paths();
        let (release, holder) = hold_elsewhere(&paths);
        release.send(()).unwrap();
        holder.join().unwrap();
        let mine = StoreLock::exclusive(&paths).unwrap();
        assert!(mine.is_held());
    }

    #[test]
    fn a_filesystem_without_locks_is_recognised() {
        use std::io::{Error, ErrorKind};
        assert!(has_no_locks(&Error::from(ErrorKind::Unsupported)));
        assert!(!has_no_locks(&Error::from(ErrorKind::PermissionDenied)));
        assert!(!has_no_locks(&Error::other("anything else")));
        #[cfg(unix)]
        for code in [libc::ENOLCK, libc::EOPNOTSUPP, libc::ENOTSUP] {
            assert!(has_no_locks(&Error::from_raw_os_error(code)), "{code}");
        }
        #[cfg(unix)]
        assert!(!has_no_locks(&Error::from_raw_os_error(libc::EIO)));
    }

    #[test]
    fn any_other_failure_names_the_sentinel() {
        // The root is a file: the store directory cannot be created.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("not-a-dir");
        fs::write(&root, b"x").unwrap();
        let paths = TargetStorePaths::for_root(root);
        let mut events = Vec::new();
        let err = StoreLock::exclusive_or_wait(&paths, |e| events.push(seen(e)))
            .map(|_| ())
            .expect_err("a file where the store goes");
        let sentinel = paths.lock_file().display().to_string();
        assert!(matches!(err, CliError::Io(_)), "{err:?}");
        assert!(err.to_string().contains(&sentinel), "{err}");
        assert!(events.is_empty(), "an error, not a warning: {events:?}");
    }

    /// `flock` needs no write access: a sentinel this process may only
    /// read is still locked (true as root too, who may write it anyway).
    #[cfg(unix)]
    #[test]
    fn a_read_only_sentinel_is_still_locked() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, paths) = make_paths();
        drop(StoreLock::exclusive(&paths).unwrap());
        fs::set_permissions(paths.lock_file(), fs::Permissions::from_mode(0o444)).unwrap();
        let mut events = Vec::new();
        let lock = StoreLock::exclusive_or_wait(&paths, |e| events.push(seen(e))).unwrap();
        assert!(lock.is_held());
        assert!(events.is_empty(), "{events:?}");
        let other = fs::File::open(paths.lock_file()).unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
    }

    /// Restores a directory's mode on drop, so a failed test still lets
    /// its `TempDir` clean up.
    #[cfg(unix)]
    struct ModeGuard(PathBuf);

    #[cfg(unix)]
    impl Drop for ModeGuard {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }

    /// A store this process cannot write, with no sentinel in it yet: there
    /// is nothing to lock, so the guard holds nothing and says why.
    ///
    /// Skipped — loudly — where the directory mode does not bind this
    /// process (running as root): the probe write below then succeeds.
    #[cfg(unix)]
    #[test]
    fn a_store_that_cannot_hold_a_sentinel_is_used_unlocked_with_a_warning() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, paths) = make_paths();
        save_global_config(&paths, &GlobalConfig::default()).unwrap();
        fs::set_permissions(paths.root(), fs::Permissions::from_mode(0o555)).unwrap();
        let _restore = ModeGuard(paths.root().to_path_buf());
        if fs::write(paths.root().join("probe"), b"").is_ok() {
            eprintln!(
                "SKIPPED a_store_that_cannot_hold_a_sentinel_is_used_unlocked_with_a_warning: \
                 a 0555 directory is writable to this process (root?)"
            );
            return;
        }
        let mut events = Vec::new();
        let lock = StoreLock::exclusive_or_wait(&paths, |e| events.push(seen(e))).unwrap();
        assert!(!lock.is_held());
        assert_eq!(
            events,
            vec![Seen::Unlocked(
                paths.lock_file(),
                std::io::ErrorKind::PermissionDenied
            )]
        );
        // The silent form gives the same guard.
        drop(lock);
        assert!(!StoreLock::exclusive(&paths).unwrap().is_held());
    }

    #[test]
    fn list_target_names_returns_empty_on_fresh_store() {
        let (_dir, paths) = make_paths();
        assert!(list_target_names(&paths).unwrap().is_empty());
    }

    #[test]
    fn list_target_names_returns_sorted_names_skipping_dot_dirs() {
        let (_dir, paths) = make_paths();
        for name in ["zeta", "alpha", "midline"] {
            fs::create_dir_all(paths.target_dir(name)).unwrap();
        }
        // Atomic-write temp leftover that should NOT leak into
        // user-facing listings.
        fs::create_dir_all(paths.targets_dir().join(".scratch")).unwrap();
        let names = list_target_names(&paths).unwrap();
        assert_eq!(names, vec!["alpha", "midline", "zeta"]);
    }

    #[test]
    fn remove_target_deletes_both_files_and_state_dir() {
        let (_dir, paths) = make_paths();
        let target = Target {
            name: "scratch".into(),
            config: TargetConfig {
                provider: "hetzner-cloud".into(),
                ..Default::default()
            },
            credentials: TargetCredentials::default(),
        };
        save_target(&paths, &target).unwrap();
        // Simulate a per-target state cache that Track A.8 will
        // populate.
        fs::create_dir_all(paths.state_dir("scratch")).unwrap();
        fs::write(paths.state_dir("scratch").join("state.json"), b"{}").unwrap();

        remove_target(&paths, "scratch").expect("remove ok");
        assert!(!paths.target_dir("scratch").exists());
        assert!(!paths.state_dir("scratch").exists());
    }

    #[test]
    fn remove_target_returns_target_not_found_when_missing() {
        let (_dir, paths) = make_paths();
        let err = remove_target(&paths, "ghost").expect_err("missing target");
        assert!(matches!(err, CliError::TargetNotFound { .. }));
    }

    #[test]
    fn rename_target_moves_dir_and_per_target_state_cache() {
        let (_dir, paths) = make_paths();
        let target = Target {
            name: "old".into(),
            config: TargetConfig {
                provider: "hetzner-cloud".into(),
                region: Some("nbg1".into()),
                ..Default::default()
            },
            credentials: TargetCredentials::default(),
        };
        save_target(&paths, &target).unwrap();
        // Simulate the per-target state cache that Track A.8
        // wires through.
        fs::create_dir_all(paths.state_dir("old")).unwrap();
        fs::write(paths.state_dir("old").join("state.json"), b"{}").unwrap();

        rename_target(&paths, "old", "new").expect("rename ok");

        assert!(!paths.target_dir("old").exists(), "old target dir gone");
        assert!(paths.target_dir("new").exists(), "new target dir exists");
        assert!(!paths.state_dir("old").exists(), "old state cache gone");
        assert!(
            paths.state_dir("new").join("state.json").exists(),
            "per-target state moved along"
        );

        // Loading by the new name surfaces the same config that
        // was saved under the old name.
        let loaded = load_target(&paths, "new").expect("load by new name");
        assert_eq!(loaded.config.region.as_deref(), Some("nbg1"));
    }

    #[test]
    fn rename_target_returns_target_not_found_when_source_missing() {
        let (_dir, paths) = make_paths();
        // Have one existing target so the "available" list is
        // non-empty and the error message is helpful.
        save_target(
            &paths,
            &Target {
                name: "existing".into(),
                config: TargetConfig {
                    provider: "hetzner-cloud".into(),
                    ..Default::default()
                },
                credentials: TargetCredentials::default(),
            },
        )
        .unwrap();
        let err = rename_target(&paths, "ghost", "new").expect_err("missing source");
        match err {
            CliError::TargetNotFound { name, available } => {
                assert_eq!(name, "ghost");
                assert_eq!(available, "existing");
            }
            other => panic!("expected TargetNotFound, got {other:?}"),
        }
    }

    #[test]
    fn rename_target_refuses_when_destination_exists() {
        let (_dir, paths) = make_paths();
        for n in ["alpha", "beta"] {
            save_target(
                &paths,
                &Target {
                    name: n.into(),
                    config: TargetConfig {
                        provider: "hetzner-cloud".into(),
                        ..Default::default()
                    },
                    credentials: TargetCredentials::default(),
                },
            )
            .unwrap();
        }
        let err = rename_target(&paths, "alpha", "beta").expect_err("dest collision");
        match err {
            CliError::Other(msg) => assert!(msg.contains("already exists"), "{msg}"),
            other => panic!("expected Other, got {other:?}"),
        }
        // Both targets remain intact — no half-rename damage.
        assert!(paths.target_dir("alpha").exists());
        assert!(paths.target_dir("beta").exists());
    }

    #[test]
    fn rename_target_works_when_no_state_cache_present() {
        // The state cache is best-effort; if it doesn't exist
        // (i.e. operator hasn't run any commands that drop state
        // under the target yet), rename should still succeed
        // without trying to touch the missing dir.
        let (_dir, paths) = make_paths();
        save_target(
            &paths,
            &Target {
                name: "fresh".into(),
                config: TargetConfig {
                    provider: "hetzner-cloud".into(),
                    ..Default::default()
                },
                credentials: TargetCredentials::default(),
            },
        )
        .unwrap();
        assert!(!paths.state_dir("fresh").exists());
        rename_target(&paths, "fresh", "minted").expect("rename ok");
        assert!(paths.target_dir("minted").exists());
    }

    #[test]
    fn credentials_debug_redacts_token() {
        let creds = TargetCredentials {
            hetzner_token: Some("hcloud_supersecret_token_value".into()),
        };
        let dbg = format!("{:?}", creds);
        assert!(
            !dbg.contains("hcloud_supersecret_token_value"),
            "Debug must redact tokens, got: {dbg}"
        );
        assert!(
            dbg.contains("<redacted>"),
            "Debug must show the <redacted> marker so a stray println! is visible in logs: {dbg}"
        );
    }

    #[test]
    fn default_config_root_honours_apprafter_config_dir_env_override() {
        // Serialise with the crate-wide test mutex — sibling
        // tests in `credentials::tests` flip the same env var.
        let _guard = crate::TEST_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let prior = std::env::var(CONFIG_DIR_ENV).ok();
        std::env::set_var(CONFIG_DIR_ENV, dir.path());
        let resolved = default_config_root().expect("env override should resolve");
        match prior {
            Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
            None => std::env::remove_var(CONFIG_DIR_ENV),
        }
        assert_eq!(
            resolved,
            dir.path(),
            "env override must be used verbatim, no `apprafter/` suffix appended"
        );
    }

    #[test]
    fn default_config_root_ignores_empty_env_override() {
        // Empty string is treated as "unset" so users who do
        // `APPRAFTER_CONFIG_DIR= apprafter target list` aren't
        // accidentally pointed at the current working directory.
        let _guard = crate::TEST_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var(CONFIG_DIR_ENV).ok();
        std::env::set_var(CONFIG_DIR_ENV, "");
        let resolved = default_config_root().expect("dirs::config_dir is set in test env");
        match prior {
            Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
            None => std::env::remove_var(CONFIG_DIR_ENV),
        }
        assert_eq!(
            resolved.file_name().and_then(|s| s.to_str()),
            Some("apprafter"),
            "empty env override must fall through to dirs::config_dir().join(apprafter), got {}",
            resolved.display()
        );
    }

    #[test]
    fn validate_hetzner_token_format_accepts_canonical_64_char_token() {
        // Canonical Hetzner Cloud Console token shape: 64 ASCII
        // alphanumeric, no prefix. Without this case passing the
        // CLI rejected every real-world token (v0.1.74 regression
        // fix — v0.1.73 had wrongly required an `hcloud_` prefix
        // that Hetzner doesn't actually issue).
        let token = "a".repeat(64);
        assert!(validate_hetzner_token_format(&token).is_ok(), "{token}");
    }

    #[test]
    fn validate_hetzner_token_format_rejects_wrong_length() {
        // Strict equality on length — 63 / 65 / way-too-short /
        // way-too-long all fail with the same canonical "are 64
        // chars" error message.
        for len in [5usize, 63, 65, 200] {
            let token = "a".repeat(len);
            let err = validate_hetzner_token_format(&token).expect_err("wrong length must fail");
            assert!(err.contains("64"), "len={len}: {err}");
        }
    }

    #[test]
    fn validate_hetzner_token_format_rejects_non_alphanumeric_at_correct_length() {
        // 63 'a' + '-' = 64 chars; length check passes, alphanumeric
        // branch fires.
        let body = "a".repeat(63);
        let token = format!("{body}-");
        assert_eq!(token.len(), 64);
        let err = validate_hetzner_token_format(&token).expect_err("dash is non-alphanumeric");
        assert!(err.contains("alphanumeric"), "{err}");
    }

    #[test]
    fn validate_hetzner_token_format_rejects_underscore_at_correct_length() {
        // Regression-guard for v0.1.74. v0.1.73 wrongly required an
        // `hcloud_` prefix; this test pins the strict alphanumeric
        // rule so that "let's accept underscores" can't sneak in
        // silently. Hetzner's real tokens have no underscores. The
        // string here is exactly 64 chars so the length check passes
        // and the alphanumeric branch is what fires.
        let body = "a".repeat(57);
        let token = format!("hcloud_{body}");
        assert_eq!(token.len(), 64);
        let err =
            validate_hetzner_token_format(&token).expect_err("underscore is non-alphanumeric");
        assert!(err.contains("alphanumeric"), "{err}");
    }

    #[test]
    fn target_config_without_server_type_loads_none() {
        // A pre-2.16h config.yaml that has no `server_type:` key must
        // still deserialise cleanly — the field defaults to None.
        let y = "provider: hetzner-cloud\nregion: nbg1\n";
        let c: TargetConfig = serde_yaml::from_str(y).unwrap();
        assert!(c.server_type.is_none());
    }

    #[test]
    fn target_config_roundtrips_server_type() {
        let c = TargetConfig {
            provider: "hetzner-cloud".into(),
            server_type: Some("ccx23".into()),
            ..Default::default()
        };
        let y = serde_yaml::to_string(&c).unwrap();
        let back: TargetConfig = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back.server_type.as_deref(), Some("ccx23"));
    }

    #[test]
    fn atomic_write_leaves_no_temp_files_on_success() {
        let (_dir, paths) = make_paths();
        save_global_config(&paths, &GlobalConfig::default()).unwrap();
        // Scan the root dir for any leftover `.apprafter-tgt-*`
        // tempfiles. The persist() call should have renamed them
        // all into place.
        let leftovers: Vec<_> = fs::read_dir(paths.root())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".apprafter-tgt-")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "expected no leftover tempfiles, found {leftovers:?}"
        );
    }

    #[test]
    fn target_config_firewall_round_trips() {
        let cfg = TargetConfig {
            provider: "hetzner-cloud".into(),
            firewall: Some(FirewallConfig {
                cloudflare_origin: true,
            }),
            ..Default::default()
        };
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        let back: TargetConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(
            back.firewall,
            Some(FirewallConfig {
                cloudflare_origin: true
            })
        );
    }

    #[test]
    fn target_config_without_firewall_key_defaults_to_none() {
        // A legacy config.yaml (pre-1.83h) has no `firewall:` key.
        let legacy = "provider: hetzner-cloud\n";
        let cfg: TargetConfig = serde_yaml::from_str(legacy).unwrap();
        assert_eq!(cfg.firewall, None);
    }

    #[test]
    fn config_root_from_override_uses_a_non_empty_value_verbatim() {
        let root = config_root_from_override(Some("/tmp/somewhere".to_string())).unwrap();
        assert_eq!(root, PathBuf::from("/tmp/somewhere"));
    }

    #[test]
    fn config_root_from_override_treats_empty_as_unset() {
        let root = config_root_from_override(Some(String::new())).unwrap();
        assert!(
            root.ends_with("apprafter"),
            "fell back to the platform dir, got {root:?}"
        );
    }

    #[test]
    fn config_root_from_override_without_value_uses_the_platform_dir() {
        let root = config_root_from_override(None).unwrap();
        assert!(root.ends_with("apprafter"), "got {root:?}");
    }

    #[test]
    fn the_config_loads_without_reading_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let paths = TargetStorePaths::for_root(dir.path().to_path_buf());
        save_target(
            &paths,
            &Target {
                name: "prod".into(),
                config: TargetConfig {
                    provider: "hetzner-cloud".into(),
                    ..Default::default()
                },
                credentials: TargetCredentials::default(),
            },
        )
        .unwrap();
        std::fs::write(paths.target_credentials_file("prod"), "{not yaml").unwrap();
        assert_eq!(
            load_target_config(&paths, "prod").unwrap().provider,
            "hetzner-cloud"
        );
        assert!(
            load_target(&paths, "prod").is_err(),
            "the full load still reads credentials"
        );
        assert!(matches!(
            load_target_config(&paths, "ghost"),
            Err(CliError::TargetNotFound { .. })
        ));
    }
}
