// SPDX-License-Identifier: FSL-1.1-MIT
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
use std::io::Write;
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
    if let Ok(custom) = std::env::var(CONFIG_DIR_ENV) {
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
    const EXPECTED_LEN: usize = 64;
    if token.len() != EXPECTED_LEN {
        return Err(format!(
            "Hetzner Cloud tokens are {EXPECTED_LEN} ASCII alphanumeric characters; got {}",
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

// ---------------------------------------------------------------
// Per-target IO
// ---------------------------------------------------------------

/// Read both halves (`config.yaml` + `credentials.yaml`) of one
/// target. Missing target → `CliError::TargetNotFound` with the
/// list of names currently present so error messages can be
/// helpful without an extra round-trip.
pub fn load_target(paths: &TargetStorePaths, name: &str) -> Result<Target> {
    let cfg_path = paths.target_config_file(name);
    if !cfg_path.exists() {
        let available = list_target_names(paths).unwrap_or_default().join(", ");
        return Err(CliError::TargetNotFound {
            name: name.to_string(),
            available,
        });
    }
    let cfg_bytes = fs::read(&cfg_path)?;
    let config: TargetConfig =
        serde_yaml::from_slice(&cfg_bytes).map_err(|err| CliError::InvalidTargetConfig {
            path: cfg_path.clone(),
            message: err.to_string(),
        })?;

    let creds_path = paths.target_credentials_file(name);
    let credentials = if creds_path.exists() {
        let bytes = fs::read(&creds_path)?;
        serde_yaml::from_slice::<TargetCredentials>(&bytes).map_err(|err| {
            CliError::InvalidTargetConfig {
                path: creds_path.clone(),
                message: err.to_string(),
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

/// Persist both halves of a target. `config.yaml` is mode 0644
/// (world-readable, ok for non-secret data); `credentials.yaml`
/// is mode 0600 on Unix — `atomic_write(..., true)` enforces it
/// before publishing the file under its final name, so there is
/// no race window where the credentials are briefly readable by
/// other local users.
pub fn save_target(paths: &TargetStorePaths, target: &Target) -> Result<()> {
    fs::create_dir_all(paths.target_dir(&target.name))?;
    ensure_auth_placeholder(paths)?;

    let cfg_yaml = serde_yaml::to_string(&target.config)?;
    atomic_write(
        &paths.target_config_file(&target.name),
        cfg_yaml.as_bytes(),
        false,
    )?;

    let creds_yaml = serde_yaml::to_string(&target.credentials)?;
    atomic_write(
        &paths.target_credentials_file(&target.name),
        creds_yaml.as_bytes(),
        true,
    )?;
    Ok(())
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

/// Write `bytes` to `final_path` atomically. The pattern is the
/// standard recipe: create a tempfile in the same directory,
/// write + fsync + close, set the desired mode while still under
/// the tempfile name, then `rename(2)` over `final_path`. The
/// rename is atomic on POSIX and on NTFS, so readers either see
/// the old file or the new file, never a partial write or an
/// over-permissive mode.
///
/// `secret = true` enforces mode 0600 (owner read/write only) on
/// Unix. On Windows the mode argument is ignored (NTFS ACLs
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

    let mut tmp = tempfile::Builder::new()
        .prefix(".apprafter-tgt-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file_mut().sync_all()?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if secret { 0o600 } else { 0o644 };
        let perms = fs::Permissions::from_mode(mode);
        fs::set_permissions(tmp.path(), perms)?;
    }
    #[cfg(not(unix))]
    {
        // Windows: mode arg ignored. Suppress unused-var lint.
        let _ = secret;
    }

    // `persist` does an atomic rename. If the destination exists
    // it is replaced atomically on POSIX (and via ReplaceFileW on
    // Windows).
    tmp.persist(final_path).map_err(|e| CliError::Io(e.error))?;
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

    #[test]
    fn load_global_config_returns_none_on_fresh_store() {
        let (_dir, paths) = make_paths();
        let loaded = load_global_config(&paths).expect("missing file is not an error");
        assert!(loaded.is_none());
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
            CliError::InvalidTargetConfig { path, .. } => {
                assert_eq!(path, paths.global_config_file());
            }
            other => panic!("expected InvalidTargetConfig, got {other:?}"),
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
        // Save the user's real env so we don't pollute it across
        // test threads — cargo test runs tests in parallel by
        // default, so set + read inside a single test function and
        // restore immediately. We use a unique tempdir so collisions
        // between concurrent test invocations are impossible.
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
}
