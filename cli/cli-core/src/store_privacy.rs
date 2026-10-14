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
//! exist count: a folder AppRafter makes later is made by this account, owner-only whatever the
//! umask ([`TargetStorePaths::create_dir_all`], WI-479), in a folder it already checked — so a
//! store AppRafter makes from nothing passes, and a command that asks before its first save
//! (the `target add` wizard) is never refused after it asked. Their parents are not checked,
//! nor any other file or folder at the root.
//!
//! A folder another account owns is often one this account cannot open either (AppRafter makes
//! each folder 0700). Whatever reads in it is then denied: [`foreign_folder_behind`] turns that
//! denial into the same refusal, naming the folder, so no command shows a bare "Permission
//! denied" for it, whatever it read first (WI-481).

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
    /// Why the folders in it could not be listed, when they should have been (`targets/`,
    /// `state/` and each folder in `state/`): they are then missing from the list. A folder
    /// another account owns, mode 0700, is one; so is this account's own made unreadable.
    pub unlisted: Option<Unlisted>,
}

/// A listing that failed: the error's kind and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unlisted {
    pub kind: io::ErrorKind,
    pub message: String,
}

impl From<&io::Error> for Unlisted {
    fn from(e: &io::Error) -> Self {
        Unlisted {
            kind: e.kind(),
            message: e.to_string(),
        }
    }
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
/// listed is listed itself, with why ([`StoreFolder::unlisted`]), and the walk goes on past it
/// (WI-481: it used to end the walk, so a folder another account owns and keeps closed hid
/// behind an error naming the store's root). The error: whether the root, or a folder found in
/// a listing, is a folder could not be read.
pub fn folders(store: &TargetStorePaths) -> io::Result<Vec<StoreFolder>> {
    let mut out = Vec::new();
    if !is_dir(store.root())? {
        return Ok(out);
    }
    out.push(StoreFolder {
        path: store.root().to_path_buf(),
        rel: String::new(),
        unlisted: None,
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
        unlisted: None,
    });
    if depth == 0 {
        return Ok(());
    }
    let listed = std::fs::read_dir(dir).and_then(|entries| {
        entries
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<io::Result<Vec<_>>>()
    });
    let mut names = match listed {
        Ok(names) => names,
        Err(e) => {
            if let Some(folder) = out.last_mut() {
                folder.unlisted = Some(Unlisted::from(&e));
            }
            return Ok(());
        }
    };
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
/// permissions cannot be read is the I/O error naming it (fail closed), and so is one whose
/// folders could not be listed, when no folder was refused (WI-481: a folder another account
/// owns is refused by name even when it cannot be opened).
pub fn ensure_private(store: &TargetStorePaths) -> Result<()> {
    let folders = folders(store).map_err(|e| io_error(store.root(), e))?;
    let (mut writable, mut unlisted) = (None, None);
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
            writable = Some(folder.path.clone());
        }
        if let (Some(why), None) = (folder.unlisted, &unlisted) {
            unlisted = Some((folder.path, why));
        }
    }
    if let Some(path) = writable {
        return Err(CliError::StoreNotPrivate {
            path,
            problem: StoreFolderProblem::WritableByOthers,
        });
    }
    match unlisted {
        Some((path, why)) => Err(io_error(&path, io::Error::new(why.kind, why.message))),
        None => Ok(()),
    }
}

/// The refusal behind `e`, an error reading in `folder` (a file in it, or its own entries),
/// when another account is why: `e` is a permission denial, and `folder` or a folder above it,
/// up to the store's root, belongs to another account. Then the first such folder from the root
/// down is refused as [`ensure_private`] refuses it ([`CliError::StoreNotPrivate`], naming it):
/// the account that owns it decides who may open it, and the store would be refused for it
/// before any of its secrets is used anyway. `None` otherwise — another error, a folder outside
/// the store, or folders that are all this account's own (one it made unreadable itself is its
/// own problem, said by the caller's error) — and the caller keeps its error. Read only: the
/// owners of those folders.
pub fn foreign_folder_behind(
    store: &TargetStorePaths,
    folder: &Path,
    e: &io::Error,
) -> Option<CliError> {
    if e.kind() != io::ErrorKind::PermissionDenied {
        return None;
    }
    let rel = folder.strip_prefix(store.root()).ok()?;
    let mut at = store.root().to_path_buf();
    let mut chain = vec![at.clone()];
    for part in rel.components() {
        at.push(part);
        chain.push(at.clone());
    }
    for path in chain {
        match crate::owner_only::control(&path) {
            Ok(control) if control.foreign_owner => {
                return Some(CliError::StoreNotPrivate {
                    path,
                    problem: StoreFolderProblem::ForeignOwner,
                })
            }
            Ok(_) => continue,
            // Nothing further down can be read either: its owner is not known here.
            Err(_) => return None,
        }
    }
    None
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

    /// Unix: `path` made unopenable (mode 000) while the guard lives, then 0700 again. Refuses
    /// to go on where mode 000 does not lock this account out (root): the tests that use it
    /// are about what cannot be opened, and would pass there without opening anything closed.
    #[cfg(unix)]
    struct Closed(PathBuf);

    #[cfg(unix)]
    impl Closed {
        fn new(path: &Path) -> Closed {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
            let closed = Closed(path.to_path_buf());
            assert!(
                std::fs::read_dir(path).is_err(),
                "mode 000 does not lock this account out of {path:?}: run the tests as a user, \
                 not as root"
            );
            closed
        }
    }

    #[cfg(unix)]
    impl Drop for Closed {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }

    /// WI-481, Unix: a folder another account owns (pretended) and keeps closed (mode 000, as
    /// such a folder made 0700 is to this account) is refused by name — the folders the walk
    /// lists (`targets`, `state`, a target's state folder) as well as one it does not open (a
    /// target's folder). It used to end the walk with an I/O error naming the store's root.
    #[cfg(unix)]
    #[test]
    fn a_folder_another_account_owns_and_keeps_closed_is_refused_by_name() {
        let (_dir, paths) = store();
        mkdirs(&paths, &["targets/prod", "state/prod/.apprafter"]);
        for path in [
            paths.targets_dir(),
            paths.target_dir("prod"),
            paths.root().join("state"),
            paths.state_dir("prod"),
        ] {
            let _foreign = pretend::foreign_owner(&path);
            let _closed = Closed::new(&path);
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

    /// WI-481, Unix: a folder whose folders cannot be listed stays in the list, with why, and
    /// the walk goes on past it; the store's refusal fails closed on it, naming that folder —
    /// after a folder others can write, which is refused first.
    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_listed_is_listed_with_why_and_fails_closed_by_name() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, paths) = store();
        mkdirs(
            &paths,
            &["targets/prod", "state/prod/.apprafter", "state/zed"],
        );
        let closed_path = paths.state_dir("prod");
        let closed = Closed::new(&closed_path);
        let all = folders(&paths).unwrap();
        assert_eq!(
            all.iter().map(|f| f.rel.as_str()).collect::<Vec<_>>(),
            [
                "",
                "targets",
                "targets/prod",
                "state",
                "state/prod",
                "state/zed"
            ]
        );
        let why = all[4].unlisted.as_ref().expect("state/prod is unlisted");
        assert_eq!(why.kind, io::ErrorKind::PermissionDenied);
        assert_eq!(all.iter().filter(|f| f.unlisted.is_some()).count(), 1);
        match ensure_private(&paths) {
            Err(CliError::Io(e)) => {
                let text = e.to_string();
                assert!(text.contains(&closed_path.display().to_string()), "{text}");
                assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
            }
            other => panic!("{other:?}"),
        }
        std::fs::set_permissions(paths.root(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            ensure_private(&paths),
            Err(CliError::StoreNotPrivate { path, problem: StoreFolderProblem::WritableByOthers })
                if path == paths.root()
        ));
        std::fs::set_permissions(paths.root(), std::fs::Permissions::from_mode(0o700)).unwrap();
        drop(closed);
        ensure_private(&paths).unwrap();
    }

    /// WI-481: a denial in a folder another account owns (pretended) is that folder's refusal,
    /// naming the first such folder from the store's root down to the folder read in; any other
    /// error, a folder outside the store, or a chain of this account's own folders is `None`.
    #[test]
    fn a_denial_in_a_folder_another_account_owns_is_its_refusal() {
        let (dir, paths) = store();
        mkdirs(&paths, &["targets/prod", "state/prod/.apprafter"]);
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        let named = |folder: &Path, e: &io::Error| match foreign_folder_behind(&paths, folder, e) {
            Some(CliError::StoreNotPrivate {
                path,
                problem: StoreFolderProblem::ForeignOwner,
            }) => Some(path),
            None => None,
            Some(other) => panic!("{other:?}"),
        };
        let prod = paths.target_dir("prod");
        assert_eq!(named(&prod, &denied), None, "all this account's own");
        {
            let _foreign = pretend::foreign_owner(&prod);
            assert_eq!(named(&prod, &denied), Some(prod.clone()));
            assert_eq!(
                named(&paths.targets_dir(), &denied),
                None,
                "below the folder read in"
            );
            let missing = io::Error::from(io::ErrorKind::NotFound);
            assert_eq!(named(&prod, &missing), None, "not a denial");
            assert_eq!(named(dir.path(), &denied), None, "outside the store");
            let _targets = pretend::foreign_owner(&paths.targets_dir());
            assert_eq!(
                named(&prod, &denied),
                Some(paths.targets_dir()),
                "the first from the root"
            );
            assert_eq!(
                named(&paths.targets_dir(), &denied),
                Some(paths.targets_dir())
            );
        }
        let apprafter = paths.state_dir("prod").join(".apprafter");
        let _root = pretend::foreign_owner(paths.root());
        assert_eq!(named(&apprafter, &denied), Some(paths.root().to_path_buf()));
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
