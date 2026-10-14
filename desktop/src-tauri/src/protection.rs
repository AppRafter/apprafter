// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Where the target store keeps its secrets, as the app reads it (D.4a T74, §3.14):
//! `store.yaml`, and whether the vault beside it exists — its directory entry only, never its
//! bytes, and no key is asked for (T-lazy-3). Read again on every `app_info`, on window focus,
//! before `lock_info` and doctor, and when a secret read finds no key (the shell's `recheck`).
//!
//! **Protected** — the app's unlock opens the store's vault, and the lock's settings are pinned
//! (O7) — means `store.yaml` says the secrets are not in files and names the store's id, and
//! the vault is there. A store whose interrupted `lock on` wrote no vault has nothing to open:
//! its app lock stays D.2's, and the Security page offers to finish or back out. A `store.yaml`
//! that cannot be read is reported `locked` and counts as protected with no id, as the core
//! treats it (none of its secrets is used; fail closed, overview rule 3): the app stays locked
//! until it is repaired, and `problem` says why. A lock whose vault is gone (O8) is not protected
//! — nothing opens it — and `problem` says that too. Neither is ever shown as "encrypted".

use std::io;
use std::path::Path;

use apprafter_core::secrets::{read_store_file, LockSlotKind, SecretsMode, StoreId};
use apprafter_core::vault::VAULT_FILE;
use apprafter_core::{CoreError, UiError};
use apprafter_desktop_ipc::SecretsInfo;
use cli_core::{CliError, VaultProblem};

/// The store's state, as one read found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protection {
    pub mode: SecretsMode,
    pub slots: Vec<LockSlotKind>,
    /// The app's unlock opens the vault (see the module docs).
    pub protected: bool,
    /// The id the vault must name, when it can be read.
    pub id: Option<StoreId>,
    /// The root, home-abbreviated.
    pub store: String,
    /// Why the lock state cannot be used (see the module docs); `None` otherwise.
    pub problem: Option<UiError>,
}

impl Protection {
    /// Reads `root`'s `store.yaml` and the vault's directory entry; `home` abbreviates the
    /// root as the app shows paths.
    pub fn read(root: &Path, home: Option<&Path>) -> Protection {
        let store = cli_core::paths::abbreviate_home(root, home);
        match read_store_file(root) {
            Ok(None) => Protection {
                mode: SecretsMode::Files,
                slots: Vec::new(),
                protected: false,
                id: None,
                store,
                problem: None,
            },
            Ok(Some(file)) => {
                let vault = match std::fs::symlink_metadata(root.join(VAULT_FILE)) {
                    Ok(_) => true,
                    Err(e) => e.kind() != io::ErrorKind::NotFound,
                };
                let id = file
                    .id
                    .clone()
                    .filter(|_| file.secrets != SecretsMode::Files && vault);
                // O8's lost vault: a lock nothing opens, said in the core's words. An
                // interrupted run without its vault (`locking`, `unlocking`) is no problem.
                let lost = file.secrets == SecretsMode::Locked && !vault;
                let problem = lost.then(|| {
                    UiError::from(&CoreError::Cli(CliError::VaultUnreadable {
                        path: root.join(VAULT_FILE),
                        problem: VaultProblem::Missing,
                        detail: String::new(),
                    }))
                });
                Protection {
                    mode: file.secrets,
                    slots: file.slots.iter().map(|s| s.kind).collect(),
                    protected: id.is_some(),
                    id,
                    store,
                    problem,
                }
            }
            Err(e) => {
                tracing::warn!("the target store's store.yaml cannot be read: {e}");
                Protection {
                    mode: SecretsMode::Locked,
                    slots: Vec::new(),
                    protected: true,
                    id: None,
                    store,
                    problem: Some(UiError::from(&CoreError::from(e))),
                }
            }
        }
    }

    /// As `AppInfo.secrets` carries it.
    pub fn info(&self) -> SecretsInfo {
        SecretsInfo {
            mode: self.mode,
            slots: self.slots.clone(),
            protected: self.protected,
            store: self.store.clone(),
            problem: self.problem.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use apprafter_core::error::codes;
    use apprafter_core::secrets::testing::locked_store;
    use apprafter_core::secrets::{LockSlotKind, SecretsMode};
    use apprafter_core::vault::VAULT_FILE;

    use super::Protection;

    #[test]
    fn a_store_in_files_is_not_protected() {
        let dir = tempfile::tempdir().unwrap();
        let p = Protection::read(dir.path(), None);
        assert_eq!(p.mode, SecretsMode::Files);
        assert!(p.slots.is_empty() && !p.protected && p.id.is_none() && p.problem.is_none());
        assert_eq!(p.store, dir.path().display().to_string());
    }

    #[test]
    fn a_locked_store_with_its_vault_is_protected_under_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(dir.path(), &[("prod", None)]);
        let p = Protection::read(dir.path(), None);
        assert_eq!(p.mode, SecretsMode::Locked);
        assert_eq!(p.slots, vec![LockSlotKind::RecoveryKey]);
        assert!(p.protected && p.problem.is_none());
        assert_eq!(p.id.as_ref(), Some(&store.store_id));
        let info = p.info();
        assert!(info.protected && info.mode == SecretsMode::Locked && info.problem.is_none());
    }

    /// A lock whose vault is gone (O8's lost vault): nothing opens it, so it is not
    /// protected, and the read says why in the core's words — never "encrypted" (rule 1).
    #[test]
    fn a_locked_store_whose_vault_is_gone_is_a_problem_and_not_protected() {
        let dir = tempfile::tempdir().unwrap();
        locked_store(dir.path(), &[]);
        std::fs::remove_file(dir.path().join(VAULT_FILE)).unwrap();
        let p = Protection::read(dir.path(), None);
        assert_eq!(p.mode, SecretsMode::Locked);
        assert!(!p.protected && p.id.is_none());
        let problem = p.problem.expect("a lost vault is said");
        assert_eq!(problem.code.as_deref(), Some(codes::VAULT_UNREADABLE));
        assert_eq!(problem.fields["problem"], "missing");
    }

    /// An interrupted `lock on` that wrote no vault: nothing to open, so the app lock stays
    /// D.2's, and the Security page offers to finish or back out — a state, not a problem.
    #[test]
    fn an_interrupted_protect_without_its_vault_is_not_protected_and_no_problem() {
        let dir = tempfile::tempdir().unwrap();
        locked_store(dir.path(), &[]);
        std::fs::remove_file(dir.path().join(VAULT_FILE)).unwrap();
        let file = dir.path().join("store.yaml");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("secrets: locked"), "{text}");
        std::fs::write(&file, text.replace("secrets: locked", "secrets: locking")).unwrap();
        let p = Protection::read(dir.path(), None);
        assert_eq!(p.mode, SecretsMode::Locking);
        assert!(!p.protected && p.id.is_none() && p.problem.is_none());
    }

    /// The core treats an unreadable store.yaml as locked and uses none of its secrets: so
    /// does the app (fail closed: overview rule 3, §3.5), with no id to open, and it says why.
    #[test]
    fn an_unreadable_store_file_reads_as_locked_and_protected_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("store.yaml"),
            "format: 1\nsecrets: sideways\n",
        )
        .unwrap();
        let p = Protection::read(dir.path(), None);
        assert_eq!(p.mode, SecretsMode::Locked);
        assert!(p.protected && p.id.is_none() && p.slots.is_empty());
        let problem = p.problem.expect("an unreadable store.yaml is said");
        assert_eq!(problem.code.as_deref(), Some(codes::STORE_FILE_UNREADABLE));
    }

    #[test]
    fn the_store_is_shown_under_the_home_as_paths_are() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cfg").join("apprafter");
        std::fs::create_dir_all(&root).unwrap();
        let p = Protection::read(&root, Some(dir.path()));
        assert_eq!(
            p.store,
            format!("~/{}", Path::new("cfg").join("apprafter").display())
        );
    }
}
