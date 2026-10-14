// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Whether a target store is its account's alone (WI-475).
//!
//! An account that owns a folder of the store, or may write to it, can replace what AppRafter
//! reads there: a target's `credentials.yaml` with its own Hetzner token, a `state.json` with a
//! kubeconfig or an SSH host key for a server of its choosing, the vault. The next deploy then
//! goes where that account wants, and a secret AppRafter writes there may be read back by it.
//! So before a secret of the store is used or written ([`ensure_private`]: the core's secret
//! store, and [`crate::target::StoreLock`] before any change) every folder of the store must be
//! this account's alone ([`crate::owner_only::control`]); and doctor lists every folder and
//! secret file another account owns or can write, changing nothing.
//!
//! The folders are the store's root, `targets/` and each target's folder in it, and `state/`,
//! each folder in it and each folder in those (a target's `.apprafter`). Only the folders that
//! exist count: a folder AppRafter makes later is made by this account, in a folder it already
//! checked. Their parents are not checked, nor any other file or folder at the root.

use std::io;
use std::path::{Path, PathBuf};

use crate::error::StoreFolderProblem;
use crate::target::TargetStorePaths;
use crate::{CliError, Result};

/// One of the store's folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreFolder {
    pub path: PathBuf,
    /// Its path under the store's root, `/`-separated on every platform (`targets/prod`);
    /// empty for the root itself.
    pub rel: String,
}

impl StoreFolder {
    /// Whether it is the store's root.
    pub fn is_root(&self) -> bool {
        self.rel.is_empty()
    }
}

/// The store's folders that exist, in a fixed order: the root, `targets/` and each folder in
/// it, then `state/`, each folder in it and each folder in those, by name. A symlink is
/// followed (what it points at is what is read and written). A folder whose entries cannot be
/// listed is the error.
pub fn folders(store: &TargetStorePaths) -> io::Result<Vec<StoreFolder>> {
    let mut out = Vec::new();
    if !is_dir(store.root())? {
        return Ok(out);
    }
    out.push(StoreFolder {
        path: store.root().to_path_buf(),
        rel: String::new(),
    });
    for (top, depth) in [("targets", 1), ("state", 2)] {
        walk(&store.root().join(top), top.to_string(), depth, &mut out)?;
    }
    Ok(out)
}

/// `dir` and, `depth` levels down, the folders in it, by name.
fn walk(dir: &Path, rel: String, depth: u32, out: &mut Vec<StoreFolder>) -> io::Result<()> {
    if !is_dir(dir)? {
        return Ok(());
    }
    out.push(StoreFolder {
        path: dir.to_path_buf(),
        rel: rel.clone(),
    });
    if depth == 0 {
        return Ok(());
    }
    let mut names: Vec<_> = std::fs::read_dir(dir)?
        .map(|entry| entry.map(|e| e.file_name()))
        .collect::<io::Result<_>>()?;
    names.sort();
    for name in names {
        walk(
            &dir.join(&name),
            format!("{rel}/{}", name.to_string_lossy()),
            depth - 1,
            out,
        )?;
    }
    Ok(())
}

/// Whether `path` is a directory, a symlink followed: `false` when nothing (or a dangling link)
/// is there, or something else.
fn is_dir(path: &Path) -> io::Result<bool> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.is_dir()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// The store's refusal: `Ok` when every folder of the store is this account's alone; else
/// [`CliError::StoreNotPrivate`] naming the first folder another account owns, or else the
/// first one other accounts can write. Read only: nothing is changed. A folder whose owner or
/// permissions cannot be read is the I/O error naming it (fail closed).
pub fn ensure_private(store: &TargetStorePaths) -> Result<()> {
    let folders = folders(store).map_err(|e| io_error(store.root(), e))?;
    let mut writable = None;
    for folder in folders {
        let control = match crate::owner_only::control(&folder.path) {
            Ok(control) => control,
            // Removed since it was listed: nothing there to replace.
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io_error(&folder.path, e)),
        };
        if control.foreign_owner {
            return Err(CliError::StoreNotPrivate {
                path: folder.path,
                problem: StoreFolderProblem::ForeignOwner,
            });
        }
        if control.writable_by_others && writable.is_none() {
            writable = Some(folder.path);
        }
    }
    match writable {
        Some(path) => Err(CliError::StoreNotPrivate {
            path,
            problem: StoreFolderProblem::WritableByOthers,
        }),
        None => Ok(()),
    }
}

fn io_error(path: &Path, e: io::Error) -> CliError {
    CliError::Io(io::Error::new(
        e.kind(),
        format!(
            "cannot read who may change the target store's folder {}: {e}",
            path.display()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner_only::pretend;

    fn store() -> (tempfile::TempDir, TargetStorePaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = TargetStorePaths::for_root(dir.path().join("store"));
        (dir, paths)
    }

    fn mkdirs(paths: &TargetStorePaths, rels: &[&str]) {
        for rel in rels {
            std::fs::create_dir_all(paths.root().join(rel)).unwrap();
        }
    }

    fn rels(paths: &TargetStorePaths) -> Vec<String> {
        folders(paths).unwrap().into_iter().map(|f| f.rel).collect()
    }

    #[test]
    fn a_store_that_is_not_there_has_no_folders_and_is_private() {
        let (_dir, paths) = store();
        assert!(folders(&paths).unwrap().is_empty());
        ensure_private(&paths).unwrap();
    }

    /// The root, `targets/` and each target's folder, `state/`, each target's state folder and
    /// what is in it; files, other folders at the root and deeper folders are not listed.
    #[test]
    fn the_folders_are_the_root_the_targets_and_their_state() {
        let (_dir, paths) = store();
        mkdirs(
            &paths,
            &[
                "targets/prod/deeper",
                "targets/dev",
                "state/prod/.apprafter/deeper",
                "state/gone",
                "auth",
                "run",
            ],
        );
        std::fs::write(paths.root().join("targets/file"), b"x").unwrap();
        std::fs::write(paths.root().join("state/prod/file"), b"x").unwrap();
        assert_eq!(
            rels(&paths),
            [
                "",
                "targets",
                "targets/dev",
                "targets/prod",
                "state",
                "state/gone",
                "state/prod",
                "state/prod/.apprafter",
            ]
        );
        let all = folders(&paths).unwrap();
        assert!(all[0].is_root() && all[0].path == paths.root());
        assert_eq!(all[3].path, paths.target_dir("prod"));
        assert_eq!(all[6].path, paths.state_dir("prod"));
        ensure_private(&paths).unwrap();
    }

    /// Every folder is checked: one another account owns (pretended, in a temp dir) is refused,
    /// naming it, and a foreign owner is named before a folder others can write.
    #[test]
    fn a_folder_another_account_owns_is_refused_by_name() {
        let (_dir, paths) = store();
        mkdirs(&paths, &["targets/prod", "state/prod/.apprafter"]);
        for path in [
            paths.root().to_path_buf(),
            paths.targets_dir(),
            paths.target_dir("prod"),
            paths.root().join("state"),
            paths.state_dir("prod"),
            paths.state_dir("prod").join(".apprafter"),
        ] {
            let _foreign = pretend::foreign_owner(&path);
            match ensure_private(&paths) {
                Err(CliError::StoreNotPrivate {
                    path: named,
                    problem: StoreFolderProblem::ForeignOwner,
                }) => assert_eq!(named, path),
                other => panic!("{path:?}: {other:?}"),
            }
        }
        ensure_private(&paths).unwrap();
    }

    /// Unix: a folder others can write is refused, naming it; its mode put back, the store is
    /// used again.
    #[cfg(unix)]
    #[test]
    fn a_folder_others_can_write_is_refused_by_name() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, paths) = store();
        mkdirs(&paths, &["targets/prod", "state/prod/.apprafter"]);
        for path in [
            paths.root().to_path_buf(),
            paths.target_dir("prod"),
            paths.state_dir("prod").join(".apprafter"),
        ] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
            match ensure_private(&paths) {
                Err(CliError::StoreNotPrivate {
                    path: named,
                    problem: StoreFolderProblem::WritableByOthers,
                }) => assert_eq!(named, path),
                other => panic!("{path:?}: {other:?}"),
            }
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            ensure_private(&paths).unwrap();
        }
        let _foreign = pretend::foreign_owner(&paths.target_dir("prod"));
        std::fs::set_permissions(paths.root(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            ensure_private(&paths),
            Err(CliError::StoreNotPrivate {
                problem: StoreFolderProblem::ForeignOwner,
                ..
            })
        ));
        std::fs::set_permissions(paths.root(), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Windows: a folder whose DACL lets another account write is refused, naming it.
    #[cfg(windows)]
    #[test]
    fn a_folder_others_can_write_is_refused_by_name() {
        use crate::owner_only::testing;
        let (_dir, paths) = store();
        mkdirs(&paths, &["targets/prod"]);
        let me = testing::this_user_sid().unwrap();
        let path = paths.target_dir("prod");
        testing::set_dacl(
            &path,
            &format!("D:P(A;OICI;FA;;;{me})(A;OICI;0x1301bf;;;BU)"),
        )
        .unwrap();
        match ensure_private(&paths) {
            Err(CliError::StoreNotPrivate {
                path: named,
                problem: StoreFolderProblem::WritableByOthers,
            }) => assert_eq!(named, path),
            other => panic!("{other:?}"),
        }
        testing::set_dacl(&path, &format!("D:P(A;OICI;FA;;;{me})")).unwrap();
        ensure_private(&paths).unwrap();
    }
}
