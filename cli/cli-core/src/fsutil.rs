// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Filesystem helpers for the files the CLI and AppRafter Desktop both
//! rewrite: the target store and `state.json`.

use std::io::{self, Write};
use std::path::Path;

/// Replace `dest` with `bytes` atomically: write a temp file next to it
/// (`<prefix>XXXXXX.tmp`), fsync it, then rename it over `dest` with
/// `std::fs::rename` — which on Windows retries a refused `MoveFileExW`
/// with POSIX rename semantics, so a reader holding `dest` open does not
/// make the replace fail (tempfile's `persist` does not retry). On Unix the
/// parent directory is fsynced afterwards (errors ignored). On error the
/// temp file is removed, on Windows too: the owner-only temp file's handle
/// shares removal by name ([`crate::owner_only::create_new`]).
///
/// `mode` is the file's mode on Unix, applied to the temp file before the
/// rename; `None` keeps tempfile's 0600. A file that is owner-only on Unix
/// (`None`, or a mode that grants nothing to group or others: `credentials.yaml`,
/// `state.json`) is owner-only on Windows too: its temp file gets the
/// protected one-ACE DACL ([`crate::owner_only`]) on its open handle before
/// its first byte, and the same-volume rename keeps it, so a store in a
/// folder other users can read keeps them out. A wider mode (0644) leaves
/// the Windows file what it inherits from its directory.
///
/// A reader sees the old file or the new one, never a partial write, and
/// never the new one under a wider mode than `mode`.
pub fn atomic_replace(
    dest: &Path,
    bytes: &[u8],
    prefix: &str,
    mode: Option<u32>,
) -> io::Result<()> {
    let parent = match dest.parent() {
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) => p,
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} has no parent directory to write next to",
                    dest.display()
                ),
            ))
        }
    };

    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix).suffix(".tmp");
    #[cfg(windows)]
    let mut tmp = if mode.is_none_or(|mode| mode & 0o077 == 0) {
        builder.make_in(parent, crate::owner_only::create_new)?
    } else {
        builder.tempfile_in(parent)?
    };
    #[cfg(not(windows))]
    let mut tmp = builder.tempfile_in(parent)?;
    #[cfg(all(test, windows))]
    crate::owner_only::testing::record_before_first_byte(tmp.as_file());
    tmp.write_all(bytes)?;
    #[cfg(test)]
    if WRITE_FAILS.with(std::cell::Cell::take) {
        return Err(io::Error::other("a forced write error"));
    }
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    tmp.as_file().sync_all()?;

    // Until here a failure drops `tmp`, which removes the temp file. From
    // here on the temp file is ours to remove: it is closed first, because
    // Windows will not rename a file this process still has open.
    let (file, path) = tmp.keep().map_err(|e| e.error)?;
    drop(file);
    if let Err(e) = std::fs::rename(&path, dest) {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }

    // The rename is durable once the directory entry is: fsync the parent.
    // Best-effort — a filesystem that cannot fsync a directory still did
    // the replace.
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// A test's forced write error: this thread's next [`atomic_replace`] fails once its temp
    /// file holds the bytes, as a write to a full disk does.
    static WRITE_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names in `dir` other than `keep`.
    fn others(dir: &Path, keep: &str) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != keep)
            .collect()
    }

    #[test]
    fn it_creates_the_file_and_then_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("f.yaml");
        atomic_replace(&dest, b"one", ".f.yaml.", None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"one");
        atomic_replace(&dest, b"two", ".f.yaml.", None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"two");
    }

    #[test]
    fn it_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("f.yaml");
        atomic_replace(&dest, b"one", ".f.yaml.", None).unwrap();
        atomic_replace(&dest, b"two", ".f.yaml.", None).unwrap();
        assert!(others(dir.path(), "f.yaml").is_empty());
    }

    #[test]
    fn a_failed_rename_removes_the_temp_file() {
        // `dest` is a non-empty directory: the temp file is written, and
        // the rename over it is refused on every platform.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("occupied");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("inside"), b"x").unwrap();
        atomic_replace(&dest, b"new", ".occupied.", None).expect_err("cannot replace a directory");
        assert!(
            others(dir.path(), "occupied").is_empty(),
            "temp file left behind"
        );
    }

    /// WI-474: a write that fails leaves no temp file. On Windows the owner-only temp file
    /// (`None`, 0600) is removed by tempfile's cleanup while its handle is still open, which
    /// that handle must share; a wider mode's temp file is tempfile's own.
    #[test]
    fn a_failed_write_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        for (name, mode) in [("none", None), ("0600", Some(0o600)), ("0644", Some(0o644))] {
            let dest = dir.path().join(name);
            WRITE_FAILS.with(|fails| fails.set(true));
            let err = atomic_replace(&dest, b"secret", ".f.", mode).expect_err(name);
            assert_eq!(err.to_string(), "a forced write error", "{name}");
            let left = others(dir.path(), "");
            assert!(left.is_empty(), "{name}: left behind: {left:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_mode_is_applied_and_none_keeps_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        let open = dir.path().join("open");
        atomic_replace(&open, b"x", ".open.", Some(0o644)).unwrap();
        assert_eq!(mode(&open), 0o644);

        let secret = dir.path().join("secret");
        std::fs::write(&secret, b"old").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
        atomic_replace(&secret, b"x", ".secret.", None).unwrap();
        assert_eq!(mode(&secret), 0o600, "None must keep tempfile's 0600");
    }

    /// WI-472: an owner-only file (`None`, or a mode that grants group and others nothing) gets
    /// the protected one-ACE DACL on its temp file before the first byte, keeps it through the
    /// rename, and replaces a file that inherited its folder's DACL.
    #[cfg(windows)]
    #[test]
    fn an_owner_only_file_has_the_owner_only_dacl_from_its_first_byte() {
        use crate::owner_only::testing;
        let dir = tempfile::tempdir().unwrap();
        for (name, mode) in [("none", None), ("0600", Some(0o600)), ("0400", Some(0o400))] {
            let dest = dir.path().join(name);
            std::fs::write(&dest, b"old, inherited").unwrap();
            let _ = testing::take_before_first_byte();
            atomic_replace(&dest, b"secret", ".f.", mode).unwrap();
            testing::take_before_first_byte()
                .expect("the write recorded its temp file's DACL")
                .unwrap()
                .assert_owner_only_file(&format!("{name}: the temp file before its first byte"));
            testing::of_path(&dest)
                .unwrap()
                .assert_owner_only_file(&format!("{name}: the file after the rename"));
            assert_eq!(std::fs::read(&dest).unwrap(), b"secret");
        }
        assert!(others(dir.path(), "").iter().all(|n| !n.ends_with(".tmp")));
    }

    /// A mode that lets group or others read is not a secret's: on Windows the file keeps what
    /// its directory hands down.
    #[cfg(windows)]
    #[test]
    fn a_wider_mode_keeps_what_the_directory_hands_down() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("config.yaml");
        atomic_replace(&dest, b"x", ".config.yaml.", Some(0o644)).unwrap();
        assert!(
            !crate::owner_only::testing::of_path(&dest)
                .unwrap()
                .protected
        );
    }

    /// The reason this helper exists: tempfile's `persist` fails with
    /// "Access is denied" when a reader holds the destination open.
    #[cfg(windows)]
    #[test]
    fn a_reader_holding_the_destination_open_does_not_stop_the_replace() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("f.yaml");
        std::fs::write(&dest, b"old").unwrap();
        let reader = std::fs::File::open(&dest).unwrap();
        atomic_replace(&dest, b"new", ".f.yaml.", None).unwrap();
        drop(reader);
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
    }
}
