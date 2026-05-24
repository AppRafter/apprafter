// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Local runtime state for `apprafter`.
//!
//! ## Storage location — v0.1.154 migration
//!
//! State now lives **per-target** under the user's config
//! directory:
//!
//! ```text
//! $XDG_CONFIG_HOME/apprafter/state/<target>/state.json
//! $XDG_CONFIG_HOME/apprafter/state/<target>/known_hosts
//! ```
//!
//! Up through v0.1.153 the same data lived **per-cwd** at
//! `<cwd>/.apprafter/state.json`. That layout was hostile to the
//! day-to-day operator flow: running `apprafter apply` from the
//! repo root and `apprafter app add` from `landing/cms/` produced
//! "state has no hetzner_cloud section" because the second
//! invocation's cwd had its own (empty) `.apprafter/` instead of
//! seeing the one from the apply. Per-target state pins the cache
//! to the deployment target the operator already names with
//! `apprafter target use <name>`, and pluralises cleanly when an
//! operator runs `dev` and `prod` deployments side-by-side.
//!
//! ## Constructors
//!
//! - [`StatePaths::for_active_target`] — the canonical factory.
//!   Pass the resolved `TargetStorePaths` + the active target name
//!   (typically from `cli_core::target::resolve_active_target_name`).
//!   Used by every operational command (`apply`, `destroy`,
//!   `kubeconfig`, `app add`, …).
//!
//! - [`StatePaths::for_root`] — legacy, kept so tests and the few
//!   call sites that still want a hand-rooted layout
//!   (`migrate_legacy_state_if_present`'s "from" side, internal
//!   tests) compile without churn. New code should prefer
//!   `for_active_target`.
//!
//! ## One-shot migration
//!
//! [`migrate_legacy_state_if_present`] is called before every read
//! site (see `platform-cli/src/commands/state_paths.rs`). On first
//! invocation after the v0.1.154 upgrade it moves a legacy
//! `<cwd>/.apprafter/state.json` (and sibling `known_hosts`) into
//! the new per-target directory, printing a single notice on
//! stderr. Subsequent runs are no-ops because the legacy file is
//! gone. New-wins semantics: if the per-target dir already has a
//! `state.json`, the legacy file is left untouched (operator may
//! reuse the directory under a different target on their next
//! shell).
//!
//! Encoding stays JSON for now — switching to CUE-encoded text
//! lands in phase 1.x.

use std::path::{Path, PathBuf};

use cli_core::{target::TargetStorePaths, CliError, Result, Tier};
use serde::{Deserialize, Serialize};

const STATE_DIR: &str = ".apprafter";
const STATE_FILE: &str = "state.json";
const KNOWN_HOSTS_FILE: &str = "known_hosts";

/// Locator for the files that make up one target's runtime state
/// cache. Carries the parent directory rather than touching the
/// filesystem at construction, so tests can point it at a
/// `tempfile::TempDir` and `for_active_target` round-trips work
/// without writing to the operator's real `~/.config/apprafter/`.
///
/// Two equivalent shapes live in here:
///
/// * Per-target (production): `root = <store>/state/<target>`,
///   then `state_dir() = <store>/state/<target>/.apprafter`,
///   `state_file() = <store>/state/<target>/.apprafter/state.json`.
///   The intermediate `.apprafter` segment is kept so the on-disk
///   shape mirrors the legacy `.apprafter/state.json` operators
///   already grep for; only the parent path moved.
///
/// * Hand-rooted (tests + legacy migration source): `root =
///   <some dir>`, same `.apprafter/state.json` shape inside.
#[derive(Debug, Clone)]
pub struct StatePaths {
    root: PathBuf,
}

impl StatePaths {
    /// Legacy hand-rooted constructor. Kept for tests + as the
    /// "from" side of [`migrate_legacy_state_if_present`]; new
    /// production code should call [`Self::for_active_target`]
    /// instead so state is pinned to the operator's chosen
    /// deployment target rather than to whatever directory they
    /// happened to invoke the CLI from.
    pub fn for_root(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Per-target constructor. Resolves to
    /// `<store_root>/state/<target>` so two parallel targets
    /// (`dev` / `prod`) get independent state caches, and so the
    /// same operator running the CLI from any directory under
    /// their project sees the same state — pinned to the active
    /// target, not the cwd.
    ///
    /// `name` is typically the value returned by
    /// `cli_core::target::resolve_active_target_name` after
    /// applying the `--target <name>` override.
    pub fn for_active_target(store: &TargetStorePaths, name: &str) -> Self {
        Self {
            root: store.state_dir(name),
        }
    }

    pub fn state_dir(&self) -> PathBuf {
        self.root.join(STATE_DIR)
    }

    pub fn state_file(&self) -> PathBuf {
        self.state_dir().join(STATE_FILE)
    }

    /// Per-cluster SSH `known_hosts` file. Lives next to
    /// `state.json` so it is naturally scoped to one cluster: when
    /// `destroy --yes` clears state, this file is removed too, and
    /// the next `apply` against a fresh server (which Hetzner may
    /// happily place at a recently-recycled IP) starts with a clean
    /// slate. Avoids the "host key verification failed" annoyance
    /// from the user's `~/.ssh/known_hosts` that would otherwise
    /// require manual `ssh-keygen -R` after every destroy+apply.
    pub fn known_hosts_file(&self) -> PathBuf {
        self.state_dir().join(KNOWN_HOSTS_FILE)
    }
}

/// Move a legacy per-cwd `<legacy_root>/.apprafter/state.json` (and
/// sibling `known_hosts`, if present) into the per-target paths
/// described by `target_paths`. Idempotent: no-op when there's
/// nothing to migrate or when the destination already exists.
///
/// Called from `platform-cli` before every state read so operators
/// upgrading from v0.1.153 transparently inherit their existing
/// cluster pointer without having to re-run `apprafter import`.
/// Prints a one-time stderr notice on success so the operator sees
/// that the move happened — silent migration would leave them
/// wondering why state appeared under `~/.config/apprafter/state/`
/// after they had been editing `.apprafter/state.json` by hand.
///
/// New-wins semantics: when both the legacy file and the new
/// per-target file exist, the function leaves the legacy file
/// untouched. Operators who have already run `apply` under the new
/// scheme on a freshly-created target should not have their state
/// silently overwritten by stale per-cwd data; the legacy file is
/// theirs to inspect and delete manually if they want to.
pub fn migrate_legacy_state_if_present(
    legacy_root: &Path,
    target_paths: &StatePaths,
) -> Result<()> {
    let legacy_state = legacy_root.join(STATE_DIR).join(STATE_FILE);
    if !legacy_state.exists() {
        return Ok(());
    }
    if target_paths.state_file().exists() {
        // New-wins: don't clobber an already-populated per-target
        // cache with the legacy per-cwd one. The two sources may
        // describe different clusters, and the operator's recent
        // intent is more likely encoded in the per-target file
        // (they had to opt into the new layout by running a
        // command under v0.1.154+).
        return Ok(());
    }

    std::fs::create_dir_all(target_paths.state_dir())?;
    std::fs::rename(&legacy_state, target_paths.state_file())?;

    // known_hosts is best-effort: a fresh cluster cache without
    // SSH activity won't have one, and we shouldn't fail the whole
    // migration if the move can't be completed (the next
    // `kubeconfig` will recreate it on demand). Errors here are
    // logged via the stderr notice but don't propagate.
    let legacy_known = legacy_root.join(STATE_DIR).join(KNOWN_HOSTS_FILE);
    let mut moved_known_hosts = false;
    if legacy_known.exists() {
        match std::fs::rename(&legacy_known, target_paths.known_hosts_file()) {
            Ok(()) => {
                moved_known_hosts = true;
            }
            Err(e) => {
                eprintln!(
                    "warning: could not migrate known_hosts from {legacy}: {e} \
                     (next `apprafter kubeconfig` will rebuild it)",
                    legacy = legacy_known.display()
                );
            }
        }
    }

    eprintln!(
        "note: migrated state from {legacy} to {new}{suffix}",
        legacy = legacy_state.display(),
        new = target_paths.state_file().display(),
        suffix = if moved_known_hosts {
            " (known_hosts moved alongside)"
        } else {
            ""
        }
    );

    // Best-effort cleanup of the now-empty `.apprafter/` directory
    // so operators who `ls` the project root see the layout has
    // really moved. Failing here is fine — a non-empty dir means
    // the operator parked unrelated files there.
    let legacy_dir = legacy_root.join(STATE_DIR);
    if legacy_dir.exists() {
        let _ = std::fs::remove_dir(&legacy_dir);
    }

    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub cluster_name: Option<String>,
    pub tier: Option<Tier>,
    pub provider: Option<String>,
    pub region: Option<String>,
    pub hetzner_cloud: Option<HetznerCloudState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HetznerCloudState {
    pub server_id: u64,
    pub server_name: String,
    #[serde(default)]
    pub ssh_key_ids: Vec<u64>,
    #[serde(default)]
    pub network_id: Option<u64>,
    #[serde(default)]
    pub firewall_id: Option<u64>,
    #[serde(default)]
    pub floating_ip_ids: Vec<u64>,
    /// Plain-text k3s kubeconfig — legacy, kept for one cycle so
    /// existing state.json files still print something useful via
    /// `apprafter kubeconfig`. New caches go to
    /// `kubeconfig_age` instead.
    #[serde(default)]
    pub kubeconfig_yaml: Option<String>,
    /// age-encrypted (ASCII-armored) k3s kubeconfig. Populated by
    /// `apprafter kubeconfig` on cold-fetch and re-fetch.
    #[serde(default)]
    pub kubeconfig_age: Option<String>,
    /// age-encrypted (ASCII-armored) Argo CD admin password.
    /// Populated by `apprafter argocd-password` on first call.
    #[serde(default)]
    pub argocd_admin_password_age: Option<String>,
}

impl State {
    pub fn load_or_default(paths: &StatePaths) -> Result<Self> {
        let path = paths.state_file();
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = std::fs::read(&path)?;
        serde_json::from_slice(&bytes).map_err(|err| CliError::InvalidState {
            path,
            message: err.to_string(),
        })
    }

    pub fn save(&self, paths: &StatePaths) -> Result<()> {
        std::fs::create_dir_all(paths.state_dir())?;
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(paths.state_file(), bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_store() -> (tempfile::TempDir, TargetStorePaths) {
        let dir = tempdir().unwrap();
        let store = TargetStorePaths::for_root(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn for_active_target_lays_out_state_under_store_state_target() {
        // Pins the on-disk shape against the per-target layout
        // declared in cli-core/src/target.rs §state_dir. If anyone
        // re-paths state_dir the assertion here fails loudly.
        let (_dir, store) = make_store();
        let paths = StatePaths::for_active_target(&store, "prod");
        assert_eq!(
            paths.state_file(),
            store.root().join("state/prod/.apprafter/state.json")
        );
        assert_eq!(
            paths.known_hosts_file(),
            store.root().join("state/prod/.apprafter/known_hosts")
        );
    }

    #[test]
    fn migrate_legacy_no_op_when_legacy_absent() {
        // Idempotent fast path: every read site calls the helper,
        // and a fresh cwd without a legacy `.apprafter/` should
        // pay nothing.
        let (_dir, store) = make_store();
        let paths = StatePaths::for_active_target(&store, "default");
        let legacy_root = tempdir().unwrap();
        migrate_legacy_state_if_present(legacy_root.path(), &paths).unwrap();
        assert!(!paths.state_file().exists(), "no state must be created");
    }

    #[test]
    fn migrate_legacy_moves_state_when_only_legacy_exists() {
        // Happy path: operator upgrades from v0.1.153 with a
        // populated `.apprafter/state.json` under their project
        // root. First v0.1.154 command moves it over.
        let (_dir, store) = make_store();
        let paths = StatePaths::for_active_target(&store, "default");
        let legacy_root = tempdir().unwrap();
        let legacy_dir = legacy_root.path().join(STATE_DIR);
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join(STATE_FILE), br#"{"cluster_name":"p"}"#).unwrap();

        migrate_legacy_state_if_present(legacy_root.path(), &paths).unwrap();

        assert!(paths.state_file().exists(), "new file must exist");
        assert!(
            !legacy_dir.join(STATE_FILE).exists(),
            "legacy file must be moved, not copied"
        );
        let body = std::fs::read_to_string(paths.state_file()).unwrap();
        assert!(body.contains("\"cluster_name\":\"p\""), "{body}");
    }

    #[test]
    fn migrate_legacy_leaves_existing_target_state_untouched() {
        // New-wins guard: an operator who already ran a v0.1.154
        // command (and so has per-target state) must not have it
        // clobbered by a stale per-cwd file that survived from a
        // pre-upgrade run.
        let (_dir, store) = make_store();
        let paths = StatePaths::for_active_target(&store, "default");
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        std::fs::write(paths.state_file(), br#"{"cluster_name":"new"}"#).unwrap();

        let legacy_root = tempdir().unwrap();
        let legacy_dir = legacy_root.path().join(STATE_DIR);
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join(STATE_FILE), br#"{"cluster_name":"legacy"}"#).unwrap();

        migrate_legacy_state_if_present(legacy_root.path(), &paths).unwrap();

        // New file kept verbatim.
        let body = std::fs::read_to_string(paths.state_file()).unwrap();
        assert!(body.contains("\"cluster_name\":\"new\""), "{body}");
        // Legacy file kept where it was — operator can inspect /
        // delete it manually.
        assert!(legacy_dir.join(STATE_FILE).exists());
    }

    #[test]
    fn migrate_legacy_moves_known_hosts_alongside_state() {
        // The known_hosts file is per-cluster (cleared on
        // `destroy`). When state moves, it must too — otherwise
        // the next `apprafter kubeconfig` SSH handshake would
        // fail with "host key verification failed" against an
        // IP that's already trusted.
        let (_dir, store) = make_store();
        let paths = StatePaths::for_active_target(&store, "default");
        let legacy_root = tempdir().unwrap();
        let legacy_dir = legacy_root.path().join(STATE_DIR);
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join(STATE_FILE), b"{}").unwrap();
        std::fs::write(
            legacy_dir.join(KNOWN_HOSTS_FILE),
            b"[1.2.3.4]:22 ssh-ed25519 AAA\n",
        )
        .unwrap();

        migrate_legacy_state_if_present(legacy_root.path(), &paths).unwrap();

        assert!(paths.state_file().exists());
        assert!(paths.known_hosts_file().exists());
        let kh = std::fs::read_to_string(paths.known_hosts_file()).unwrap();
        assert!(kh.contains("1.2.3.4"), "{kh}");
    }
}
